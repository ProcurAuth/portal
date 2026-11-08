//! The configuration of the program: one HCL file, strict and closed.
//!
//! - The table in `keys.rs` holds the type, the range, the default and the description of each key.
//! - `hcl.rs` reads the file. `resolve.rs` gives every value a type once and reads the secrets.
//! - This module builds the typed structure that the rest of the program reads.
//!
//! The file is the only source. The program reads an environment variable only when the file
//! names it (`<name>_env`), and a file only when the file names it (`<name>_file`).

mod accounts;
mod hcl;
mod inbox;
pub mod keys;
mod resolve;

use std::collections::BTreeMap;
use std::fs;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::grants::{Grant, Mode};

#[allow(unused_imports)] // some of these types serve only the module `inbox`
pub use inbox::{
    ApiKeyConfig, AtRestConfig, ClusterConfig, DevicesConfig, DigestConfig, GroupConfig,
    InboxConfig, InboxSecrets, InboxStorageConfig, ItemsConfig, LeaseConfig, ReplicaConfig,
    WatchConfig,
};
pub use resolve::{Effective, Secret};
use resolve::{Env, Resolved};

pub struct Config {
    pub server: ServerConfig,
    pub authentication: AuthenticationConfig,
    pub session: SessionConfig,
    pub security: SecurityConfig,
    pub metrics: MetricsConfig,
    pub discovery: DiscoveryConfig,
    pub access: AccessConfig,
    /// `None` when the file has no `identity` block. `/auth/verify` gives no identity token.
    pub identity: Option<IdentityConfig>,
    /// `None` when the file has no `pdp` block: the PDP listener is off.
    pub pdp: Option<PdpConfig>,
    /// `None` when the file has no `keys` block: the portal refuses every key (portal 10.1).
    pub keys: Option<KeysConfig>,
    /// The values of the block `keys`, or the defaults of the table without the block. The bounds
    /// of the counter rows and of the grants apply also without the block (portal 10.1a).
    pub key_limits: KeysConfig,
    pub users: UsersConfig,
    pub storage: StorageConfig,
    pub ui: UiConfig,
    pub logging: LoggingConfig,
    /// `None` when the file has no `inbox` block: the inbox is off.
    pub inbox: Option<InboxConfig>,
    /// The servers whose block `pdp_client` has `approvals = true`.
    pub pdp_approvals: std::collections::BTreeSet<String>,
    /// The value of each key that applies, with its source. Secrets are hidden.
    pub effective: Vec<Effective>,
}

/// The secrets of the file. They are not part of `Config`, so the program can erase them after
/// use. Each value is erased from memory when it is dropped.
pub struct Secrets {
    /// The login of the superadmin, exactly as the file or the variable gives it.
    pub superadmin_login: String,
    pub superadmin_password: Secret,
    /// The key of the TOTP secrets. `None` when `authentication.mfa` is `off`.
    pub totp_key: Option<Secret>,
    /// The signing key. `None` when `session.secret_mode` is `ephemeral`.
    pub session_secret: Option<Secret>,
    /// `None` when the file has no `postgres` block. The password is valid UTF-8.
    pub postgres_password: Option<Secret>,
    /// The Ed25519 private key of the identity tokens. `None` without the `identity` block.
    pub identity_key: Option<Secret>,
    /// The bearer token of each `pdp_client` block, in the order of the file.
    pub pdp_tokens: Vec<(String, Secret)>,
    /// The secrets of the block `inbox`. `None` without the block.
    pub inbox: Option<InboxSecrets>,
}

/// The grants of the users and the actions of the services.
#[derive(Clone, Debug)]
pub struct AccessConfig {
    pub grants_mode: Mode,
    /// The `user` blocks: login to grants. Empty in the mode `dynamic`.
    pub configured_grants: BTreeMap<String, Vec<Grant>>,
    /// The `user` blocks: the accounts of the file, with their keys (portal 3.4a).
    pub accounts: BTreeMap<String, FileAccount>,
    pub actions_timeout: Duration,
    pub actions_refresh: Duration,
    /// The longest time that the last good document of a service stays valid (portal 10.3a).
    pub actions_max_age: Duration,
    /// The longest wait for the first reads of the documents at start (portal 10.2c).
    pub start_read_deadline: Duration,
    /// The size that the document of a service can have (portal 7.9).
    pub bounds: crate::actions::Bounds,
}

/// An account of the file (portal 3.4a): a block `user`.
#[derive(Clone, Debug)]
pub struct FileAccount {
    pub grants: Vec<Grant>,
    /// The Argon2id digest of the password. Without it, the account opens no session.
    pub password_hash: Option<String>,
    pub keys: Vec<FileKey>,
}

/// A key of the file (portal 3.4c): a block `key` inside a block `user`. The program never holds
/// the text of the key, only its SHA-256 digest.
#[derive(Clone, Debug)]
pub struct FileKey {
    pub name: String,
    /// The local part of the subject id (portal 3.4c).
    pub id: String,
    pub digest: [u8; 32],
    /// The attribute `interaction` of the block.
    pub interaction: crate::keys::Interaction,
    pub grants: Vec<Grant>,
}

