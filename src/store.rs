//! Where the portal keeps its state: accounts, the history that the settings pages show, and, with
//! PostgreSQL, the live sessions.
//!
//! The store has two backends behind one type. SQLite is one file. One process owns it.
//! PostgreSQL is shared by any number of instances (see [`crate::sync`]).
//! Callers run these methods on blocking threads. Such a thread is the writer thread of [`crate::tracking`], or a `spawn_blocking` call from a handler.
//! The request path reads the in-memory directory instead. It never waits on the database.

pub mod pg;
mod sqlite;

use std::path::Path;
use std::time::Duration;

use crate::catalog::external::ExternalEntry;
use crate::config::PostgresConfig;
use crate::grants::Grant;
use crate::tracking::{AuditEvent, AuditKind, Batch, EndReason, Event, SessionStart};

use pg::Pg;
use sqlite::Sqlite;

/// Days drawn by the connection graphs. Today is included.
pub const GRAPH_DAYS: usize = 30;
const RECENT_SESSIONS: i64 = 8;
/// The most rows that the lists of the tab Activity hold. Each list covers the days of the graph.
pub const WINDOW_ROWS: i64 = 1_000;
const TOP_SERVICES: i64 = 12;
const DAY: i64 = 86_400;
/// The sign-in log keeps at most this many rows, newest first.
/// Failures are bounded per minute by the rate limiter. This limit bounds what a long siege can leave in the database.
const MAX_LOGIN_ROWS: i64 = 20_000;

/// A row of `user_grants` as a grant. The column `resource_path` holds the segments after the
/// service in their text form (`host:local/database:analytics`). It is empty for a grant on the
/// whole service. The column `limits` holds the limit values as JSON. The column `confirm` holds
/// the property of the same name. A row that is not a valid grant stops the read: the database
/// holds only what the servers wrote.
fn grant_from_row(
    action: &str,
    kind: &str,
    id: &str,
    path: &str,
    limits: &str,
    confirm: bool,
) -> Result<Grant, String> {
    if kind != crate::grants::SERVICE_TYPE {
        return Err(format!("user_grants: unknown resource type {kind:?}"));
    }
    let limits = crate::limits::Limits::from_json(limits)
        .map_err(|e| format!("user_grants: the limits of a grant: {e}"))?;
    Grant::from_path_text(action, id, path)
        .map(|grant| grant.with_limits(limits).with_confirm(confirm))
        .map_err(|e| format!("user_grants: {e}"))
}

/// The columns of a grant in `user_grants`: `resource_type`, `resource_id`, `resource_path`,
/// `limits` and `confirm`.
fn grant_columns(grant: &Grant) -> (&'static str, &str, String, String, bool) {
    (
        crate::grants::SERVICE_TYPE,
        grant.service.as_str(),
        grant.path_text(),
        grant.limits.to_json(),
        grant.confirm,
    )
}

/// The text form of a grant on one sub-resource of schema version 4 (SQLite) or 3 (PostgreSQL):
/// `<type>:<encoded id>`. The migration to `resource_path` uses it. The type and the id must be
/// valid for this version, else the migration stops and the database does not change.
fn migrated_path(kind: &str, id: &str) -> Result<String, String> {
    if kind.is_empty() && id.is_empty() {
        return Ok(String::new());
    }
    crate::grants::Segment::new(kind, id)
        .map(|segment| segment.to_string())
        .map_err(|e| {
            format!("user_grants: the grant on {kind:?} {id:?} cannot move to resource_path: {e}")
        })
}

#[derive(Clone, PartialEq, Eq)]
pub struct UserRow {
    pub login: String,
    pub password_hash: String,
    /// The grants that the settings pages gave, sorted.
    pub grants: Vec<Grant>,
    pub created_at: i64,
}

/// The debug text never shows the password hash.
impl std::fmt::Debug for UserRow {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("UserRow")
            .field("login", &self.login)
            .field("password_hash", &"<hidden>")
            .field("grants", &self.grants)
            .field("created_at", &self.created_at)
            .finish()
    }
}

/// The two modes of a key that the form of the key changes: what the key can do when the portal asks a person, and what
/// the portal does with the agents that share the key (lot G1b).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct KeyModes {
    pub interaction: crate::keys::Interaction,
    pub agents: crate::keys::AgentsMode,
}

/// A key of the database, with its grants (portal 4, 5). The database keeps the SHA-256 digest of
/// the text of the key, never the text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KeyRow {
    /// 16 characters from `0-9` and `a-f` (portal 4.7).
    pub id: String,
    /// The login of the owner. `None`: the key of the superadmin (portal 3.7b).
    pub owner: Option<String>,
    pub name: String,
    pub digest: [u8; 32],
    pub hint: String,
    pub created_at: i64,
    /// `None`: the key has no expiry date.
    pub expires_at: Option<i64>,
    pub last_check_at: Option<i64>,
    pub check_count: i64,
    /// The kind of the key: the prefix of its text (portal 5.1).
    pub kind: crate::keys::KeyKind,
    /// What the key can do when the portal asks a person (design of the dialogue, 1.6).
    pub interaction: crate::keys::Interaction,
    /// What the portal does with the agents of the key (lot G1b).
    pub agents: crate::keys::AgentsMode,
    /// Sorted.
    pub grants: Vec<Grant>,
}

/// The summary row of a revoked key (portal 5.4). It holds no grant and no counter.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KeyHistoryRow {
    pub id: String,
    pub owner: Option<String>,
    pub name: String,
    pub created_at: i64,
    pub revoked_at: i64,
    pub last_check_at: Option<i64>,
    pub check_count: i64,
}

/// What an instance reads in one directory round (portal 10.7): the version, the users and the
/// keys with their grants, all from one transaction.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DirectorySnapshot {
    pub version: i64,
    pub users: Vec<UserRow>,
    pub keys: Vec<KeyRow>,
    /// The external services, sorted by id.
    pub externals: Vec<ExternalEntry>,
}

/// The result of the count of one use (portal 7.2b).
#[derive(Clone, Debug, PartialEq)]
pub enum CountOutcome {
    /// Each counter had room: the statement took one unit from each.
    Counted,
    /// One counter or more had no room: the statement took nothing. Each refusal gives the index
    /// of the counter in the request and the seconds to the end of its window, by the clock of the
    /// database (portal 7.6a).
    Refused(Vec<Refusal>),
    /// A counter has no row (portal 7.2b): the answer is HTTP 503.
    RowMissing,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Refusal {
    pub index: usize,
    pub seconds_left: f64,
}

/// One line of the page of the keys (portal 5.5): a key, or the summary row of a revoked key.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KeyListItem {
    pub id: String,
    /// The login of the owner. `None`: the key of the superadmin.
    pub owner: Option<String>,
    pub name: String,
    pub hint: String,
    pub created_at: i64,
    pub expires_at: Option<i64>,
    pub last_check_at: Option<i64>,
    pub check_count: i64,
    /// `Some`: the key is revoked, and this is its summary row.
    pub revoked_at: Option<i64>,
    /// The kind of a key that exists. `None` for a revoked key: its summary row has no kind.
    pub kind: Option<crate::keys::KeyKind>,
    /// The interaction of a key that exists. `None` for a revoked key.
    pub interaction: Option<crate::keys::Interaction>,
    /// The mode of the agents of a key that exists. `None` for a revoked key.
    pub agents: Option<crate::keys::AgentsMode>,
    /// The grants of a key that exists. Empty for a revoked key.
    pub grants: Vec<Grant>,
}

/// Which keys a page lists.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeyScope<'a> {
    /// The keys of all the users, and the keys of the superadmin (section "All").
    All,
    /// The keys of one user of the database (section "Mine").
    Owner(&'a str),
    /// The keys of the superadmin (section "Mine" of the superadmin).
    Superadmin,
}

/// The checks of one key since the last write (portal 5.5a).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KeyCheck {
    pub id: String,
    pub last_check_at: i64,
    pub count: i64,
}

/// The bounds that a write of a key checks under its lock (portal 5.4, 7.2f, 10.7d).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct KeyCaps {
    /// `keys.max_per_user`.
    pub max_per_user: usize,
    /// `keys.max_counter_rows`.
    pub max_counter_rows: usize,
    /// `keys.max_directory_grants`.
    pub max_directory_grants: usize,
    /// The time of the write. A key with an expiry date at or before this time is not active.
    pub now: i64,
    /// A key created before this time is too old, so it is not active (portal 5.5b). `None`: the
    /// owner has no `max_key_lifetime`.
    pub created_not_before: Option<i64>,
}

/// Why the write of a key failed.
#[derive(Debug, PartialEq, Eq)]
pub enum KeyWriteError {
    /// The owner has `max_per_user` active keys (portal 5.4).
    TooManyKeys,
    /// The write would put the counter rows above `keys.max_counter_rows`, or the grants of the
    /// directory above `keys.max_directory_grants` (portal 5.8f `capacity`).
    Capacity,
    /// The owner does not exist.
    UnknownOwner,
    /// No such key for this owner.
    UnknownKey,
    Db(String),
}

#[derive(Debug, PartialEq, Eq)]
pub enum InsertError {
    Exists,
    Db(String),
}

/// One row of the table `external_services`, as the database keeps it. The conversion checks the type of the image
/// against its bytes, and gives an error that names the row.
pub struct ExternalRow {
    pub id: String,
    pub name: String,
    pub href: String,
    pub description: String,
    pub icon_type: Option<String>,
    pub icon: Option<Vec<u8>>,
    pub created_at: i64,
}

impl ExternalRow {
    pub fn into_entry(self) -> Result<ExternalEntry, String> {
        let icon = match (self.icon_type, self.icon) {
            (None, None) => None,
            (Some(mime), Some(bytes)) => Some(std::sync::Arc::new(
                crate::catalog::image::Image::from_stored(&mime, bytes).map_err(|reason| {
                    format!(
                        "the database holds an external service that the rules refuse: the row {:?} of the table external_services: icon: {reason}; fix or delete the row, then start again",
                        self.id
                    )
                })?,
            )),
            _ => {
                return Err(format!(
                    "the database holds an external service that the rules refuse: the row {:?} of the table external_services: icon_type and icon must both be set or both be empty; fix or delete the row, then start again",
                    self.id
                ))
            }
        };
        Ok(ExternalEntry {
            id: self.id,
            name: self.name,
            href: self.href,
            description: self.description,
            icon,
            created_at: self.created_at,
            from_file: false,
        })
    }
}

/// Why the store refuses a new external service.
#[derive(Debug, PartialEq, Eq)]
pub enum ExternalError {
    /// An external service has this id.
    Exists,
    /// The store holds the largest number of external services.
    Full,
    Db(String),
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DayStat {
    pub sessions: u32,
    /// Seconds spent connected, summed over the sessions of the day.
    pub seconds: i64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionRow {
    pub started_at: i64,
    pub last_seen_at: i64,
    /// `None` while the session is still open.
    pub ended_at: Option<i64>,
    pub end_reason: Option<String>,
    pub ip: Option<String>,
    pub user_agent: String,
    pub impersonated: bool,
    /// The session had the remembered rule ("Remember me").
    pub remembered: bool,
    /// The sign-in used the second factor.
    pub mfa: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ServiceUse {
    pub service_id: String,
    pub hits: u64,
    pub last_at: i64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AuditRow {
    pub ts: i64,
    pub kind: String,
    pub login: Option<String>,
    pub actor: Option<String>,
    pub ip: Option<String>,
}

/// One line of the sign-in log.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LoginRow {
    pub ts: i64,
    /// What was typed. The text is made safe before it is recorded.
    pub login: String,
    /// The typed login named an account.
    pub known: bool,
    pub success: bool,
    pub ip: Option<String>,
    pub user_agent: String,
}

/// The sign-in log of a period and the figures above it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LoginPage {
    /// The newest rows of the period that match the filter, at most `WINDOW_ROWS`.
    pub rows: Vec<LoginRow>,
    /// Rows of the period that match the filter. It is more than `rows.len()` when the list is cut.
    pub matching: u32,
    /// Every attempt of the period, and the refused ones among them. The figures do not depend on
    /// the filter: they are the counts of the chips of the tab Activity.
    pub total: u32,
    pub refused: u32,
}

/// What the Users page shows about one account.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AccountActivity {
    pub login: String,
    pub days: Vec<DayStat>,
    pub recent: Vec<SessionRow>,
    pub services: Vec<ServiceUse>,
    pub sessions_30d: u32,
    pub failed_30d: u32,
    pub last_login: Option<i64>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Snapshot {
    /// In the order the logins were asked for.
    pub accounts: Vec<AccountActivity>,
    /// Every real connection, all accounts.
    pub days: Vec<DayStat>,
    pub recent_events: Vec<AuditRow>,
    /// Unix time of the first day drawn.
    pub first_day: i64,
}

impl Snapshot {
    /// What the Users page shows when the history cannot be read. It shows the accounts, with no history recorded.
    pub fn blank(accounts: &[(String, bool)], now: i64) -> Self {
        Self {
            accounts: accounts
                .iter()
                .map(|(login, _)| AccountActivity {
                    login: login.clone(),
                    days: vec![DayStat::default(); GRAPH_DAYS],
                    ..AccountActivity::default()
                })
                .collect(),
            days: vec![DayStat::default(); GRAPH_DAYS],
            first_day: now - now.rem_euclid(DAY) - (GRAPH_DAYS as i64 - 1) * DAY,
            ..Self::default()
        }
    }
}

/// The idle times of the two rules of a session, in seconds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Idle {
    /// A session without the remembered rule.
    pub normal: i64,
    /// A remembered session.
    pub remembered: i64,
}

impl Idle {
    pub fn new(normal: Duration, remembered: Duration) -> Self {
        Self {
            normal: normal.as_secs() as i64,
            remembered: remembered.as_secs() as i64,
        }
    }

    /// The same time for both rules.
    #[cfg(test)]
    pub fn same(idle: Duration) -> Self {
        Self::new(idle, idle)
    }
}

/// A live session as the shared store keeps it. The store keeps only data that is not secret and not derived.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SharedSession {
    pub sid: u64,
    /// `None`: the superadmin.
    pub login: Option<String>,
    pub impersonated: bool,
    /// The session has the remembered rule: its life and its idle time are the longer ones.
    pub remembered: bool,
    /// The sign-in used the second factor.
    pub mfa: bool,
    /// Unix time.
    pub expires_at: i64,
    /// The stamp of the credential that opened the session (see [`crate::session::Credential`]).
    pub credential: [u8; 32],
}

/// The limits of the live sessions that a new session must respect.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SessionLimits {
    /// The largest number of live sessions of every principal (`session.max_active_sessions`).
    pub max_active: usize,
    /// The largest number of live sessions of one principal (`session.max_sessions_per_account`). An impersonated
    /// session does not count.
    pub max_per_account: usize,
}

/// The second factor of one account, as the store keeps it. The secret is sealed: only the key of the
/// configuration opens it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TotpRow {
    pub sealed: Vec<u8>,
    pub created_at: i64,
    /// The last time step that a code was accepted for. A code of this step or of an older step is refused.
    pub last_step: i64,
    /// The failed codes since `failed_since`, while the account is not locked.
    pub failed: u32,
    /// The account takes no code before this Unix time. The value is 0 when the account was never locked.
    pub locked_until: i64,
    /// The recovery codes that nobody used.
    pub recovery_left: u32,
}

/// How many failed second-factor attempts lock the account, and for how long.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TotpLimits {
    pub max_failures: i64,
    /// The failures count while they stay inside this window, in seconds.
    pub window: i64,
    /// The lock lasts this long, in seconds.
    pub lockout: i64,
}

