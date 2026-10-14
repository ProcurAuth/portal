//! Where the portal keeps its state: accounts, the history the settings pages show, and, with
//! PostgreSQL, the live sessions.
//!
//! Two backends behind one type. SQLite is one file owned by one process; PostgreSQL is shared by any
//! number of instances (see [`crate::sync`]). Callers run these methods on blocking threads (the
//! writer thread of [`crate::tracking`], or `spawn_blocking` from a handler); the request path reads
//! the in-memory directory instead and never waits on the database.

mod pg;
mod sqlite;

use std::path::Path;
use std::time::Duration;

use crate::config::PostgresConfig;
use crate::tracking::{Batch, SessionStart};

use pg::Pg;
use sqlite::Sqlite;

/// Days drawn by the connection graphs, today included.
pub const GRAPH_DAYS: usize = 30;
const RECENT_SESSIONS: i64 = 8;
const RECENT_EVENTS: i64 = 12;
const TOP_SERVICES: i64 = 12;
const DAY: i64 = 86_400;
/// Rows per page of the sign-in log.
pub const LOGIN_PAGE_SIZE: i64 = 50;
/// The sign-in log keeps at most this many rows, newest first: failures are bounded per minute by the
/// rate limiter, and this bounds what a long siege can leave in the database.
const MAX_LOGIN_ROWS: i64 = 20_000;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UserRow {
    pub login: String,
    pub password_hash: String,
    pub roles: Vec<String>,
    pub created_at: i64,
}

#[derive(Debug, PartialEq, Eq)]
pub enum InsertError {
    Exists,
    Db(String),
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DayStat {
    pub sessions: u32,
    /// Seconds spent connected, summed over the day's sessions.
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
    /// What was typed, already made safe when it was recorded.
    pub login: String,
    /// The typed login named an account.
    pub known: bool,
    pub success: bool,
    pub ip: Option<String>,
    pub user_agent: String,
}

/// One page of the sign-in log and the figures above it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LoginPage {
    pub rows: Vec<LoginRow>,
    /// Rows that match the filter, over every page.
    pub matching: u32,
    /// Zero-based, already brought back into range.
    pub page: u32,
    pub attempts_24h: u32,
    pub failed_24h: u32,
    /// Distinct addresses behind the failed attempts of the last 24 hours.
    pub addresses_24h: u32,
}

impl LoginPage {
    pub fn pages(&self) -> u32 {
        self.matching.div_ceil(LOGIN_PAGE_SIZE as u32).max(1)
    }
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
    /// Failed logins on a name that does not exist (the typed names are in the sign-in log).
    pub unknown_failures_30d: u32,
    /// Unix time of the first day drawn.
    pub first_day: i64,
}

impl Snapshot {
    /// What the Users page shows when the history cannot be read: the accounts, with nothing recorded.
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

/// A live session as the shared store keeps it (what is not secret and not derived).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SharedSession {
    pub sid: u64,
    /// `None`: the superadmin.
    pub login: Option<String>,
    pub impersonated: bool,
    /// Unix time.
    pub expires_at: i64,
}

/// A line of `sessions_log`, as the import reads and writes it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LoggedSession {
    pub sid: i64,
    pub login: String,
    pub superadmin: bool,
    pub impersonated: bool,
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

/// Everything a SQLite file holds that the import carries over. The accounts keep their identifier.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Dump {
    pub users: Vec<(i64, UserRow)>,
    pub sessions: Vec<LoggedSession>,
    pub accesses: Vec<LoggedAccess>,
    pub audit: Vec<LoggedAudit>,
    pub logins: Vec<LoggedLogin>,
}

