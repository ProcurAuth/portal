//! Admin front: embedded assets and admin API.
//!
//! There is no password by design: whoever reaches `server.admin_listen` can send
//! alerts and revoke devices, so that listener belongs on localhost, a private
//! network or behind an authenticating reverse proxy. What the server enforces is
//! that a *web page* cannot act on behalf of someone who can reach it:
//! - the `Host` header must be one of `admin.allowed_hosts` (DNS rebinding);
//! - cross-site requests (`Sec-Fetch-Site`) are refused;
//! - state-changing requests need `Content-Type: application/json` and
//!   `X-Inbox-Request: 1`, which a cross-origin page cannot send without a CORS
//!   preflight that this server never grants.

use std::net::SocketAddr;
use std::sync::Arc;

use axum::Json;
use axum::body::Bytes;
use axum::extract::{ConnectInfo, Path, Query, Request, State};
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, Uri, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use serde::Deserialize;
use serde_json::json;

use crate::api::{ApiError, accept_alert};
use crate::db::{self, NewAlert};
use crate::state::AppState;
use crate::util::{is_hex_id, now_ms};

// ---------------------------------------------------------------- assets

mod assets {
    include!(concat!(env!("OUT_DIR"), "/assets.rs"));
}

fn find_asset(
    path: &str,
) -> Option<&'static (&'static str, &'static str, &'static [u8], &'static str)> {
    assets::ASSETS.iter().find(|(p, ..)| *p == path)
}

/// Maps a request path to an embedded file, whatever prefix reverse proxies added:
/// the longest suffix that names an asset wins, `.../doc` is the API documentation
/// and anything else is the dashboard.
fn resolve_asset(path: &str) -> &'static str {
    let segments: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
    for start in 0..segments.len() {
        let candidate = segments[start..].join("/");
        if let Some((name, ..)) = find_asset(&candidate) {
            return name;
        }
    }
    if segments.last() == Some(&"doc") { "doc.html" } else { "index.html" }
}

/// Rewrites `/<any prefix>/api/...` to `/api/...` so that the admin front works
/// behind reverse proxies that keep their path prefix, with no configuration.
pub async fn rewrite_request(mut req: Request) -> Request {
    let path = req.uri().path();
    let rewritten = if let Some(i) = path.rfind("/api/admin/") {
        (i > 0).then(|| path[i..].to_string())
    } else if path.ends_with("/api/version") && path != "/api/version" {
        Some("/api/version".to_string())
    } else {
        None
    };
    if let Some(new_path) = rewritten {
        let pq = match req.uri().query() {
            Some(q) => format!("{new_path}?{q}"),
            None => new_path,
        };
        if let Ok(uri) = pq.parse::<Uri>() {
            *req.uri_mut() = uri;
        }
    }
    req
}