impl TotpLimits {
    /// The counters after one more failure at `now`: the failures, the start of the window, and the end of the lock
    /// when this failure locks the account. A lock resets the counters, so no lock is permanent.
    pub fn after_failure(self, failed: i64, since: i64, now: i64) -> (i64, i64, Option<i64>) {
        let (failed, since) = if failed <= 0 || now - since > self.window {
            (1, now)
        } else {
            (failed + 1, since)
        };
        if failed >= self.max_failures {
            (0, 0, Some(now + self.lockout))
        } else {
            (failed, since, None)
        }
    }
}

/// What [`Store::totp_begin_attempt`] found.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Attempt {
    /// The account has no second factor.
    NotEnrolled,
    /// The account takes no code until this Unix time. The attempt does not count.
    Locked { until: i64 },
    /// The attempt counts as a failure until a good code clears the count. `locked_until` is set when this
    /// attempt locks the account.
    Counted { locked_until: Option<i64> },
}

/// A line of `sessions_log`, as the import reads and writes it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LoggedSession {
    pub sid: i64,
    pub login: String,
    pub superadmin: bool,
    pub impersonated: bool,
    pub remembered: bool,
    pub mfa: bool,
    pub ip: Option<String>,
    pub user_agent: String,
    pub started_at: i64,
    pub last_seen_at: i64,
    pub ended_at: Option<i64>,
    pub end_reason: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LoggedAccess {
    pub sid: i64,
    pub service_id: String,
    pub first_at: i64,
    pub last_at: i64,
    pub hits: i64,
    pub ip: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LoggedAudit {
    pub id: i64,
    pub ts: i64,
    pub kind: String,
    pub login: Option<String>,
    pub actor: Option<String>,
    pub ip: Option<String>,
    pub user_agent: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LoggedLogin {
    pub id: i64,
    pub ts: i64,
    pub ip: Option<String>,
    pub login: String,
    pub known: bool,
    pub success: bool,
    pub user_agent: String,
}

/// Everything that the import carries over from a SQLite file. The accounts keep their identifier.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Dump {
    pub users: Vec<(i64, UserRow)>,
    pub sessions: Vec<LoggedSession>,
    pub accesses: Vec<LoggedAccess>,
    pub audit: Vec<LoggedAudit>,
    pub logins: Vec<LoggedLogin>,
    /// The keys with their grants, and the summary rows of the revoked keys (schema 6).
    pub keys: Vec<KeyRow>,
    pub key_history: Vec<KeyHistoryRow>,
    /// The external services.
    pub externals: Vec<ExternalEntry>,
    /// The second factor of the accounts, with its recovery codes, and the check value of its key.
    pub totp: Vec<TotpDump>,
    pub totp_check: Option<Vec<u8>>,
}

/// One account with a second factor, as the import reads and writes it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TotpDump {
    pub login: String,
    pub sealed: Vec<u8>,
    pub created_at: i64,
    pub last_step: i64,
    pub recovery: Vec<Vec<u8>>,
}

/// What an import copied.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImportReport {
    pub users: usize,
    pub sessions: usize,
    pub accesses: usize,
    pub audit: usize,
    pub logins: usize,
    pub keys: usize,
    pub key_history: usize,
    pub externals: usize,
    pub totp: usize,
}

/// The file that tells that a portal runs on a SQLite file: the file of the database and `.pid`.
fn sqlite_pid_file(path: &Path) -> std::path::PathBuf {
    let mut name = path.as_os_str().to_os_string();
    name.push(".pid");
    std::path::PathBuf::from(name)
}

/// Writes the process id of this portal next to its SQLite file. A command that writes to the
/// file reads it (portal 10.4a).
pub fn mark_sqlite_instance(path: &Path) -> Result<(), String> {
    std::fs::write(sqlite_pid_file(path), format!("{}\n", std::process::id()))
        .map_err(|e| format!("cannot write {}: {e}", sqlite_pid_file(path).display()))
}

/// Whether a portal runs on this SQLite file: the file `.pid` names a live process that is not
/// this one. The check reads `/proc`.
pub fn sqlite_instance_running(path: &Path) -> bool {
    let Ok(text) = std::fs::read_to_string(sqlite_pid_file(path)) else {
        return false;
    };
    let Ok(pid) = text.trim().parse::<u32>() else {
        return false;
    };
    pid != std::process::id() && Path::new("/proc").join(pid.to_string()).exists()
}

pub enum Store {
    Sqlite(Sqlite),
    Pg(Box<Pg>),
}

macro_rules! backend {
    ($self:ident . $method:ident ( $($arg:expr),* )) => {
        match $self {
            Store::Sqlite(store) => store.$method($($arg),*),
            Store::Pg(store) => store.$method($($arg),*),
        }
    };
}

impl Store {
    pub fn open(path: &Path) -> Result<Self, String> {
        Sqlite::open(path).map(Store::Sqlite)
    }

    /// Connects to PostgreSQL. The call waits up to one minute for it. Then it brings the schema up to date.
    pub fn open_postgres(
        config: &PostgresConfig,
        password: &str,
        counter_connections: usize,
    ) -> Result<Self, String> {
        let mut store = Pg::connect(config, password, pg::STARTUP_RETRY)?;
        store.set_counter_connections(counter_connections);
        Ok(Store::Pg(Box::new(store)))
    }

    #[cfg(test)]
    pub fn open_in_memory() -> Self {
        Store::Sqlite(Sqlite::open_in_memory())
    }

    /// Several portal instances use this store at once. Sessions are kept in it.
    /// The accounts must be read again from time to time.
    pub fn is_shared(&self) -> bool {
        matches!(self, Store::Pg(_))
    }

    /// Rows of a table. The tests use these rows to check what a method left behind.
    #[cfg(test)]
    pub fn count(&self, table: &str) -> i64 {
        backend!(self.count(table))
    }

    /// The sum of the units of all the counters. The tests use it to see what a use counted.
    #[cfg(test)]
    pub fn counted_units(&self) -> i64 {
        backend!(self.counted_units())
    }

    /// Runs SQL that the test needs, for example to remove a row. Not for the program.
    #[cfg(test)]
    pub fn execute_raw(&self, sql: &str) {
        match self {
            Store::Sqlite(store) => store.execute_raw(sql),
            Store::Pg(store) => store.execute_raw(sql),
        }
    }

    /// What a lost connection looks like to the next calls. This applies to PostgreSQL only.
    #[cfg(test)]
    pub fn go_down(&self) {
        if let Store::Pg(store) = self {
            store.go_down();
        }
    }

    // Accounts --------------------------------------------------------------------------------

    #[cfg(test)]
    pub fn load_users(&self) -> Result<Vec<UserRow>, String> {
        backend!(self.load_users())
    }

    pub fn insert_user(&self, user: &UserRow) -> Result<(), InsertError> {
        backend!(self.insert_user(user))
    }

    /// Replaces the password hash and the grants of a user. Each is replaced only when it is given.
    /// The change runs in one transaction. `false` if no such user.
    pub fn update_user(
        &self,
        login: &str,
        password_hash: Option<&str>,
        grants: Option<&[Grant]>,
    ) -> Result<bool, String> {
        backend!(self.update_user(login, password_hash, grants))
    }

    /// Removes an account with its grants, its keys and its second factor.
    pub fn remove_user(&self, login: &str) -> Result<bool, String> {
        backend!(self.remove_user(login))
    }

    // External services -------------------------------------------------------------------------

    /// Adds an external service. The write adds 1 to the version of the directory. It fails with `Exists` when
    /// the id is in use, and with `Full` when the store holds `max` entries.
    pub fn insert_external(&self, entry: &ExternalEntry, max: usize) -> Result<(), ExternalError> {
        backend!(self.insert_external(entry, max))
    }

    /// Removes an external service and every grant that names its id, of the accounts and of the keys, with
    /// their counters. The change runs in one transaction. `false`: no such service.
    pub fn remove_external(&self, id: &str) -> Result<bool, String> {
        backend!(self.remove_external(id))
    }

    // Directory and keys ------------------------------------------------------------------------

    /// The version of the directory: it grows with each write to the users, the grants, the keys and the
    /// external services (portal 10.7).
    pub fn directory_version(&self) -> Result<i64, String> {
        backend!(self.directory_version())
    }

    /// The version, the users and the keys, read in one transaction.
    pub fn load_directory(&self) -> Result<DirectorySnapshot, String> {
        backend!(self.load_directory())
    }

    /// Makes a key. The write adds 1 to the version, and it makes the counter rows of the grants of
    /// the key.
    pub fn insert_key(&self, key: &KeyRow, caps: &KeyCaps) -> Result<(), KeyWriteError> {
        backend!(self.insert_key(key, caps))
    }

    /// Changes the name, the modes and the grants of a key (portal 5.8d). `owner` is the login
    /// of the owner: the key must be his. `None` is the key of the superadmin.
    pub fn update_key(
        &self,
        id: &str,
        owner: Option<&str>,
        name: &str,
        modes: KeyModes,
        grants: &[Grant],
        caps: &KeyCaps,
    ) -> Result<(), KeyWriteError> {
        backend!(self.update_key(id, owner, name, modes, grants, caps))
    }

    /// Revokes a key: deletes it with its grants and its counters, and keeps one summary row.
    /// `owner` limits the revocation to the keys of one owner (`Some(None)` is the superadmin).
    /// `false`: no such key.
    pub fn revoke_key(
        &self,
        id: &str,
        owner: Option<Option<&str>>,
        now: i64,
    ) -> Result<bool, String> {
        backend!(self.revoke_key(id, owner, now))
    }

    /// Revokes the keys of one user of the database, or all the keys of the database when `login`
    /// is `None`. Returns the number of keys.
    pub fn revoke_keys(&self, login: Option<&str>, now: i64) -> Result<usize, String> {
        backend!(self.revoke_keys(login, now))
    }

    /// Adds the checks of the keys (portal 5.5a): the later time and the number of checks of each
    /// key. A key that does not exist any more is ignored.
    pub fn write_key_checks(&self, checks: &[KeyCheck]) -> Result<(), String> {
        backend!(self.write_key_checks(checks))
    }

    /// Deletes the keys that expired more than `expired_before` seconds ago, and the summary rows
    /// that are older than `history_before` (portal 5.4).
    pub fn prune_keys(&self, expired_before: i64, history_before: i64) -> Result<(), String> {
        backend!(self.prune_keys(expired_before, history_before))
    }

    /// Counts one use: takes one unit from each counter, or none (portal 7.2b). Each counter names
    /// its owner, its budget, its window and its amount.
    ///
    /// `clock` replaces the clock of the database for the window of the counters, in seconds since
    /// the Unix epoch. The program passes `None`: only a test fixes the clock.
    pub fn count_use(
        &self,
        counters: &[crate::decision::Counter],
        clock: Option<f64>,
    ) -> Result<CountOutcome, String> {
        backend!(self.count_use(counters, clock))
    }

    /// Gives back the unit that [`Store::count_use`] took from each counter, when the window of the
    /// counter has not ended. The PDP uses it when it counted a use and then could not consume the
    /// approval of the use (portal 7.7d). `clock` is as for `count_use`.
    pub fn refund_use(
        &self,
        counters: &[crate::decision::Counter],
        clock: Option<f64>,
    ) -> Result<(), String> {
        backend!(self.refund_use(counters, clock))
    }

    /// Makes the counter rows that the table does not hold yet (portal 7.2a). Returns the number of
    /// rows that it made.
    pub fn ensure_counter_rows(
        &self,
        rows: &[(crate::decision::CounterOwner, crate::grants::CounterId)],
    ) -> Result<usize, String> {
        backend!(self.ensure_counter_rows(rows))
    }

    /// Deletes the counter rows of the subjects of the file that the file no longer has
    /// (portal 3.4d, 7.2d). `keep` lists the subjects that the file still has.
    pub fn delete_file_counters(&self, keep: &[String]) -> Result<usize, String> {
        backend!(self.delete_file_counters(keep))
    }

    /// The number of counter rows (portal 7.2f).
    pub fn counter_rows(&self) -> Result<i64, String> {
        backend!(self.counter_rows())
    }

    /// Whether the portal keeps the summary row of a revoked key with this id. `owner` limits the
    /// row to one owner, as in [`Store::revoke_key`].
    pub fn has_key_history(&self, id: &str, owner: Option<Option<&str>>) -> Result<bool, String> {
        backend!(self.has_key_history(id, owner))
    }

    /// One page of the keys and the summary rows of the revoked keys, newest first, after the
    /// cursor `after` (the creation time and the id of the last line of the page before). It gives
    /// at most `limit` lines (portal 5.5).
    pub fn list_keys(
        &self,
        scope: KeyScope<'_>,
        after: Option<(i64, &str)>,
        limit: usize,
    ) -> Result<Vec<KeyListItem>, String> {
        backend!(self.list_keys(scope, after, limit))
    }

    /// The summary rows of the revoked keys. The tests read them.
    #[cfg(test)]
    pub fn key_history(&self) -> Result<Vec<KeyHistoryRow>, String> {
        backend!(self.key_history())
    }

    // History ---------------------------------------------------------------------------------

    /// One transaction for everything the tracker gathered since the last flush.
    pub fn apply(&self, batch: &Batch) -> Result<(), String> {
        backend!(self.apply(batch))
    }

    /// Sessions that a previous run left open end at the last time they were seen. Use this only for a store that one process owns.
    /// In a shared store, the sessions outlive a restart. See [`Store::close_lost_sessions`].
    pub fn close_open_sessions(&self, now: i64, reason: &str) -> Result<(), String> {
        match self {
            Store::Sqlite(store) => store.close_open_sessions(now, reason),
            Store::Pg(_) => Ok(()),
        }
    }

    /// Deletes the rows that started before `cutoff`.
    pub fn prune(&self, cutoff: i64) -> Result<(), String> {
        backend!(self.prune(cutoff))
    }

    /// The sign-in log from `since` (unix time), newest first, at most `WINDOW_ROWS` rows. If
    /// `only_failed` is true, the list holds only the refused attempts.
    pub fn login_attempts(&self, since: i64, only_failed: bool) -> Result<LoginPage, String> {
        backend!(self.login_attempts(since, only_failed))
    }

    /// Everything the Users page shows. `accounts` is `(login, is_superadmin)`.
    pub fn snapshot(&self, accounts: &[(String, bool)], now: i64) -> Result<Snapshot, String> {
        backend!(self.snapshot(accounts, now))
    }

    // The second factor ---------------------------------------------------------------------------

    /// Tells whether `check` is the check value of the key that sealed the secrets of this database. The first call
    /// on a database without a check value records `check` and answers `true`.
    pub fn totp_bind_key(&self, check: &[u8; 32]) -> Result<bool, String> {
        backend!(self.totp_bind_key(check))
    }

    pub fn totp_get(&self, login: &str) -> Result<Option<TotpRow>, String> {
        backend!(self.totp_get(login))
    }

    /// The logins that have a second factor, in order.
    pub fn totp_logins(&self) -> Result<Vec<String>, String> {
        backend!(self.totp_logins())
    }

    /// Turns the second factor on. `step` is the time step of the code that confirmed the enrolment: it cannot be used
    /// again. The answer is `false`, and nothing changes, when the account has one already.
    pub fn totp_enable(
        &self,
        login: &str,
        sealed: &[u8],
        recovery: &[[u8; 32]],
        step: i64,
        now: i64,
    ) -> Result<bool, String> {
        backend!(self.totp_enable(login, sealed, recovery, step, now))
    }

    /// Records `step` as the last accepted step, and clears the failures and the lock. The answer is `true` for one
    /// caller only: a code is good once, also when two instances check it at the same time.
    pub fn totp_accept_step(&self, login: &str, step: i64) -> Result<bool, String> {
        backend!(self.totp_accept_step(login, step))
    }

    /// Consumes one recovery code, and clears the failures and the lock. The answer is `true` for one caller only.
    pub fn totp_use_recovery(&self, login: &str, hash: &[u8; 32]) -> Result<bool, String> {
        backend!(self.totp_use_recovery(login, hash))
    }

