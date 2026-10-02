//! Device endpoints: enrollment (`/v1/enroll`) and sessions (`/v1/session`).
//!
//! Delivery contract, per device:
//! - every delivery row not yet acknowledged as `received` is (re)sent, oldest first,
//!   at most `devices.max_inflight` at a time, again after `devices.resend_after`;
//! - the device stores the alert durably before acknowledging `received`, and
//!   deduplicates on `alert_id`, so retransmissions never show twice;
//! - nothing is ever dropped: a device that is offline gets its backlog on reconnect.

use std::collections::{HashMap, HashSet};
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};
use std::time::Duration;

use axum::extract::ws::{CloseFrame, Message, WebSocket, WebSocketUpgrade};
use axum::extract::{ConnectInfo, State};
use axum::http::HeaderMap;
use axum::response::Response;
use opentelemetry::KeyValue;
use inbox_core::enroll::EnrollServer;
use inbox_core::noise::{self, Responder, Session};
use inbox_core::proto::{AlertMsg, ClientMsg, ServerMsg, SessionAccept, SessionHello, close};
use inbox_core::rng::Rng;
use inbox_core::{Error as CoreError, PROTOCOL_VERSION};
use tokio::sync::{Notify, watch};
use tokio::time::{Instant, timeout};
use tracing::Instrument;

use crate::db::{self, DbError};
use crate::hub::Live;
use crate::net::client_ip;
use crate::state::AppState;
use crate::util::{is_hex_id, now_ms};

const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(15);
/// Argon2 runs on the phone between the two enrollment messages.
const ENROLL_FINISH_TIMEOUT: Duration = Duration::from_secs(60);
const ENROLL_MAX_MESSAGE: usize = 8 * 1024;

pub async fn enroll_ws(
    State(app): State<Arc<AppState>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    ws: WebSocketUpgrade,
) -> Response {
    let ip = client_ip(peer, &headers, &app.config.server.trusted_proxies);
    ws.max_message_size(ENROLL_MAX_MESSAGE).max_frame_size(ENROLL_MAX_MESSAGE).on_upgrade(
        move |socket| {
            run_enroll(app, socket, ip)
                .instrument(tracing::info_span!("device.enroll", client.address = %ip))
        },
    )
}

pub async fn session_ws(
    State(app): State<Arc<AppState>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    ws: WebSocketUpgrade,
) -> Response {
    let ip = client_ip(peer, &headers, &app.config.server.trusted_proxies);
    ws.max_message_size(noise::MAX_MESSAGE)
        .max_frame_size(noise::MAX_MESSAGE)
        .on_upgrade(move |socket| run_session(app, socket, ip))
}

async fn close_with(socket: &mut WebSocket, code: u16, reason: &str) {
    let frame = CloseFrame { code, reason: reason.into() };
    let _ = timeout(Duration::from_secs(2), socket.send(Message::Close(Some(frame)))).await;
}

async fn recv_text(socket: &mut WebSocket, limit: Duration) -> Option<String> {
    match timeout(limit, socket.recv()).await {
        Ok(Some(Ok(Message::Text(t)))) => Some(t.to_string()),
        _ => None,
    }
}

async fn recv_binary(socket: &mut WebSocket, limit: Duration) -> Option<Vec<u8>> {
    loop {
        match timeout(limit, socket.recv()).await {
            Ok(Some(Ok(Message::Binary(b)))) => return Some(b.to_vec()),
            Ok(Some(Ok(Message::Ping(_) | Message::Pong(_)))) => continue,
            _ => return None,
        }
    }
}

// ---------------------------------------------------------------- enrollment

