//! The state of the inbox. The program builds it one time at start.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize};
use std::time::Duration;

use inbox_core::proto::GroupInfo;
use inbox_core::server_key::ServerKey;
use sha2::{Digest, Sha256};
use zeroize::Zeroizing;

use super::hub::Hub;
use super::metrics::InboxMetrics;
use super::peer::Cluster;
use super::ratelimit::AuthLimiter;
use super::replica::{Replica, Stats as ReplicaStats};
use super::shutdown::Shutdown;
use super::store::Store;
use crate::config::{InboxConfig, InboxSecrets, MIN_TOKEN_LEN};

pub struct Producer {
    pub name: String,
    pub token_hash: [u8; 32],
    pub groups: Vec<String>,
    pub webhook_send_resolved: bool,
}

/// The secrets of the inbox, decoded. The configuration gives the bytes: from a variable or
/// from a file.
pub struct Secrets {
    pub server_key: ServerKey,
    pub producers: Vec<Producer>,
}

impl Secrets {
    /// Decodes and checks every secret of the block `inbox`. `check-config` and the start use it.
    pub fn load(secrets: &InboxSecrets, config: &InboxConfig) -> Result<Secrets, String> {
        let key_text = text(secrets.server_key.as_bytes(), "inbox.server_key")?;
        let server_key = ServerKey::decode(&key_text).map_err(|e| {
            format!("inbox.server_key: {e} (make a key with `portal inbox keygen`; a key file of protocol 3 is not read)")
        })?;
        let mut producers = Vec::new();
        let mut hashes = std::collections::HashSet::new();
        for (name, token) in &secrets.producers {
            let label = format!("inbox.producer.{name}.token");
            let token = text(token.as_bytes(), &label)?;
            if token.len() < MIN_TOKEN_LEN || !token.bytes().all(|b| b.is_ascii_graphic()) {
                return Err(format!(
                    "{label}: the token must have at least {MIN_TOKEN_LEN} printable characters (make one with `portal inbox token`)"
                ));
            }
            let token_hash: [u8; 32] = Sha256::digest(token.as_bytes()).into();
            if !hashes.insert(token_hash) {
                return Err(format!("{label}: another producer has the same token"));
            }
            let block = config
                .producers
                .get(name)
                .ok_or_else(|| format!("internal error: inbox.producer.{name} is missing"))?;
            producers.push(Producer {
                name: name.clone(),
                token_hash,
                groups: block.groups.clone(),
                webhook_send_resolved: block.webhook_send_resolved,
            });
        }
        Ok(Secrets {
            server_key,
            producers,
        })
    }
}

fn text(bytes: &[u8], label: &str) -> Result<Zeroizing<String>, String> {
    std::str::from_utf8(bytes)
        .map(|t| Zeroizing::new(t.to_string()))
        .map_err(|_| format!("{label}: the value is not valid UTF-8"))
}

/// The limits of the HTTP listeners of the inbox. They come from the block `server` of the portal.
#[derive(Clone, Copy)]
pub struct HttpLimits {
    pub header_read_timeout: Duration,
    pub body_read_timeout: Duration,
    pub max_headers: usize,
    pub max_header_bytes: usize,
    /// The largest number of connections that one listener holds.
    pub max_connections: usize,
}

pub struct App {
    pub config: InboxConfig,
    pub secrets: Secrets,
    pub db: Store,
    pub replica: Replica,
    /// The state of the replication. `None` without the block `inbox.replica`.
    pub replica_stats: Option<std::sync::Arc<ReplicaStats>>,
    /// The other instances, for what is accepted while the database is away (disabled without a `cluster` block).
    pub cluster: Cluster,
    pub hub: Hub,
    pub limiter: AuthLimiter,
    pub metrics: InboxMetrics,
    /// Triggered when the inbox stops: the background tasks end.
    pub shutdown: Shutdown,
    /// True from the signal to stop: `/readyz` says `draining`.
    pub draining: AtomicBool,
    pub groups: Vec<GroupInfo>,
    pub group_names: HashMap<String, String>,
    pub next_session_id: AtomicU64,
    pub http: HttpLimits,
    /// The connections of the listeners of the inbox, and the signal that closes them.
    pub stop: crate::shutdown::Stop,
    /// The WebSockets that are open, also those that did not finish their handshake.
    pub sockets: AtomicUsize,
    /// What the portal gives for the rights (see `rights.rs`). Empty until the portal sets it.
    pub authorizer: std::sync::OnceLock<std::sync::Arc<dyn super::rights::Authorizer>>,
}

impl App {
    /// Alerts created before this Unix time (ms) have expired for delivery.
    pub fn delivery_cutoff(&self) -> i64 {
        super::util::now_ms() - self.config.storage.delivery_ttl.as_millis() as i64
    }