    /// Replaces the recovery codes of an account that has a second factor. `false`: it has none.
    pub fn totp_replace_recovery(
        &self,
        login: &str,
        recovery: &[[u8; 32]],
    ) -> Result<bool, String> {
        backend!(self.totp_replace_recovery(login, recovery))
    }

    /// Turns the second factor off. `false`: the account had none.
    pub fn totp_remove(&self, login: &str) -> Result<bool, String> {
        backend!(self.totp_remove(login))
    }

    /// Removes the second factor of every account and the check value of the key, so that a new key can serve.
    /// Returns the number of accounts.
    pub fn totp_remove_all(&self) -> Result<usize, String> {
        backend!(self.totp_remove_all())
    }

    /// Reads the lock of an account and counts one attempt of the second step, in one write transaction, before the
    /// caller compares the code. Attempts that arrive at the same time thus never pass a lock that an earlier attempt
    /// set. The count is in the database, so every instance adds to it. A good code clears it (see
    /// [`Store::totp_accept_step`]).
    pub fn totp_begin_attempt(
        &self,
        login: &str,
        now: i64,
        limits: TotpLimits,
    ) -> Result<Attempt, String> {
        backend!(self.totp_begin_attempt(login, now, limits))
    }

    // Shared sessions (PostgreSQL only) -------------------------------------------------------

    fn shared(&self) -> Result<&Pg, String> {
        match self {
            Store::Pg(store) => Ok(store),
            Store::Sqlite(_) => Err("this database does not hold shared sessions".into()),
        }
    }

    /// Records a session and its line of the history. A session that is not impersonated first ends the sessions
    /// of its principal that were used least recently. Thus the principal has at most `limits.max_per_account`
    /// sessions. Returns the identifiers of these ended sessions. Returns `None`, and changes nothing, when
    /// `limits.max_active` sessions are live. `generation` is the credential generation of the superadmin of this
    /// instance.
    pub fn session_insert(
        &self,
        digest: &[u8; 32],
        binding: &[u8; 32],
        session: &SharedSession,
        start: &SessionStart,
        limits: SessionLimits,
        generation: &[u8; 32],
    ) -> Result<Option<Vec<u64>>, String> {
        self.shared()?
            .session_insert(digest, binding, session, start, limits, generation)
    }

    /// Returns the live session of `digest` for `binding`. Its idle timer is renewed to `now`.
    /// A superadmin or impersonated session must carry `generation`.
    /// `idle` is the idle time of a session. A remembered session has `remember_idle`.
    pub fn session_touch(
        &self,
        digest: &[u8; 32],
        binding: &[u8; 32],
        now: i64,
        idle: Idle,
        generation: &[u8; 32],
    ) -> Result<Option<SharedSession>, String> {
        self.shared()?
            .session_touch(digest, binding, now, idle, generation)
    }

    /// The same as [`Self::session_touch`], and the idle timer stays as it is. The call writes
    /// nothing.
    pub fn session_peek(
        &self,
        digest: &[u8; 32],
        binding: &[u8; 32],
        now: i64,
        idle: Idle,
        generation: &[u8; 32],
    ) -> Result<Option<SharedSession>, String> {
        self.shared()?
            .session_peek(digest, binding, now, idle, generation)
    }

    /// Ends the superadmin and impersonated sessions of every other credential generation.
    /// Returns the identifiers that this call ended.
    pub fn sessions_end_other_generations(
        &self,
        generation: &[u8; 32],
    ) -> Result<Vec<u64>, String> {
        self.shared()?.sessions_end_other_generations(generation)
    }

    /// Ends one session. The session is returned only when this call ends it.
    pub fn session_delete(&self, digest: &[u8; 32]) -> Result<Option<SharedSession>, String> {
        self.shared()?.session_delete(digest)
    }

    /// Ends every session of a user. Returns the identifiers that this call ended.
    pub fn sessions_delete_user(&self, login: &str) -> Result<Vec<u64>, String> {
        self.shared()?.sessions_delete_user(login)
    }

    /// Ends every session in the shared database, the superadmin sessions too. Returns the identifiers that this call ended.
    pub fn sessions_delete_all(&self) -> Result<Vec<u64>, String> {
        self.shared()?.sessions_delete_all()
    }

    /// The incident command `portal end-sessions`. It ends every session in the shared database and closes their history rows with the reason `revoked`.
    /// It writes one audit event, with the actor `cli`. It returns the number of sessions that it ended.
    /// A SQLite file holds no live session: the sessions are in the memory of the portal process. This call then fails and changes nothing.
    pub fn end_all_sessions(&self, now: i64) -> Result<usize, String> {
        let ended = self.shared().map_err(|_| {
            "this configuration keeps the sessions in the memory of the portal process: restart the portal to end them".to_string()
        })?.sessions_delete_all()?;
        let mut batch = Batch::default();
        for sid in &ended {
            batch.events.push(Event::SessionEnded {
                sid: *sid,
                reason: EndReason::Revoked,
                at: now,
            });
        }
        batch.events.push(Event::Audit(AuditEvent {
            kind: AuditKind::SessionsEnded,
            login: None,
            actor: Some("cli".into()),
            ip: None,
            user_agent: "portal end-sessions".into(),
            at: now,
        }));
        self.apply(&batch)?;
        Ok(ended.len())
    }

    /// Ends the sessions past their lifetime or idle time. Each result states whether the session expired. Otherwise, the session idled.
    pub fn sessions_expire(&self, now: i64, idle: Idle) -> Result<Vec<(u64, bool)>, String> {
        self.shared()?.sessions_expire(now, idle)
    }

    pub fn sessions_count(&self) -> Result<usize, String> {
        self.shared()?.sessions_count()
    }

    /// Closes the history rows of gone sessions that were last seen before `before`.
    pub fn close_lost_sessions(&self, before: i64, reason: &str) -> Result<usize, String> {
        self.shared()?.close_lost_sessions(before, reason)
    }

    // Import ----------------------------------------------------------------------------------

    /// Copies a SQLite file into this PostgreSQL database. The database must be empty.
    pub fn import_sqlite(&self, path: &Path) -> Result<ImportReport, String> {
        let target = self
            .shared()
            .map_err(|_| "the target of an import must be a PostgreSQL database".to_string())?;
        target.import(&Sqlite::open_read_only(path)?.dump()?)
    }
}

#[cfg(test)]
pub mod testing {
    //! Every backend that a test can run on. SQLite is always used. PostgreSQL is used when `PORTAL_TEST_POSTGRES` holds the
    //! URL of a database (`postgres://user:password@host:port/database`).
    //! Each PostgreSQL store lives in a schema of its own. Therefore, tests run side by side and do not change the rest of the database.

    use super::*;

    pub const POSTGRES_ENV: &str = "PORTAL_TEST_POSTGRES";

    #[derive(Clone)]
    pub enum Backend {
        Sqlite,
        Postgres(String),
    }

    impl Backend {
        pub fn name(&self) -> &'static str {
            match self {
                Backend::Sqlite => "sqlite",
                Backend::Postgres(_) => "postgres",
            }
        }

        /// An empty store.
        pub fn fresh(&self) -> Store {
            match self {
                Backend::Sqlite => Store::open_in_memory(),
                Backend::Postgres(url) => Store::Pg(Box::new(Pg::open_isolated(url))),
            }
        }