async fn run_enroll(app: Arc<AppState>, mut socket: WebSocket, ip: IpAddr) {
    let reject =
        |code: &str, message: &str| Message::Text(EnrollServer::rejected(code, message).into());
    if !app.limiter.allowed(ip) {
        let _ = socket.send(reject("rate_limited", "too many failed attempts, retry later")).await;
        close_with(&mut socket, close::RATE_LIMITED, "rate limited").await;
        return;
    }
    let mut server = EnrollServer::new(&app.secrets.server_key, &app.secrets.device_record);

    let Some(text) = recv_text(&mut socket, HANDSHAKE_TIMEOUT).await else { return };
    let challenge =
        match EnrollServer::parse(&text).and_then(|m| server.on_start(m, &mut Rng::os())) {
            Ok(c) => c,
            Err(CoreError::UnsupportedProtocol(v)) => {
                let _ = socket
                    .send(reject("protocol", &format!("protocol {v} unsupported, update the app")))
                    .await;
                return;
            }
            Err(e) => {
                tracing::info!(event = "enroll_rejected", reason = %e);
                let _ = socket.send(reject("protocol", "malformed enrollment")).await;
                return;
            }
        };
    // With OPAQUE a wrong password is detected by the client, which may never come
    // back: every started attempt counts as a failure until it succeeds.
    app.limiter.record_failure(ip);
    if socket.send(Message::Text(challenge.into())).await.is_err() {
        return;
    }

    let Some(text) = recv_text(&mut socket, ENROLL_FINISH_TIMEOUT).await else { return };
    let verified = match EnrollServer::parse(&text).and_then(|m| server.on_finish(m)) {
        Ok(v) => v,
        Err(CoreError::AuthenticationFailed) => {
            app.limiter.record_failure(ip);
            app.metrics.auth_failures.add(1, &[KeyValue::new("kind", "device_password")]);
            tracing::warn!(event = "enroll_bad_password", client.address = %ip);
            let _ = socket.send(reject("bad_password", "wrong password")).await;
            return;
        }
        Err(e) => {
            tracing::info!(event = "enroll_rejected", reason = %e);
            let _ = socket.send(reject("protocol", "malformed enrollment")).await;
            return;
        }
    };

    let max_devices = app.config.devices.max_devices;
    let ip_text = ip.to_string();
    let name = verified.name.clone();
    let user = verified.user_name.clone().unwrap_or_default();
    let stored = app
        .db
        .call(move |c| {
            if db::count_active_devices(c)? >= max_devices {
                return Ok(None);
            }
            Ok(Some(db::insert_device(
                c,
                &verified.name,
                verified.user_name.as_deref(),
                &verified.public_key,
                &ip_text,
            )?))
        })
        .await;
    let device_id = match stored {
        Ok(Some(id)) => id,
        Ok(None) => {
            let _ = socket
                .send(reject("too_many_devices", "device limit reached, revoke unused devices"))
                .await;
            return;
        }
        Err(e) => {
            tracing::error!(event = "enroll_store_failed", error = %e);
            let _ = socket.send(reject("internal", "server storage error")).await;
            return;
        }
    };
    let accepted = match server.accepted(&device_id, &app.config.server.name) {
        Ok(a) => a,
        Err(e) => {
            tracing::error!(event = "enroll_accept_failed", error = %e);
            return;
        }
    };
    app.limiter.record_success(ip);
    app.metrics.enrollments.add(1, &[]);
    tracing::info!(
        event = "device_enrolled",
        device.id = %device_id,
        device.name = %name,
        user.name = %user,
        client.address = %ip
    );
    let _ = socket.send(Message::Text(accepted.into())).await;
    close_with(&mut socket, 1000, "enrolled").await;
}

// ---------------------------------------------------------------- sessions

