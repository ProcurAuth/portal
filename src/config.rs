use std::collections::{HashMap, HashSet};
use std::fs;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::time::Duration;

pub struct Config {
    pub server: ServerConfig,
    pub authentication: AuthenticationConfig,
    pub session: SessionConfig,
    pub security: SecurityConfig,
    pub metrics: MetricsConfig,
    pub discovery: DiscoveryConfig,
    pub roles: RolesConfig,
    pub users: UsersConfig,
    pub storage: StorageConfig,
    pub ui: UiConfig,
    pub logging: LoggingConfig,
}

/// The roles that exist. Services bind to them with `portal.roles`; a role a service names that is
/// not listed here grants access to nobody but the superadmin.
#[derive(Clone, Debug, Default)]
pub struct RolesConfig {
    pub names: Vec<String>,
}

#[derive(Clone, Debug)]
pub struct UsersConfig {
    pub max_users: usize,
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
    /// A shared PostgreSQL database (the `postgres` block): any number of instances are active at
    /// once, and the sessions are in the database too.
    Postgres(PostgresConfig),
}

#[derive(Clone, Debug)]
pub struct PostgresConfig {
    pub host: String,
    pub port: u16,
    pub database: String,
    pub user: String,
    pub password_file: PathBuf,
    /// Connections kept to the database.
    pub pool_size: usize,
    pub connect_timeout: Duration,
}

#[derive(Clone, Debug)]
pub struct UiConfig {
    /// Directory of the offline icon library (`di/`, `si/`, `tb/`). Missing: icons fall back to initials.
    pub icons_dir: PathBuf,
}

/// Where the service catalogue comes from. Every source is off unless enabled.
#[derive(Clone, Debug)]
pub struct DiscoveryConfig {
    pub interval: Duration,
    pub probe_timeout: Duration,
    /// Read IngressRoute annotations through the in-cluster API (ServiceAccount token).
    pub kubernetes: bool,
    /// Read container labels from this Docker socket (Compose).
    pub docker_socket: Option<PathBuf>,
}

#[derive(Clone, Debug)]
pub struct ServerConfig {
    pub listen_address: SocketAddr,
    pub workers: usize,
    pub max_body_bytes: usize,
    pub max_connections: usize,
    pub max_headers: usize,
    pub max_header_bytes: usize,
    pub header_read_timeout: Duration,
    pub body_read_timeout: Duration,
}

#[derive(Clone, Debug)]
pub struct AuthenticationConfig {
    /// Login and password of the superadmin (the account that is not in the database).
    pub superadmin_login_file: PathBuf,
    pub superadmin_password_file: PathBuf,
}

pub struct SessionConfig {
    /// Cookie name when the browser reached the portal over HTTPS (`__Host-` prefix). Over plain
    /// HTTP the same name without the prefix is used: browsers refuse `__Host-` and `Secure`
    /// cookies on HTTP.
    pub cookie_name: String,
    pub ttl: Duration,
    /// A session unused for this long ends, even before `ttl`.
    pub idle_timeout: Duration,
    /// What a session cookie is tied to, so a stolen cookie is useless from another machine.
    pub bind: SessionBind,
    pub same_site: SameSite,
    pub secret: SessionSecretConfig,
    pub max_active_sessions: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionBind {
    /// Client address and User-Agent at login.
    IpUserAgent,
    UserAgent,
    None,
}

pub enum SessionSecretConfig {
    Ephemeral,
    File(PathBuf),
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
    pub fn parse(value: &str) -> Result<Self, String> {
        match value.to_ascii_lowercase().as_str() {
            "error" => Ok(Self::Error),
            "warn" | "warning" => Ok(Self::Warn),
            "info" => Ok(Self::Info),
            "debug" => Ok(Self::Debug),
            _ => Err(format!("invalid logging.level: {value}")),
        }
    }
}

impl Config {
    pub fn load(path: &Path) -> Result<Self, String> {
        let raw = fs::read_to_string(path)
            .map_err(|e| format!("cannot read config {}: {e}", path.display()))?;
        Self::parse(&raw)
    }

