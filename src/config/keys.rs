//! The table of configuration keys: the one place that holds the type, the range, the default
//! and the description of each key. The parser, the effective configuration and the generated
//! reference in the README all read this table.

use std::net::SocketAddr;
use std::path::Path;
use std::time::Duration;

/// The type of a key and the values that it accepts.
pub enum Kind {
    /// A quoted string. `check` refuses a bad value and says why.
    Text {
        expected: &'static str,
        check: fn(&str) -> Result<(), String>,
    },
    /// A decimal number written without quotes.
    Int { min: u64, max: u64 },
    /// `true` or `false`, written without quotes.
    Bool,
    /// A quoted duration with a unit: `ms`, `s`, `m`, `h` or `d`.
    Duration { min: Duration, max: Duration },
    /// A quoted size with a unit: `B`, `KiB` or `MiB`.
    Size { min: usize, max: usize },
    /// A quoted string with one of these exact values.
    Enum(&'static [&'static str]),
    /// A list of quoted strings: `["a", "b"]`. `check` refuses a bad item and says why.
    List {
        expected: &'static str,
        check: fn(&str) -> Result<(), String>,
        max_items: usize,
    },
    /// A secret. The file never holds it: the key `<name>_env` names an environment variable, or the
    /// key `<name>_file` names a file. `max_bytes` is the largest size that the program reads.
    Secret { max_bytes: usize },
}

/// What the program does when the file does not give the key.
pub enum Presence {
    /// The program stops at start.
    Required,
    /// The program uses this value. It is written as the file would write it, without quotes.
    Default(&'static str),
    /// The key has no value. The feature that the key controls stays off.
    Optional,
}

/// The condition under which a key applies. A key that does not apply must not be in the file.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum When {
    Always,
    /// The block `postgres` is present.
    PostgresBlock,
    /// The block `postgres` is absent.
    NoPostgres,
    /// `discovery.kubernetes` is `true`.
    KubernetesOn,
    /// `security.login_rate_limit_enabled` is `true`.
    LoginLimiterOn,
    /// `security.global_login_rate_limit_enabled` is `true`.
    GlobalLimiterOn,
    /// At least one of the two limiters is on.
    AnyLimiterOn,
    /// `metrics.enabled` is `true`.
    MetricsOn,
    /// `session.secret_mode` is `provided`.
    SecretProvided,
    /// The block `identity` is present.
    IdentityBlock,
    /// The block `pdp` is present.
    PdpBlock,
    /// A block with a label: the key applies in each block of its kind.
    LabeledBlock,
}

impl When {
    /// Why a written key is refused when the condition is false.
    pub fn refusal(self) -> &'static str {
        match self {
            When::Always
            | When::PostgresBlock
            | When::IdentityBlock
            | When::PdpBlock
            | When::LabeledBlock => "the key always applies",
            When::NoPostgres => "the postgres block is present",
            When::KubernetesOn => "discovery.kubernetes is false",
            When::LoginLimiterOn => "security.login_rate_limit_enabled is false",
            When::GlobalLimiterOn => "security.global_login_rate_limit_enabled is false",
            When::AnyLimiterOn => "both login limiters are off",
            When::MetricsOn => "metrics.enabled is false",
            When::SecretProvided => "session.secret_mode is ephemeral",
        }
    }

    /// The condition as a sentence for the reference.
    pub fn sentence(self) -> &'static str {
        match self {
            When::Always
            | When::PostgresBlock
            | When::IdentityBlock
            | When::PdpBlock
            | When::LabeledBlock => "",
            When::NoPostgres => "Applies only without the `postgres` block.",
            When::KubernetesOn => "Applies only when `discovery.kubernetes` is `true`.",
            When::LoginLimiterOn => {
                "Applies only when `security.login_rate_limit_enabled` is `true`."
            }
            When::GlobalLimiterOn => {
                "Applies only when `security.global_login_rate_limit_enabled` is `true`."
            }
            When::AnyLimiterOn => "Applies only when at least one login limiter is on.",
            When::MetricsOn => "Applies only when `metrics.enabled` is `true`.",
            When::SecretProvided => "Applies only when `session.secret_mode` is `provided`.",
        }
    }
}

pub struct Key {
    pub block: &'static str,
    pub name: &'static str,
    pub kind: Kind,
    pub presence: Presence,
    pub when: When,
    /// One or two sentences in ASD-STE100.
    pub doc: &'static str,
}

impl Key {
    pub fn path(&self) -> String {
        format!("{}.{}", self.block, self.name)
    }

    pub fn is_secret(&self) -> bool {
        matches!(self.kind, Kind::Secret { .. })
    }

