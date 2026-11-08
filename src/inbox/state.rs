//! The state of the inbox. The program builds it one time at start.

use std::collections::{HashMap, VecDeque};
use std::net::IpAddr;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use inbox_core::proto::GroupInfo;
use inbox_core::server_key::ServerKey;
use sha2::{Digest, Sha256};
use tokio::sync::Notify;
use zeroize::Zeroizing;

use super::content_box::{self, ContentBox};
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
    /// The groups that the key sends alerts to: the owner holds `alert.send` on each.
    pub groups: Vec<String>,
    /// The groups of the key where the owner holds `alert.read`.
    pub read_groups: Vec<String>,
    /// The services whose alerts the key reads: the services that the owner may open.
    pub services: super::rights::Services,
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
    /// The keys that seal the contents of the items. `None` without the block `inbox.at_rest`.
    pub content: Option<ContentBox>,
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
                read_groups: block.read_groups.clone(),
                services: block.services.clone(),
                direct_alerts: block.direct_alerts,
                webhook_send_resolved: block.webhook_send_resolved,
            });
        }
        let content = match &config.at_rest {
            Some(at_rest) => Some(load_content_keys(
                &secrets.content_keys,
                &at_rest.active_key,
            )?),
            None => None,
        };
        Ok(Secrets {
            server_key,
            file_keys,
            content,
        })
    }
}

/// Decodes the keys of the blocks `inbox.at_rest.key`. The error names the key and what is wrong, never the
/// value. Two keys must not have the same bytes: a copy under a new id would hide a key that did not change.
fn load_content_keys(
    texts: &[(String, crate::config::Secret)],
    active: &str,
) -> Result<ContentBox, String> {
    let mut keys: Vec<(String, Zeroizing<[u8; content_box::KEY_BYTES]>)> = Vec::new();
    for (id, text) in texts {
        let label = format!("inbox.at_rest.key.{id}.secret");
        let bytes = content_box::parse_secret(text.as_bytes())
            .map_err(|e| format!("{label}: {e} (the value is hidden)"))?;
        if let Some((other, _)) = keys.iter().find(|(_, b)| **b == *bytes) {
            return Err(format!(
                "{label}: the key has the same bytes as inbox.at_rest.key.{other}.secret: two ids must hold two keys (the value is hidden)"
            ));
        }
        keys.push((id.clone(), bytes));
    }
    ContentBox::new(&keys, active)
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
    /// The handshakes of each client address (`inbox.devices.max_handshakes_per_address`).
    pub handshakes: Handshakes,
    /// The clock of every time rule of the items.
    pub clock: std::sync::Arc<super::items::SystemClock>,
    /// Wakes the agents that wait for an item when an item changes.
    pub item_changed: tokio::sync::Notify,
    /// What the portal gives for the rights (see `rights.rs`). Empty until the portal sets it.
    pub authorizer: std::sync::OnceLock<std::sync::Arc<dyn super::rights::Authorizer>>,
}

impl App {
    /// Alerts created before this Unix time (ms) have expired for delivery.
    pub fn delivery_cutoff(&self) -> i64 {
        super::util::now_ms() - self.config.storage.delivery_ttl.as_millis() as i64
    }

    /// The engine of the items. `None` without the blocks `inbox.items` and `inbox.at_rest`.
    pub fn items(&self) -> Option<super::items_store::Items<'_>> {
        let (config, content) = (self.config.items.as_ref()?, self.secrets.content.as_ref()?);
        Some(super::items_store::Items {
            store: &self.db,
            content,
            rules: super::items::Rules::from_config(config),
            clock: &*self.clock,
            hub: Some(&self.hub),
            replica: Some(&self.replica),
            metrics: Some(&self.metrics),
            changed: Some(&self.item_changed),
            admin_body_read: self
                .config
                .at_rest
                .as_ref()
                .is_some_and(|a| a.admin_body_read),
        })
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

/// The phone WebSockets of each client address that did not finish their handshake. One address
/// holds at most `inbox.devices.max_handshakes_per_address` of them. A new handshake of an address
/// at its maximum ends the oldest handshake of this address. Thus one client cannot keep this share
/// of the session budget against the other phones of its address: it must open new connections
/// faster than a phone completes its handshake.
pub struct Handshakes {
    max: usize,
    next_id: AtomicU64,
    open: std::sync::Mutex<HashMap<IpAddr, Waiting>>,
}

/// The handshakes of one address, oldest first: the id and the signal of each one.
type Waiting = VecDeque<(u64, Arc<Notify>)>;

/// One handshake that [`Handshakes`] counts.
struct HandshakeSlot {
    ip: IpAddr,
    id: u64,
    /// Notified when a newer handshake of the same address ended this one.
    ended: Arc<Notify>,
}

impl Handshakes {
    pub fn new(max: u32) -> Handshakes {
        Handshakes {
            max: max as usize,
            next_id: AtomicU64::new(1),
            open: std::sync::Mutex::new(HashMap::new()),
        }
    }