        /// Two instances on one empty database. This is for PostgreSQL only.
        pub fn fresh_pair(&self) -> (Store, Store) {
            let Store::Pg(first) = self.fresh() else {
                panic!("two instances can share a PostgreSQL database only");
            };
            let second = first.sibling();
            (Store::Pg(first), Store::Pg(Box::new(second)))
        }
    }

    pub fn backends() -> Vec<Backend> {
        let mut all = vec![Backend::Sqlite];
        match std::env::var(POSTGRES_ENV) {
            Ok(url) if !url.is_empty() => all.push(Backend::Postgres(url)),
            _ => {
                eprintln!("{POSTGRES_ENV} is not set: the PostgreSQL side of this test is skipped")
            }
        }
        all
    }

    pub fn postgres() -> Option<Backend> {
        backends()
            .into_iter()
            .find(|b| matches!(b, Backend::Postgres(_)))
    }

    /// Runs `test` once per available backend.
    pub fn each_backend(test: impl Fn(&Backend)) {
        for backend in backends() {
            eprintln!("-- {}", backend.name());
            test(&backend);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::testing::*;
    use super::*;
    use crate::tracking::{AuditEvent, AuditKind, EndReason, Event, Hit, LoginAttempt};
    use std::collections::HashMap;

    /// An account with `access` on each named service.
    fn user(login: &str, services: &[&str]) -> UserRow {
        UserRow {
            login: login.into(),
            password_hash: "$argon2id$stub".into(),
            grants: access(services),
            created_at: 1_000,
        }
    }

    fn access(services: &[&str]) -> Vec<Grant> {
        services
            .iter()
            .map(|s| Grant::new("access", s).unwrap())
            .collect()
    }

    fn start(sid: u64, login: &str, superadmin: bool, impersonated: bool, at: i64) -> Event {
        Event::SessionStarted(SessionStart {
            sid,
            login: login.into(),
            superadmin,
            impersonated,
            remembered: false,
            mfa: false,
            ip: Some("10.0.0.2".into()),
            user_agent: "Browser/1.0".into(),
            at,
        })
    }

    const NOW: i64 = 1_000_000;

    fn attempt(login: &str, success: bool, ip: &str, at: i64) -> Event {
        Event::Login(LoginAttempt {
            login: login.into(),
            known: login == "alice",
            success,
            ip: Some(ip.into()),
            user_agent: "Browser/1.0".into(),
            at,
        })
    }

    fn modes() -> KeyModes {
        KeyModes {
            interaction: crate::keys::Interaction::Conversational,
            agents: crate::keys::AgentsMode::Discover,
        }
    }

    fn caps() -> KeyCaps {
        KeyCaps {
            max_per_user: 3,
            max_counter_rows: 100,
            max_directory_grants: 100,
            now: NOW,
            created_not_before: None,
        }
    }

    /// A key with the grants `access on service:db` with a budget of two windows, and a read.
    fn key(id: &str, owner: Option<&str>, byte: u8) -> KeyRow {
        use crate::limits::Limits;
        let budget = Limits::from_json(
            r#"{"requests":[{"amount":10,"window":"second"},{"amount":300,"window":"minute"}]}"#,
        )
        .unwrap();
        let mut grants = vec![
            Grant::new("access", "db").unwrap().with_limits(budget),
            Grant::from_path_text("data.read", "db", "host:prod").unwrap(),
        ];
        grants.sort();
        KeyRow {
            kind: crate::keys::KeyKind::Mcp,
            interaction: crate::keys::Interaction::Conversational,
            agents: crate::keys::AgentsMode::Discover,
            id: id.into(),
            owner: owner.map(Into::into),
            name: format!("key {id}"),
            digest: [byte; 32],
            hint: "ab12".into(),
            created_at: NOW - 10,
            expires_at: None,
            last_check_at: None,
            check_count: 0,
            grants,
        }
    }

    fn version(store: &Store) -> i64 {
        store.directory_version().unwrap()
    }

    #[test]
    fn each_write_to_the_users_the_grants_and_the_keys_adds_1_to_the_directory_version() {
        each_backend(|backend| {
            let store = backend.fresh();
            assert_eq!(version(&store), 0);
            store.insert_user(&user("alice", &[])).unwrap();
            assert_eq!(version(&store), 1);
            assert!(store
                .update_user("alice", Some("h2"), Some(&access(&["db"])))
                .unwrap());
            assert_eq!(version(&store), 2);
            assert!(!store.update_user("nobody", Some("h2"), None).unwrap());
            assert_eq!(
                version(&store),
                2,
                "a write that changes nothing adds nothing"
            );
            store
                .insert_key(&key("k1", Some("alice"), 1), &caps())
                .unwrap();
            assert_eq!(version(&store), 3);
            assert!(store.revoke_key("k1", None, NOW).unwrap());
            assert_eq!(version(&store), 4);
            assert!(!store.revoke_key("k1", None, NOW).unwrap());
            assert_eq!(version(&store), 4);
            assert!(store.remove_user("alice").unwrap());
            assert_eq!(version(&store), 5);
            assert!(!store.remove_user("alice").unwrap());
            assert_eq!(version(&store), 5);
            // The checks of the keys are not a change of the directory (portal 10.7b).
            store.insert_user(&user("bob", &[])).unwrap();
            store
                .insert_key(&key("k2", Some("bob"), 2), &caps())
                .unwrap();
            let before = version(&store);
            store
                .write_key_checks(&[KeyCheck {
                    id: "k2".into(),
                    last_check_at: NOW,
                    count: 1,
                }])
                .unwrap();
            assert_eq!(version(&store), before);
        });
    }

    fn external(name: &str, href: &str) -> ExternalEntry {
        let (name, id) = crate::catalog::external::check_name(name).unwrap();
        ExternalEntry {
            id,
            name,
            href: href.into(),
            created_at: NOW,
            ..Default::default()
        }
    }

    #[test]
    fn an_external_service_is_stored_listed_and_removed_with_every_grant_that_names_it() {
        each_backend(|backend| {
            let store = backend.fresh();
            let wiki = external("Team wiki", "https://wiki.example.org/");
            let docs = external("Docs", "https://docs.example.org/x?y=1#z");
            assert_eq!(wiki.id, "ext-team-wiki");
            assert!(store.load_directory().unwrap().externals.is_empty());
            let before = version(&store);
            store.insert_external(&wiki, 2).unwrap();
            assert_eq!(version(&store), before + 1, "a write adds 1 to the version");
            store.insert_external(&docs, 2).unwrap();
            assert_eq!(
                store.insert_external(&wiki, 5),
                Err(ExternalError::Exists),
                "the id is the key"
            );
            let third = external("Third", "https://third.example.org/");
            assert_eq!(
                store.insert_external(&third, 2),
                Err(ExternalError::Full),
                "the store holds the most that the caller allows"
            );
            assert_eq!(
                version(&store),
                before + 2,
                "a refused write adds nothing to the version"
            );
            let directory = store.load_directory().unwrap();
            assert_eq!(directory.version, version(&store));
            assert_eq!(
                directory.externals,
                [docs.clone(), wiki.clone()],
                "sorted by id, as written"
            );

            // The grants that name the service: an account, a key and the counters of a budget.
            let budget = crate::limits::Limits::from_json(
                r#"{"requests":[{"amount":10,"window":"minute"}]}"#,
            )
            .unwrap();
            let mut alice = user("alice", &["db", &wiki.id, &docs.id]);
            alice.grants[1] = alice.grants[1].clone().with_limits(budget);
            alice.grants.sort();
            store.insert_user(&alice).unwrap();
            let mut agent = key("k1", Some("alice"), 1);
            agent.grants.push(Grant::new("access", &wiki.id).unwrap());
            agent.grants.sort();
            store.insert_key(&agent, &caps()).unwrap();
            let counters = store.count("budget_counters");
            assert!(counters > 0);

            let before = version(&store);
            assert!(store.remove_external(&wiki.id).unwrap());
            assert_eq!(version(&store), before + 1);
            let directory = store.load_directory().unwrap();
            assert_eq!(directory.externals, std::slice::from_ref(&docs));
            let grants: Vec<String> = directory.users[0]
                .grants
                .iter()
                .map(|g| g.service.clone())
                .collect();
            assert_eq!(grants, ["db", "ext-docs"], "the grants of the service go");
            let key_services: Vec<&str> = directory.keys[0]
                .grants
                .iter()
                .map(|g| g.service.as_str())
                .collect();
            assert!(!key_services.contains(&"ext-team-wiki"));
            assert!(key_services.contains(&"db"));
            assert!(
                store.count("budget_counters") < counters,
                "the counters of the budget go too"
            );

            assert!(!store.remove_external(&wiki.id).unwrap());
            assert_eq!(version(&store), before + 1, "nothing to remove, no change");
            // The id is free again.
            store.insert_external(&wiki, 2).unwrap();
            assert_eq!(store.load_directory().unwrap().externals.len(), 2);
            // The accounts and their other grants stay.
            assert_eq!(store.load_users().unwrap()[0].grants.len(), 2);
        });
    }

    fn svg_icon() -> std::sync::Arc<crate::catalog::image::Image> {
        std::sync::Arc::new(
            crate::catalog::image::Image::from_bytes(
                br##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 8 8"><rect width="8" height="8"/></svg>"##
                    .to_vec(),
                None,
            )
            .unwrap(),
        )
    }

    fn png_icon() -> std::sync::Arc<crate::catalog::image::Image> {
        std::sync::Arc::new(
            crate::catalog::image::Image::from_bytes(
                b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR\0\0\0\x01\0\0\0\x01\x08\x06\0\0\0\x1f\x15\xc4\x89\xff\0\x0a".to_vec(),
                None,
            )
            .unwrap(),
        )
    }

    #[test]
    fn an_external_service_keeps_its_description_and_its_icon_exactly() {
        each_backend(|backend| {
            let store = backend.fresh();
            let mut plain = external("Plain", "https://plain.example.org/");
            plain.created_at = 7;
            let mut svg = external("With svg", "https://svg.example.org/");
            svg.description = "\u{e9}".repeat(200);
            svg.icon = Some(svg_icon());
            let mut png = external("With png", "http://[::1]:8080/x");
            png.description = "Dashboards".into();
            png.icon = Some(png_icon());
            for entry in [&plain, &svg, &png] {
                store.insert_external(entry, 10).unwrap();
            }
            let loaded = store.load_directory().unwrap().externals;
            assert_eq!(
                loaded,
                [plain.clone(), png.clone(), svg.clone()],
                "{}",
                backend.name()
            );
            // The bytes come back as they went in: the last byte of the PNG is a line end.
            let png_back = loaded.iter().find(|e| e.id == png.id).unwrap();
            assert_eq!(png_back.icon.as_ref().unwrap().bytes().last(), Some(&b'\n'));
            assert_eq!(png_back.icon.as_ref().unwrap().mime(), "image/png");
            assert!(
                loaded.iter().all(|e| !e.from_file),
                "a row is never from the file"
            );
            assert_eq!(
                loaded
                    .iter()
                    .find(|e| e.id == plain.id)
                    .unwrap()
                    .description,
                ""
            );
        });
    }

    /// The bytes of a binary column in the syntax of each backend.
    fn blob(backend: &testing::Backend, hex: &str) -> String {
        match backend {
            testing::Backend::Sqlite => format!("X'{hex}'"),
            testing::Backend::Postgres(_) => format!("'\\x{hex}'::bytea"),
        }
    }

    #[test]
    fn a_row_that_breaks_the_rules_is_named_when_the_start_reads_it() {
        each_backend(|backend| {
            let store = backend.fresh();
            // A row that a hand wrote, with a link that the rule refuses.
            store.execute_raw(
                "INSERT INTO external_services (id, name, href, created_at) VALUES ('ext-bad', 'Bad', 'http://1.2.3/', 1)",
            );
            let snapshot = store.load_directory().unwrap();
            let error = crate::bootstrap::check_external_rows(&snapshot).unwrap_err();
            assert!(
                error.contains("ext-bad")
                    && error.contains("href")
                    && error.contains("IPv4")
                    && error.contains("http://1.2.3/"),
                "{error}"
            );
            store.execute_raw("DELETE FROM external_services");
            // A name that does not make its id.
            store.execute_raw(
                "INSERT INTO external_services (id, name, href, created_at) VALUES ('ext-other', 'Wiki', 'https://wiki.example.org/', 1)",
            );
            let error = crate::bootstrap::check_external_rows(&store.load_directory().unwrap())
                .unwrap_err();
            assert!(
                error.contains("ext-other") && error.contains("id"),
                "{error}"
            );
            store.execute_raw("DELETE FROM external_services");
            // A good row passes.
            store.execute_raw(
                "INSERT INTO external_services (id, name, href, created_at, description) VALUES ('ext-wiki', 'Wiki', 'https://wiki.example.org/', 1, 'The wiki')",
            );
            assert!(
                crate::bootstrap::check_external_rows(&store.load_directory().unwrap()).is_ok()
            );
            store.execute_raw("DELETE FROM external_services");
            // An image whose bytes are not the declared type: the read stops and names the row.
            store.execute_raw(&format!(
                "INSERT INTO external_services (id, name, href, created_at, icon_type, icon) VALUES ('ext-wiki', 'Wiki', 'https://wiki.example.org/', 1, 'image/png', {})",
                blob(backend, "68656c6c6f")
            ));
            let error = store.load_directory().expect_err("the row is refused");
            assert!(
                error.contains("ext-wiki")
                    && error.contains("external_services")
                    && error.contains("icon"),
                "{error}"
            );
            store.execute_raw("DELETE FROM external_services");
            // An SVG with a script, written by hand, is refused too.
            let script = "<svg xmlns=\"http://www.w3.org/2000/svg\"><script>x</script></svg>"
                .bytes()
                .map(|b| format!("{b:02x}"))
                .collect::<String>();
            store.execute_raw(&format!(
                "INSERT INTO external_services (id, name, href, created_at, icon_type, icon) VALUES ('ext-wiki', 'Wiki', 'https://wiki.example.org/', 1, 'image/svg+xml', {})",
                blob(backend, &script)
            ));
            let error = store.load_directory().expect_err("the row is refused");
            assert!(
                error.contains("ext-wiki") && error.contains("script"),
                "{error}"
            );
        });
    }

    #[test]
    fn the_import_of_a_sqlite_file_keeps_the_description_and_the_icon() {
        let Some(pg) = postgres() else { return };
        let dir = std::env::temp_dir().join(format!("portal-import-icon-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("portal.db");
        let mut entry = external("Wiki", "https://wiki.example.org/");
        entry.description = "The wiki".into();
        entry.icon = Some(png_icon());
        {
            let source = Store::open(&path).unwrap();
            source.insert_external(&entry, 5).unwrap();
        }
        let target = pg.fresh();
        assert_eq!(target.import_sqlite(&path).unwrap().externals, 1);
        assert_eq!(target.load_directory().unwrap().externals, [entry]);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn the_import_of_a_sqlite_file_keeps_the_external_services() {
        let Some(pg) = postgres() else { return };
        let dir = std::env::temp_dir().join(format!("portal-import-ext-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("portal.db");
        {
            let source = Store::open(&path).unwrap();
            source
                .insert_external(&external("Wiki", "https://wiki.example.org/"), 5)
                .unwrap();
        }
        let target = pg.fresh();
        let report = target.import_sqlite(&path).unwrap();
        assert_eq!(report.externals, 1);
        assert_eq!(
            target.load_directory().unwrap().externals,
            [external("Wiki", "https://wiki.example.org/")]
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_key_is_read_back_with_its_owner_its_grants_and_its_counter_rows() {
        each_backend(|backend| {
            let store = backend.fresh();
            store.insert_user(&user("alice", &[])).unwrap();
            store
                .insert_key(&key("k1", Some("alice"), 1), &caps())
                .unwrap();
            store.insert_key(&key("k0", None, 2), &caps()).unwrap();
            let directory = store.load_directory().unwrap();
            assert_eq!(directory.version, version(&store));
            assert_eq!(directory.users.len(), 1);
            assert_eq!(directory.keys.len(), 2);
            let k1 = directory.keys.iter().find(|k| k.id == "k1").unwrap();
            assert_eq!(*k1, key("k1", Some("alice"), 1));
            let k0 = directory.keys.iter().find(|k| k.id == "k0").unwrap();
            assert_eq!(k0.owner, None, "a key of the superadmin has no owner");
            // One counter row for each window of the budget of each key.
            assert_eq!(store.count("budget_counters"), 4);
            // The same digest cannot name two keys.
            let mut twin = key("k9", Some("alice"), 1);
            twin.name = "twin".into();
            assert!(matches!(
                store.insert_key(&twin, &caps()),
                Err(KeyWriteError::Db(_))
            ));
            assert_eq!(
                store.insert_key(&key("k8", Some("nobody"), 8), &caps()),
                Err(KeyWriteError::UnknownOwner)
            );
        });
    }

    /// The largest limit is 2^53 - 1 (protocol 6.1): the database keeps the value as it is, and the read back accepts it.
    #[test]
    fn the_largest_limit_value_comes_back_exact_on_both_backends() {
        each_backend(|backend| {
            let store = backend.fresh();
            let limits = crate::limits::Limits::from_json(
                r#"{"max_read_size":9007199254740991,"max_rows":5000000000}"#,
            )
            .unwrap();
            let big = Grant::from_path_text("query.run.read", "db", "host:prod")
                .unwrap()
                .with_limits(limits.clone());
            let mut alice = user("alice", &[]);
            alice.grants = vec![big.clone()];
            store.insert_user(&alice).unwrap();
            let back = store.load_directory().unwrap().users;
            assert_eq!(back[0].grants, vec![big]);
            assert_eq!(back[0].grants[0].limits, limits);
        });
    }

    #[test]
    fn grant_roundtrip_confirm_both_backends() {
        use crate::keys::Interaction;
        each_backend(|backend| {
            let store = backend.fresh();
            let asking = Grant::from_path_text("data.write", "db", "host:prod")
                .unwrap()
                .with_confirm(true);
            let plain = Grant::from_path_text("data.write", "db", "host:dev").unwrap();
            let mut grants = vec![asking.clone(), plain.clone()];
            grants.sort();
            let mut alice = user("alice", &[]);
            alice.grants = grants.clone();
            store.insert_user(&alice).unwrap();
            // The grants of an account come back with confirm.
            let back = store.load_directory().unwrap().users;
            assert_eq!(back[0].grants, grants);
            assert!(back[0].grants.iter().any(|g| g.confirm));
            assert!(back[0].grants.iter().any(|g| !g.confirm));
            // A change of the grants keeps the property too.
            let flipped = vec![
                asking.clone().with_confirm(false),
                plain.clone().with_confirm(true),
            ];
            let mut flipped = flipped;
            flipped.sort();
            store.update_user("alice", None, Some(&flipped)).unwrap();
            assert_eq!(store.load_directory().unwrap().users[0].grants, flipped);
            // The grants of a key, and its interaction.
            let mut one = key("k1", Some("alice"), 1);
            one.grants = grants.clone();
            one.interaction = Interaction::OneShot;
            one.agents = crate::keys::AgentsMode::Enrolled;
            store.insert_key(&one, &caps()).unwrap();
            let mut two = key("k2", Some("alice"), 2);
            two.kind = crate::keys::KeyKind::Api;
            two.interaction = Interaction::Conversational;
            two.agents = crate::keys::AgentsMode::Key;
            store.insert_key(&two, &caps()).unwrap();
            let directory = store.load_directory().unwrap();
            let k1 = directory.keys.iter().find(|k| k.id == "k1").unwrap();
            assert_eq!(
                *k1, one,
                "grants with confirm, the interaction and the mode of the agents"
            );
            let k2 = directory.keys.iter().find(|k| k.id == "k2").unwrap();
            assert_eq!(k2.interaction, Interaction::Conversational);
            assert_eq!(k2.agents, crate::keys::AgentsMode::Key);
            assert!(k2.grants.iter().all(|g| !g.confirm));
            // The list of the keys has the interaction and the grants with confirm.
            let items = store.list_keys(KeyScope::All, None, 10).unwrap();
            let item = items.iter().find(|i| i.id == "k1").unwrap();
            assert_eq!(item.interaction, Some(Interaction::OneShot));
            assert_eq!(item.agents, Some(crate::keys::AgentsMode::Enrolled));
            assert_eq!(item.grants, grants);
            // The change of a key writes the interaction and the grants.
            store
                .update_key(
                    "k1",
                    Some("alice"),
                    "k1",
                    KeyModes {
                        interaction: Interaction::Conversational,
                        agents: crate::keys::AgentsMode::Enrolled,
                    },
                    std::slice::from_ref(&plain),
                    &caps(),
                )
                .unwrap();
            let after = store.load_directory().unwrap();
            let k1 = after.keys.iter().find(|k| k.id == "k1").unwrap();
            assert_eq!(k1.interaction, Interaction::Conversational);
            assert_eq!(
                k1.agents,
                crate::keys::AgentsMode::Enrolled,
                "the change writes the mode of the agents"
            );
            assert_eq!(k1.grants, [plain]);
            // A revoked key has no interaction in the list.
            assert!(store.revoke_key("k2", None, NOW).unwrap());
            let items = store.list_keys(KeyScope::All, None, 10).unwrap();
            let gone = items.iter().find(|i| i.id == "k2").unwrap();
            assert_eq!((gone.interaction, gone.agents), (None, None));
        });
    }

    #[test]
    fn a_user_has_at_most_max_per_user_active_keys() {
        each_backend(|backend| {
            let store = backend.fresh();
            store.insert_user(&user("alice", &[])).unwrap();
            store.insert_user(&user("bob", &[])).unwrap();
            // An expired key and a too-old key are not active (portal 5.4, 5.5b).
            let mut expired = key("e1", Some("alice"), 11);
            expired.expires_at = Some(NOW - 1);
            let mut old = key("o1", Some("alice"), 12);
            old.created_at = NOW - 5_000;
            store.insert_key(&expired, &caps()).unwrap();
            let mut with_age = caps();
            with_age.created_not_before = Some(NOW - 1_000);
            store.insert_key(&old, &with_age).unwrap();
            for (n, id) in ["a1", "a2", "a3"].into_iter().enumerate() {
                store
                    .insert_key(&key(id, Some("alice"), n as u8 + 1), &with_age)
                    .unwrap();
            }
            assert_eq!(
                store.insert_key(&key("a4", Some("alice"), 4), &with_age),
                Err(KeyWriteError::TooManyKeys)
            );
            // The refusal writes nothing, not even the version.
            let after_refusal = version(&store);
            assert_eq!(store.load_directory().unwrap().keys.len(), 5);
            // Without the age bound, the old key counts: alice is above the maximum.
            assert_eq!(
                store.insert_key(&key("a5", Some("alice"), 5), &caps()),
                Err(KeyWriteError::TooManyKeys)
            );
            assert_eq!(version(&store), after_refusal);
            // The maximum is for each owner. The superadmin has its own count.
            store
                .insert_key(&key("b1", Some("bob"), 21), &with_age)
                .unwrap();
            store.insert_key(&key("s1", None, 22), &with_age).unwrap();
        });
    }

    #[test]
    fn a_write_above_the_capacity_changes_nothing() {
        each_backend(|backend| {
            let store = backend.fresh();
            store.insert_user(&user("alice", &[])).unwrap();
            let mut caps = caps();
            // One key with a budget of two windows needs two counter rows.
            caps.max_counter_rows = 3;
            store
                .insert_key(&key("k1", Some("alice"), 1), &caps)
                .unwrap();
            let before = version(&store);
            assert_eq!(
                store.insert_key(&key("k2", Some("alice"), 2), &caps),
                Err(KeyWriteError::Capacity)
            );
            assert_eq!(store.count("keys"), 1);
            assert_eq!(store.count("budget_counters"), 2);
            assert_eq!(version(&store), before);
            caps.max_counter_rows = 100;
            caps.max_directory_grants = 3;
            assert_eq!(
                store.insert_key(&key("k3", Some("alice"), 3), &caps),
                Err(KeyWriteError::Capacity),
                "the key adds 2 grants to the 2 of the first key"
            );
            assert_eq!(store.count("key_grants"), 2);
        });
    }

    #[test]
    fn a_key_changes_only_for_its_owner() {
        each_backend(|backend| {
            let store = backend.fresh();
            store.insert_user(&user("alice", &[])).unwrap();
            store.insert_user(&user("bob", &[])).unwrap();
            store
                .insert_key(&key("k1", Some("alice"), 1), &caps())
                .unwrap();
            store.insert_key(&key("s1", None, 2), &caps()).unwrap();
            let one = vec![Grant::new("access", "other").unwrap()];
            for (owner, id) in [
                (Some("bob"), "k1"),
                (None, "k1"),
                (Some("alice"), "s1"),
                (Some("nobody"), "s1"),
                (Some("alice"), "missing"),
            ] {
                assert_eq!(
                    store.update_key(id, owner, "renamed", modes(), &one, &caps()),
                    Err(KeyWriteError::UnknownKey),
                    "{owner:?} {id}"
                );
            }
            store
                .update_key("k1", Some("alice"), "renamed", modes(), &one, &caps())
                .unwrap();
            store
                .update_key("s1", None, "root", modes(), &one, &caps())
                .unwrap();
            let directory = store.load_directory().unwrap();
            let k1 = directory.keys.iter().find(|k| k.id == "k1").unwrap();
            assert_eq!((k1.name.as_str(), &k1.grants), ("renamed", &one));
            assert_eq!(k1.digest, [1; 32], "the digest does not change");
            // The old counter rows of the key stay with the key (portal 7.2).
            assert_eq!(store.count("budget_counters"), 4);
        });
    }

    #[test]
    fn a_revocation_deletes_the_key_with_its_rows_and_keeps_one_summary_row() {
        each_backend(|backend| {
            let store = backend.fresh();
            store.insert_user(&user("alice", &[])).unwrap();
            store.insert_user(&user("bob", &[])).unwrap();
            store
                .insert_key(&key("k1", Some("alice"), 1), &caps())
                .unwrap();
            store
                .insert_key(&key("k2", Some("bob"), 2), &caps())
                .unwrap();
            store.insert_key(&key("s1", None, 3), &caps()).unwrap();
            store
                .write_key_checks(&[KeyCheck {
                    id: "k1".into(),
                    last_check_at: NOW - 5,
                    count: 7,
                }])
                .unwrap();
            // The owner limits the revocation.
            assert!(!store.revoke_key("k1", Some(Some("bob")), NOW).unwrap());
            assert!(!store.revoke_key("k1", Some(None), NOW).unwrap());
            assert!(store.revoke_key("k1", Some(Some("alice")), NOW).unwrap());
            assert!(!store.revoke_key("k1", None, NOW).unwrap());
            assert_eq!(store.count("keys"), 2);
            assert_eq!(store.count("key_grants"), 4);
            assert_eq!(store.count("budget_counters"), 4, "the rows of k1 are gone");
            let history = store.key_history().unwrap();
            assert_eq!(history.len(), 1);
            assert_eq!(
                history[0],
                KeyHistoryRow {
                    id: "k1".into(),
                    owner: Some("alice".into()),
                    name: "key k1".into(),
                    created_at: NOW - 10,
                    revoked_at: NOW,
                    last_check_at: Some(NOW - 5),
                    check_count: 7,
                }
            );
            // The superadmin key is revoked with `Some(None)`.
            assert!(store.revoke_key("s1", Some(None), NOW + 1).unwrap());
            assert_eq!(store.key_history().unwrap()[1].owner, None);
            // All the keys of one user, then all the keys.
            store
                .insert_key(&key("k3", Some("bob"), 4), &caps())
                .unwrap();
            assert_eq!(store.revoke_keys(Some("bob"), NOW + 2).unwrap(), 2);
            assert_eq!(store.revoke_keys(Some("bob"), NOW + 2).unwrap(), 0);
            store
                .insert_key(&key("k4", Some("alice"), 5), &caps())
                .unwrap();
            store.insert_key(&key("s2", None, 6), &caps()).unwrap();
            assert_eq!(store.revoke_keys(None, NOW + 3).unwrap(), 2);
            assert_eq!(store.count("keys"), 0);
            assert_eq!(store.count("budget_counters"), 0);
            assert_eq!(store.key_history().unwrap().len(), 6);
        });
    }

    #[test]
    fn a_checks_write_keeps_the_later_time_and_adds_the_counts() {
        each_backend(|backend| {
            let store = backend.fresh();
            store.insert_user(&user("alice", &[])).unwrap();
            store
                .insert_key(&key("k1", Some("alice"), 1), &caps())
                .unwrap();
            let check = |at, count| KeyCheck {
                id: "k1".into(),
                last_check_at: at,
                count,
            };
            store.write_key_checks(&[check(NOW, 2)]).unwrap();
            store.write_key_checks(&[check(NOW - 100, 3)]).unwrap();
            store
                .write_key_checks(&[
                    check(NOW + 5, 1),
                    KeyCheck {
                        id: "gone".into(),
                        last_check_at: 1,
                        count: 1,
                    },
                ])
                .unwrap();
            let directory = store.load_directory().unwrap();
            assert_eq!(directory.keys[0].last_check_at, Some(NOW + 5));
            assert_eq!(directory.keys[0].check_count, 6);
        });
    }

    #[test]
    fn deleting_a_user_deletes_the_keys_but_not_the_keys_of_the_superadmin() {
        each_backend(|backend| {
            let store = backend.fresh();
            store.insert_user(&user("alice", &[])).unwrap();
            store
                .insert_key(&key("k1", Some("alice"), 1), &caps())
                .unwrap();
            store.insert_key(&key("s1", None, 2), &caps()).unwrap();
            store.revoke_key("s1", None, NOW).unwrap();
            store.insert_key(&key("s2", None, 3), &caps()).unwrap();
            store.revoke_key("k1", None, NOW).unwrap();
            store
                .insert_key(&key("k5", Some("alice"), 4), &caps())
                .unwrap();
            assert!(store.remove_user("alice").unwrap());
            let directory = store.load_directory().unwrap();
            assert_eq!(
                directory
                    .keys
                    .iter()
                    .map(|k| k.id.as_str())
                    .collect::<Vec<_>>(),
                ["s2"]
            );
            assert_eq!(store.count("budget_counters"), 2, "the rows of s2");
            assert_eq!(store.count("key_grants"), 2);
            let history = store.key_history().unwrap();
            assert_eq!(
                history.iter().map(|h| h.id.as_str()).collect::<Vec<_>>(),
                ["s1"],
                "the summary rows of the user go with the user (portal 5.7)"
            );
        });
    }

    #[test]
    fn the_pages_of_the_keys_are_newest_first_with_a_cursor_and_hold_the_summary_rows() {
        each_backend(|backend| {
            let store = backend.fresh();
            let big = KeyCaps {
                max_per_user: 100,
                ..caps()
            };
            store.insert_user(&user("alice", &[])).unwrap();
            store.insert_user(&user("bob", &[])).unwrap();
            // Five lines for Alice: four keys (two with the same time) and one revoked key.
            for (id, byte, created) in [
                ("a1", 1u8, 100),
                ("a2", 2, 200),
                ("a3", 3, 200),
                ("a4", 4, 400),
                ("a5", 5, 300),
            ] {
                let mut row = key(id, Some("alice"), byte);
                row.created_at = created;
                store.insert_key(&row, &big).unwrap();
            }
            store.revoke_key("a5", None, NOW).unwrap();
            let mut other = key("b1", Some("bob"), 9);
            other.created_at = 250;
            store.insert_key(&other, &big).unwrap();
            let mut root = key("s1", None, 10);
            root.created_at = 150;
            store.insert_key(&root, &big).unwrap();
            let ids =
                |items: &[KeyListItem]| items.iter().map(|i| i.id.clone()).collect::<Vec<_>>();
            // The section of Alice: newest first, and the id breaks a tie.
            let first = store.list_keys(KeyScope::Owner("alice"), None, 3).unwrap();
            assert_eq!(ids(&first), ["a4", "a5", "a3"]);
            assert_eq!(first[1].revoked_at, Some(NOW), "the summary row");
            assert!(first[1].grants.is_empty());
            assert_eq!(first[0].grants, key("a4", Some("alice"), 4).grants);
            let last = first.last().unwrap();
            let second = store
                .list_keys(
                    KeyScope::Owner("alice"),
                    Some((last.created_at, &last.id)),
                    3,
                )
                .unwrap();
            assert_eq!(ids(&second), ["a2", "a1"]);
            // The keys of the superadmin, and all the keys.
            assert_eq!(
                ids(&store.list_keys(KeyScope::Superadmin, None, 50).unwrap()),
                ["s1"]
            );
            let all = store.list_keys(KeyScope::All, None, 50).unwrap();
            assert_eq!(ids(&all), ["a4", "a5", "b1", "a3", "a2", "s1", "a1"]);
            assert_eq!(
                all.iter().find(|i| i.id == "b1").unwrap().owner.as_deref(),
                Some("bob")
            );
            assert_eq!(all.iter().find(|i| i.id == "s1").unwrap().owner, None);
            assert!(store
                .list_keys(KeyScope::Owner("nobody"), None, 50)
                .unwrap()
                .is_empty());
        });
    }

    #[test]
    fn the_prune_deletes_old_expired_keys_and_old_summary_rows() {
        each_backend(|backend| {
            let store = backend.fresh();
            store.insert_user(&user("alice", &[])).unwrap();
            let mut gone = key("gone", Some("alice"), 1);
            gone.expires_at = Some(NOW - 100);
            let mut recent = key("recent", Some("alice"), 2);
            recent.expires_at = Some(NOW - 10);
            let forever = key("forever", Some("alice"), 3);
            for key in [&gone, &recent, &forever] {
                store.insert_key(key, &caps()).unwrap();
            }
            store
                .insert_key(&key("r1", Some("alice"), 4), &caps())
                .unwrap();
            store.revoke_key("r1", None, NOW - 50).unwrap();
            store
                .insert_key(&key("r2", Some("alice"), 5), &caps())
                .unwrap();
            store.revoke_key("r2", None, NOW).unwrap();
            let before = version(&store);
            store.prune_keys(NOW - 50, NOW - 20).unwrap();
            let ids: Vec<String> = store
                .load_directory()
                .unwrap()
                .keys
                .into_iter()
                .map(|k| k.id)
                .collect();
            assert!(!ids.contains(&"gone".to_string()));
            assert!(ids.contains(&"recent".to_string()) && ids.contains(&"forever".to_string()));
            assert_eq!(version(&store), before + 1);
            let history = store.key_history().unwrap();
            assert_eq!(history.len(), 1);
            assert_eq!(history[0].id, "r2");
            store.prune_keys(NOW - 50, NOW - 20).unwrap();
            assert_eq!(
                version(&store),
                before + 1,
                "nothing to delete: no new version"
            );
        });
    }

    fn counter(
        owner: crate::decision::CounterOwner,
        window: crate::limits::Window,
        amount: u64,
    ) -> crate::decision::Counter {
        crate::decision::Counter {
            owner,
            id: crate::grants::CounterId {
                service: "db".into(),
                budget: "requests".into(),
                window,
            },
            amount,
        }
    }

    /// Alice with a budget of one window each of the second and the minute, and one key.
    fn alice_with_counters(store: &Store) {
        use crate::limits::Limits;
        let budget = Limits::from_json(
            r#"{"requests":[{"amount":5,"window":"second"},{"amount":50,"window":"minute"}]}"#,
        )
        .unwrap();
        let mut row = user("alice", &[]);
        row.grants = vec![Grant::new("access", "db").unwrap().with_limits(budget)];
        store.insert_user(&row).unwrap();
        store
            .insert_key(&key("k1", Some("alice"), 1), &caps())
            .unwrap();
    }

    #[test]
    fn a_use_takes_one_unit_from_each_counter_until_one_has_no_room() {
        use crate::decision::CounterOwner as Owner;
        use crate::limits::Window;
        each_backend(|backend| {
            let store = backend.fresh();
            alice_with_counters(&store);
            let alice = Owner::User("alice".into());
            let one_a_minute = vec![counter(alice.clone(), Window::Minute, 1)];
            assert_eq!(
                store.count_use(&one_a_minute, None).unwrap(),
                CountOutcome::Counted
            );
            let CountOutcome::Refused(refusals) = store.count_use(&one_a_minute, None).unwrap()
            else {
                panic!("the second use must be refused");
            };
            assert_eq!(refusals.len(), 1);
            assert_eq!(refusals[0].index, 0);
            assert!(
                refusals[0].seconds_left > 0.0 && refusals[0].seconds_left <= 60.0,
                "{}",
                refusals[0].seconds_left
            );
            // A counter with room: the amount is a parameter of the use. The refused use took
            // nothing, so the counter has 1 unit and 2 uses are allowed.
            let two = vec![counter(alice.clone(), Window::Minute, 2)];
            assert_eq!(store.count_use(&two, None).unwrap(), CountOutcome::Counted);
            assert!(matches!(
                store.count_use(&two, None).unwrap(),
                CountOutcome::Refused(_)
            ));
        });
    }

    #[test]
    fn a_use_takes_nothing_when_one_counter_of_the_use_has_no_room() {
        use crate::decision::CounterOwner as Owner;
        use crate::limits::Window;
        each_backend(|backend| {
            let store = backend.fresh();
            alice_with_counters(&store);
            let alice = Owner::User("alice".into());
            let key = Owner::Key("k1".into());
            let both = |key_amount: u64, owner_amount: u64| {
                vec![
                    counter(key.clone(), Window::Minute, key_amount),
                    counter(alice.clone(), Window::Minute, owner_amount),
                    counter(alice.clone(), Window::Second, 5),
                ]
            };
            // The owner has room for one use. The key has room for three.
            assert_eq!(
                store.count_use(&both(3, 1), None).unwrap(),
                CountOutcome::Counted
            );
            let CountOutcome::Refused(refusals) = store.count_use(&both(3, 1), None).unwrap()
            else {
                panic!("the owner has no room");
            };
            assert_eq!(refusals.iter().map(|r| r.index).collect::<Vec<_>>(), [1]);
            // The key counter did not move: with an amount of 2 it still has room for one use.
            let key_only = vec![counter(key.clone(), Window::Minute, 2)];
            assert_eq!(
                store.count_use(&key_only, None).unwrap(),
                CountOutcome::Counted
            );
            assert!(matches!(
                store.count_use(&key_only, None).unwrap(),
                CountOutcome::Refused(_)
            ));
            // Two counters can refuse together: each one is in the answer.
            let CountOutcome::Refused(refusals) = store.count_use(&both(1, 1), None).unwrap()
            else {
                panic!("both refuse");
            };
            let mut indexes: Vec<usize> = refusals.iter().map(|r| r.index).collect();
            indexes.sort();
            assert_eq!(indexes, [0, 1]);
        });
    }

    #[test]
    fn a_window_that_ended_starts_again_with_no_unit() {
        use crate::decision::CounterOwner as Owner;
        use crate::limits::Window;
        each_backend(|backend| {
            let store = backend.fresh();
            alice_with_counters(&store);
            let one = vec![counter(Owner::User("alice".into()), Window::Second, 1)];
            // The window of one second can end between two statements: try until two statements
            // fall in the same window.
            let mut refused = false;
            for _ in 0..20 {
                if matches!(
                    store.count_use(&one, None).unwrap(),
                    CountOutcome::Refused(_)
                ) {
                    refused = true;
                    break;
                }
            }
            assert!(refused, "the second use of a window is refused");
            std::thread::sleep(std::time::Duration::from_millis(1100));
            assert_eq!(store.count_use(&one, None).unwrap(), CountOutcome::Counted);
        });
    }

    #[test]
    fn a_counter_without_a_row_is_reported_and_nothing_is_counted() {
        use crate::decision::CounterOwner as Owner;
        use crate::limits::Window;
        each_backend(|backend| {
            let store = backend.fresh();
            alice_with_counters(&store);
            let alice = Owner::User("alice".into());
            let missing = vec![
                counter(alice.clone(), Window::Minute, 1),
                counter(alice.clone(), Window::Hour, 1),
            ];
            assert_eq!(
                store.count_use(&missing, None).unwrap(),
                CountOutcome::RowMissing
            );
            // The first counter did not move.
            let first = vec![counter(alice, Window::Minute, 1)];
            assert_eq!(
                store.count_use(&first, None).unwrap(),
                CountOutcome::Counted
            );
            let nobody = vec![counter(Owner::User("nobody".into()), Window::Minute, 1)];
            assert_eq!(
                store.count_use(&nobody, None).unwrap(),
                CountOutcome::RowMissing
            );
            let no_key = vec![counter(Owner::Key("nokey".into()), Window::Minute, 1)];
            assert_eq!(
                store.count_use(&no_key, None).unwrap(),
                CountOutcome::RowMissing
            );
        });
    }

    #[test]
    fn counter_rows_are_made_once_and_the_rows_of_the_file_follow_the_file() {
        use crate::decision::CounterOwner as Owner;
        use crate::grants::CounterId;
        use crate::limits::Window;
        each_backend(|backend| {
            let store = backend.fresh();
            alice_with_counters(&store);
            let id = |window| CounterId {
                service: "db".into(),
                budget: "requests".into(),
                window,
            };
            let rows = vec![
                (Owner::User("alice".into()), id(Window::Second)), // exists
                (Owner::User("alice".into()), id(Window::Hour)),   // new
                (Owner::Key("k1".into()), id(Window::Hour)),       // new
                (Owner::File("carol".into()), id(Window::Day)),    // new
                (Owner::File("key:34b1100f45b235ba".into()), id(Window::Day)), // new
                (Owner::User("nobody".into()), id(Window::Day)),   // no such user: skipped
                (Owner::Key("nokey".into()), id(Window::Day)),     // no such key: skipped
            ];
            let before = store.counter_rows().unwrap();
            assert_eq!(store.ensure_counter_rows(&rows).unwrap(), 4);
            assert_eq!(store.counter_rows().unwrap(), before + 4);
            assert_eq!(
                store.ensure_counter_rows(&rows).unwrap(),
                0,
                "a second call makes nothing"
            );
            // A row of the file counts like any other.
            let file = vec![counter(Owner::File("carol".into()), Window::Day, 1)];
            assert_eq!(store.count_use(&file, None).unwrap(), CountOutcome::Counted);
            assert!(matches!(
                store.count_use(&file, None).unwrap(),
                CountOutcome::Refused(_)
            ));
            // The file no longer has `carol` (portal 3.4d).
            assert_eq!(
                store
                    .delete_file_counters(&["key:34b1100f45b235ba".to_string()])
                    .unwrap(),
                1
            );
            assert_eq!(
                store.count_use(&file, None).unwrap(),
                CountOutcome::RowMissing
            );
            assert_eq!(store.counter_rows().unwrap(), before + 3);
        });
    }

    #[test]
    fn the_budget_counter_rows_follow_the_grants_of_a_user() {
        use crate::limits::Limits;
        each_backend(|backend| {
            let store = backend.fresh();
            let budget = Limits::from_json(
                r#"{"requests":[{"amount":5,"window":"hour"},{"amount":50,"window":"day"}]}"#,
            )
            .unwrap();
            let mut row = user("alice", &[]);
            row.grants = vec![Grant::new("access", "db").unwrap().with_limits(budget)];
            store.insert_user(&row).unwrap();
            assert_eq!(store.count("budget_counters"), 2);
            // A new window adds a row. A removed value leaves its row: only the user deletes it.
            let wider =
                Limits::from_json(r#"{"requests":[{"amount":5,"window":"minute"}]}"#).unwrap();
            let grants = vec![Grant::new("access", "db").unwrap().with_limits(wider)];
            store.update_user("alice", None, Some(&grants)).unwrap();
            assert_eq!(store.count("budget_counters"), 3);
            store.remove_user("alice").unwrap();
            assert_eq!(store.count("budget_counters"), 0);
        });
    }

    #[test]
    fn the_limit_values_of_a_grant_survive_a_write_a_read_and_an_update() {
        use crate::limits::Limits;
        each_backend(|backend| {
            let store = backend.fresh();
            let budget = Limits::from_json(
                r#"{"requests":[{"amount":10,"window":"second"},{"amount":300,"window":"minute"}]}"#,
            )
            .unwrap();
            let ceiling = Limits::from_json(r#"{"max_rows":1000}"#).unwrap();
            let grants = vec![
                Grant::new("access", "db").unwrap().with_limits(budget),
                Grant::from_path_text("data.read", "db", "host:prod/database:test1")
                    .unwrap()
                    .with_limits(ceiling),
                Grant::new("keys.own.read", "portal").unwrap(),
            ];
            let mut row = user("alice", &[]);
            row.grants = grants.clone();
            row.grants.sort();
            store.insert_user(&row).unwrap();
            let loaded = store.load_users().unwrap();
            assert_eq!(loaded[0].grants, row.grants);
            let read = loaded[0]
                .grants
                .iter()
                .find(|g| g.action == "data.read")
                .unwrap();
            assert_eq!(read.limits.to_json(), r#"{"max_rows":1000}"#);
            assert!(loaded[0]
                .grants
                .iter()
                .find(|g| g.action == "keys.own.read")
                .unwrap()
                .limits
                .is_empty());
            // An update replaces the values with the grants.
            let lower = Limits::from_json(r#"{"max_rows":10}"#).unwrap();
            let changed =
                vec![
                    Grant::from_path_text("data.read", "db", "host:prod/database:test1")
                        .unwrap()
                        .with_limits(lower),
                ];
            assert!(store.update_user("alice", None, Some(&changed)).unwrap());
            assert_eq!(store.load_users().unwrap()[0].grants, changed);
        });
    }

    #[test]
    fn users_round_trip_with_their_grants_and_reject_a_duplicate() {
        each_backend(|backend| {
            let store = backend.fresh();
            store.insert_user(&user("alice", &["ops", "dev"])).unwrap();
            store.insert_user(&user("bob", &[])).unwrap();
            assert_eq!(
                store.insert_user(&user("alice", &[])),
                Err(InsertError::Exists)
            );
            let users = store.load_users().unwrap();
            assert_eq!(users.len(), 2);
            assert_eq!(users[0].login, "alice");
            assert_eq!(users[0].grants, access(&["dev", "ops"]), "sorted");
            assert!(users[1].grants.is_empty());
            assert!(store.remove_user("alice").unwrap());
            assert!(!store.remove_user("alice").unwrap());
            assert_eq!(store.load_users().unwrap().len(), 1);
        });
    }

    #[test]
    fn updating_a_user_replaces_its_grants_and_its_password_each_when_given() {
        each_backend(|backend| {
            let store = backend.fresh();
            store.insert_user(&user("alice", &["ops", "dev"])).unwrap();
            store.insert_user(&user("bob", &["ops"])).unwrap();
            let row = |login: &str| {
                store
                    .load_users()
                    .unwrap()
                    .into_iter()
                    .find(|u| u.login == login)
                    .unwrap()
            };
            let mut viewer = access(&["viewer"]);
            viewer.push(Grant::new("query.run.read", "viewer").unwrap());
            for text in [
                "query.run.write on service:viewer/database:b",
                "query.run.write on service:viewer/database:a",
                "query.run.write on service:viewer/table:a",
                "query.run.write on service:viewer/host:h/database:a",
                "query.run.write on service:viewer/host:h/database:a%2Fb%20c",
                "query.run.write on service:viewer/host:h%3A1/database:%C3%A9",
                "query.run.read on service:viewer/a:1/b:2/c:3/d:4",
            ] {
                viewer.push(Grant::parse(text).unwrap());
            }
            viewer.sort();
            assert!(store.update_user("alice", None, Some(&viewer)).unwrap());
            assert_eq!(row("alice").grants, viewer);
            assert_eq!(
                row("alice").password_hash,
                "$argon2id$stub",
                "kept without a new hash"
            );
            assert!(store.update_user("alice", Some("new-hash"), None).unwrap());
            assert_eq!(row("alice").password_hash, "new-hash");
            assert_eq!(row("alice").grants, viewer, "kept without grants");
            assert!(store.update_user("alice", None, Some(&[])).unwrap());
            assert!(row("alice").grants.is_empty());
            assert_eq!(
                row("bob").grants,
                access(&["ops"]),
                "other users are untouched"
            );
            assert!(!store.update_user("nobody", None, Some(&[])).unwrap());
        });
    }

    #[test]
    fn end_all_sessions_needs_a_shared_database_and_changes_nothing_without_one() {
        each_backend(|backend| {
            let store = backend.fresh();
            if store.is_shared() {
                assert_eq!(store.end_all_sessions(NOW), Ok(0));
                assert_eq!(store.count("audit_events"), 1, "the command is audited");
            } else {
                let error = store.end_all_sessions(NOW).unwrap_err();
                assert!(error.contains("restart the portal"), "{error}");
                assert_eq!(store.count("audit_events"), 0);
            }
        });
    }

    #[test]
    fn deleting_a_user_deletes_its_grants_but_keeps_its_history() {
        each_backend(|backend| {
            let store = backend.fresh();
            store.insert_user(&user("alice", &["ops"])).unwrap();
            store
                .apply(&Batch {
                    events: vec![start(1, "alice", false, false, NOW)],
                    ..Batch::default()
                })
                .unwrap();
            store.remove_user("alice").unwrap();
            assert_eq!(store.count("user_grants"), 0);
            let snap = store.snapshot(&[("alice".into(), false)], NOW).unwrap();
            assert_eq!(snap.accounts[0].recent.len(), 1);
        });
    }

    #[test]
    fn a_session_lifecycle_services_and_failures_show_up_in_the_snapshot() {
        each_backend(|backend| {
            let store = backend.fresh();
            let day = NOW - NOW.rem_euclid(DAY);
            let mut hits = HashMap::new();
            hits.insert(
                (1u64, "grafana".to_string()),
                Hit {
                    first_at: day + 100,
                    last_at: day + 200,
                    hits: 3,
                    ip: Some("10.0.0.2".into()),
                },
            );
            let mut seen = HashMap::new();
            seen.insert(1u64, day + 400);
            store
                .apply(&Batch {
                    events: vec![
                        start(1, "alice", false, false, day + 50),
                        start(2, "alice", false, true, day + 60),
                        start(3, "root", true, false, day + 70),
                        Event::Audit(AuditEvent {
                            kind: AuditKind::LoginFailed,
                            login: Some("alice".into()),
                            actor: None,
                            ip: Some("10.0.0.9".into()),
                            user_agent: "x".into(),
                            at: day + 80,
                        }),
                        Event::Audit(AuditEvent {
                            kind: AuditKind::LoginFailed,
                            login: None,
                            actor: None,
                            ip: None,
                            user_agent: "x".into(),
                            at: day + 90,
                        }),
                        Event::SessionEnded {
                            sid: 1,
                            reason: EndReason::Logout,
                            at: day + 500,
                        },
                    ],
                    hits,
                    seen,
                })
                .unwrap();
            let snap = store
                .snapshot(&[("alice".into(), false), ("root".into(), true)], NOW)
                .unwrap();
            let alice = &snap.accounts[0];
            assert_eq!(alice.recent.len(), 2, "the impersonated session is listed");
            assert!(alice.recent.iter().any(|s| s.impersonated));
            let first = alice.recent.iter().find(|s| !s.impersonated).unwrap();
            assert_eq!(first.end_reason.as_deref(), Some("logout"));
            assert_eq!(first.ended_at, Some(day + 500));
            assert_eq!(
                alice.sessions_30d, 1,
                "an impersonated session is not a connection"
            );
            assert_eq!(alice.days[GRAPH_DAYS - 1].sessions, 1);
            assert_eq!(alice.days[GRAPH_DAYS - 1].seconds, 450);
            assert_eq!(
                alice.services,
                [ServiceUse {
                    service_id: "grafana".into(),
                    hits: 3,
                    last_at: day + 200
                }]
            );
            assert_eq!(alice.failed_30d, 1);
            assert_eq!(alice.last_login, Some(day + 50));
            assert_eq!(
                snap.accounts[1].sessions_30d, 1,
                "the superadmin is tracked apart"
            );
            assert_eq!(
                snap.days[GRAPH_DAYS - 1].sessions,
                2,
                "all accounts, real connections only"
            );
            assert_eq!(snap.recent_events.len(), 2);
            assert_eq!(snap.first_day, day - 29 * DAY);
        });
    }

    #[test]
    fn hits_accumulate_across_flushes_and_open_sessions_close_on_restart() {
        each_backend(|backend| {
            let store = backend.fresh();
            store
                .apply(&Batch {
                    events: vec![start(5, "alice", false, false, 100)],
                    ..Batch::default()
                })
                .unwrap();
            for last_at in [200, 300] {
                let mut hits = HashMap::new();
                hits.insert(
                    (5u64, "notes".to_string()),
                    Hit {
                        first_at: last_at,
                        last_at,
                        hits: 2,
                        ip: None,
                    },
                );
                let mut seen = HashMap::new();
                seen.insert(5u64, last_at);
                store
                    .apply(&Batch {
                        hits,
                        seen,
                        ..Batch::default()
                    })
                    .unwrap();
            }
            if store.is_shared() {
                // Sessions outlive a restart there. The sessions that nobody ended are found by their silence.
                store.close_lost_sessions(1_000, "restart").unwrap();
            } else {
                store.close_open_sessions(1_000, "restart").unwrap();
            }
            let snap = store.snapshot(&[("alice".into(), false)], 400).unwrap();
            let alice = &snap.accounts[0];
            assert_eq!(alice.services[0].hits, 4);
            assert_eq!(alice.services[0].last_at, 300);
            assert_eq!(alice.recent[0].end_reason.as_deref(), Some("restart"));
            assert_eq!(
                alice.recent[0].ended_at,
                Some(300),
                "ends when it was last seen"
            );
        });
    }

    #[test]
    fn sign_in_attempts_are_listed_newest_first_with_the_figures_of_the_chips() {
        each_backend(|backend| {
            let store = backend.fresh();
            store
                .apply(&Batch {
                    events: vec![
                        attempt("alice", true, "10.0.0.2", NOW - 3 * DAY),
                        attempt("alice", false, "10.0.0.2", NOW - 600),
                        attempt("admin", false, "203.0.113.7", NOW - 500),
                        attempt("root", false, "203.0.113.7", NOW - 400),
                        attempt("alice", true, "10.0.0.2", NOW - 300),
                    ],
                    ..Batch::default()
                })
                .unwrap();
            let all = store.login_attempts(0, false).unwrap();
            assert_eq!(all.matching, 5);
            assert_eq!(all.rows[0].ts, NOW - 300, "newest first");
            assert!(all.rows[0].success && all.rows[0].known);
            assert_eq!(all.rows[2].login, "admin");
            assert!(!all.rows[2].known);
            assert_eq!(all.rows[2].ip.as_deref(), Some("203.0.113.7"));
            assert_eq!((all.total, all.refused), (5, 3));
            let failed = store.login_attempts(0, true).unwrap();
            assert_eq!(failed.matching, 3);
            assert!(failed.rows.iter().all(|row| !row.success));
            assert_eq!(
                (failed.total, failed.refused),
                (5, 3),
                "the figures do not depend on the filter"
            );
        });
    }

    #[test]
    fn a_hostile_login_is_stored_and_read_back_as_the_text_it_is() {
        each_backend(|backend| {
            let store = backend.fresh();
            let nasty = "x'); DROP TABLE users; DELETE FROM login_attempts; --";
            store
                .apply(&Batch {
                    events: vec![attempt(nasty, false, "10.0.0.9", NOW)],
                    ..Batch::default()
                })
                .unwrap();
            store.insert_user(&user("alice", &[])).unwrap();
            let page = store.login_attempts(0, false).unwrap();
            assert_eq!(page.rows[0].login, nasty);
            assert_eq!(
                store.load_users().unwrap().len(),
                1,
                "the tables are intact"
            );
        });
    }

    #[test]
    fn the_sign_in_log_covers_a_period_and_its_figures_are_those_of_the_period() {
        each_backend(|backend| {
            let store = backend.fresh();
            store
                .apply(&Batch {
                    events: vec![
                        attempt("alice", true, "10.0.0.2", NOW - 3 * DAY),
                        attempt("alice", false, "10.0.0.2", NOW - 3 * DAY + 5),
                        attempt("alice", false, "10.0.0.2", NOW - 600),
                        attempt("alice", true, "10.0.0.2", NOW - 300),
                    ],
                    ..Batch::default()
                })
                .unwrap();
            let recent = store.login_attempts(NOW - DAY, false).unwrap();
            assert_eq!(
                recent.rows.len(),
                2,
                "the older rows are outside the period"
            );
            assert_eq!(
                (recent.matching, recent.total, recent.refused),
                (2, 2, 1),
                "the chips count what the list holds"
            );
            let failed = store.login_attempts(NOW - DAY, true).unwrap();
            assert_eq!(
                (failed.rows.len(), failed.matching, failed.total),
                (1, 1, 2)
            );
            let all = store.login_attempts(0, false).unwrap();
            assert_eq!((all.rows.len(), all.total, all.refused), (4, 4, 2));
            let none = backend.fresh().login_attempts(NOW - DAY, false).unwrap();
            assert_eq!((none.rows.len(), none.matching, none.total), (0, 0, 0));
        });
    }

    #[test]
    fn the_list_of_a_period_holds_the_newest_rows_up_to_a_hard_limit() {
        each_backend(|backend| {
            let store = backend.fresh();
            let events = (0..(WINDOW_ROWS + 5))
                .map(|i| attempt("alice", false, "10.0.0.2", NOW - 10_000 + i))
                .collect();
            store
                .apply(&Batch {
                    events,
                    ..Batch::default()
                })
                .unwrap();
            let page = store.login_attempts(0, false).unwrap();
            assert_eq!(page.rows.len() as i64, WINDOW_ROWS);
            assert_eq!(
                i64::from(page.matching),
                WINDOW_ROWS + 5,
                "the figure is whole"
            );
            assert_eq!(
                page.rows[0].ts,
                NOW - 10_000 + WINDOW_ROWS + 4,
                "the newest are the ones kept"
            );
        });
    }

    #[test]
    fn the_sign_in_log_is_pruned_by_age_and_capped_in_size() {
        each_backend(|backend| {
            let store = backend.fresh();
            store
                .apply(&Batch {
                    events: vec![
                        attempt("old", false, "10.0.0.2", 10),
                        attempt("new", false, "10.0.0.2", 900),
                    ],
                    ..Batch::default()
                })
                .unwrap();
            store.prune(500).unwrap();
            let page = store.login_attempts(0, false).unwrap();
            assert_eq!(page.matching, 1);
            assert_eq!(page.rows[0].login, "new");

            let store = backend.fresh();
            let flood = |from: i64, count: i64| Batch {
                events: (from..from + count)
                    .map(|i| attempt("bot", false, "198.51.100.1", i))
                    .collect(),
                ..Batch::default()
            };
            store.apply(&flood(0, MAX_LOGIN_ROWS - 10)).unwrap();
            store.apply(&flood(MAX_LOGIN_ROWS, 30)).unwrap();
            let page = store.login_attempts(0, false).unwrap();
            assert_eq!(
                i64::from(page.matching),
                MAX_LOGIN_ROWS,
                "the oldest rows made room"
            );
            assert_eq!(
                page.rows[0].ts,
                MAX_LOGIN_ROWS + 29,
                "the newest are the ones kept"
            );
        });
    }

    #[test]
    fn pruning_removes_old_rows_with_their_service_accesses() {
        each_backend(|backend| {
            let store = backend.fresh();
            let mut hits = HashMap::new();
            hits.insert(
                (1u64, "a".to_string()),
                Hit {
                    first_at: 10,
                    last_at: 10,
                    hits: 1,
                    ip: None,
                },
            );
            hits.insert(
                (2u64, "a".to_string()),
                Hit {
                    first_at: 900,
                    last_at: 900,
                    hits: 1,
                    ip: None,
                },
            );
            store
                .apply(&Batch {
                    events: vec![
                        start(1, "alice", false, false, 10),
                        start(2, "alice", false, false, 900),
                        Event::Audit(AuditEvent {
                            kind: AuditKind::UserAdded,
                            login: Some("bob".into()),
                            actor: Some("root".into()),
                            ip: None,
                            user_agent: "x".into(),
                            at: 10,
                        }),
                    ],
                    hits,
                    ..Batch::default()
                })
                .unwrap();
            store.prune(500).unwrap();
            assert_eq!(store.count("sessions_log"), 1);
            assert_eq!(store.count("service_access"), 1);
            assert_eq!(store.count("audit_events"), 0);
        });
    }

    #[test]
    fn the_history_keeps_whether_a_session_was_remembered() {
        each_backend(|backend| {
            let store = backend.fresh();
            let mut kept = start(1, "alice", false, false, NOW);
            if let Event::SessionStarted(start) = &mut kept {
                start.remembered = true;
            }
            store
                .apply(&Batch {
                    events: vec![kept, start(2, "alice", false, false, NOW + 1)],
                    ..Batch::default()
                })
                .unwrap();
            let snap = store.snapshot(&[("alice".into(), false)], NOW + 2).unwrap();
            let recent = &snap.accounts[0].recent;
            assert_eq!(recent.len(), 2);
            // The newest session first.
            assert!(!recent[0].remembered);
            assert!(recent[1].remembered);
        });
    }

    // PostgreSQL only: shared sessions, several instances, the import --------------------------

    const HERE: [u8; 32] = [7; 32];
    const ELSEWHERE: [u8; 32] = [8; 32];
    const IDLE: Duration = Duration::from_secs(1800);
    /// The credential generation of the superadmin in these tests.
    const GEN: [u8; 32] = [9; 32];

    fn digest(n: u8) -> [u8; 32] {
        [n; 32]
    }

    fn shared_session(sid: u64, login: Option<&str>, expires_at: i64) -> SharedSession {
        SharedSession {
            sid,
            login: login.map(str::to_string),
            impersonated: false,
            remembered: false,
            mfa: false,
            expires_at,
            credential: [4; 32],
        }
    }

    /// A limit of the live sessions, and no limit of the sessions of one principal that a test reaches.
    fn limits(max: usize) -> SessionLimits {
        SessionLimits {
            max_active: max,
            max_per_account: 1_000,
        }
    }

    fn started(sid: u64, login: &str, at: i64) -> SessionStart {
        SessionStart {
            sid,
            login: login.into(),
            superadmin: false,
            impersonated: false,
            remembered: false,
            mfa: false,
            ip: Some("10.0.0.2".into()),
            user_agent: "Browser/1.0".into(),
            at,
        }
    }

    /// Two instances on one database.
    fn pair(backend: &Backend) -> (Store, Store) {
        backend.fresh_pair()
    }

    #[test]
    fn sqlite_holds_no_shared_sessions_and_cannot_be_the_target_of_an_import() {
        let store = Store::open_in_memory();
        assert!(!store.is_shared());
        assert!(store.session_delete(&digest(1)).is_err());
        assert!(store.sessions_count().is_err());
        let error = store.import_sqlite(Path::new("/nonexistent")).unwrap_err();
        assert!(error.contains("PostgreSQL"), "{error}");
    }

    #[test]
    fn a_shared_session_is_recorded_with_its_history_and_found_by_its_cookie_and_binding() {
        let Some(pg) = postgres() else { return };
        let store = pg.fresh();
        let session = shared_session(1, Some("alice"), NOW + 3600);
        assert!(store
            .session_insert(
                &digest(1),
                &HERE,
                &session,
                &started(1, "alice", NOW),
                limits(10),
                &GEN
            )
            .unwrap()
            .is_some());
        let snap = store.snapshot(&[("alice".into(), false)], NOW).unwrap();
        assert_eq!(
            snap.accounts[0].recent.len(),
            1,
            "the history row exists at once"
        );
        assert_eq!(snap.accounts[0].recent[0].ended_at, None);

        let found = store
            .session_touch(&digest(1), &HERE, NOW + 10, Idle::same(IDLE), &GEN)
            .unwrap();
        assert_eq!(found, Some(session));
        assert_eq!(
            store
                .session_touch(&digest(1), &ELSEWHERE, NOW + 11, Idle::same(IDLE), &GEN)
                .unwrap(),
            None,
            "another binding finds nothing"
        );
        assert_eq!(
            store
                .session_touch(&digest(2), &HERE, NOW + 11, Idle::same(IDLE), &GEN)
                .unwrap(),
            None
        );
        // The refused attempt did not renew the idle timer. The session is unused since NOW + 10.
        assert_eq!(
            store
                .session_touch(
                    &digest(1),
                    &HERE,
                    NOW + 10 + 100,
                    Idle::same(Duration::from_secs(60)),
                    &GEN
                )
                .unwrap(),
            None,
            "idle for 100 s against a limit of 60"
        );
        assert_eq!(
            store.sessions_count().unwrap(),
            1,
            "found idle, but only a sweep ends it"
        );
    }

    #[test]
    fn a_remembered_session_has_its_own_idle_time_and_keeps_its_flag() {
        let Some(pg) = postgres() else { return };
        let (first, second) = pg.fresh_pair();
        let idle = Idle::new(Duration::from_secs(60), Duration::from_secs(600));
        let mut remembered = shared_session(1, Some("alice"), NOW + 100_000);
        remembered.remembered = true;
        for (n, session) in [
            (1u8, remembered.clone()),
            (2, shared_session(2, Some("alice"), NOW + 100_000)),
        ] {
            first
                .session_insert(
                    &digest(n),
                    &HERE,
                    &session,
                    &started(u64::from(n), "alice", NOW),
                    limits(10),
                    &GEN,
                )
                .unwrap();
        }
        // The other instance reads the flag with the session.
        assert_eq!(
            second
                .session_peek(&digest(1), &HERE, NOW + 30, idle, &GEN)
                .unwrap(),
            Some(remembered.clone())
        );
        // After 300 s: past the idle time of a normal session, inside the idle time of a remembered one.
        assert_eq!(
            second
                .session_touch(&digest(2), &HERE, NOW + 300, idle, &GEN)
                .unwrap(),
            None
        );
        assert_eq!(
            second
                .session_touch(&digest(1), &HERE, NOW + 300, idle, &GEN)
                .unwrap(),
            Some(remembered.clone())
        );
        // The sweep ends the normal session as idle, and keeps the remembered one.
        assert_eq!(
            first.sessions_expire(NOW + 300, idle).unwrap(),
            [(2, false)]
        );
        assert_eq!(first.sessions_count().unwrap(), 1);
        // After the remembered idle time, the remembered session idles out too.
        assert_eq!(
            second
                .session_touch(&digest(1), &HERE, NOW + 300 + 601, idle, &GEN)
                .unwrap(),
            None
        );
        assert_eq!(
            first.sessions_expire(NOW + 300 + 601, idle).unwrap(),
            [(1, false)]
        );
        // The absolute end still applies to a remembered session that is in use.
        first
            .session_insert(
                &digest(3),
                &HERE,
                &SharedSession {
                    sid: 3,
                    expires_at: NOW + 1_000,
                    ..remembered
                },
                &started(3, "alice", NOW),
                limits(10),
                &GEN,
            )
            .unwrap();
        assert_eq!(
            second
                .session_touch(&digest(3), &HERE, NOW + 1_000, idle, &GEN)
                .unwrap(),
            None
        );
        assert_eq!(
            first.sessions_expire(NOW + 1_000, idle).unwrap(),
            [(3, true)]
        );
    }

    #[test]
    fn a_session_is_ended_once_whoever_asks() {
        let Some(pg) = postgres() else { return };
        let (first, second) = pair(&pg);
        let session = shared_session(1, Some("alice"), NOW + 3600);
        first
            .session_insert(
                &digest(1),
                &HERE,
                &session,
                &started(1, "alice", NOW),
                limits(10),
                &GEN,
            )
            .unwrap();
        assert!(second
            .session_touch(&digest(1), &HERE, NOW + 1, Idle::same(IDLE), &GEN)
            .unwrap()
            .is_some());
        assert!(first.session_delete(&digest(1)).unwrap().is_some());
        assert!(
            second.session_delete(&digest(1)).unwrap().is_none(),
            "already ended"
        );
        assert!(second
            .session_touch(&digest(1), &HERE, NOW + 2, Idle::same(IDLE), &GEN)
            .unwrap()
            .is_none());
    }

    #[test]
    fn expired_and_idle_sessions_are_handed_to_exactly_one_sweeper() {
        let Some(pg) = postgres() else { return };
        let (first, second) = pair(&pg);
        let insert = |n: u8, expires_at: i64| {
            first
                .session_insert(
                    &digest(n),
                    &HERE,
                    &shared_session(u64::from(n), Some("alice"), expires_at),
                    &started(u64::from(n), "alice", NOW),
                    limits(10),
                    &GEN,
                )
                .unwrap()
        };
        insert(1, NOW + 50); // runs out of lifetime
        insert(2, NOW + 3600); // goes idle
        insert(3, NOW + 3600);
        second
            .session_touch(&digest(3), &HERE, NOW + 100, Idle::same(IDLE), &GEN)
            .unwrap(); // kept alive
        let mut ended = first
            .sessions_expire(NOW + 60, Idle::same(Duration::from_secs(90)))
            .unwrap();
        ended.sort();
        assert_eq!(
            ended,
            [(1, true)],
            "only the first lived out its lifetime so far"
        );
        let mut ended = second
            .sessions_expire(NOW + 100, Idle::same(Duration::from_secs(90)))
            .unwrap();
        ended.sort();
        assert_eq!(
            ended,
            [(2, false)],
            "the second idled out; the third was used at NOW + 100"
        );
        assert!(first
            .sessions_expire(NOW + 100, Idle::same(Duration::from_secs(90)))
            .unwrap()
            .is_empty());
        assert_eq!(first.sessions_count().unwrap(), 1);
    }

    #[test]
    fn the_number_of_live_sessions_is_capped_and_expired_ones_make_room() {
        let Some(pg) = postgres() else { return };
        let store = pg.fresh();
        let insert = |n: u8, at: i64| {
            store
                .session_insert(
                    &digest(n),
                    &HERE,
                    &shared_session(u64::from(n), None, at + 100),
                    &started(u64::from(n), "root", at),
                    limits(2),
                    &GEN,
                )
                .unwrap()
                .is_some()
        };
        assert!(insert(1, NOW));
        assert!(insert(2, NOW));
        assert!(!insert(3, NOW), "two are live");
        assert!(insert(3, NOW + 200), "the first two have run out by then");
    }

    /// One principal keeps at most `max_per_account` live sessions: its new session ends its session that was used
    /// least recently. The other principals and the impersonated sessions keep theirs.
    #[test]
    fn a_principal_keeps_its_number_of_sessions_and_its_least_used_one_makes_room() {
        let Some(pg) = postgres() else { return };
        let store = pg.fresh();
        let per_account = SessionLimits {
            max_active: 10,
            max_per_account: 2,
        };
        let insert = |n: u8, login: Option<&str>, impersonated: bool| {
            let mut session = shared_session(u64::from(n), login, NOW + 3600);
            session.impersonated = impersonated;
            store
                .session_insert(
                    &digest(n),
                    &HERE,
                    &session,
                    &started(u64::from(n), login.unwrap_or("root"), NOW),
                    per_account,
                    &GEN,
                )
                .unwrap()
        };
        assert_eq!(insert(1, Some("mallory"), false), Some(vec![]));
        assert_eq!(insert(2, Some("mallory"), false), Some(vec![]));
        // The session 1 is in use: the session 2 is the one that was used least recently.
        assert!(store
            .session_touch(&digest(1), &HERE, NOW + 5, Idle::same(IDLE), &GEN)
            .unwrap()
            .is_some());
        assert_eq!(insert(3, Some("mallory"), false), Some(vec![2]));
        assert_eq!(insert(4, Some("bob"), false), Some(vec![]));
        assert_eq!(
            insert(5, Some("mallory"), true),
            Some(vec![]),
            "an impersonated session ends no session"
        );
        assert_eq!(insert(6, None, false), Some(vec![]));
        assert_eq!(insert(7, None, false), Some(vec![]));
        assert!(store
            .session_touch(&digest(7), &HERE, NOW + 5, Idle::same(IDLE), &GEN)
            .unwrap()
            .is_some());
        assert_eq!(insert(8, None, false), Some(vec![6]), "the superadmin too");
        assert_eq!(store.sessions_count().unwrap(), 6);
        assert!(store
            .session_touch(&digest(2), &HERE, NOW + 6, Idle::same(IDLE), &GEN)
            .unwrap()
            .is_none());
    }

    /// A superadmin or impersonated row is valid only with the credential generation that it
    /// carries. The rows of the database users do not depend on it.
    #[test]
    fn superadmin_rows_are_bound_to_the_credential_generation() {
        let Some(pg) = postgres() else { return };
        let store = pg.fresh();
        let other = [3; 32];
        let mut impersonated = shared_session(2, Some("alice"), NOW + 3600);
        impersonated.impersonated = true;
        for (n, session) in [
            (1u8, shared_session(1, None, NOW + 3600)),
            (2, impersonated),
            (3, shared_session(3, Some("bob"), NOW + 3600)),
        ] {
            assert!(store
                .session_insert(
                    &digest(n),
                    &HERE,
                    &session,
                    &started(u64::from(n), "x", NOW),
                    limits(10),
                    &GEN
                )
                .unwrap()
                .is_some());
        }
        for n in [1, 2] {
            assert!(store
                .session_touch(&digest(n), &HERE, NOW + 1, Idle::same(IDLE), &other)
                .unwrap()
                .is_none());
            assert!(store
                .session_touch(&digest(n), &HERE, NOW + 1, Idle::same(IDLE), &GEN)
                .unwrap()
                .is_some());
        }
        assert!(store
            .session_touch(&digest(3), &HERE, NOW + 1, Idle::same(IDLE), &other)
            .unwrap()
            .is_some());
        let mut ended = store.sessions_end_other_generations(&other).unwrap();
        ended.sort();
        assert_eq!(ended, [1, 2]);
        assert!(store
            .sessions_end_other_generations(&other)
            .unwrap()
            .is_empty());
        assert_eq!(store.sessions_count().unwrap(), 1);
    }

    #[test]
    fn deleting_the_sessions_of_a_user_leaves_the_others() {
        let Some(pg) = postgres() else { return };
        let store = pg.fresh();
        for (n, login) in [
            (1u8, Some("alice")),
            (2, Some("alice")),
            (3, Some("bob")),
            (4, None),
        ] {
            store
                .session_insert(
                    &digest(n),
                    &HERE,
                    &shared_session(u64::from(n), login, NOW + 100),
                    &started(u64::from(n), login.unwrap_or("root"), NOW),
                    limits(10),
                    &GEN,
                )
                .unwrap();
        }
        let mut ended = store.sessions_delete_user("alice").unwrap();
        ended.sort();
        assert_eq!(ended, [1, 2]);
        assert_eq!(store.sessions_count().unwrap(), 2);
    }

    #[test]
    fn history_rows_nobody_ended_are_closed_only_when_their_session_is_gone_and_silent() {
        let Some(pg) = postgres() else { return };
        let store = pg.fresh();
        store
            .apply(&Batch {
                events: vec![
                    start(1, "alice", false, false, 100), // gone and silent: lost
                    start(2, "alice", false, false, 100), // gone but seen recently
                    start(3, "alice", false, false, 100), // still a live session
                ],
                seen: HashMap::from([(2u64, 900i64)]),
                ..Batch::default()
            })
            .unwrap();
        store
            .session_insert(
                &digest(3),
                &HERE,
                &shared_session(3, Some("alice"), NOW + 100),
                &started(3, "alice", 100),
                limits(10),
                &GEN,
            )
            .unwrap();
        assert_eq!(store.close_lost_sessions(500, "restart").unwrap(), 1);
        let snap = store.snapshot(&[("alice".into(), false)], 1_000).unwrap();
        let open: Vec<_> = snap.accounts[0]
            .recent
            .iter()
            .filter(|s| s.ended_at.is_none())
            .collect();
        assert_eq!(open.len(), 2, "the recent one and the live one stay open");
        let closed = snap.accounts[0]
            .recent
            .iter()
            .find(|s| s.ended_at.is_some())
            .unwrap();
        assert_eq!(closed.end_reason.as_deref(), Some("restart"));
        assert_eq!(closed.ended_at, Some(100), "it ends when it was last seen");
    }

    #[test]
    fn an_account_made_on_one_instance_is_read_on_the_other() {
        let Some(pg) = postgres() else { return };
        let (first, second) = pair(&pg);
        first.insert_user(&user("alice", &["ops"])).unwrap();
        assert_eq!(second.load_users().unwrap()[0].grants, access(&["ops"]));
        assert_eq!(
            second.insert_user(&user("alice", &[])),
            Err(InsertError::Exists)
        );
        assert!(second
            .update_user("alice", Some("h2"), Some(&access(&["dev"])))
            .unwrap());
        assert_eq!(first.load_users().unwrap()[0].password_hash, "h2");
        assert!(first.remove_user("alice").unwrap());
        assert!(second.load_users().unwrap().is_empty());
    }

    #[test]
    fn concurrent_flushes_from_two_instances_neither_deadlock_nor_lose_hits() {
        let Some(pg) = postgres() else { return };
        let (first, second) = pair(&pg);
        let first = std::sync::Arc::new(first);
        let second = std::sync::Arc::new(second);
        first
            .apply(&Batch {
                events: (1..=20)
                    .map(|sid| start(sid, "alice", false, false, 100))
                    .collect(),
                ..Batch::default()
            })
            .unwrap();
        let flush = |store: std::sync::Arc<Store>, reversed: bool| {
            std::thread::spawn(move || {
                for round in 0..15i64 {
                    let mut sids: Vec<u64> = (1..=20).collect();
                    if reversed {
                        sids.reverse();
                    }
                    let hits = sids
                        .iter()
                        .map(|sid| {
                            (
                                (*sid, "grafana".to_string()),
                                Hit {
                                    first_at: 100,
                                    last_at: 200 + round,
                                    hits: 1,
                                    ip: None,
                                },
                            )
                        })
                        .collect();
                    let seen = sids.iter().map(|sid| (*sid, 300 + round)).collect();
                    store
                        .apply(&Batch {
                            hits,
                            seen,
                            ..Batch::default()
                        })
                        .unwrap();
                }
            })
        };
        let threads = [
            flush(first.clone(), false),
            flush(second.clone(), true),
            flush(first.clone(), true),
            flush(second.clone(), false),
        ];
        for thread in threads {
            thread.join().unwrap();
        }
        let snap = first.snapshot(&[("alice".into(), false)], 1_000).unwrap();
        assert_eq!(snap.accounts[0].services[0].hits, 4 * 15 * 20);
    }

    /// A SQLite file with sample data of every kind. The file is still open, as the running portal holds it.
    fn sqlite_file_with_history(name: &str) -> (std::path::PathBuf, Store) {
        let dir = std::env::temp_dir().join(format!("portal-import-{}-{name}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("portal.db");
        let _ = std::fs::remove_file(&path);
        let store = Store::open(&path).unwrap();
        store.insert_user(&user("alice", &["ops", "dev"])).unwrap();
        store.insert_user(&user("bob", &[])).unwrap();
        store.insert_user(&user("carol", &["viewer"])).unwrap();
        store.remove_user("bob").unwrap(); // a gap in the identifiers
                                           // The second factor of a database account and of an account of the file, and the check of the key.
        store.totp_bind_key(&[9; 32]).unwrap();
        store
            .totp_enable("alice", b"sealed-alice", &[[1; 32], [2; 32]], 5, NOW)
            .unwrap();
        store
            .totp_enable("a-file-account", b"sealed-file", &[[3; 32]], 6, NOW)
            .unwrap();
        // A key with a budget, a key of the superadmin and a revoked key (schema 6).
        let big = KeyCaps {
            max_per_user: 100,
            ..caps()
        };
        store
            .insert_key(&key("k1", Some("alice"), 1), &big)
            .unwrap();
        store.insert_key(&key("s1", None, 2), &big).unwrap();
        store
            .insert_key(&key("k2", Some("carol"), 3), &big)
            .unwrap();
        store.revoke_key("k2", None, NOW).unwrap();
        store
            .apply(&Batch {
                events: vec![
                    start(1, "alice", false, false, 100),
                    start(2, "root", true, false, 120),
                    Event::SessionEnded {
                        sid: 1,
                        reason: EndReason::Logout,
                        at: 400,
                    },
                    Event::Audit(AuditEvent {
                        kind: AuditKind::UserAdded,
                        login: Some("alice".into()),
                        actor: Some("root".into()),
                        ip: None,
                        user_agent: "x".into(),
                        at: 90,
                    }),
                    attempt("alice", true, "10.0.0.2", 100),
                    attempt("admin", false, "203.0.113.7", 110),
                ],
                hits: HashMap::from([(
                    (1u64, "grafana".to_string()),
                    Hit {
                        first_at: 150,
                        last_at: 300,
                        hits: 5,
                        ip: Some("10.0.0.2".into()),
                    },
                )]),
                seen: HashMap::from([(2u64, 500i64)]),
            })
            .unwrap();
        (path, store)
    }

    #[test]
    fn the_import_copies_accounts_and_history_into_an_empty_database() {
        let Some(pg) = postgres() else { return };
        let (path, source) = sqlite_file_with_history("copy");
        let target = pg.fresh();
        let report = target.import_sqlite(&path).unwrap();
        assert_eq!(
            report,
            ImportReport {
                users: 2,
                sessions: 2,
                accesses: 1,
                audit: 1,
                logins: 2,
                keys: 2,
                key_history: 1,
                externals: 0,
                totp: 2,
            }
        );
        // The second factor comes with its recovery codes and the check of its key.
        assert_eq!(
            target.totp_get("alice").unwrap(),
            source.totp_get("alice").unwrap()
        );
        assert_eq!(target.totp_get("alice").unwrap().unwrap().recovery_left, 2);
        assert_eq!(target.totp_logins().unwrap(), ["a-file-account", "alice"]);
        assert!(target.totp_bind_key(&[9; 32]).unwrap());
        assert!(!target.totp_bind_key(&[8; 32]).unwrap());
        assert_eq!(target.load_users().unwrap(), source.load_users().unwrap());
        // The keys keep their digests, their grants and their owners. The import adds 1 to the
        // version of the directory, so the instances read it again.
        let (copied_keys, original_keys) = (
            target.load_directory().unwrap(),
            source.load_directory().unwrap(),
        );
        assert_eq!(copied_keys.keys, original_keys.keys);
        assert!(copied_keys.version > 0);
        assert_eq!(target.key_history().unwrap(), source.key_history().unwrap());
        assert_eq!(
            target.login_attempts(0, false).unwrap(),
            source.login_attempts(0, false).unwrap()
        );
        let accounts = [("alice".to_string(), false), ("root".to_string(), true)];
        let (copied, original) = (
            target.snapshot(&accounts, 600).unwrap(),
            source.snapshot(&accounts, 600).unwrap(),
        );
        assert_eq!(copied.accounts[0], original.accounts[0]);
        assert_eq!(copied.recent_events, original.recent_events);
        // The superadmin's session was open. Sessions do not move. So, the session ends where it was last seen.
        let root = &copied.accounts[1].recent[0];
        assert_eq!(root.end_reason.as_deref(), Some("restart"));
        assert_eq!(root.ended_at, Some(500));
        assert_eq!(
            original.accounts[1].recent[0].ended_at, None,
            "the source is untouched"
        );
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn the_import_refuses_a_database_that_is_not_empty_and_changes_nothing() {
        let Some(pg) = postgres() else { return };
        let (path, _source) = sqlite_file_with_history("refuse");
        let target = pg.fresh();
        target.import_sqlite(&path).unwrap();
        let error = target.import_sqlite(&path).unwrap_err();
        assert!(error.contains("not empty"), "{error}");
        assert_eq!(target.count("users"), 2);
        assert_eq!(target.count("login_attempts"), 2);

        let busy = pg.fresh();
        busy.insert_user(&user("dave", &[])).unwrap();
        assert!(busy.import_sqlite(&path).is_err());
        assert_eq!(busy.count("users"), 1);
        assert_eq!(busy.count("sessions_log"), 0, "nothing was half copied");
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn what_is_written_after_an_import_continues_the_identifiers() {
        let Some(pg) = postgres() else { return };
        let (path, _source) = sqlite_file_with_history("sequences");
        let target = pg.fresh();
        target.import_sqlite(&path).unwrap();
        target.insert_user(&user("erin", &["ops"])).unwrap();
        target
            .apply(&Batch {
                events: vec![
                    attempt("alice", true, "10.0.0.2", 700),
                    Event::Audit(AuditEvent {
                        kind: AuditKind::UserAdded,
                        login: Some("erin".into()),
                        actor: Some("root".into()),
                        ip: None,
                        user_agent: "x".into(),
                        at: 700,
                    }),
                ],
                ..Batch::default()
            })
            .unwrap();
        assert_eq!(target.load_users().unwrap().len(), 3);
        assert_eq!(target.login_attempts(0, false).unwrap().matching, 3);
        assert_eq!(target.count("audit_events"), 2);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn a_file_that_is_not_a_portal_database_is_refused_by_the_import() {
        let Some(pg) = postgres() else { return };
        let target = pg.fresh();
        assert!(target
            .import_sqlite(Path::new("/nonexistent/portal.db"))
            .is_err());
        let dir =
            std::env::temp_dir().join(format!("portal-import-{}-foreign", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("other.db");
        let _ = std::fs::remove_file(&path);
        rusqlite::Connection::open(&path)
            .unwrap()
            .execute_batch("CREATE TABLE other (x INTEGER);")
            .unwrap();
        assert!(target.import_sqlite(&path).is_err());
        assert_eq!(target.count("users"), 0);
        let _ = std::fs::remove_dir_all(dir);
    }
}

#[cfg(test)]
mod pid_tests {
    use super::*;

    #[test]
    fn a_mark_of_a_live_process_other_than_this_one_tells_that_a_portal_runs() {
        let dir = std::env::temp_dir().join(format!("portal-pid-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let db = dir.join("portal.db");
        assert!(!sqlite_instance_running(&db), "no mark");
        mark_sqlite_instance(&db).unwrap();
        assert!(
            !sqlite_instance_running(&db),
            "the mark of this process is not another portal"
        );
        // The process 1 exists in a container and is not this one.
        std::fs::write(sqlite_pid_file(&db), "1\n").unwrap();
        assert!(sqlite_instance_running(&db));
        // A process that does not exist, and a bad text.
        std::fs::write(sqlite_pid_file(&db), "4194303\n").unwrap();
        assert!(!sqlite_instance_running(&db));
        std::fs::write(sqlite_pid_file(&db), "nonsense").unwrap();
        assert!(!sqlite_instance_running(&db));
        std::fs::remove_dir_all(dir).unwrap();
    }
}
