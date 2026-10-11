//! The SQLite file: accounts, and the history the settings pages show.
//!
//! One connection behind a mutex. Callers run these methods on blocking threads (the writer thread
//! of [`crate::tracking`], or `spawn_blocking` from a handler); the request path reads the in-memory
//! directory instead and never waits on the disk.

use std::path::Path;
use std::sync::Mutex;

use rusqlite::{params, Connection, ErrorCode, OptionalExtension};

use crate::tracking::{AuditEvent, Batch, Event, LoginAttempt};

/// Days drawn by the connection graphs, today included.
pub const GRAPH_DAYS: usize = 30;
const RECENT_SESSIONS: i64 = 8;
const RECENT_EVENTS: i64 = 12;
const TOP_SERVICES: i64 = 12;
const DAY: i64 = 86_400;
const SCHEMA_VERSION: i64 = 2;
/// Rows per page of the sign-in log.
pub const LOGIN_PAGE_SIZE: i64 = 50;
/// The sign-in log keeps at most this many rows, newest first: failures are bounded per minute by the
/// rate limiter, and this bounds what a long siege can leave on the volume.
const MAX_LOGIN_ROWS: i64 = 20_000;

const SCHEMA: &str = "
CREATE TABLE users (
  id INTEGER PRIMARY KEY,
  login TEXT NOT NULL UNIQUE,
  password_hash TEXT NOT NULL,
  created_at INTEGER NOT NULL
);
CREATE TABLE user_roles (
  user_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  role TEXT NOT NULL,
  PRIMARY KEY (user_id, role)
);
CREATE TABLE sessions_log (
  sid INTEGER PRIMARY KEY,
  login TEXT NOT NULL,
  superadmin INTEGER NOT NULL,
  impersonated INTEGER NOT NULL,
  ip TEXT,
  user_agent TEXT NOT NULL,
  started_at INTEGER NOT NULL,
  last_seen_at INTEGER NOT NULL,
  ended_at INTEGER,
  end_reason TEXT
);
CREATE INDEX sessions_log_login ON sessions_log (login, started_at);
CREATE INDEX sessions_log_started ON sessions_log (started_at);
CREATE TABLE service_access (
  sid INTEGER NOT NULL,
  service_id TEXT NOT NULL,
  first_at INTEGER NOT NULL,
  last_at INTEGER NOT NULL,
  hits INTEGER NOT NULL,
  ip TEXT,
  PRIMARY KEY (sid, service_id)
);
CREATE TABLE audit_events (
  id INTEGER PRIMARY KEY,
  ts INTEGER NOT NULL,
  kind TEXT NOT NULL,
  login TEXT,
  actor TEXT,
  ip TEXT,
  user_agent TEXT NOT NULL
);
CREATE INDEX audit_events_ts ON audit_events (ts);
";

/// Added by schema version 2 (a database at version 1 gets it when opened).
const LOGIN_SCHEMA: &str = "
CREATE TABLE login_attempts (
  id INTEGER PRIMARY KEY,
  ts INTEGER NOT NULL,
  ip TEXT,
  login TEXT NOT NULL,
  known INTEGER NOT NULL,
  success INTEGER NOT NULL,
  user_agent TEXT NOT NULL
);
CREATE INDEX login_attempts_ts ON login_attempts (ts);
";

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

pub struct Store {
    conn: Mutex<Connection>,
}

fn db(error: rusqlite::Error) -> String {
    error.to_string()
}

