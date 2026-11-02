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

mod keys;

const SCHEMA_VERSION: i64 = 7;

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

/// Schema version 4: a grant can name a sub-resource of the service. SQLite cannot change a primary
/// key, so the table is made again with its rows. The rows of version 3 are grants on the whole
/// service: their two new columns are empty.
const SUB_RESOURCE_SCHEMA: &str = "
CREATE TABLE user_grants_v4 (
  user_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  action TEXT NOT NULL,
  resource_type TEXT NOT NULL,
  resource_id TEXT NOT NULL,
  sub_resource_type TEXT NOT NULL DEFAULT '',
  sub_resource_id TEXT NOT NULL DEFAULT '',
  PRIMARY KEY (user_id, action, resource_type, resource_id, sub_resource_type, sub_resource_id)
);
INSERT INTO user_grants_v4 (user_id, action, resource_type, resource_id)
  SELECT user_id, action, resource_type, resource_id FROM user_grants;
DROP TABLE user_grants;
ALTER TABLE user_grants_v4 RENAME TO user_grants;
";

/// Schema version 5: the path of a grant is one column, `resource_path`. It holds the segments
/// after the service in their text form, for example `host:local/database:analytics`. It is empty
/// for a grant on the whole service. The rows of version 4 move by [`migrate_to_resource_path`].
const RESOURCE_PATH_SCHEMA: &str = "
CREATE TABLE user_grants_v5 (
  user_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  action TEXT NOT NULL,
  resource_type TEXT NOT NULL,
  resource_id TEXT NOT NULL,
  resource_path TEXT NOT NULL DEFAULT '',
  PRIMARY KEY (user_id, action, resource_type, resource_id, resource_path)
);
";