enum End {
    Code(u16, &'static str),
    PeerClosed,
    Timeout,
    Protocol(&'static str),
    Storage(DbError),
    Send,
}

impl End {
    fn reason(&self) -> &'static str {
        match self {
            End::Code(_, r) => r,
            End::PeerClosed => "peer_closed",
            End::Timeout => "timeout",
            End::Protocol(_) => "protocol_error",
            End::Storage(_) => "storage_error",
            End::Send => "send_failed",
        }
    }
}

async fn send_msg(
    socket: &mut WebSocket,
    session: &mut Session,
    msg: &ServerMsg,
) -> Result<(), End> {
    let json = serde_json::to_vec(msg).expect("protocol messages serialize");
    let frame = session.encrypt(&json).map_err(|_| End::Protocol("frame too large"))?;
    socket.send(Message::Binary(frame.into())).await.map_err(|_| End::Send)
}

async fn run_session(app: Arc<AppState>, mut socket: WebSocket, ip: IpAddr) {
    if !app.limiter.allowed(ip) {
        close_with(&mut socket, close::RATE_LIMITED, "rate limited").await;
        return;
    }
    let reject = |app: &AppState, ip: IpAddr| {
        app.limiter.record_failure(ip);
        app.metrics.auth_failures.add(1, &[KeyValue::new("kind", "device_key")]);
    };

    let Some(m1) = recv_binary(&mut socket, HANDSHAKE_TIMEOUT).await else { return };
    let Ok((responder, remote_static, hello)) =
        Responder::start(&app.secrets.server_key.noise_private, &m1)
    else {
        reject(&app, ip);
        close_with(&mut socket, close::UNKNOWN_DEVICE, "handshake failed").await;
        return;
    };
    let Ok(hello) = serde_json::from_slice::<SessionHello>(&hello) else {
        close_with(&mut socket, close::PROTOCOL, "malformed hello").await;
        return;
    };
    if hello.protocol != PROTOCOL_VERSION {
        close_with(&mut socket, close::PROTOCOL, "unsupported protocol").await;
        return;
    }
    if !is_hex_id(&hello.device_id) {
        reject(&app, ip);
        close_with(&mut socket, close::UNKNOWN_DEVICE, "unknown device").await;
        return;
    }
    let device_id = hello.device_id.clone();
    let id = device_id.clone();
    let device = match app.db.call(move |c| Ok(db::find_device(c, &id)?)).await {
        Ok(d) => d,
        Err(e) => {
            tracing::error!(event = "session_lookup_failed", error = %e);
            close_with(&mut socket, 1011, "storage error").await;
            return;
        }
    };
    let device = match device {
        Some(d) if d.public_key == remote_static && !d.revoked => d,
        Some(d) if d.public_key == remote_static => {
            close_with(&mut socket, close::REVOKED, "device revoked").await;
            return;
        }
        _ => {
            reject(&app, ip);
            close_with(&mut socket, close::UNKNOWN_DEVICE, "unknown device").await;
            return;
        }
    };

    let accept =
        SessionAccept { protocol: PROTOCOL_VERSION, server_name: app.config.server.name.clone() };
    let accept = serde_json::to_vec(&accept).expect("serializes");
    let Ok((mut session, m2)) = responder.finish(&accept) else { return };
    if socket.send(Message::Binary(m2.into())).await.is_err() {
        return;
    }
    // The session only counts once the device proves it holds the transport keys.
    let ready = match recv_binary(&mut socket, HANDSHAKE_TIMEOUT).await {
        Some(frame) => {
            session.decrypt(&frame).ok().and_then(|p| serde_json::from_slice::<ClientMsg>(&p).ok())
        }
        None => return,
    };
    if !matches!(ready, Some(ClientMsg::Ready)) {
        close_with(&mut socket, close::PROTOCOL, "expected ready").await;
        return;
    }

    let span = tracing::info_span!(
        "device.session",
        device.id = %device_id,
        device.name = %device.name,
        user.name = %device.user_name.as_deref().unwrap_or(""),
        client.address = %ip,
        app.version = %hello.app_version
    );
    established(app, socket, session, device_id, hello.app_version, ip).instrument(span).await;
}

async fn established(
    app: Arc<AppState>,
    mut socket: WebSocket,
    mut session: Session,
    device_id: String,
    app_version: String,
    ip: IpAddr,
) {
    let session_id = app.next_session_id.fetch_add(1, Ordering::Relaxed);
    let (kick_tx, mut kick_rx) = watch::channel(None);
    let wake = Arc::new(Notify::new());
    let last_seen = Arc::new(AtomicI64::new(now_ms()));
    let app_version = inbox_core::enroll::validate_device_name(&app_version)
        .unwrap_or_else(|_| "unknown".into());
    app.hub.register(
        &device_id,
        Live {
            session_id,
            ip: ip.to_string(),
            app_version: app_version.clone(),
            connected_at: now_ms(),
            last_seen: last_seen.clone(),
            wake: wake.clone(),
            kick: kick_tx,
        },
        close::REPLACED,
    );
    let (id, ip_text, ver) = (device_id.clone(), ip.to_string(), app_version.clone());
    let connection_id =
        match app.db.call(move |c| Ok(db::record_connect(c, &id, &ip_text, &ver)?)).await {
            Ok(id) => id,
            Err(e) => {
                tracing::error!(event = "session_record_failed", error = %e);
                app.hub.unregister(&device_id, session_id);
                close_with(&mut socket, 1011, "storage error").await;
                return;
            }
        };
    app.metrics.sessions_active.add(1, &[]);
    tracing::info!(event = "device_connected");

    let end =
        deliver(&app, &mut socket, &mut session, &device_id, &mut kick_rx, &wake, &last_seen).await;

    app.hub.unregister(&device_id, session_id);
    app.metrics.sessions_active.add(-1, &[]);
    app.metrics.session_closes.add(1, &[KeyValue::new("reason", end.reason())]);
    let reason = end.reason();
    let id = device_id.clone();
    if let Err(e) =
        app.db.call(move |c| Ok(db::record_disconnect(c, connection_id, &id, reason)?)).await
    {
        tracing::error!(event = "session_disconnect_record_failed", error = %e);
    }
    match &end {
        End::Code(code, r) => close_with(&mut socket, *code, r).await,
        End::Timeout => close_with(&mut socket, 1001, "session timeout").await,
        End::Protocol(r) => close_with(&mut socket, close::PROTOCOL, r).await,
        End::Storage(e) => {
            tracing::error!(event = "session_storage_error", error = %e);
            close_with(&mut socket, 1011, "storage error").await;
        }
        End::PeerClosed | End::Send => {}
    }
    tracing::info!(event = "device_disconnected", reason);
}

async fn deliver(
    app: &AppState,
    socket: &mut WebSocket,
    session: &mut Session,
    device_id: &str,
    kick_rx: &mut watch::Receiver<Option<u16>>,
    wake: &Notify,
    last_seen: &AtomicI64,
) -> End {
    let cfg = &app.config.devices;
    let id = device_id.to_string();
    let profile = app.db.call(move |c| {
        let user_name = db::find_device(c, &id)?.and_then(|d| d.user_name);
        Ok((db::subscriptions(c, &id)?, user_name))
    });
    let (subscriptions, user_name) = match profile.await {
        Ok(p) => p,
        Err(e) => return End::Storage(e),
    };
    let welcome = ServerMsg::Welcome {
        server_name: app.config.server.name.clone(),
        groups: app.groups.clone(),
        subscriptions,
        heartbeat_interval_ms: cfg.heartbeat_interval.as_millis() as u64,
        offline_warning_after_ms: cfg.offline_warning_after.as_millis() as u64,
        server_time_ms: now_ms(),
        user_name,
    };
    if let Err(e) = send_msg(socket, session, &welcome).await {
        return e;
    }

    let mut inflight: HashMap<i64, Instant> = HashMap::new();
    let mut resend = tokio::time::interval(cfg.resend_after.min(Duration::from_secs(5)));
    resend.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut deadline = Instant::now() + cfg.session_timeout;
    let mut flush = true;

    loop {
        if flush {
            flush = false;
            if let Err(e) = flush_deliveries(app, socket, session, device_id, &mut inflight).await {
                return e;
            }
        }
        tokio::select! {
            biased;
            changed = kick_rx.changed() => {
                let code = if changed.is_ok() { *kick_rx.borrow() } else { None };
                return match code {
                    Some(close::REVOKED) => End::Code(close::REVOKED, "device revoked"),
                    Some(close::REPLACED) => End::Code(close::REPLACED, "replaced by a newer session"),
                    Some(1012) => End::Code(1012, "server restarting"),
                    Some(c) => End::Code(c, "closed by server"),
                    None => End::Code(1012, "server restarting"),
                };
            }
            msg = tokio::time::timeout_at(deadline, socket.recv()) => {
                let frame = match msg {
                    Err(_) => return End::Timeout,
                    Ok(None) | Ok(Some(Err(_))) | Ok(Some(Ok(Message::Close(_)))) => return End::PeerClosed,
                    Ok(Some(Ok(Message::Ping(_) | Message::Pong(_)))) => continue,
                    Ok(Some(Ok(Message::Text(_)))) => return End::Protocol("text frame"),
                    Ok(Some(Ok(Message::Binary(b)))) => b,
                };
                deadline = Instant::now() + cfg.session_timeout;
                last_seen.store(now_ms(), Ordering::Relaxed);
                let Ok(plain) = session.decrypt(&frame) else { return End::Protocol("decrypt failed") };
                let Ok(msg) = serde_json::from_slice::<ClientMsg>(&plain) else { return End::Protocol("malformed message") };
                match handle(app, socket, session, device_id, msg, &mut inflight).await {
                    Ok(f) => flush |= f,
                    Err(e) => return e,
                }
            }
            _ = wake.notified() => flush = true,
            // Periodic flush even without a wake-up: belt and braces for retransmission.
            _ = resend.tick() => flush = true,
        }
    }
}

/// Returns whether deliveries should be flushed afterwards.
async fn handle(
    app: &AppState,
    socket: &mut WebSocket,
    session: &mut Session,
    device_id: &str,
    msg: ClientMsg,
    inflight: &mut HashMap<i64, Instant>,
) -> Result<bool, End> {
    match msg {
        ClientMsg::Ping { nonce } => {
            send_msg(socket, session, &ServerMsg::Pong { nonce }).await?;
            Ok(false)
        }
        // Every stage is a delivery receipt: legacy apps also send displayed and
        // acknowledged, which only ever follow a receipt.
        ClientMsg::Ack { alert_id, stage: _ } => {
            if !is_hex_id(&alert_id) {
                return Err(End::Protocol("malformed alert id"));
            }
            let id = device_id.to_string();
            let outcome = app
                .db
                .call(move |c| Ok(db::ack(c, &id, &alert_id)?))
                .await
                .map_err(End::Storage)?;
            app.metrics.acks.add(1, &[]);
            if let Some(o) = outcome {
                if let Some(ms) = o.first_receipt_latency_ms {
                    app.metrics.delivery_latency.record(ms.max(0) as f64 / 1000.0, &[]);
                }
                inflight.remove(&o.seq);
            }
            Ok(true)
        }
        ClientMsg::Subscribe { group, action } => {
            if !app.group_names.contains_key(&group) {
                send_msg(
                    socket,
                    session,
                    &ServerMsg::Error { code: "unknown_group".into(), message: group },
                )
                .await?;
                return Ok(false);
            }
            let id = device_id.to_string();
            let subs = app
                .db
                .call(move |c| {
                    db::set_subscription(c, &id, &group, action)?;
                    Ok(db::subscriptions(c, &id)?)
                })
                .await
                .map_err(End::Storage)?;
            tracing::info!(event = "subscription_changed", action = action.as_str());
            send_msg(socket, session, &ServerMsg::Subscriptions { subscriptions: subs }).await?;
            Ok(false)
        }
        ClientMsg::SetUserName { user_name } => {
            let Ok(user_name) = inbox_core::enroll::validate_user_name(&user_name) else {
                let error = ServerMsg::Error {
                    code: "invalid_user_name".into(),
                    message: "1..64 bytes without control characters".into(),
                };
                send_msg(socket, session, &error).await?;
                return Ok(false);
            };
            let (id, name) = (device_id.to_string(), user_name.clone());
            app.db
                .call(move |c| Ok(db::set_user_name(c, &id, &name)?))
                .await
                .map_err(End::Storage)?;
            tracing::info!(event = "user_name_changed", user.name = %user_name);
            send_msg(socket, session, &ServerMsg::UserName { user_name }).await?;
            Ok(false)
        }
        ClientMsg::Unsubscribe { group } => {
            let id = device_id.to_string();
            let subs = app
                .db
                .call(move |c| {
                    db::delete_subscription(c, &id, &group)?;
                    Ok(db::subscriptions(c, &id)?)
                })
                .await
                .map_err(End::Storage)?;
            tracing::info!(event = "subscription_removed");
            send_msg(socket, session, &ServerMsg::Subscriptions { subscriptions: subs }).await?;
            Ok(false)
        }
        ClientMsg::Ready => Err(End::Protocol("duplicate ready")),
    }
}

async fn flush_deliveries(
    app: &AppState,
    socket: &mut WebSocket,
    session: &mut Session,
    device_id: &str,
    inflight: &mut HashMap<i64, Instant>,
) -> Result<(), End> {
    let id = device_id.to_string();
    let limit = app.config.devices.max_inflight;
    let pending = app
        .db
        .call({
            let cutoff = app.delivery_cutoff();
            move |c| Ok(db::pending_deliveries(c, &id, limit, cutoff)?)
        })
        .await
        .map_err(End::Storage)?;
    let still_pending: HashSet<i64> = pending.iter().map(|p| p.seq).collect();
    inflight.retain(|seq, _| still_pending.contains(seq));

    let now = Instant::now();
    let resend_after = app.config.devices.resend_after;
    let mut sent = Vec::new();
    for p in pending {
        if inflight.get(&p.seq).is_some_and(|at| now.duration_since(*at) < resend_after) {
            continue;
        }
        let msg = ServerMsg::Alert(AlertMsg {
            alert_id: p.alert_id,
            group_name: app.group_name(&p.group),
            group: p.group,
            title: p.title,
            message: p.message,
            created_at_ms: p.created_at,
            action: p.action,
        });
        send_msg(socket, session, &msg).await?;
        inflight.insert(p.seq, now);
        sent.push(p.seq);
    }
    if !sent.is_empty() {
        app.metrics.deliveries_sent.add(sent.len() as u64, &[]);
        app.db.call(move |c| Ok(db::mark_sent(c, &sent)?)).await.map_err(End::Storage)?;
    }
    Ok(())
}