    pub fn parse(raw: &str) -> Result<Self, String> {
        let values = parse_hcl_subset(raw)?;
        validate_known_keys(&values)?;

        let listen_address = get_required(&values, "server", "listen_address")?
            .parse::<SocketAddr>()
            .map_err(|e| format!("invalid server.listen_address: {e}"))?;
        let workers = parse_usize(get_or(&values, "server", "workers", "4")?, "server.workers")?;
        let max_body_bytes = parse_size(get_or(&values, "server", "max_body_bytes", "8KiB")?)?;
        let max_connections = parse_usize(
            get_or(&values, "server", "max_connections", "1024")?,
            "server.max_connections",
        )?;
        let max_headers = parse_usize(
            get_or(&values, "server", "max_headers", "64")?,
            "server.max_headers",
        )?;
        let max_header_bytes = parse_size(get_or(
            &values,
            "server",
            "max_header_bytes",
            "32KiB",
        )?)?;
        let header_read_timeout = parse_duration(get_or(
            &values,
            "server",
            "header_read_timeout",
            "10s",
        )?)?;
        let body_read_timeout = parse_duration(get_or(
            &values,
            "server",
            "body_read_timeout",
            "10s",
        )?)?;

        let superadmin_login_file = PathBuf::from(get_required(
            &values,
            "authentication",
            "superadmin_login_file",
        )?);
        validate_secret_path(&superadmin_login_file, "authentication.superadmin_login_file")?;
        let superadmin_password_file = PathBuf::from(get_required(
            &values,
            "authentication",
            "superadmin_password_file",
        )?);
        validate_secret_path(
            &superadmin_password_file,
            "authentication.superadmin_password_file",
        )?;

        let cookie_name = get_or(
            &values,
            "session",
            "cookie_name",
            "__Host-portal",
        )?
        .to_string();
        validate_cookie_name(&cookie_name)?;
        if !cookie_name.starts_with("__Host-") {
            return Err("session.cookie_name must use the __Host- prefix".into());
        }
        let ttl = parse_duration(get_or(&values, "session", "ttl", "8h")?)?;
        if ttl < Duration::from_secs(60) || ttl > Duration::from_secs(7 * 24 * 3600) {
            return Err("session.ttl must be between 1m and 7d".into());
        }
        let idle_timeout = parse_duration(get_or(&values, "session", "idle_timeout", "30m")?)?;
        if idle_timeout < Duration::from_secs(60) || idle_timeout > ttl {
            return Err("session.idle_timeout must be between 1m and session.ttl".into());
        }
        let bind = match get_or(&values, "session", "bind", "ip_user_agent")?
            .to_ascii_lowercase()
            .as_str()
        {
            "ip_user_agent" => SessionBind::IpUserAgent,
            "user_agent" => SessionBind::UserAgent,
            "none" => SessionBind::None,
            _ => return Err("session.bind must be ip_user_agent, user_agent or none".into()),
        };
        let same_site = match get_or(&values, "session", "same_site", "Strict")?
            .to_ascii_lowercase()
            .as_str()
        {
            "strict" => SameSite::Strict,
            "lax" => SameSite::Lax,
            _ => return Err("session.same_site must be Strict or Lax".into()),
        };
        let max_active_sessions = parse_usize(
            get_or(&values, "session", "max_active_sessions", "4096")?,
            "session.max_active_sessions",
        )?;

        let secret_mode = get_or(&values, "session", "secret_mode", "ephemeral")?
            .to_ascii_lowercase();
        let configured_secret_file = values
            .get(&("session".to_string(), "secret_file".to_string()))
            .map(String::as_str);
        let session_secret = match secret_mode.as_str() {
            "ephemeral" => {
                if configured_secret_file.is_some() {
                    return Err(
                        "session.secret_file must not be set when session.secret_mode is ephemeral"
                            .into(),
                    );
                }
                SessionSecretConfig::Ephemeral
            }
            "file" => {
                let path = PathBuf::from(configured_secret_file.ok_or_else(|| {
                    "session.secret_file is required when session.secret_mode is file".to_string()
                })?);
                validate_secret_path(&path, "session.secret_file")?;
                SessionSecretConfig::File(path)
            }
            _ => return Err("session.secret_mode must be ephemeral or file".into()),
        };

        let max_password_bytes = parse_usize(
            get_or(&values, "security", "max_password_bytes", "1024")?,
            "security.max_password_bytes",
        )?;
        let max_forwarded_uri_bytes = parse_usize(
            get_or(&values, "security", "max_forwarded_uri_bytes", "4096")?,
            "security.max_forwarded_uri_bytes",
        )?;
        let max_cookie_header_bytes = parse_usize(
            get_or(&values, "security", "max_cookie_header_bytes", "8192")?,
            "security.max_cookie_header_bytes",
        )?;
        let login_rate_limit_enabled = parse_bool(
            get_or(&values, "security", "login_rate_limit_enabled", "true")?,
            "security.login_rate_limit_enabled",
        )?;
        let login_attempts_per_ip = parse_usize(
            get_or(&values, "security", "login_attempts_per_ip", "5")?,
            "security.login_attempts_per_ip",
        )?;
        let login_window = parse_duration(get_or(
            &values,
            "security",
            "login_window",
            "1m",
        )?)?;
        let login_lockout = parse_duration(get_or(
            &values,
            "security",
            "login_lockout",
            "5m",
        )?)?;
        let global_login_rate_limit_enabled = parse_bool(
            get_or(&values, "security", "global_login_rate_limit_enabled", "true")?,
            "security.global_login_rate_limit_enabled",
        )?;
        let global_login_attempts = parse_usize(
            get_or(&values, "security", "global_login_attempts", "100")?,
            "security.global_login_attempts",
        )?;
        let global_login_window = parse_duration(get_or(
            &values,
            "security",
            "global_login_window",
            "1m",
        )?)?;
        let global_login_lockout = parse_duration(get_or(
            &values,
            "security",
            "global_login_lockout",
            "30s",
        )?)?;
        let max_rate_limit_entries = parse_usize(
            get_or(&values, "security", "max_rate_limit_entries", "4096")?,
            "security.max_rate_limit_entries",
        )?;
        let max_concurrent_password_checks = parse_usize(
            get_or(
                &values,
                "security",
                "max_concurrent_password_checks",
                "1",
            )?,
            "security.max_concurrent_password_checks",
        )?;

        if workers == 0 || workers > 64 {
            return Err("server.workers must be between 1 and 64".into());
        }
        if max_body_bytes < 1024 || max_body_bytes > 1024 * 1024 {
            return Err("server.max_body_bytes must be between 1KiB and 1MiB".into());
        }
        if max_connections == 0 || max_connections > 100_000 {
            return Err("server.max_connections must be between 1 and 100000".into());
        }
        if max_headers < 8 || max_headers > 256 {
            return Err("server.max_headers must be between 8 and 256".into());
        }
        if !(8 * 1024..=1024 * 1024).contains(&max_header_bytes) {
            return Err("server.max_header_bytes must be between 8KiB and 1MiB".into());
        }
        if header_read_timeout < Duration::from_secs(1)
            || header_read_timeout > Duration::from_secs(60)
        {
            return Err("server.header_read_timeout must be between 1s and 60s".into());
        }
        if body_read_timeout < Duration::from_secs(1)
            || body_read_timeout > Duration::from_secs(60)
        {
            return Err("server.body_read_timeout must be between 1s and 60s".into());
        }
        if max_active_sessions == 0 || max_active_sessions > 1_000_000 {
            return Err("session.max_active_sessions must be between 1 and 1000000".into());
        }
        if max_password_bytes < 8 || max_password_bytes > 16 * 1024 {
            return Err("security.max_password_bytes must be between 8 and 16384".into());
        }
        if max_forwarded_uri_bytes < 128 || max_forwarded_uri_bytes > 64 * 1024 {
            return Err("security.max_forwarded_uri_bytes must be between 128 and 65536".into());
        }
        if max_cookie_header_bytes < 256 || max_cookie_header_bytes > 64 * 1024 {
            return Err("security.max_cookie_header_bytes must be between 256 and 65536".into());
        }
        if login_attempts_per_ip == 0 {
            return Err("security.login_attempts_per_ip must be greater than zero".into());
        }
        if login_window < Duration::from_secs(1) || login_window > Duration::from_secs(24 * 3600) {
            return Err("security.login_window must be between 1s and 24h".into());
        }
        if login_lockout < Duration::from_secs(1) || login_lockout > Duration::from_secs(24 * 3600) {
            return Err("security.login_lockout must be between 1s and 24h".into());
        }
        if global_login_attempts == 0 || global_login_attempts > 1_000_000 {
            return Err("security.global_login_attempts must be between 1 and 1000000".into());
        }
        if global_login_window < Duration::from_secs(1) || global_login_window > Duration::from_secs(24 * 3600) {
            return Err("security.global_login_window must be between 1s and 24h".into());
        }
        if global_login_lockout < Duration::from_secs(1) || global_login_lockout > Duration::from_secs(24 * 3600) {
            return Err("security.global_login_lockout must be between 1s and 24h".into());
        }
        if max_rate_limit_entries < 64 || max_rate_limit_entries > 1_000_000 {
            return Err("security.max_rate_limit_entries must be between 64 and 1000000".into());
        }
        if max_concurrent_password_checks == 0 || max_concurrent_password_checks > 64 {
            return Err("security.max_concurrent_password_checks must be between 1 and 64".into());
        }

        let metrics_enabled = parse_bool(
            get_or(&values, "metrics", "enabled", "true")?,
            "metrics.enabled",
        )?;
        let metrics_path = get_or(&values, "metrics", "path", "/metrics")?.to_string();
        validate_metrics_path(&metrics_path)?;

        let interval = parse_duration(get_or(&values, "discovery", "interval", "15s")?)?;
        if interval < Duration::from_secs(2) || interval > Duration::from_secs(3600) {
            return Err("discovery.interval must be between 2s and 1h".into());
        }
        let probe_timeout = parse_duration(get_or(&values, "discovery", "probe_timeout", "3s")?)?;
        if probe_timeout < Duration::from_millis(200) || probe_timeout > Duration::from_secs(30) {
            return Err("discovery.probe_timeout must be between 200ms and 30s".into());
        }
        let kubernetes = parse_bool(
            get_or(&values, "discovery", "kubernetes", "false")?,
            "discovery.kubernetes",
        )?;
        let docker_socket = match values.get(&("discovery".to_string(), "docker_socket".to_string())) {
            Some(path) if Path::new(path).is_absolute() => Some(PathBuf::from(path)),
            Some(_) => return Err("discovery.docker_socket must be an absolute path".into()),
            None => None,
        };

        let role_names = crate::catalog::parse_roles(get_or(&values, "roles", "names", "")?)
            .map_err(|e| format!("invalid roles.names: {e}"))?;

        let max_users = parse_usize(get_or(&values, "users", "max_users", "64")?, "users.max_users")?;
        if !(1..=1024).contains(&max_users) {
            return Err("users.max_users must be between 1 and 1024".into());
        }

        let retention_days = parse_usize(
            get_or(&values, "storage", "retention_days", "90")?,
            "storage.retention_days",
        )?;
        if !(1..=3650).contains(&retention_days) {
            return Err("storage.retention_days must be between 1 and 3650".into());
        }
        let backend = parse_backend(&values)?;
        if matches!(backend, StorageBackend::Postgres(_))
            && matches!(session_secret, SessionSecretConfig::Ephemeral)
        {
            return Err(
                "session.secret_mode must be file when the postgres block is present: every \
                 instance has to sign with the same key"
                    .into(),
            );
        }

        let icons_dir = PathBuf::from(get_or(&values, "ui", "icons_dir", "/icons")?);
        if !icons_dir.is_absolute() {
            return Err("ui.icons_dir must be an absolute path".into());
        }

        let level = LogLevel::parse(get_or(&values, "logging", "level", "info")?)?;

        Ok(Self {
            server: ServerConfig {
                listen_address,
                workers,
                max_body_bytes,
                max_connections,
                max_headers,
                max_header_bytes,
                header_read_timeout,
                body_read_timeout,
            },
            authentication: AuthenticationConfig {
                superadmin_login_file,
                superadmin_password_file,
            },
            session: SessionConfig {
                cookie_name,
                ttl,
                idle_timeout,
                bind,
                same_site,
                secret: session_secret,
                max_active_sessions,
            },
            security: SecurityConfig {
                max_password_bytes,
                max_forwarded_uri_bytes,
                max_cookie_header_bytes,
                login_rate_limit_enabled,
                login_attempts_per_ip,
                login_window,
                login_lockout,
                global_login_rate_limit_enabled,
                global_login_attempts,
                global_login_window,
                global_login_lockout,
                max_rate_limit_entries,
                max_concurrent_password_checks,
            },
            metrics: MetricsConfig {
                enabled: metrics_enabled,
                path: metrics_path,
            },
            discovery: DiscoveryConfig {
                interval,
                probe_timeout,
                kubernetes,
                docker_socket,
            },
            roles: RolesConfig { names: role_names },
            users: UsersConfig { max_users },
            storage: StorageConfig {
                backend,
                retention_days: retention_days as u32,
            },
            ui: UiConfig { icons_dir },
            logging: LoggingConfig { level },
        })
    }
}

/// SQLite unless the `postgres` block is there; then the file is not configured at all, so a
/// configuration never names two places for the same data.
fn parse_backend(values: &HashMap<(String, String), String>) -> Result<StorageBackend, String> {
    let has_postgres = values.keys().any(|(block, _)| block == "postgres");
    if !has_postgres {
        let database = PathBuf::from(get_or(
            values,
            "storage",
            "database",
            "/var/lib/portal/portal.db",
        )?);
        validate_secret_path(&database, "storage.database")?;
        return Ok(StorageBackend::Sqlite(database));
    }
    if values.contains_key(&("storage".to_string(), "database".to_string())) {
        return Err("storage.database must not be set when the postgres block is present".into());
    }
    let host = get_required(values, "postgres", "host")?.to_string();
    if host.is_empty()
        || host.len() > 253
        || !host
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_' | b':'))
    {
        return Err("postgres.host must be a host name or an address".into());
    }
    let port = parse_usize(get_or(values, "postgres", "port", "5432")?, "postgres.port")?;
    let port = u16::try_from(port)
        .ok()
        .filter(|p| *p != 0)
        .ok_or("postgres.port must be between 1 and 65535")?;
    let database = get_required(values, "postgres", "database")?.to_string();
    let user = get_required(values, "postgres", "user")?.to_string();
    for (name, value) in [("database", &database), ("user", &user)] {
        if value.is_empty()
            || value.len() > 63
            || !value
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_'))
        {
            return Err(format!(
                "postgres.{name} must be 1 to 63 letters, digits, '.', '-' or '_'"
            ));
        }
    }
    let password_file = PathBuf::from(get_required(values, "postgres", "password_file")?);
    validate_secret_path(&password_file, "postgres.password_file")?;
    let pool_size = parse_usize(
        get_or(values, "postgres", "pool_size", "4")?,
        "postgres.pool_size",
    )?;
    if !(1..=64).contains(&pool_size) {
        return Err("postgres.pool_size must be between 1 and 64".into());
    }
    let connect_timeout = parse_duration(get_or(values, "postgres", "connect_timeout", "5s")?)?;
    if !(Duration::from_secs(1)..=Duration::from_secs(60)).contains(&connect_timeout) {
        return Err("postgres.connect_timeout must be between 1s and 60s".into());
    }
    Ok(StorageBackend::Postgres(PostgresConfig {
        host,
        port,
        database,
        user,
        password_file,
        pool_size,
        connect_timeout,
    }))
}

fn validate_known_keys(values: &HashMap<(String, String), String>) -> Result<(), String> {
    let allowed: HashSet<(&str, &str)> = [
        ("server", "listen_address"),
        ("server", "workers"),
        ("server", "max_body_bytes"),
        ("server", "max_connections"),
        ("server", "max_headers"),
        ("server", "max_header_bytes"),
        ("server", "header_read_timeout"),
        ("server", "body_read_timeout"),
        ("authentication", "superadmin_login_file"),
        ("authentication", "superadmin_password_file"),
        ("roles", "names"),
        ("users", "max_users"),
        ("storage", "database"),
        ("storage", "retention_days"),
        ("postgres", "host"),
        ("postgres", "port"),
        ("postgres", "database"),
        ("postgres", "user"),
        ("postgres", "password_file"),
        ("postgres", "pool_size"),
        ("postgres", "connect_timeout"),
        ("session", "cookie_name"),
        ("discovery", "interval"),
        ("discovery", "probe_timeout"),
        ("discovery", "kubernetes"),
        ("discovery", "docker_socket"),
        ("ui", "icons_dir"),
        ("session", "ttl"),
        ("session", "idle_timeout"),
        ("session", "bind"),
        ("session", "same_site"),
        ("session", "secret_mode"),
        ("session", "secret_file"),
        ("session", "max_active_sessions"),
        ("security", "max_password_bytes"),
        ("security", "max_forwarded_uri_bytes"),
        ("security", "max_cookie_header_bytes"),
        ("security", "login_rate_limit_enabled"),
        ("security", "login_attempts_per_ip"),
        ("security", "login_window"),
        ("security", "login_lockout"),
        ("security", "global_login_rate_limit_enabled"),
        ("security", "global_login_attempts"),
        ("security", "global_login_window"),
        ("security", "global_login_lockout"),
        ("security", "max_rate_limit_entries"),
        ("security", "max_concurrent_password_checks"),
        ("metrics", "enabled"),
        ("metrics", "path"),
        ("logging", "level"),
    ]
    .into_iter()
    .collect();
    for (block, key) in values.keys() {
        if !allowed.contains(&(block.as_str(), key.as_str())) {
            return Err(format!("unknown configuration key: {block}.{key}"));
        }
    }
    Ok(())
}
fn get_required<'a>(
    values: &'a HashMap<(String, String), String>,
    block: &str,
    key: &str,
) -> Result<&'a str, String> {
    values
        .get(&(block.to_string(), key.to_string()))
        .map(String::as_str)
        .ok_or_else(|| format!("missing required configuration key: {block}.{key}"))
}

