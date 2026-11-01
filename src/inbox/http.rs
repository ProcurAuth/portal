//! The HTTP routes of the inbox.
//!
//! The listener of the phones (`inbox.devices.listen`) is a listener of the inbox. It has no
//! session. It serves `/v1/enroll`, `/v1/session` (WebSocket), `/healthz` and `/readyz`. A phone
//! on a mobile network reaches this port, and the pages of the portal stay private.
//!
//! The producers and the webhooks use the listener of the portal, on the port of the pages. The
//! function [`serve_api`] answers the paths that [`owns_api`] names: `/api/v1/*`, `/api/version`
//! and `/readyz`. They need no session. Producers prove themselves with a token or a key.
//!
//! The listener of the cluster (`inbox.cluster.listen`) is a third listener, for the copies of
//! alerts between the instances. It is not public. All of them use hyper.

use std::convert::Infallible;
use std::net::{IpAddr, SocketAddr};
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;

use http_body_util::{BodyExt, Full, Limited};
use hyper::body::{Bytes, Incoming};
use hyper::header::{HeaderValue, CACHE_CONTROL, CONTENT_LENGTH, CONTENT_TYPE};
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{Method, Request, Response, StatusCode};
use hyper_util::rt::{TokioIo, TokioTimer};
use serde_json::{json, Value};
use tokio::net::TcpListener;
use tokio::sync::Semaphore;

use super::api::{self, ApiError};
use super::log;
use super::state::App;
use super::{device, peer};

pub type Resp = Response<Full<Bytes>>;

/// What a listener serves.
#[derive(Clone, Copy)]
pub enum Kind {
    /// Phones and the health routes.
    Devices,
    /// The requests of the other instances.
    Cluster,
}

pub fn text(status: StatusCode, body: &'static str) -> Resp {
    let mut response = Response::new(Full::new(Bytes::from_static(body.as_bytes())));
    *response.status_mut() = status;
    response.headers_mut().insert(
        CONTENT_TYPE,
        HeaderValue::from_static("text/plain; charset=utf-8"),
    );
    response
}

pub fn empty(status: StatusCode) -> Resp {
    let mut response = Response::new(Full::new(Bytes::new()));
    *response.status_mut() = status;
    response
}

/// A JSON answer that no cache keeps.
pub fn json(status: StatusCode, value: &Value) -> Resp {
    let body = serde_json::to_vec(value).unwrap_or_default();
    let mut response = Response::new(Full::new(Bytes::from(body)));
    *response.status_mut() = status;
    let headers = response.headers_mut();
    headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    headers.insert(CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

/// Reads the body of a request. The read stops above `limit` bytes and after the body timeout.
pub async fn read_body(
    request: Request<Incoming>,
    limit: usize,
    timeout: Duration,
) -> Result<Bytes, ApiError> {
    let too_large = || {
        ApiError::new(
            StatusCode::PAYLOAD_TOO_LARGE,
            "too_large",
            format!("the body is larger than {limit} bytes"),
        )
    };
    let declared = request
        .headers()
        .get(CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<usize>().ok());
    if declared.is_some_and(|n| n > limit) {
        return Err(too_large());
    }
    match tokio::time::timeout(timeout, Limited::new(request.into_body(), limit).collect()).await {
        Ok(Ok(collected)) => Ok(collected.to_bytes()),
        Ok(Err(error))
            if error
                .downcast_ref::<http_body_util::LengthLimitError>()
                .is_some() =>
        {
            Err(too_large())
        }
        Ok(Err(_)) => Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "invalid_body",
            "the body could not be read",
        )),
        Err(_) => Err(ApiError::new(
            StatusCode::REQUEST_TIMEOUT,
            "body_timeout",
            "the body did not arrive in time",
        )),
    }
}

/// Accepts the connections of one listener until the task is aborted.
pub async fn accept_loop(app: Arc<App>, listener: TcpListener, kind: Kind) {
    let gate = Arc::new(Semaphore::new(app.http.max_connections));
    loop {
        let (stream, peer_addr) = match listener.accept().await {
            Ok(v) => v,
            Err(error) => {
                log::error!(event = "accept_error", error = %error);
                tokio::time::sleep(Duration::from_millis(100)).await;
                continue;
            }
        };
        let Ok(permit) = Arc::clone(&gate).try_acquire_owned() else {
            drop(stream);
            continue;
        };
        let app = Arc::clone(&app);
        tokio::spawn(async move {
            let _permit = permit;
            let _live = app.stop.enter();
            let _ = stream.set_nodelay(true);
            let io = TokioIo::new(stream);
            let service_app = Arc::clone(&app);
            let service = service_fn(move |request| {
                let app = Arc::clone(&service_app);
                async move {
                    Ok::<_, Infallible>(match kind {
                        Kind::Devices => route_devices(app, peer_addr, request).await,
                        Kind::Cluster => peer::route(app, request).await,
                    })
                }
            });
            let mut builder = http1::Builder::new();
            builder
                .timer(TokioTimer::new())
                .header_read_timeout(app.http.header_read_timeout)
                .max_headers(app.http.max_headers)
                .max_buf_size(app.http.max_header_bytes)
                .half_close(false)
                .keep_alive(true);
            if let Err(error) = app
                .stop
                .drive_upgradable(builder.serve_connection(io, service).with_upgrades())
                .await
            {
                log::debug!(
                    event = "connection_closed_with_error",
                    remote_addr = %peer_addr,
                    error = %error
                );
            }
        });
    }
}

