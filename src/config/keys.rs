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
    /// A list of grants. An item is a quoted grant, or an object with the members `grant` (the
    /// quoted grant) and `limits` (an object of limit values). `expected` describes an item.
    Grants {
        expected: &'static str,
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
    /// The block `postgres` is present. The key is in another block.
    WithPostgres,
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
    /// The block `keys` is present.
    KeysBlock,
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
            | When::KeysBlock
            | When::LabeledBlock => "the key always applies",
            When::NoPostgres => "the postgres block is present",
            When::WithPostgres => "the postgres block is absent",
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
            | When::KeysBlock
            | When::LabeledBlock => "",
            When::NoPostgres => "Applies only without the `postgres` block.",
            When::WithPostgres => "Applies only with the `postgres` block.",
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
            Kind::Grants {
                expected,
                max_items,
            } => format!(
                "a list of at most {max_items} grants. Each item is a quoted grant or an object with grant and limits: {expected}"
            ),
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
    (
        "keys",
        "This block is optional. If it is present, the server accepts keys: a user makes keys in the tab Access. A key has one kind: `mcp` for an agent, verified at the key edge and by the PDP, and `api` for a service, verified by the API of the inbox. The keys `mcp` and `api` turn each kind on or off. The kind `mcp` needs the blocks `identity` and `pdp`. The kind `api` needs the block `inbox`. The block needs one kind on at least.",
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
    (
        "inbox",
        "This block is optional. If it is present, this server runs the inbox: it sends alerts to phones. The inbox uses the database of the portal. The blocks `inbox.devices` and `inbox.storage` are mandatory inside this block. The blocks `inbox.replica`, `inbox.cluster` and `inbox.watch` are optional. The blocks `group \"<id>\"` and `api_key \"<name>\"` are inside this block.",
    ),
    (
        "inbox.devices",
        "This block is inside the block `inbox`. It sets the public address that phones use, the limits of the phones, and the delivery values. The phones use the listener of the portal, as the senders do. The routes of the phones have no session: phones prove themselves.",
    ),
    (
        "inbox.enrollment",
        "This block is optional and is inside the block `inbox`. It sets the enrollment codes: an account of the portal makes a code, and a phone uses it one time to enroll. Every key has a default.",
    ),
    (
        "inbox.storage",
        "This block is inside the block `inbox`. It sets the lifetime of alerts and of the connection log. The inbox keeps its tables in the database of the portal: the SQLite file of the block `storage`, or the database of the block `postgres`.",
    ),
    (
        "inbox.replica",
        "This block is optional and is inside the block `inbox`. If it is present, the inbox copies the SQLite file to a directory that the standby instances share. A standby instance takes over when the owner is lost. The block needs the SQLite file: it must not be present with the block `postgres`. The copy covers the whole file, with the tables of the portal.",
    ),
    (
        "inbox.cluster",
        "This block is optional and is inside the block `inbox`. If it is present, the instances copy to each other the alerts that they accept while PostgreSQL has no writable primary. The block needs the block `postgres`.",
    ),
    (
        "inbox.watch",
        "This block is optional and is inside the block `inbox`. If it is present, the portal sends alerts about itself to one group: when a service of the launcher is unhealthy, and when sign-ins fail in a burst. Without this block, the portal sends no alert about itself.",
    ),
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
        name: "name",
        kind: Kind::Text {
            expected: "1 to 64 bytes, not blank, no control character",
            check: check_server_name,
        },
        presence: Presence::Default("Portal"),
        when: When::Always,
        doc: "The name of this instance. The phones show it as the name of the server, and the welcome message uses it.",
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
        doc: "The largest number of open TCP connections of the pages and the API. A connection that upgrades to a phone session gives its permit back and takes a permit of the session budget: `server.max_connections` plus two times `inbox.devices.max_devices`. This budget exists only with the block `inbox`.",
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
        doc: "The networks of the proxies (Traefik) that set `X-Forwarded-For`. The server reads this header only from a TCP peer in these networks. It then takes the first address from the right that is not in these networks. From any other peer, the client address is the address of the peer. `[]` trusts no proxy. The client address keys the session binding, the login limiter and, for the phones, the limiter of the enrollment attempts (the routes of the phones use this list too).",
    },
    Key {
        block: "authentication",
        name: "superadmin_login",
        kind: Kind::Secret { max_bytes: 256 },
        presence: Presence::Required,
        when: When::Always,
        doc: "The login of the superadmin. It has 1 to 64 characters, and no upper-case letter, no space, no control character and no colon. The portal keeps the colon for the ids of keys. The server does not change it. A new login ends the sessions of the superadmin and the impersonated sessions.",
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
        block: "users",
        name: "allow_creation",
        kind: Kind::Bool,
        presence: Presence::Default("true"),
        when: When::Always,
        doc: "`true`: the superadmin can make accounts in the database. `false`: the route that makes an account answers `404`, and the pages show no button and no entry that makes an account. The accounts that exist stay editable, as `access.grants_mode` says. The value `true` contradicts `access.grants_mode = \"static\"`: in this mode the database holds no account.",
    },
    Key {
        block: "users",
        name: "max_grants",
        kind: Kind::Int { min: 1, max: 30_000 },
        presence: Presence::Default("512"),
        when: When::Always,
        doc: "The largest number of grants of one user. A key holds at most 512 grants, at any value of this key. A user with 3 grants for each service and the 8 actions of the service `portal` needs 3 x the number of services + 8 grants.",
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
        doc: "The time that the server waits for the `/.well-known/procurauth/actions` document of one service.",
    },
    Key {
        block: "access",
        name: "actions_refresh",
        kind: Kind::Duration { min: secs(10), max: secs(DAY) },
        presence: Presence::Default("5m"),
        when: When::Always,
        doc: "The time between two reads of the `/.well-known/procurauth/actions` documents. It is also the shortest time between two early reads of the document of one service, after an `invalid_resource` of the PDP.",
    },
    Key {
        block: "access",
        name: "actions_max_age",
        kind: Kind::Duration { min: secs(MINUTE), max: secs(7 * DAY) },
        presence: Presence::Default("1h"),
        when: When::Always,
        doc: "The longest time that the server keeps the last good document of a service after its last good read. After this time, the server acts as if the service had no good document. The edge also refuses each key for this service. The value must be more than `access.actions_refresh`.",
    },
    Key {
        block: "access",
        name: "max_actions",
        kind: Kind::Int { min: 64, max: 4096 },
        presence: Presence::Default("1024"),
        when: When::Always,
        doc: "The largest number of actions in the document of one service. A larger document is bad. Protocol 5.3c asks a reader to accept 64 at least.",
    },
    Key {
        block: "access",
        name: "max_resources",
        kind: Kind::Int { min: 256, max: 65_536 },
        presence: Presence::Default("4096"),
        when: When::Always,
        doc: "The largest number of resources in the document of one service. A larger document is bad. Protocol 5.3c asks a reader to accept 256 at least.",
    },
    Key {
        block: "access",
        name: "max_document_bytes",
        kind: Kind::Int { min: 64 * 1024, max: 4 * 1024 * 1024 },
        presence: Presence::Default("262144"),
        when: When::Always,
        doc: "The largest size, in bytes, of the document of one service. The server stops the read at this size, and the document is bad. Protocol 5.3c asks a reader to accept 64 KiB at least. The server keeps the last good document of each service in memory.",
    },
    Key {
        block: "access",
        name: "start_read_deadline",
        kind: Kind::Duration { min: secs(1), max: secs(300) },
        presence: Presence::Default("5s"),
        when: When::Always,
        doc: "The longest time that the server waits, at start, for the first read of the documents of the services. After this time, the server opens its ports and reads the other documents in the background.",
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
        doc: "The Ed25519 private key that signs the identity tokens (algorithm `Ed25519`): a PKCS#8 key in PEM, or the base64 text of the PEM on one line. `portal identity-public-key` prints the public key for the services, as a JWK Set.",
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
        block: "pdp",
        name: "plain_http",
        kind: Kind::Bool,
        presence: Presence::Default("false"),
        when: When::PdpBlock,
        doc: "Set it to `true` to accept an `http` value of `pdp.url`. Then the operator states that other means secure the link to the PDP. With `false`, an `http` URL stops the server. With `true`, an `https` URL also stops the server. With `true`, the start log has a warning.",
    },
    Key {
        block: "pdp",
        name: "counter_connections",
        kind: Kind::Int { min: 1, max: 64 },
        presence: Presence::Default("2"),
        when: When::PdpBlock,
        doc: "The number of database connections that count the uses of the budgets. Each connection counts approximately 1,000 uses each second. A use that waits more than 500 ms gets the HTTP status 503.",
    },
    Key {
        block: "keys",
        name: "mcp",
        kind: Kind::Bool,
        presence: Presence::Default("true"),
        when: When::KeysBlock,
        doc: "`true`: the kind `mcp` is on. A user can make MCP keys, with the prefix `mcp_`, and the key edge and the PDP accept them. `false`: the kind is off. The key edge refuses these keys, and the pages show no MCP key, no MCP right and no MCP chip. The value `true` needs the blocks `identity` and `pdp`.",
    },
    Key {
        block: "keys",
        name: "api",
        kind: Kind::Bool,
        presence: Presence::Default("false"),
        when: When::KeysBlock,
        doc: "`true`: the kind `api` is on. A user can make API keys, with the prefix `api_`, and the API of the inbox accepts them. `false`: the kind is off. The API of the inbox refuses these keys, and the pages show no API key, no API right and no API chip. The value `true` needs the block `inbox`. A block `api_key` needs the value `true`.",
    },
    Key {
        block: "keys",
        name: "default_lifetime",
        kind: Kind::Duration { min: secs(DAY), max: secs(365 * DAY) },
        presence: Presence::Default("30d"),
        when: When::KeysBlock,
        doc: "The lifetime that the form of a new key proposes. The form uses the lower of this value and the longest lifetime of the owner (`max_key_lifetime`).",
    },
    Key {
        block: "keys",
        name: "directory_max_age",
        kind: Kind::Duration { min: secs(15), max: secs(300) },
        presence: Presence::Default("30s"),
        when: When::KeysBlock,
        doc: "The longest age of the directory of the users and the keys. When the last good read of the database is older, the PDP answers `false` for each key and the edge refuses each key. The directory is read again every 5 seconds, so the minimum is 3 rounds.",
    },
    Key {
        block: "keys",
        name: "max_per_user",
        kind: Kind::Int { min: 1, max: 10_000 },
        presence: Presence::Default("100"),
        when: When::KeysBlock,
        doc: "The largest number of active keys of one user. An expired key and a key that is too old do not count.",
    },
    Key {
        block: "keys",
        name: "max_counter_rows",
        kind: Kind::Int { min: 1, max: 100_000_000 },
        presence: Presence::Default("2000000"),
        when: When::KeysBlock,
        doc: "The largest number of counter rows. The server refuses a write that makes more rows. A file that needs more rows stops the server at start. One row is approximately 300 bytes.",
    },
    Key {
        block: "keys",
        name: "max_directory_grants",
        kind: Kind::Int { min: 1, max: 100_000_000 },
        presence: Presence::Default("4000000"),
        when: When::KeysBlock,
        doc: "The largest number of grants of the directory: the grants of the users and the keys. The server refuses a write that makes more grants. A file that needs more grants stops the server at start. One grant takes approximately 200 bytes of memory.",
    },
    Key {
        block: "keys",
        name: "history_days",
        kind: Kind::Int { min: 1, max: 3650 },
        presence: Presence::Default("90"),
        when: When::KeysBlock,
        doc: "The number of days that the server keeps the summary row of a revoked key.",
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
            expected: "an absolute URL path of at most 128 bytes, not /healthz, /readyz or /api/version, and not under /auth/, /v1/ or /api/v1/",
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
    Key {
        block: "inbox",
        name: "server_key",
        kind: Kind::Secret { max_bytes: 4096 },
        presence: Presence::Required,
        when: When::Always,
        doc: "The key of the server for the phones. Make it one time with `portal inbox keygen`. A new key makes each enrolled phone and each enrollment code invalid. Back it up.",
    },
    Key {
        block: "inbox.devices",
        name: "public_address",
        kind: Kind::Text {
            expected: "the public base URL of the portal as the phones reach it: `host:port`, a domain, or an http(s) URL with an optional path prefix; 1 to 512 bytes, no white space, no control character",
            check: check_public_address,
        },
        presence: Presence::Required,
        when: When::Always,
        doc: "The public base URL of the portal, as the phones reach it from outside: `host:port`, a domain, or an http(s) URL `http(s)://host[:port][/prefix]`, for example `http://172.17.0.1:8088`. The phones add `/v1/enroll` and `/v1/session` to it. The portal serves these routes on `server.listen_address`, with the pages. A proxy that publishes the portal under a prefix must remove the prefix before it sends the request: the portal has no base path. It is the `s` of the enrollment code. The inbox does not open a connection to it.",
    },
    Key {
        block: "inbox.devices",
        name: "max_devices",
        kind: Kind::Int { min: 1, max: 100_000 },
        presence: Presence::Default("1000"),
        when: When::Always,
        doc: "The largest number of enrolled phones that are not revoked.",
    },
    Key {
        block: "inbox.devices",
        name: "heartbeat_interval",
        kind: Kind::Duration { min: secs(5), max: secs(HOUR) },
        presence: Presence::Required,
        when: When::Always,
        doc: "The app sends a ping at this interval.",
    },
    Key {
        block: "inbox.devices",
        name: "session_timeout",
        kind: Kind::Duration { min: secs(10), max: secs(DAY) },
        presence: Presence::Required,
        when: When::Always,
        doc: "A session without a frame for this time ends. The value is at least two times `inbox.devices.heartbeat_interval`.",
    },
    Key {
        block: "inbox.devices",
        name: "resend_after",
        kind: Kind::Duration { min: secs(1), max: secs(DAY) },
        presence: Presence::Required,
        when: When::Always,
        doc: "The inbox sends an alert again to a phone that did not confirm the receipt after this time.",
    },
    Key {
        block: "inbox.devices",
        name: "max_inflight",
        kind: Kind::Int { min: 1, max: 1024 },
        presence: Presence::Required,
        when: When::Always,
        doc: "The largest number of alerts that the inbox sends to one session before the phone confirms them.",
    },
    Key {
        block: "inbox.devices",
        name: "unreachable_after",
        kind: Kind::Duration { min: secs(10), max: secs(30 * DAY) },
        presence: Presence::Required,
        when: When::Always,
        doc: "The inbox reports a subscribed phone as unreachable when it did not see the phone for this time. The value is at least `inbox.devices.session_timeout`.",
    },
    Key {
        block: "inbox.devices",
        name: "offline_warning_after",
        kind: Kind::Duration { min: secs(10), max: secs(30 * DAY) },
        presence: Presence::Required,
        when: When::Always,
        doc: "The app warns its user after it was disconnected for this time. The value is at least two times `inbox.devices.heartbeat_interval`.",
    },
    Key {
        block: "inbox.devices",
        name: "max_auth_failures",
        kind: Kind::Int { min: 1, max: 1000 },
        presence: Presence::Default("5"),
        when: When::Always,
        doc: "The number of enrollment attempts of one client address in the window `inbox.devices.auth_failure_window`. Each started enrollment counts until it succeeds.",
    },
    Key {
        block: "inbox.devices",
        name: "auth_failure_window",
        kind: Kind::Duration { min: secs(1), max: secs(DAY) },
        presence: Presence::Default("10m"),
        when: When::Always,
        doc: "The window of `inbox.devices.max_auth_failures`.",
    },
    Key {
        block: "inbox.devices",
        name: "max_request_bytes",
        kind: Kind::Size { min: 4096, max: 1024 * 1024 },
        presence: Presence::Default("64KiB"),
        when: When::Always,
        doc: "The largest request body that the listener reads.",
    },
    Key {
        block: "inbox.enrollment",
        name: "code_ttl",
        kind: Kind::Duration { min: secs(MINUTE), max: secs(DAY) },
        presence: Presence::Default("10m"),
        when: When::Always,
        doc: "An enrollment code works for this time after an account made it. A code works one time.",
    },
    Key {
        block: "inbox.enrollment",
        name: "max_active_per_account",
        kind: Kind::Int { min: 1, max: 100 },
        presence: Presence::Default("5"),
        when: When::Always,
        doc: "The largest number of enrollment codes of one account that are not used and not expired. The inbox refuses a new code above this number.",
    },
    Key {
        block: "inbox.storage",
        name: "delivery_ttl",
        kind: Kind::Duration { min: secs(MINUTE), max: secs(3650 * DAY) },
        presence: Presence::Required,
        when: When::Always,
        doc: "The inbox does not send an alert again to a phone that did not receive it within this time.",
    },
    Key {
        block: "inbox.storage",
        name: "alert_retention",
        kind: Kind::Duration { min: secs(HOUR), max: secs(3650 * DAY) },
        presence: Presence::Required,
        when: When::Always,
        doc: "The inbox deletes the alerts, delivered or expired, that are older than this. The value is at least `inbox.storage.delivery_ttl`.",
    },
    Key {
        block: "inbox.storage",
        name: "connection_log_retention",
        kind: Kind::Duration { min: secs(HOUR), max: secs(3650 * DAY) },
        presence: Presence::Required,
        when: When::Always,
        doc: "The inbox deletes the connection records that are older than this.",
    },
    Key {
        block: "inbox.storage",
        name: "pool_size",
        kind: Kind::Int { min: 2, max: 64 },
        presence: Presence::Default("8"),
        when: When::WithPostgres,
        doc: "The number of connections that the inbox uses for requests and sessions. The inbox keeps one more connection for the notifications of the other instances.",
    },
    Key {
        block: "inbox.replica",
        name: "dir",
        kind: Kind::Text {
            expected: "an absolute path without '..'",
            check: check_absolute_path,
        },
        presence: Presence::Required,
        when: When::Always,
        doc: "The directory that every instance that can run the inbox shares. It is a volume of plain files. It must not be the directory of `storage.database`.",
    },
    Key {
        block: "inbox.replica",
        name: "snapshot_interval",
        kind: Kind::Duration { min: secs(1), max: secs(DAY) },
        presence: Presence::Default("10s"),
        when: When::Always,
        doc: "The inbox copies a changed database at most this often. An accepted alert, an enrollment and a revocation do not wait for this time.",
    },
    Key {
        block: "inbox.replica",
        name: "lease_ttl",
        kind: Kind::Duration { min: secs(5), max: secs(HOUR) },
        presence: Presence::Default("30s"),
        when: When::Always,
        doc: "The other instances consider an instance lost when its lease did not change for this time. They then take over.",
    },
    Key {
        block: "inbox.replica",
        name: "sync_timeout",
        kind: Kind::Duration { min: secs(1), max: secs(MINUTE) },
        presence: Presence::Default("5s"),
        when: When::Always,
        doc: "The time that an accepted alert waits for the copy that holds it.",
    },
    Key {
        block: "inbox.replica",
        name: "sync_alerts",
        kind: Kind::Bool,
        presence: Presence::Default("true"),
        when: When::Always,
        doc: "`true`: the inbox confirms an alert only after the copy holds it. If the copy fails, the inbox answers `503` and the sender sends the alert again. `false`: the inbox confirms an alert after the local commit and copies it as soon as possible.",
    },
    Key {
        block: "inbox.cluster",
        name: "listen",
        kind: Kind::Text {
            expected: "an IP address and a port, such as 0.0.0.0:8082",
            check: check_socket_address,
        },
        presence: Presence::Required,
        when: When::Always,
        doc: "The address and the port where this instance receives the requests of the other instances. Only the pods of the service can reach this port.",
    },
    Key {
        block: "inbox.cluster",
        name: "peers",
        kind: Kind::List {
            expected: "names with a port such as \"inbox-peers.apps.svc.cluster.local:8082\"",
            check: check_peer_address,
            max_items: 16,
        },
        presence: Presence::Required,
        when: When::Always,
        doc: "The names that list the instances, for example a headless Service, with the port of `inbox.cluster.listen`. The inbox asks each address that the names resolve to who it is. It skips its own address. The list has at least one name.",
    },
    Key {
        block: "inbox.watch",
        name: "group",
        kind: Kind::Text {
            expected: "the id of a block `group` of the block `inbox`",
            check: check_inbox_id,
        },
        presence: Presence::Required,
        when: When::Always,
        doc: "The group that receives the alerts of the portal. It must be the label of a block `group` of the block `inbox`.",
    },
    Key {
        block: "inbox.watch",
        name: "service_unhealthy",
        kind: Kind::Bool,
        presence: Presence::Default("true"),
        when: When::Always,
        doc: "`true`: the portal sends an alert when a service of the launcher stays unhealthy for `inbox.watch.unhealthy_after`. A service is unhealthy when its tile shows `down` or `unhealthy`. A service with no health check is never unhealthy.",
    },
    Key {
        block: "inbox.watch",
        name: "unhealthy_after",
        kind: Kind::Duration { min: secs(10), max: secs(DAY) },
        presence: Presence::Default("2m"),
        when: When::Always,
        doc: "How long a service stays unhealthy before the alert. A service that is healthy or starting in this time does not raise the alert.",
    },
    Key {
        block: "inbox.watch",
        name: "sign_in_burst",
        kind: Kind::Bool,
        presence: Presence::Default("true"),
        when: When::Always,
        doc: "`true`: the portal sends an alert when `inbox.watch.sign_in_failures` sign-ins are refused in the window `inbox.watch.sign_in_window`. A wrong password, an unknown login and a sign-in that a limiter blocks count. Each instance counts the refusals that it sees.",
    },
    Key {
        block: "inbox.watch",
        name: "sign_in_failures",
        kind: Kind::Int { min: 2, max: 10000 },
        presence: Presence::Default("10"),
        when: When::Always,
        doc: "The number of refused sign-ins in the window that raises the alert.",
    },
    Key {
        block: "inbox.watch",
        name: "sign_in_window",
        kind: Kind::Duration { min: secs(10), max: secs(DAY) },
        presence: Presence::Default("5m"),
        when: When::Always,
        doc: "The window of `inbox.watch.sign_in_failures`. It slides: the portal counts the refusals of the last window.",
    },
    Key {
        block: "inbox.watch",
        name: "repeat_after",
        kind: Kind::Duration { min: secs(MINUTE), max: secs(7 * DAY) },
        presence: Presence::Default("1h"),
        when: When::Always,
        doc: "The shortest time between two alerts of the same kind and the same subject. A service that stays unhealthy raises the alert again after this time. A burst that continues does the same. The alerts of this time have one shared identity, so two instances send one alert.",
    },
    Key {
        block: "inbox.watch",
        name: "recovered",
        kind: Kind::Bool,
        presence: Presence::Default("true"),
        when: When::Always,
        doc: "`true`: the portal sends a short alert when a service is healthy again after the portal sent an alert about it.",
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
        "key",
        "<name>",
        "One key of a user of the file. The block is inside a block `user`. The label is the name of the key: 1 to 64 characters, with no control character. Two keys of one user have two names. The subject id of the key is `key:` and the first 16 hexadecimal characters of the SHA-256 digest of the login, a zero byte and the name. A key of the file has no expiry date. To stop it, remove the block and restart each instance. The block needs the block `keys`.",
    ),
    (
        "group",
        "<id>",
        "One group of alerts. The block is inside the block `inbox`. The label is the id of the group: 1 to 64 characters of a-z, 0-9, '-' and '_'. A phone subscribes to a group. A key sends an alert to a group.",
    ),
    (
        "api_key",
        "<name>",
        "One API key of the file: the key of a monitoring tool, a script or a webhook that sends alerts. The block is inside the block `inbox`. The label is the name of the key: 1 to 64 characters of a-z, 0-9, '-' and '_'. The key follows its owner, as each key does: it can do only what the key and its owner both have the right to do. The owner is an account of the file (a block `user`), so the key and its account have one source. The rights of a file account do not change while the server runs, so the server decides at start which groups the key can send to. This decision does not wait for the database. The block needs `keys.api = true`. The key is not in the list of keys.",
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
        kind: Kind::Grants {
            expected: "a grant such as \"access on service:grafana\" or \"query.run.write on service:clickhouse-dash/host:local/database:analytics\": an action, the word on, service: and the id of the service, then 0 to 4 segments /type:id with each id percent-encoded. To give limit values, write { grant = \"access on service:datastore\", limits = { requests = [ { amount = 300, window = \"minute\" } ] } }",
            max_items: 30_000,
        },
        presence: Presence::Required,
        when: When::LabeledBlock,
        doc: "The grants of the user. The user is an account of the file, with no row in the database. `access` lets the user open the service. The other actions are the names that the service publishes at `/.well-known/procurauth/actions`. A grant names a node of the resource tree of the service: `<action> on service:<id>`, then 0 to 4 segments `/<type>:<id>`, for example `query.run.read on service:clickhouse-dash/host:local/database:analytics`. A grant covers its node and each node below it. A type starts with a-z, then has a-z, 0-9 and '_', 1 to 32 characters, and is not `service`. Each id is percent-encoded: each byte outside A-Z a-z 0-9 - . _ ~ is %XX with upper-case hex digits, for example `bucket:a%2Fb` for the id `a/b`. Another encoding of the same id stops the start. A decoded id has 1 to 256 bytes, without a control character. The action `access` has grants on the service only. The `limits` object holds one member for each limit. A ceiling is a whole number. A budget is a list of objects with `amount` and `window` (`second`, `minute`, `hour` or `day`). A budget value is only on the grant `access` on a service. A grant has at most 8 limit values. A value is at most 1,000,000,000. The limit values take at most 1024 bytes as compact JSON. The number of grants is at most `users.max_grants`. At start, the server checks only the form of each grant: the services and their trees of types are known at run time. The Services page of the settings shows a grant that does not follow the tree of its service. The PDP does not apply this grant. A block `user` stops the server when it gives `keys.own.create`, or an action that needs another action that it does not give.",
    },
    Key {
        block: "user",
        name: "password_hash",
        kind: Kind::Secret { max_bytes: 512 },
        presence: Presence::Optional,
        when: When::LabeledBlock,
        doc: "The Argon2id digest of the password of the user, in the PHC string form, as the database keeps it. Without it, the account opens no session, and only its keys act for it.",
    },
    Key {
        block: "key",
        name: "grants",
        kind: Kind::Grants {
            expected: "the same form as `user.grants`",
            max_items: 512,
        },
        presence: Presence::Required,
        when: When::LabeledBlock,
        doc: "The grants of the key, in the form of `user.grants`. A key holds at most 512 grants. A grant of an action of the service `portal` stops the server. The decision for a key never gives more than the grants of its owner.",
    },
    Key {
        block: "key",
        name: "key",
        kind: Kind::Secret { max_bytes: 64 },
        presence: Presence::Optional,
        when: When::LabeledBlock,
        doc: "The text of the key: `mcp_`, 43 characters and 6 characters of checksum. Give `key_env` or `key_file`, or give `key_sha256_env` or `key_sha256_file`: exactly one of the four. A bad form stops the server.",
    },
    Key {
        block: "key",
        name: "key_sha256",
        kind: Kind::Secret { max_bytes: 128 },
        presence: Presence::Optional,
        when: When::LabeledBlock,
        doc: "The SHA-256 digest of the text of the key: 64 hexadecimal characters, in upper or lower case. A digest that the database or another block also holds stops the server. The digest must be the digest of a text of the form `mcp_`, 43 characters and 6 characters of checksum: the server cannot check this at start.",
    },
    Key {
        block: "group",
        name: "name",
        kind: Kind::Text {
            expected: "1 to 128 bytes, not blank",
            check: check_group_name,
        },
        presence: Presence::Required,
        when: When::LabeledBlock,
        doc: "The name of the group that the phones show.",
    },
    Key {
        block: "group",
        name: "description",
        kind: Kind::Text {
            expected: "at most 512 bytes",
            check: check_group_description,
        },
        presence: Presence::Default(""),
        when: When::LabeledBlock,
        doc: "The description of the group.",
    },
    Key {
        block: "group",
        name: "critical",
        kind: Kind::Bool,
        presence: Presence::Default("false"),
        when: When::LabeledBlock,
        doc: "`true`: the group is critical. A limit on the action `alert.send` never applies to a critical group. `false`: the limit of a key applies to the group.",
    },
    Key {
        block: "api_key",
        name: "owner",
        kind: Kind::Text {
            expected: "the login of an account of the file (a block `user`)",
            check: check_login_text,
        },
        presence: Presence::Required,
        when: When::LabeledBlock,
        doc: "The login of the owner of the key. The owner must be an account of the file: a block `user`. The owner must hold `keys.own.use` on `service:portal/key_kind:api`, `alert.send` on each group of the key, and `alert.send_user` on `service:portal` when `direct_alerts` is `true`.",
    },
    Key {
        block: "api_key",
        name: "secret",
        kind: Kind::Secret { max_bytes: 64 },
        presence: Presence::Required,
        when: When::LabeledBlock,
        doc: "The text of the key: `api_`, 43 characters and 6 characters of checksum, 53 characters in all. Make it with `portal inbox token`. Two keys must not have the same text. A bad form stops the server.",
    },
    Key {
        block: "api_key",
        name: "groups",
        kind: Kind::List {
            expected: "ids of groups that the block `inbox` defines",
            check: check_inbox_id,
            max_items: 64,
        },
        presence: Presence::Required,
        when: When::LabeledBlock,
        doc: "The groups that the key can send alerts to. The list has at least one group, except when `direct_alerts` is `true`: then it can be empty. Each group must exist, and the owner must hold `alert.send` on it.",
    },
    Key {
        block: "api_key",
        name: "direct_alerts",
        kind: Kind::Bool,
        presence: Presence::Default("false"),
        when: When::LabeledBlock,
        doc: "`true`: the key can send an alert to any account (`POST /api/v1/alerts` with the field `user`). The owner must hold `alert.send_user` on `service:portal`. `false`: the key sends to groups only.",
    },
    Key {
        block: "api_key",
        name: "webhook_send_resolved",
        kind: Kind::Bool,
        presence: Presence::Default("false"),
        when: When::LabeledBlock,
        doc: "`true`: the notifications \"resolved\" of Alertmanager and Grafana become alerts. `false`: the inbox ignores them.",
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

/// The name of each block at the top of the file, for the closed list of the parser.
pub fn block_names() -> Vec<&'static str> {
    BLOCKS
        .iter()
        .map(|(name, _)| *name)
        .filter(|name| !name.contains('.'))
        .collect()
}

/// The name of each block of the table, with the blocks that other blocks hold.
#[cfg(test)]
pub fn all_block_names() -> Vec<&'static str> {
    BLOCKS.iter().map(|(name, _)| *name).collect()
}

/// The blocks with a label that a block with a label holds: `(parent, child)`.
pub const NESTED_BLOCKS: &[(&str, &str)] =
    &[("user", "key"), ("inbox", "group"), ("inbox", "api_key")];

/// The blocks without a label that a block without a label holds: `(parent, child)`. The keys of
/// the child are in the block `parent.child` of the table.
pub const NESTED_PLAIN_BLOCKS: &[(&str, &str)] = &[
    ("inbox", "devices"),
    ("inbox", "enrollment"),
    ("inbox", "storage"),
    ("inbox", "replica"),
    ("inbox", "cluster"),
    ("inbox", "watch"),
];

/// The name of each block with a label at the top of the file, for the closed list of the parser.
pub fn labeled_block_names() -> Vec<&'static str> {
    LABELED_BLOCKS
        .iter()
        .map(|(name, _, _)| *name)
        .filter(|name| !NESTED_BLOCKS.iter().any(|(_, child)| child == name))
        .collect()
}