    /// The authorizer of the portal. `None` before the portal gives it.
    pub fn authorizer(&self) -> Option<&std::sync::Arc<dyn super::rights::Authorizer>> {
        self.authorizer.get()
    }

    /// Whether the group is critical: a limit never applies to it.
    pub fn is_critical(&self, group: &str) -> bool {
        self.config.groups.get(group).is_some_and(|g| g.critical)
    }

    pub fn group_name(&self, id: &str) -> String {
        self.group_names
            .get(id)
            .cloned()
            .unwrap_or_else(|| id.to_string())
    }

    /// Sessions open on this instance and, with a shared database, on the others (a local one is the freshest).
    pub async fn live_sessions(&self) -> HashMap<String, super::hub::LiveInfo> {
        let mut live = self.db.remote_sessions().await.unwrap_or_default();
        live.extend(self.hub.snapshot());
        live
    }

    /// Finds the producer owning `token`, comparing hashes in constant time.
    pub fn producer_for_token(&self, token: &str) -> Option<&Producer> {
        let presented: [u8; 32] = Sha256::digest(token.as_bytes()).into();
        let mut found = None;
        for p in &self.secrets.producers {
            let mut diff = 0u8;
            for (a, b) in p.token_hash.iter().zip(presented.iter()) {
                diff |= a ^ b;
            }
            if diff == 0 {
                found = Some(p);
            }
        }
        found
    }
}

/// The guard of one open WebSocket. The count goes down when the guard is dropped.
pub struct SocketGuard(std::sync::Arc<App>);

impl SocketGuard {
    pub fn new(app: &std::sync::Arc<App>) -> SocketGuard {
        app.sockets
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        SocketGuard(std::sync::Arc::clone(app))
    }
}

impl Drop for SocketGuard {
    fn drop(&mut self) {
        self.0
            .sockets
            .fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;

    /// A configuration with two producers. `tokens` are their tokens.
    fn load(tokens: [&str; 2], key: Option<&str>) -> Result<Secrets, String> {
        let server_key = ServerKey::generate().unwrap();
        let key_text = key.map_or_else(|| server_key.encode().to_string(), str::to_string);
        let tokens = tokens.map(str::to_string);
        let raw = r#"
            server { listen_address = "0.0.0.0:4181" trusted_proxies = [] }
            authentication { superadmin_login_env = "L" superadmin_password_env = "P" }
            inbox {
              server_key_env = "K"
              devices {
                listen = "0.0.0.0:8080" public_address = "inbox.example.org:8080" trusted_proxies = []
                heartbeat_interval = "20s" session_timeout = "60s" resend_after = "15s"
                max_inflight = 32 unreachable_after = "2m" offline_warning_after = "3m"
              }
              storage { delivery_ttl = "7d" alert_retention = "30d" connection_log_retention = "90d" }
              group "infra" { name = "Infra" }
              producer "a" { token_env = "A" groups = ["infra"] }
              producer "b" { token_env = "B" groups = ["infra"] webhook_send_resolved = true }
            }"#;
        let (config, mut secrets) = Config::parse(raw, &|name| match name {
            "L" => Some("root".into()),
            "P" => Some("a password for the tests".into()),
            "K" => Some(key_text.clone()),
            "A" => Some(tokens[0].clone()),
            "B" => Some(tokens[1].clone()),
            _ => None,
        })?;
        Secrets::load(
            &secrets.inbox.take().unwrap(),
            config.inbox.as_ref().unwrap(),
        )
    }

    const TOKEN_A: &str = "0123456789abcdef0123456789abcdef";
    const TOKEN_B: &str = "fedcba9876543210fedcba9876543210";

    #[test]
    fn the_secrets_decode_and_the_producers_keep_only_the_hash_of_their_token() {
        let secrets = load([TOKEN_A, TOKEN_B], None).unwrap();
        assert_eq!(secrets.producers.len(), 2);
        let a = secrets.producers.iter().find(|p| p.name == "a").unwrap();
        let b = secrets.producers.iter().find(|p| p.name == "b").unwrap();
        assert!(!a.webhook_send_resolved && b.webhook_send_resolved);
        assert_eq!(a.groups, ["infra"]);
        let expected: [u8; 32] = Sha256::digest(TOKEN_A.as_bytes()).into();
        assert_eq!(a.token_hash, expected);
    }

    #[test]
    fn a_bad_secret_stops_the_start_and_the_error_names_the_key() {
        let error = |r: Result<Secrets, String>| r.err().expect("must be refused");
        assert!(error(load([TOKEN_A, TOKEN_A], None)).contains("inbox.producer.b.token"));
        assert!(error(load(["short", TOKEN_B], None)).contains("inbox.producer.a.token"));
        assert!(error(load(
            ["a token with spaces in it, long enough", TOKEN_B],
            None
        ))
        .contains("printable"));
        assert!(error(load([TOKEN_A, TOKEN_B], Some("not a key"))).contains("inbox.server_key"));
    }
}
