//! The SQLite backend: one file, one connection behind a mutex.
//!
//! Callers run these methods on blocking threads.
//! Such a thread is the writer thread of [`crate::tracking`], or a `spawn_blocking` call from a handler.
//! The request path reads the in-memory directory instead. It never waits on the disk.

use std::path::Path;
use std::sync::Mutex;

use rusqlite::{params, Connection, ErrorCode, OptionalExtension};

use super::*;
use crate::grants::Grant;
use crate::tracking::{AuditEvent, Batch, Event, LoginAttempt};

const SCHEMA_VERSION: i64 = 3;

/// The tables of schema version 1 that the current version keeps.
const SCHEMA: &str = "
CREATE TABLE users (
  id INTEGER PRIMARY KEY,
  login TEXT NOT NULL UNIQUE,
  password_hash TEXT NOT NULL,
  created_at INTEGER NOT NULL
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

/// Added by schema version 2. A database at version 1 gets it when it is opened.
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

/// Schema version 3: the grants of the users. The table of the old access model is dropped with its rows.
/// After the migration, a user has the grants that the settings pages give again.
const GRANTS_SCHEMA: &str = "
DROP TABLE IF EXISTS user_roles;
CREATE TABLE user_grants (
  user_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  action TEXT NOT NULL,
  resource_type TEXT NOT NULL,
  resource_id TEXT NOT NULL,
  PRIMARY KEY (user_id, action, resource_type, resource_id)
);
";

/// Reads the grants of one user. A row that is not a valid grant stops the read.
/// The database holds only what the server wrote.
fn grants_of(conn: &Connection, user_id: i64) -> Result<Vec<Grant>, String> {
    let rows = conn
        .prepare_cached(
            "SELECT action, resource_type, resource_id FROM user_grants
              WHERE user_id = ?1 ORDER BY action, resource_id",
        )
        .map_err(db)?
        .query_map([user_id], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })
        .map_err(db)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(db)?;
    rows.into_iter()
        .map(|(action, kind, id)| {
            if kind != crate::grants::SERVICE_TYPE {
                return Err(format!("user_grants: unknown resource type {kind:?}"));
            }
            Grant::new(&action, &id).map_err(|e| format!("user_grants: {e}"))
        })
        .collect()
}

fn insert_grants(conn: &Connection, user_id: i64, grants: &[Grant]) -> rusqlite::Result<()> {
    for grant in grants {
        conn.execute(
            "INSERT OR IGNORE INTO user_grants (user_id, action, resource_type, resource_id)
             VALUES (?1, ?2, ?3, ?4)",
            params![user_id, grant.action, crate::grants::SERVICE_TYPE, grant.service],
        )?;
    }
    Ok(())
}

pub struct Sqlite {
    conn: Mutex<Connection>,
}

fn db(error: rusqlite::Error) -> String {
    error.to_string()
}

