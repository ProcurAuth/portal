//! Admin front: embedded assets, OPAQUE login and signed admin API.
//!
//! The admin password is used only inside OPAQUE, run in the browser by the
//! WebAssembly build of `inbox-core`. The OPAQUE session key never travels: every
//! later request carries an HMAC over method, path, timestamp, nonce and body, so a
//! passive observer of plain HTTP can neither learn the password nor forge or
//! replay a request. Active tampering with the served JavaScript is outside what
//! plain HTTP can prevent: put the admin listener behind TLS or on a private network.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::Json;
use axum::body::Bytes;
use axum::extract::{ConnectInfo, Path, Query, Request, State};
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, Uri, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use opentelemetry::KeyValue;
use inbox_core::login::{LoginServer, verify_mac};
use inbox_core::rng::Rng;
use inbox_core::suite::Role;
use serde::Deserialize;
use serde_json::json;
use zeroize::Zeroizing;

use crate::api::{ApiError, accept_alert};
use crate::db::{self, NewAlert};
use crate::net::client_ip;
use crate::state::AppState;
use crate::util::{is_hex_id, now_ms, random_id};

const LOGIN_TTL: Duration = Duration::from_secs(60);
const MAX_PENDING_LOGINS: usize = 256;
const MAX_SESSIONS: usize = 256;
const MAX_NONCES_PER_SESSION: usize = 100_000;

struct Session {
    key: Zeroizing<[u8; 32]>,
    expires: Instant,
    nonces: HashMap<String, Instant>,
}

#[derive(Default)]
pub struct AdminSessions {
    pending: Mutex<HashMap<String, (LoginServer, Instant)>>,
    sessions: Mutex<HashMap<String, Session>>,
}

// ---------------------------------------------------------------- assets

mod assets {
    include!(concat!(env!("OUT_DIR"), "/assets.rs"));
}

pub async fn static_asset(uri: Uri, headers: HeaderMap) -> Response {
    let path = uri.path().trim_start_matches('/');
    let path = if path.is_empty() { "index.html" } else { path };
    let Some((_, mime, body, etag)) = assets::ASSETS.iter().find(|(p, ..)| *p == path) else {
        return (StatusCode::NOT_FOUND, "not found").into_response();
    };
    let quoted = format!("\"{etag}\"");
    if headers.get(header::IF_NONE_MATCH).and_then(|v| v.to_str().ok()) == Some(quoted.as_str()) {
        return (StatusCode::NOT_MODIFIED, [(header::ETAG, quoted)]).into_response();
    }
    (
        [
            (header::CONTENT_TYPE, mime.to_string()),
            (header::ETAG, quoted),
            (header::CACHE_CONTROL, "no-cache".to_string()),
        ],
        *body,
    )
        .into_response()
}

/// Security headers on every admin response.
pub async fn security_headers(req: Request, next: Next) -> Response {
    let mut res = next.run(req).await;
    let h = res.headers_mut();
    h.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static(
            "default-src 'none'; script-src 'self' 'wasm-unsafe-eval'; style-src 'self'; img-src 'self' data:; \
             connect-src 'self'; base-uri 'none'; form-action 'none'; frame-ancestors 'none'",
        ),
    );
    h.insert(header::X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff"));
    h.insert(header::X_FRAME_OPTIONS, HeaderValue::from_static("DENY"));
    h.insert(header::REFERRER_POLICY, HeaderValue::from_static("no-referrer"));
    h.insert("cross-origin-opener-policy", HeaderValue::from_static("same-origin"));
    h.insert("cross-origin-resource-policy", HeaderValue::from_static("same-origin"));
    res
}