/// Schema version 6: the keys of the users, their grants and their counters (portal 4, 5, 7 and
/// 10.7). A file at version 6 opens only with the programs that understand version 6. A key
/// belongs to one user, or to the superadmin: then `owner_id` is NULL (portal 3.7b). Deleting a
/// user deletes the keys of the user with their grants and their counters. The `id` of a key is
/// text that the program makes at random. The key table holds the SHA-256 digest of a key, never
/// the key. A revoked key leaves one summary row in `key_history`. The column `limits` of a grant
/// holds the limit values as JSON. The row of `directory_version` counts the writes to the users,
/// the grants and the keys. The tables have the form of PostgreSQL (portal 10.4a).
const KEYS_SCHEMA: &str = "
ALTER TABLE user_grants ADD COLUMN limits TEXT NOT NULL DEFAULT '{}';
CREATE TABLE keys (
  id TEXT PRIMARY KEY,
  owner_id INTEGER REFERENCES users(id) ON DELETE CASCADE,
  name TEXT NOT NULL,
  digest BLOB NOT NULL UNIQUE,
  hint TEXT NOT NULL,
  created_at INTEGER NOT NULL,
  expires_at INTEGER,
  last_check_at INTEGER,
  check_count INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX keys_owner ON keys (owner_id, created_at, id);
CREATE INDEX keys_created ON keys (created_at, id);
CREATE TABLE key_grants (
  key_id TEXT NOT NULL REFERENCES keys(id) ON DELETE CASCADE,
  action TEXT NOT NULL,
  resource_type TEXT NOT NULL,
  resource_id TEXT NOT NULL,
  resource_path TEXT NOT NULL DEFAULT '',
  limits TEXT NOT NULL DEFAULT '{}',
  PRIMARY KEY (key_id, action, resource_type, resource_id, resource_path)
);
CREATE TABLE key_history (
  id TEXT PRIMARY KEY,
  owner_id INTEGER REFERENCES users(id) ON DELETE CASCADE,
  name TEXT NOT NULL,
  created_at INTEGER NOT NULL,
  revoked_at INTEGER NOT NULL,
  last_check_at INTEGER,
  check_count INTEGER NOT NULL
);
CREATE INDEX key_history_owner ON key_history (owner_id, created_at, id);
CREATE INDEX key_history_created ON key_history (created_at, id);
CREATE TABLE budget_counters (
  id INTEGER PRIMARY KEY,
  user_id INTEGER REFERENCES users(id) ON DELETE CASCADE,
  key_id TEXT REFERENCES keys(id) ON DELETE CASCADE,
  file_subject TEXT,
  service TEXT NOT NULL,
  budget TEXT NOT NULL,
  \"window\" TEXT NOT NULL CHECK (\"window\" IN ('second', 'minute', 'hour', 'day')),
  window_start INTEGER NOT NULL,
  units INTEGER NOT NULL,
  CHECK ((user_id IS NOT NULL) + (key_id IS NOT NULL) + (file_subject IS NOT NULL) = 1)
);
CREATE UNIQUE INDEX budget_counters_user ON budget_counters (user_id, service, budget, \"window\");
CREATE UNIQUE INDEX budget_counters_key ON budget_counters (key_id, service, budget, \"window\");
CREATE UNIQUE INDEX budget_counters_file ON budget_counters (file_subject, service, budget, \"window\");
CREATE TABLE directory_version (
  id INTEGER PRIMARY KEY CHECK (id = 1),
  version INTEGER NOT NULL
);
INSERT INTO directory_version (id, version) VALUES (1, 0);
";

/// Schema version 7: the kind of a key (portal 5.1). The kind is the prefix of the text of the key:
/// `mcp` for an agent, `api` for a service. A key that exists before this version is an MCP key: no
/// other kind existed.
const KEY_KIND_SCHEMA: &str = "
ALTER TABLE keys ADD COLUMN kind TEXT NOT NULL DEFAULT 'mcp' CHECK (kind IN ('mcp', 'api'));
";

/// The columns of `user_grants` in a file of a given schema version.
#[derive(Clone, Copy, PartialEq, Eq)]
enum GrantColumns {
    /// Version 3: grants on the whole service only.
    ServiceOnly,
    /// Version 4: `sub_resource_type` and `sub_resource_id`.
    SubResource,
    /// Version 5: `resource_path`.
    Path,
    /// Version 6 and later: `resource_path` and `limits`.
    PathLimits,
}

impl GrantColumns {
    fn of(version: i64) -> Self {
        match version {
            ..=3 => Self::ServiceOnly,
            4 => Self::SubResource,
            5 => Self::Path,
            _ => Self::PathLimits,
        }
    }
}

/// Moves the grants of version 4 to the table of version 5. A grant on a sub-resource
/// `(type, id)` becomes the path `type:<encoded id>`. A row that does not fit the rules of version
/// 5 stops the migration: the caller rolls back, and the file stays at version 4.
fn migrate_to_resource_path(conn: &Connection) -> Result<(), String> {
    let rows = conn
        .prepare(
            "SELECT user_id, action, resource_type, resource_id, sub_resource_type, sub_resource_id
               FROM user_grants",
        )
        .map_err(db)?
        .query_map([], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, String>(5)?,
            ))
        })
        .map_err(db)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(db)?;
    conn.execute_batch(RESOURCE_PATH_SCHEMA).map_err(db)?;
    for (user_id, action, kind, id, sub_type, sub_id) in rows {
        let path = migrated_path(&sub_type, &sub_id)?;
        conn.execute(
            "INSERT INTO user_grants_v5 (user_id, action, resource_type, resource_id, resource_path)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![user_id, action, kind, id, path],
        )
        .map_err(db)?;
    }
    conn.execute_batch("DROP TABLE user_grants; ALTER TABLE user_grants_v5 RENAME TO user_grants;")
        .map_err(db)
}

/// Reads the grants of one user. A row that is not a valid grant stops the read.
/// The database holds only what the server wrote. `columns` gives the layout of the table: the
/// import reads files of earlier versions.
fn grants_of(conn: &Connection, user_id: i64, columns: GrantColumns) -> Result<Vec<Grant>, String> {
    let sql = match columns {
        GrantColumns::PathLimits => {
            "SELECT action, resource_type, resource_id, resource_path, '', limits
               FROM user_grants WHERE user_id = ?1
              ORDER BY action, resource_id, resource_path"
        }
        GrantColumns::Path => {
            "SELECT action, resource_type, resource_id, resource_path, '', '{}'
               FROM user_grants WHERE user_id = ?1
              ORDER BY action, resource_id, resource_path"
        }
        GrantColumns::SubResource => {
            "SELECT action, resource_type, resource_id, sub_resource_type, sub_resource_id, '{}'
               FROM user_grants WHERE user_id = ?1
              ORDER BY action, resource_id, sub_resource_type, sub_resource_id"
        }
        GrantColumns::ServiceOnly => {
            "SELECT action, resource_type, resource_id, '', '', '{}' FROM user_grants
              WHERE user_id = ?1 ORDER BY action, resource_id"
        }
    };
    let rows = conn
        .prepare_cached(sql)
        .map_err(db)?
        .query_map([user_id], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, String>(5)?,
            ))
        })
        .map_err(db)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(db)?;
    let mut grants = rows
        .iter()
        .map(|(action, kind, id, third, fourth, limits)| {
            let path = match columns {
                GrantColumns::Path | GrantColumns::PathLimits => third.clone(),
                _ => migrated_path(third, fourth)?,
            };
            grant_from_row(action, kind, id, &path, limits)
        })
        .collect::<Result<Vec<_>, _>>()?;
    grants.sort();
    Ok(grants)
}

