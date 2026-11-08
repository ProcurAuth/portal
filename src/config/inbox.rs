//! The block `inbox`: the typed values of the inbox.
//!
//! The table in `keys.rs` holds the type, the range and the default of each key. This module reads
//! the values once, checks the keys that contradict each other, and builds the structure that the
//! module `crate::inbox` reads. The delivery values have no default.

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::Duration;

use super::keys::show_duration;
use super::resolve::{Resolved, Secret};
use super::{Config, StorageBackend};

/// The values of the block `inbox`.
#[derive(Clone, Debug)]
pub struct InboxConfig {
    pub devices: DevicesConfig,
    pub enrollment: EnrollmentConfig,
    pub storage: InboxStorageConfig,
    /// `None` without the block `inbox.replica`.
    pub replica: Option<ReplicaConfig>,
    /// `None` without the block `inbox.cluster`.
    pub cluster: Option<ClusterConfig>,
    /// `None` without the block `inbox.watch`.
    pub watch: Option<WatchConfig>,
    /// `None` without the block `inbox.items`. The block comes with `inbox.at_rest`, never alone.
    #[allow(dead_code)] // the item engine reads the limits
    pub items: Option<ItemsConfig>,
    /// `None` without the block `inbox.at_rest`.
    pub at_rest: Option<AtRestConfig>,
    /// The blocks `group`, by id.
    pub groups: BTreeMap<String, GroupConfig>,
    /// The blocks `api_key`, by name.
    pub api_keys: BTreeMap<String, ApiKeyConfig>,
}

/// The block `inbox.devices`.
#[derive(Clone, Debug)]
pub struct DevicesConfig {
    /// The public base URL of the portal as the phones reach it. It is the `s` of an enrollment code.
    pub public_address: String,
    pub max_devices: u32,
    /// The most phones of one account that are not revoked. At most `max_devices`.
    pub max_per_account: u32,
    pub heartbeat_interval: Duration,
    pub session_timeout: Duration,
    pub resend_after: Duration,
    pub max_inflight: u32,
    pub unreachable_after: Duration,
    pub offline_warning_after: Duration,
    pub max_auth_failures: u32,
    pub auth_failure_window: Duration,
    /// The most phone WebSockets of one client address that did not finish the handshake.
    pub max_handshakes_per_address: u32,
    pub max_request_bytes: usize,
}

/// The block `inbox.enrollment`.
#[derive(Clone, Debug)]
pub struct EnrollmentConfig {
    /// How long an enrollment code works.
    pub code_ttl: Duration,
    /// The most codes of one account that are not used and not expired.
    pub max_active_per_account: u32,
}

/// The block `inbox.storage`.
#[derive(Clone, Debug)]
pub struct InboxStorageConfig {
    pub delivery_ttl: Duration,
    pub alert_retention: Duration,
    pub connection_log_retention: Duration,
    /// The connections of the pool. The value applies only with the block `postgres`.
    pub pool_size: usize,
}

/// The block `inbox.replica`.
#[derive(Clone, Debug)]
pub struct ReplicaConfig {
    pub dir: PathBuf,
    pub snapshot_interval: Duration,
    pub lease_ttl: Duration,
    pub sync_timeout: Duration,
    pub sync_alerts: bool,
}

/// The block `inbox.cluster`.
#[derive(Clone, Debug)]
pub struct ClusterConfig {
    pub listen: SocketAddr,
    pub peers: Vec<String>,
}

/// The block `inbox.watch`: the alerts that the portal sends about itself.
#[derive(Clone, Debug)]
pub struct WatchConfig {
    /// The id of the group that receives the alerts. It is a block `group`.
    pub group: String,
    pub service_unhealthy: bool,
    pub unhealthy_after: Duration,
    pub sign_in_burst: bool,
    pub sign_in_failures: u32,
    pub sign_in_window: Duration,
    pub repeat_after: Duration,
    pub recovered: bool,
}