impl Sqlite {
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
        // `journal_mode` answers with a row. Therefore, the code reads it and does not execute it.
        let _: String = conn
            .query_row("PRAGMA journal_mode = WAL", [], |row| row.get(0))
            .map_err(db)?;
        conn.execute_batch("PRAGMA foreign_keys = ON; PRAGMA synchronous = NORMAL;")
            .map_err(db)?;
        let version: i64 = conn
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .map_err(db)?;
        // Each version adds its part. The part and the new version number are written in one transaction.
        let missing = match version {
            0 => format!("{SCHEMA} {LOGIN_SCHEMA} {GRANTS_SCHEMA}"),
            1 => format!("{LOGIN_SCHEMA} {GRANTS_SCHEMA}"),
            2 => GRANTS_SCHEMA.to_string(),
            _ => String::new(),
        };
        match version {
            0..=2 => conn
                .execute_batch(&format!(
                    "BEGIN; {missing} PRAGMA user_version = {SCHEMA_VERSION}; COMMIT;"
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

    #[cfg(test)]
    pub fn count(&self, table: &str) -> i64 {
        self.conn()
            .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0))
            .unwrap()
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
                        grants: Vec::new(),
                        created_at: row.get(3)?,
                    },
                ))
            })
            .map_err(db)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(db)?;
        for (id, user) in &mut users {
            user.grants = grants_of(&conn, *id)?;
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
        insert_grants(&tx, id, &user.grants).map_err(|e| InsertError::Db(db(e)))?;
        tx.commit().map_err(|e| InsertError::Db(db(e)))
    }

    /// Replaces the password hash and the grants of a user. Each is replaced only when it is given.
    /// The change runs in one transaction. `false` if no such user.
    pub fn update_user(
        &self,
        login: &str,
        password_hash: Option<&str>,
        grants: Option<&[Grant]>,
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
        if let Some(grants) = grants {
            tx.execute("DELETE FROM user_grants WHERE user_id = ?1", [id])
                .map_err(db)?;
            insert_grants(&tx, id, grants).map_err(db)?;
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

    /// Deletes the rows that started before `cutoff`.
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

    /// One page of the sign-in log, newest first. If `only_failed` is true, the page holds only the refused attempts.
    /// A page past the end is brought back to the last page.
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

/// Real connections per day over [`GRAPH_DAYS`] days. An impersonated session is not a connection.
/// The count is for every account, or for one account.
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

impl Sqlite {
    /// Opens an existing file without changing it. The import uses this to read all data in the file.
    pub fn open_read_only(path: &Path) -> Result<Self, String> {
        let conn = Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .map_err(|e| format!("cannot open the database {}: {e}", path.display()))?;
        conn.busy_timeout(std::time::Duration::from_secs(5))
            .map_err(db)?;
        let version: i64 = conn
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .map_err(db)?;
        if !(1..=SCHEMA_VERSION).contains(&version) {
            return Err(format!(
                "{} is not a portal database this version can read (schema version {version})",
                path.display()
            ));
        }
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    /// Every table, as the import copies it. A version 1 file has no sign-in log yet.
    pub fn dump(&self) -> Result<Dump, String> {
        let guard = self.conn();
        // The code uses one read transaction. The live portal may write in the meantime, so the tables must show one moment.
        let tx = guard.unchecked_transaction().map_err(db)?;
        let conn: &Connection = &tx;
        let version: i64 = conn
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .map_err(db)?;
        let users = conn
            .prepare("SELECT id, login, password_hash, created_at FROM users ORDER BY id")
            .map_err(db)?
            .query_map([], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    UserRow {
                        login: row.get(1)?,
                        password_hash: row.get(2)?,
                        grants: Vec::new(),
                        created_at: row.get(3)?,
                    },
                ))
            })
            .map_err(db)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(db)?;
        // A file before version 3 has no grants. Its users get the grants again in the settings pages.
        let mut with_grants = Vec::with_capacity(users.len());
        for (id, mut user) in users {
            if version >= 3 {
                user.grants = grants_of(conn, id)?;
            }
            with_grants.push((id, user));
        }
        let sessions = conn
            .prepare(
                "SELECT sid, login, superadmin, impersonated, ip, user_agent, started_at,
                        last_seen_at, ended_at, end_reason
                   FROM sessions_log ORDER BY started_at, sid",
            )
            .map_err(db)?
            .query_map([], |row| {
                Ok(LoggedSession {
                    sid: row.get(0)?,
                    login: row.get(1)?,
                    superadmin: row.get(2)?,
                    impersonated: row.get(3)?,
                    ip: row.get(4)?,
                    user_agent: row.get(5)?,
                    started_at: row.get(6)?,
                    last_seen_at: row.get(7)?,
                    ended_at: row.get(8)?,
                    end_reason: row.get(9)?,
                })
            })
            .map_err(db)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(db)?;
        let accesses = conn
            .prepare("SELECT sid, service_id, first_at, last_at, hits, ip FROM service_access")
            .map_err(db)?
            .query_map([], |row| {
                Ok(LoggedAccess {
                    sid: row.get(0)?,
                    service_id: row.get(1)?,
                    first_at: row.get(2)?,
                    last_at: row.get(3)?,
                    hits: row.get(4)?,
                    ip: row.get(5)?,
                })
            })
            .map_err(db)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(db)?;
        let audit = conn
            .prepare("SELECT id, ts, kind, login, actor, ip, user_agent FROM audit_events ORDER BY id")
            .map_err(db)?
            .query_map([], |row| {
                Ok(LoggedAudit {
                    id: row.get(0)?,
                    ts: row.get(1)?,
                    kind: row.get(2)?,
                    login: row.get(3)?,
                    actor: row.get(4)?,
                    ip: row.get(5)?,
                    user_agent: row.get(6)?,
                })
            })
            .map_err(db)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(db)?;
        let logins = if version >= 2 {
            conn.prepare(
                "SELECT id, ts, ip, login, known, success, user_agent FROM login_attempts ORDER BY id",
            )
            .map_err(db)?
            .query_map([], |row| {
                Ok(LoggedLogin {
                    id: row.get(0)?,
                    ts: row.get(1)?,
                    ip: row.get(2)?,
                    login: row.get(3)?,
                    known: row.get(4)?,
                    success: row.get(5)?,
                    user_agent: row.get(6)?,
                })
            })
            .map_err(db)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(db)?
        } else {
            Vec::new()
        };
        Ok(Dump {
            users: with_grants,
            sessions,
            accesses,
            audit,
            logins,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: i64 = 1_000_000;

    fn user(login: &str, services: &[&str]) -> UserRow {
        UserRow {
            login: login.into(),
            password_hash: "$argon2id$stub".into(),
            grants: services.iter().map(|s| Grant::new("access", s).unwrap()).collect(),
            created_at: 1_000,
        }
    }

    fn version(store: &Sqlite) -> i64 {
        store
            .conn()
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap()
    }

    fn has_table(store: &Sqlite, name: &str) -> bool {
        store
            .conn()
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = ?1",
                [name],
                |r| r.get::<_, i64>(0),
            )
            .unwrap()
            == 1
    }

    #[test]
    fn the_database_survives_being_reopened() {
        let dir = std::env::temp_dir().join(format!("portal-store-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("portal.db");
        {
            let store = Sqlite::open(&path).unwrap();
            store.insert_user(&user("alice", &["ops"])).unwrap();
        }
        let store = Sqlite::open(&path).unwrap();
        assert_eq!(
            store.load_users().unwrap()[0].grants,
            [Grant::new("access", "ops").unwrap()]
        );
        std::fs::remove_dir_all(dir).unwrap();
    }


    #[test]
    fn a_newer_schema_is_refused() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("PRAGMA user_version = 99;").unwrap();
        assert!(Sqlite::init(conn).is_err());
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
        let store = Sqlite::init(conn).unwrap();
        assert_eq!(store.load_users().unwrap().len(), 1, "what was there stays");
        assert_eq!(store.login_attempts(NOW, 0, false).unwrap().matching, 0);
        assert_eq!(version(&store), SCHEMA_VERSION);
        assert!(has_table(&store, "user_grants"));
    }

    /// The tables of a file that an older portal wrote. The file is at version 2.
    /// It has the table of the old access model, with some rows in it.
    const VERSION_2_ACCESS_TABLE: &str = "
CREATE TABLE user_roles (
  user_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  role TEXT NOT NULL,
  PRIMARY KEY (user_id, role)
);";

    #[test]
    fn the_migration_to_version_three_drops_the_old_access_data_and_adds_the_grants() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(&format!(
            "{SCHEMA} {VERSION_2_ACCESS_TABLE} {LOGIN_SCHEMA} PRAGMA user_version = 2;"
        ))
        .unwrap();
        conn.execute_batch(
            "INSERT INTO users (id, login, password_hash, created_at) VALUES (1, 'alice', 'h', 1);
             INSERT INTO user_roles (user_id, role) VALUES (1, 'ops'), (1, 'dev');",
        )
        .unwrap();
        let store = Sqlite::init(conn).unwrap();
        assert_eq!(version(&store), SCHEMA_VERSION);
        assert!(!has_table(&store, "user_roles"), "the old data is gone");
        assert!(has_table(&store, "user_grants"));
        let users = store.load_users().unwrap();
        assert_eq!(users.len(), 1, "the account stays");
        assert!(users[0].grants.is_empty(), "a migrated user has no grant");
        // The new table works, and a second open changes nothing.
        let grant = Grant::new("access", "grafana").unwrap();
        assert!(store.update_user("alice", None, Some(&[grant.clone()])).unwrap());
        assert_eq!(store.load_users().unwrap()[0].grants, [grant]);
    }

    #[test]
    fn a_new_database_is_at_the_last_version_without_the_old_access_table() {
        let store = Sqlite::open_in_memory();
        assert_eq!(version(&store), SCHEMA_VERSION);
        assert!(!has_table(&store, "user_roles"));
        assert!(has_table(&store, "user_grants"));
    }

    #[test]
    fn a_grant_row_that_the_server_did_not_write_stops_the_read() {
        let store = Sqlite::open_in_memory();
        store.insert_user(&user("alice", &[])).unwrap();
        store
            .conn()
            .execute(
                "INSERT INTO user_grants (user_id, action, resource_type, resource_id)
                 VALUES (1, 'access', 'database', 'x')",
                [],
            )
            .unwrap();
        assert!(store.load_users().unwrap_err().contains("resource type"));
    }

}