// ---------------------------------------------------------------- login

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LoginStart {
    #[serde(with = "inbox_core::b64::serde_bytes")]
    credential_request: Vec<u8>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LoginFinish {
    login_id: String,
    #[serde(with = "inbox_core::b64::serde_bytes")]
    credential_finalization: Vec<u8>,
}

fn too_many() -> ApiError {
    ApiError::new(StatusCode::TOO_MANY_REQUESTS, "rate_limited", "too many failed attempts, retry later")
}

pub async fn login_start(
    State(app): State<Arc<AppState>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(req): Json<LoginStart>,
) -> Result<Response, ApiError> {
    let ip = client_ip(peer, &headers, &app.config.server.trusted_proxies);
    if !app.limiter.allowed(ip) {
        return Err(too_many());
    }
    let (server, response) = LoginServer::start(
        &app.secrets.server_key.opaque,
        &app.secrets.admin_record,
        Role::Admin,
        &req.credential_request,
        &mut Rng::os(),
    )
    .map_err(|_| ApiError::new(StatusCode::BAD_REQUEST, "invalid_request", "malformed credential request"))?;
    let login_id = random_id();
    {
        let mut pending = app.admin_sessions.pending.lock().expect("admin lock");
        let now = Instant::now();
        pending.retain(|_, (_, exp)| *exp > now);
        if pending.len() >= MAX_PENDING_LOGINS {
            return Err(too_many());
        }
        pending.insert(login_id.clone(), (server, now + LOGIN_TTL));
    }
    Ok(Json(json!({
        "login_id": login_id,
        "credential_response": inbox_core::b64::encode(&response),
    }))
    .into_response())
}

pub async fn login_finish(
    State(app): State<Arc<AppState>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(req): Json<LoginFinish>,
) -> Result<Response, ApiError> {
    let ip = client_ip(peer, &headers, &app.config.server.trusted_proxies);
    if !app.limiter.allowed(ip) {
        return Err(too_many());
    }
    let pending = app.admin_sessions.pending.lock().expect("admin lock").remove(&req.login_id);
    let Some((server, expires)) = pending.filter(|(_, exp)| *exp > Instant::now()) else {
        return Err(ApiError::new(StatusCode::BAD_REQUEST, "login_expired", "login expired, start again"));
    };
    let _ = expires;
    let key = match server.finish(&req.credential_finalization) {
        Ok(k) => k,
        Err(_) => {
            app.limiter.record_failure(ip);
            app.metrics.auth_failures.add(1, &[KeyValue::new("kind", "admin_password")]);
            tracing::warn!(event = "admin_login_failed", client.address = %ip);
            return Err(ApiError::new(StatusCode::UNAUTHORIZED, "bad_password", "wrong password"));
        }
    };
    app.limiter.record_success(ip);
    let session_id = random_id();
    let ttl = app.config.admin.session_ttl;
    {
        let mut sessions = app.admin_sessions.sessions.lock().expect("admin lock");
        let now = Instant::now();
        sessions.retain(|_, s| s.expires > now);
        if sessions.len() >= MAX_SESSIONS {
            return Err(too_many());
        }
        sessions.insert(
            session_id.clone(),
            Session { key: key.derive("admin/request-mac"), expires: now + ttl, nonces: HashMap::new() },
        );
    }
    tracing::info!(event = "admin_login", client.address = %ip);
    Ok(Json(json!({ "session_id": session_id, "expires_in_ms": ttl.as_millis() as u64 })).into_response())
}

// ---------------------------------------------------------------- signed requests

/// Verifies the request signature. Returns the session id.
fn authorize(app: &AppState, method: &Method, uri: &Uri, headers: &HeaderMap, body: &[u8]) -> Result<String, ApiError> {
    let unauthorized = |m: &str| ApiError::new(StatusCode::UNAUTHORIZED, "unauthorized", m.to_string());
    let get = |name: &str| headers.get(name).and_then(|v| v.to_str().ok()).map(str::to_string);
    let (Some(sid), Some(ts), Some(nonce), Some(sig)) = (
        get("x-inbox-session"),
        get("x-inbox-timestamp"),
        get("x-inbox-nonce"),
        get("x-inbox-signature"),
    ) else {
        return Err(unauthorized("missing signature headers"));
    };
    let ts_ms: i64 = ts.parse().map_err(|_| unauthorized("bad timestamp"))?;
    let skew = app.config.security.admin_request_max_skew.as_millis() as i64;
    if (now_ms() - ts_ms).abs() > skew {
        return Err(unauthorized("timestamp outside allowed skew, check the clock"));
    }
    if nonce.len() < 16 || nonce.len() > 64 {
        return Err(unauthorized("bad nonce"));
    }
    let sig = inbox_core::b64::decode(&sig).ok_or_else(|| unauthorized("bad signature encoding"))?;
    let path = uri.path_and_query().map(|p| p.as_str()).unwrap_or("/");

    let mut sessions = app.admin_sessions.sessions.lock().expect("admin lock");
    let now = Instant::now();
    let session = sessions.get_mut(&sid).filter(|s| s.expires > now).ok_or_else(|| unauthorized("session expired"))?;
    verify_mac(
        &session.key,
        &[b"inbox/v1/admin-request", method.as_str().as_bytes(), path.as_bytes(), ts.as_bytes(), nonce.as_bytes(), body],
        &sig,
    )
    .map_err(|_| unauthorized("bad signature"))?;
    let window = Duration::from_millis(2 * skew as u64);
    if session.nonces.len() >= MAX_NONCES_PER_SESSION {
        session.nonces.retain(|_, seen| now.duration_since(*seen) < window);
    }
    if session.nonces.insert(nonce, now).is_some() {
        return Err(unauthorized("replayed request"));
    }
    Ok(sid)
}

fn no_store(v: serde_json::Value) -> Response {
    ([(header::CACHE_CONTROL, "no-store")], Json(v)).into_response()
}

pub async fn logout(
    State(app): State<Arc<AppState>>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, ApiError> {
    let sid = authorize(&app, &method, &uri, &headers, &body)?;
    app.admin_sessions.sessions.lock().expect("admin lock").remove(&sid);
    Ok(no_store(json!({ "ok": true })))
}

pub async fn overview(
    State(app): State<Arc<AppState>>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, ApiError> {
    authorize(&app, &method, &uri, &headers, &body)?;
    let (devices, (pending, oldest)) = app
        .db
        .call(|c| Ok((db::list_devices(c)?, db::pending_stats(c)?)))
        .await
        .map_err(ApiError::storage)?;
    let live = app.hub.snapshot();
    let now = now_ms();
    let unreachable_after = app.config.devices.unreachable_after.as_millis() as i64;
    let (mut online, mut unreachable, mut enrolled) = (0, 0, 0);
    let devices: Vec<_> = devices
        .into_iter()
        .map(|d| {
            let l = live.get(&d.id).cloned();
            let status = if d.revoked_at.is_some() {
                "revoked"
            } else if l.is_some() {
                "online"
            } else if !d.subscriptions.is_empty()
                && d.last_seen_at.is_none_or(|t| now - t > unreachable_after)
            {
                "unreachable"
            } else {
                "offline"
            };
            match status {
                "online" => online += 1,
                "unreachable" => unreachable += 1,
                _ => {}
            }
            if d.revoked_at.is_none() {
                enrolled += 1;
            }
            json!({ "device": d, "live": l, "status": status })
        })
        .collect();
    Ok(no_store(json!({
        "server": {
            "name": app.config.server.name,
            "version": env!("CARGO_PKG_VERSION"),
            "time_ms": now,
            "unreachable_after_ms": unreachable_after,
        },
        "groups": app.groups,
        "devices": devices,
        "stats": {
            "online": online,
            "enrolled": enrolled,
            "unreachable": unreachable,
            "pending_deliveries": pending,
            "oldest_pending_ms": oldest,
        },
    })))
}

#[derive(Deserialize)]
pub struct LimitQuery {
    limit: Option<u32>,
}

pub async fn alerts(
    State(app): State<Arc<AppState>>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    Query(q): Query<LimitQuery>,
    body: Bytes,
) -> Result<Response, ApiError> {
    authorize(&app, &method, &uri, &headers, &body)?;
    let limit = q.limit.unwrap_or(50).clamp(1, 500);
    let list = app.db.call(move |c| Ok(db::recent_alerts(c, limit)?)).await.map_err(ApiError::storage)?;
    Ok(no_store(json!({ "alerts": list })))
}

pub async fn alert(
    State(app): State<Arc<AppState>>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    Path(id): Path<String>,
    body: Bytes,
) -> Result<Response, ApiError> {
    authorize(&app, &method, &uri, &headers, &body)?;
    let not_found = || ApiError::new(StatusCode::NOT_FOUND, "not_found", "no such alert");
    if !is_hex_id(&id) {
        return Err(not_found());
    }
    let detail = app.db.call(move |c| Ok(db::alert_detail(c, &id)?)).await.map_err(ApiError::storage)?;
    Ok(no_store(serde_json::to_value(detail.ok_or_else(not_found)?).expect("serializes")))
}

pub async fn connections(
    State(app): State<Arc<AppState>>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    Query(q): Query<LimitQuery>,
    body: Bytes,
) -> Result<Response, ApiError> {
    authorize(&app, &method, &uri, &headers, &body)?;
    let limit = q.limit.unwrap_or(100).clamp(1, 1000);
    let list = app.db.call(move |c| Ok(db::recent_connections(c, limit)?)).await.map_err(ApiError::storage)?;
    Ok(no_store(json!({ "connections": list })))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AdminAlert {
    group: String,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    message: Option<String>,
    idempotency_key: String,
}

pub async fn send_alert(
    State(app): State<Arc<AppState>>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, ApiError> {
    authorize(&app, &method, &uri, &headers, &body)?;
    let req: AdminAlert = serde_json::from_slice(&body)
        .map_err(|e| ApiError::new(StatusCode::BAD_REQUEST, "invalid_json", e.to_string()))?;
    if req.idempotency_key.is_empty() || req.idempotency_key.len() > 128 {
        return Err(ApiError::new(StatusCode::BAD_REQUEST, "invalid_idempotency_key", "idempotency_key must be 1..128 bytes"));
    }
    let accepted = accept_alert(
        &app,
        NewAlert {
            group: req.group,
            title: req.title,
            message: req.message,
            source: "admin".into(),
            idempotency_key: req.idempotency_key,
        },
        "admin",
    )
    .await?;
    let status = if accepted.duplicate { StatusCode::OK } else { StatusCode::CREATED };
    Ok((status, [(header::CACHE_CONTROL, "no-store")], Json(accepted)).into_response())
}

pub async fn revoke(
    State(app): State<Arc<AppState>>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    Path(id): Path<String>,
    body: Bytes,
) -> Result<Response, ApiError> {
    authorize(&app, &method, &uri, &headers, &body)?;
    if !is_hex_id(&id) {
        return Err(ApiError::new(StatusCode::NOT_FOUND, "not_found", "no such device"));
    }
    let device = id.clone();
    let revoked = app.db.call(move |c| Ok(db::revoke_device(c, &device)?)).await.map_err(ApiError::storage)?;
    if revoked {
        app.hub.kick(&id, inbox_core::proto::close::REVOKED);
        tracing::info!(event = "device_revoked", device.id = %id);
    }
    Ok(no_store(json!({ "revoked": revoked })))
}