/// What an import copied.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImportReport {
    pub users: usize,
    pub sessions: usize,
    pub accesses: usize,
    pub audit: usize,
    pub logins: usize,
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

    /// Connects to PostgreSQL, waiting for it for up to a minute, and brings the schema up to date.
    pub fn open_postgres(config: &PostgresConfig, password: &str) -> Result<Self, String> {
        Pg::connect(config, password, pg::STARTUP_RETRY).map(|store| Store::Pg(Box::new(store)))
    }

    #[cfg(test)]
    pub fn open_in_memory() -> Self {
        Store::Sqlite(Sqlite::open_in_memory())
    }

    /// Several portal instances use this store at once: sessions live in it and the accounts have to
    /// be read again from time to time.
    pub fn is_shared(&self) -> bool {
        matches!(self, Store::Pg(_))
    }

    /// Rows of a table, for the tests that look at what a method left behind.
    #[cfg(test)]
    pub fn count(&self, table: &str) -> i64 {
        backend!(self.count(table))
    }

    /// What a lost connection looks like to the next calls (PostgreSQL only).
    #[cfg(test)]
    pub fn go_down(&self) {
        if let Store::Pg(store) = self {
            store.go_down();
        }
    }

    // Accounts --------------------------------------------------------------------------------

    pub fn load_users(&self) -> Result<Vec<UserRow>, String> {
        backend!(self.load_users())
    }

    pub fn insert_user(&self, user: &UserRow) -> Result<(), InsertError> {
        backend!(self.insert_user(user))
    }

    /// Replaces the roles of a user and, when given, its password hash, in one transaction.
    /// `false` if no such user.
    pub fn update_user(
        &self,
        login: &str,
        password_hash: Option<&str>,
        roles: &[String],
    ) -> Result<bool, String> {
        backend!(self.update_user(login, password_hash, roles))
    }

    pub fn remove_user(&self, login: &str) -> Result<bool, String> {
        backend!(self.remove_user(login))
    }

    // History ---------------------------------------------------------------------------------

    /// One transaction for everything the tracker gathered since the last flush.
    pub fn apply(&self, batch: &Batch) -> Result<(), String> {
        backend!(self.apply(batch))
    }

    /// Sessions a previous run left open end at the last time they were seen. Only for a store one
    /// process owns: in a shared one the sessions outlive a restart (see [`Store::close_lost_sessions`]).
    pub fn close_open_sessions(&self, now: i64, reason: &str) -> Result<(), String> {
        match self {
            Store::Sqlite(store) => store.close_open_sessions(now, reason),
            Store::Pg(_) => Ok(()),
        }
    }

    /// Deletes what started before `cutoff`.
    pub fn prune(&self, cutoff: i64) -> Result<(), String> {
        backend!(self.prune(cutoff))
    }

    /// One page of the sign-in log, newest first; `only_failed` keeps the refused attempts. A page
    /// past the end is brought back to the last one.
    pub fn login_attempts(
        &self,
        now: i64,
        page: u32,
        only_failed: bool,
    ) -> Result<LoginPage, String> {
        backend!(self.login_attempts(now, page, only_failed))
    }

    /// Everything the Users page shows. `accounts` is `(login, is_superadmin)`.
    pub fn snapshot(&self, accounts: &[(String, bool)], now: i64) -> Result<Snapshot, String> {
        backend!(self.snapshot(accounts, now))
    }

    // Shared sessions (PostgreSQL only) -------------------------------------------------------

    fn shared(&self) -> Result<&Pg, String> {
        match self {
            Store::Pg(store) => Ok(store),
            Store::Sqlite(_) => Err("this database does not hold shared sessions".into()),
        }
    }

    /// Records a session and its line of the history. `false`: the limit of live sessions is reached.
    pub fn session_insert(
        &self,
        digest: &[u8; 32],
        binding: &[u8; 32],
        session: &SharedSession,
        start: &SessionStart,
        max_active: usize,
    ) -> Result<bool, String> {
        self.shared()?
            .session_insert(digest, binding, session, start, max_active)
    }

    /// The live session of `digest` for `binding`, its idle timer renewed to `now`.
    pub fn session_touch(
        &self,
        digest: &[u8; 32],
        binding: &[u8; 32],
        now: i64,
        idle: Duration,
    ) -> Result<Option<SharedSession>, String> {
        self.shared()?
            .session_touch(digest, binding, now, idle.as_secs() as i64)
    }

    /// Ends one session; it is returned when this call is the one that ended it.
    pub fn session_delete(&self, digest: &[u8; 32]) -> Result<Option<SharedSession>, String> {
        self.shared()?.session_delete(digest)
    }

    /// Ends every session of a user; the identifiers this call ended.
    pub fn sessions_delete_user(&self, login: &str) -> Result<Vec<u64>, String> {
        self.shared()?.sessions_delete_user(login)
    }

    /// Ends the sessions past their lifetime or idle; each with whether it expired rather than idled.
    pub fn sessions_expire(&self, now: i64, idle: Duration) -> Result<Vec<(u64, bool)>, String> {
        self.shared()?.sessions_expire(now, idle.as_secs() as i64)
    }

    pub fn sessions_count(&self) -> Result<usize, String> {
        self.shared()?.sessions_count()
    }

    /// Closes the history rows of sessions that are gone and were last seen before `before`.
    pub fn close_lost_sessions(&self, before: i64, reason: &str) -> Result<usize, String> {
        self.shared()?.close_lost_sessions(before, reason)
    }

    // Import ----------------------------------------------------------------------------------

    /// Copies a SQLite file into this (empty) PostgreSQL database.
    pub fn import_sqlite(&self, path: &Path) -> Result<ImportReport, String> {
        let target = self
            .shared()
            .map_err(|_| "the target of an import must be a PostgreSQL database".to_string())?;
        target.import(&Sqlite::open_read_only(path)?.dump()?)
    }
}