/// The block `keys` (portal 10.2).
#[derive(Clone, Debug)]
pub struct KeysConfig {
    pub default_lifetime: Duration,
    pub directory_max_age: Duration,
    pub max_per_user: usize,
    pub max_counter_rows: usize,
    pub max_directory_grants: usize,
    pub history_days: u32,
    /// The kind `mcp` is on (portal 5.1, 10.2).
    pub mcp: bool,
    /// The kind `api` is on (portal 5.1, 10.2).
    pub api: bool,
}

impl KeysConfig {
    /// The kinds of key that are on, in the order of [`crate::keys::KeyKind::ALL`].
    pub fn kinds(&self) -> Vec<crate::keys::KeyKind> {
        crate::keys::KeyKind::ALL
            .into_iter()
            .filter(|kind| match kind {
                crate::keys::KeyKind::Mcp => self.mcp,
                crate::keys::KeyKind::Api => self.api,
            })
            .collect()
    }
}

/// The identity token that `/auth/verify` gives to a service (contract 11.3).
#[derive(Clone, Debug)]
pub struct IdentityConfig {
    /// The response header, in lower case.
    pub header: String,
    pub issuer: String,
    pub token_ttl: Duration,
}

/// The AuthZEN PDP listener (contract 11.5).
#[derive(Clone, Debug)]
pub struct PdpConfig {
    pub listen_address: SocketAddr,
    /// The base URL that the services use, without the last `/`.
    pub url: String,
    pub max_body_bytes: usize,
    pub max_evaluations: usize,
    /// `true`: the URL can be `http` (portal 9.7a).
    pub plain_http: bool,
    /// The database connections that count the uses (portal 7.7e).
    pub counter_connections: usize,
}

#[derive(Clone, Debug)]
pub struct UsersConfig {
    /// `false`: the superadmin makes no account in the database (the route answers 404).
    pub allow_creation: bool,
    pub max_users: usize,
    /// The largest number of grants of one user (portal 10.2d).
    pub max_grants: usize,
}

/// Where the accounts and the connection history live.
#[derive(Clone, Debug)]
pub struct StorageConfig {
    pub backend: StorageBackend,
    /// Sessions, service accesses and audit events older than this are deleted.
    pub retention_days: u32,
}

#[derive(Clone, Debug)]
pub enum StorageBackend {
    /// One SQLite file: one instance owns it, and its sessions are in its memory.
    Sqlite(PathBuf),
    /// A shared PostgreSQL database (the `postgres` block). Any number of instances are active at
    /// once. The sessions are in the database too.
    Postgres(PostgresConfig),
}

#[derive(Clone, Debug)]
pub struct PostgresConfig {
    pub host: String,
    pub port: u16,
    pub database: String,
    pub user: String,
    /// Connections kept to the database.
    pub pool_size: usize,
    pub connect_timeout: Duration,
}

#[derive(Clone, Debug)]
pub struct UiConfig {
    /// Directory of the offline icon library (`di/`, `si/`, `tb/`).
    /// When the directory is missing, icons fall back to initials.
    pub icons_dir: PathBuf,
    /// The icon of the browser tab (`ui.favicon`).
    pub favicon: Favicon,
    /// Each tile of the launcher opens its service in a new tab (`ui.services_new_tab`).
    pub services_new_tab: bool,
}

/// The values of `ui.favicon`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Favicon {
    /// The pages after sign-in link the built-in icon.
    Default,
    /// No page links an icon.
    None,
}

impl Favicon {
    /// The exact names of the table: `default` and `none`.
    pub fn parse(value: &str) -> Result<Self, String> {
        match value {
            "default" => Ok(Self::Default),
            "none" => Ok(Self::None),
            _ => Err(format!("invalid ui.favicon: {value:?}")),
        }
    }
}

/// The API of the cluster, as the environment variables that the file names give it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KubernetesApi {
    pub host: String,
    pub port: u16,
}

/// Where the service catalogue comes from. Every source is off unless enabled.
#[derive(Clone, Debug)]
pub struct DiscoveryConfig {
    pub interval: Duration,
    pub probe_timeout: Duration,
    /// Read IngressRoute annotations through the in-cluster API (ServiceAccount token).
    pub kubernetes: Option<KubernetesApi>,
    /// Read container labels from this Docker socket (Compose).
    pub docker_socket: Option<PathBuf>,
}

#[derive(Clone, Debug)]
pub struct ServerConfig {
    pub listen_address: SocketAddr,
    /// The name of this instance. The phones show it as the name of the server.
    pub name: String,
    pub workers: usize,
    pub max_body_bytes: usize,
    pub max_connections: usize,
    pub max_headers: usize,
    pub max_header_bytes: usize,
    pub header_read_timeout: Duration,
    pub body_read_timeout: Duration,
    /// The largest number of sign-in bodies that the server reads at the same time.
    pub max_login_body_reads: usize,
    /// The proxies whose `X-Forwarded-For` the server reads.
    pub trusted_proxies: Vec<crate::transport::Cidr>,
}