fn get_or<'a>(
    values: &'a HashMap<(String, String), String>,
    block: &str,
    key: &str,
    default: &'a str,
) -> Result<&'a str, String> {
    Ok(values
        .get(&(block.to_string(), key.to_string()))
        .map(String::as_str)
        .unwrap_or(default))
}

fn parse_bool(value: &str, name: &str) -> Result<bool, String> {
    match value.to_ascii_lowercase().as_str() {
        "true" => Ok(true),
        "false" => Ok(false),
        _ => Err(format!("invalid {name}: expected true or false")),
    }
}

fn parse_usize(value: &str, name: &str) -> Result<usize, String> {
    value
        .parse::<usize>()
        .map_err(|e| format!("invalid {name}: {e}"))
}

pub fn parse_duration(value: &str) -> Result<Duration, String> {
    let split = value
        .find(|c: char| !c.is_ascii_digit())
        .ok_or_else(|| format!("duration requires a unit: {value}"))?;
    let (n, unit) = value.split_at(split);
    let n = n
        .parse::<u64>()
        .map_err(|e| format!("invalid duration {value}: {e}"))?;
    let seconds = match unit {
        "s" => Some(n),
        "m" => n.checked_mul(60),
        "h" => n.checked_mul(3600),
        "d" => n.checked_mul(86400),
        _ => None,
    }
    .ok_or_else(|| format!("invalid or overflowing duration: {value}"))?;
    Ok(Duration::from_secs(seconds))
}

