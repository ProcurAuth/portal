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
use crate::config::{InboxConfig, InboxSecrets};

/// A key of the file (a block `api_key`). It follows its owner, an account of the file. The rights
/// of a file account do not change while the server runs, so the start decided the groups. The
/// check of the key reads no directory and no database: a key of the file works when the
/// directory is not ready.
pub struct FileKey {
    pub name: String,
    pub digest: [u8; 32],
    pub groups: Vec<String>,
    /// The key sends alerts to accounts (`direct_alerts = true`). The start checked that its
    /// owner holds `alert.send_user`.
    pub direct_alerts: bool,
    pub webhook_send_resolved: bool,
}

/// The secrets of the inbox, decoded. The configuration gives the bytes: from a variable or
/// from a file.
pub struct Secrets {
    pub server_key: ServerKey,
    pub file_keys: Vec<FileKey>,
}

impl Secrets {
    /// Decodes and checks every secret of the block `inbox`. `check-config` and the start use it.
    pub fn load(secrets: &InboxSecrets, config: &InboxConfig) -> Result<Secrets, String> {
        let key_text = text(secrets.server_key.as_bytes(), "inbox.server_key")?;
        let server_key = ServerKey::decode(&key_text).map_err(|e| {
            format!("inbox.server_key: {e} (make a key with `portal inbox keygen`; a key file of protocol 3 is not read)")
        })?;
        let mut file_keys = Vec::new();
        let mut digests = std::collections::HashSet::new();
        for (name, key_text) in &secrets.api_keys {
            let label = format!("inbox.api_key.{name}.secret");
            let key_text = text(key_text.as_bytes(), &label)?;
            // The configuration checked the form. The start checks it again: a key that the
            // verifier refuses must never reach the table.
            match crate::keys::check_format(&key_text) {
                Ok(crate::keys::KeyKind::Api) => {}
                _ => {
                    return Err(format!(
                        "{label}: expected an API key with the prefix api_ (make one with `portal inbox token`)"
                    ))
                }
            }
            let digest = crate::keys::digest(&key_text);
            if !digests.insert(digest) {
                return Err(format!("{label}: another key has the same text"));
            }
            let block = config
                .api_keys
                .get(name)
                .ok_or_else(|| format!("internal error: inbox.api_key.{name} is missing"))?;
            file_keys.push(FileKey {
                name: name.clone(),
                digest,
                groups: block.groups.clone(),
                direct_alerts: block.direct_alerts,
                webhook_send_resolved: block.webhook_send_resolved,
            });
        }
        Ok(Secrets {
            server_key,
            file_keys,
        })
    }
}

fn text(bytes: &[u8], label: &str) -> Result<Zeroizing<String>, String> {
    std::str::from_utf8(bytes)
        .map(|t| Zeroizing::new(t.to_string()))
        .map_err(|_| format!("{label}: the value is not valid UTF-8"))
}

/// The limits of the HTTP listeners of the inbox. They come from the block `server` of the portal.
#[derive(Clone)]
pub struct HttpLimits {
    pub header_read_timeout: Duration,
    pub body_read_timeout: Duration,
    pub max_headers: usize,
    pub max_header_bytes: usize,
    /// The largest number of connections that the listener of the cluster holds.
    pub max_connections: usize,
    /// The largest number of open phone sessions: the budget that is separate from the pages. It is
    /// `server.max_connections` plus two times `inbox.devices.max_devices`.
    pub max_sessions: usize,
}

pub struct App {
    pub config: InboxConfig,
    /// The name of the instance (`server.name`). The phones show it as the name of the server.
    pub server_name: String,
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
    /// The budget of phone sessions. A phone takes one permit before the upgrade and keeps it until
    /// the WebSocket ends. The pages and the API use the budget `server.max_connections`.
    pub session_budget: SessionBudget,
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

    /// Finds the key of the file that has this text, comparing digests in constant time.
    pub fn file_key_for_token(&self, token: &str) -> Option<&FileKey> {
        let presented: [u8; 32] = Sha256::digest(token.as_bytes()).into();
        let mut found = None;
        for p in &self.secrets.file_keys {
            let mut diff = 0u8;
            for (a, b) in p.digest.iter().zip(presented.iter()) {
                diff |= a ^ b;
            }
            if diff == 0 {
                found = Some(p);
            }
        }
        found
    }
}

/// The budget of phone sessions, separate from the budget of the connections of the pages.
pub struct SessionBudget(std::sync::Arc<tokio::sync::Semaphore>);

impl SessionBudget {
    pub fn new(size: usize) -> SessionBudget {
        SessionBudget(std::sync::Arc::new(tokio::sync::Semaphore::new(size)))
    }

    /// Takes one permit, or `None` when the budget is full. The permit goes back when it drops.
    pub fn take(&self) -> Option<tokio::sync::OwnedSemaphorePermit> {
        std::sync::Arc::clone(&self.0).try_acquire_owned().ok()
    }