    /// What this key accepts, for an error message.
    pub fn expected(&self) -> String {
        match &self.kind {
            Kind::Text { expected, .. } => (*expected).to_string(),
            Kind::Int { min, max } => format!("a number without quotes, from {min} to {max}"),
            Kind::Bool => "true or false, without quotes".to_string(),
            Kind::Duration { min, max } => format!(
                "a quoted duration with a unit (ms, s, m, h or d), from {} to {}",
                show_duration(*min),
                show_duration(*max)
            ),
            Kind::Size { min, max } => format!(
                "a quoted size with a unit (B, KiB or MiB), from {} to {}",
                show_size(*min),
                show_size(*max)
            ),
            Kind::Enum(values) => format!(
                "one of {} in a quoted string, with this exact case",
                values
                    .iter()
                    .map(|v| format!("\"{v}\""))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            Kind::List {
                expected,
                max_items,
                ..
            } => format!("a list of at most {max_items} quoted strings: {expected}"),
            Kind::Secret { .. } => "a secret".to_string(),
        }
    }
}

const fn secs(n: u64) -> Duration {
    Duration::from_secs(n)
}

const MINUTE: u64 = 60;
const HOUR: u64 = 3600;
const DAY: u64 = 86_400;

/// The blocks of the file, in the order of the reference. Each block holds the keys of the table.
pub const BLOCKS: &[(&str, &str)] = &[
    ("server", "The HTTP server."),
    ("authentication", "The superadmin: the one account that is not in the database."),
    ("users", "The accounts in the database."),
    (
        "access",
        "The grants of the users, and the actions that the services publish.",
    ),
    (
        "identity",
        "This block is optional. If it is present, the server gives an identity token to each service that it lets a user open.",
    ),
    (
        "pdp",
        "This block is optional. If it is present, the server is an AuthZEN PDP on a second, internal listener.",
    ),
    ("storage", "The history, and the SQLite file."),
    (
        "postgres",
        "This block is optional. If it is present, the server uses a shared PostgreSQL database in place of the SQLite file.",
    ),
    ("session", "The sessions and the signing key."),
    ("security", "The limits and the login limiters."),
    ("metrics", "The Prometheus endpoint."),
    ("discovery", "The sources of the service catalogue."),
    ("ui", "The pages."),
    ("logging", "The log."),
];

/// Every key. The order is the order of resolution: a key comes after the keys that its
/// condition reads.
pub static KEYS: &[Key] = &[
    Key {
        block: "server",
        name: "listen_address",
        kind: Kind::Text {
            expected: "an IP address and a port, such as 0.0.0.0:4181",
            check: check_socket_address,
        },
        presence: Presence::Required,
        when: When::Always,
        doc: "The address and the port that the server listens on.",
    },
    Key {
        block: "server",
        name: "workers",
        kind: Kind::Int { min: 1, max: 64 },
        presence: Presence::Default("4"),
        when: When::Always,
        doc: "The number of worker threads.",
    },
    Key {
        block: "server",
        name: "max_body_bytes",
        kind: Kind::Size { min: 1024, max: 1024 * 1024 },
        presence: Presence::Default("32KiB"),
        when: When::Always,
        doc: "The largest request body that the server reads. The form of a user holds one field for each grant that it shows.",
    },
    Key {
        block: "server",
        name: "max_connections",
        kind: Kind::Int { min: 1, max: 100_000 },
        presence: Presence::Default("1024"),
        when: When::Always,
        doc: "The largest number of open TCP connections.",
    },
    Key {
        block: "server",
        name: "max_headers",
        kind: Kind::Int { min: 8, max: 256 },
        presence: Presence::Default("64"),
        when: When::Always,
        doc: "The largest number of headers in a request.",
    },
    Key {
        block: "server",
        name: "max_header_bytes",
        kind: Kind::Size { min: 8 * 1024, max: 1024 * 1024 },
        presence: Presence::Default("32KiB"),
        when: When::Always,
        doc: "The largest size of the headers of a request.",
    },
    Key {
        block: "server",
        name: "header_read_timeout",
        kind: Kind::Duration { min: secs(1), max: secs(MINUTE) },
        presence: Presence::Default("10s"),
        when: When::Always,
        doc: "The time that a client has to send the request headers.",
    },
    Key {
        block: "server",
        name: "body_read_timeout",
        kind: Kind::Duration { min: secs(1), max: secs(MINUTE) },
        presence: Presence::Default("10s"),
        when: When::Always,
        doc: "The time that a client has to send the request body.",
    },
    Key {
        block: "server",
        name: "max_login_body_reads",
        kind: Kind::Int { min: 1, max: 100_000 },
        presence: Presence::Default("64"),
        when: When::Always,
        doc: "The largest number of sign-in requests whose body the server reads at the same time. The server answers `503` at once to a sign-in request above this number. Slow sign-in bodies thus cannot hold all the connections that ForwardAuth needs. It must be less than `server.max_connections`.",
    },
    Key {
        block: "server",
        name: "trusted_proxies",
        kind: Kind::List {
            expected: "IP networks such as \"10.42.0.0/16\" or \"127.0.0.1/32\"; the address is the first address of the network",
            check: check_cidr,
            max_items: 64,
        },
        presence: Presence::Required,
        when: When::Always,
        doc: "The networks of the proxies (Traefik) that set `X-Forwarded-For`. The server reads this header only from a TCP peer in these networks. It then takes the first address from the right that is not in these networks. From any other peer, the client address is the address of the peer. `[]` trusts no proxy. The client address keys the session binding and the login limiter.",
    },
    Key {
        block: "authentication",
        name: "superadmin_login",
        kind: Kind::Secret { max_bytes: 256 },
        presence: Presence::Required,
        when: When::Always,
        doc: "The login of the superadmin. It has 1 to 64 characters, and no upper-case letter, no space and no control character. The server does not change it. A new login ends the sessions of the superadmin and the impersonated sessions.",
    },
    Key {
        block: "authentication",
        name: "superadmin_password",
        kind: Kind::Secret { max_bytes: 16 * 1024 },
        presence: Presence::Required,
        when: When::Always,
        doc: "The password of the superadmin. At start, the server hashes it with Argon2id and erases it from memory. It has at most `security.max_password_bytes` bytes. The server refuses the example values of `.env.example` and of the README. A new password ends the sessions of the superadmin and the impersonated sessions.",
    },
    Key {
        block: "users",
        name: "max_users",
        kind: Kind::Int { min: 1, max: 1024 },
        presence: Presence::Default("64"),
        when: When::Always,
        doc: "The largest number of accounts in the database.",
    },
    Key {
        block: "access",
        name: "grants_mode",
        kind: Kind::Enum(&["static", "dynamic", "hybrid"]),
        presence: Presence::Default("dynamic"),
        when: When::Always,
        doc: "Where the grants come from. `static`: only the `user` blocks; the settings pages show the grants and do not change them. `dynamic`: only the settings pages; a `user` block stops the server. `hybrid`: both; the grants of a `user` block are read-only in the settings pages.",
    },
    Key {
        block: "access",
        name: "actions_timeout",
        kind: Kind::Duration { min: Duration::from_millis(100), max: secs(30) },
        presence: Presence::Default("2s"),
        when: When::Always,
        doc: "The time that the server waits for the `/.well-known/access-actions` document of one service.",
    },
    Key {
        block: "access",
        name: "actions_refresh",
        kind: Kind::Duration { min: secs(10), max: secs(DAY) },
        presence: Presence::Default("5m"),
        when: When::Always,
        doc: "The time between two reads of the `/.well-known/access-actions` documents.",
    },
    Key {
        block: "identity",
        name: "header",
        kind: Kind::Text {
            expected: "an HTTP header name of 1 to 64 characters: letters, digits and '-'",
            check: check_header_name,
        },
        presence: Presence::Default("X-Identity-Token"),
        when: When::IdentityBlock,
        doc: "The response header of `/auth/verify` that holds the identity token. Traefik copies it to the request with `authResponseHeaders`.",
    },
    Key {
        block: "identity",
        name: "issuer",
        kind: Kind::Text {
            expected: "1 to 256 visible ASCII characters, for example https://portal.example.internal",
            check: check_issuer,
        },
        presence: Presence::Required,
        when: When::IdentityBlock,
        doc: "The claim `iss` of each identity token. Each service compares it with its `identity.issuer`.",
    },
    Key {
        block: "identity",
        name: "token_ttl",
        kind: Kind::Duration { min: secs(10), max: secs(HOUR) },
        presence: Presence::Default("5m"),
        when: When::IdentityBlock,
        doc: "The life of an identity token (`exp - iat`). It must not be more than the `identity.max_age` of the services. The server gives the same token again until half of its life is gone.",
    },
    Key {
        block: "identity",
        name: "signing_key",
        kind: Kind::Secret { max_bytes: 4096 },
        presence: Presence::Required,
        when: When::IdentityBlock,
        doc: "The Ed25519 private key that signs the identity tokens (algorithm `EdDSA`): a PKCS#8 key in PEM, or the base64 text of the PEM on one line. `portal identity-public-key` prints the public key for the services.",
    },
    Key {
        block: "pdp",
        name: "listen_address",
        kind: Kind::Text {
            expected: "an IP address and a port, such as 0.0.0.0:4182",
            check: check_socket_address,
        },
        presence: Presence::Required,
        when: When::PdpBlock,
        doc: "The address and the port of the PDP listener. Do not expose it through Traefik: only the services use it.",
    },
    Key {
        block: "pdp",
        name: "url",
        kind: Kind::Text {
            expected: "an http or https URL without a path, a query or a fragment",
            check: check_base_url,
        },
        presence: Presence::Required,
        when: When::PdpBlock,
        doc: "The URL at which the services reach the PDP. `/.well-known/authzen-configuration` gives the endpoints under this URL.",
    },
    Key {
        block: "pdp",
        name: "max_body_bytes",
        kind: Kind::Size { min: 1024, max: 1024 * 1024 },
        presence: Presence::Default("64KiB"),
        when: When::PdpBlock,
        doc: "The largest request body that the PDP reads.",
    },
    Key {
        block: "pdp",
        name: "max_evaluations",
        kind: Kind::Int { min: 1, max: 1024 },
        presence: Presence::Default("64"),
        when: When::PdpBlock,
        doc: "The largest number of evaluations in one request to `/access/v1/evaluations`.",
    },
    Key {
        block: "storage",
        name: "database",
        kind: Kind::Text {
            expected: "an absolute path without '..'",
            check: check_absolute_path,
        },
        presence: Presence::Default("/var/lib/portal/portal.db"),
        when: When::NoPostgres,
        doc: "The path of the SQLite file.",
    },
    Key {
        block: "storage",
        name: "retention_days",
        kind: Kind::Int { min: 1, max: 3650 },
        presence: Presence::Default("90"),
        when: When::Always,
        doc: "The number of days that the history keeps the sessions, the service accesses, the audit events and the sign-in attempts.",
    },
    Key {
        block: "postgres",
        name: "host",
        kind: Kind::Text {
            expected: "a host name or an address of at most 253 characters",
            check: check_host,
        },
        presence: Presence::Required,
        when: When::PostgresBlock,
        doc: "The host name or the address of the PostgreSQL server.",
    },
    Key {
        block: "postgres",
        name: "port",
        kind: Kind::Int { min: 1, max: 65_535 },
        presence: Presence::Default("5432"),
        when: When::PostgresBlock,
        doc: "The TCP port of the PostgreSQL server.",
    },
    Key {
        block: "postgres",
        name: "database",
        kind: Kind::Text {
            expected: "1 to 63 letters, digits, '.', '-' or '_'",
            check: check_postgres_name,
        },
        presence: Presence::Required,
        when: When::PostgresBlock,
        doc: "The name of the database.",
    },
    Key {
        block: "postgres",
        name: "user",
        kind: Kind::Text {
            expected: "1 to 63 letters, digits, '.', '-' or '_'",
            check: check_postgres_name,
        },
        presence: Presence::Required,
        when: When::PostgresBlock,
        doc: "The name of the database user.",
    },
    Key {
        block: "postgres",
        name: "password",
        kind: Kind::Secret { max_bytes: 1024 },
        presence: Presence::Required,
        when: When::PostgresBlock,
        doc: "The password of the database user. It is valid UTF-8.",
    },
    Key {
        block: "postgres",
        name: "pool_size",
        kind: Kind::Int { min: 1, max: 64 },
        presence: Presence::Default("4"),
        when: When::PostgresBlock,
        doc: "The number of connections that the server keeps to the database.",
    },
    Key {
        block: "postgres",
        name: "connect_timeout",
        kind: Kind::Duration { min: secs(1), max: secs(MINUTE) },
        presence: Presence::Default("5s"),
        when: When::PostgresBlock,
        doc: "The time that the server waits for a new connection.",
    },
    Key {
        block: "session",
        name: "cookie_name",
        kind: Kind::Text {
            expected: "a cookie name that starts with __Host-",
            check: check_cookie_name,
        },
        presence: Presence::Default("__Host-portal"),
        when: When::Always,
        doc: "The name of the session cookie over HTTPS. Over plain HTTP the server uses the same name without the prefix, because browsers refuse `__Host-` cookies there.",
    },
    Key {
        block: "session",
        name: "ttl",
        kind: Kind::Duration { min: secs(MINUTE), max: secs(7 * DAY) },
        presence: Presence::Default("8h"),
        when: When::Always,
        doc: "The lifetime of a session.",
    },
    Key {
        block: "session",
        name: "idle_timeout",
        kind: Kind::Duration { min: secs(MINUTE), max: secs(7 * DAY) },
        presence: Presence::Default("30m"),
        when: When::Always,
        doc: "A session that nobody uses for this time ends. The value is at most `session.ttl`.",
    },
    Key {
        block: "session",
        name: "bind",
        kind: Kind::Enum(&["ip_user_agent", "user_agent", "none"]),
        presence: Presence::Default("ip_user_agent"),
        when: When::Always,
        doc: "What a session cookie is tied to: the client address and the User-Agent, the User-Agent only, or nothing.",
    },
    Key {
        block: "session",
        name: "same_site",
        kind: Kind::Enum(&["Strict", "Lax"]),
        presence: Presence::Default("Strict"),
        when: When::Always,
        doc: "The SameSite attribute of the session cookie.",
    },
    Key {
        block: "session",
        name: "secret_mode",
        kind: Kind::Enum(&["ephemeral", "provided"]),
        presence: Presence::Default("ephemeral"),
        when: When::Always,
        doc: "`ephemeral`: the server makes a new random signing key at each start. `provided`: the key comes from `session.secret_env` or `session.secret_file`. The `postgres` block needs `provided`, because all instances use the same key.",
    },
    Key {
        block: "session",
        name: "secret",
        kind: Kind::Secret { max_bytes: 4096 },
        presence: Presence::Required,
        when: When::SecretProvided,
        doc: "The signing key of the login forms, of the return links and of the anti-CSRF tokens. It has at least 32 bytes. The server refuses the example value of `.env.example`. A new value ends the sessions of the superadmin and the impersonated sessions.",
    },
    Key {
        block: "session",
        name: "max_active_sessions",
        kind: Kind::Int { min: 1, max: 1_000_000 },
        presence: Presence::Default("4096"),
        when: When::Always,
        doc: "The largest number of open sessions. A login above this number gets the status 503.",
    },
    Key {
        block: "security",
        name: "max_password_bytes",
        kind: Kind::Int { min: 8, max: 16 * 1024 },
        presence: Presence::Default("1024"),
        when: When::Always,
        doc: "The largest size of a password.",
    },
    Key {
        block: "security",
        name: "max_forwarded_uri_bytes",
        kind: Kind::Int { min: 128, max: 64 * 1024 },
        presence: Presence::Default("4096"),
        when: When::Always,
        doc: "The largest size of the forwarded URI.",
    },
    Key {
        block: "security",
        name: "max_cookie_header_bytes",
        kind: Kind::Int { min: 256, max: 64 * 1024 },
        presence: Presence::Default("8192"),
        when: When::Always,
        doc: "The largest size of the Cookie header.",
    },
    Key {
        block: "security",
        name: "login_rate_limit_enabled",
        kind: Kind::Bool,
        presence: Presence::Default("true"),
        when: When::Always,
        doc: "Turn the limiter of failed logins for each client address on or off.",
    },
    Key {
        block: "security",
        name: "login_attempts_per_ip",
        kind: Kind::Int { min: 1, max: 1_000_000 },
        presence: Presence::Default("5"),
        when: When::LoginLimiterOn,
        doc: "The number of failed logins for each client address in one window before the lockout.",
    },
    Key {
        block: "security",
        name: "login_window",
        kind: Kind::Duration { min: secs(1), max: secs(DAY) },
        presence: Presence::Default("1m"),
        when: When::LoginLimiterOn,
        doc: "The window in which the failed logins of one client address count.",
    },
    Key {
        block: "security",
        name: "login_lockout",
        kind: Kind::Duration { min: secs(1), max: secs(DAY) },
        presence: Presence::Default("5m"),
        when: When::LoginLimiterOn,
        doc: "The time that a client address stays locked out.",
    },
    Key {
        block: "security",
        name: "global_login_rate_limit_enabled",
        kind: Kind::Bool,
        presence: Presence::Default("true"),
        when: When::Always,
        doc: "Turn the global limiter of failed logins on or off. A good login does not clear it.",
    },
    Key {
        block: "security",
        name: "global_login_attempts",
        kind: Kind::Int { min: 1, max: 1_000_000 },
        presence: Presence::Default("100"),
        when: When::GlobalLimiterOn,
        doc: "The number of failed logins from all clients in one window before the lockout. The superadmin login has a separate budget of the same size. Failures against other logins never lock out the superadmin.",
    },
    Key {
        block: "security",
        name: "global_login_window",
        kind: Kind::Duration { min: secs(1), max: secs(DAY) },
        presence: Presence::Default("1m"),
        when: When::GlobalLimiterOn,
        doc: "The window in which the failed logins of all clients count.",
    },
    Key {
        block: "security",
        name: "global_login_lockout",
        kind: Kind::Duration { min: secs(1), max: secs(DAY) },
        presence: Presence::Default("30s"),
        when: When::GlobalLimiterOn,
        doc: "The time that the logins of the budget stay locked out: all other logins, or the superadmin login.",
    },
    Key {
        block: "security",
        name: "max_rate_limit_entries",
        kind: Kind::Int { min: 64, max: 1_000_000 },
        presence: Presence::Default("4096"),
        when: When::AnyLimiterOn,
        doc: "The largest number of client addresses that the limiter remembers.",
    },
    Key {
        block: "security",
        name: "max_concurrent_password_checks",
        kind: Kind::Int { min: 1, max: 64 },
        presence: Presence::Default("1"),
        when: When::Always,
        doc: "The largest number of Argon2 checks that run at the same time.",
    },
    Key {
        block: "metrics",
        name: "enabled",
        kind: Kind::Bool,
        presence: Presence::Default("true"),
        when: When::Always,
        doc: "Turn the Prometheus endpoint on or off.",
    },
    Key {
        block: "metrics",
        name: "path",
        kind: Kind::Text {
            expected: "an absolute URL path of at most 128 bytes, not /healthz and not under /auth/",
            check: check_metrics_path,
        },
        presence: Presence::Default("/metrics"),
        when: When::MetricsOn,
        doc: "The URL path of the Prometheus endpoint.",
    },
    Key {
        block: "discovery",
        name: "interval",
        kind: Kind::Duration { min: secs(2), max: secs(HOUR) },
        presence: Presence::Default("15s"),
        when: When::Always,
        doc: "The time between two reads of the sources.",
    },
    Key {
        block: "discovery",
        name: "probe_timeout",
        kind: Kind::Duration { min: Duration::from_millis(200), max: secs(30) },
        presence: Presence::Default("3s"),
        when: When::Always,
        doc: "The time that a health probe and a source request wait for an answer.",
    },
    Key {
        block: "discovery",
        name: "kubernetes",
        kind: Kind::Bool,
        presence: Presence::Default("false"),
        when: When::Always,
        doc: "Read the IngressRoute annotations through the API of the cluster. The server needs the ServiceAccount of the pod.",
    },
    Key {
        block: "discovery",
        name: "kubernetes_host_env",
        kind: Kind::Text {
            expected: "the name of an environment variable",
            check: check_env_name,
        },
        presence: Presence::Required,
        when: When::KubernetesOn,
        doc: "The environment variable that holds the host of the API of the cluster, for example `KUBERNETES_SERVICE_HOST`. At start, the variable must exist and must not be empty.",
    },
    Key {
        block: "discovery",
        name: "kubernetes_port_env",
        kind: Kind::Text {
            expected: "the name of an environment variable",
            check: check_env_name,
        },
        presence: Presence::Required,
        when: When::KubernetesOn,
        doc: "The environment variable that holds the HTTPS port of the API of the cluster, for example `KUBERNETES_SERVICE_PORT_HTTPS`. At start, the variable must exist and must hold a port number.",
    },
    Key {
        block: "discovery",
        name: "docker_socket",
        kind: Kind::Text {
            expected: "an absolute path without '..'",
            check: check_absolute_path,
        },
        presence: Presence::Optional,
        when: When::Always,
        doc: "The Docker socket that the server reads the container labels from. Without this key, the Docker source is off.",
    },
    Key {
        block: "ui",
        name: "icons_dir",
        kind: Kind::Text {
            expected: "an absolute path without '..'",
            check: check_absolute_path,
        },
        presence: Presence::Default("/icons"),
        when: When::Always,
        doc: "The directory of the offline icon library. If the directory is missing, the pages show initials.",
    },
    Key {
        block: "logging",
        name: "level",
        kind: Kind::Enum(&["error", "warn", "info", "debug"]),
        presence: Presence::Default("info"),
        when: When::Always,
        doc: "The lowest level that the server logs.",
    },
];

/// The blocks with a label, in the order of the reference. The label names a user or a server.
pub const LABELED_BLOCKS: &[(&str, &str, &str)] = &[
    (
        "user",
        "<login>",
        "The grants of one user that the configuration gives (modes `static` and `hybrid`). The label is the login, as the server keeps it: 1 to 32 characters of a-z, 0-9, '.', '-' and '_'. The account itself is in the database. The superadmin has all grants and has no `user` block.",
    ),
    (
        "pdp_client",
        "<server>",
        "One server that asks the PDP for decisions. The label is the name of the server: the id of its service in the catalogue, and the audience of its identity tokens. The PDP answers only for the resources of this server.",
    ),
];

/// The keys of the blocks with a label. Each block of a kind has these keys.
pub static LABELED_KEYS: &[Key] = &[
    Key {
        block: "user",
        name: "grants",
        kind: Kind::List {
            expected: "grants such as \"access on service:grafana\" or \"query.run.write on service:clickhouse-dash/database:analytics\": an action, the word on, service: and the id of the service, then optionally / and a sub-resource as type:id",
            check: check_grant,
            max_items: crate::grants::MAX_GRANTS_PER_USER,
        },
        presence: Presence::Required,
        when: When::LabeledBlock,
        doc: "The grants of the user. `access` lets the user open the service. The other actions are the names that the service publishes at `/.well-known/access-actions`. A grant on a service covers all the sub-resources of this service. A grant on a sub-resource, `<action> on service:<id>/<type>:<resource id>`, covers only this sub-resource. The type has 1 to 32 characters of a-z, 0-9 and '_', and is not `service`. The resource id has 1 to 128 characters, without a space. The action `access` has no grant on a sub-resource. At start, the server checks only the form of each grant: the services and their resource types are known at run time. The Services page of the settings shows a grant whose type the service does not declare. The PDP does not apply this grant.",
    },
    Key {
        block: "pdp_client",
        name: "token",
        kind: Kind::Secret { max_bytes: 1024 },
        presence: Presence::Required,
        when: When::LabeledBlock,
        doc: "The bearer token of the server. It has at least 32 bytes. Two servers must not have the same token.",
    },
];

pub fn find(block: &str, name: &str) -> Option<&'static Key> {
    KEYS.iter().find(|k| k.block == block && k.name == name)
}