impl Store {
    pub fn open(path: &Path) -> Result<Self, String> {
        let conn = Connection::open(path)
            .map_err(|e| format!("cannot open the database {}: {e}", path.display()))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
        }
        Self::init(conn)
    }

    #[cfg(test)]
    pub fn open_in_memory() -> Self {
        Self::init(Connection::open_in_memory().unwrap()).unwrap()
    }

    fn init(conn: Connection) -> Result<Self, String> {
        conn.busy_timeout(std::time::Duration::from_secs(5))
            .map_err(db)?;
        // `journal_mode` answers with a row, so it is read rather than executed.
        let _: String = conn
            .query_row("PRAGMA journal_mode = WAL", [], |row| row.get(0))
            .map_err(db)?;
        conn.execute_batch("PRAGMA foreign_keys = ON; PRAGMA synchronous = NORMAL;")
            .map_err(db)?;
        let version: i64 = conn
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .map_err(db)?;
        match version {
            0 => conn
                .execute_batch(&format!(
                    "BEGIN; {SCHEMA} {LOGIN_SCHEMA} PRAGMA user_version = {SCHEMA_VERSION}; COMMIT;"
                ))
                .map_err(db)?,
            1 => conn
                .execute_batch(&format!(
                    "BEGIN; {LOGIN_SCHEMA} PRAGMA user_version = {SCHEMA_VERSION}; COMMIT;"
                ))
                .map_err(db)?,
            SCHEMA_VERSION => {}
            newer => {
                return Err(format!(
                "the database schema is version {newer}, this portal understands {SCHEMA_VERSION}"
            ))
            }
        }
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    fn conn(&self) -> std::sync::MutexGuard<'_, Connection> {
        self.conn.lock().unwrap_or_else(|p| p.into_inner())
    }

    // Accounts --------------------------------------------------------------------------------

    pub fn load_users(&self) -> Result<Vec<UserRow>, String> {
        let conn = self.conn();
        let mut users = conn
            .prepare("SELECT id, login, password_hash, created_at FROM users ORDER BY login")
            .map_err(db)?
            .query_map([], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    UserRow {
                        login: row.get(1)?,
                        password_hash: row.get(2)?,
                        roles: Vec::new(),
                        created_at: row.get(3)?,
                    },
                ))
            })
            .map_err(db)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(db)?;
        let mut roles = conn
            .prepare("SELECT role FROM user_roles WHERE user_id = ?1 ORDER BY role")
            .map_err(db)?;
        for (id, user) in &mut users {
            user.roles = roles
                .query_map([*id], |row| row.get::<_, String>(0))
                .map_err(db)?
                .collect::<Result<Vec<_>, _>>()
                .map_err(db)?;
        }
        Ok(users.into_iter().map(|(_, user)| user).collect())
    }

    pub fn insert_user(&self, user: &UserRow) -> Result<(), InsertError> {
        let mut conn = self.conn();
        let tx = conn.transaction().map_err(|e| InsertError::Db(db(e)))?;
        match tx.execute(
            "INSERT INTO users (login, password_hash, created_at) VALUES (?1, ?2, ?3)",
            params![user.login, user.password_hash, user.created_at],
        ) {
            Ok(_) => {}
            Err(rusqlite::Error::SqliteFailure(e, _))
                if e.code == ErrorCode::ConstraintViolation =>
            {
                return Err(InsertError::Exists)
            }
            Err(e) => return Err(InsertError::Db(db(e))),
        }
        let id = tx.last_insert_rowid();
        for role in &user.roles {
            tx.execute(
                "INSERT OR IGNORE INTO user_roles (user_id, role) VALUES (?1, ?2)",
                params![id, role],
            )
            .map_err(|e| InsertError::Db(db(e)))?;
        }
        tx.commit().map_err(|e| InsertError::Db(db(e)))
    }

    /// Replaces the roles of a user and, when given, its password hash, in one transaction.
    /// `false` if no such user.
    pub fn update_user(
        &self,
        login: &str,
        password_hash: Option<&str>,
        roles: &[String],
    ) -> Result<bool, String> {
        let mut conn = self.conn();
        let tx = conn.transaction().map_err(db)?;
        let id: Option<i64> = tx
            .query_row("SELECT id FROM users WHERE login = ?1", [login], |row| {
                row.get(0)
            })
            .optional()
            .map_err(db)?;
        let Some(id) = id else {
            return Ok(false);
        };
        if let Some(hash) = password_hash {
            tx.execute(
                "UPDATE users SET password_hash = ?2 WHERE id = ?1",
                params![id, hash],
            )
            .map_err(db)?;
        }
        tx.execute("DELETE FROM user_roles WHERE user_id = ?1", [id])
            .map_err(db)?;
        for role in roles {
            tx.execute(
                "INSERT OR IGNORE INTO user_roles (user_id, role) VALUES (?1, ?2)",
                params![id, role],
            )
            .map_err(db)?;
        }
        tx.commit().map_err(db)?;
        Ok(true)
    }

    pub fn remove_user(&self, login: &str) -> Result<bool, String> {
        let changed = self
            .conn()
            .execute("DELETE FROM users WHERE login = ?1", [login])
            .map_err(db)?;
        Ok(changed > 0)
    }

    // History: writes -------------------------------------------------------------------------

    /// One transaction for everything the tracker gathered since the last flush.
    pub fn apply(&self, batch: &Batch) -> Result<(), String> {
        if batch.events.is_empty() && batch.hits.is_empty() && batch.seen.is_empty() {
            return Ok(());
        }
        let mut conn = self.conn();
        let tx = conn.transaction().map_err(db)?;
        let mut logged_attempts = false;
        for event in &batch.events {
            match event {
                Event::SessionStarted(start) => {
                    tx.execute(
                        "INSERT OR IGNORE INTO sessions_log
                           (sid, login, superadmin, impersonated, ip, user_agent, started_at, last_seen_at)
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7)",
                        params![
                            start.sid as i64,
                            start.login,
                            start.superadmin,
                            start.impersonated,
                            start.ip,
                            start.user_agent,
                            start.at
                        ],
                    )
                    .map_err(db)?;
                }
                Event::SessionEnded { sid, reason, at } => {
                    tx.execute(
                        "UPDATE sessions_log
                            SET ended_at = ?2, end_reason = ?3, last_seen_at = MAX(last_seen_at, ?2)
                          WHERE sid = ?1 AND ended_at IS NULL",
                        params![*sid as i64, at, reason.as_str()],
                    )
                    .map_err(db)?;
                }
                Event::Audit(audit) => insert_audit(&tx, audit)?,
                Event::Login(attempt) => {
                    insert_login(&tx, attempt)?;
                    logged_attempts = true;
                }
            }
        }
        if logged_attempts {
            tx.execute(
                "DELETE FROM login_attempts WHERE id <= (SELECT MAX(id) FROM login_attempts) - ?1",
                [MAX_LOGIN_ROWS],
            )
            .map_err(db)?;
        }
        for ((sid, service), hit) in &batch.hits {
            tx.execute(
                "INSERT INTO service_access (sid, service_id, first_at, last_at, hits, ip)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                 ON CONFLICT (sid, service_id) DO UPDATE SET
                   last_at = MAX(last_at, excluded.last_at),
                   hits = hits + excluded.hits,
                   ip = COALESCE(excluded.ip, ip)",
                params![
                    *sid as i64,
                    service,
                    hit.first_at,
                    hit.last_at,
                    hit.hits as i64,
                    hit.ip
                ],
            )
            .map_err(db)?;
        }
        for (sid, at) in &batch.seen {
            tx.execute(
                "UPDATE sessions_log SET last_seen_at = MAX(last_seen_at, ?2)
                  WHERE sid = ?1 AND ended_at IS NULL",
                params![*sid as i64, at],
            )
            .map_err(db)?;
        }
        tx.commit().map_err(db)
    }

    /// Sessions a previous run left open end at the last time they were seen.
    pub fn close_open_sessions(&self, _now: i64, reason: &str) -> Result<(), String> {
        self.conn()
            .execute(
                "UPDATE sessions_log SET ended_at = last_seen_at, end_reason = ?1 WHERE ended_at IS NULL",
                [reason],
            )
            .map_err(db)?;
        Ok(())
    }

    /// Deletes what started before `cutoff`.
    pub fn prune(&self, cutoff: i64) -> Result<(), String> {
        let mut conn = self.conn();
        let tx = conn.transaction().map_err(db)?;
        tx.execute("DELETE FROM sessions_log WHERE started_at < ?1", [cutoff])
            .map_err(db)?;
        tx.execute(
            "DELETE FROM service_access WHERE sid NOT IN (SELECT sid FROM sessions_log)",
            [],
        )
        .map_err(db)?;
        tx.execute("DELETE FROM audit_events WHERE ts < ?1", [cutoff])
            .map_err(db)?;
        tx.execute("DELETE FROM login_attempts WHERE ts < ?1", [cutoff])
            .map_err(db)?;
        tx.commit().map_err(db)
    }

    // History: reads --------------------------------------------------------------------------

    /// One page of the sign-in log, newest first; `only_failed` keeps the refused attempts. A page
    /// past the end is brought back to the last one.
    pub fn login_attempts(
        &self,
        now: i64,
        page: u32,
        only_failed: bool,
    ) -> Result<LoginPage, String> {
        let conn = self.conn();
        let matching: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM login_attempts WHERE (?1 = 0 OR success = 0)",
                [only_failed],
                |row| row.get(0),
            )
            .map_err(db)?;
        let mut result = LoginPage {
            matching: matching as u32,
            ..LoginPage::default()
        };
        result.page = page.min(result.pages() - 1);
        result.rows = conn
            .prepare(
                "SELECT ts, login, known, success, ip, user_agent FROM login_attempts
                  WHERE (?1 = 0 OR success = 0)
                  ORDER BY id DESC LIMIT ?2 OFFSET ?3",
            )
            .map_err(db)?
            .query_map(
                params![only_failed, LOGIN_PAGE_SIZE, i64::from(result.page) * LOGIN_PAGE_SIZE],
                |row| {
                    Ok(LoginRow {
                        ts: row.get(0)?,
                        login: row.get(1)?,
                        known: row.get(2)?,
                        success: row.get(3)?,
                        ip: row.get(4)?,
                        user_agent: row.get(5)?,
                    })
                },
            )
            .map_err(db)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(db)?;
        let (attempts, failed, addresses): (i64, i64, i64) = conn
            .query_row(
                "SELECT COUNT(*), COALESCE(SUM(success = 0), 0),
                        COUNT(DISTINCT CASE WHEN success = 0 THEN ip END)
                   FROM login_attempts WHERE ts >= ?1",
                [now - DAY],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .map_err(db)?;
        result.attempts_24h = attempts as u32;
        result.failed_24h = failed as u32;
        result.addresses_24h = addresses as u32;
        Ok(result)
    }

    /// Everything the Users page shows. `accounts` is `(login, is_superadmin)`.
    pub fn snapshot(&self, accounts: &[(String, bool)], now: i64) -> Result<Snapshot, String> {
        let conn = self.conn();
        let first_day = now - now.rem_euclid(DAY) - (GRAPH_DAYS as i64 - 1) * DAY;
        let mut snapshot = Snapshot {
            days: day_stats(&conn, first_day, None)?,
            first_day,
            ..Snapshot::default()
        };
        for (login, superadmin) in accounts {
            let days = day_stats(&conn, first_day, Some((login, *superadmin)))?;
            let recent = conn
                .prepare(
                    "SELECT started_at, last_seen_at, ended_at, end_reason, ip, user_agent, impersonated
                       FROM sessions_log WHERE login = ?1 AND superadmin = ?2
                      ORDER BY started_at DESC, sid DESC LIMIT ?3",
                )
                .map_err(db)?
                .query_map(params![login, superadmin, RECENT_SESSIONS], |row| {
                    Ok(SessionRow {
                        started_at: row.get(0)?,
                        last_seen_at: row.get(1)?,
                        ended_at: row.get(2)?,
                        end_reason: row.get(3)?,
                        ip: row.get(4)?,
                        user_agent: row.get(5)?,
                        impersonated: row.get(6)?,
                    })
                })
                .map_err(db)?
                .collect::<Result<Vec<_>, _>>()
                .map_err(db)?;
            let services = conn
                .prepare(
                    "SELECT a.service_id, SUM(a.hits), MAX(a.last_at)
                       FROM service_access a JOIN sessions_log s ON s.sid = a.sid
                      WHERE s.login = ?1 AND s.superadmin = ?2 AND s.impersonated = 0
                      GROUP BY a.service_id ORDER BY MAX(a.last_at) DESC LIMIT ?3",
                )
                .map_err(db)?
                .query_map(params![login, superadmin, TOP_SERVICES], |row| {
                    Ok(ServiceUse {
                        service_id: row.get(0)?,
                        hits: row.get::<_, i64>(1)? as u64,
                        last_at: row.get(2)?,
                    })
                })
                .map_err(db)?
                .collect::<Result<Vec<_>, _>>()
                .map_err(db)?;
            let failed_30d: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM audit_events WHERE kind = 'login_failed' AND login = ?1 AND ts >= ?2",
                    params![login, first_day],
                    |row| row.get(0),
                )
                .map_err(db)?;
            let last_login: Option<i64> = conn
                .query_row(
                    "SELECT MAX(started_at) FROM sessions_log
                      WHERE login = ?1 AND superadmin = ?2 AND impersonated = 0",
                    params![login, superadmin],
                    |row| row.get(0),
                )
                .optional()
                .map_err(db)?
                .flatten();
            snapshot.accounts.push(AccountActivity {
                login: login.clone(),
                sessions_30d: days.iter().map(|d| d.sessions).sum(),
                days,
                recent,
                services,
                failed_30d: failed_30d as u32,
                last_login,
            });
        }
        snapshot.recent_events = conn
            .prepare(
                "SELECT ts, kind, login, actor, ip FROM audit_events ORDER BY ts DESC, id DESC LIMIT ?1",
            )
            .map_err(db)?
            .query_map([RECENT_EVENTS], |row| {
                Ok(AuditRow {
                    ts: row.get(0)?,
                    kind: row.get(1)?,
                    login: row.get(2)?,
                    actor: row.get(3)?,
                    ip: row.get(4)?,
                })
            })
            .map_err(db)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(db)?;
        let unknown: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM audit_events WHERE kind = 'login_failed' AND login IS NULL AND ts >= ?1",
                [first_day],
                |row| row.get(0),
            )
            .map_err(db)?;
        snapshot.unknown_failures_30d = unknown as u32;
        Ok(snapshot)
    }
}

