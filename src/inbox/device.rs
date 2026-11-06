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
use tokio::time::{timeout_at, Instant};
use zeroize::Zeroize;

use super::db::{DbError, DeviceKey, DeviceLimits, Redeem};
use super::enrollment;
use super::http::Resp;
use super::hub::Live;
use super::log;
use super::rights::{self, Standing};
use super::state::{App, SocketGuard};
use super::util::{is_hex_id, now_ms};
use super::ws::{self, Message, Socket};

/// A session WebSocket that does not finish the handshake within this time is closed. The time
/// counts from the upgrade: a ping or a pong does not extend it.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(15);
/// An enrollment connection that sends no message 1 within this time is closed.
const ENROLL_HELLO_TIMEOUT: Duration = Duration::from_secs(10);
const ENROLL_MAX_MESSAGE: usize = 8 * 1024;
/// The close code of a handshake that a newer handshake of the same address ended
/// (`inbox.devices.max_handshakes_per_address`). The app tries again after its back-off.
const TRY_AGAIN_LATER: u16 = 1013;

/// `GET /v1/enroll`: the upgrade to a WebSocket, then the enrollment.
pub fn enroll_ws(app: Arc<App>, ip: IpAddr, request: Request<Incoming>) -> Resp {
    let Some(guard) = SocketGuard::new(&app, ip) else {
        return refuse_busy(&ip);
    };
    let answer = ws::upgrade(request, ENROLL_MAX_MESSAGE, move |socket| async move {
        run_enroll(app, socket, ip, &guard).await
    });
    answer.unwrap_or_else(|refusal| refusal)
}