    /// Counts one more handshake of `ip`. When `ip` holds its maximum, the oldest handshake of `ip`
    /// leaves the count and its owner is notified. The second value is `true` in this case.
    fn take(&self, ip: IpAddr) -> (HandshakeSlot, bool) {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let ended = Arc::new(Notify::new());
        let mut open = self.open.lock().unwrap_or_else(|p| p.into_inner());
        let queue = open.entry(ip).or_default();
        let mut evicted = false;
        while queue.len() >= self.max {
            let Some((_, oldest)) = queue.pop_front() else {
                break;
            };
            oldest.notify_one();
            evicted = true;
        }
        queue.push_back((id, Arc::clone(&ended)));
        (HandshakeSlot { ip, id, ended }, evicted)
    }

    /// Removes a handshake from the count. A handshake that a newer one ended is not counted any more.
    fn give_back(&self, slot: &HandshakeSlot) {
        let mut open = self.open.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(queue) = open.get_mut(&slot.ip) {
            queue.retain(|(id, _)| *id != slot.id);
            if queue.is_empty() {
                open.remove(&slot.ip);
            }
        }
    }

    /// The handshakes of `ip` that the count holds.
    #[cfg(test)]
    fn count(&self, ip: IpAddr) -> usize {
        let open = self.open.lock().unwrap_or_else(|p| p.into_inner());
        open.get(&ip).map_or(0, VecDeque::len)
    }
}

/// The guard of one open WebSocket. The count goes down and the permit of the session budget goes
/// back when the guard is dropped. Until [`SocketGuard::handshake_done`], the WebSocket also counts
/// as a handshake of its client address.
pub struct SocketGuard {
    app: Arc<App>,
    _permit: tokio::sync::OwnedSemaphorePermit,
    handshake: Option<HandshakeSlot>,
    ended: Arc<Notify>,
}

impl SocketGuard {
    /// Counts one more WebSocket and takes a permit of the session budget. Then it counts one more
    /// handshake of `ip`. The result is `None` when the budget is full.
    pub fn new(app: &Arc<App>, ip: IpAddr) -> Option<SocketGuard> {
        let permit = app.session_budget.take()?;
        let (slot, evicted) = app.handshakes.take(ip);
        if evicted {
            super::log::info!(event = "handshake_ended_by_newer", client.address = %ip);
        }
        app.sockets.fetch_add(1, Ordering::SeqCst);
        Some(SocketGuard {
            app: Arc::clone(app),
            _permit: permit,
            ended: Arc::clone(&slot.ended),
            handshake: Some(slot),
        })
    }

    /// The phone proved its key: the WebSocket does not count as a handshake of its address any more.
    pub fn handshake_done(&mut self) {
        if let Some(slot) = self.handshake.take() {
            self.app.handshakes.give_back(&slot);
        }
    }