/// The block `inbox.items`: the limits of the approvals and of the questions. Each limit is the
/// value of the file or the documented default.
#[derive(Clone, Debug)]
#[allow(dead_code)] // the item engine reads the values
pub struct ItemsConfig {
    pub max_lifetime: Duration,
    pub max_open_per_key: u32,
    pub max_open_per_owner: u32,
    pub max_open_total: u32,
    pub lease: LeaseConfig,
    pub max_chain: u32,
    pub max_turns: u32,
    pub stale_after: Duration,
    pub retention: Duration,
    pub body_purge_after: Duration,
    pub audit_retention: Duration,
    pub digest: DigestConfig,
    pub max_body_bytes: usize,
    pub wait_max: Duration,
}

/// The block `inbox.items.lease`.
#[derive(Clone, Debug)]
#[allow(dead_code)] // the item engine reads the values
pub struct LeaseConfig {
    pub min: Duration,
    pub max: Duration,
    pub grace: Duration,
    pub revive_window: Duration,
    pub max_revivals: u32,
}

/// The block `inbox.items.digest`: the default summary of the open items.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DigestConfig {
    Off,
    /// One summary each day at `hour`:`minute`, in the time zone `timezone` (an IANA name that
    /// the start checks for its form only).
    Daily {
        hour: u8,
        minute: u8,
        timezone: String,
    },
    /// One summary at each interval.
    Every(Duration),
}

/// The block `inbox.at_rest`: the keys that seal the contents. The secrets are in
/// [`InboxSecrets::content_keys`].
#[derive(Clone, Debug)]
#[allow(dead_code)] // the item engine reads the values
pub struct AtRestConfig {
    /// The id of the key that seals the new contents.
    pub active_key: String,
    /// The ids of the blocks `key`, in the order of the file.
    pub keys: Vec<String>,
    /// The superadmin can read the contents.
    pub admin_body_read: bool,
}

/// A block `group`.
#[derive(Clone, Debug)]
pub struct GroupConfig {
    pub name: String,
    pub description: String,
    /// A critical group never has a limit on `alert.send`.
    pub critical: bool,
}

/// A block `api_key`, without its text.
#[derive(Clone, Debug)]
pub struct ApiKeyConfig {
    /// The login of the owner: an account of the file.
    pub owner: String,
    /// The groups that the block lists. The server keeps only those that the owner may send to.
    pub groups: Vec<String>,
    /// The groups of the block where the owner holds `alert.read`: the key reads the alerts of
    /// these groups only. [`check_api_keys`] fills the list from the grants of the owner.
    pub read_groups: Vec<String>,
    /// The services whose alerts the key reads: the services that the owner may open. An alert
    /// about another service stays hidden from the key. [`check_api_keys`] fills it from the
    /// grants of the owner.
    pub services: crate::inbox::rights::Services,
    /// The key can send an alert to an account. The owner holds `alert.send_user`.
    pub direct_alerts: bool,
    pub webhook_send_resolved: bool,
}

/// The secrets of the inbox. The program erases them from memory when it drops them.
pub struct InboxSecrets {
    /// The text of the key of the server.
    pub server_key: Secret,
    /// The text of each block `api_key`, by name.
    pub api_keys: Vec<(String, Secret)>,
    /// The text of each block `inbox.at_rest.key`, by id, in the order of the file.
    pub content_keys: Vec<(String, Secret)>,
}

fn socket(r: &Resolved, block: &str, name: &str) -> Result<SocketAddr, String> {
    r.text(block, name)?
        .parse::<SocketAddr>()
        .map_err(|e| format!("internal error: {block}.{name}: {e}"))
}

fn small(r: &Resolved, block: &str, name: &str) -> Result<u32, String> {
    u32::try_from(r.int(block, name)?)
        .map_err(|_| format!("internal error: {block}.{name} is out of range"))
}

