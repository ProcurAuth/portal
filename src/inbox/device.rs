//! The endpoints of the phones: enrollment (`/v1/enroll`) and sessions (`/v1/session`).
//!
//! The delivery contract of one phone:
//! - The inbox sends each delivery that the phone did not confirm as `received`. It sends the
//!   oldest first, at most `inbox.devices.max_inflight` at a time. It sends it again after
//!   `inbox.devices.resend_after`.
//! - The phone stores the alert before it confirms `received`. It ignores a repeated
//!   `alert_id`. A retransmission therefore never shows twice.
//! - The inbox never drops an alert. A phone that is offline gets its backlog when it connects.

use std::collections::{HashMap, HashSet};
use std::net::IpAddr;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use hyper::body::Incoming;
use hyper::Request;
use inbox_core::enroll::EnrollResponder;
use inbox_core::noise::{self, Responder, Session};
use inbox_core::proto::{
    close, AckStage, AlertMsg, ClientMsg, EnrollAccept, ServerMsg, SessionAccept, SessionHello,
};
use inbox_core::{Error as CoreError, PROTOCOL_VERSION};
use sha2::{Digest, Sha256};
use tokio::sync::{watch, Notify};
use tokio::time::{timeout, timeout_at, Instant};
use zeroize::Zeroize;

use super::db::{DbError, DeviceKey, Redeem};
use super::http::Resp;
use super::hub::Live;
use super::log;
use super::rights;
use super::state::{App, SocketGuard};
use super::util::{is_hex_id, now_ms};
use super::ws::{self, Message, Socket};

const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(15);
/// An enrollment connection that sends no message 1 within this time is closed.
const ENROLL_HELLO_TIMEOUT: Duration = Duration::from_secs(10);
const ENROLL_MAX_MESSAGE: usize = 8 * 1024;

/// `GET /v1/enroll`: the upgrade to a WebSocket, then the enrollment.
pub fn enroll_ws(app: Arc<App>, ip: IpAddr, request: Request<Incoming>) -> Resp {
    let Some(guard) = SocketGuard::new(&app) else {
        return refuse_busy(&ip);
    };
    let answer = ws::upgrade(request, ENROLL_MAX_MESSAGE, move |socket| async move {
        let _guard = guard;
        run_enroll(app, socket, ip).await
    });
    answer.unwrap_or_else(|refusal| refusal)
}

/// `GET /v1/session`: the upgrade to a WebSocket, then the session of a phone.
pub fn session_ws(app: Arc<App>, ip: IpAddr, request: Request<Incoming>) -> Resp {
    let Some(guard) = SocketGuard::new(&app) else {
        return refuse_busy(&ip);
    };
    let answer = ws::upgrade(request, noise::MAX_MESSAGE, move |socket| async move {
        let _guard = guard;
        run_session(app, socket, ip).await
    });
    answer.unwrap_or_else(|refusal| refusal)
}

/// The budget of sessions is full: the phone gets `503` and no WebSocket exists.
fn refuse_busy(ip: &IpAddr) -> Resp {
    log::warn!(event = "session_budget_full", client.address = %ip);
    super::http::busy()
}

async fn close_with(socket: &mut Socket, code: u16, reason: &str) {
    socket.close_with(code, reason).await;
}

async fn recv_binary(socket: &mut Socket, limit: Duration) -> Option<Vec<u8>> {
    loop {
        match timeout(limit, socket.recv()).await {
            Ok(Some(Ok(Message::Binary(b)))) => return Some(b.to_vec()),
            Ok(Some(Ok(Message::Ping(_) | Message::Pong(_)))) => continue,
            _ => return None,
        }
    }
}

// ---------------------------------------------------------------- enrollment

/// The first frame of an enrollment connection.
enum First {
    /// Noise message 1.
    Message(Vec<u8>),
    /// A text frame. Protocol 4 uses binary frames only.
    Text,
    /// The phone did not send message 1 in time.
    Silent,
    /// The connection ended.
    Gone,
}

/// Waits for the first data frame until `deadline`. A ping or a pong does not extend the deadline.
async fn first_frame(socket: &mut Socket, deadline: Instant) -> First {
    loop {
        match timeout_at(deadline, socket.recv()).await {
            Err(_) => return First::Silent,
            Ok(Some(Ok(Message::Binary(b)))) => return First::Message(b.to_vec()),
            Ok(Some(Ok(Message::Text(_)))) => return First::Text,
            Ok(Some(Ok(Message::Ping(_) | Message::Pong(_) | Message::Frame(_)))) => continue,
            Ok(None | Some(Err(_)) | Some(Ok(Message::Close(_)))) => return First::Gone,
        }
    }
}