fn insert_audit(conn: &Connection, audit: &AuditEvent) -> Result<(), String> {
    conn.execute(
        "INSERT INTO audit_events (ts, kind, login, actor, ip, user_agent) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            audit.at,
            audit.kind.as_str(),
            audit.login,
            audit.actor,
            audit.ip,
            audit.user_agent
        ],
    )
    .map_err(db)?;
    Ok(())
}

fn insert_login(conn: &Connection, attempt: &LoginAttempt) -> Result<(), String> {
    conn.execute(
        "INSERT INTO login_attempts (ts, ip, login, known, success, user_agent) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            attempt.at,
            attempt.ip,
            attempt.login,
            attempt.known,
            attempt.success,
            attempt.user_agent
        ],
    )
    .map_err(db)?;
    Ok(())
}

/// Real connections per day over [`GRAPH_DAYS`] days (an impersonated session is not a connection),
/// for every account or for one.
fn day_stats(
    conn: &Connection,
    first_day: i64,
    account: Option<(&str, bool)>,
) -> Result<Vec<DayStat>, String> {
    let mut days = vec![DayStat::default(); GRAPH_DAYS];
    let (login, superadmin) = match account {
        Some((login, superadmin)) => (Some(login), superadmin),
        None => (None, false),
    };
    let rows = conn
        .prepare(
            "SELECT (started_at - ?1) / 86400,
                    COUNT(*),
                    SUM(MAX(COALESCE(ended_at, last_seen_at) - started_at, 0))
               FROM sessions_log
              WHERE started_at >= ?1 AND impersonated = 0
                AND (?2 IS NULL OR (login = ?2 AND superadmin = ?3))
              GROUP BY 1",
        )
        .map_err(db)?
        .query_map(params![first_day, login, superadmin], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, i64>(2)?,
            ))
        })
        .map_err(db)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(db)?;
    for (day, sessions, seconds) in rows {
        if let Some(slot) = usize::try_from(day).ok().and_then(|d| days.get_mut(d)) {
            slot.sessions = sessions as u32;
            slot.seconds = seconds;
        }
    }
    Ok(days)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tracking::{AuditKind, EndReason, Hit, SessionStart};
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

    #[test]
    fn users_round_trip_with_their_roles_and_reject_a_duplicate() {
        let store = Store::open_in_memory();
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
    }

    #[test]
    fn updating_a_user_replaces_its_roles_and_optionally_its_password() {
        let store = Store::open_in_memory();
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
    }

    #[test]
    fn deleting_a_user_deletes_its_roles_but_keeps_its_history() {
        let store = Store::open_in_memory();
        store.insert_user(&user("alice", &["ops"])).unwrap();
        store
            .apply(&Batch {
                events: vec![start(1, "alice", false, false, NOW)],
                ..Batch::default()
            })
            .unwrap();
        store.remove_user("alice").unwrap();
        let roles: i64 = store
            .conn()
            .query_row("SELECT COUNT(*) FROM user_roles", [], |r| r.get(0))
            .unwrap();
        assert_eq!(roles, 0);
        let snap = store.snapshot(&[("alice".into(), false)], NOW).unwrap();
        assert_eq!(snap.accounts[0].recent.len(), 1);
    }

    #[test]
    fn the_database_survives_being_reopened() {
        let dir = std::env::temp_dir().join(format!("portal-store-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("portal.db");
        {
            let store = Store::open(&path).unwrap();
            store.insert_user(&user("alice", &["ops"])).unwrap();
        }
        let store = Store::open(&path).unwrap();
        assert_eq!(store.load_users().unwrap()[0].roles, ["ops"]);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_newer_schema_is_refused() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("PRAGMA user_version = 99;").unwrap();
        assert!(Store::init(conn).is_err());
    }

    #[test]
    fn a_session_lifecycle_services_and_failures_show_up_in_the_snapshot() {
        let store = Store::open_in_memory();
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
    }

    #[test]
    fn hits_accumulate_across_flushes_and_open_sessions_close_on_restart() {
        let store = Store::open_in_memory();
        store
            .apply(&Batch {
                events: vec![start(5, "alice", false, false, 100)],
                ..Batch::default()
            })
            .unwrap();
        for last_at in [200, 300] {
            let mut hits = HashMap::new();
            hits.insert(
                (5u64, "windmill".to_string()),
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
        store.close_open_sessions(1_000, "restart").unwrap();
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
    }

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
    fn sign_in_attempts_are_listed_newest_first_with_the_figures_of_the_last_day() {
        let store = Store::open_in_memory();
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
    }

    #[test]
    fn a_hostile_login_is_stored_and_read_back_as_the_text_it_is() {
        let store = Store::open_in_memory();
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
    }

    #[test]
    fn the_sign_in_log_pages_and_a_page_past_the_end_comes_back_to_the_last_one() {
        let store = Store::open_in_memory();
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
        let empty = Store::open_in_memory().login_attempts(NOW, 7, false).unwrap();
        assert_eq!((empty.page, empty.pages(), empty.rows.len()), (0, 1, 0));
    }

    #[test]
    fn the_sign_in_log_is_pruned_by_age_and_capped_in_size() {
        let store = Store::open_in_memory();
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

        let store = Store::open_in_memory();
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
    }

    #[test]
    fn a_database_at_version_one_gains_the_sign_in_log_when_opened() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(&format!("{SCHEMA} PRAGMA user_version = 1;")).unwrap();
        conn.execute(
            "INSERT INTO users (login, password_hash, created_at) VALUES ('alice', 'h', 1)",
            [],
        )
        .unwrap();
        let store = Store::init(conn).unwrap();
        assert_eq!(store.load_users().unwrap().len(), 1, "what was there stays");
        assert_eq!(store.login_attempts(NOW, 0, false).unwrap().matching, 0);
        let version: i64 = store
            .conn()
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(version, SCHEMA_VERSION);
    }

    #[test]
    fn pruning_removes_old_rows_with_their_service_accesses() {
        let store = Store::open_in_memory();
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
        let count = |sql: &str| -> i64 { store.conn().query_row(sql, [], |r| r.get(0)).unwrap() };
        assert_eq!(count("SELECT COUNT(*) FROM sessions_log"), 1);
        assert_eq!(count("SELECT COUNT(*) FROM service_access"), 1);
        assert_eq!(count("SELECT COUNT(*) FROM audit_events"), 0);
    }
}