fn insert_grants(conn: &Connection, user_id: i64, grants: &[Grant]) -> rusqlite::Result<()> {
    for grant in grants {
        let (kind, id, path, limits) = grant_columns(grant);
        conn.execute(
            "INSERT OR IGNORE INTO user_grants
               (user_id, action, resource_type, resource_id, resource_path, limits)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![user_id, grant.action, kind, id, path, limits],
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

    fn init(mut conn: Connection) -> Result<Self, String> {
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
        // Each version adds its part. The parts and the new version number are written in one
        // transaction: a failure leaves the file as it was.
        let missing = match version {
            0 => format!("{SCHEMA} {LOGIN_SCHEMA} {GRANTS_SCHEMA} {SUB_RESOURCE_SCHEMA}"),
            1 => format!("{LOGIN_SCHEMA} {GRANTS_SCHEMA} {SUB_RESOURCE_SCHEMA}"),
            2 => format!("{GRANTS_SCHEMA} {SUB_RESOURCE_SCHEMA}"),
            3 => SUB_RESOURCE_SCHEMA.to_string(),
            _ => String::new(),
        };
        match version {
            0..=4 => {
                let tx = conn.transaction().map_err(db)?;
                tx.execute_batch(&missing).map_err(db)?;
                migrate_to_resource_path(&tx)?;
                tx.execute_batch(KEYS_SCHEMA).map_err(db)?;
                tx.execute_batch(KEY_KIND_SCHEMA).map_err(db)?;
                tx.execute_batch(&format!("PRAGMA user_version = {SCHEMA_VERSION};"))
                    .map_err(db)?;
                tx.commit().map_err(db)?;
            }
            5 => {
                let tx = conn.transaction().map_err(db)?;
                tx.execute_batch(KEYS_SCHEMA).map_err(db)?;
                tx.execute_batch(KEY_KIND_SCHEMA).map_err(db)?;
                tx.execute_batch(&format!("PRAGMA user_version = {SCHEMA_VERSION};"))
                    .map_err(db)?;
                tx.commit().map_err(db)?;
            }
            6 => {
                let tx = conn.transaction().map_err(db)?;
                tx.execute_batch(KEY_KIND_SCHEMA).map_err(db)?;
                tx.execute_batch(&format!("PRAGMA user_version = {SCHEMA_VERSION};"))
                    .map_err(db)?;
                tx.commit().map_err(db)?;
            }
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
    pub fn execute_raw(&self, sql: &str) {
        self.conn().execute_batch(sql).unwrap();
    }

    #[cfg(test)]
    pub fn count(&self, table: &str) -> i64 {
        self.conn()
            .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0))
            .unwrap()
    }

    // Accounts --------------------------------------------------------------------------------

    #[cfg(test)]
    pub fn load_users(&self) -> Result<Vec<UserRow>, String> {
        Self::load_users_with(&self.conn())
    }

    fn load_users_with(conn: &Connection) -> Result<Vec<UserRow>, String> {
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
            user.grants = grants_of(conn, *id, GrantColumns::PathLimits)?;
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
        keys::insert_counter_rows(&tx, keys::Subject::User(id), &user.grants)
            .map_err(|e| InsertError::Db(db(e)))?;
        keys::bump(&tx).map_err(|e| InsertError::Db(db(e)))?;
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
            keys::insert_counter_rows(&tx, keys::Subject::User(id), grants).map_err(db)?;
        }
        keys::bump(&tx).map_err(db)?;
        tx.commit().map_err(db)?;
        Ok(true)
    }

    pub fn remove_user(&self, login: &str) -> Result<bool, String> {
        let mut conn = self.conn();
        let tx = conn.transaction().map_err(db)?;
        let changed = tx
            .execute("DELETE FROM users WHERE login = ?1", [login])
            .map_err(db)?;
        if changed > 0 {
            keys::bump(&tx).map_err(db)?;
        }
        tx.commit().map_err(db)?;
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
    pub fn login_attempts(&self, page: u32, only_failed: bool) -> Result<LoginPage, String> {
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
                params![
                    only_failed,
                    LOGIN_PAGE_SIZE,
                    i64::from(result.page) * LOGIN_PAGE_SIZE
                ],
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
        let (total, refused): (i64, i64) = conn
            .query_row(
                "SELECT COUNT(*), COALESCE(SUM(success = 0), 0) FROM login_attempts",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .map_err(db)?;
        result.total = total as u32;
        result.refused = refused as u32;
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
                user.grants = grants_of(conn, id, GrantColumns::of(version))?;
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
            .prepare(
                "SELECT id, ts, kind, login, actor, ip, user_agent FROM audit_events ORDER BY id",
            )
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
        // A file of version 6 has the keys and the summary rows of the revoked keys.
        let (keys, key_history) = if version >= 6 {
            (
                keys::read_keys(conn, version >= 7)?,
                keys::read_history(conn)?,
            )
        } else {
            (Vec::new(), Vec::new())
        };
        Ok(Dump {
            users: with_grants,
            sessions,
            accesses,
            audit,
            logins,
            keys,
            key_history,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn user(login: &str, services: &[&str]) -> UserRow {
        UserRow {
            login: login.into(),
            password_hash: "$argon2id$stub".into(),
            grants: services
                .iter()
                .map(|s| Grant::new("access", s).unwrap())
                .collect(),
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

    /// Adds a key with a grant, a counter and a summary row for the user. The tables are those of
    /// version 6.
    fn insert_key(store: &Sqlite, user_login: &str) {
        let conn = store.conn();
        conn.execute(
            "INSERT INTO keys (id, owner_id, name, digest, hint, created_at, expires_at)
             SELECT 'key1', id, 'agent', x'00ff', 'ab12', 1, 2 FROM users WHERE login = ?1",
            [user_login],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO key_grants (key_id, action, resource_type, resource_id, limits)
             VALUES ('key1', 'access', 'service', 'db',
                     '{\"requests\":[{\"amount\":10,\"window\":\"minute\"}]}')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO budget_counters (key_id, service, budget, \"window\", window_start, units)
             VALUES ('key1', 'db', 'requests', 'minute', 0, 0)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO key_history (id, owner_id, name, created_at, revoked_at, check_count)
             SELECT 'key0', id, 'old', 1, 2, 0 FROM users WHERE login = ?1",
            [user_login],
        )
        .unwrap();
    }

    fn count(store: &Sqlite, table: &str) -> i64 {
        store
            .conn()
            .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0))
            .unwrap()
    }

    const KEY_TABLES: [&str; 5] = [
        "keys",
        "key_grants",
        "key_history",
        "budget_counters",
        "directory_version",
    ];

    fn has_column(store: &Sqlite, table: &str, column: &str) -> bool {
        let conn = store.conn();
        let mut statement = conn
            .prepare(&format!("SELECT name FROM pragma_table_info('{table}')"))
            .unwrap();
        let names: Vec<String> = statement
            .query_map([], |row| row.get(0))
            .unwrap()
            .map(Result::unwrap)
            .collect();
        names.iter().any(|n| n == column)
    }

    #[test]
    fn a_new_database_has_the_tables_of_the_keys() {
        let store = Sqlite::open_in_memory();
        assert_eq!(version(&store), 7);
        assert!(has_column(&store, "keys", "kind"));
        for table in KEY_TABLES {
            assert!(has_table(&store, table), "{table}");
        }
        assert!(has_column(&store, "user_grants", "limits"));
        assert!(has_column(&store, "key_grants", "limits"));
        assert_eq!(count(&store, "directory_version"), 1);
        let version: i64 = store
            .conn()
            .query_row("SELECT version FROM directory_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(version, 0);
    }

    #[test]
    fn a_database_at_version_six_gains_the_kind_of_a_key_and_its_keys_are_mcp_keys() {
        let dir = std::env::temp_dir().join(format!("portal-v6-kind-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("portal.db");
        let store = Sqlite::open(&path).unwrap();
        store.insert_user(&user("alice", &["db"])).unwrap();
        store.execute_raw(
            "INSERT INTO keys (id, owner_id, name, digest, hint, created_at)
             SELECT 'key1', id, 'agent', zeroblob(32), 'ab12', 1 FROM users WHERE login = 'alice'",
        );
        drop(store);
        // This is the file of the portal before version 7: the column `kind` did not exist.
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch("ALTER TABLE keys DROP COLUMN kind; PRAGMA user_version = 6;")
            .unwrap();
        drop(conn);
        let store = Sqlite::open(&path).unwrap();
        assert_eq!(version(&store), SCHEMA_VERSION);
        assert!(has_column(&store, "keys", "kind"));
        let keys = store.load_directory().unwrap().keys;
        assert_eq!(keys.len(), 1);
        assert_eq!(
            keys[0].kind,
            crate::keys::KeyKind::Mcp,
            "no other kind existed"
        );
        // The column accepts the two kinds and no other.
        store.execute_raw("UPDATE keys SET kind = 'api'");
        assert_eq!(
            store.load_directory().unwrap().keys[0].kind,
            crate::keys::KeyKind::Api
        );
        let bad = store.conn().execute("UPDATE keys SET kind = 'whk'", []);
        assert!(bad.is_err(), "the kind whk is reserved");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_database_at_version_five_gains_the_tables_of_the_keys_and_keeps_its_rows() {
        let dir = std::env::temp_dir().join(format!("portal-v5-keys-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("portal.db");
        let store = Sqlite::open(&path).unwrap();
        store.insert_user(&user("alice", &["db"])).unwrap();
        drop(store);
        // This is the file of the portal before version 6: the tables and the column did not exist.
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(
            "DROP TABLE budget_counters; DROP TABLE key_history; DROP TABLE key_grants;
             DROP TABLE keys; DROP TABLE directory_version;
             ALTER TABLE user_grants DROP COLUMN limits;
             PRAGMA user_version = 5;",
        )
        .unwrap();
        drop(conn);
        let store = Sqlite::open(&path).unwrap();
        assert_eq!(version(&store), SCHEMA_VERSION);
        for table in KEY_TABLES {
            assert!(has_table(&store, table), "{table}");
        }
        assert!(has_column(&store, "user_grants", "limits"));
        assert_eq!(
            store.load_users().unwrap(),
            [user("alice", &["db"])],
            "what was there stays"
        );
        // A second start finds nothing to do.
        drop(store);
        assert!(Sqlite::open(&path).is_ok());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn deleting_a_user_deletes_the_keys_their_grants_their_counters_and_their_history() {
        let store = Sqlite::open_in_memory();
        store.insert_user(&user("alice", &[])).unwrap();
        store.insert_user(&user("bob", &[])).unwrap();
        insert_key(&store, "alice");
        assert_eq!(count(&store, "keys"), 1);
        assert!(store.remove_user("bob").unwrap());
        assert_eq!(count(&store, "keys"), 1, "the key of alice stays");
        assert!(store.remove_user("alice").unwrap());
        for table in ["keys", "key_grants", "key_history", "budget_counters"] {
            assert_eq!(count(&store, table), 0, "{table}");
        }
    }

    #[test]
    fn a_key_digest_is_unique_and_a_key_needs_an_existing_owner_or_none() {
        let store = Sqlite::open_in_memory();
        store.insert_user(&user("alice", &[])).unwrap();
        insert_key(&store, "alice");
        let conn = store.conn();
        let twice = conn.execute(
            "INSERT INTO keys (id, owner_id, name, digest, hint, created_at)
             VALUES ('key2', NULL, 'x', x'00ff', 'cd34', 1)",
            [],
        );
        assert!(twice.is_err(), "the same digest cannot name two keys");
        let orphan = conn.execute(
            "INSERT INTO keys (id, owner_id, name, digest, hint, created_at)
             VALUES ('key3', 999, 'x', x'01', 'cd34', 1)",
            [],
        );
        assert!(orphan.is_err(), "a key needs an existing owner");
        // A key of the superadmin has no owner row and no expiry date (portal 3.7b, 5.3).
        conn.execute(
            "INSERT INTO keys (id, owner_id, name, digest, hint, created_at)
             VALUES ('key4', NULL, 'root key', x'02', 'ef56', 1)",
            [],
        )
        .unwrap();
    }

    #[test]
    fn a_counter_row_has_one_subject_and_one_row_for_each_budget_and_window() {
        let store = Sqlite::open_in_memory();
        store.insert_user(&user("alice", &[])).unwrap();
        insert_key(&store, "alice");
        let conn = store.conn();
        let insert = |user_id: &str, key_id: &str, file: &str, window: &str| {
            conn.execute(
                &format!(
                    "INSERT INTO budget_counters
                       (user_id, key_id, file_subject, service, budget, \"window\", window_start, units)
                     VALUES ({user_id}, {key_id}, {file}, 'db', 'requests', '{window}', 0, 0)"
                ),
                [],
            )
        };
        assert!(
            insert("NULL", "NULL", "NULL", "second").is_err(),
            "no subject"
        );
        assert!(
            insert("1", "'key1'", "NULL", "second").is_err(),
            "two subjects"
        );
        assert!(insert("1", "NULL", "NULL", "week").is_err(), "a bad window");
        assert!(insert("1", "NULL", "NULL", "minute").is_ok());
        assert!(
            insert("1", "NULL", "NULL", "minute").is_err(),
            "one row for each user, budget and window"
        );
        assert!(insert("NULL", "NULL", "'carol'", "minute").is_ok());
        assert!(
            insert("NULL", "NULL", "'carol'", "minute").is_err(),
            "one row for each subject of the file, budget and window"
        );
        assert!(
            insert("NULL", "'key1'", "NULL", "minute").is_err(),
            "the helper already made the row of this key"
        );
    }

    #[test]
    fn the_directory_version_holds_exactly_one_row() {
        let store = Sqlite::open_in_memory();
        let conn = store.conn();
        assert!(conn
            .execute(
                "INSERT INTO directory_version (id, version) VALUES (2, 0)",
                []
            )
            .is_err());
        assert!(conn
            .execute(
                "INSERT INTO directory_version (id, version) VALUES (1, 0)",
                []
            )
            .is_err());
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
        conn.execute_batch(&format!("{SCHEMA} PRAGMA user_version = 1;"))
            .unwrap();
        conn.execute(
            "INSERT INTO users (login, password_hash, created_at) VALUES ('alice', 'h', 1)",
            [],
        )
        .unwrap();
        let store = Sqlite::init(conn).unwrap();
        assert_eq!(store.load_users().unwrap().len(), 1, "what was there stays");
        assert_eq!(store.login_attempts(0, false).unwrap().matching, 0);
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
        assert!(store
            .update_user("alice", None, Some(std::slice::from_ref(&grant)))
            .unwrap());
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

    #[test]
    fn the_migration_to_version_four_keeps_the_grants_and_adds_the_sub_resources() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(&format!(
            "{SCHEMA} {LOGIN_SCHEMA} {GRANTS_SCHEMA} PRAGMA user_version = 3;"
        ))
        .unwrap();
        conn.execute_batch(
            "INSERT INTO users (id, login, password_hash, created_at) VALUES (1, 'alice', 'h', 1);
             INSERT INTO user_grants (user_id, action, resource_type, resource_id)
               VALUES (1, 'access', 'service', 'db'), (1, 'query.run.read', 'service', 'db');",
        )
        .unwrap();
        let store = Sqlite::init(conn).unwrap();
        assert_eq!(version(&store), SCHEMA_VERSION);
        assert!(!has_table(&store, "user_grants_v4"));
        let kept = vec![
            Grant::new("access", "db").unwrap(),
            Grant::new("query.run.read", "db").unwrap(),
        ];
        assert_eq!(
            store.load_users().unwrap()[0].grants,
            kept,
            "the grants stay"
        );
        let mut next = kept.clone();
        next.push(Grant::parse("query.run.write on service:db/database:analytics").unwrap());
        next.push(Grant::parse("query.run.write on service:db/database:logs").unwrap());
        next.sort();
        assert!(store.update_user("alice", None, Some(&next)).unwrap());
        assert_eq!(store.load_users().unwrap()[0].grants, next);
        assert!(store.remove_user("alice").unwrap());
        assert_eq!(store.count("user_grants"), 0, "the delete cascades");
    }

    #[test]
    fn a_sub_resource_row_that_the_server_did_not_write_stops_the_read() {
        let store = Sqlite::open_in_memory();
        store.insert_user(&user("alice", &[])).unwrap();
        store
            .conn()
            .execute(
                "INSERT INTO user_grants
                   (user_id, action, resource_type, resource_id, resource_path)
                 VALUES (1, 'access', 'service', 'db', 'database:x')",
                [],
            )
            .unwrap();
        assert!(store.load_users().unwrap_err().contains("sub-resource"));
    }

    /// A file of portal 4.1.0: schema version 4, with grants on one sub-resource.
    fn version_four(conn: &Connection, rows: &str) {
        conn.execute_batch(&format!(
            "{SCHEMA} {LOGIN_SCHEMA} {GRANTS_SCHEMA} {SUB_RESOURCE_SCHEMA} PRAGMA user_version = 4;
             INSERT INTO users (id, login, password_hash, created_at) VALUES (1, 'alice', 'h', 1);
             {rows}"
        ))
        .unwrap();
    }

    #[test]
    fn the_migration_to_version_five_moves_each_grant_to_a_path() {
        let conn = Connection::open_in_memory().unwrap();
        version_four(
            &conn,
            "INSERT INTO user_grants
               (user_id, action, resource_type, resource_id, sub_resource_type, sub_resource_id)
             VALUES (1, 'access', 'service', 'db', '', ''),
                    (1, 'query.run.read', 'service', 'db', 'database', 'analytics'),
                    (1, 'query.run.write', 'service', 'db', 'database', 'a/b:c'),
                    (1, 'query.run.write', 'service', 'db', 'database', 'été');",
        );
        let store = Sqlite::init(conn).unwrap();
        assert_eq!(version(&store), SCHEMA_VERSION);
        assert!(!has_table(&store, "user_grants_v5"));
        let paths: Vec<String> = store
            .conn()
            .prepare("SELECT resource_path FROM user_grants ORDER BY resource_path")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert_eq!(
            paths,
            [
                "",
                "database:%C3%A9t%C3%A9",
                "database:a%2Fb%3Ac",
                "database:analytics"
            ]
        );
        let mut expected: Vec<Grant> = [
            "access on service:db",
            "query.run.read on service:db/database:analytics",
            "query.run.write on service:db/database:a%2Fb%3Ac",
            "query.run.write on service:db/database:%C3%A9t%C3%A9",
        ]
        .iter()
        .map(|text| Grant::parse(text).unwrap())
        .collect();
        expected.sort();
        assert_eq!(store.load_users().unwrap()[0].grants, expected);
        assert_eq!(expected[2].path[0].id, "a/b:c", "the id is the same");
        // The new table takes grants on several levels.
        let mut next = expected.clone();
        next.push(Grant::parse("query.run.read on service:db/host:h/database:x").unwrap());
        next.sort();
        assert!(store.update_user("alice", None, Some(&next)).unwrap());
        assert_eq!(store.load_users().unwrap()[0].grants, next);
    }

    #[test]
    fn a_grant_that_does_not_fit_stops_the_migration_and_the_file_stays_at_version_four() {
        let dir = std::env::temp_dir().join(format!("portal-v5-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        for (row, why) in [
            (
                format!(
                    "(1, 'q', 'service', 'db', 'database', '{}')",
                    "é".repeat(129)
                ),
                "more than 256 bytes",
            ),
            (
                "(1, 'q', 'service', 'db', '1database', 'x')".to_string(),
                "a digit first",
            ),
        ] {
            let path = dir.join("portal.db");
            let _ = std::fs::remove_file(&path);
            version_four(
                &Connection::open(&path).unwrap(),
                &format!(
                    "INSERT INTO user_grants
                       (user_id, action, resource_type, resource_id, sub_resource_type, sub_resource_id)
                     VALUES {row};"
                ),
            );
            let error = match Sqlite::open(&path) {
                Ok(_) => panic!("{why}: the migration must stop"),
                Err(error) => error,
            };
            assert!(error.contains("resource_path"), "{why}: {error}");
            let conn = Connection::open(&path).unwrap();
            let version: i64 = conn
                .query_row("PRAGMA user_version", [], |r| r.get(0))
                .unwrap();
            assert_eq!(version, 4, "{why}: the file did not change");
            let rows: i64 = conn
                .query_row("SELECT COUNT(*) FROM user_grants", [], |r| r.get(0))
                .unwrap();
            assert_eq!(rows, 1, "{why}");
        }
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn the_import_reads_a_file_of_version_four() {
        let dir = std::env::temp_dir().join(format!("portal-v4-dump-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("portal.db");
        version_four(
            &Connection::open(&path).unwrap(),
            "INSERT INTO user_grants
               (user_id, action, resource_type, resource_id, sub_resource_type, sub_resource_id)
             VALUES (1, 'access', 'service', 'db', '', ''),
                    (1, 'query.run.read', 'service', 'db', 'database', 'a b');",
        );
        let dump = Sqlite::open_read_only(&path).unwrap().dump().unwrap();
        assert_eq!(
            dump.users[0].1.grants,
            [
                Grant::parse("access on service:db").unwrap(),
                Grant::parse("query.run.read on service:db/database:a%20b").unwrap()
            ]
        );
        std::fs::remove_dir_all(dir).unwrap();
    }
}