pub fn parse_size(value: &str) -> Result<usize, String> {
    let split = value
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(value.len());
    let (n, unit) = value.split_at(split);
    let n = n
        .parse::<usize>()
        .map_err(|e| format!("invalid size {value}: {e}"))?;
    let multiplier = match unit {
        "" | "B" => 1usize,
        "KiB" => 1024,
        "MiB" => 1024 * 1024,
        _ => return Err(format!("invalid size unit in {value}")),
    };
    n.checked_mul(multiplier)
        .ok_or_else(|| format!("size overflows usize: {value}"))
}

fn validate_secret_path(path: &Path, name: &str) -> Result<(), String> {
    if !path.is_absolute() {
        return Err(format!("{name} must be an absolute path"));
    }
    if path.components().any(|component| matches!(component, std::path::Component::ParentDir)) {
        return Err(format!("{name} must not contain '..'"));
    }
    if path.as_os_str().is_empty() {
        return Err(format!("{name} must not be empty"));
    }
    Ok(())
}

fn validate_metrics_path(path: &str) -> Result<(), String> {
    if path.is_empty() || path.len() > 128 || !path.starts_with('/') {
        return Err("metrics.path must be an absolute path of at most 128 bytes".into());
    }
    if path.contains('?') || path.contains('#') || path.bytes().any(|b| b <= 0x20 || b == 0x7f) {
        return Err("metrics.path must not contain query, fragment, whitespace, or control characters".into());
    }
    if path == "/healthz" || path.starts_with("/auth/") {
        return Err("metrics.path must not overlap /healthz or the reserved /auth/ namespace".into());
    }
    Ok(())
}