pub fn find_labeled(block: &str, name: &str) -> Option<&'static Key> {
    LABELED_KEYS
        .iter()
        .find(|k| k.block == block && k.name == name)
}

/// The name of each block, for the closed list of the parser.
pub fn block_names() -> Vec<&'static str> {
    BLOCKS.iter().map(|(name, _)| *name).collect()
}

/// The name of each block with a label, for the closed list of the parser.
pub fn labeled_block_names() -> Vec<&'static str> {
    LABELED_BLOCKS.iter().map(|(name, _, _)| *name).collect()
}

/// The blocks that the file may omit. Their keys apply only when the block is present.
pub fn is_optional_block(block: &str) -> bool {
    matches!(block, "postgres" | "identity" | "pdp")
}

// Checks of the text keys. Each returns the reason when the value is bad.

fn check_cidr(value: &str) -> Result<(), String> {
    crate::transport::Cidr::parse(value).map(|_| ())
}

fn check_socket_address(value: &str) -> Result<(), String> {
    value
        .parse::<SocketAddr>()
        .map(|_| ())
        .map_err(|e| e.to_string())
}

fn check_absolute_path(value: &str) -> Result<(), String> {
    let path = Path::new(value);
    if value.is_empty() || !path.is_absolute() {
        return Err("the path is not absolute".into());
    }
    if path
        .components()
        .any(|component| matches!(component, std::path::Component::ParentDir))
    {
        return Err("the path contains '..'".into());
    }
    Ok(())
}

