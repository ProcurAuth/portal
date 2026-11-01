//! The replication between instances that does not use PostgreSQL.
//!
//! While the database has no writable primary (`spool.rs`), one instance accepts an alert. The
//! alert must not live on this instance only. Before the inbox answers the producer, it copies
//! the alert to the other instances. They keep it in their own spool and wake the phones that are
//! connected to them. The receipts, and the message "this phone opened a session here", use the
//! same path.
//!
//! The instances find each other with the names of `inbox.cluster.peers`. A headless Service
//! lists every pod. The inbox asks each address who it is. It skips an address that answers
//! with its own id. The requests are plain HTTP on the port `inbox.cluster.listen`. An HMAC-SHA256
//! over the time, the path and the body authenticates each request. The key comes from the
//! server key, which every instance shares. Only the pods of the service must reach the port.
//! A stranger who reaches it cannot forge a request.

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use hmac::{Hmac, Mac};
use hyper::body::Incoming;
use hyper::header::HeaderMap;
use hyper::{Method, Request, StatusCode};
use inbox_core::proto::close;
use inbox_core::server_key::ServerKey;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

use super::http::{empty, json, read_body, text, Resp};
use super::log;
use super::spool::{Accepted, WireAlert};
use super::state::App;
use super::util::{hex, is_hex_id, now_ms};
use crate::config::ClusterConfig;

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
        PeerAuth {
            key: h.finalize().into(),
        }
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
        let Some(sig) = unhex(signature) else {
            return false;
        };
        self.mac(ts, path, body).verify_slice(&sig).is_ok()
    }
}

