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
    pub path: PathBuf,
    /// Delivered alerts older than this are purged. Undelivered alerts never are.
    #[serde(deserialize_with = "duration")]
    pub alert_retention: Duration,
    #[serde(deserialize_with = "duration")]
    pub connection_log_retention: Duration,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Devices {
    /// OPAQUE record of the shared device password (`inbox credential --role device`).
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

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Admin {
    /// OPAQUE record of the admin password (`inbox credential --role admin`).
    pub credential_file: PathBuf,
    #[serde(deserialize_with = "duration")]
    pub session_ttl: Duration,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Security {
    /// Failed password attempts per client address within `auth_failure_window`.
    pub max_auth_failures: u32,
    #[serde(deserialize_with = "duration")]
    pub auth_failure_window: Duration,
    /// Accepted clock skew on signed admin requests.
    #[serde(deserialize_with = "duration")]
    pub admin_request_max_skew: Duration,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Limits {
    pub max_title_bytes: usize,
    pub max_message_bytes: usize,
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

        check(
            self.admin.session_ttl >= Duration::from_secs(60)
                && self.admin.session_ttl <= Duration::from_secs(7 * 86_400),
            "admin.session_ttl must be in 1m..7d",
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
        check(
            sec.admin_request_max_skew >= Duration::from_secs(5)
                && sec.admin_request_max_skew <= Duration::from_secs(600),
            "security.admin_request_max_skew must be in 5s..10m",
        );

        let l = &self.limits;
        check((1..=1024).contains(&l.max_title_bytes), "limits.max_title_bytes must be in 1..1024");
        check(
            (1..=32 * 1024).contains(&l.max_message_bytes),
            "limits.max_message_bytes must be in 1..32768 (one encrypted frame)",
        );
        check(
            (4096..=1024 * 1024).contains(&l.max_request_bytes),
            "limits.max_request_bytes must be in 4096..1048576",
        );

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

    pub const EXAMPLE: &str = include_str!("../../../config.example.hcl");

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

    #[test]
    fn durations() {
        assert_eq!(parse_duration("500ms").unwrap(), Duration::from_millis(500));
        assert_eq!(parse_duration("30d").unwrap(), Duration::from_secs(30 * 86_400));
        assert!(parse_duration("10").is_err());
        assert!(parse_duration("1w").is_err());
        assert!(parse_duration("s").is_err());
    }
}