fn check_grant(value: &str) -> Result<(), String> {
    crate::grants::Grant::parse(value).map(|_| ())
}

fn check_header_name(value: &str) -> Result<(), String> {
    if value.is_empty()
        || value.len() > 64
        || !value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-')
    {
        return Err("not a header name".into());
    }
    if value.eq_ignore_ascii_case("authorization") || value.eq_ignore_ascii_case("cookie") {
        return Err("the header must not be Authorization or Cookie".into());
    }
    Ok(())
}

fn check_issuer(value: &str) -> Result<(), String> {
    if value.is_empty() || value.len() > 256 || !value.bytes().all(|b| b.is_ascii_graphic()) {
        return Err("not 1 to 256 visible ASCII characters".into());
    }
    Ok(())
}

fn check_base_url(value: &str) -> Result<(), String> {
    let rest = value
        .strip_prefix("http://")
        .or_else(|| value.strip_prefix("https://"))
        .ok_or_else(|| "the URL does not start with http:// or https://".to_string())?;
    let authority = rest.strip_suffix('/').unwrap_or(rest);
    if authority.is_empty()
        || value.len() > 256
        || !authority
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b':' | b'[' | b']'))
    {
        return Err("the URL has a path, a query, a fragment or a bad character".into());
    }
    Ok(())
}

