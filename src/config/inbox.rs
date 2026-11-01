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
    /// The name that the phones show.
    pub name: String,
    pub devices: DevicesConfig,
    pub enrollment: EnrollmentConfig,
    pub storage: InboxStorageConfig,
    /// `None` without the block `inbox.replica`.
    pub replica: Option<ReplicaConfig>,
    /// `None` without the block `inbox.cluster`.
    pub cluster: Option<ClusterConfig>,
    /// `None` without the block `inbox.portal_alerts`.
    pub portal_alerts: Option<PortalAlertsConfig>,
    /// The blocks `group`, by id.
    pub groups: BTreeMap<String, GroupConfig>,
    /// The blocks `producer`, by name.
    pub producers: BTreeMap<String, ProducerConfig>,
}

/// The block `inbox.devices`.
#[derive(Clone, Debug)]
pub struct DevicesConfig {
    /// The public base URL of the portal as the phones reach it. It is the `s` of an enrollment code.
    pub public_address: String,
    pub max_devices: u32,
    pub heartbeat_interval: Duration,
    pub session_timeout: Duration,
    pub resend_after: Duration,
    pub max_inflight: u32,
    pub unreachable_after: Duration,
    pub offline_warning_after: Duration,
    pub max_auth_failures: u32,
    pub auth_failure_window: Duration,
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

/// The block `inbox.portal_alerts`: the alerts that the portal sends about itself.
#[derive(Clone, Debug)]
pub struct PortalAlertsConfig {
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

/// A block `group`.
#[derive(Clone, Debug)]
pub struct GroupConfig {
    pub name: String,
    pub description: String,
    /// A critical group never has a limit on `alert.send`.
    pub critical: bool,
}

/// A block `producer`, without its token.
#[derive(Clone, Debug)]
pub struct ProducerConfig {
    pub groups: Vec<String>,
    pub webhook_send_resolved: bool,
}

/// The secrets of the inbox. The program erases them from memory when it drops them.
pub struct InboxSecrets {
    /// The text of the key of the server.
    pub server_key: Secret,
    /// The token of each block `producer`, by name.
    pub producers: Vec<(String, Secret)>,
}

/// The shortest token of a producer.
pub const MIN_TOKEN_LEN: usize = 32;

fn socket(r: &Resolved, block: &str, name: &str) -> Result<SocketAddr, String> {
    r.text(block, name)?
        .parse::<SocketAddr>()
        .map_err(|e| format!("internal error: {block}.{name}: {e}"))
}

fn small(r: &Resolved, block: &str, name: &str) -> Result<u32, String> {
    u32::try_from(r.int(block, name)?)
        .map_err(|_| format!("internal error: {block}.{name} is out of range"))
}

/// Reads the block `inbox`. The result is `None` without the block.
pub fn build(r: &mut Resolved) -> Result<Option<(InboxConfig, InboxSecrets)>, String> {
    if !r.has_block("inbox") {
        return Ok(None);
    }
    let devices = DevicesConfig {
        public_address: r.text("inbox.devices", "public_address")?,
        max_devices: small(r, "inbox.devices", "max_devices")?,
        heartbeat_interval: r.duration("inbox.devices", "heartbeat_interval")?,
        session_timeout: r.duration("inbox.devices", "session_timeout")?,
        resend_after: r.duration("inbox.devices", "resend_after")?,
        max_inflight: small(r, "inbox.devices", "max_inflight")?,
        unreachable_after: r.duration("inbox.devices", "unreachable_after")?,
        offline_warning_after: r.duration("inbox.devices", "offline_warning_after")?,
        max_auth_failures: small(r, "inbox.devices", "max_auth_failures")?,
        auth_failure_window: r.duration("inbox.devices", "auth_failure_window")?,
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

    let portal_alerts = if r.has_block("inbox.portal_alerts") {
        Some(PortalAlertsConfig {
            group: r.text("inbox.portal_alerts", "group")?,
            service_unhealthy: r.bool("inbox.portal_alerts", "service_unhealthy")?,
            unhealthy_after: r.duration("inbox.portal_alerts", "unhealthy_after")?,
            sign_in_burst: r.bool("inbox.portal_alerts", "sign_in_burst")?,
            sign_in_failures: small(r, "inbox.portal_alerts", "sign_in_failures")?,
            sign_in_window: r.duration("inbox.portal_alerts", "sign_in_window")?,
            repeat_after: r.duration("inbox.portal_alerts", "repeat_after")?,
            recovered: r.bool("inbox.portal_alerts", "recovered")?,
        })
    } else {
        None
    };

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
    let mut producers = BTreeMap::new();
    let mut tokens: Vec<(String, Secret)> = Vec::new();
    for block in r.labeled.iter_mut().filter(|b| b.kind == "producer") {
        let name = block.label.clone();
        super::keys::check_inbox_id(&name)
            .map_err(|e| format!("inbox.producer {name:?}: the label is not a name: {e}"))?;
        let listed = block.list("groups")?;
        if listed.is_empty() {
            return Err(format!(
                "invalid inbox.producer.{name}.groups: expected at least one group id, got []"
            ));
        }
        for group in &listed {
            if !groups.contains_key(group) {
                return Err(format!(
                    "invalid inbox.producer.{name}.groups: the group {group:?} is not a block group of the block inbox"
                ));
            }
        }
        let token = block
            .take_secret("token")
            .ok_or_else(|| format!("internal error: inbox.producer.{name} has no token"))?;
        producers.insert(
            name.clone(),
            ProducerConfig {
                groups: listed,
                webhook_send_resolved: block.bool("webhook_send_resolved")?,
            },
        );
        tokens.push((name, token));
    }

    let secrets = InboxSecrets {
        server_key: r
            .take_secret("inbox", "server_key")
            .ok_or("internal error: the secret inbox.server_key is missing")?,
        producers: tokens,
    };
    let config = InboxConfig {
        name: r.text("inbox", "name")?,
        devices,
        enrollment,
        storage,
        replica,
        cluster,
        portal_alerts,
        groups,
        producers,
    };
    check_values(&config)?;
    Ok(Some((config, secrets)))
}

/// The keys of the inbox that contradict each other.
fn check_values(c: &InboxConfig) -> Result<(), String> {
    let d = &c.devices;
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
    if let Some(alerts) = &c.portal_alerts {
        if !c.groups.contains_key(&alerts.group) {
            let ids: Vec<&str> = c.groups.keys().map(String::as_str).collect();
            return Err(format!(
                "invalid inbox.portal_alerts.group: expected the id of a block group of the block inbox ({}), got {:?}",
                ids.join(", "),
                alerts.group
            ));
        }
        if !alerts.service_unhealthy && !alerts.sign_in_burst {
            return Err("inbox.portal_alerts sends nothing: inbox.portal_alerts.service_unhealthy and inbox.portal_alerts.sign_in_burst are both false; set one of them to true, or remove the block".into());
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