pub async fn static_asset(uri: Uri, headers: HeaderMap) -> Response {
    let name = resolve_asset(uri.path());
    let Some((_, mime, body, etag)) = find_asset(name) else {
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

// ---------------------------------------------------------------- request guard

/// Host allow-list, cross-site and CSRF checks, then security headers.
pub async fn guard(
    State(app): State<Arc<AppState>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    req: Request,
    next: Next,
) -> Response {
    // A trusted reverse proxy already routed on its own host name: any Host is
    // fine through it. Direct clients must use a listed host. Loopback peers are
    // never exempt: SSH tunnels and local browsers arrive from loopback.
    let via_proxy = exempt_from_host_check(peer.ip(), &app.config.server.trusted_proxies);
    if let Err(e) = check_request(&app, req.method(), req.headers(), via_proxy) {
        if req.uri().path().starts_with("/api/") {
            return with_security_headers(e.into_response());
        }
        // A person opening the front on an unlisted host gets a readable page.
        let text = format!(
            "Inbox: {}\n\nAdd the host used in the address bar to admin.allowed_hosts \
             in the server configuration, then restart Inbox.\n",
            e.message()
        );
        let res = (e.status(), [(header::CONTENT_TYPE, "text/plain; charset=utf-8")], text);
        return with_security_headers(res.into_response());
    }
    with_security_headers(next.run(req).await)
}

/// Whether a request may use any `Host` because it comes from a trusted reverse
/// proxy. Loopback is never exempt: SSH tunnels and local browsers use it, and a
/// DNS-rebinding page would reach the admin front through them.
pub(crate) fn exempt_from_host_check(peer: std::net::IpAddr, trusted: &[ipnet::IpNet]) -> bool {
    let ip = peer.to_canonical();
    !ip.is_loopback() && trusted.iter().any(|n| n.contains(&ip))
}

fn check_request(
    app: &AppState,
    method: &Method,
    headers: &HeaderMap,
    via_proxy: bool,
) -> Result<(), ApiError> {
    let host = headers.get(header::HOST).and_then(|v| v.to_str().ok()).unwrap_or("");
    if !via_proxy && !app.config.admin.allowed_hosts.iter().any(|h| h.eq_ignore_ascii_case(host)) {
        tracing::warn!(event = "admin_host_refused", host = %host);
        return Err(ApiError::new(
            StatusCode::FORBIDDEN,
            "host_not_allowed",
            format!("host {host:?} is not listed in admin.allowed_hosts"),
        ));
    }
    let site = headers.get("sec-fetch-site").and_then(|v| v.to_str().ok());
    if site.is_some_and(|s| s != "same-origin" && s != "none") {
        return Err(ApiError::new(StatusCode::FORBIDDEN, "csrf", "cross-site request refused"));
    }
    if method != Method::GET && method != Method::HEAD {
        let json = headers
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| v.split(';').next().is_some_and(|m| m.trim() == "application/json"));
        let marker = headers.get("x-inbox-request").and_then(|v| v.to_str().ok()) == Some("1");
        if !json || !marker {
            return Err(ApiError::new(
                StatusCode::FORBIDDEN,
                "csrf",
                "requests must send Content-Type: application/json and X-Inbox-Request: 1",
            ));
        }
    }
    Ok(())
}

/// Inline scripts are allowed by hash only (computed at build time).
fn csp() -> &'static HeaderValue {
    static CSP: std::sync::OnceLock<HeaderValue> = std::sync::OnceLock::new();
    CSP.get_or_init(|| {
        let policy = format!(
            "default-src 'none'; script-src 'self' {}; style-src 'self'; img-src 'self' data:; \
             connect-src 'self'; base-uri 'none'; form-action 'none'; frame-ancestors 'none'",
            assets::CSP_SCRIPT_HASHES
        );
        HeaderValue::from_str(&policy).expect("CSP is ASCII")
    })
}

fn with_security_headers(mut res: Response) -> Response {
    let h = res.headers_mut();
    h.insert(header::CONTENT_SECURITY_POLICY, csp().clone());
    h.insert(header::X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff"));
    h.insert(header::X_FRAME_OPTIONS, HeaderValue::from_static("DENY"));
    h.insert(header::REFERRER_POLICY, HeaderValue::from_static("no-referrer"));
    h.insert("cross-origin-opener-policy", HeaderValue::from_static("same-origin"));
    h.insert("cross-origin-resource-policy", HeaderValue::from_static("same-origin"));
    res
}

// ---------------------------------------------------------------- API

fn no_store(v: serde_json::Value) -> Response {
    ([(header::CACHE_CONTROL, "no-store")], Json(v)).into_response()
}

pub async fn overview(State(app): State<Arc<AppState>>) -> Result<Response, ApiError> {
    let (devices, (pending, oldest)) = app
        .db
        .call({
            let cutoff = app.delivery_cutoff();
            move |c| Ok((db::list_devices(c, cutoff)?, db::pending_stats(c, cutoff)?))
        })
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
            // A delivery younger than this is still in flight, not failed.
            "resend_after_ms": app.config.devices.resend_after.as_millis() as u64,
            "heartbeat_interval_ms": app.config.devices.heartbeat_interval.as_millis() as u64,
            // Port of the device / producer API listener, for the API documentation.
            "api_port": app.config.server.listen.port(),
        },
        // Producers and their groups; tokens are never exposed.
        "producers": app.secrets.producers.iter().map(|p| json!({
            "name": p.name,
            "groups": p.groups,
            "webhook_send_resolved": p.webhook_send_resolved,
        })).collect::<Vec<_>>(),
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
    /// Only connections of this user pseudonym.
    user: Option<String>,
    /// Only alerts created at or after this Unix time in milliseconds.
    since_ms: Option<i64>,
}