fn check_host(value: &str) -> Result<(), String> {
    if value.is_empty()
        || value.len() > 253
        || !value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_' | b':'))
    {
        return Err("not a host name or an address".into());
    }
    Ok(())
}

fn check_postgres_name(value: &str) -> Result<(), String> {
    if value.is_empty()
        || value.len() > 63
        || !value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_'))
    {
        return Err("bad length or bad character".into());
    }
    Ok(())
}

fn check_cookie_name(value: &str) -> Result<(), String> {
    if value.is_empty()
        || !value.bytes().all(|b| {
            b.is_ascii_alphanumeric()
                || matches!(
                    b,
                    b'!' | b'#'
                        | b'$'
                        | b'%'
                        | b'&'
                        | b'\''
                        | b'*'
                        | b'+'
                        | b'-'
                        | b'.'
                        | b'^'
                        | b'_'
                        | b'`'
                        | b'|'
                        | b'~'
                )
        })
    {
        return Err("the name contains a character that a cookie name cannot hold".into());
    }
    if !value.starts_with("__Host-") {
        return Err("the name does not start with __Host-".into());
    }
    Ok(())
}

fn check_metrics_path(path: &str) -> Result<(), String> {
    if path.is_empty() || path.len() > 128 || !path.starts_with('/') {
        return Err("not an absolute path of at most 128 bytes".into());
    }
    if path.contains('?') || path.contains('#') || path.bytes().any(|b| b <= 0x20 || b == 0x7f) {
        return Err("the path holds a query, a fragment, a space or a control character".into());
    }
    if path == "/healthz" || path.starts_with("/auth/") {
        return Err("the path overlaps /healthz or the reserved /auth/ namespace".into());
    }
    Ok(())
}