/// The name of each block with a label, at the top of the file or inside a block.
#[cfg(test)]
pub fn all_labeled_block_names() -> Vec<&'static str> {
    LABELED_BLOCKS.iter().map(|(name, _, _)| *name).collect()
}

/// The block that must be in the file for the keys of `block` to apply. `None`: the keys of the
/// block always apply. When the gate block is absent, the keys do not apply and the file must
/// not have them.
pub fn gate(block: &str) -> Option<&'static str> {
    match block {
        "postgres" => Some("postgres"),
        "identity" => Some("identity"),
        "pdp" => Some("pdp"),
        "keys" => Some("keys"),
        "inbox" | "inbox.devices" | "inbox.enrollment" | "inbox.storage" => Some("inbox"),
        "inbox.replica" => Some("inbox.replica"),
        "inbox.cluster" => Some("inbox.cluster"),
        "inbox.watch" => Some("inbox.watch"),
        _ => None,
    }
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

pub fn check_grant(value: &str) -> Result<(), String> {
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

/// The public base URL of the portal, as the phones write it: `host:port`, a domain or an
/// http(s) URL with an optional path prefix. The rules are the rules of the app.
fn check_public_address(value: &str) -> Result<(), String> {
    if value.is_empty() || value.len() > 512 {
        return Err("not 1 to 512 bytes".into());
    }
    if value.chars().any(|c| c.is_control() || c.is_whitespace()) {
        return Err("it has white space or a control character".into());
    }
    let rest = match value.split_once("://") {
        Some((scheme, rest)) => {
            if !scheme.eq_ignore_ascii_case("http") && !scheme.eq_ignore_ascii_case("https") {
                return Err("the scheme is not http or https".into());
            }
            rest
        }
        None => value,
    };
    if rest.contains(['?', '#']) {
        return Err("it has a query or a fragment".into());
    }
    let authority = rest.split('/').next().unwrap_or_default();
    if authority.contains('@') {
        return Err("it has a user name".into());
    }
    let ipv6 = authority.starts_with('[');
    let (host, port) = if let Some(inner) = authority.strip_prefix('[') {
        let (host, after) = inner
            .split_once(']')
            .ok_or_else(|| "the IPv6 address has no closing bracket".to_string())?;
        if host.parse::<std::net::Ipv6Addr>().is_err() {
            return Err("not an IPv6 address".into());
        }
        if after.is_empty() {
            (host, None)
        } else {
            let port = after
                .strip_prefix(':')
                .ok_or_else(|| "there is text after the IPv6 address".to_string())?;
            (host, Some(port))
        }
    } else {
        match authority.split_once(':') {
            Some((host, port)) => (host, Some(port)),
            None => (authority, None),
        }
    };
    if host.is_empty()
        || host.len() > 253
        || (!ipv6
            && !host
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_')))
    {
        return Err("the host is empty or has a bad character".into());
    }
    if let Some(port) = port {
        if port.is_empty()
            || !port.bytes().all(|b| b.is_ascii_digit())
            || port.parse::<u16>().ok().is_none_or(|n| n == 0)
        {
            return Err("the port is not a number from 1 to 65535".into());
        }
    }
    Ok(())
}

fn check_server_name(value: &str) -> Result<(), String> {
    if value.trim().is_empty() || value.len() > 64 || value.chars().any(char::is_control) {
        return Err("not 1 to 64 bytes, or blank, or with a control character".into());
    }
    Ok(())
}

fn check_group_name(value: &str) -> Result<(), String> {
    if value.trim().is_empty() || value.len() > 128 || value.chars().any(char::is_control) {
        return Err("not 1 to 128 bytes, or blank, or with a control character".into());
    }
    Ok(())
}

fn check_group_description(value: &str) -> Result<(), String> {
    if value.len() > 512 || value.chars().any(char::is_control) {
        return Err("more than 512 bytes, or with a control character".into());
    }
    Ok(())
}

/// The id of a group and the name of an API key: 1 to 64 characters of a-z, 0-9, '-' and '_'.
pub fn check_inbox_id(value: &str) -> Result<(), String> {
    if value.is_empty()
        || value.len() > 64
        || !value
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_')
    {
        return Err("not 1 to 64 characters of a-z, 0-9, '-' and '_'".into());
    }
    Ok(())
}

/// The login of an account: 1 to 32 characters of a-z, 0-9, '.', '-' and '_'.
fn check_login_text(value: &str) -> Result<(), String> {
    match crate::users::normalize_login(value) {
        Ok(login) if login == value => Ok(()),
        _ => Err("not 1 to 32 characters of a-z, 0-9, '.', '-' and '_'".into()),
    }
}

fn check_peer_address(value: &str) -> Result<(), String> {
    match value.rsplit_once(':') {
        Some((host, port))
            if !host.is_empty()
                && host.len() <= 253
                && port.parse::<u16>().is_ok_and(|n| n != 0)
                && port.bytes().all(|b| b.is_ascii_digit()) =>
        {
            Ok(())
        }
        _ => Err("expected a host name or an address, ':' and a port from 1 to 65535".into()),
    }
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
    let reserved = ["/healthz", "/readyz", "/api/version", "/api/v1", "/v1"];
    if path.starts_with("/auth/")
        || path.starts_with("/api/v1/")
        || path.starts_with("/v1/")
        || reserved.contains(&path)
    {
        return Err("the path overlaps /healthz, /readyz, /api/version, /v1, /api/v1 or the reserved /auth/ namespace".into());
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
    if !ms.is_multiple_of(1000) {
        return format!("{ms}ms");
    }
    let s = value.as_secs();
    for (unit, size) in [("d", DAY), ("h", HOUR), ("m", MINUTE)] {
        if s != 0 && s.is_multiple_of(size) {
            return format!("{}{unit}", s / size);
        }
    }
    format!("{s}s")
}

/// The shortest exact form of a size.
pub fn show_size(value: usize) -> String {
    for (unit, size) in [("MiB", 1024 * 1024), ("KiB", 1024)] {
        if value != 0 && value.is_multiple_of(size) {
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
        Kind::Grants {
            expected,
            max_items,
        } => format!(
            "list of at most {max_items} grants, each a quoted text or an object with `grant` and `limits`: {expected}"
        ),
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
        let blocks = all_block_names();
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
        let labeled = all_labeled_block_names();
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