pub struct SessionConfig {
    /// Cookie name when the browser reached the portal over HTTPS (`__Host-` prefix).
    /// Over plain HTTP, the same name without the prefix is used.
    /// Browsers refuse `__Host-` and `Secure` cookies on HTTP.
    pub cookie_name: String,
    pub ttl: Duration,
    /// A session unused for this long ends, even before `ttl`.
    pub idle_timeout: Duration,
    /// The remembered sessions. `None` when `session.remember` is `false`: the form has no box.
    pub remember: Option<Remember>,
    /// What a session cookie is bound to. A stolen cookie is then useless from another machine.
    pub bind: SessionBind,
    pub same_site: SameSite,
    pub max_active_sessions: usize,
    /// The largest number of live sessions of one principal. An impersonated session does not count.
    pub max_sessions_per_account: usize,
}

/// The rule of a remembered session, the one that a person asks with "Remember me".
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Remember {
    /// The lifetime from the sign-in. The persistent cookie has the same life.
    pub ttl: Duration,
    /// A remembered session unused for this long ends, even before `ttl`.
    pub idle_timeout: Duration,
}

/// The policy of the second factor (`authentication.mfa`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MfaPolicy {
    /// The portal never asks for a code and shows no page of the second factor.
    Off,
    /// Each account chooses to turn the second factor on.
    Optional,
    /// Each account must have the second factor. The sign-in of an account without it ends in the page of the
    /// enrolment.
    Required,
}

impl MfaPolicy {
    pub fn is_on(self) -> bool {
        self != Self::Off
    }
}

/// The block `authentication`, without the secrets.
#[derive(Clone, Debug)]
pub struct AuthenticationConfig {
    pub mfa: MfaPolicy,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionBind {
    /// The client address and the User-Agent at login.
    IpUserAgent,
    UserAgent,
    None,
}

#[derive(Clone, Copy, Debug)]
pub enum SameSite {
    Strict,
    Lax,
}

impl SameSite {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Strict => "Strict",
            Self::Lax => "Lax",
        }
    }
}

/// The limiter values apply only when their limiter is on. When it is off, they hold the default.
#[derive(Clone, Debug)]
pub struct SecurityConfig {
    pub max_password_bytes: usize,
    pub max_forwarded_uri_bytes: usize,
    pub max_cookie_header_bytes: usize,
    pub login_rate_limit_enabled: bool,
    pub login_attempts_per_ip: usize,
    pub login_window: Duration,
    pub login_lockout: Duration,
    pub global_login_rate_limit_enabled: bool,
    pub global_login_attempts: usize,
    pub global_login_window: Duration,
    pub global_login_lockout: Duration,
    pub max_rate_limit_entries: usize,
    pub max_concurrent_password_checks: usize,
}

#[derive(Clone, Debug)]
pub struct MetricsConfig {
    pub enabled: bool,
    pub path: String,
}

#[derive(Clone, Debug)]
pub struct LoggingConfig {
    pub level: LogLevel,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum LogLevel {
    Error = 0,
    Warn = 1,
    Info = 2,
    Debug = 3,
}

impl LogLevel {
    /// The exact names of the table: `error`, `warn`, `info` and `debug`.
    pub fn parse(value: &str) -> Result<Self, String> {
        match value {
            "error" => Ok(Self::Error),
            "warn" => Ok(Self::Warn),
            "info" => Ok(Self::Info),
            "debug" => Ok(Self::Debug),
            _ => Err(format!("invalid logging.level: {value:?}")),
        }
    }
}

impl Config {
    /// What the file turns on: the groups of the inbox and the kinds of key. The document of the
    /// service `portal` follows it.
    pub fn portal_features(&self) -> crate::portal_actions::Features {
        crate::portal_actions::Features::new(self.inbox.as_ref(), self.keys.as_ref())
    }

    /// Read the file. The environment variables that the file names come from the process.
    pub fn load(path: &Path) -> Result<(Self, Secrets), String> {
        let raw = fs::read_to_string(path)
            .map_err(|e| format!("cannot read config {}: {e}", path.display()))?;
        Self::parse(&raw, &|name| std::env::var(name).ok())
    }

    /// Parse the text of a file. `env` reads one environment variable: the program calls it only
    /// for the variables that the text names.
    pub fn parse(raw: &str, env: Env<'_>) -> Result<(Self, Secrets), String> {
        let document = hcl::parse(
            raw,
            &keys::block_names(),
            &keys::labeled_block_names(),
            keys::NESTED_BLOCKS,
            keys::NESTED_PLAIN_BLOCKS,
        )?;
        let mut resolved = resolve::resolve(&document, env)?;
        build(&mut resolved, env)
    }