/// A valid name of an environment variable: a letter or `_`, then letters, digits or `_`.
pub fn check_env_name(value: &str) -> Result<(), String> {
    let mut bytes = value.bytes();
    let first_ok = bytes
        .next()
        .is_some_and(|b| b.is_ascii_alphabetic() || b == b'_');
    if !first_ok || value.len() > 128 || !bytes.all(|b| b.is_ascii_alphanumeric() || b == b'_') {
        return Err("not a valid variable name".into());
    }
    Ok(())
}

// Units.

/// A duration: digits, then `ms`, `s`, `m`, `h` or `d`. A unit is mandatory.
pub fn parse_duration(value: &str) -> Result<Duration, String> {
    let split = value
        .find(|c: char| !c.is_ascii_digit())
        .ok_or_else(|| "the unit is missing".to_string())?;
    let (digits, unit) = value.split_at(split);
    if digits.is_empty() {
        return Err("the number is missing".into());
    }
    let n = digits.parse::<u64>().map_err(|e| e.to_string())?;
    let duration = match unit {
        "ms" => Some(Duration::from_millis(n)),
        "s" => Some(Duration::from_secs(n)),
        "m" => n.checked_mul(MINUTE).map(Duration::from_secs),
        "h" => n.checked_mul(HOUR).map(Duration::from_secs),
        "d" => n.checked_mul(DAY).map(Duration::from_secs),
        _ => return Err(format!("unknown unit {unit:?}")),
    };
    duration.ok_or_else(|| "the duration is too large".to_string())
}