/// The path after `prefix`, when it is one segment that is not empty.
fn segment<'a>(path: &'a str, prefix: &str) -> Option<&'a str> {
    path.strip_prefix(prefix)
        .filter(|rest| !rest.is_empty() && !rest.contains('/'))
}

/// The client address of a phone: the portal rule (`server.trusted_proxies`).
fn phone_ip(app: &App, peer: SocketAddr, request: &Request<Incoming>) -> IpAddr {
    crate::transport::client_ip(request.headers(), peer.ip(), &app.http.trusted_proxies)
}

/// The routes of the listener of the phones.
async fn route_devices(app: Arc<App>, peer: SocketAddr, request: Request<Incoming>) -> Resp {
    let get = request.method() == Method::GET;
    let path = request.uri().path().to_string();
    match path.as_str() {
        "/v1/enroll" | "/v1/session" if !get => empty(StatusCode::METHOD_NOT_ALLOWED),
        "/v1/enroll" => {
            let ip = phone_ip(&app, peer, &request);
            device::enroll_ws(app, ip, request)
        }
        "/v1/session" => {
            let ip = phone_ip(&app, peer, &request);
            device::session_ws(app, ip, request)
        }
        "/healthz" if get => text(StatusCode::OK, "ok"),
        "/readyz" if get => readyz(&app).await,
        "/healthz" | "/readyz" => empty(StatusCode::METHOD_NOT_ALLOWED),
        _ => empty(StatusCode::NOT_FOUND),
    }
}

/// Tells whether the inbox owns the path on the listener of the portal: the API of the
/// producers, the version and the readiness. The portal gives these paths to the inbox and to no
/// other handler. The paths of the phones (`/v1/`) are not here: they are on the listener of the
/// phones.
pub fn owns_api(path: &str) -> bool {
    path.starts_with("/api/v1/") || matches!(path, "/readyz" | "/api/version")
}

/// Answers one request that [`owns_api`] names, on the listener of the portal.
pub async fn serve_api(app: Arc<App>, request: Request<Incoming>) -> Resp {
    let method = request.method().clone();
    let path = request.uri().path().to_string();
    let get = method == Method::GET;
    let post = method == Method::POST;
    if path == "/api/v1/alerts" {
        return if post {
            api::post_alert(&app, request).await
        } else {
            empty(StatusCode::METHOD_NOT_ALLOWED)
        };
    }
    if let Some(id) = segment(&path, "/api/v1/alerts/") {
        return if get {
            api::get_alert(&app, request, id).await
        } else {
            empty(StatusCode::METHOD_NOT_ALLOWED)
        };
    }
    if let Some(group) = segment(&path, "/api/v1/webhooks/alertmanager/") {
        return if post {
            api::alertmanager(&app, request, group).await
        } else {
            empty(StatusCode::METHOD_NOT_ALLOWED)
        };
    }
    if let Some(group) = segment(&path, "/api/v1/notify/") {
        return if post {
            api::notify(&app, request, group).await
        } else {
            empty(StatusCode::METHOD_NOT_ALLOWED)
        };
    }
    match (get, path.as_str()) {
        (true, "/readyz") => readyz(&app).await,
        (true, "/api/version") => version(),
        (false, "/readyz" | "/api/version") => empty(StatusCode::METHOD_NOT_ALLOWED),
        _ => empty(StatusCode::NOT_FOUND),
    }
}

/// With PostgreSQL the instance is ready whenever it runs: without a primary it still accepts alerts and serves its phones
/// (see `spool.rs`), and taking it out of the service would only remove that. SQLite needs its file.
async fn readyz(app: &App) -> Resp {
    if app.draining.load(Ordering::SeqCst) || app.shutdown.is_triggered() {
        return text(StatusCode::SERVICE_UNAVAILABLE, "draining");
    }
    if app.db.is_shared() {
        return text(
            StatusCode::OK,
            if app.db.is_degraded() {
                "degraded"
            } else {
                "ready"
            },
        );
    }
    match tokio::time::timeout(Duration::from_secs(2), app.db.ping()).await {
        Ok(Ok(())) => text(StatusCode::OK, "ready"),
        _ => text(StatusCode::SERVICE_UNAVAILABLE, "storage unavailable"),
    }
}

/// The version of the program, the commit and the protocol of the phones.
pub fn version() -> Resp {
    json(
        StatusCode::OK,
        &json!({
            "version": super::VERSION,
            "git_sha": option_env!("PORTAL_GIT_SHA").unwrap_or("unknown"),
            "protocol": inbox_core::PROTOCOL_VERSION,
        }),
    )
}
