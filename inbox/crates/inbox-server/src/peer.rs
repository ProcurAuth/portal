//! Replication between Inbox instances that does not go through PostgreSQL.
//!
//! While the database has no writable primary (`spool.rs`) an alert is accepted by one instance and must not live only there:
//! before the producer is answered it is copied to the other instances, which hold it in their own spool and wake the phones
//! connected to them. Receipts and "this device opened a session here" travel the same way.
//!
//! The instances find each other by resolving the `cluster.peers` names (a headless Service lists every pod) and asking each
//! address who it is: an address that answers with this instance's own id is skipped. Requests are plain HTTP on the
//! `cluster.listen` port, authenticated by an HMAC-SHA256 over the time, the path and the body, keyed from the server key
//! (shared by every instance): the port must stay reachable from the pods of the service only, but a stranger who reaches
//! it still cannot forge a request.

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::Router;
use axum::body::Bytes;
use axum::extract::{DefaultBodyLimit, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use hmac::{Hmac, Mac};
use inbox_core::proto::close;
use inbox_core::server_key::ServerKey;
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

use crate::config::Cluster as ClusterConfig;
use crate::spool::{Accepted, WireAlert};
use crate::state::AppState;
use crate::util::{hex, is_hex_id, now_ms};

/// A request older or newer than this (clock skew included) is refused.
const MAX_SKEW_MS: i64 = 60_000;
/// How long a call to a peer may take: a peer that does not answer must not hold an alert back.
const CALL_TIMEOUT: Duration = Duration::from_secs(1);
const DISCOVERY_EVERY: Duration = Duration::from_secs(2);
const MAX_ADDRESSES_PER_NAME: usize = 16;
const MAX_BODY: usize = 64 * 1024;

pub const PATH_HELLO: &str = "/internal/v1/hello";
pub const PATH_SPOOL: &str = "/internal/v1/spool";
pub const PATH_ACK: &str = "/internal/v1/ack";
pub const PATH_REPLACED: &str = "/internal/v1/replaced";

/// The key of the peer channel: a hash of the server key, domain separated, so that the key of the sessions is never used
/// as is.
pub struct PeerAuth {
    key: [u8; 32],
}

impl PeerAuth {
    pub fn new(server_key: &ServerKey) -> PeerAuth {
        let mut h = Sha256::new();
        h.update(b"inbox-peer-v1\0");
        h.update(&*server_key.noise_private);
        PeerAuth { key: h.finalize().into() }
    }

    fn mac(&self, ts: i64, path: &str, body: &[u8]) -> Hmac<Sha256> {
        let mut m = <Hmac<Sha256> as Mac>::new_from_slice(&self.key).expect("any key length");
        m.update(ts.to_string().as_bytes());
        m.update(b"\n");
        m.update(path.as_bytes());
        m.update(b"\n");
        m.update(body);
        m
    }

    pub fn sign(&self, ts: i64, path: &str, body: &[u8]) -> String {
        hex(&self.mac(ts, path, body).finalize().into_bytes())
    }

    /// Checks the signature (in constant time) and the age of the request.
    pub fn verify(&self, ts: i64, path: &str, body: &[u8], signature: &str, now: i64) -> bool {
        if (now - ts).abs() > MAX_SKEW_MS {
            return false;
        }
        let Some(sig) = unhex(signature) else { return false };
        self.mac(ts, path, body).verify_slice(&sig).is_ok()
    }
}

fn unhex(text: &str) -> Option<Vec<u8>> {
    if !text.len().is_multiple_of(2) || !text.is_ascii() {
        return None;
    }
    (0..text.len()).step_by(2).map(|i| u8::from_str_radix(&text[i..i + 2], 16).ok()).collect()
}

#[derive(Debug, Serialize, Deserialize)]
pub struct AckMsg {
    pub device: String,
    pub alert_id: String,
    pub at: i64,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ReplacedMsg {
    pub device: String,
    pub connected_at: i64,
}

struct Inner {
    instance: String,
    auth: PeerAuth,
    /// `host:port` names of `cluster.peers`.
    names: Vec<String>,
    /// Peers that answered the last discovery.
    live: Mutex<Vec<SocketAddr>>,
    /// Addresses that turned out to be this instance.
    own: Mutex<Vec<SocketAddr>>,
}

/// The other instances, as seen from this one. Without a `cluster` block there are none.
#[derive(Clone)]
pub struct Cluster {
    inner: Option<Arc<Inner>>,
}

impl Cluster {
    pub fn disabled() -> Cluster {
        Cluster { inner: None }
    }

    pub fn new(config: &ClusterConfig, instance: String, server_key: &ServerKey) -> Cluster {
        Cluster {
            inner: Some(Arc::new(Inner {
                instance,
                auth: PeerAuth::new(server_key),
                names: config.peers.clone(),
                live: Mutex::new(Vec::new()),
                own: Mutex::new(Vec::new()),
            })),
        }
    }

    pub fn enabled(&self) -> bool {
        self.inner.is_some()
    }

    pub fn instance(&self) -> &str {
        self.inner.as_ref().map_or("", |i| i.instance.as_str())
    }

    /// Peers that answered the last discovery.
    pub fn live(&self) -> usize {
        self.inner.as_ref().map_or(0, |i| i.live.lock().expect("peer lock").len())
    }

    /// Resolves the peer names and asks every address who it is.
    pub async fn discover(&self) {
        let Some(inner) = &self.inner else { return };
        let mut found = Vec::new();
        for name in &inner.names {
            let resolved =
                tokio::time::timeout(CALL_TIMEOUT, tokio::net::lookup_host(name.as_str()));
            let Ok(Ok(addresses)) = resolved.await else { continue };
            for addr in addresses.take(MAX_ADDRESSES_PER_NAME) {
                if inner.own.lock().expect("peer lock").contains(&addr) || found.contains(&addr) {
                    continue;
                }
                let Ok((200, body)) = call(&inner.auth, addr, "GET", PATH_HELLO, &[]).await else {
                    continue;
                };
                let who = serde_json::from_slice::<serde_json::Value>(&body)
                    .ok()
                    .and_then(|v| v["instance"].as_str().map(str::to_string));
                match who {
                    Some(id) if id == inner.instance => {
                        inner.own.lock().expect("peer lock").push(addr)
                    }
                    Some(_) => found.push(addr),
                    None => {}
                }
            }
        }
        *inner.live.lock().expect("peer lock") = found;
    }

    /// Copies an alert to every live peer and returns how many stored it. Peers are asked at the same time.
    pub async fn replicate(&self, alert: &WireAlert) -> usize {
        let Some(inner) = &self.inner else { return 0 };
        let peers = inner.live.lock().expect("peer lock").clone();
        let body = serde_json::to_vec(alert).expect("serializes");
        let calls = peers.iter().map(|addr| call(&inner.auth, *addr, "POST", PATH_SPOOL, &body));
        futures_util::future::join_all(calls)
            .await
            .into_iter()
            .filter(|r| matches!(r, Ok((200, _))))
            .count()
    }

    /// Tells the peers, without waiting, that a device acknowledged an alert of the spool.
    pub fn forward_ack(&self, device: &str, alert_id: &str, at: i64) {
        let msg = AckMsg { device: device.to_string(), alert_id: alert_id.to_string(), at };
        self.fire(PATH_ACK, serde_json::to_vec(&msg).expect("serializes"));
    }

    /// Tells the peers, without waiting, that a device opened a session here: they close the older one they may hold.
    pub fn forward_replaced(&self, device: &str, connected_at: i64) {
        let msg = ReplacedMsg { device: device.to_string(), connected_at };
        self.fire(PATH_REPLACED, serde_json::to_vec(&msg).expect("serializes"));
    }

    fn fire(&self, path: &'static str, body: Vec<u8>) {
        let Some(inner) = &self.inner else { return };
        for addr in inner.live.lock().expect("peer lock").clone() {
            let (inner, body) = (inner.clone(), body.clone());
            tokio::spawn(async move {
                let _ = call(&inner.auth, addr, "POST", path, &body).await;
            });
        }
    }

    /// Looks for the peers every few seconds until the server stops.
    pub async fn run_discovery(&self, app: Arc<AppState>) {
        if !self.enabled() {
            return;
        }
        let mut known = 0;
        loop {
            self.discover().await;
            let now = self.live();
            if now != known {
                tracing::info!(event = "peers_changed", peers = now);
                known = now;
            }
            tokio::select! {
                _ = tokio::time::sleep(DISCOVERY_EVERY) => {}
                _ = app.shutdown.wait() => return,
            }
        }
    }
}

/// One signed request, one answer: (status, body).
async fn call(
    auth: &PeerAuth,
    addr: SocketAddr,
    method: &str,
    path: &str,
    body: &[u8],
) -> Result<(u16, Vec<u8>), String> {
    let exchange = async {
        let mut stream = TcpStream::connect(addr).await.map_err(|e| e.to_string())?;
        let ts = now_ms();
        let head = format!(
            "{method} {path} HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\nContent-Type: application/json\r\n\
             Content-Length: {}\r\nX-Peer-Time: {ts}\r\nX-Peer-Auth: {}\r\n\r\n",
            body.len(),
            auth.sign(ts, path, body)
        );
        stream.write_all(head.as_bytes()).await.map_err(|e| e.to_string())?;
        stream.write_all(body).await.map_err(|e| e.to_string())?;
        let mut raw = Vec::new();
        (&mut stream)
            .take(MAX_BODY as u64)
            .read_to_end(&mut raw)
            .await
            .map_err(|e| e.to_string())?;
        parse_response(&raw)
    };
    tokio::time::timeout(CALL_TIMEOUT, exchange).await.map_err(|_| "timeout".to_string())?
}

fn parse_response(raw: &[u8]) -> Result<(u16, Vec<u8>), String> {
    let split = raw.windows(4).position(|w| w == b"\r\n\r\n").ok_or("truncated answer")?;
    let head = std::str::from_utf8(&raw[..split]).map_err(|_| "answer is not text")?;
    let status = head
        .lines()
        .next()
        .and_then(|l| l.split_whitespace().nth(1))
        .and_then(|s| s.parse().ok())
        .ok_or("no status")?;
    Ok((status, raw[split + 4..].to_vec()))
}

// ---------------------------------------------------------------- the listener

pub fn router(app: Arc<AppState>) -> Router {
    Router::new()
        .route(PATH_HELLO, get(hello))
        .route(PATH_SPOOL, post(spool))
        .route(PATH_ACK, post(ack))
        .route(PATH_REPLACED, post(replaced))
        .layer(DefaultBodyLimit::max(MAX_BODY))
        .with_state(app)
}

fn authorize(
    app: &AppState,
    headers: &HeaderMap,
    path: &str,
    body: &[u8],
) -> Result<(), StatusCode> {
    let Some(inner) = &app.cluster.inner else { return Err(StatusCode::UNAUTHORIZED) };
    let ts = headers.get("x-peer-time").and_then(|v| v.to_str().ok()).and_then(|v| v.parse().ok());
    let sig = headers.get("x-peer-auth").and_then(|v| v.to_str().ok());
    match (ts, sig) {
        (Some(ts), Some(sig)) if inner.auth.verify(ts, path, body, sig, now_ms()) => Ok(()),
        _ => {
            tracing::warn!(event = "peer_request_refused", path);
            Err(StatusCode::UNAUTHORIZED)
        }
    }
}

async fn hello(State(app): State<Arc<AppState>>, headers: HeaderMap) -> Response {
    if let Err(status) = authorize(&app, &headers, PATH_HELLO, &[]) {
        return status.into_response();
    }
    axum::Json(json!({ "instance": app.cluster.instance() })).into_response()
}

async fn spool(State(app): State<Arc<AppState>>, headers: HeaderMap, body: Bytes) -> Response {
    if let Err(status) = authorize(&app, &headers, PATH_SPOOL, &body) {
        return status.into_response();
    }
    let Ok(alert) = serde_json::from_slice::<WireAlert>(&body) else {
        return (StatusCode::BAD_REQUEST, "malformed alert").into_response();
    };
    if !is_hex_id(&alert.id) || !app.group_names.contains_key(&alert.group) {
        return (StatusCode::UNPROCESSABLE_ENTITY, "unknown group or malformed id").into_response();
    }
    let Some(spool) = app.db.spool() else {
        return (StatusCode::NOT_FOUND, "no spool").into_response();
    };
    let devices = match spool.merge(alert) {
        Accepted::Created(e) | Accepted::Duplicate(e) => e.waiting(),
        Accepted::Conflict => {
            return (StatusCode::CONFLICT, "idempotency key used for another alert")
                .into_response();
        }
        Accepted::Full => {
            return (StatusCode::SERVICE_UNAVAILABLE, "spool full").into_response();
        }
    };
    app.hub.wake(&devices);
    axum::Json(json!({ "instance": app.cluster.instance(), "stored": true })).into_response()
}

async fn ack(State(app): State<Arc<AppState>>, headers: HeaderMap, body: Bytes) -> Response {
    if let Err(status) = authorize(&app, &headers, PATH_ACK, &body) {
        return status.into_response();
    }
    let Ok(msg) = serde_json::from_slice::<AckMsg>(&body) else {
        return (StatusCode::BAD_REQUEST, "malformed receipt").into_response();
    };
    if let Some(spool) = app.db.spool() {
        spool.ack_remote(&msg.device, &msg.alert_id, msg.at);
    }
    StatusCode::OK.into_response()
}

async fn replaced(State(app): State<Arc<AppState>>, headers: HeaderMap, body: Bytes) -> Response {
    if let Err(status) = authorize(&app, &headers, PATH_REPLACED, &body) {
        return status.into_response();
    }
    let Ok(msg) = serde_json::from_slice::<ReplacedMsg>(&body) else {
        return (StatusCode::BAD_REQUEST, "malformed message").into_response();
    };
    app.hub.kick_older(&msg.device, msg.connected_at, close::REPLACED);
    StatusCode::OK.into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use inbox_core::rng::Rng;

    fn auth() -> PeerAuth {
        PeerAuth::new(&ServerKey::generate(&mut Rng::os()).unwrap())
    }

    #[test]
    fn a_signed_request_is_accepted_once_signed_for_these_bytes() {
        let a = auth();
        let sig = a.sign(1_000, PATH_SPOOL, b"{\"x\":1}");
        assert!(a.verify(1_000, PATH_SPOOL, b"{\"x\":1}", &sig, 1_000));
        // Any change of the body, the path or the time breaks it.
        assert!(!a.verify(1_000, PATH_SPOOL, b"{\"x\":2}", &sig, 1_000));
        assert!(!a.verify(1_000, PATH_ACK, b"{\"x\":1}", &sig, 1_000));
        assert!(!a.verify(1_001, PATH_SPOOL, b"{\"x\":1}", &sig, 1_001));
        // Garbage is refused, not a panic.
        assert!(!a.verify(1_000, PATH_SPOOL, b"", "zz", 1_000));
        assert!(!a.verify(1_000, PATH_SPOOL, b"", "", 1_000));
        assert!(!a.verify(1_000, PATH_SPOOL, b"", "é", 1_000));
    }

    #[test]
    fn another_server_key_cannot_sign() {
        let (a, b) = (auth(), auth());
        let sig = b.sign(5, PATH_HELLO, b"");
        assert!(!a.verify(5, PATH_HELLO, b"", &sig, 5));
    }

    #[test]
    fn an_old_or_future_request_is_refused() {
        let a = auth();
        let sig = a.sign(1_000_000, PATH_ACK, b"");
        assert!(a.verify(1_000_000, PATH_ACK, b"", &sig, 1_000_000 + MAX_SKEW_MS));
        assert!(!a.verify(1_000_000, PATH_ACK, b"", &sig, 1_000_000 + MAX_SKEW_MS + 1));
        assert!(!a.verify(1_000_000, PATH_ACK, b"", &sig, 1_000_000 - MAX_SKEW_MS - 1));
    }

    #[test]
    fn answers_are_parsed() {
        let (status, body) =
            parse_response(b"HTTP/1.1 200 OK\r\ncontent-length: 2\r\n\r\n{}").unwrap();
        assert_eq!((status, body.as_slice()), (200, &b"{}"[..]));
        assert!(parse_response(b"HTTP/1.1 200 OK\r\n").is_err());
        assert!(parse_response(b"nonsense\r\n\r\n").is_err());
    }
}