/// A size: digits, then `B`, `KiB` or `MiB`. A unit is mandatory.
pub fn parse_size(value: &str) -> Result<usize, String> {
    let split = value
        .find(|c: char| !c.is_ascii_digit())
        .ok_or_else(|| "the unit is missing".to_string())?;
    let (digits, unit) = value.split_at(split);
    if digits.is_empty() {
        return Err("the number is missing".into());
    }
    let n = digits.parse::<usize>().map_err(|e| e.to_string())?;
    let multiplier = match unit {
        "B" => 1usize,
        "KiB" => 1024,
        "MiB" => 1024 * 1024,
        _ => return Err(format!("unknown unit {unit:?}")),
    };
    n.checked_mul(multiplier)
        .ok_or_else(|| "the size is too large".to_string())
}

/// The shortest exact form of a duration.
pub fn show_duration(value: Duration) -> String {
    let ms = value.as_millis();
    if ms % 1000 != 0 {
        return format!("{ms}ms");
    }
    let s = value.as_secs();
    for (unit, size) in [("d", DAY), ("h", HOUR), ("m", MINUTE)] {
        if s != 0 && s % size == 0 {
            return format!("{}{unit}", s / size);
        }
    }
    format!("{s}s")
}

/// The shortest exact form of a size.
pub fn show_size(value: usize) -> String {
    for (unit, size) in [("MiB", 1024 * 1024), ("KiB", 1024)] {
        if value != 0 && value % size == 0 {
            return format!("{}{unit}", value / size);
        }
    }
    format!("{value}B")
}

// The reference.