/// Reads the block `inbox`. The result is `None` without the block. `api_on` is the value of
/// `keys.api`: a block `api_key` needs it.
pub fn build(
    r: &mut Resolved,
    api_on: bool,
) -> Result<Option<(InboxConfig, InboxSecrets)>, String> {
    if !r.has_block("inbox") {
        return Ok(None);
    }
    let devices = DevicesConfig {
        public_address: r.text("inbox.devices", "public_address")?,
        max_devices: small(r, "inbox.devices", "max_devices")?,
        max_per_account: small(r, "inbox.devices", "max_per_account")?,
        heartbeat_interval: r.duration("inbox.devices", "heartbeat_interval")?,
        session_timeout: r.duration("inbox.devices", "session_timeout")?,
        resend_after: r.duration("inbox.devices", "resend_after")?,
        max_inflight: small(r, "inbox.devices", "max_inflight")?,
        unreachable_after: r.duration("inbox.devices", "unreachable_after")?,
        offline_warning_after: r.duration("inbox.devices", "offline_warning_after")?,
        max_auth_failures: small(r, "inbox.devices", "max_auth_failures")?,
        auth_failure_window: r.duration("inbox.devices", "auth_failure_window")?,
        max_handshakes_per_address: small(r, "inbox.devices", "max_handshakes_per_address")?,
        max_request_bytes: r.size("inbox.devices", "max_request_bytes")?,
    };
    let enrollment = EnrollmentConfig {
        code_ttl: r.duration("inbox.enrollment", "code_ttl")?,
        max_active_per_account: small(r, "inbox.enrollment", "max_active_per_account")?,
    };
    let storage = InboxStorageConfig {
        delivery_ttl: r.duration("inbox.storage", "delivery_ttl")?,
        alert_retention: r.duration("inbox.storage", "alert_retention")?,
        connection_log_retention: r.duration("inbox.storage", "connection_log_retention")?,
        pool_size: r.count("inbox.storage", "pool_size")?,
    };
    let replica = if r.has_block("inbox.replica") {
        Some(ReplicaConfig {
            dir: PathBuf::from(r.text("inbox.replica", "dir")?),
            snapshot_interval: r.duration("inbox.replica", "snapshot_interval")?,
            lease_ttl: r.duration("inbox.replica", "lease_ttl")?,
            sync_timeout: r.duration("inbox.replica", "sync_timeout")?,
            sync_alerts: r.bool("inbox.replica", "sync_alerts")?,
        })
    } else {
        None
    };
    let cluster = if r.has_block("inbox.cluster") {
        let peers = r.list("inbox.cluster", "peers")?;
        if peers.is_empty() {
            return Err(
                "invalid inbox.cluster.peers: expected a list of 1 to 16 names with a port, got []"
                    .into(),
            );
        }
        Some(ClusterConfig {
            listen: socket(r, "inbox.cluster", "listen")?,
            peers,
        })
    } else {
        None
    };

    let watch = if r.has_block("inbox.watch") {
        Some(WatchConfig {
            group: r.text("inbox.watch", "group")?,
            service_unhealthy: r.bool("inbox.watch", "service_unhealthy")?,
            unhealthy_after: r.duration("inbox.watch", "unhealthy_after")?,
            sign_in_burst: r.bool("inbox.watch", "sign_in_burst")?,
            sign_in_failures: small(r, "inbox.watch", "sign_in_failures")?,
            sign_in_window: r.duration("inbox.watch", "sign_in_window")?,
            repeat_after: r.duration("inbox.watch", "repeat_after")?,
            recovered: r.bool("inbox.watch", "recovered")?,
        })
    } else {
        None
    };

    let items = if r.has_block("inbox.items") {
        Some(build_items(r)?)
    } else {
        None
    };
    let (at_rest, content_keys) = build_at_rest(r)?;
    if items.is_some() != at_rest.is_some() {
        let (present, absent) = if items.is_some() {
            ("inbox.items", "inbox.at_rest")
        } else {
            ("inbox.at_rest", "inbox.items")
        };
        return Err(format!(
            "the block {present} needs the block {absent}: the items are sealed at rest, and the keys seal only items; add the block {absent}, or remove the block {present}"
        ));
    }

    let mut groups = BTreeMap::new();
    for block in r.labeled.iter().filter(|b| b.kind == "group") {
        super::keys::check_inbox_id(&block.label)
            .map_err(|e| format!("inbox.group {:?}: the label is not an id: {e}", block.label))?;
        groups.insert(
            block.label.clone(),
            GroupConfig {
                name: block.text("name")?,
                description: block.text("description")?,
                critical: block.bool("critical")?,
            },
        );
    }
    if groups.is_empty() {
        return Err(
            "the block inbox needs at least one block group: add group \"<id>\" { name = \"...\" }"
                .into(),
        );
    }
    let mut api_keys = BTreeMap::new();
    let mut texts: Vec<(String, Secret)> = Vec::new();
    let sources: Vec<(String, String)> = r
        .effective
        .iter()
        .map(|line| (line.key.clone(), line.source.clone()))
        .collect();
    for block in r.labeled.iter_mut().filter(|b| b.kind == "api_key") {
        let name = block.label.clone();
        if !api_on {
            return Err(format!(
                "inbox.api_key {name:?}: a block api_key needs keys.api = true: keys.api is false, or the block keys is absent"
            ));
        }
        super::keys::check_inbox_id(&name)
            .map_err(|e| format!("inbox.api_key {name:?}: the label is not a name: {e}"))?;
        let listed = block.list("groups")?;
        let direct_alerts = block.bool("direct_alerts")?;
        if listed.is_empty() && !direct_alerts {
            return Err(format!(
                "invalid inbox.api_key.{name}.groups: expected at least one group id, or direct_alerts = true, got []"
            ));
        }
        for group in &listed {
            if !groups.contains_key(group) {
                return Err(format!(
                    "invalid inbox.api_key.{name}.groups: the group {group:?} is not a block group of the block inbox"
                ));
            }
        }
        let secret = block
            .take_secret("secret")
            .ok_or_else(|| format!("internal error: inbox.api_key.{name} has no secret"))?;
        // The text has the form of an API key. The error never shows the value.
        let source = sources
            .iter()
            .find(|(key, _)| *key == format!("inbox.api_key.{name}.secret"))
            .map(|(_, source)| source.clone())
            .unwrap_or_default();
        let text = std::str::from_utf8(secret.as_bytes()).unwrap_or_default();
        match crate::keys::check_format(text) {
            Ok(crate::keys::KeyKind::Api) => {}
            Ok(kind) => {
                return Err(format!(
                    "inbox.api_key.{name}.secret: expected an API key with the prefix api_, 43 characters and 6 characters of checksum, 53 characters in all; the text is a {} key ({source}; the value is hidden)",
                    kind.label()
                ))
            }
            Err(reason) => {
                return Err(format!(
                    "inbox.api_key.{name}.secret: expected an API key with the prefix api_, 43 characters and 6 characters of checksum, 53 characters in all ({reason:?}; {source}; the value is hidden)"
                ))
            }
        }
        if let Some((other, _)) = texts
            .iter()
            .find(|(_, s)| s.as_bytes() == secret.as_bytes())
        {
            return Err(format!(
                "inbox.api_key.{name}.secret: the key has the same text as inbox.api_key.{other}.secret ({source}; the value is hidden)"
            ));
        }
        api_keys.insert(
            name.clone(),
            ApiKeyConfig {
                owner: block.text("owner")?,
                groups: listed,
                read_groups: Vec::new(),
                services: crate::inbox::rights::Services::Only(Default::default()),
                direct_alerts,
                webhook_send_resolved: block.bool("webhook_send_resolved")?,
            },
        );
        texts.push((name, secret));
    }

    let secrets = InboxSecrets {
        server_key: r
            .take_secret("inbox", "server_key")
            .ok_or("internal error: the secret inbox.server_key is missing")?,
        api_keys: texts,
        content_keys,
    };
    let config = InboxConfig {
        devices,
        enrollment,
        storage,
        replica,
        cluster,
        watch,
        items,
        at_rest,
        groups,
        api_keys,
    };
    check_values(&config)?;
    Ok(Some((config, secrets)))
}