pub async fn alerts(
    State(app): State<Arc<AppState>>,
    Query(q): Query<LimitQuery>,
) -> Result<Response, ApiError> {
    let limit = q.limit.unwrap_or(50).clamp(1, 500);
    let since = q.since_ms.unwrap_or(0).max(0);
    let list = app
        .db
        .call({
            let cutoff = app.delivery_cutoff();
            move |c| Ok(db::recent_alerts(c, limit, since, cutoff)?)
        })
        .await
        .map_err(ApiError::storage)?;
    Ok(no_store(json!({ "alerts": list })))
}

pub async fn alert(
    State(app): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<Response, ApiError> {
    let not_found = || ApiError::new(StatusCode::NOT_FOUND, "not_found", "no such alert");
    if !is_hex_id(&id) {
        return Err(not_found());
    }
    let detail = app
        .db
        .call({
            let cutoff = app.delivery_cutoff();
            move |c| Ok(db::alert_detail(c, &id, cutoff)?)
        })
        .await
        .map_err(ApiError::storage)?;
    Ok(no_store(serde_json::to_value(detail.ok_or_else(not_found)?).expect("serializes")))
}

pub async fn connections(
    State(app): State<Arc<AppState>>,
    Query(q): Query<LimitQuery>,
) -> Result<Response, ApiError> {
    let limit = q.limit.unwrap_or(100).clamp(1, 1000);
    let user = q.user.filter(|u| !u.trim().is_empty());
    let list = app
        .db
        .call(move |c| Ok(db::recent_connections(c, limit, user.as_deref())?))
        .await
        .map_err(ApiError::storage)?;
    Ok(no_store(json!({ "connections": list })))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AdminAlert {
    group: String,
    title: String,
    idempotency_key: String,
}

pub async fn send_alert(
    State(app): State<Arc<AppState>>,
    body: Bytes,
) -> Result<Response, ApiError> {
    let req: AdminAlert = serde_json::from_slice(&body)
        .map_err(|e| ApiError::new(StatusCode::BAD_REQUEST, "invalid_json", e.to_string()))?;
    if req.idempotency_key.is_empty() || req.idempotency_key.len() > 128 {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "invalid_idempotency_key",
            "idempotency_key must be 1..128 bytes",
        ));
    }
    let accepted = accept_alert(
        &app,
        NewAlert {
            group: req.group,
            title: req.title,
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
    Path(id): Path<String>,
) -> Result<Response, ApiError> {
    if !is_hex_id(&id) {
        return Err(ApiError::new(StatusCode::NOT_FOUND, "not_found", "no such device"));
    }
    let device = id.clone();
    let revoked = app
        .db
        .call(move |c| Ok(db::revoke_device(c, &device)?))
        .await
        .map_err(ApiError::storage)?;
    if revoked {
        if let Err(e) = app.replica.sync().await {
            app.metrics.replica_sync_failures.add(1, &[]);
            tracing::warn!(event = "replica_sync_failed", error = %e);
        }
        app.hub.kick(&id, inbox_core::proto::close::REVOKED);
        tracing::info!(event = "device_revoked", device.id = %id);
    }
    Ok(no_store(json!({ "revoked": revoked })))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_non_loopback_trusted_proxies_skip_the_host_check() {
        let trusted: Vec<ipnet::IpNet> =
            vec!["127.0.0.0/8".parse().unwrap(), "172.30.0.0/24".parse().unwrap()];
        assert!(exempt_from_host_check("172.30.0.10".parse().unwrap(), &trusted));
        assert!(!exempt_from_host_check("127.0.0.1".parse().unwrap(), &trusted), "loopback");
        assert!(!exempt_from_host_check("::ffff:127.0.0.1".parse().unwrap(), &trusted));
        assert!(!exempt_from_host_check("10.0.0.1".parse().unwrap(), &trusted), "not trusted");
    }

    #[test]
    fn assets_resolve_under_any_prefix() {
        assert_eq!(resolve_asset("/"), "index.html");
        assert_eq!(resolve_asset("/inbox"), "index.html");
        assert_eq!(resolve_asset("/a/b/app.js"), "app.js");
        assert_eq!(resolve_asset("/a/doc"), "doc.html");
        assert_eq!(resolve_asset("/a/doc/style.css"), "style.css");
    }
}