fn type_cell(key: &Key) -> String {
    match &key.kind {
        Kind::Text { expected, .. } => format!("text: {expected}"),
        Kind::Int { min, max } => format!("number, {min} to {max}"),
        Kind::Bool => "`true` or `false`".to_string(),
        Kind::Duration { min, max } => {
            format!(
                "duration, {} to {}",
                show_duration(*min),
                show_duration(*max)
            )
        }
        Kind::Size { min, max } => format!("size, {} to {}", show_size(*min), show_size(*max)),
        Kind::Enum(values) => values
            .iter()
            .map(|v| format!("`{v}`"))
            .collect::<Vec<_>>()
            .join(", "),
        Kind::List {
            expected,
            max_items,
            ..
        } => {
            format!("list of at most {max_items} quoted strings: {expected}")
        }
        Kind::Secret { .. } => format!(
            "secret: give `{0}_env` (a variable name) or `{0}_file` (an absolute path), not both",
            key.name
        ),
    }
}

fn default_cell(key: &Key) -> String {
    match key.presence {
        Presence::Required => "required".to_string(),
        Presence::Optional => "none".to_string(),
        Presence::Default(value) => match key.kind {
            Kind::Int { .. } | Kind::Bool => format!("`{value}`"),
            _ => format!("`\"{value}\"`"),
        },
    }
}

fn escape_cell(text: &str) -> String {
    text.replace('|', "\\|")
}

/// The configuration reference in Markdown, generated from the table. The README holds a copy
/// that a test compares with this text.
pub fn reference() -> String {
    let mut out = String::new();
    for (block, note) in BLOCKS {
        out.push_str(&format!("#### Block `{block}`\n\n{note}\n\n"));
        out.push_str("| Key | Type | Default | Description |\n|---|---|---|---|\n");
        for key in KEYS.iter().filter(|k| k.block == *block) {
            let mut description = key.doc.to_string();
            let sentence = key.when.sentence();
            if !sentence.is_empty() {
                description.push(' ');
                description.push_str(sentence);
            }
            out.push_str(&format!(
                "| `{}` | {} | {} | {} |\n",
                key.name,
                escape_cell(&type_cell(key)),
                default_cell(key),
                escape_cell(&description)
            ));
        }
        out.push('\n');
    }
    for (block, label, note) in LABELED_BLOCKS {
        out.push_str(&format!(
            "#### Block `{block} \"{label}\"`\n\nThis block has a label and can occur more than one time. {note}\n\n"
        ));
        out.push_str("| Key | Type | Default | Description |\n|---|---|---|---|\n");
        for key in LABELED_KEYS.iter().filter(|k| k.block == *block) {
            out.push_str(&format!(
                "| `{}` | {} | {} | {} |\n",
                key.name,
                escape_cell(&type_cell(key)),
                default_cell(key),
                escape_cell(key.doc)
            ));
        }
        out.push('\n');
    }
    out.truncate(out.trim_end().len());
    out.push('\n');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_table_has_no_duplicate_key_and_only_known_blocks() {
        let blocks = block_names();
        for (i, key) in KEYS.iter().enumerate() {
            assert!(blocks.contains(&key.block), "{}", key.path());
            assert!(
                KEYS[i + 1..]
                    .iter()
                    .all(|other| (other.block, other.name) != (key.block, key.name)),
                "duplicate {}",
                key.path()
            );
            assert!(!key.doc.is_empty(), "{} has no description", key.path());
        }
        for block in &blocks {
            assert!(
                KEYS.iter().any(|k| k.block == *block),
                "empty block {block}"
            );
        }
        let labeled = labeled_block_names();
        for key in LABELED_KEYS {
            assert!(labeled.contains(&key.block), "{}", key.path());
            assert!(!blocks.contains(&key.block), "{} has a label", key.block);
        }
    }

    #[test]
    fn units_are_strict() {
        assert_eq!(parse_size("8KiB").unwrap(), 8192);
        assert_eq!(parse_size("8192B").unwrap(), 8192);
        assert!(parse_size("8192").is_err(), "a size needs a unit");
        assert!(parse_size("8kib").is_err());
        assert!(parse_size("KiB").is_err());
        assert_eq!(parse_duration("5m").unwrap(), Duration::from_secs(300));
        assert_eq!(parse_duration("200ms").unwrap(), Duration::from_millis(200));
        assert!(parse_duration("5").is_err(), "a duration needs a unit");
        assert!(parse_duration("5 m").is_err());
        assert!(parse_duration("5M").is_err());
        assert!(parse_duration("m").is_err());
    }

    #[test]
    fn durations_and_sizes_are_shown_in_their_shortest_form() {
        assert_eq!(show_duration(Duration::from_millis(200)), "200ms");
        assert_eq!(show_duration(Duration::from_secs(60)), "1m");
        assert_eq!(show_duration(Duration::from_secs(7 * DAY)), "7d");
        assert_eq!(show_duration(Duration::from_secs(90)), "90s");
        assert_eq!(show_size(1024 * 1024), "1MiB");
        assert_eq!(show_size(8 * 1024), "8KiB");
        assert_eq!(show_size(100), "100B");
    }

    #[test]
    fn the_reference_lists_every_key() {
        let reference = reference();
        for key in KEYS.iter().chain(LABELED_KEYS) {
            assert!(
                reference.contains(&format!("| `{}` |", key.name)),
                "{}",
                key.path()
            );
        }
    }
}