/// Reads the block `inbox.items` and checks the limits that contradict each other.
fn build_items(r: &Resolved) -> Result<ItemsConfig, String> {
    const B: &str = "inbox.items";
    const L: &str = "inbox.items.lease";
    const D: &str = "inbox.items.digest";
    let lease = LeaseConfig {
        min: r.duration(L, "min")?,
        max: r.duration(L, "max")?,
        grace: r.duration(L, "grace")?,
        revive_window: r.duration(L, "revive_window")?,
        max_revivals: small(r, L, "max_revivals")?,
    };
    let digest = match r.text(D, "mode")?.as_str() {
        "off" => DigestConfig::Off,
        "daily" => {
            let at = r.text(D, "at")?;
            // The table checked the form `HH:MM`.
            let number = |range: std::ops::Range<usize>| {
                at[range]
                    .parse::<u8>()
                    .map_err(|_| format!("internal error: {D}.at is not a time"))
            };
            DigestConfig::Daily {
                hour: number(0..2)?,
                minute: number(3..5)?,
                timezone: r.text(D, "timezone")?,
            }
        }
        "every" => DigestConfig::Every(r.duration(D, "every")?),
        other => return Err(format!("internal error: {D}.mode is {other:?}")),
    };
    let items = ItemsConfig {
        max_lifetime: r.duration(B, "max_lifetime")?,
        max_open_per_key: small(r, B, "max_open_per_key")?,
        max_open_per_owner: small(r, B, "max_open_per_owner")?,
        max_open_total: small(r, B, "max_open_total")?,
        lease,
        max_chain: small(r, B, "max_chain")?,
        max_turns: small(r, B, "max_turns")?,
        stale_after: r.duration(B, "stale_after")?,
        retention: r.duration(B, "retention")?,
        body_purge_after: r.duration(B, "body_purge_after")?,
        audit_retention: r.duration(B, "audit_retention")?,
        digest,
        max_body_bytes: r.size(B, "max_body_bytes")?,
        wait_max: r.duration(B, "wait_max")?,
    };
    check_items(&items)?;
    Ok(items)
}

