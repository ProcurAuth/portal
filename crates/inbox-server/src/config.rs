//! Declarative HCL configuration.
//!
//! Every attribute is mandatory: there are no implicit defaults, unknown attributes
//! or blocks are rejected, and semantic checks run before anything starts. Secrets
//! are never inline; the configuration only holds paths to secret files.

use std::collections::{BTreeMap, HashSet};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::time::Duration;

use ipnet::IpNet;
use serde::{Deserialize, Deserializer};

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub server: Server,
    pub storage: Storage,
    pub devices: Devices,
    pub admin: Admin,
    pub security: Security,
    pub limits: Limits,
    pub logging: Logging,
    pub telemetry: Telemetry,
    /// `group "<id>" { ... }`
    pub group: BTreeMap<String, Group>,
    /// `producer "<name>" { ... }`, optional: the admin front can always send alerts.
    #[serde(default)]
    pub producer: BTreeMap<String, Producer>,
    /// Optional `replica { ... }` block: standby replication to a shared directory (see `replica.rs`).
    #[serde(default)]
    pub replica: Option<Replica>,
    /// Optional `postgres { ... }` block: PostgreSQL instead of SQLite, so that several instances can be active at once.
    #[serde(default)]
    pub postgres: Option<Postgres>,
    /// Optional `cluster { ... }` block (with `postgres`): how the instances copy alerts to each other while the database has
    /// no writable primary (see `peer.rs`).
    #[serde(default)]
    pub cluster: Option<Cluster>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Server {
    /// Display name shown in the app and the front.
    pub name: String,
    /// Devices, producer API, webhooks and health probes.
    pub listen: SocketAddr,
    /// Admin front and admin API.
    pub admin_listen: SocketAddr,
    /// Peers allowed to set X-Forwarded-For / X-Real-IP.
    pub trusted_proxies: Vec<IpNet>,
    #[serde(deserialize_with = "duration")]
    pub shutdown_grace: Duration,
    pub server_key_file: PathBuf,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Storage {
    /// SQLite database file. Required, unless a `postgres` block selects PostgreSQL (then it must be absent).
    #[serde(default)]
    pub path: Option<PathBuf>,
    /// An alert a device has not received within this delay is no longer sent to it.
    #[serde(deserialize_with = "duration")]
    pub delivery_ttl: Duration,
    /// Alerts (delivered or expired) older than this are purged. Must be >= delivery_ttl.
    #[serde(deserialize_with = "duration")]
    pub alert_retention: Duration,
    #[serde(deserialize_with = "duration")]
    pub connection_log_retention: Duration,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Devices {
    /// OPAQUE record of the shared device password (`inbox credential`).
    pub credential_file: PathBuf,
    /// The app sends a ping at this interval.
    #[serde(deserialize_with = "duration")]
    pub heartbeat_interval: Duration,
    /// A session without any frame for this long is closed.
    #[serde(deserialize_with = "duration")]
    pub session_timeout: Duration,
    /// An alert sent but not acknowledged as received is sent again after this.
    #[serde(deserialize_with = "duration")]
    pub resend_after: Duration,
    /// Alerts sent and not yet acknowledged, per session.
    pub max_inflight: u32,
    /// A subscribed device not seen for this long is reported unreachable.
    #[serde(deserialize_with = "duration")]
    pub unreachable_after: Duration,
    /// The app warns its user after being disconnected for this long.
    #[serde(deserialize_with = "duration")]
    pub offline_warning_after: Duration,
    pub max_devices: u32,
}

/// The admin front has no password: whoever reaches `server.admin_listen` can send
/// alerts and revoke devices. Expose it on localhost, a private network or behind
/// an authenticating reverse proxy.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Admin {
    /// Accepted `Host` header values (e.g. `localhost:8081`). Any other host is
    /// refused, which defeats DNS rebinding from a malicious web page.
    pub allowed_hosts: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Security {
    /// Device password attempts per client address within `auth_failure_window`.
    pub max_auth_failures: u32,
    #[serde(deserialize_with = "duration")]
    pub auth_failure_window: Duration,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Limits {
    pub max_request_bytes: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LogLevel {
    Error,
    Warn,
    Info,
    Debug,
    Trace,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LogFormat {
    Json,
    Text,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Logging {
    pub level: LogLevel,
    pub format: LogFormat,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Telemetry {
    pub enabled: bool,
    /// Required when enabled. OTLP over HTTP/protobuf, e.g. `http://otel-collector:4318`.
    pub otlp_endpoint: Option<String>,
    pub service_name: Option<String>,
    #[serde(default, deserialize_with = "opt_duration")]
    pub export_interval: Option<Duration>,
}

/// Present: Inbox keeps its state in PostgreSQL, shared by every instance, and any number of instances can run (and
/// serve phones) at the same time. Absent: the state is a SQLite file owned by one instance.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Postgres {
    pub host: String,
    pub port: u16,
    pub database: String,
    pub user: String,
    /// File holding the password of `user` (a secret: never inline).
    pub password_file: PathBuf,
    /// Connections used by requests and sessions (one more is kept for notifications).
    pub pool_size: u32,
    #[serde(deserialize_with = "duration")]
    pub connect_timeout: Duration,
}

/// Present: the instances copy the alerts they accept without the database (`spool.rs`) to each other, over plain HTTP.
/// Absent: such an alert lives on the instance that accepted it until the database is back.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Cluster {
    /// Where this instance receives the requests of the other instances. Reachable from the pods of the service only.
    pub listen: SocketAddr,
    /// `host:port` names that list the instances (a headless Service name and `cluster.listen`'s port). Every address they
    /// resolve to is asked who it is; this instance's own address is recognised and skipped.
    pub peers: Vec<String>,
}

/// Present: the database is copied to a directory shared with the other instances that may take over.
/// Absent: Inbox runs alone, with the database on its local disk only.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Replica {
    /// Directory shared by every instance that may run Inbox (a ReadWriteMany plain-file volume).
    pub dir: PathBuf,
    /// A changed database is copied at most this often. Accepted alerts, enrollments and revocations do not wait.
    #[serde(deserialize_with = "duration")]
    pub snapshot_interval: Duration,
    /// An instance whose lease has not changed for this long is considered lost by the others, which then take over.
    #[serde(deserialize_with = "duration")]
    pub lease_ttl: Duration,
    /// How long an accepted alert waits for the copy that contains it.
    #[serde(deserialize_with = "duration")]
    pub sync_timeout: Duration,
    /// true: an alert is acknowledged only once copied (503 `replication_unavailable` otherwise, the producer
    /// retries); false: acknowledged after the local commit, copied as soon as possible.
    pub sync_alerts: bool,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Group {
    pub name: String,
    pub description: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Producer {
    /// Bearer token file (`inbox token`).
    pub token_file: PathBuf,
    /// Groups this producer may alert.
    pub groups: Vec<String>,
    /// Whether Alertmanager / Grafana "resolved" notifications become alerts.
    pub webhook_send_resolved: bool,
}

// ---------------------------------------------------------------- durations

/// Parses `"500ms"`, `"10s"`, `"5m"`, `"12h"`, `"30d"`.
pub fn parse_duration(text: &str) -> Result<Duration, String> {
    let text = text.trim();
    let split = text.find(|c: char| !c.is_ascii_digit()).ok_or("missing unit")?;
    let (num, unit) = text.split_at(split);
    let n: u64 = num.parse().map_err(|_| "invalid number".to_string())?;
    let ms = match unit {
        "ms" => n,
        "s" => n.checked_mul(1_000).ok_or("overflow")?,
        "m" => n.checked_mul(60_000).ok_or("overflow")?,
        "h" => n.checked_mul(3_600_000).ok_or("overflow")?,
        "d" => n.checked_mul(86_400_000).ok_or("overflow")?,
        _ => return Err(format!("unknown unit {unit:?} (ms, s, m, h, d)")),
    };
    Ok(Duration::from_millis(ms))
}

fn duration<'de, D: Deserializer<'de>>(d: D) -> Result<Duration, D::Error> {
    let text = String::deserialize(d)?;
    parse_duration(&text).map_err(|e| serde::de::Error::custom(format!("duration {text:?}: {e}")))
}

fn opt_duration<'de, D: Deserializer<'de>>(d: D) -> Result<Option<Duration>, D::Error> {
    duration(d).map(Some)
}

// ---------------------------------------------------------------- loading

impl Config {
    pub fn load(path: &Path) -> Result<Config, String> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| format!("cannot read config {}: {e}", path.display()))?;
        Config::parse(&text)
    }

    pub fn parse(text: &str) -> Result<Config, String> {
        let config: Config = hcl::from_str(text).map_err(|e| format!("invalid config: {e}"))?;
        config.validate()?;
        Ok(config)
    }

    fn validate(&self) -> Result<(), String> {
        let mut errors = Vec::new();
        let mut check = |ok: bool, msg: &str| {
            if !ok {
                errors.push(msg.to_string());
            }
        };

        let s = &self.server;
        check(!s.name.trim().is_empty() && s.name.len() <= 64, "server.name must be 1..64 bytes");
        check(s.listen != s.admin_listen, "server.listen and server.admin_listen must differ");
        check(s.shutdown_grace >= Duration::from_secs(1), "server.shutdown_grace must be >= 1s");

        let st = &self.storage;
        check(
            st.alert_retention >= Duration::from_secs(3600),
            "storage.alert_retention must be >= 1h",
        );
        check(st.delivery_ttl >= Duration::from_secs(60), "storage.delivery_ttl must be >= 1m");
        check(
            st.alert_retention >= st.delivery_ttl,
            "storage.alert_retention must be >= storage.delivery_ttl",
        );
        check(
            st.connection_log_retention >= Duration::from_secs(3600),
            "storage.connection_log_retention must be >= 1h",
        );

        let d = &self.devices;
        check(
            d.heartbeat_interval >= Duration::from_secs(5),
            "devices.heartbeat_interval must be >= 5s",
        );
        check(
            d.session_timeout >= d.heartbeat_interval * 2,
            "devices.session_timeout must be >= 2 x devices.heartbeat_interval",
        );
        check(d.resend_after >= Duration::from_secs(1), "devices.resend_after must be >= 1s");
        check((1..=1024).contains(&d.max_inflight), "devices.max_inflight must be in 1..1024");
        check(
            d.unreachable_after >= d.session_timeout,
            "devices.unreachable_after must be >= devices.session_timeout",
        );
        check(
            d.offline_warning_after >= d.heartbeat_interval * 2,
            "devices.offline_warning_after must be >= 2 x devices.heartbeat_interval",
        );
        check((1..=100_000).contains(&d.max_devices), "devices.max_devices must be in 1..100000");

        let hosts = &self.admin.allowed_hosts;
        check(!hosts.is_empty(), "admin.allowed_hosts must not be empty");
        check(
            hosts.iter().all(|h| {
                !h.is_empty() && h.len() <= 255 && h.bytes().all(|b| b.is_ascii_graphic())
            }),
            "admin.allowed_hosts entries must be host[:port] values",
        );

        let sec = &self.security;
        check(
            (1..=1000).contains(&sec.max_auth_failures),
            "security.max_auth_failures must be in 1..1000",
        );
        check(
            sec.auth_failure_window >= Duration::from_secs(1),
            "security.auth_failure_window must be >= 1s",
        );

        let l = &self.limits;
        check(
            (4096..=1024 * 1024).contains(&l.max_request_bytes),
            "limits.max_request_bytes must be in 4096..1048576",
        );

        check(
            !(self.postgres.is_some() && st.path.is_some()),
            "storage.path must be absent when the postgres block is present",
        );
        check(
            self.postgres.is_some() || st.path.is_some(),
            "storage.path is required (or a postgres block to use PostgreSQL)",
        );
        if let Some(p) = &self.postgres {
            check(
                !p.host.trim().is_empty() && p.host.len() <= 253,
                "postgres.host must be 1..253 bytes",
            );
            check(p.port != 0, "postgres.port must not be 0");
            check(
                [&p.database, &p.user].iter().all(|n| !n.is_empty() && n.len() <= 63),
                "postgres.database and postgres.user must be 1..63 bytes",
            );
            check((2..=64).contains(&p.pool_size), "postgres.pool_size must be in 2..64");
            check(
                (Duration::from_secs(1)..=Duration::from_secs(60)).contains(&p.connect_timeout),
                "postgres.connect_timeout must be in 1s..60s",
            );
            check(
                self.replica.is_none(),
                "the replica block is for SQLite: remove it when using postgres",
            );
        }
        if let Some(c) = &self.cluster {
            check(
                self.postgres.is_some(),
                "the cluster block needs the postgres block: it replicates what PostgreSQL cannot take",
            );
            check(
                c.listen != s.listen && c.listen != s.admin_listen,
                "cluster.listen must differ from server.listen and server.admin_listen",
            );
            check(
                !c.peers.is_empty() && c.peers.len() <= 16,
                "cluster.peers must list 1..16 host:port names",
            );
            check(
                c.peers.iter().all(|p| {
                    p.rsplit_once(':').is_some_and(|(host, port)| {
                        !host.is_empty()
                            && host.len() <= 253
                            && port.parse::<u16>().is_ok_and(|n| n != 0)
                    })
                }),
                "cluster.peers entries must be host:port",
            );
        }
        if let Some(r) = &self.replica {
            check(r.dir.is_absolute(), "replica.dir must be an absolute path");
            check(
                r.snapshot_interval >= Duration::from_secs(1),
                "replica.snapshot_interval must be >= 1s",
            );
            check(r.lease_ttl >= Duration::from_secs(5), "replica.lease_ttl must be >= 5s");
            check(
                (Duration::from_secs(1)..=Duration::from_secs(60)).contains(&r.sync_timeout),
                "replica.sync_timeout must be in 1s..60s",
            );
            check(
                self.storage.path.as_ref().and_then(|p| p.parent()).is_none_or(|p| p != r.dir),
                "replica.dir must not be the directory of storage.path",
            );
        }

        let t = &self.telemetry;
        if t.enabled {
            match &t.otlp_endpoint {
                Some(e) if e.starts_with("http://") => {}
                Some(_) => errors.push(
                    "telemetry.otlp_endpoint must be an http:// OTLP/HTTP endpoint".to_string(),
                ),
                None => errors.push("telemetry.otlp_endpoint is required when enabled".to_string()),
            }
            if t.service_name.as_deref().is_none_or(|n| n.trim().is_empty()) {
                errors.push("telemetry.service_name is required when enabled".to_string());
            }
            match t.export_interval {
                Some(i) if i >= Duration::from_secs(1) => {}
                Some(_) => errors.push("telemetry.export_interval must be >= 1s".to_string()),
                None => {
                    errors.push("telemetry.export_interval is required when enabled".to_string())
                }
            }
        } else if t.otlp_endpoint.is_some()
            || t.service_name.is_some()
            || t.export_interval.is_some()
        {
            errors.push("telemetry: remove endpoint settings or set enabled = true".to_string());
        }

        if self.group.is_empty() {
            errors.push("at least one group block is required".to_string());
        }
        for (id, g) in &self.group {
            if !valid_id(id) {
                errors.push(format!("group {id:?}: id must match [a-z0-9_-]{{1,64}}"));
            }
            if g.name.trim().is_empty() || g.name.len() > 128 {
                errors.push(format!("group {id:?}: name must be 1..128 bytes"));
            }
            if g.description.len() > 512 {
                errors.push(format!("group {id:?}: description must be <= 512 bytes"));
            }
        }

        let mut token_files = HashSet::new();
        for (name, p) in &self.producer {
            if !valid_id(name) {
                errors.push(format!("producer {name:?}: name must match [a-z0-9_-]{{1,64}}"));
            }
            if p.groups.is_empty() {
                errors.push(format!("producer {name:?}: groups must not be empty"));
            }
            for g in &p.groups {
                if !self.group.contains_key(g) {
                    errors.push(format!("producer {name:?}: unknown group {g:?}"));
                }
            }
            if !token_files.insert(p.token_file.clone()) {
                errors.push(format!("producer {name:?}: token_file shared with another producer"));
            }
        }

        if errors.is_empty() {
            Ok(())
        } else {
            Err(format!("invalid config:\n  - {}", errors.join("\n  - ")))
        }
    }
}

pub fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_')
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub const EXAMPLE: &str = include_str!("../../../deploy/inbox/config.example.hcl");

    #[test]
    fn example_config_is_valid() {
        let c = Config::parse(EXAMPLE).unwrap();
        assert_eq!(c.devices.heartbeat_interval, Duration::from_secs(20));
        assert!(c.group.contains_key("infra"));
    }

    #[test]
    fn every_attribute_is_mandatory() {
        // Drop each attribute line of the example in turn: the config must be refused.
        let lines: Vec<&str> = EXAMPLE.lines().collect();
        for (i, line) in lines.iter().enumerate() {
            let t = line.trim();
            if t.starts_with('#')
                || !t.contains(" = ")
                || t.starts_with("otlp_endpoint")
                || t.starts_with("service_name")
                || t.starts_with("export_interval")
            {
                continue;
            }
            let mut copy = lines.clone();
            copy.remove(i);
            assert!(Config::parse(&copy.join("\n")).is_err(), "accepted without line: {t}");
        }
    }

    #[test]
    fn unknown_attribute_is_rejected() {
        let text = EXAMPLE.replace("max_devices", "max_devicez");
        assert!(Config::parse(&text).is_err());
        let text = format!("{EXAMPLE}\nbogus {{ a = 1 }}\n");
        assert!(Config::parse(&text).is_err());
    }

    #[test]
    fn producer_groups_must_exist() {
        let text = EXAMPLE
            .replace("groups                = [\"infra\"]", "groups                = [\"nope\"]");
        let err = Config::parse(&text).unwrap_err();
        assert!(err.contains("unknown group"), "{err}");
    }

    const REPLICA: &str = r#"
replica {
  dir               = "/replica"
  snapshot_interval = "10s"
  lease_ttl         = "30s"
  sync_timeout      = "5s"
  sync_alerts       = true
}
"#;

    #[test]
    fn replica_block_is_optional_and_strict() {
        assert!(Config::parse(EXAMPLE).unwrap().replica.is_none());
        let c = Config::parse(&format!("{EXAMPLE}{REPLICA}")).unwrap();
        let r = c.replica.unwrap();
        assert_eq!(r.lease_ttl, Duration::from_secs(30));
        assert!(r.sync_alerts);
        // Every attribute of the block is mandatory, like everywhere else.
        for line in REPLICA.lines().filter(|l| l.contains(" = ")) {
            let broken = REPLICA.replace(&format!("{line}\n"), "");
            assert!(
                Config::parse(&format!("{EXAMPLE}{broken}")).is_err(),
                "accepted without: {line}"
            );
        }
        let bad = REPLICA.replace("\"30s\"", "\"2s\"");
        assert!(Config::parse(&format!("{EXAMPLE}{bad}")).unwrap_err().contains("lease_ttl"));
        let bad = REPLICA.replace("/replica", "replica");
        assert!(Config::parse(&format!("{EXAMPLE}{bad}")).unwrap_err().contains("absolute"));
        let bad = REPLICA.replace("/replica", "/var/lib/inbox");
        assert!(
            Config::parse(&format!("{EXAMPLE}{bad}"))
                .unwrap_err()
                .contains("directory of storage.path")
        );
    }

    const POSTGRES: &str = r#"
postgres {
  host            = "postgresql-rw.data.svc.cluster.local"
  port            = 5432
  database        = "inbox"
  user            = "inbox"
  password_file   = "/run/secrets/inbox_db_password"
  pool_size       = 8
  connect_timeout = "5s"
}
"#;

    /// The example selects SQLite; with PostgreSQL the file path goes away.
    fn with_postgres() -> String {
        EXAMPLE.replace("  path = \"/var/lib/inbox/inbox.db\"\n", "") + POSTGRES
    }

    #[test]
    fn postgres_replaces_the_sqlite_path_and_is_strict() {
        let c = Config::parse(&with_postgres()).unwrap();
        assert!(c.storage.path.is_none());
        assert_eq!(c.postgres.unwrap().pool_size, 8);
        // Both, or neither: refused.
        let both = format!("{EXAMPLE}{POSTGRES}");
        assert!(Config::parse(&both).unwrap_err().contains("storage.path must be absent"));
        let neither = EXAMPLE.replace("  path = \"/var/lib/inbox/inbox.db\"\n", "");
        assert!(Config::parse(&neither).unwrap_err().contains("storage.path is required"));
        // Every attribute of the block is mandatory.
        for line in POSTGRES.lines().filter(|l| l.contains(" = ")) {
            let broken = with_postgres().replace(&format!("{line}\n"), "");
            assert!(Config::parse(&broken).is_err(), "accepted without: {line}");
        }
        let bad = with_postgres().replace("pool_size       = 8", "pool_size       = 1");
        assert!(Config::parse(&bad).unwrap_err().contains("pool_size"));
        // The standby replication is for SQLite.
        let replica = format!("{}{REPLICA}", with_postgres());
        assert!(Config::parse(&replica).unwrap_err().contains("replica block is for SQLite"));
    }

    const CLUSTER: &str = r#"
cluster {
  listen = "0.0.0.0:8082"
  peers  = ["inbox-peers.apps.svc.cluster.local:8082"]
}
"#;

    #[test]
    fn cluster_block_is_optional_strict_and_needs_postgres() {
        assert!(Config::parse(&with_postgres()).unwrap().cluster.is_none());
        let c = Config::parse(&format!("{}{CLUSTER}", with_postgres())).unwrap();
        assert_eq!(c.cluster.unwrap().peers.len(), 1);
        // Every attribute of the block is mandatory.
        for line in CLUSTER.lines().filter(|l| l.contains(" = ")) {
            let broken = CLUSTER.replace(&format!("{line}\n"), "");
            assert!(
                Config::parse(&format!("{}{broken}", with_postgres())).is_err(),
                "accepted without: {line}"
            );
        }
        // SQLite has no spool to replicate.
        let sqlite = format!("{EXAMPLE}{CLUSTER}");
        assert!(Config::parse(&sqlite).unwrap_err().contains("needs the postgres block"));
        let bad = CLUSTER.replace("8082\"]", "\"]");
        assert!(
            Config::parse(&format!("{}{bad}", with_postgres())).unwrap_err().contains("host:port")
        );
        let bad = CLUSTER.replace("0.0.0.0:8082", "0.0.0.0:8080");
        assert!(
            Config::parse(&format!("{}{bad}", with_postgres()))
                .unwrap_err()
                .contains("must differ")
        );
        let empty = CLUSTER.replace("[\"inbox-peers.apps.svc.cluster.local:8082\"]", "[]");
        assert!(
            Config::parse(&format!("{}{empty}", with_postgres())).unwrap_err().contains("peers")
        );
    }

    #[test]
    fn durations() {
        assert_eq!(parse_duration("500ms").unwrap(), Duration::from_millis(500));
        assert_eq!(parse_duration("30d").unwrap(), Duration::from_secs(30 * 86_400));
        assert!(parse_duration("10").is_err());
        assert!(parse_duration("1w").is_err());
        assert!(parse_duration("s").is_err());
    }
}