/// `GET /v1/session`: the upgrade to a WebSocket, then the session of a phone.
pub fn session_ws(app: Arc<App>, ip: IpAddr, request: Request<Incoming>) -> Resp {
    let Some(mut guard) = SocketGuard::new(&app, ip) else {
        return refuse_busy(&ip);
    };
    let answer = ws::upgrade(request, noise::MAX_MESSAGE, move |socket| async move {
        run_session(app, socket, ip, &mut guard).await
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

async fn run_enroll(app: Arc<App>, mut socket: Socket, ip: IpAddr, guard: &SocketGuard) {
    if !app.limiter.allowed(ip) {
        log::info!(event = "enroll_refused", reason = "rate_limited", client.address = %ip);
        close_with(&mut socket, close::RATE_LIMITED, "rate limited").await;
        return;
    }
    // A newer handshake of the same address ends only the wait for message 1. After message 1,
    // the enrollment ends in a short time, and it must not leave a used code without message 2.
    let first = tokio::select! {
        first = first_frame(&mut socket, Instant::now() + ENROLL_HELLO_TIMEOUT) => first,
        () = guard.ended_by_newer() => {
            close_with(&mut socket, TRY_AGAIN_LATER, "too many handshakes from this address").await;
            return;
        }
    };
    let message = match first {
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

    let limits = DeviceLimits {
        max_devices: app.config.devices.max_devices,
        max_per_account: app.config.devices.max_per_account,
    };
    let redeemed = app
        .db
        .redeem_code(
            digest,
            limits,
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
        Ok(Redeem::TooManyAccountDevices) => {
            refuse(
                &mut socket,
                ip,
                close::ENROLL_REFUSED,
                "too_many_account_devices",
            )
            .await;
            return;
        }
        Err(e) => {
            log::error!(event = "enroll_store_failed", error = %e);
            close_with(&mut socket, 1011, "storage error").await;
            return;
        }
    };
    // A code of a login that no account has gives no phone (SQLite): an account of the file that
    // left the file, or an old login of the superadmin. The state of the login ends, the new device
    // with it.
    if rights::standing(&app, &login) == Standing::Absent {
        if let Err(e) = enrollment::end_account(&app, &login).await {
            log::error!(event = "absent_account_end_failed", user.name = %login, error = %e);
            if let Err(e) = app.db.revoke_device(&device_id).await {
                log::error!(event = "enroll_cleanup_failed", device.id = %device_id, error = %e);
            }
        }
        refuse(&mut socket, ip, close::ENROLL_REFUSED, "no_account").await;
        return;
    }
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

/// A phone that finished the handshake: it proved the key of an enrolled device that is not revoked.
struct Proven {
    session: Session,
    device: DeviceKey,
    device_id: String,
    app_version: String,
}

/// Why a session handshake ended before the phone proved its key.
enum Unproven {
    /// The handshake proves no enrolled device. It counts against the client address.
    Failed {
        code: u16,
        reason: &'static str,
        kind: &'static str,
    },
    /// The device is known but revoked, the protocol is wrong, or the storage failed. It does not count.
    Refused(u16, &'static str),
    /// The connection ended.
    Gone,
}

/// The next binary frame of a handshake. A ping or a pong is skipped.
async fn handshake_frame(socket: &mut Socket) -> Result<Vec<u8>, Unproven> {
    loop {
        match socket.recv().await {
            Some(Ok(Message::Binary(b))) => return Ok(b.to_vec()),
            Some(Ok(Message::Ping(_) | Message::Pong(_) | Message::Frame(_))) => continue,
            Some(Ok(Message::Text(_))) => {
                return Err(Unproven::Refused(close::PROTOCOL, "text frame"))
            }
            Some(Ok(Message::Close(_)) | Err(_)) | None => return Err(Unproven::Gone),
        }
    }
}

/// The Noise handshake of a session, up to the frame `ready`. It reads the device row and changes
/// nothing, so it can stop at any point.
async fn handshake(app: &App, socket: &mut Socket) -> Result<Proven, Unproven> {
    let unknown = |reason| Unproven::Failed {
        code: close::UNKNOWN_DEVICE,
        reason,
        kind: "device_key",
    };
    let m1 = handshake_frame(socket).await?;
    let Ok((responder, remote_static, hello)) =
        Responder::start(&app.secrets.server_key.noise_private, &m1)
    else {
        return Err(unknown("handshake failed"));
    };
    let Ok(hello) = serde_json::from_slice::<SessionHello>(&hello) else {
        return Err(Unproven::Refused(close::PROTOCOL, "malformed hello"));
    };
    if hello.protocol != PROTOCOL_VERSION {
        return Err(Unproven::Refused(close::PROTOCOL, "unsupported protocol"));
    }
    if !is_hex_id(&hello.device_id) {
        return Err(unknown("unknown device"));
    }
    let device = match app.db.find_device(&hello.device_id).await {
        Ok(Some(d)) if d.public_key == remote_static && !d.revoked => d,
        Ok(Some(d)) if d.public_key == remote_static => {
            return Err(Unproven::Refused(close::REVOKED, "device revoked"))
        }
        Ok(_) => return Err(unknown("unknown device")),
        Err(e) => {
            log::error!(event = "session_lookup_failed", error = %e);
            return Err(Unproven::Refused(1011, "storage error"));
        }
    };

    let accept = SessionAccept {
        protocol: PROTOCOL_VERSION,
        server_name: app.server_name.clone(),
    };
    let accept = serde_json::to_vec(&accept).expect("serializes");
    let Ok((mut session, m2)) = responder.finish(&accept) else {
        return Err(Unproven::Gone);
    };
    if socket.send(Message::Binary(m2.into())).await.is_err() {
        return Err(Unproven::Gone);
    }
    // The session only counts once the device proves it holds the transport keys. A replay of
    // message 1 cannot do it.
    let frame = handshake_frame(socket).await?;
    let ready = session
        .decrypt(&frame)
        .ok()
        .and_then(|p| serde_json::from_slice::<ClientMsg>(&p).ok());
    if !matches!(ready, Some(ClientMsg::Ready)) {
        return Err(Unproven::Refused(close::PROTOCOL, "expected ready"));
    }
    Ok(Proven {
        session,
        device,
        device_id: hello.device_id,
        app_version: hello.app_version,
    })
}

async fn run_session(app: Arc<App>, mut socket: Socket, ip: IpAddr, guard: &mut SocketGuard) {
    // One deadline for the whole handshake: a ping or a pong does not extend it. The limiter of
    // the address applies only to a handshake that proves no device. A phone that proves its key
    // gets its session, whatever the other clients of its address did.
    let deadline = Instant::now() + HANDSHAKE_TIMEOUT;
    let outcome = tokio::select! {
        outcome = timeout_at(deadline, handshake(&app, &mut socket)) => match outcome {
            Ok(outcome) => outcome,
            Err(_) => Err(Unproven::Failed {
                code: 1001,
                reason: "handshake timeout",
                kind: "handshake_timeout",
            }),
        },
        () = guard.ended_by_newer() => {
            Err(Unproven::Refused(TRY_AGAIN_LATER, "too many handshakes from this address"))
        }
    };
    let proven = match outcome {
        Ok(proven) => proven,
        Err(Unproven::Failed { code, reason, kind }) => {
            app.metrics.auth_failure(kind);
            if app.limiter.allowed(ip) {
                app.limiter.record_failure(ip);
                close_with(&mut socket, code, reason).await;
            } else {
                close_with(&mut socket, close::RATE_LIMITED, "rate limited").await;
            }
            return;
        }
        Err(Unproven::Refused(code, reason)) => {
            close_with(&mut socket, code, reason).await;
            return;
        }
        Err(Unproven::Gone) => return,
    };
    guard.handshake_done();
    // The account of the phone must exist. An account of the file that left the file, or an old
    // login of the superadmin, ends here (SQLite). With a shared database, a login that this
    // instance does not know gets no session and the inbox writes nothing: the file of another
    // instance can hold it. The phone tries again later.
    match rights::standing(&app, &proven.device.user_name) {
        Standing::Absent => {
            let code = match enrollment::end_account(&app, &proven.device.user_name).await {
                Ok(_) => close::REVOKED,
                Err(e) => {
                    log::error!(event = "absent_account_end_failed", user.name = %proven.device.user_name, error = %e);
                    1011
                }
            };
            close_with(&mut socket, code, "device revoked").await;
            return;
        }
        Standing::NotHere => {
            log::info!(
                event = "session_refused",
                reason = "no_account_here",
                device.id = %proven.device_id,
                user.name = %proven.device.user_name
            );
            close_with(&mut socket, TRY_AGAIN_LATER, "no account here").await;
            return;
        }
        Standing::Exists | Standing::Unknown => {}
    }
    established(app, socket, proven, ip).await;
}

async fn established(app: Arc<App>, mut socket: Socket, proven: Proven, ip: IpAddr) {
    let Proven {
        mut session,
        device,
        device_id,
        app_version,
    } = proven;
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
    // A revocation during the handshake found no session to kick. The hub holds the session now,
    // so the row is read again. A revocation stores the row before it kicks: either this read
    // sees the revocation, or the kick finds this session.
    match app.db.find_device(&device_id).await {
        Ok(Some(d)) if d.public_key == device.public_key && !d.revoked => {}
        Ok(_) => {
            app.hub.unregister(&device_id, session_id);
            close_with(&mut socket, close::REVOKED, "device revoked").await;
            return;
        }
        Err(e) => {
            log::error!(event = "session_lookup_failed", error = %e);
            app.hub.unregister(&device_id, session_id);
            close_with(&mut socket, 1011, "storage error").await;
            return;
        }
    }
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
            if let Err(e) =
                flush_deliveries(app, socket, session, device_id, &user_name, &mut inflight).await
            {
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

/// Sends the deliveries that wait for the phone. An alert about a service goes to the phone only
/// while its account `login` may see the service: the rights can change after the fan-out. A
/// loaded directory that says no withdraws the delivery, so the phone never gets the alert. When
/// the portal cannot tell, the delivery waits. An alert that the phone received stays on it.
async fn flush_deliveries(
    app: &App,
    socket: &mut Socket,
    session: &mut Session,
    device_id: &str,
    login: &str,
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
    // The services of the account, read once, and only for an alert about a service.
    let mut services: Option<Option<rights::Services>> = None;
    let mut withdrawn = Vec::new();
    for p in pending {
        if let Some(service) = p.service.as_deref() {
            match services.get_or_insert_with(|| rights::services_of(app, login)) {
                Some(seen) if seen.sees(Some(service)) => {}
                Some(_) => {
                    withdrawn.push(p.seq);
                    continue;
                }
                None => continue,
            }
        }
        if inflight
            .get(&p.seq)
            .is_some_and(|at| now.duration_since(*at) < resend_after)
        {
            continue;
        }
        let (group, group_name) = p.target.frame(|id| app.group_name(id));
        let msg = ServerMsg::Alert(AlertMsg {
            alert_id: p.alert_id,
            kind: p.kind,
            group_name,
            group,
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
    if !withdrawn.is_empty() {
        let count = withdrawn.len();
        app.db.withdraw(withdrawn).await.map_err(End::Storage)?;
        log::info!(
            event = "deliveries_withdrawn_by_rights",
            device.id = %device_id,
            user.name = %login,
            count
        );
    }
    Ok(())
}