/// The limits of the items that contradict each other. Each error names the two keys and the two
/// values.
fn check_items(i: &ItemsConfig) -> Result<(), String> {
    let l = &i.lease;
    let shown = show_duration;
    if l.min > l.max {
        return Err(format!(
            "inbox.items.lease.min must be at most inbox.items.lease.max: min is {}, max is {}",
            shown(l.min),
            shown(l.max)
        ));
    }
    if l.max > i.max_lifetime {
        return Err(format!(
            "inbox.items.lease.max must be at most inbox.items.max_lifetime: lease.max is {}, max_lifetime is {}",
            shown(l.max),
            shown(i.max_lifetime)
        ));
    }
    if l.revive_window > i.max_lifetime {
        return Err(format!(
            "inbox.items.lease.revive_window must be at most inbox.items.max_lifetime: revive_window is {}, max_lifetime is {}",
            shown(l.revive_window),
            shown(i.max_lifetime)
        ));
    }
    if i.retention < i.max_lifetime + l.revive_window {
        return Err(format!(
            "inbox.items.retention must be at least inbox.items.max_lifetime plus inbox.items.lease.revive_window: retention is {}, max_lifetime is {}, revive_window is {}",
            shown(i.retention),
            shown(i.max_lifetime),
            shown(l.revive_window)
        ));
    }
    if i.audit_retention < i.retention {
        return Err(format!(
            "inbox.items.audit_retention must be at least inbox.items.retention: audit_retention is {}, retention is {}",
            shown(i.audit_retention),
            shown(i.retention)
        ));
    }
    if i.body_purge_after > i.retention {
        return Err(format!(
            "inbox.items.body_purge_after must be at most inbox.items.retention: body_purge_after is {}, retention is {}",
            shown(i.body_purge_after),
            shown(i.retention)
        ));
    }
    Ok(())
}