    /// The permits that nobody holds.
    #[cfg(test)]
    pub fn available(&self) -> usize {
        self.0.available_permits()
    }
}

/// The guard of one open WebSocket. The count goes down and the permit of the session budget goes
/// back when the guard is dropped.
pub struct SocketGuard {
    app: std::sync::Arc<App>,
    _permit: tokio::sync::OwnedSemaphorePermit,
}

impl SocketGuard {
    /// Counts one more WebSocket and takes a permit of the session budget. The result is `None`
    /// when the budget is full.
    pub fn new(app: &std::sync::Arc<App>) -> Option<SocketGuard> {
        let permit = app.session_budget.take()?;
        app.sockets
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Some(SocketGuard {
            app: std::sync::Arc::clone(app),
            _permit: permit,
        })
    }
}

impl Drop for SocketGuard {
    fn drop(&mut self) {
        self.app
            .sockets
            .fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;

    /// The text of two API keys that the tests use.
    fn key_text(index: usize) -> String {
        static CELL: std::sync::OnceLock<Vec<String>> = std::sync::OnceLock::new();
        CELL.get_or_init(|| {
            (0..3)
                .map(|_| {
                    crate::keys::generate(crate::keys::KeyKind::Api)
                        .unwrap()
                        .to_string()
                })
                .collect()
        })[index]
            .clone()
    }

    /// A configuration with two API keys. `tokens` are their texts.
    fn load(tokens: [&str; 2], key: Option<&str>) -> Result<Secrets, String> {
        let server_key = ServerKey::generate().unwrap();
        let key_text = key.map_or_else(|| server_key.encode().to_string(), str::to_string);
        let tokens = tokens.map(str::to_string);
        let raw = r#"
            server { listen_address = "0.0.0.0:4181" trusted_proxies = [] }
            authentication { superadmin_login_env = "L" superadmin_password_env = "P" mfa = "off" }
            access { grants_mode = "static" }
            users { allow_creation = false }
            keys { mcp = false api = true }
            user "ci" {
              grants = [
                "keys.own.use on service:portal/key_kind:api",
                "alert.send on service:portal/inbox_group:infra",
              ]
            }
            inbox {
              server_key_env = "K"
              devices {
                public_address = "inbox.example.org:8080"
                heartbeat_interval = "20s" session_timeout = "60s" resend_after = "15s"
                max_inflight = 32 unreachable_after = "2m" offline_warning_after = "3m"
              }
              storage { delivery_ttl = "7d" alert_retention = "30d" connection_log_retention = "90d" }
              group "infra" { name = "Infra" }
              api_key "a" { owner = "ci" secret_env = "A" groups = ["infra"] }
              api_key "b" { owner = "ci" secret_env = "B" groups = ["infra"] webhook_send_resolved = true }
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

    #[test]
    fn the_secrets_decode_and_the_file_keys_keep_only_the_digest_of_their_text() {
        let (a_text, b_text) = (key_text(0), key_text(1));
        let secrets = load([&a_text, &b_text], None).unwrap();
        assert_eq!(secrets.file_keys.len(), 2);
        let a = secrets.file_keys.iter().find(|p| p.name == "a").unwrap();
        let b = secrets.file_keys.iter().find(|p| p.name == "b").unwrap();
        assert!(!a.webhook_send_resolved && b.webhook_send_resolved);
        assert_eq!(a.groups, ["infra"]);
        let expected: [u8; 32] = Sha256::digest(a_text.as_bytes()).into();
        assert_eq!(a.digest, expected);
    }

    #[test]
    fn a_bad_secret_stops_the_start_and_the_error_names_the_key() {
        let (a_text, b_text) = (key_text(0), key_text(1));
        let error = |r: Result<Secrets, String>| r.err().expect("must be refused");
        assert!(error(load([&a_text, &a_text], None)).contains("inbox.api_key.b.secret"));
        assert!(error(load(["short", &b_text], None)).contains("inbox.api_key.a.secret"));
        let mcp = crate::keys::generate(crate::keys::KeyKind::Mcp).unwrap();
        let refused = error(load([&mcp, &b_text], None));
        assert!(refused.contains("inbox.api_key.a.secret") && !refused.contains(&*mcp));
        assert!(error(load([&a_text, &b_text], Some("not a key"))).contains("inbox.server_key"));
    }

    #[test]
    fn the_session_budget_gives_its_permits_one_by_one_and_takes_them_back() {
        let budget = SessionBudget::new(2);
        let first = budget.take().expect("first permit");
        let second = budget.take().expect("second permit");
        assert!(budget.take().is_none(), "the budget is full");
        assert_eq!(budget.available(), 0);
        drop(first);
        assert_eq!(budget.available(), 1);
        let third = budget.take().expect("a freed permit is given again");
        assert!(budget.take().is_none());
        drop((second, third));
        assert_eq!(budget.available(), 2);
    }
}