#[cfg(test)]
pub mod testing {
    //! Every backend a test can run on: SQLite always, PostgreSQL when `PORTAL_TEST_POSTGRES` holds the
    //! URL of a database to use (`postgres://user:password@host:port/database`). Each PostgreSQL
    //! store lives in a schema of its own, so tests run side by side and leave the rest alone.

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

        /// Two instances on one empty database (PostgreSQL only).
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
            _ => eprintln!("{POSTGRES_ENV} is not set: the PostgreSQL side of this test is skipped"),
        }
        all
    }

    pub fn postgres() -> Option<Backend> {
        backends().into_iter().find(|b| matches!(b, Backend::Postgres(_)))
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

    fn user(login: &str, roles: &[&str]) -> UserRow {
        UserRow {
            login: login.into(),
            password_hash: "$argon2id$stub".into(),
            roles: roles.iter().map(|r| r.to_string()).collect(),
            created_at: 1_000,
        }
    }

    fn start(sid: u64, login: &str, superadmin: bool, impersonated: bool, at: i64) -> Event {
        Event::SessionStarted(SessionStart {
            sid,
            login: login.into(),
            superadmin,
            impersonated,
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

    #[test]
    fn users_round_trip_with_their_roles_and_reject_a_duplicate() {
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
            assert_eq!(users[0].roles, ["dev", "ops"]);
            assert!(users[1].roles.is_empty());
            assert!(store.remove_user("alice").unwrap());
            assert!(!store.remove_user("alice").unwrap());
            assert_eq!(store.load_users().unwrap().len(), 1);

        });
    }

    #[test]
    fn updating_a_user_replaces_its_roles_and_optionally_its_password() {
        each_backend(|backend| {

            let store = backend.fresh();
            store.insert_user(&user("alice", &["ops", "dev"])).unwrap();
            store.insert_user(&user("bob", &["ops"])).unwrap();
            let roles = |login: &str| {
                store
                    .load_users()
                    .unwrap()
                    .into_iter()
                    .find(|u| u.login == login)
                    .unwrap()
            };
            assert!(store
                .update_user("alice", None, &["viewer".to_string()])
                .unwrap());
            assert_eq!(roles("alice").roles, ["viewer"]);
            assert_eq!(
                roles("alice").password_hash,
                "$argon2id$stub",
                "kept without a new hash"
            );
            assert!(store.update_user("alice", Some("new-hash"), &[]).unwrap());
            assert_eq!(roles("alice").password_hash, "new-hash");
            assert!(roles("alice").roles.is_empty());
            assert_eq!(roles("bob").roles, ["ops"], "other users are untouched");
            assert!(!store.update_user("nobody", None, &[]).unwrap());

        });
    }

    #[test]
    fn deleting_a_user_deletes_its_roles_but_keeps_its_history() {
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
            assert_eq!(store.count("user_roles"), 0);
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
            assert_eq!(snap.unknown_failures_30d, 1);
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
                // Sessions outlive a restart there: the ones nobody ended are found by their silence.
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
    fn sign_in_attempts_are_listed_newest_first_with_the_figures_of_the_last_day() {
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
            let all = store.login_attempts(NOW, 0, false).unwrap();
            assert_eq!(all.matching, 5);
            assert_eq!(all.rows[0].ts, NOW - 300, "newest first");
            assert!(all.rows[0].success && all.rows[0].known);
            assert_eq!(all.rows[2].login, "admin");
            assert!(!all.rows[2].known);
            assert_eq!(all.rows[2].ip.as_deref(), Some("203.0.113.7"));
            assert_eq!(
                (all.attempts_24h, all.failed_24h, all.addresses_24h),
                (4, 3, 2),
                "the attempt of three days ago is out of the figures, two addresses failed"
            );
            let failed = store.login_attempts(NOW, 0, true).unwrap();
            assert_eq!(failed.matching, 3);
            assert!(failed.rows.iter().all(|row| !row.success));
            assert_eq!(failed.attempts_24h, 4, "the figures do not depend on the filter");

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
            let page = store.login_attempts(NOW, 0, false).unwrap();
            assert_eq!(page.rows[0].login, nasty);
            assert_eq!(store.load_users().unwrap().len(), 1, "the tables are intact");

        });
    }

    #[test]
    fn the_sign_in_log_pages_and_a_page_past_the_end_comes_back_to_the_last_one() {
        each_backend(|backend| {

            let store = backend.fresh();
            let events = (0..(LOGIN_PAGE_SIZE + 5))
                .map(|i| attempt("alice", false, "10.0.0.2", NOW - 1_000 + i))
                .collect();
            store
                .apply(&Batch {
                    events,
                    ..Batch::default()
                })
                .unwrap();
            let first = store.login_attempts(NOW, 0, false).unwrap();
            assert_eq!((first.rows.len() as i64, first.pages()), (LOGIN_PAGE_SIZE, 2));
            let second = store.login_attempts(NOW, 1, false).unwrap();
            assert_eq!(second.rows.len(), 5);
            assert_eq!(second.rows[0].ts, NOW - 1_000 + 4, "continues where the first page stopped");
            let beyond = store.login_attempts(NOW, 99, false).unwrap();
            assert_eq!(beyond.page, 1);
            assert_eq!(beyond.rows.len(), 5);
            let empty = backend.fresh().login_attempts(NOW, 7, false).unwrap();
            assert_eq!((empty.page, empty.pages(), empty.rows.len()), (0, 1, 0));

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
            let page = store.login_attempts(1_000, 0, false).unwrap();
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
            let page = store.login_attempts(NOW, 0, false).unwrap();
            assert_eq!(i64::from(page.matching), MAX_LOGIN_ROWS, "the oldest rows made room");
            assert_eq!(page.rows[0].ts, MAX_LOGIN_ROWS + 29, "the newest are the ones kept");

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

    // PostgreSQL only: shared sessions, several instances, the import --------------------------

    const HERE: [u8; 32] = [7; 32];
    const ELSEWHERE: [u8; 32] = [8; 32];
    const IDLE: Duration = Duration::from_secs(1800);

    fn digest(n: u8) -> [u8; 32] {
        [n; 32]
    }

    fn shared_session(sid: u64, login: Option<&str>, expires_at: i64) -> SharedSession {
        SharedSession {
            sid,
            login: login.map(str::to_string),
            impersonated: false,
            expires_at,
        }
    }

    fn started(sid: u64, login: &str, at: i64) -> SessionStart {
        SessionStart {
            sid,
            login: login.into(),
            superadmin: false,
            impersonated: false,
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
            .session_insert(&digest(1), &HERE, &session, &started(1, "alice", NOW), 10)
            .unwrap());
        let snap = store.snapshot(&[("alice".into(), false)], NOW).unwrap();
        assert_eq!(snap.accounts[0].recent.len(), 1, "the history row exists at once");
        assert_eq!(snap.accounts[0].recent[0].ended_at, None);

        let found = store.session_touch(&digest(1), &HERE, NOW + 10, IDLE).unwrap();
        assert_eq!(found, Some(session));
        assert_eq!(
            store.session_touch(&digest(1), &ELSEWHERE, NOW + 11, IDLE).unwrap(),
            None,
            "another binding finds nothing"
        );
        assert_eq!(store.session_touch(&digest(2), &HERE, NOW + 11, IDLE).unwrap(), None);
        // The refused attempt did not renew the idle timer: unused since NOW + 10.
        assert_eq!(
            store
                .session_touch(&digest(1), &HERE, NOW + 10 + 100, Duration::from_secs(60))
                .unwrap(),
            None,
            "idle for 100 s against a limit of 60"
        );
        assert_eq!(store.sessions_count().unwrap(), 1, "found idle, but only a sweep ends it");
    }

    #[test]
    fn a_session_is_ended_once_whoever_asks() {
        let Some(pg) = postgres() else { return };
        let (first, second) = pair(&pg);
        let session = shared_session(1, Some("alice"), NOW + 3600);
        first
            .session_insert(&digest(1), &HERE, &session, &started(1, "alice", NOW), 10)
            .unwrap();
        assert!(second.session_touch(&digest(1), &HERE, NOW + 1, IDLE).unwrap().is_some());
        assert!(first.session_delete(&digest(1)).unwrap().is_some());
        assert!(second.session_delete(&digest(1)).unwrap().is_none(), "already ended");
        assert!(second.session_touch(&digest(1), &HERE, NOW + 2, IDLE).unwrap().is_none());
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
                    10,
                )
                .unwrap()
        };
        insert(1, NOW + 50); // runs out of lifetime
        insert(2, NOW + 3600); // goes idle
        insert(3, NOW + 3600);
        second.session_touch(&digest(3), &HERE, NOW + 100, IDLE).unwrap(); // kept alive
        let mut ended = first.sessions_expire(NOW + 60, Duration::from_secs(90)).unwrap();
        ended.sort();
        assert_eq!(ended, [(1, true)], "only the first lived out its lifetime so far");
        let mut ended = second.sessions_expire(NOW + 100, Duration::from_secs(90)).unwrap();
        ended.sort();
        assert_eq!(ended, [(2, false)], "the second idled out; the third was used at NOW + 100");
        assert!(first.sessions_expire(NOW + 100, Duration::from_secs(90)).unwrap().is_empty());
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
                    2,
                )
                .unwrap()
        };
        assert!(insert(1, NOW));
        assert!(insert(2, NOW));
        assert!(!insert(3, NOW), "two are live");
        assert!(insert(3, NOW + 200), "the first two have run out by then");
    }

    #[test]
    fn deleting_the_sessions_of_a_user_leaves_the_others() {
        let Some(pg) = postgres() else { return };
        let store = pg.fresh();
        for (n, login) in [(1u8, Some("alice")), (2, Some("alice")), (3, Some("bob")), (4, None)] {
            store
                .session_insert(
                    &digest(n),
                    &HERE,
                    &shared_session(u64::from(n), login, NOW + 100),
                    &started(u64::from(n), login.unwrap_or("root"), NOW),
                    10,
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
                10,
            )
            .unwrap();
        assert_eq!(store.close_lost_sessions(500, "restart").unwrap(), 1);
        let snap = store.snapshot(&[("alice".into(), false)], 1_000).unwrap();
        let open: Vec<_> = snap.accounts[0].recent.iter().filter(|s| s.ended_at.is_none()).collect();
        assert_eq!(open.len(), 2, "the recent one and the live one stay open");
        let closed = snap.accounts[0].recent.iter().find(|s| s.ended_at.is_some()).unwrap();
        assert_eq!(closed.end_reason.as_deref(), Some("restart"));
        assert_eq!(closed.ended_at, Some(100), "it ends when it was last seen");
    }

    #[test]
    fn an_account_made_on_one_instance_is_read_on_the_other() {
        let Some(pg) = postgres() else { return };
        let (first, second) = pair(&pg);
        first.insert_user(&user("alice", &["ops"])).unwrap();
        assert_eq!(second.load_users().unwrap()[0].roles, ["ops"]);
        assert_eq!(second.insert_user(&user("alice", &[])), Err(InsertError::Exists));
        assert!(second.update_user("alice", Some("h2"), &["dev".to_string()]).unwrap());
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
                events: (1..=20).map(|sid| start(sid, "alice", false, false, 100)).collect(),
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
                                Hit { first_at: 100, last_at: 200 + round, hits: 1, ip: None },
                            )
                        })
                        .collect();
                    let seen = sids.iter().map(|sid| (*sid, 300 + round)).collect();
                    store.apply(&Batch { hits, seen, ..Batch::default() }).unwrap();
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

    /// A SQLite file with a bit of everything in it, still open (as the running portal has it).
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
        store
            .apply(&Batch {
                events: vec![
                    start(1, "alice", false, false, 100),
                    start(2, "root", true, false, 120),
                    Event::SessionEnded { sid: 1, reason: EndReason::Logout, at: 400 },
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
                    Hit { first_at: 150, last_at: 300, hits: 5, ip: Some("10.0.0.2".into()) },
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
            ImportReport { users: 2, sessions: 2, accesses: 1, audit: 1, logins: 2 }
        );
        assert_eq!(target.load_users().unwrap(), source.load_users().unwrap());
        assert_eq!(
            target.login_attempts(NOW, 0, false).unwrap(),
            source.login_attempts(NOW, 0, false).unwrap()
        );
        let accounts = [("alice".to_string(), false), ("root".to_string(), true)];
        let (copied, original) = (
            target.snapshot(&accounts, 600).unwrap(),
            source.snapshot(&accounts, 600).unwrap(),
        );
        assert_eq!(copied.accounts[0], original.accounts[0]);
        assert_eq!(copied.recent_events, original.recent_events);
        assert_eq!(copied.unknown_failures_30d, original.unknown_failures_30d);
        // The superadmin's session was open: sessions do not move, so it ends where it was last seen.
        let root = &copied.accounts[1].recent[0];
        assert_eq!(root.end_reason.as_deref(), Some("restart"));
        assert_eq!(root.ended_at, Some(500));
        assert_eq!(original.accounts[1].recent[0].ended_at, None, "the source is untouched");
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
        assert_eq!(target.login_attempts(800, 0, false).unwrap().matching, 3);
        assert_eq!(target.count("audit_events"), 2);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn a_file_that_is_not_a_portal_database_is_refused_by_the_import() {
        let Some(pg) = postgres() else { return };
        let target = pg.fresh();
        assert!(target.import_sqlite(Path::new("/nonexistent/portal.db")).is_err());
        let dir = std::env::temp_dir().join(format!("portal-import-{}-foreign", std::process::id()));
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