fn validate_cookie_name(name: &str) -> Result<(), String> {
    if name.is_empty()
        || !name.bytes().all(|b| {
            b.is_ascii_alphanumeric()
                || matches!(b, b'!' | b'#' | b'$' | b'%' | b'&' | b'\'' | b'*' | b'+' | b'-' | b'.' | b'^' | b'_' | b'`' | b'|' | b'~')
        })
    {
        return Err("session.cookie_name contains invalid characters".into());
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Token {
    Ident(String),
    String(String),
    Bare(String),
    LBrace,
    RBrace,
    Equal,
}

fn parse_hcl_subset(raw: &str) -> Result<HashMap<(String, String), String>, String> {
    let tokens = tokenize(raw)?;
    let allowed_blocks: HashSet<&str> = [
        "server",
        "authentication",
        "session",
        "security",
        "metrics",
        "discovery",
        "roles",
        "users",
        "storage",
        "postgres",
        "ui",
        "logging",
    ]
    .into_iter()
    .collect();
    let mut seen_blocks = HashSet::new();
    let mut i = 0usize;
    let mut result = HashMap::new();
    while i < tokens.len() {
        let block = match tokens.get(i) {
            Some(Token::Ident(s)) => s.clone(),
            other => return Err(format!("expected block name, found {other:?}")),
        };
        if !allowed_blocks.contains(block.as_str()) {
            return Err(format!("unknown configuration block: {block}"));
        }
        if !seen_blocks.insert(block.clone()) {
            return Err(format!("duplicate configuration block: {block}"));
        }
        i += 1;
        match tokens.get(i) {
            Some(Token::LBrace) => i += 1,
            other => return Err(format!("expected '{{' after {block}, found {other:?}")),
        }
        loop {
            match tokens.get(i) {
                Some(Token::RBrace) => {
                    i += 1;
                    break;
                }
                Some(Token::Ident(key)) => {
                    let key = key.clone();
                    i += 1;
                    match tokens.get(i) {
                        Some(Token::Equal) => i += 1,
                        other => return Err(format!("expected '=' after {block}.{key}, found {other:?}")),
                    }
                    let value = match tokens.get(i) {
                        Some(Token::String(v)) | Some(Token::Bare(v)) | Some(Token::Ident(v)) => v.clone(),
                        other => return Err(format!("expected scalar value for {block}.{key}, found {other:?}")),
                    };
                    i += 1;
                    if result.insert((block.clone(), key.clone()), value).is_some() {
                        return Err(format!("duplicate configuration key: {block}.{key}"));
                    }
                }
                Some(Token::LBrace) => return Err("nested HCL blocks are not supported".into()),
                None => return Err(format!("unterminated block: {block}")),
                other => return Err(format!("unexpected token in block {block}: {other:?}")),
            }
        }
    }
    Ok(result)
}

fn tokenize(raw: &str) -> Result<Vec<Token>, String> {
    let chars: Vec<char> = raw.chars().collect();
    let mut out = Vec::new();
    let mut i = 0usize;
    while i < chars.len() {
        let c = chars[i];
        if c.is_whitespace() {
            i += 1;
            continue;
        }
        if c == '#' {
            while i < chars.len() && chars[i] != '\n' {
                i += 1;
            }
            continue;
        }
        if c == '/' && i + 1 < chars.len() && chars[i + 1] == '/' {
            i += 2;
            while i < chars.len() && chars[i] != '\n' {
                i += 1;
            }
            continue;
        }
        match c {
            '{' => {
                out.push(Token::LBrace);
                i += 1;
            }
            '}' => {
                out.push(Token::RBrace);
                i += 1;
            }
            '=' => {
                out.push(Token::Equal);
                i += 1;
            }
            '"' => {
                i += 1;
                let mut value = String::new();
                let mut closed = false;
                while i < chars.len() {
                    match chars[i] {
                        '"' => {
                            i += 1;
                            closed = true;
                            break;
                        }
                        '\\' => {
                            i += 1;
                            if i >= chars.len() {
                                return Err("unterminated escape in string".into());
                            }
                            match chars[i] {
                                '"' => value.push('"'),
                                '\\' => value.push('\\'),
                                'n' => value.push('\n'),
                                'r' => value.push('\r'),
                                't' => value.push('\t'),
                                other => return Err(format!("unsupported escape: \\{other}")),
                            }
                            i += 1;
                        }
                        ch => {
                            if ch == '\0' {
                                return Err("NUL is forbidden in configuration strings".into());
                            }
                            value.push(ch);
                            i += 1;
                        }
                    }
                }
                if !closed {
                    return Err("unterminated quoted string".into());
                }
                out.push(Token::String(value));
            }
            _ if is_ident_start(c) => {
                let start = i;
                i += 1;
                while i < chars.len() && is_ident_continue(chars[i]) {
                    i += 1;
                }
                out.push(Token::Ident(chars[start..i].iter().collect()));
            }
            _ if c.is_ascii_digit() => {
                let start = i;
                i += 1;
                while i < chars.len()
                    && !chars[i].is_whitespace()
                    && !matches!(chars[i], '{' | '}' | '=')
                {
                    i += 1;
                }
                out.push(Token::Bare(chars[start..i].iter().collect()));
            }
            other => return Err(format!("unexpected character in HCL: {other:?}")),
        }
    }
    Ok(out)
}

fn is_ident_start(c: char) -> bool {
    c.is_ascii_alphabetic() || c == '_'
}

fn is_ident_continue(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '_' | '-')
}


#[cfg(test)]
mod tests {
    use super::*;

    fn minimal_config() -> &'static str {
        r#"
        server {
          listen_address = "0.0.0.0:4181"
        }
        authentication {
          superadmin_login_file    = "/run/secrets/portal_superadmin_login"
          superadmin_password_file = "/run/secrets/portal_superadmin_password"
        }
        session {
          ttl = "8h"
        }
        "#
    }

    #[test]
    fn parses_minimal_config() {
        let cfg = Config::parse(minimal_config()).unwrap();
        assert_eq!(
            cfg.authentication.superadmin_login_file,
            PathBuf::from("/run/secrets/portal_superadmin_login")
        );
        assert_eq!(
            cfg.authentication.superadmin_password_file,
            PathBuf::from("/run/secrets/portal_superadmin_password")
        );
        assert!(cfg.roles.names.is_empty());
        assert_eq!(cfg.users.max_users, 64);
        assert!(matches!(
            &cfg.storage.backend,
            StorageBackend::Sqlite(path) if path == &PathBuf::from("/var/lib/portal/portal.db")
        ));
        assert_eq!(cfg.storage.retention_days, 90);
        assert_eq!(cfg.server.max_body_bytes, 8192);
        assert_eq!(cfg.server.max_connections, 1024);
        assert_eq!(cfg.server.max_headers, 64);
        assert_eq!(cfg.server.max_header_bytes, 32 * 1024);
        assert_eq!(cfg.server.header_read_timeout, Duration::from_secs(10));
        assert_eq!(cfg.server.body_read_timeout, Duration::from_secs(10));
        assert_eq!(cfg.session.ttl, Duration::from_secs(8 * 3600));
        assert_eq!(cfg.session.max_active_sessions, 4096);
    }

    #[test]
    fn discovery_is_off_by_default_and_parses_its_keys() {
        let cfg = Config::parse(minimal_config()).unwrap();
        assert!(!cfg.discovery.kubernetes);
        assert!(cfg.discovery.docker_socket.is_none());
        assert_eq!(cfg.discovery.interval, Duration::from_secs(15));

        let raw = format!(
            "{}\n        discovery {{\n          interval = \"30s\"\n          probe_timeout = \"2s\"\n          kubernetes = true\n          docker_socket = \"/var/run/docker.sock\"\n        }}",
            minimal_config()
        );
        let cfg = Config::parse(&raw).unwrap();
        assert!(cfg.discovery.kubernetes);
        assert_eq!(cfg.discovery.docker_socket, Some(PathBuf::from("/var/run/docker.sock")));
        assert_eq!(cfg.discovery.probe_timeout, Duration::from_secs(2));
    }

    #[test]
    fn rejects_bad_discovery_values() {
        for body in ["interval = \"1s\"", "docker_socket = \"docker.sock\"", "kubernetes = \"maybe\""] {
            let raw = format!("{}\n        discovery {{\n          {body}\n        }}", minimal_config());
            assert!(Config::parse(&raw).is_err(), "{body} must be rejected");
        }
    }

    #[test]
    fn session_idle_timeout_and_binding_have_safe_defaults() {
        let cfg = Config::parse(minimal_config()).unwrap();
        assert_eq!(cfg.session.idle_timeout, Duration::from_secs(30 * 60));
        assert_eq!(cfg.session.bind, SessionBind::IpUserAgent);
    }

    #[test]
    fn parses_idle_timeout_and_binding() {
        let raw = minimal_config().replace(
            "ttl = \"8h\"",
            "ttl = \"8h\"\n          idle_timeout = \"10m\"\n          bind = \"user_agent\"",
        );
        let cfg = Config::parse(&raw).unwrap();
        assert_eq!(cfg.session.idle_timeout, Duration::from_secs(600));
        assert_eq!(cfg.session.bind, SessionBind::UserAgent);
    }

    #[test]
    fn rejects_bad_idle_timeout_and_binding() {
        for extra in ["idle_timeout = \"10s\"", "idle_timeout = \"9h\"", "bind = \"ip\""] {
            let raw = minimal_config().replace("ttl = \"8h\"", &format!("ttl = \"8h\"\n          {extra}"));
            assert!(Config::parse(&raw).is_err(), "{extra} must be rejected");
        }
    }

    #[test]
    fn session_secret_defaults_to_ephemeral() {
        let cfg = Config::parse(minimal_config()).unwrap();
        assert!(matches!(cfg.session.secret, SessionSecretConfig::Ephemeral));
    }

    #[test]
    fn parses_file_session_secret() {
        let raw = minimal_config().replace(
            "ttl = \"8h\"",
            "ttl = \"8h\"\n          secret_mode = \"file\"\n          secret_file = \"/run/secrets/portal_session_secret\"",
        );
        let cfg = Config::parse(&raw).unwrap();
        match cfg.session.secret {
            SessionSecretConfig::File(path) => {
                assert_eq!(path, PathBuf::from("/run/secrets/portal_session_secret"));
            }
            SessionSecretConfig::Ephemeral => panic!("expected file session secret"),
        }
    }

    #[test]
    fn rejects_secret_file_with_ephemeral_mode() {
        let raw = minimal_config().replace(
            "ttl = \"8h\"",
            "ttl = \"8h\"\n          secret_mode = \"ephemeral\"\n          secret_file = \"/run/secrets/session\"",
        );
        assert!(Config::parse(&raw).is_err());
    }

    #[test]
    fn file_mode_requires_absolute_secret_file() {
        let missing = minimal_config().replace(
            "ttl = \"8h\"",
            "ttl = \"8h\"\n          secret_mode = \"file\"",
        );
        assert!(Config::parse(&missing).is_err());

        let relative = minimal_config().replace(
            "ttl = \"8h\"",
            "ttl = \"8h\"\n          secret_mode = \"file\"\n          secret_file = \"relative/session-secret\"",
        );
        assert!(Config::parse(&relative).is_err());
    }

    #[test]
    fn rejects_legacy_inline_secret() {
        let raw = minimal_config().replace(
            "ttl = \"8h\"",
            "ttl = \"8h\"\n          secret = \"0123456789abcdef0123456789abcdef\"",
        );
        assert!(Config::parse(&raw).is_err());
    }

    #[test]
    fn rejects_relative_superadmin_files_and_the_old_key() {
        for (from, to) in [
            (
                "superadmin_password_file = \"/run/secrets/portal_superadmin_password\"",
                "superadmin_password_file = \"relative/password\"",
            ),
            (
                "superadmin_login_file    = \"/run/secrets/portal_superadmin_login\"",
                "superadmin_login_file = \"relative/login\"",
            ),
            (
                "superadmin_login_file    = \"/run/secrets/portal_superadmin_login\"",
                "password_file = \"/run/secrets/portal_password\"",
            ),
        ] {
            let raw = minimal_config().replace(from, to);
            assert!(Config::parse(&raw).is_err(), "{to} must be rejected");
        }
    }

    #[test]
    fn parses_roles_users_and_storage() {
        let raw = format!(
            "{}\nroles {{ names = \"Ops, dev,ops , viewer\" }}\nusers {{ max_users = 5 }}\nstorage {{ database = \"/data/p.db\" retention_days = 30 }}",
            minimal_config()
        );
        let cfg = Config::parse(&raw).unwrap();
        assert_eq!(cfg.roles.names, ["ops", "dev", "viewer"]);
        assert_eq!(cfg.users.max_users, 5);
        assert!(matches!(
            &cfg.storage.backend,
            StorageBackend::Sqlite(path) if path == &PathBuf::from("/data/p.db")
        ));
        assert_eq!(cfg.storage.retention_days, 30);
    }

    /// The minimal configuration with the shared signing key a PostgreSQL setup needs.
    fn shared_config() -> String {
        minimal_config().replace(
            "ttl = \"8h\"",
            "ttl = \"8h\" secret_mode = \"file\" secret_file = \"/run/secrets/portal_session_secret\"",
        )
    }

    const POSTGRES_BLOCK: &str = "postgres { host = \"postgresql-rw.data.svc.cluster.local\" database = \"portal\" user = \"portal\" password_file = \"/run/secrets/portal_db_password\" }";

    #[test]
    fn the_postgres_block_replaces_the_sqlite_file() {
        let raw = format!("{}\nstorage {{ retention_days = 30 }}\n{POSTGRES_BLOCK}", shared_config());
        let cfg = Config::parse(&raw).unwrap();
        let StorageBackend::Postgres(pg) = &cfg.storage.backend else {
            panic!("expected the postgres backend");
        };
        assert_eq!(pg.host, "postgresql-rw.data.svc.cluster.local");
        assert_eq!((pg.port, pg.pool_size), (5432, 4));
        assert_eq!(pg.connect_timeout, Duration::from_secs(5));
        assert_eq!(pg.password_file, PathBuf::from("/run/secrets/portal_db_password"));
        assert_eq!(cfg.storage.retention_days, 30, "retention applies to both");

        let tuned = POSTGRES_BLOCK.replace(" }", " port = 6432 pool_size = 8 connect_timeout = \"2s\" }");
        let cfg = Config::parse(&format!("{}\n{tuned}", shared_config())).unwrap();
        let StorageBackend::Postgres(pg) = &cfg.storage.backend else {
            panic!("expected the postgres backend");
        };
        assert_eq!((pg.port, pg.pool_size, pg.connect_timeout), (6432, 8, Duration::from_secs(2)));
    }

    #[test]
    fn the_postgres_block_and_the_sqlite_file_exclude_each_other() {
        let raw = format!(
            "{}\nstorage {{ database = \"/data/p.db\" }}\n{POSTGRES_BLOCK}",
            shared_config()
        );
        let error = Config::parse(&raw).err().unwrap();
        assert!(error.contains("storage.database"), "{error}");
    }

    #[test]
    fn postgres_needs_the_shared_signing_key() {
        let raw = format!("{}\n{POSTGRES_BLOCK}", minimal_config());
        let error = Config::parse(&raw).err().unwrap();
        assert!(error.contains("session.secret_mode"), "{error}");
    }

    #[test]
    fn rejects_bad_postgres_values() {
        for block in [
            "postgres { host = \"db\" database = \"portal\" user = \"portal\" }",
            "postgres { host = \"db\" database = \"portal\" password_file = \"/p\" }",
            "postgres { database = \"portal\" user = \"portal\" password_file = \"/p\" }",
            "postgres { host = \"db host\" database = \"portal\" user = \"portal\" password_file = \"/p\" }",
            "postgres { host = \"db\" database = \"po rtal\" user = \"portal\" password_file = \"/p\" }",
            "postgres { host = \"db\" database = \"portal\" user = \"portal\" password_file = \"p\" }",
            "postgres { host = \"db\" database = \"portal\" user = \"portal\" password_file = \"/p\" port = 0 }",
            "postgres { host = \"db\" database = \"portal\" user = \"portal\" password_file = \"/p\" port = 70000 }",
            "postgres { host = \"db\" database = \"portal\" user = \"portal\" password_file = \"/p\" pool_size = 0 }",
            "postgres { host = \"db\" database = \"portal\" user = \"portal\" password_file = \"/p\" connect_timeout = \"0s\" }",
            "postgres { host = \"db\" database = \"portal\" user = \"portal\" password_file = \"/p\" sslmode = \"require\" }",
        ] {
            let raw = format!("{}\n{block}", shared_config());
            assert!(Config::parse(&raw).is_err(), "{block} must be rejected");
        }
    }

    #[test]
    fn rejects_bad_roles_users_and_storage_values() {
        for block in [
            "roles { names = \"ops, bad role\" }",
            "roles { names = \"a,b,c,d,e,f,g,h,i,j,k,l,m,n,o,p,q\" }",
            "users { max_users = 0 }",
            "users { max_users = 5000 }",
            "storage { database = \"relative.db\" }",
            "storage { retention_days = 0 }",
            "storage { retention_days = 9999 }",
            "roles { label = \"x\" }",
        ] {
            let raw = format!("{}\n{block}", shared_config());
            assert!(Config::parse(&raw).is_err(), "{block} must be rejected");
        }
    }

    #[test]
    fn rejects_unknown_key() {
        let raw = format!("{}\nlogging {{ magic = \"yes\" }}", minimal_config());
        assert!(Config::parse(&raw).is_err());
    }

    #[test]
    fn rejects_duplicate_key() {
        let raw = r#"server { workers = 1 workers = 2 }"#;
        assert!(parse_hcl_subset(raw).is_err());
    }

    #[test]
    fn rejects_unknown_and_duplicate_blocks() {
        assert!(parse_hcl_subset("unknown {}").is_err());
        assert!(parse_hcl_subset("server {} server {}").is_err());
    }

    #[test]
    fn parses_login_rate_limit_toggle() {
        assert!(parse_bool("true", "security.login_rate_limit_enabled").unwrap());
        assert!(!parse_bool("false", "security.login_rate_limit_enabled").unwrap());
        assert!(parse_bool("yes", "security.login_rate_limit_enabled").is_err());
    }

    #[test]
    fn parses_global_rate_limit_and_metrics() {
        let raw = format!(
            "{}\nsecurity {{ global_login_rate_limit_enabled = true global_login_attempts = 250 global_login_window = \"2m\" global_login_lockout = \"45s\" }}\nmetrics {{ enabled = true path = \"/metrics\" }}",
            minimal_config()
        );
        let cfg = Config::parse(&raw).unwrap();
        assert!(cfg.security.global_login_rate_limit_enabled);
        assert_eq!(cfg.security.global_login_attempts, 250);
        assert_eq!(cfg.security.global_login_window, Duration::from_secs(120));
        assert_eq!(cfg.security.global_login_lockout, Duration::from_secs(45));
        assert!(cfg.metrics.enabled);
        assert_eq!(cfg.metrics.path, "/metrics");
    }

    #[test]
    fn rejects_invalid_metrics_path() {
        let raw = format!("{}\nmetrics {{ path = \"metrics\" }}", minimal_config());
        assert!(Config::parse(&raw).is_err());
    }

    #[test]
    fn parses_units() {
        assert_eq!(parse_size("8KiB").unwrap(), 8192);
        assert_eq!(parse_duration("5m").unwrap(), Duration::from_secs(300));
    }
}