fn unhex(text: &str) -> Option<Vec<u8>> {
    if !text.len().is_multiple_of(2) || !text.is_ascii() {
        return None;
    }
    (0..text.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&text[i..i + 2], 16).ok())
        .collect()
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
        self.inner
            .as_ref()
            .map_or(0, |i| i.live.lock().expect("peer lock").len())
    }

    /// Resolves the peer names and asks every address who it is.
    pub async fn discover(&self) {
        let Some(inner) = &self.inner else { return };
        let mut found = Vec::new();
        for name in &inner.names {
            let resolved =
                tokio::time::timeout(CALL_TIMEOUT, tokio::net::lookup_host(name.as_str()));
            let Ok(Ok(addresses)) = resolved.await else {
                continue;
            };
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
        let calls = peers
            .iter()
            .map(|addr| call(&inner.auth, *addr, "POST", PATH_SPOOL, &body));
        futures_util::future::join_all(calls)
            .await
            .into_iter()
            .filter(|r| matches!(r, Ok((200, _))))
            .count()
    }

    /// Tells the peers, without waiting, that a device acknowledged an alert of the spool.
    pub fn forward_ack(&self, device: &str, alert_id: &str, at: i64) {
        let msg = AckMsg {
            device: device.to_string(),
            alert_id: alert_id.to_string(),
            at,
        };
        self.fire(PATH_ACK, serde_json::to_vec(&msg).expect("serializes"));
    }

    /// Tells the peers, without waiting, that a device opened a session here: they close the older one they may hold.
    pub fn forward_replaced(&self, device: &str, connected_at: i64) {
        let msg = ReplacedMsg {
            device: device.to_string(),
            connected_at,
        };
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
    pub async fn run_discovery(&self, app: Arc<App>) {
        if !self.enabled() {
            return;
        }
        let mut known = 0;
        loop {
            self.discover().await;
            let now = self.live();
            if now != known {
                log::info!(event = "peers_changed", peers = now);
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
        stream
            .write_all(head.as_bytes())
            .await
            .map_err(|e| e.to_string())?;
        stream.write_all(body).await.map_err(|e| e.to_string())?;
        let mut raw = Vec::new();
        (&mut stream)
            .take(MAX_BODY as u64)
            .read_to_end(&mut raw)
            .await
            .map_err(|e| e.to_string())?;
        parse_response(&raw)
    };
    tokio::time::timeout(CALL_TIMEOUT, exchange)
        .await
        .map_err(|_| "timeout".to_string())?
}

fn parse_response(raw: &[u8]) -> Result<(u16, Vec<u8>), String> {
    let split = raw
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .ok_or("truncated answer")?;
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

/// Answers one request of another instance. Every route checks the signature of the request.
pub async fn route(app: Arc<App>, request: Request<Incoming>) -> Resp {
    let path = request.uri().path().to_string();
    let known = [PATH_HELLO, PATH_SPOOL, PATH_ACK, PATH_REPLACED].contains(&path.as_str());
    if !known {
        return empty(StatusCode::NOT_FOUND);
    }
    let expected = if path == PATH_HELLO {
        Method::GET
    } else {
        Method::POST
    };
    if *request.method() != expected {
        return empty(StatusCode::METHOD_NOT_ALLOWED);
    }
    let headers = request.headers().clone();
    let body = match read_body(request, MAX_BODY, app.http.body_read_timeout).await {
        Ok(body) => body,
        Err(error) => return empty(error.status()),
    };
    if let Err(status) = authorize(&app, &headers, &path, &body) {
        return empty(status);
    }
    match path.as_str() {
        PATH_HELLO => hello(&app),
        PATH_SPOOL => spool(&app, &body),
        PATH_ACK => ack(&app, &body),
        _ => replaced(&app, &body),
    }
}

fn authorize(app: &App, headers: &HeaderMap, path: &str, body: &[u8]) -> Result<(), StatusCode> {
    let Some(inner) = &app.cluster.inner else {
        return Err(StatusCode::UNAUTHORIZED);
    };
    let ts = headers
        .get("x-peer-time")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse().ok());
    let sig = headers.get("x-peer-auth").and_then(|v| v.to_str().ok());
    match (ts, sig) {
        (Some(ts), Some(sig)) if inner.auth.verify(ts, path, body, sig, now_ms()) => Ok(()),
        _ => {
            log::warn!(event = "peer_request_refused", path);
            Err(StatusCode::UNAUTHORIZED)
        }
    }
}

fn hello(app: &App) -> Resp {
    json(
        StatusCode::OK,
        &serde_json::json!({ "instance": app.cluster.instance() }),
    )
}

fn spool(app: &App, body: &[u8]) -> Resp {
    let Ok(alert) = serde_json::from_slice::<WireAlert>(body) else {
        return text(StatusCode::BAD_REQUEST, "malformed alert");
    };
    if !is_hex_id(&alert.id) || !app.group_names.contains_key(&alert.group) {
        return text(
            StatusCode::UNPROCESSABLE_ENTITY,
            "unknown group or malformed id",
        );
    }
    let Some(spool) = app.db.spool() else {
        return text(StatusCode::NOT_FOUND, "no spool");
    };
    let devices = match spool.merge(alert) {
        Accepted::Created(e) | Accepted::Duplicate(e) => e.waiting(),
        Accepted::Conflict => {
            return text(
                StatusCode::CONFLICT,
                "idempotency key used for another alert",
            );
        }
        Accepted::Full => {
            return text(StatusCode::SERVICE_UNAVAILABLE, "spool full");
        }
    };
    app.hub.wake(&devices);
    json(
        StatusCode::OK,
        &serde_json::json!({ "instance": app.cluster.instance(), "stored": true }),
    )
}

fn ack(app: &App, body: &[u8]) -> Resp {
    let Ok(msg) = serde_json::from_slice::<AckMsg>(body) else {
        return text(StatusCode::BAD_REQUEST, "malformed receipt");
    };
    if let Some(spool) = app.db.spool() {
        spool.ack_remote(&msg.device, &msg.alert_id, msg.at);
    }
    empty(StatusCode::OK)
}

fn replaced(app: &App, body: &[u8]) -> Resp {
    let Ok(msg) = serde_json::from_slice::<ReplacedMsg>(body) else {
        return text(StatusCode::BAD_REQUEST, "malformed message");
    };
    app.hub
        .kick_older(&msg.device, msg.connected_at, close::REPLACED);
    empty(StatusCode::OK)
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