/// Reads the block `inbox.at_rest` and its blocks `key`. The result has no block without the
/// block `inbox.at_rest`. The text of each key stays a secret: the checks of its form are in
/// `crate::inbox`, where the program decodes it.
#[allow(clippy::type_complexity)]
fn build_at_rest(
    r: &mut Resolved,
) -> Result<(Option<AtRestConfig>, Vec<(String, Secret)>), String> {
    const B: &str = "inbox.at_rest";
    const K: &str = "inbox.at_rest.key";
    if !r.has_block(B) {
        if let Some(block) = r.labeled.iter().find(|b| b.kind == K) {
            return Err(format!(
                "{K} {:?}: a block key needs the block inbox.at_rest",
                block.label
            ));
        }
        return Ok((None, Vec::new()));
    }
    let active_key = r.text(B, "active_key")?;
    let mut ids = Vec::new();
    let mut secrets: Vec<(String, Secret)> = Vec::new();
    for block in r.labeled.iter_mut().filter(|b| b.kind == K) {
        let id = block.label.clone();
        super::keys::check_content_key_id(&id)
            .map_err(|e| format!("{K} {id:?}: the label is not a key id: {e}"))?;
        let secret = block
            .take_secret("secret")
            .ok_or_else(|| format!("internal error: {K}.{id} has no secret"))?;
        ids.push(id.clone());
        secrets.push((id, secret));
    }
    if ids.is_empty() || ids.len() > 8 {
        return Err(format!(
            "the block {B} needs 1 to 8 blocks key \"<id>\" {{ secret_file = \"...\" }}, found {}",
            ids.len()
        ));
    }
    if !ids.contains(&active_key) {
        return Err(format!(
            "invalid {B}.active_key: expected the label of a block key ({}), got {active_key:?}",
            ids.iter()
                .map(|id| format!("{id:?}"))
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    // The effective configuration tells for each key whether it seals or only reads.
    for id in &ids {
        let after = format!("{K}.{id}.secret");
        let at = r
            .effective
            .iter()
            .position(|line| line.key == after)
            .map_or(r.effective.len(), |i| i + 1);
        r.effective.insert(
            at,
            super::resolve::Effective {
                key: format!("{K}.{id}.use"),
                value: if *id == active_key {
                    "\"active\"".into()
                } else {
                    "\"read-only\"".into()
                },
                source: format!("derived from {B}.active_key"),
            },
        );
    }
    let config = AtRestConfig {
        active_key,
        keys: ids,
        admin_body_read: r.bool(B, "admin_body_read")?,
    };
    Ok((Some(config), secrets))
}

/// The keys of the inbox that contradict each other.
fn check_values(c: &InboxConfig) -> Result<(), String> {
    let d = &c.devices;
    if d.max_per_account > d.max_devices {
        return Err(format!(
            "inbox.devices.max_per_account must be at most inbox.devices.max_devices: max_per_account is {}, max_devices is {}",
            d.max_per_account, d.max_devices
        ));
    }
    if d.session_timeout < d.heartbeat_interval * 2 {
        return Err(format!(
            "inbox.devices.session_timeout must be at least two times inbox.devices.heartbeat_interval: session_timeout is {}, heartbeat_interval is {}",
            show_duration(d.session_timeout),
            show_duration(d.heartbeat_interval)
        ));
    }
    if d.unreachable_after < d.session_timeout {
        return Err(format!(
            "inbox.devices.unreachable_after must be at least inbox.devices.session_timeout: unreachable_after is {}, session_timeout is {}",
            show_duration(d.unreachable_after),
            show_duration(d.session_timeout)
        ));
    }
    if d.offline_warning_after < d.heartbeat_interval * 2 {
        return Err(format!(
            "inbox.devices.offline_warning_after must be at least two times inbox.devices.heartbeat_interval: offline_warning_after is {}, heartbeat_interval is {}",
            show_duration(d.offline_warning_after),
            show_duration(d.heartbeat_interval)
        ));
    }
    let s = &c.storage;
    if s.alert_retention < s.delivery_ttl {
        return Err(format!(
            "inbox.storage.alert_retention must be at least inbox.storage.delivery_ttl: alert_retention is {}, delivery_ttl is {}",
            show_duration(s.alert_retention),
            show_duration(s.delivery_ttl)
        ));
    }
    if let Some(alerts) = &c.watch {
        if !c.groups.contains_key(&alerts.group) {
            let ids: Vec<&str> = c.groups.keys().map(String::as_str).collect();
            return Err(format!(
                "invalid inbox.watch.group: expected the id of a block group of the block inbox ({}), got {:?}",
                ids.join(", "),
                alerts.group
            ));
        }
        if !alerts.service_unhealthy && !alerts.sign_in_burst {
            return Err("inbox.watch sends nothing: inbox.watch.service_unhealthy and inbox.watch.sign_in_burst are both false; set one of them to true, or remove the block".into());
        }
    }
    Ok(())
}

/// Two listeners overlap when they have the same port and the same address, or one of them
/// listens on all addresses.
fn overlap(a: SocketAddr, b: SocketAddr) -> bool {
    a.port() == b.port() && (a.ip() == b.ip() || a.ip().is_unspecified() || b.ip().is_unspecified())
}

/// The keys of the inbox that contradict the keys of the portal. The check runs when the whole
/// structure exists.
pub fn check_against(config: &Config, inbox: &InboxConfig, postgres: bool) -> Result<(), String> {
    let mut taken: Vec<(&str, SocketAddr)> =
        vec![("server.listen_address", config.server.listen_address)];
    if let Some(pdp) = &config.pdp {
        taken.push(("pdp.listen_address", pdp.listen_address));
    }
    if let Some(cluster) = &inbox.cluster {
        if !postgres {
            return Err("the block inbox.cluster needs the block postgres: it copies what PostgreSQL cannot take".into());
        }
        for (key, addr) in &taken {
            if overlap(cluster.listen, *addr) {
                return Err(format!(
                    "inbox.cluster.listen must not use the port of {key}: {key} is {addr}, inbox.cluster.listen is {}",
                    cluster.listen
                ));
            }
        }
    }
    if let Some(replica) = &inbox.replica {
        if postgres {
            return Err("the block inbox.replica is for the SQLite file: remove it when the block postgres is present".into());
        }
        if let StorageBackend::Sqlite(path) = &config.storage.backend {
            if path.parent().is_some_and(|dir| dir == replica.dir) {
                return Err(format!(
                    "inbox.replica.dir must not be the directory of storage.database: storage.database is {}, inbox.replica.dir is {}",
                    path.display(),
                    replica.dir.display()
                ));
            }
        }
    }
    Ok(())
}

/// Checks each block `api_key` against the accounts of the file (portal 3.4a, 4.1). The owner is an
/// account of the file. The owner must hold `keys.own.use` on the kind `api`, `alert.send` on each
/// group of the key, and `alert.send_user` on the service when the key has `direct_alerts`. The
/// error names the key, the expected value and the value found. The function also keeps, for each
/// key, the groups of the block where the owner holds `alert.read`, and the services that the owner
/// may open: a key never has more rights than its owner.
pub fn check_api_keys(
    config: &Config,
    inbox: &mut InboxConfig,
    features: &crate::portal_actions::Features,
) -> Result<(), String> {
    use crate::portal_actions as portal;
    let document = portal::document(features);
    for (name, key) in &mut inbox.api_keys {
        let Some(account) = config.access.accounts.get(&key.owner) else {
            return Err(format!(
                "invalid inbox.api_key.{name}.owner: expected the login of an account of the file (a block user), got {:?}",
                key.owner
            ));
        };
        if !portal::effective_kind(
            &account.grants,
            portal::KEYS_OWN_USE,
            crate::keys::KeyKind::Api,
        ) {
            return Err(format!(
                "inbox.api_key.{name}: the owner {:?} does not hold keys.own.use on service:portal/key_kind:api: add this grant to the block user, or the key can do nothing",
                key.owner
            ));
        }
        if key.direct_alerts
            && !portal::allows(
                &document,
                &key.owner,
                &account.grants,
                portal::ALERT_SEND_USER,
                None,
            )
        {
            return Err(format!(
                "inbox.api_key.{name}.direct_alerts: the owner {:?} does not hold alert.send_user on service:portal: add this grant to the block user, or set direct_alerts = false",
                key.owner
            ));
        }
        for group in &key.groups {
            if !portal::allows(
                &document,
                &key.owner,
                &account.grants,
                portal::ALERT_SEND,
                Some(group),
            ) {
                return Err(format!(
                    "inbox.api_key.{name}.groups: the owner {:?} does not hold alert.send on service:portal/inbox_group:{group}: add this grant to the block user, or remove the group from the key",
                    key.owner
                ));
            }
        }
        key.read_groups = key
            .groups
            .iter()
            .filter(|group| {
                portal::allows(
                    &document,
                    &key.owner,
                    &account.grants,
                    portal::ALERT_READ,
                    Some(group),
                )
            })
            .cloned()
            .collect();
        key.services = crate::inbox::rights::Services::of_grants(Some(&account.grants));
    }
    Ok(())
}