    /// Completes when a newer handshake of the same address ended this handshake.
    pub async fn ended_by_newer(&self) {
        self.ended.notified().await;
    }
}

impl Drop for SocketGuard {
    fn drop(&mut self) {
        self.handshake_done();
        self.app.sockets.fetch_sub(1, Ordering::SeqCst);
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

    /// One client address holds at most its share of handshakes. A newer handshake of a full address
    /// ends its oldest one, so one client cannot keep the share against the other phones of the
    /// address. The other addresses are not affected.
    #[tokio::test]
    async fn a_new_handshake_of_a_full_address_ends_its_oldest_handshake() {
        let handshakes = Handshakes::new(2);
        let shared: IpAddr = "198.51.100.7".parse().unwrap();
        let other: IpAddr = "203.0.113.9".parse().unwrap();
        let (first, evicted) = handshakes.take(shared);
        assert!(!evicted);
        let (second, evicted) = handshakes.take(shared);
        assert!(!evicted);
        let (elsewhere, evicted) = handshakes.take(other);
        assert!(!evicted, "another address has its own share");
        assert_eq!(handshakes.count(shared), 2);

        let (third, evicted) = handshakes.take(shared);
        assert!(
            evicted,
            "the third handshake of the address ends the oldest one"
        );
        assert_eq!(handshakes.count(shared), 2);
        let ended = |slot: &HandshakeSlot| {
            let notified = slot.ended.notified();
            futures_util::FutureExt::now_or_never(notified).is_some()
        };
        assert!(ended(&first), "the oldest handshake is told to end");
        assert!(!ended(&second) && !ended(&third) && !ended(&elsewhere));

        // A handshake that leaves gives its place back, also when a newer one ended it before.
        handshakes.give_back(&first);
        assert_eq!(handshakes.count(shared), 2);
        handshakes.give_back(&second);
        assert_eq!(handshakes.count(shared), 1);
        let (_fourth, evicted) = handshakes.take(shared);
        assert!(!evicted);
        handshakes.give_back(&elsewhere);
        assert_eq!(handshakes.count(other), 0);
        assert!(!handshakes.open.lock().unwrap().contains_key(&other));
    }

    /// The inbox with `items` and `at_rest`. `keys` holds the id and the text of each content key.
    fn load_content(keys: &[(&str, &str)], active: &str) -> Result<Secrets, String> {
        let blocks: String = keys
            .iter()
            .enumerate()
            .map(|(i, (id, _))| format!("key \"{id}\" {{ secret_env = \"C{i}\" }}\n"))
            .collect();
        let raw = format!(
            r#"
            server {{ listen_address = "0.0.0.0:4181" trusted_proxies = [] }}
            authentication {{ superadmin_login_env = "L" superadmin_password_env = "P" mfa = "off" }}
            inbox {{
              server_key_env = "K"
              devices {{
                public_address = "inbox.example.org:8080"
                heartbeat_interval = "20s" session_timeout = "60s" resend_after = "15s"
                max_inflight = 32 unreachable_after = "2m" offline_warning_after = "3m"
              }}
              storage {{ delivery_ttl = "7d" alert_retention = "30d" connection_log_retention = "90d" }}
              group "infra" {{ name = "Infra" }}
              items {{
                max_lifetime = "90d" max_open_per_key = 5 max_open_per_owner = 20 max_open_total = 1000
                retention = "180d" body_purge_after = "30d" audit_retention = "365d"
                lease {{ min = "30s" max = "7d" grace = "2m" revive_window = "7d" }}
                digest {{ mode = "off" }}
              }}
              at_rest {{ active_key = "{active}" {blocks} }}
            }}"#
        );
        let server_key = ServerKey::generate().unwrap().encode().to_string();
        let (config, mut secrets) = Config::parse(&raw, &|name| match name {
            "L" => Some("root".into()),
            "P" => Some("a password for the tests".into()),
            "K" => Some(server_key.clone()),
            other => other
                .strip_prefix('C')
                .and_then(|i| i.parse::<usize>().ok())
                .map(|i| keys[i].1.to_string()),
        })?;
        Secrets::load(
            &secrets.inbox.take().unwrap(),
            config.inbox.as_ref().unwrap(),
        )
    }

    fn content_text(seed: u8) -> String {
        let bytes: Vec<u8> = (0..32u8)
            .map(|i| seed.wrapping_add(i.wrapping_mul(5)))
            .collect();
        inbox_core::b64::encode(&bytes)
    }

    #[test]
    fn the_content_keys_decode_into_a_box_with_the_active_key() {
        let (a, b) = (content_text(1), content_text(2));
        let secrets = load_content(&[("old", &a), ("new", &b)], "new").unwrap();
        let content = secrets.content.as_ref().unwrap();
        assert_eq!(content.active_kid(), "new");
        assert_eq!(content.kids().collect::<Vec<_>>(), ["old", "new"]);
        let aad = content_box::aad("r", content_box::FIELD_BODY, 0, None);
        assert!(content
            .open(&content.seal(b"x", &aad).unwrap(), &aad)
            .is_some());
    }

    #[test]
    fn secret_not_32_bytes_or_all_equal_stops_start() {
        let good = content_text(1);
        let equal = inbox_core::b64::encode(&[9u8; 32]);
        let short = inbox_core::b64::encode(&[1u8; 16]);
        let long = inbox_core::b64::encode(&[1u8; 33]);
        let bang = "!".repeat(44);
        let hex = "00112233445566778899aabbccddeeff00112233445566778899aabbccddeeff";
        for (text, expected) in [
            (equal.as_str(), "all bytes of the value are equal"),
            (short.as_str(), "the value has 24 characters"),
            (long.as_str(), "not the canonical base64 of 32 bytes"),
            (hex, "the value has 64 characters"),
            (bang.as_str(), "not base64"),
            ("", ""),
        ] {
            // The configuration refuses the empty value before the start does.
            let result = load_content(&[("k1", text), ("k2", &good)], "k2");
            let error = result.err().expect("must be refused");
            assert!(error.contains(expected), "{text}: {error}");
            if !text.is_empty() {
                assert!(
                    error.contains("inbox.at_rest.key.k1.secret")
                        && error.contains("expected the base64 text of 32 random bytes"),
                    "{error}"
                );
                assert!(
                    !error.contains(text),
                    "the error never shows the value: {error}"
                );
            }
        }
    }

    #[test]
    fn two_ids_with_one_key_stop_the_start() {
        let a = content_text(1);
        let error = load_content(&[("k1", &a), ("k2", &a)], "k1").err().unwrap();
        assert!(
            error.contains("inbox.at_rest.key.k2.secret")
                && error.contains("inbox.at_rest.key.k1.secret")
                && !error.contains(&a),
            "{error}"
        );
    }
}