/// Closes an enrollment connection after a refusal. The phone gets no message 2. The reason is for
/// the log only: the phone gets the same close code for every cause that concerns the code.
async fn refuse(socket: &mut Socket, ip: IpAddr, code: u16, reason: &str) {
    log::info!(event = "enroll_refused", reason, client.address = %ip);
    close_with(socket, code, "enrollment refused").await;
}

async fn run_enroll(app: Arc<App>, mut socket: Socket, ip: IpAddr) {
    if !app.limiter.allowed(ip) {
        log::info!(event = "enroll_refused", reason = "rate_limited", client.address = %ip);
        close_with(&mut socket, close::RATE_LIMITED, "rate limited").await;
        return;
    }
    let message = match first_frame(&mut socket, Instant::now() + ENROLL_HELLO_TIMEOUT).await {
        First::Message(m) => m,
        First::Text => {
            app.limiter.record_failure(ip);
            refuse(&mut socket, ip, close::PROTOCOL, "text_frame").await;
            return;
        }
        First::Silent => {
            app.limiter.record_failure(ip);
            refuse(&mut socket, ip, close::ENROLL_REFUSED, "no_message").await;
            return;
        }
        First::Gone => return,
    };
    // Every started enrollment counts as an attempt until it succeeds. A phone that leaves after
    // a refusal may never come back to tell the server.
    app.limiter.record_failure(ip);
    let (responder, device_public, mut hello) =
        match EnrollResponder::start(&app.secrets.server_key.noise_private, &message) {
            Ok(started) => started,
            Err(CoreError::UnsupportedProtocol(_)) => {
                refuse(&mut socket, ip, close::PROTOCOL, "unsupported_protocol").await;
                return;
            }
            Err(_) => {
                app.metrics.auth_failure("enroll_message");
                refuse(&mut socket, ip, close::ENROLL_REFUSED, "unreadable_message").await;
                return;
            }
        };
    let digest: [u8; 32] = Sha256::digest(&hello.secret).into();
    hello.secret.zeroize();

    let redeemed = app
        .db
        .redeem_code(
            digest,
            app.config.devices.max_devices,
            hello.device_name.clone(),
            device_public,
            ip.to_string(),
        )
        .await;
    let (device_id, login) = match redeemed {
        Ok(Redeem::Enrolled { device_id, login }) => (device_id, login),
        Ok(Redeem::Refused(why)) => {
            app.metrics.auth_failure("enroll_code");
            refuse(&mut socket, ip, close::ENROLL_REFUSED, why.as_str()).await;
            return;
        }
        Ok(Redeem::TooManyDevices) => {
            refuse(&mut socket, ip, close::ENROLL_REFUSED, "too_many_devices").await;
            return;
        }
        Err(e) => {
            log::error!(event = "enroll_store_failed", error = %e);
            close_with(&mut socket, 1011, "storage error").await;
            return;
        }
    };
    // The code is used and the device is stored. With a replica, the phone learns of it only
    // after the copy holds it, so a takeover never loses a device that believes it is enrolled.
    if let Err(e) = app.replica.sync().await {
        app.metrics.replica_sync_failure();
        log::warn!(event = "replica_sync_failed", error = %e);
        if app.replica.strict_alerts() {
            if let Err(e) = app.db.revoke_device(&device_id).await {
                log::error!(event = "enroll_cleanup_failed", device.id = %device_id, error = %e);
            }
            refuse(
                &mut socket,
                ip,
                close::ENROLL_REFUSED,
                "replica_sync_failed",
            )
            .await;
            return;
        }
    }
    let accept = EnrollAccept {
        protocol: PROTOCOL_VERSION,
        device_id: device_id.clone(),
        server_name: app.server_name.clone(),
        user_name: login.clone(),
    };
    let reply = match responder.finish(&accept) {
        Ok(m) => m,
        Err(e) => {
            log::error!(event = "enroll_accept_failed", device.id = %device_id, error = %e);
            close_with(&mut socket, 1011, "internal error").await;
            return;
        }
    };
    app.limiter.record_success(ip);
    app.metrics.enrollment();
    log::info!(
        event = "device_enrolled",
        device.id = %device_id,
        device.name = %hello.device_name,
        user.name = %login,
        client.address = %ip
    );
    let _ = socket.send(Message::Binary(reply.into())).await;
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

async fn send_msg(socket: &mut Socket, session: &mut Session, msg: &ServerMsg) -> Result<(), End> {
    let json = serde_json::to_vec(msg).expect("protocol messages serialize");
    let frame = session
        .encrypt(&json)
        .map_err(|_| End::Protocol("frame too large"))?;
    socket
        .send(Message::Binary(frame.into()))
        .await
        .map_err(|_| End::Send)
}

async fn run_session(app: Arc<App>, mut socket: Socket, ip: IpAddr) {
    if !app.limiter.allowed(ip) {
        close_with(&mut socket, close::RATE_LIMITED, "rate limited").await;
        return;
    }
    let reject = |app: &App, ip: IpAddr| {
        app.limiter.record_failure(ip);
        app.metrics.auth_failure("device_key");
    };

    let Some(m1) = recv_binary(&mut socket, HANDSHAKE_TIMEOUT).await else {
        return;
    };
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
    let device = match app.db.find_device(&id).await {
        Ok(d) => d,
        Err(e) => {
            log::error!(event = "session_lookup_failed", error = %e);
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

    let accept = SessionAccept {
        protocol: PROTOCOL_VERSION,
        server_name: app.server_name.clone(),
    };
    let accept = serde_json::to_vec(&accept).expect("serializes");
    let Ok((mut session, m2)) = responder.finish(&accept) else {
        return;
    };
    if socket.send(Message::Binary(m2.into())).await.is_err() {
        return;
    }
    // The session only counts once the device proves it holds the transport keys.
    let ready = match recv_binary(&mut socket, HANDSHAKE_TIMEOUT).await {
        Some(frame) => session
            .decrypt(&frame)
            .ok()
            .and_then(|p| serde_json::from_slice::<ClientMsg>(&p).ok()),
        None => return,
    };
    if !matches!(ready, Some(ClientMsg::Ready)) {
        close_with(&mut socket, close::PROTOCOL, "expected ready").await;
        return;
    }

    established(
        app,
        socket,
        session,
        device,
        device_id,
        hello.app_version,
        ip,
    )
    .await;
}

async fn established(
    app: Arc<App>,
    mut socket: Socket,
    mut session: Session,
    device: DeviceKey,
    device_id: String,
    app_version: String,
    ip: IpAddr,
) {
    let session_id = app.next_session_id.fetch_add(1, Ordering::Relaxed);
    let (kick_tx, mut kick_rx) = watch::channel(None);
    let wake = Arc::new(Notify::new());
    let last_seen = Arc::new(AtomicI64::new(now_ms()));
    let resub = Arc::new(AtomicBool::new(false));
    let app_version =
        inbox_core::enroll::validate_device_name(&app_version).unwrap_or_else(|_| "unknown".into());
    let connected_at = now_ms();
    app.hub.register(
        &device_id,
        Live {
            session_id,
            ip: ip.to_string(),
            app_version: app_version.clone(),
            connected_at,
            last_seen: last_seen.clone(),
            wake: wake.clone(),
            kick: kick_tx,
            resub: resub.clone(),
        },
        close::REPLACED,
    );
    let connection_id = match app
        .db
        .record_connect(&device_id, &ip.to_string(), &app_version)
        .await
    {
        Ok(id) => id,
        Err(e) => {
            log::error!(event = "session_record_failed", error = %e);
            app.hub.unregister(&device_id, session_id);
            close_with(&mut socket, 1011, "storage error").await;
            return;
        }
    };
    if connection_id < 0 {
        // Without the database nobody else learns of this session from it: tell the other instances directly.
        app.cluster.forward_replaced(&device_id, connected_at);
    }
    app.metrics.session_opened();
    log::info!(
        event = "device_connected",
        recorded = connection_id >= 0,
        device.id = %device_id,
        device.name = %device.name,
        user.name = %device.user_name,
        client.address = %ip,
        app.version = %app_version
    );

    let end = deliver(
        &app,
        &mut socket,
        &mut session,
        &device_id,
        &mut kick_rx,
        &wake,
        &last_seen,
        &resub,
    )
    .await;

    app.hub.unregister(&device_id, session_id);
    app.metrics.session_ended();
    app.metrics.session_closed(end.reason());
    let reason = end.reason();
    if let Err(e) = app
        .db
        .record_disconnect(connection_id, &device_id, reason)
        .await
    {
        log::error!(event = "session_disconnect_record_failed", error = %e);
    }
    match &end {
        End::Code(code, r) => close_with(&mut socket, *code, r).await,
        End::Timeout => close_with(&mut socket, 1001, "session timeout").await,
        End::Protocol(r) => close_with(&mut socket, close::PROTOCOL, r).await,
        End::Storage(e) => {
            log::error!(event = "session_storage_error", error = %e);
            close_with(&mut socket, 1011, "storage error").await;
        }
        End::PeerClosed | End::Send => {}
    }
    log::info!(
        event = "device_disconnected",
        reason,
        device.id = %device_id,
        device.name = %device.name,
        user.name = %device.user_name,
        client.address = %ip
    );
}

// The session loop needs these handles of one session; a struct for them would only rename the arguments.
#[allow(clippy::too_many_arguments)]
async fn deliver(
    app: &App,
    socket: &mut Socket,
    session: &mut Session,
    device_id: &str,
    kick_rx: &mut watch::Receiver<Option<u16>>,
    wake: &Notify,
    last_seen: &AtomicI64,
    resub: &AtomicBool,
) -> End {
    let cfg = &app.config.devices;
    let (mut subscriptions, user_name) = match app.db.profile(device_id).await {
        Ok(p) => p,
        Err(e) => return End::Storage(e),
    };
    // The phone sees only the groups that its account may subscribe to. When the portal cannot
    // tell, the phone sees all the groups and nothing changes.
    let allowed = rights::subscribable_groups(app, &user_name);
    let groups = match &allowed {
        Some(allowed) => app
            .groups
            .iter()
            .filter(|g| allowed.contains(&g.id))
            .cloned()
            .collect(),
        None => app.groups.clone(),
    };
    if let Some(allowed) = &allowed {
        // A loaded directory says that the account lost the right: the subscription goes now.
        for group in subscriptions
            .clone()
            .iter()
            .filter(|g| !allowed.contains(*g))
        {
            match app.db.unsubscribe(device_id, group).await {
                Ok(left) => {
                    log::info!(
                        event = "subscription_removed_by_rights",
                        device.id = %device_id,
                        user.name = %user_name,
                        group = %group
                    );
                    subscriptions = left;
                }
                Err(e) => log::warn!(event = "rights_unsubscribe_failed", error = %e),
            }
        }
    }
    let welcome = ServerMsg::Welcome {
        server_name: app.server_name.clone(),
        groups,
        subscriptions,
        heartbeat_interval_ms: cfg.heartbeat_interval.as_millis() as u64,
        offline_warning_after_ms: cfg.offline_warning_after.as_millis() as u64,
        server_time_ms: now_ms(),
        user_name: user_name.clone(),
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
        if resub.swap(false, Ordering::SeqCst) {
            // The rights of the account changed the subscriptions. The phone learns it.
            match app.db.profile(device_id).await {
                Ok((subscriptions, _)) => {
                    if let Err(e) =
                        send_msg(socket, session, &ServerMsg::Subscriptions { subscriptions }).await
                    {
                        return e;
                    }
                }
                Err(e) if e.is_unavailable() => {}
                Err(e) => return End::Storage(e),
            }
        }
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
                    Ok(Some(Ok(Message::Frame(_)))) => continue,
                };
                deadline = Instant::now() + cfg.session_timeout;
                last_seen.store(now_ms(), Ordering::Relaxed);
                let Ok(plain) = session.decrypt(&frame) else { return End::Protocol("decrypt failed") };
                let Ok(msg) = serde_json::from_slice::<ClientMsg>(&plain) else { return End::Protocol("malformed message") };
                match handle(app, socket, session, device_id, &user_name, msg, &mut inflight).await {
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
    app: &App,
    socket: &mut Socket,
    session: &mut Session,
    device_id: &str,
    login: &str,
    msg: ClientMsg,
    inflight: &mut HashMap<i64, Instant>,
) -> Result<bool, End> {
    match msg {
        ClientMsg::Ping { nonce } => {
            send_msg(socket, session, &ServerMsg::Pong { nonce }).await?;
            Ok(false)
        }
        ClientMsg::Ack {
            alert_id,
            stage: AckStage::Received,
        } => {
            if !is_hex_id(&alert_id) {
                return Err(End::Protocol("malformed alert id"));
            }
            let outcome = app
                .db
                .ack(device_id, &alert_id)
                .await
                .map_err(End::Storage)?;
            app.metrics.receipt();
            if let Some(o) = outcome {
                if o.seq < 0 {
                    // A delivery of the spool: the other instances hold a copy of it.
                    app.cluster.forward_ack(device_id, &alert_id, now_ms());
                }
                if let Some(ms) = o.first_receipt_latency_ms {
                    app.metrics.delivery_latency(ms.max(0) as f64 / 1000.0);
                }
                inflight.remove(&o.seq);
            }
            Ok(true)
        }
        ClientMsg::Subscribe { group } => {
            if !app.group_names.contains_key(&group) {
                send_msg(
                    socket,
                    session,
                    &ServerMsg::Error {
                        code: "unknown_group".into(),
                        message: group,
                    },
                )
                .await?;
                return Ok(false);
            }
            // The account needs `group.subscribe` on the group. When the portal cannot tell, the
            // phone gets a retry error: a new subscription never rests on a guess.
            match rights::subscribable_groups(app, login) {
                Some(allowed) if allowed.contains(&group) => {}
                Some(_) => {
                    send_msg(
                        socket,
                        session,
                        &ServerMsg::Error {
                            code: "forbidden_group".into(),
                            message: group,
                        },
                    )
                    .await?;
                    return Ok(false);
                }
                None => {
                    send_msg(
                        socket,
                        session,
                        &ServerMsg::Error {
                            code: "unavailable".into(),
                            message: "the rights are not ready, retry in a moment".into(),
                        },
                    )
                    .await?;
                    return Ok(false);
                }
            }
            let subs = match app.db.subscribe(device_id, &group).await {
                Ok(subs) => subs,
                Err(e) if e.is_unavailable() => return unavailable(socket, session).await,
                Err(e) => return Err(End::Storage(e)),
            };
            log::info!(event = "subscription_changed");
            send_msg(
                socket,
                session,
                &ServerMsg::Subscriptions {
                    subscriptions: subs,
                },
            )
            .await?;
            Ok(false)
        }
        ClientMsg::Unsubscribe { group } => {
            let subs = match app.db.unsubscribe(device_id, &group).await {
                Ok(subs) => subs,
                Err(e) if e.is_unavailable() => return unavailable(socket, session).await,
                Err(e) => return Err(End::Storage(e)),
            };
            log::info!(event = "subscription_removed");
            send_msg(
                socket,
                session,
                &ServerMsg::Subscriptions {
                    subscriptions: subs,
                },
            )
            .await?;
            Ok(false)
        }
        ClientMsg::Ready => Err(End::Protocol("duplicate ready")),
    }
}

/// A change that needs the database, while it is away: the phone is told, and the session goes on.
async fn unavailable(socket: &mut Socket, session: &mut Session) -> Result<bool, End> {
    let error = ServerMsg::Error {
        code: "unavailable".into(),
        message: "the database is away, retry in a moment".into(),
    };
    send_msg(socket, session, &error).await?;
    Ok(false)
}

async fn flush_deliveries(
    app: &App,
    socket: &mut Socket,
    session: &mut Session,
    device_id: &str,
    inflight: &mut HashMap<i64, Instant>,
) -> Result<(), End> {
    let limit = app.config.devices.max_inflight;
    let pending = app
        .db
        .pending_deliveries(device_id, limit, app.delivery_cutoff())
        .await
        .map_err(End::Storage)?;
    let still_pending: HashSet<i64> = pending.iter().map(|p| p.seq).collect();
    inflight.retain(|seq, _| still_pending.contains(seq));

    let now = Instant::now();
    let resend_after = app.config.devices.resend_after;
    let mut sent = Vec::new();
    for p in pending {
        if inflight
            .get(&p.seq)
            .is_some_and(|at| now.duration_since(*at) < resend_after)
        {
            continue;
        }
        let msg = ServerMsg::Alert(AlertMsg {
            alert_id: p.alert_id,
            kind: p.kind,
            group_name: app.group_name(&p.group),
            group: p.group,
            title: p.title,
            created_at_ms: p.created_at,
        });
        send_msg(socket, session, &msg).await?;
        inflight.insert(p.seq, now);
        sent.push(p.seq);
    }
    if !sent.is_empty() {
        app.metrics.deliveries_sent(sent.len() as u64);
        app.db.mark_sent(sent).await.map_err(End::Storage)?;
    }
    Ok(())
}