    /// The effective configuration: one line for each key that applies, with its source.
    pub fn render_effective(&self) -> String {
        let mut out = String::new();
        for line in &self.effective {
            out.push_str(&format!(
                "{} = {}  # {}\n",
                line.key, line.value, line.source
            ));
        }
        out
    }
}

fn build(r: &mut Resolved, env: Env<'_>) -> Result<(Config, Secrets), String> {
    // Check the contradictions between two keys.
    let ttl = r.duration("session", "ttl")?;
    let idle_timeout = r.duration("session", "idle_timeout")?;
    if idle_timeout > ttl {
        return Err(format!(
            "session.idle_timeout must not be longer than session.ttl: idle_timeout is {}, ttl is {}",
            keys::show_duration(idle_timeout),
            keys::show_duration(ttl)
        ));
    }
    let remember = if r.bool("session", "remember")? {
        let remember_ttl = r.duration("session", "remember_ttl")?;
        let remember_idle = r.duration("session", "remember_idle_timeout")?;
        if remember_ttl < ttl {
            return Err(format!(
                "session.remember_ttl must not be shorter than session.ttl: remember_ttl is {}, ttl is {}",
                keys::show_duration(remember_ttl),
                keys::show_duration(ttl)
            ));
        }
        if remember_idle > remember_ttl {
            return Err(format!(
                "session.remember_idle_timeout must not be longer than session.remember_ttl: remember_idle_timeout is {}, remember_ttl is {}",
                keys::show_duration(remember_idle),
                keys::show_duration(remember_ttl)
            ));
        }
        if remember_idle < idle_timeout {
            return Err(format!(
                "session.remember_idle_timeout must not be shorter than session.idle_timeout: remember_idle_timeout is {}, idle_timeout is {}",
                keys::show_duration(remember_idle),
                keys::show_duration(idle_timeout)
            ));
        }
        Some(Remember {
            ttl: remember_ttl,
            idle_timeout: remember_idle,
        })
    } else {
        None
    };
    let max_active_sessions = r.count("session", "max_active_sessions")?;
    let max_sessions_per_account = r.count("session", "max_sessions_per_account")?;
    if max_sessions_per_account > max_active_sessions {
        return Err(format!(
            "session.max_sessions_per_account must not be larger than session.max_active_sessions: max_sessions_per_account is {max_sessions_per_account}, max_active_sessions is {max_active_sessions}"
        ));
    }
    let max_connections = r.count("server", "max_connections")?;
    let max_login_body_reads = r.count("server", "max_login_body_reads")?;
    if max_login_body_reads >= max_connections {
        return Err(format!(
            "server.max_login_body_reads must be less than server.max_connections: max_login_body_reads is {max_login_body_reads}, max_connections is {max_connections}"
        ));
    }
    let postgres = r.has_block("postgres");
    if postgres && r.text("session", "secret_mode")? != "provided" {
        return Err(
            "session.secret_mode must be \"provided\" when the postgres block is present: every \
             instance has to sign with the same key"
                .into(),
        );
    }

    // Without the block, each key of the block gives its default (portal 10.1a).
    let key_limits = KeysConfig {
        default_lifetime: r.duration("keys", "default_lifetime")?,
        directory_max_age: r.duration("keys", "directory_max_age")?,
        max_per_user: r.count("keys", "max_per_user")?,
        max_counter_rows: r.count("keys", "max_counter_rows")?,
        max_directory_grants: r.count("keys", "max_directory_grants")?,
        history_days: u32::try_from(r.int("keys", "history_days")?)
            .map_err(|_| "internal error: keys.history_days is out of range".to_string())?,
        mcp: r.bool("keys", "mcp")?,
        api: r.bool("keys", "api")?,
    };
    let keys = if r.has_block("keys") {
        // The five contradictions of the switches (portal 10.2e): each error names the key, the
        // expected value and the value found.
        if !key_limits.mcp && !key_limits.api {
            return Err("a block keys with both kinds off does nothing: keys.mcp is false and keys.api is false; expected one of them to be true, or no block keys".into());
        }
        if key_limits.mcp {
            for block in ["identity", "pdp"] {
                if !r.has_block(block) {
                    return Err(format!(
                        "keys.mcp is true and the block {block} is absent: the kind mcp needs the blocks identity and pdp; expected keys.mcp = false, or the block {block}"
                    ));
                }
            }
        }
        if key_limits.api && !r.has_block("inbox") {
            return Err("keys.api is true and the block inbox is absent: the kind api is verified by the API of the inbox; expected keys.api = false, or the block inbox".into());
        }
        Some(key_limits.clone())
    } else {
        None
    };
    let allow_creation = r.bool("users", "allow_creation")?;
    if allow_creation && r.text("access", "grants_mode")? == "static" {
        return Err("users.allow_creation is true and access.grants_mode is \"static\": in this mode the database holds no account; expected users.allow_creation = false, or another value of access.grants_mode".into());
    }
    let mut secrets = build_secrets(r)?;
    let access = build_access(r, &secrets.superadmin_login, keys.as_ref())?;
    secrets.pdp_tokens = build_pdp_tokens(r)?;
    let pdp = if r.has_block("pdp") {
        Some(PdpConfig {
            listen_address: r
                .text("pdp", "listen_address")?
                .parse::<SocketAddr>()
                .map_err(|e| format!("internal error: pdp.listen_address: {e}"))?,
            url: r.text("pdp", "url")?.trim_end_matches('/').to_string(),
            max_body_bytes: r.size("pdp", "max_body_bytes")?,
            max_evaluations: r.count("pdp", "max_evaluations")?,
            plain_http: r.bool("pdp", "plain_http")?,
            counter_connections: r.count("pdp", "counter_connections")?,
        })
    } else {
        if !secrets.pdp_tokens.is_empty() {
            return Err(
                "a pdp_client block needs the pdp block: without it, the PDP listener is off"
                    .into(),
            );
        }
        None
    };
    let identity = if r.has_block("identity") {
        Some(IdentityConfig {
            header: r.text("identity", "header")?.to_ascii_lowercase(),
            issuer: r.text("identity", "issuer")?,
            token_ttl: r.duration("identity", "token_ttl")?,
        })
    } else {
        None
    };

    let storage_backend = if postgres {
        StorageBackend::Postgres(PostgresConfig {
            host: r.text("postgres", "host")?,
            port: u16::try_from(r.int("postgres", "port")?)
                .map_err(|_| "internal error: postgres.port is out of range".to_string())?,
            database: r.text("postgres", "database")?,
            user: r.text("postgres", "user")?,
            pool_size: r.count("postgres", "pool_size")?,
            connect_timeout: r.duration("postgres", "connect_timeout")?,
        })
    } else {
        StorageBackend::Sqlite(PathBuf::from(r.text("storage", "database")?))
    };

    let inbox_built = inbox::build(r, keys.as_ref().is_some_and(|k| k.api))?;
    let mut pdp_approvals = std::collections::BTreeSet::new();
    for block in r.labeled.iter().filter(|b| b.kind == "pdp_client") {
        if block.bool("approvals")? {
            pdp_approvals.insert(block.label.clone());
        }
    }

    let kubernetes = if r.bool("discovery", "kubernetes")? {
        Some(kubernetes_api(r, env)?)
    } else {
        None
    };

    let config = Config {
        server: ServerConfig {
            listen_address: r
                .text("server", "listen_address")?
                .parse::<SocketAddr>()
                .map_err(|e| format!("internal error: server.listen_address: {e}"))?,
            name: r.text("server", "name")?,
            workers: r.count("server", "workers")?,
            max_body_bytes: r.size("server", "max_body_bytes")?,
            max_connections: r.count("server", "max_connections")?,
            max_headers: r.count("server", "max_headers")?,
            max_header_bytes: r.size("server", "max_header_bytes")?,
            header_read_timeout: r.duration("server", "header_read_timeout")?,
            body_read_timeout: r.duration("server", "body_read_timeout")?,
            max_login_body_reads,
            trusted_proxies: r
                .list("server", "trusted_proxies")?
                .iter()
                .map(|net| crate::transport::Cidr::parse(net))
                .collect::<Result<_, _>>()
                .map_err(|e| format!("internal error: server.trusted_proxies: {e}"))?,
        },
        authentication: AuthenticationConfig {
            mfa: match r.text("authentication", "mfa")?.as_str() {
                "off" => MfaPolicy::Off,
                "optional" => MfaPolicy::Optional,
                _ => MfaPolicy::Required,
            },
        },
        session: SessionConfig {
            cookie_name: r.text("session", "cookie_name")?,
            ttl,
            idle_timeout,
            remember,
            bind: match r.text("session", "bind")?.as_str() {
                "ip_user_agent" => SessionBind::IpUserAgent,
                "user_agent" => SessionBind::UserAgent,
                _ => SessionBind::None,
            },
            same_site: match r.text("session", "same_site")?.as_str() {
                "Strict" => SameSite::Strict,
                _ => SameSite::Lax,
            },
            max_active_sessions,
            max_sessions_per_account,
        },
        security: SecurityConfig {
            max_password_bytes: r.count("security", "max_password_bytes")?,
            max_forwarded_uri_bytes: r.count("security", "max_forwarded_uri_bytes")?,
            max_cookie_header_bytes: r.count("security", "max_cookie_header_bytes")?,
            login_rate_limit_enabled: r.bool("security", "login_rate_limit_enabled")?,
            login_attempts_per_ip: r.count("security", "login_attempts_per_ip")?,
            login_window: r.duration("security", "login_window")?,
            login_lockout: r.duration("security", "login_lockout")?,
            global_login_rate_limit_enabled: r
                .bool("security", "global_login_rate_limit_enabled")?,
            global_login_attempts: r.count("security", "global_login_attempts")?,
            global_login_window: r.duration("security", "global_login_window")?,
            global_login_lockout: r.duration("security", "global_login_lockout")?,
            max_rate_limit_entries: r.count("security", "max_rate_limit_entries")?,
            max_concurrent_password_checks: r
                .count("security", "max_concurrent_password_checks")?,
        },
        metrics: MetricsConfig {
            enabled: r.bool("metrics", "enabled")?,
            path: r.text("metrics", "path")?,
        },
        discovery: DiscoveryConfig {
            interval: r.duration("discovery", "interval")?,
            probe_timeout: r.duration("discovery", "probe_timeout")?,
            kubernetes,
            docker_socket: r
                .optional_text("discovery", "docker_socket")?
                .map(PathBuf::from),
        },
        access,
        identity,
        pdp,
        keys,
        key_limits,
        users: UsersConfig {
            allow_creation,
            max_users: r.count("users", "max_users")?,
            max_grants: r.count("users", "max_grants")?,
        },
        storage: StorageConfig {
            backend: storage_backend,
            retention_days: u32::try_from(r.int("storage", "retention_days")?).map_err(|_| {
                "internal error: storage.retention_days is out of range".to_string()
            })?,
        },
        ui: UiConfig {
            icons_dir: PathBuf::from(r.text("ui", "icons_dir")?),
            favicon: Favicon::parse(&r.text("ui", "favicon")?)?,
            services_new_tab: r.bool("ui", "services_new_tab")?,
        },
        logging: LoggingConfig {
            level: LogLevel::parse(&r.text("logging", "level")?)?,
        },
        inbox: None,
        pdp_approvals,
        effective: std::mem::take(&mut r.effective),
    };
    let mut config = config;
    if let Some((inbox, inbox_secrets)) = inbox_built {
        inbox::check_against(&config, &inbox, postgres)?;
        config.inbox = Some(inbox);
        secrets.inbox = Some(inbox_secrets);
    }
    check_approvals(&config)?;
    // The grants of the accounts of the file on the service portal follow the document of the
    // features that the file turns on.
    let features = config.portal_features();
    for (login, account) in &config.access.accounts {
        crate::portal_actions::check_user_grants(&features, &account.grants)
            .map_err(|e| format!("user {login:?}: {e}"))?;
    }
    if let Some(mut inbox) = config.inbox.take() {
        inbox::check_api_keys(&config, &mut inbox, &features)?;
        config.inbox = Some(inbox);
    }
    if let Some(pdp) = &config.pdp {
        // The two values must agree: an `http` URL needs `plain_http`, and `plain_http` needs an
        // `http` URL (portal 10.2b).
        let http = pdp.url.starts_with("http://");
        if http && !pdp.plain_http {
            return Err(format!(
                "pdp.url is an http URL and pdp.plain_http is false: set pdp.plain_http to true, or use an https URL (pdp.url is {:?})",
                pdp.url
            ));
        }
        if !http && pdp.plain_http {
            return Err(format!(
                "pdp.plain_http is true and pdp.url is not an http URL: the two values contradict (pdp.url is {:?})",
                pdp.url
            ));
        }
        let (a, b) = (config.server.listen_address, pdp.listen_address);
        let overlap = a.port() == b.port()
            && (a.ip() == b.ip() || a.ip().is_unspecified() || b.ip().is_unspecified());
        if overlap {
            return Err(format!(
                "pdp.listen_address must not use the port of server.listen_address: server.listen_address is {a}, pdp.listen_address is {b}"
            ));
        }
    }
    if config.access.actions_max_age <= config.access.actions_refresh {
        return Err(format!(
            "access.actions_max_age must be more than access.actions_refresh: actions_max_age is {}, actions_refresh is {}",
            keys::show_duration(config.access.actions_max_age),
            keys::show_duration(config.access.actions_refresh)
        ));
    }
    if secrets.superadmin_password.len() > config.security.max_password_bytes {
        return Err(format!(
            "authentication.superadmin_password: the value has {} bytes, security.max_password_bytes is {}",
            secrets.superadmin_password.len(),
            config.security.max_password_bytes
        ));
    }
    Ok((config, secrets))
}

/// The source of a key in the effective configuration, for an error message.
fn source_of(r: &Resolved, key: &str) -> String {
    r.effective
        .iter()
        .find(|line| line.key == key)
        .map(|line| line.source.clone())
        .unwrap_or_default()
}

/// The superadmin passwords that the repository publishes as examples: `.env.example` of the
/// repository root, `.env.example` and the quick start of this README. Anybody who reads the
/// repository knows them.
const EXAMPLE_PASSWORDS: [&str; 2] = ["change-me-use-a-long-random-secret", "test1234"];
/// The session secret that `.env.example` of the repository root publishes as an example.
const EXAMPLE_SESSION_SECRETS: [&str; 1] = ["change-me-with-a-long-random-value"];

/// Tells whether `value` is one of the published example values. The error never shows the value.
fn is_example(value: &Secret, examples: &[&str]) -> bool {
    examples
        .iter()
        .any(|example| value.as_bytes() == example.as_bytes())
}

fn build_secrets(r: &mut Resolved) -> Result<Secrets, String> {
    let missing = |key: &str| format!("internal error: the secret {key} is missing");
    let login_secret = r
        .take_secret("authentication", "superadmin_login")
        .ok_or_else(|| missing("authentication.superadmin_login"))?;
    let superadmin_login = check_login(login_secret.as_bytes()).map_err(|reason| {
        format!(
            "authentication.superadmin_login: {reason} ({}; the value is hidden)",
            source_of(r, "authentication.superadmin_login")
        )
    })?;
    drop(login_secret);

    let superadmin_password = r
        .take_secret("authentication", "superadmin_password")
        .ok_or_else(|| missing("authentication.superadmin_password"))?;
    if is_example(&superadmin_password, &EXAMPLE_PASSWORDS) {
        return Err(format!(
            "authentication.superadmin_password: the value is an example value that the repository publishes; give a new random value ({}; the value is hidden)",
            source_of(r, "authentication.superadmin_password")
        ));
    }

    let totp_key = r.take_secret("authentication", "totp_key");
    if let Some(key) = &totp_key {
        crate::secretbox::SecretBox::parse_key(key.as_bytes()).map_err(|reason| {
            format!(
                "authentication.totp_key: {reason} ({}; the value is hidden)",
                source_of(r, "authentication.totp_key")
            )
        })?;
    }

    let session_secret = r.take_secret("session", "secret");
    if let Some(secret) = &session_secret {
        if secret.len() < 32 {
            return Err(format!(
                "session.secret: the value has {} bytes, the minimum is 32 ({})",
                secret.len(),
                source_of(r, "session.secret")
            ));
        }
        if is_example(secret, &EXAMPLE_SESSION_SECRETS) {
            return Err(format!(
                "session.secret: the value is an example value that the repository publishes; give a new random value ({}; the value is hidden)",
                source_of(r, "session.secret")
            ));
        }
    }

    let postgres_password = r.take_secret("postgres", "password");
    if let Some(secret) = &postgres_password {
        if std::str::from_utf8(secret.as_bytes()).is_err() {
            return Err(format!(
                "postgres.password: the value is not valid UTF-8 ({})",
                source_of(r, "postgres.password")
            ));
        }
    }

    let identity_key = r.take_secret("identity", "signing_key");

    Ok(Secrets {
        superadmin_login,
        superadmin_password,
        totp_key,
        session_secret,
        postgres_password,
        identity_key,
        pdp_tokens: Vec::new(),
        inbox: None,
    })
}

/// The `access` block and the `user` blocks.
fn build_access(
    r: &Resolved,
    superadmin_login: &str,
    keys: Option<&KeysConfig>,
) -> Result<AccessConfig, String> {
    let grants_mode = Mode::parse(&r.text("access", "grants_mode")?)?;
    let rules = accounts::Rules {
        mode: grants_mode,
        superadmin_login: superadmin_login.to_string(),
        keys_enabled: keys.is_some_and(|k| k.mcp),
        max_grants: r.count("users", "max_grants")?,
        max_keys_per_user: match keys {
            Some(keys) => keys.max_per_user,
            None => 0,
        },
    };
    let accounts = accounts::build(r, &rules)?;
    let configured_grants = accounts
        .iter()
        .map(|(login, account)| (login.clone(), account.grants.clone()))
        .collect();
    Ok(AccessConfig {
        grants_mode,
        configured_grants,
        accounts,
        actions_timeout: r.duration("access", "actions_timeout")?,
        actions_refresh: r.duration("access", "actions_refresh")?,
        actions_max_age: r.duration("access", "actions_max_age")?,
        start_read_deadline: r.duration("access", "start_read_deadline")?,
        bounds: crate::actions::Bounds {
            max_actions: r.count("access", "max_actions")?,
            max_resources: r.count("access", "max_resources")?,
            max_document_bytes: r.count("access", "max_document_bytes")?,
        },
    })
}

/// The keys that the approvals need: the block `inbox.approvals` needs the PDP and the keys of the kind `mcp`, and a
/// server with `approvals = true` needs the block.
fn check_approvals(config: &Config) -> Result<(), String> {
    let has_block = config
        .inbox
        .as_ref()
        .is_some_and(|inbox| inbox.approvals.is_some());
    if let Some(name) = config.pdp_approvals.iter().next() {
        if !has_block {
            return Err(format!(
                "pdp_client.{name}.approvals is true and the block inbox.approvals is absent: expected approvals = false, or the block inbox.approvals"
            ));
        }
    }
    if has_block {
        if config.pdp.is_none() {
            return Err("the block inbox.approvals needs the block pdp: the PDP gates the actions; add the block pdp, or remove the block inbox.approvals".into());
        }
        if !config.keys.as_ref().is_some_and(|keys| keys.mcp) {
            return Err("the block inbox.approvals needs keys.mcp = true: an agent holds a key of the kind mcp; set keys.mcp = true, or remove the block inbox.approvals".into());
        }
    }
    Ok(())
}

/// The token of each `pdp_client` block. A short token, or a token that another `pdp_client` block has, stops the start.
fn build_pdp_tokens(r: &mut Resolved) -> Result<Vec<(String, Secret)>, String> {
    let mut tokens: Vec<(String, Secret)> = Vec::new();
    for block in r.labeled.iter_mut().filter(|b| b.kind == "pdp_client") {
        let server = block.label.clone();
        crate::grants::check_service_id(&server)
            .map_err(|e| format!("pdp_client {server:?}: {e}"))?;
        let token = block
            .take_secret("token")
            .ok_or_else(|| format!("internal error: pdp_client {server:?} has no token"))?;
        if token.len() < 32 {
            return Err(format!(
                "pdp_client.{server}.token: the value has {} bytes, the minimum is 32",
                token.len()
            ));
        }
        if let Some((other, _)) = tokens
            .iter()
            .find(|(_, known)| crate::util::constant_time_eq(known.as_bytes(), token.as_bytes()))
        {
            return Err(format!(
                "pdp_client {server:?} and pdp_client {other:?} have the same token: give each server its own token"
            ));
        }
        tokens.push((server, token));
    }
    Ok(tokens)
}

/// The login is used as it is. The program does not trim it and does not change its case.
/// A login that the form could not match is refused. No value changes silently.
fn check_login(bytes: &[u8]) -> Result<String, String> {
    let text =
        std::str::from_utf8(bytes).map_err(|_| "the value is not valid UTF-8".to_string())?;
    if text.chars().count() > 64 {
        return Err("expected 1 to 64 characters".into());
    }
    if text.chars().any(char::is_uppercase) {
        return Err(
            "the login must not have an upper-case letter: write it in lower case, because the sign-in form compares in lower case"
                .into(),
        );
    }
    if text.chars().any(char::is_whitespace) {
        return Err("the login must not have a space, a tab or a line end".into());
    }
    if text.chars().any(char::is_control) {
        return Err("the login must not have a control character".into());
    }
    if text.contains(crate::subject_id::SEPARATOR) {
        return Err(format!(
            "the login must not have the character {:?}: the program keeps it for the ids of keys",
            crate::subject_id::SEPARATOR
        ));
    }
    Ok(text.to_string())
}

/// The API of the cluster comes from the two environment variables that the file names.
fn kubernetes_api(r: &mut Resolved, env: Env<'_>) -> Result<KubernetesApi, String> {
    let host_variable = r.text("discovery", "kubernetes_host_env")?;
    let port_variable = r.text("discovery", "kubernetes_port_env")?;
    let read = |attribute: &str, variable: &str| {
        let value = env(variable).ok_or_else(|| {
            format!("discovery.{attribute}: the environment variable {variable} is not set, or its value is not valid UTF-8")
        })?;
        if value.is_empty() {
            return Err(format!(
                "discovery.{attribute}: the environment variable {variable} is empty"
            ));
        }
        Ok(value)
    };
    let host = read("kubernetes_host_env", &host_variable)?;
    if !host
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_' | b':'))
    {
        return Err(format!(
            "discovery.kubernetes_host_env: the variable {host_variable} must hold a host name or an address, got {host:?}"
        ));
    }
    let port_text = read("kubernetes_port_env", &port_variable)?;
    let port = port_text
        .parse::<u16>()
        .ok()
        .filter(|p| *p != 0 && port_text.bytes().all(|b| b.is_ascii_digit()))
        .ok_or_else(|| {
            format!(
                "discovery.kubernetes_port_env: the variable {port_variable} must hold a port number from 1 to 65535, got {port_text:?}"
            )
        })?;
    let at = r
        .effective
        .iter()
        .position(|line| line.key == "discovery.kubernetes_port_env")
        .map_or(r.effective.len(), |i| i + 1);
    r.effective.insert(
        at,
        Effective {
            key: "discovery.kubernetes_port".into(),
            value: port.to_string(),
            source: format!("env:{port_variable}"),
        },
    );
    let at = r
        .effective
        .iter()
        .position(|line| line.key == "discovery.kubernetes_host_env")
        .map_or(r.effective.len(), |i| i + 1);
    r.effective.insert(
        at,
        Effective {
            key: "discovery.kubernetes_host".into(),
            value: format!("{host:?}"),
            source: format!("env:{host_variable}"),
        },
    );
    let host = if host.contains(':') {
        format!("[{host}]")
    } else {
        host
    };
    Ok(KubernetesApi { host, port })
}

#[cfg(test)]
mod tests;
