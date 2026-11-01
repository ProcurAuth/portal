//! The PostgreSQL backend: any number of portal instances share one database.
//!
//! The backend uses a small pool of blocking connections. It uses plain TCP, with no TLS stack. Therefore, there is no C dependency.
//! Callers run these methods on blocking threads, as they do for the SQLite backend.
//! A connection that broke is replaced, and the operation is retried once.
//! This is what a failover of the database looks like from here.
//!
//! The accounts, the history and the sessions live in this database.
//! Every write that two instances could make at once is one statement or one transaction.
//! The instances do not need to talk to each other.

use std::ops::{Deref, DerefMut};
use std::sync::{Condvar, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use postgres::error::SqlState;
use postgres::{Client, NoTls};

use super::*;
use crate::config::PostgresConfig;
use crate::grants::Grant;
use crate::tracking::{unix_now, AuditEvent, Batch, EndReason, Event, LoginAttempt, SessionStart};

mod keys;

/// How long the first connection is retried. A pod can start before its network is ready.
/// The database can also be failing over.
pub const STARTUP_RETRY: Duration = Duration::from_secs(60);
/// How long a caller waits for a free connection of the pool.
const POOL_WAIT: Duration = Duration::from_secs(5);
/// A statement that runs longer is cancelled by the server.
const STATEMENT_TIMEOUT_MS: u64 = 15_000;

/// Advisory locks: one per job that must not run twice at once.
const LOCK_SCHEMA: i64 = 0x7074_6c01;
const LOCK_APPLY: i64 = 0x7074_6c02;

/// One entry per schema version, applied in order and recorded in `schema_migrations`.
const SCHEMA: &[&str] = &[
    "
CREATE TABLE users (
  id BIGSERIAL PRIMARY KEY,
  login TEXT NOT NULL UNIQUE,
  password_hash TEXT NOT NULL,
  created_at BIGINT NOT NULL
);
CREATE TABLE user_roles (
  user_id BIGINT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  role TEXT NOT NULL,
  PRIMARY KEY (user_id, role)
);
CREATE TABLE sessions_log (
  sid BIGINT PRIMARY KEY,
  login TEXT NOT NULL,
  superadmin BOOLEAN NOT NULL,
  impersonated BOOLEAN NOT NULL,
  ip TEXT,
  user_agent TEXT NOT NULL,
  started_at BIGINT NOT NULL,
  last_seen_at BIGINT NOT NULL,
  ended_at BIGINT,
  end_reason TEXT
);
CREATE INDEX sessions_log_login ON sessions_log (login, started_at);
CREATE INDEX sessions_log_started ON sessions_log (started_at);
CREATE TABLE service_access (
  sid BIGINT NOT NULL,
  service_id TEXT NOT NULL,
  first_at BIGINT NOT NULL,
  last_at BIGINT NOT NULL,
  hits BIGINT NOT NULL,
  ip TEXT,
  PRIMARY KEY (sid, service_id)
);
CREATE TABLE audit_events (
  id BIGSERIAL PRIMARY KEY,
  ts BIGINT NOT NULL,
  kind TEXT NOT NULL,
  login TEXT,
  actor TEXT,
  ip TEXT,
  user_agent TEXT NOT NULL
);
CREATE INDEX audit_events_ts ON audit_events (ts);
CREATE TABLE login_attempts (
  id BIGSERIAL PRIMARY KEY,
  ts BIGINT NOT NULL,
  ip TEXT,
  login TEXT NOT NULL,
  known BOOLEAN NOT NULL,
  success BOOLEAN NOT NULL,
  user_agent TEXT NOT NULL
);
CREATE INDEX login_attempts_ts ON login_attempts (ts);
-- The live sessions, shared by every instance. The cookie is never stored, only its SHA-256.
-- `login` is NULL for the superadmin.
CREATE TABLE sessions (
  digest BYTEA PRIMARY KEY,
  binding BYTEA NOT NULL,
  sid BIGINT NOT NULL UNIQUE,
  login TEXT,
  impersonated BOOLEAN NOT NULL,
  created_at BIGINT NOT NULL,
  expires_at BIGINT NOT NULL,
  last_seen_at BIGINT NOT NULL
);
CREATE INDEX sessions_login ON sessions (login);
",
    "
-- Version 2: the grants of the users. The table of the old access model is dropped with its rows:
-- after the migration, a user has the grants that the settings pages give again.
DROP TABLE IF EXISTS user_roles;
CREATE TABLE user_grants (
  user_id BIGINT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  action TEXT NOT NULL,
  resource_type TEXT NOT NULL,
  resource_id TEXT NOT NULL,
  PRIMARY KEY (user_id, action, resource_type, resource_id)
);
",
    "
-- Version 3: a grant can name a sub-resource of the service. The rows of version 2 are grants on
-- the whole service: their two new columns are empty.
ALTER TABLE user_grants
  ADD COLUMN sub_resource_type TEXT NOT NULL DEFAULT '',
  ADD COLUMN sub_resource_id TEXT NOT NULL DEFAULT '';
ALTER TABLE user_grants DROP CONSTRAINT user_grants_pkey;
ALTER TABLE user_grants
  ADD PRIMARY KEY (user_id, action, resource_type, resource_id, sub_resource_type, sub_resource_id);
",
    "
-- Version 4: the credential generation of the superadmin: an HMAC under the session secret of the
-- superadmin login and password. A superadmin or impersonated session is valid only while its row
-- carries the generation of the instance. The rows of version 3 have no generation: they end.
ALTER TABLE sessions ADD COLUMN admin_generation BYTEA;
",
    r#"
-- Version 5: the path of a grant is one column, resource_path: the segments after the service in
-- their text form, for example host:local/database:analytics. It is empty for a grant on the whole
-- service. A grant on one sub-resource (type, id) of version 4 becomes type:<encoded id>. The
-- encoding is the percent-encoding of the grants: each byte outside A-Z a-z 0-9 - . _ ~ becomes
-- %XX with upper-case hex digits. A row that does not fit stops the migration: the transaction
-- rolls back, and the database stays at version 4.
DO $$
BEGIN
  IF EXISTS (
    SELECT 1 FROM user_grants
     WHERE (sub_resource_type <> '' OR sub_resource_id <> '')
       AND (sub_resource_type !~ '^[a-z][a-z0-9_]{0,31}$'
            OR sub_resource_type = 'service'
            OR octet_length(convert_to(sub_resource_id, 'UTF8')) NOT BETWEEN 1 AND 256
            OR sub_resource_id ~ '[[:cntrl:]]')
  ) THEN
    RAISE EXCEPTION 'user_grants: a grant on a sub-resource cannot move to resource_path: the type must be a-z first, then a-z, 0-9 and _, and the id 1 to 256 bytes without a control character';
  END IF;
END
$$;
ALTER TABLE user_grants ADD COLUMN resource_path TEXT NOT NULL DEFAULT '';
UPDATE user_grants AS g
   SET resource_path = g.sub_resource_type || ':' || (
     SELECT string_agg(
              CASE WHEN b BETWEEN 48 AND 57 OR b BETWEEN 65 AND 90 OR b BETWEEN 97 AND 122
                        OR b IN (45, 46, 95, 126)
                   THEN chr(b)
                   ELSE '%' || upper(lpad(to_hex(b), 2, '0'))
              END, '' ORDER BY i)
       FROM (SELECT i, get_byte(convert_to(g.sub_resource_id, 'UTF8'), i) AS b
               FROM generate_series(0, octet_length(convert_to(g.sub_resource_id, 'UTF8')) - 1) AS i
            ) AS bytes)
 WHERE g.sub_resource_type <> '';
ALTER TABLE user_grants DROP CONSTRAINT user_grants_pkey;
ALTER TABLE user_grants DROP COLUMN sub_resource_type, DROP COLUMN sub_resource_id;
ALTER TABLE user_grants
  ADD PRIMARY KEY (user_id, action, resource_type, resource_id, resource_path);
"#,
    "
-- Version 6: the keys of the users, their grants and their counters (portal 4, 5, 7 and 10.7). A
-- key belongs to one user, or to the superadmin: then owner_id is NULL (portal 3.7b). Deleting a
-- user deletes the keys of the user with their grants and their counters. The id of a key is text
-- that the program makes at random. The key table holds the SHA-256 digest of a key, never the
-- key. A revoked key leaves one summary row in key_history. The column limits of a grant holds the
-- limit values as JSON. No index of budget_counters covers units or window_start, so that each
-- update of a counter is a heap-only tuple update. The row of directory_version counts the writes
-- to the users, the grants and the keys.
ALTER TABLE user_grants ADD COLUMN limits TEXT NOT NULL DEFAULT '{}';
CREATE TABLE keys (
  id TEXT PRIMARY KEY,
  owner_id BIGINT REFERENCES users(id) ON DELETE CASCADE,
  name TEXT NOT NULL,
  digest BYTEA NOT NULL UNIQUE,
  hint TEXT NOT NULL,
  created_at BIGINT NOT NULL,
  expires_at BIGINT,
  last_check_at BIGINT,
  check_count BIGINT NOT NULL DEFAULT 0
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
  owner_id BIGINT REFERENCES users(id) ON DELETE CASCADE,
  name TEXT NOT NULL,
  created_at BIGINT NOT NULL,
  revoked_at BIGINT NOT NULL,
  last_check_at BIGINT,
  check_count BIGINT NOT NULL
);
CREATE INDEX key_history_owner ON key_history (owner_id, created_at, id);
CREATE INDEX key_history_created ON key_history (created_at, id);
CREATE TABLE budget_counters (
  id BIGSERIAL PRIMARY KEY,
  user_id BIGINT REFERENCES users(id) ON DELETE CASCADE,
  key_id TEXT REFERENCES keys(id) ON DELETE CASCADE,
  file_subject TEXT,
  service TEXT NOT NULL,
  budget TEXT NOT NULL,
  \"window\" TEXT NOT NULL CHECK (\"window\" IN ('second', 'minute', 'hour', 'day')),
  window_start BIGINT NOT NULL,
  units BIGINT NOT NULL,
  CHECK (num_nonnulls(user_id, key_id, file_subject) = 1)
) WITH (fillfactor = 50);
CREATE UNIQUE INDEX budget_counters_user ON budget_counters (user_id, service, budget, \"window\");
CREATE UNIQUE INDEX budget_counters_key ON budget_counters (key_id, service, budget, \"window\");
CREATE UNIQUE INDEX budget_counters_file ON budget_counters (file_subject, service, budget, \"window\");
CREATE TABLE directory_version (
  id SMALLINT PRIMARY KEY CHECK (id = 1),
  version BIGINT NOT NULL
);
INSERT INTO directory_version (id, version) VALUES (1, 0);
",
];

/// The grants of the rows of `user_grants`: action, resource type, resource id, resource path and
/// limits. A row that is not a valid grant stops the read. The database holds only what the
/// servers wrote.
fn grants_from(rows: Vec<[String; 5]>) -> Result<Vec<Grant>, String> {
    rows.iter()
        .map(|[action, kind, id, path, limits]| grant_from_row(action, kind, id, path, limits))
        .collect()
}

fn insert_grants(
    tx: &mut postgres::Transaction<'_>,
    user_id: i64,
    grants: &[Grant],
) -> Result<(), postgres::Error> {
    for grant in grants {
        let (kind, id, path, limits) = grant_columns(grant);
        tx.execute(
            "INSERT INTO user_grants
               (user_id, action, resource_type, resource_id, resource_path, limits)
             VALUES ($1, $2, $3, $4, $5, $6) ON CONFLICT DO NOTHING",
            &[&user_id, &grant.action, &kind, &id, &path, &limits],
        )?;
    }
    Ok(())
}

/// A row of the read of the users: login, password hash, creation time and one grant, if any.
type UserQueryRow = (String, String, i64, Option<[String; 5]>);

/// What went wrong. The cause is included when the server does not state it.
/// A refused connection reads "error connecting to server: Connection refused" rather than the first half only.
fn db(error: postgres::Error) -> String {
    let mut text = error.to_string();
    // The text of an error of the server is only "db error": add the severity and the message of
    // the server. The detail can hold the values of a row: it stays out of the logs.
    if let Some(server) = error.as_db_error() {
        text.push_str(&format!(": {}: {}", server.severity(), server.message()));
    } else {
        let mut cause = std::error::Error::source(&error);
        while let Some(inner) = cause {
            text.push_str(": ");
            text.push_str(&inner.to_string());
            cause = inner.source();
        }
    }
    text
}

/// A failure that the same operation on a fresh connection may not have.
fn is_transient(error: &postgres::Error) -> bool {
    match error.code() {
        // This is not an answer from the server. The connection broke or timed out.
        None => true,
        Some(code) => {
            *code == SqlState::T_R_DEADLOCK_DETECTED
                || *code == SqlState::T_R_SERIALIZATION_FAILURE
                || *code == SqlState::ADMIN_SHUTDOWN
                || *code == SqlState::CRASH_SHUTDOWN
                || *code == SqlState::CANNOT_CONNECT_NOW
                // The service points to a primary that has just been demoted.
                || *code == SqlState::READ_ONLY_SQL_TRANSACTION
        }
    }
}

struct PoolState {
    idle: Vec<Client>,
    /// The connections that exist. They are idle or lent.
    open: usize,
}

struct Pool {
    config: postgres::Config,
    #[cfg(test)]
    down: std::sync::atomic::AtomicBool,
    max: usize,
    /// How long a caller waits for a free connection.
    wait: Duration,
    state: Mutex<PoolState>,
    freed: Condvar,
}

/// A connection on loan. It goes back to the pool when it is dropped, unless it broke.
struct Conn<'a> {
    pool: &'a Pool,
    client: Option<Client>,
}

impl Deref for Conn<'_> {
    type Target = Client;
    fn deref(&self) -> &Client {
        self.client.as_ref().expect("connection present until drop")
    }
}

impl DerefMut for Conn<'_> {
    fn deref_mut(&mut self) -> &mut Client {
        self.client.as_mut().expect("connection present until drop")
    }
}

impl Conn<'_> {
    /// Does not go back to the pool.
    fn discard(&mut self) {
        if self.client.take().is_some() {
            self.pool.release(None);
        }
    }
}

impl Drop for Conn<'_> {
    fn drop(&mut self) {
        if let Some(client) = self.client.take() {
            self.pool.release(Some(client));
        }
    }
}

impl Pool {
    fn state(&self) -> std::sync::MutexGuard<'_, PoolState> {
        self.state.lock().unwrap_or_else(|p| p.into_inner())
    }

    fn release(&self, client: Option<Client>) {
        let mut state = self.state();
        match client {
            Some(client) if !client.is_closed() => state.idle.push(client),
            _ => state.open -= 1,
        }
        self.freed.notify_one();
    }

    fn get(&self) -> Result<Conn<'_>, String> {
        #[cfg(test)]
        if self.down.load(std::sync::atomic::Ordering::Relaxed) {
            return Err("the database is down (test)".into());
        }
        let give_up = Instant::now() + self.wait;
        let mut state = self.state();
        loop {
            while let Some(client) = state.idle.pop() {
                if !client.is_closed() {
                    return Ok(Conn {
                        pool: self,
                        client: Some(client),
                    });
                }
                state.open -= 1;
            }
            if state.open < self.max {
                state.open += 1;
                drop(state);
                return match self.config.connect(NoTls) {
                    Ok(client) => Ok(Conn {
                        pool: self,
                        client: Some(client),
                    }),
                    Err(e) => {
                        self.release(None);
                        Err(db(e))
                    }
                };
            }
            let left = give_up.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Err("no free database connection".into());
            }
            state = self
                .freed
                .wait_timeout(state, left)
                .unwrap_or_else(|p| p.into_inner())
                .0;
        }
    }
}

pub struct Pg {
    pool: Pool,
    /// The connections of the counter statements (portal 7.7): their own pool, with a short
    /// statement timeout and the asynchronous commit.
    counters: Pool,
}

/// The default number of counter connections (`pdp.counter_connections`, portal 7.7e).
pub const DEFAULT_COUNTER_CONNECTIONS: usize = 2;
/// The longest time of a counter statement and of the wait for a counter connection (portal 7.7).
pub const COUNTER_TIMEOUT: Duration = Duration::from_millis(500);

fn pool(config: postgres::Config, max: usize, wait: Duration) -> Pool {
    Pool {
        config,
        #[cfg(test)]
        down: std::sync::atomic::AtomicBool::new(false),
        max,
        wait,
        state: Mutex::new(PoolState {
            idle: Vec::new(),
            open: 0,
        }),
        freed: Condvar::new(),
    }
}

/// The configuration of a counter connection: the options of the main connections, but a statement
/// timeout of 500 ms and the asynchronous commit (portal 7.7).
fn counter_config(config: &postgres::Config) -> postgres::Config {
    let mut counters = config.clone();
    // The later `-c` of an option wins, so the timeout of the main connections is replaced. The
    // other options, for example the search path of a test, stay.
    counters.options(&format!(
        "{} -c statement_timeout={} -c synchronous_commit=off",
        config.get_options().unwrap_or_default(),
        COUNTER_TIMEOUT.as_millis()
    ));
    counters.application_name("portal-counters");
    counters
}

impl Pg {
    /// Connects and retries for `retry`. The database may not be reachable yet.
    /// Then it brings the schema up to date.
    pub fn connect(cfg: &PostgresConfig, password: &str, retry: Duration) -> Result<Self, String> {
        let mut config = postgres::Config::new();
        config
            .host(&cfg.host)
            .port(cfg.port)
            .dbname(&cfg.database)
            .user(&cfg.user)
            .password(password.as_bytes())
            .connect_timeout(cfg.connect_timeout)
            .keepalives(true)
            .keepalives_idle(Duration::from_secs(20))
            .tcp_user_timeout(Duration::from_secs(20))
            .application_name("portal")
            .options(&format!("-c statement_timeout={STATEMENT_TIMEOUT_MS}"));
        Self::start(config, cfg.pool_size, retry)
    }

    fn start(config: postgres::Config, pool_size: usize, retry: Duration) -> Result<Self, String> {
        let pg = Self {
            counters: pool(
                counter_config(&config),
                DEFAULT_COUNTER_CONNECTIONS,
                COUNTER_TIMEOUT,
            ),
            pool: pool(config, pool_size, POOL_WAIT),
        };
        pg.wait_until_reachable(retry)?;
        pg.migrate()
            .map_err(|e| format!("postgres schema migration: {e}"))?;
        Ok(pg)
    }

    /// Sets the number of counter connections (`pdp.counter_connections`). Call it before the
    /// store is shared.
    pub fn set_counter_connections(&mut self, connections: usize) {
        self.counters = pool(
            counter_config(&self.pool.config),
            connections,
            COUNTER_TIMEOUT,
        );
    }

    fn wait_until_reachable(&self, retry: Duration) -> Result<(), String> {
        let give_up = Instant::now() + retry;
        let mut announced = false;
        loop {
            match self.pool.get() {
                Ok(_) => return Ok(()),
                Err(error) if Instant::now() < give_up => {
                    if !announced {
                        eprintln!(
                            "{{\"ts\":{},\"level\":\"warn\",\"event\":\"postgres_unreachable\",\"retry_for_s\":{},\"error\":\"{}\"}}",
                            unix_now(),
                            retry.as_secs(),
                            crate::util::json_escape(&error)
                        );
                        announced = true;
                    }
                    thread::sleep(Duration::from_secs(1));
                }
                Err(error) => return Err(format!("cannot reach postgres: {error}")),
            }
        }
    }

    /// Runs `operation` on a pooled connection.
    /// If the failure is the kind that a new connection cures, it runs the operation once more on a fresh connection.
    /// The operation is a whole transaction, so it can be repeated.
    fn run<T>(
        &self,
        mut operation: impl FnMut(&mut Client) -> Result<T, postgres::Error>,
    ) -> Result<T, String> {
        let mut retried = false;
        loop {
            let mut conn = self.pool.get()?;
            match operation(&mut conn) {
                Ok(value) => return Ok(value),
                Err(error) => {
                    let transient = is_transient(&error);
                    if transient || conn.is_closed() {
                        conn.discard();
                    }
                    if !transient || retried {
                        return Err(db(error));
                    }
                    retried = true;
                }
            }
        }
    }

    fn migrate(&self) -> Result<(), String> {
        self.run(|c| {
            let mut tx = c.transaction()?;
            tx.execute("SELECT pg_advisory_xact_lock($1)", &[&LOCK_SCHEMA])?;
            tx.batch_execute(
                "CREATE TABLE IF NOT EXISTS schema_migrations (version BIGINT PRIMARY KEY, applied_at BIGINT NOT NULL)",
            )?;
            let current: i64 = tx
                .query_one(
                    "SELECT COALESCE(MAX(version), 0)::bigint FROM schema_migrations",
                    &[],
                )?
                .get(0);
            if current as usize > SCHEMA.len() {
                // This is not a database failure. The caller reports it as it is.
                return Ok(Err(format!(
                    "the database schema is version {current}, this portal understands {}",
                    SCHEMA.len()
                )));
            }
            for (index, sql) in SCHEMA.iter().enumerate().skip(current as usize) {
                tx.batch_execute(sql)?;
                tx.execute(
                    "INSERT INTO schema_migrations (version, applied_at) VALUES ($1, $2)",
                    &[&((index + 1) as i64), &unix_now()],
                )?;
            }
            tx.commit()?;
            Ok(Ok(()))
        })?
    }

    // Accounts --------------------------------------------------------------------------------

    #[cfg(test)]
    pub fn load_users(&self) -> Result<Vec<UserRow>, String> {
        let rows = self.run(Self::users_query)?;
        Self::assemble_users(rows)
    }

    /// One row for each grant, and one row for a user without a grant. The rows of a user follow
    /// each other.
    fn users_query<C: postgres::GenericClient>(
        c: &mut C,
    ) -> Result<Vec<UserQueryRow>, postgres::Error> {
        Ok(c.query(
            "SELECT u.login, u.password_hash, u.created_at, g.action, g.resource_type,
                    g.resource_id, g.resource_path, g.limits
               FROM users u LEFT JOIN user_grants g ON g.user_id = u.id
              ORDER BY u.login COLLATE \"C\", g.action COLLATE \"C\",
                       g.resource_id COLLATE \"C\", g.resource_path COLLATE \"C\"",
            &[],
        )?
        .into_iter()
        .map(|row| {
            let grant = match (
                row.get::<_, Option<String>>(3),
                row.get::<_, Option<String>>(4),
                row.get::<_, Option<String>>(5),
                row.get::<_, Option<String>>(6),
                row.get::<_, Option<String>>(7),
            ) {
                (Some(a), Some(k), Some(i), Some(p), Some(l)) => Some([a, k, i, p, l]),
                _ => None,
            };
            (
                row.get::<_, String>(0),
                row.get::<_, String>(1),
                row.get::<_, i64>(2),
                grant,
            )
        })
        .collect::<Vec<_>>())
    }

    fn assemble_users(rows: Vec<UserQueryRow>) -> Result<Vec<UserRow>, String> {
        let mut users: Vec<UserRow> = Vec::new();
        for (login, password_hash, created_at, grant) in rows {
            if users.last().is_none_or(|last| last.login != login) {
                users.push(UserRow {
                    login,
                    password_hash,
                    created_at,
                    grants: Vec::new(),
                });
            }
            if let (Some(user), Some(grant)) = (users.last_mut(), grant) {
                user.grants.extend(grants_from(vec![grant])?);
            }
        }
        // The order of the text of a path is not the order of the grants.
        for user in &mut users {
            user.grants.sort();
        }
        Ok(users)
    }

    pub fn insert_user(&self, user: &UserRow) -> Result<(), InsertError> {
        let inserted = self
            .run(|c| {
                let mut tx = c.transaction()?;
                let Some(row) = tx.query_opt(
                    "INSERT INTO users (login, password_hash, created_at) VALUES ($1, $2, $3)
                     ON CONFLICT (login) DO NOTHING RETURNING id",
                    &[&user.login, &user.password_hash, &user.created_at],
                )?
                else {
                    return Ok(false);
                };
                let id: i64 = row.get(0);
                insert_grants(&mut tx, id, &user.grants)?;
                keys::insert_counter_rows(&mut tx, keys::Subject::User(id), &user.grants)?;
                keys::bump(&mut tx)?;
                tx.commit()?;
                Ok(true)
            })
            .map_err(InsertError::Db)?;
        if inserted {
            Ok(())
        } else {
            Err(InsertError::Exists)
        }
    }

    pub fn update_user(
        &self,
        login: &str,
        password_hash: Option<&str>,
        grants: Option<&[Grant]>,
    ) -> Result<bool, String> {
        self.run(|c| {
            let mut tx = c.transaction()?;
            let Some(row) = tx.query_opt(
                "SELECT id FROM users WHERE login = $1 FOR UPDATE",
                &[&login],
            )?
            else {
                return Ok(false);
            };
            let id: i64 = row.get(0);
            if let Some(hash) = password_hash {
                tx.execute(
                    "UPDATE users SET password_hash = $2 WHERE id = $1",
                    &[&id, &hash],
                )?;
            }
            if let Some(grants) = grants {
                tx.execute("DELETE FROM user_grants WHERE user_id = $1", &[&id])?;
                insert_grants(&mut tx, id, grants)?;
                keys::insert_counter_rows(&mut tx, keys::Subject::User(id), grants)?;
            }
            keys::bump(&mut tx)?;
            tx.commit()?;
            Ok(true)
        })
    }

    pub fn remove_user(&self, login: &str) -> Result<bool, String> {
        self.run(|c| {
            let mut tx = c.transaction()?;
            keys::bump(&mut tx)?;
            let removed = tx.execute("DELETE FROM users WHERE login = $1", &[&login])? > 0;
            if removed {
                tx.commit()?;
            }
            Ok(removed)
        })
    }

    // History: writes -------------------------------------------------------------------------

    pub fn apply(&self, batch: &Batch) -> Result<(), String> {
        if batch.events.is_empty() && batch.hits.is_empty() && batch.seen.is_empty() {
            return Ok(());
        }
        // Every instance writes the same rows in the same order. Therefore, two flushes never deadlock.
        let mut hits: Vec<_> = batch.hits.iter().collect();
        hits.sort_by(|a, b| a.0.cmp(b.0));
        let mut seen: Vec<_> = batch.seen.iter().collect();
        seen.sort_by_key(|(sid, _)| **sid);
        self.run(|c| {
            let mut tx = c.transaction()?;
            tx.execute("SELECT pg_advisory_xact_lock($1)", &[&LOCK_APPLY])?;
            let mut logged_attempts = false;
            for event in &batch.events {
                match event {
                    Event::SessionStarted(start) => {
                        tx.execute(
                            "INSERT INTO sessions_log
                               (sid, login, superadmin, impersonated, ip, user_agent, started_at, last_seen_at)
                             VALUES ($1, $2, $3, $4, $5, $6, $7, $7)
                             ON CONFLICT (sid) DO NOTHING",
                            &[
                                &(start.sid as i64),
                                &start.login,
                                &start.superadmin,
                                &start.impersonated,
                                &start.ip,
                                &start.user_agent,
                                &start.at,
                            ],
                        )?;
                    }
                    Event::SessionEnded { sid, reason, at } => {
                        tx.execute(
                            "UPDATE sessions_log
                                SET ended_at = $2, end_reason = $3, last_seen_at = GREATEST(last_seen_at, $2)
                              WHERE sid = $1 AND ended_at IS NULL",
                            &[&(*sid as i64), at, &reason.as_str()],
                        )?;
                    }
                    Event::Audit(audit) => insert_audit(&mut tx, audit)?,
                    Event::Login(attempt) => {
                        insert_login(&mut tx, attempt)?;
                        logged_attempts = true;
                    }
                }
            }
            if logged_attempts {
                tx.execute(
                    "DELETE FROM login_attempts WHERE id <= (SELECT MAX(id) FROM login_attempts) - $1",
                    &[&MAX_LOGIN_ROWS],
                )?;
            }
            for ((sid, service), hit) in &hits {
                tx.execute(
                    "INSERT INTO service_access (sid, service_id, first_at, last_at, hits, ip)
                     VALUES ($1, $2, $3, $4, $5, $6)
                     ON CONFLICT (sid, service_id) DO UPDATE SET
                       last_at = GREATEST(service_access.last_at, EXCLUDED.last_at),
                       hits = service_access.hits + EXCLUDED.hits,
                       ip = COALESCE(EXCLUDED.ip, service_access.ip)",
                    &[
                        &(*sid as i64),
                        service,
                        &hit.first_at,
                        &hit.last_at,
                        &(hit.hits as i64),
                        &hit.ip,
                    ],
                )?;
            }
            for (sid, at) in &seen {
                tx.execute(
                    "UPDATE sessions_log SET last_seen_at = GREATEST(last_seen_at, $2)
                      WHERE sid = $1 AND ended_at IS NULL",
                    &[&(**sid as i64), *at],
                )?;
            }
            tx.commit()
        })
    }

    /// Closes the history rows of sessions that are no longer live. Nobody recorded the end of these sessions.
    /// This happens when an instance stops before it flushes its records. Such rows end at the last time they were seen.
    /// Only rows not seen since `before` are checked. Therefore, an end that still waits for its flush is not taken for a lost one.
    pub fn close_lost_sessions(&self, before: i64, reason: &str) -> Result<usize, String> {
        self.run(|c| {
            Ok(c.execute(
                "UPDATE sessions_log SET ended_at = last_seen_at, end_reason = $2
                  WHERE ended_at IS NULL AND last_seen_at < $1
                    AND sid NOT IN (SELECT sid FROM sessions)",
                &[&before, &reason],
            )? as usize)
        })
    }

    pub fn prune(&self, cutoff: i64) -> Result<(), String> {
        self.run(|c| {
            let mut tx = c.transaction()?;
            tx.execute("SELECT pg_advisory_xact_lock($1)", &[&LOCK_APPLY])?;
            tx.execute("DELETE FROM sessions_log WHERE started_at < $1", &[&cutoff])?;
            tx.execute(
                "DELETE FROM service_access WHERE sid NOT IN (SELECT sid FROM sessions_log)",
                &[],
            )?;
            tx.execute("DELETE FROM audit_events WHERE ts < $1", &[&cutoff])?;
            tx.execute("DELETE FROM login_attempts WHERE ts < $1", &[&cutoff])?;
            tx.commit()
        })
    }

    // History: reads --------------------------------------------------------------------------

    pub fn login_attempts(
        &self,
        now: i64,
        page: u32,
        only_failed: bool,
    ) -> Result<LoginPage, String> {
        self.run(|c| {
            let matching: i64 = c
                .query_one(
                    "SELECT COUNT(*) FROM login_attempts WHERE (NOT $1::boolean OR NOT success)",
                    &[&only_failed],
                )?
                .get(0);
            let mut result = LoginPage {
                matching: matching as u32,
                ..LoginPage::default()
            };
            result.page = page.min(result.pages() - 1);
            result.rows = c
                .query(
                    "SELECT ts, login, known, success, ip, user_agent FROM login_attempts
                      WHERE (NOT $1::boolean OR NOT success)
                      ORDER BY id DESC LIMIT $2 OFFSET $3",
                    &[
                        &only_failed,
                        &LOGIN_PAGE_SIZE,
                        &(i64::from(result.page) * LOGIN_PAGE_SIZE),
                    ],
                )?
                .into_iter()
                .map(|row| LoginRow {
                    ts: row.get(0),
                    login: row.get(1),
                    known: row.get(2),
                    success: row.get(3),
                    ip: row.get(4),
                    user_agent: row.get(5),
                })
                .collect();
            let figures = c.query_one(
                "SELECT COUNT(*), COUNT(*) FILTER (WHERE NOT success),
                        COUNT(DISTINCT CASE WHEN NOT success THEN ip END)
                   FROM login_attempts WHERE ts >= $1",
                &[&(now - DAY)],
            )?;
            result.attempts_24h = figures.get::<_, i64>(0) as u32;
            result.failed_24h = figures.get::<_, i64>(1) as u32;
            result.addresses_24h = figures.get::<_, i64>(2) as u32;
            Ok(result)
        })
    }

    pub fn snapshot(&self, accounts: &[(String, bool)], now: i64) -> Result<Snapshot, String> {
        self.run(|c| {
            let first_day = now - now.rem_euclid(DAY) - (GRAPH_DAYS as i64 - 1) * DAY;
            let mut snapshot = Snapshot {
                days: day_stats(c, first_day, None)?,
                first_day,
                ..Snapshot::default()
            };
            for (login, superadmin) in accounts {
                let days = day_stats(c, first_day, Some((login, *superadmin)))?;
                let recent = c
                    .query(
                        "SELECT started_at, last_seen_at, ended_at, end_reason, ip, user_agent, impersonated
                           FROM sessions_log WHERE login = $1 AND superadmin = $2
                          ORDER BY started_at DESC, sid DESC LIMIT $3",
                        &[login, superadmin, &RECENT_SESSIONS],
                    )?
                    .into_iter()
                    .map(|row| SessionRow {
                        started_at: row.get(0),
                        last_seen_at: row.get(1),
                        ended_at: row.get(2),
                        end_reason: row.get(3),
                        ip: row.get(4),
                        user_agent: row.get(5),
                        impersonated: row.get(6),
                    })
                    .collect();
                let services = c
                    .query(
                        "SELECT a.service_id, SUM(a.hits)::bigint, MAX(a.last_at)
                           FROM service_access a JOIN sessions_log s ON s.sid = a.sid
                          WHERE s.login = $1 AND s.superadmin = $2 AND NOT s.impersonated
                          GROUP BY a.service_id ORDER BY MAX(a.last_at) DESC LIMIT $3",
                        &[login, superadmin, &TOP_SERVICES],
                    )?
                    .into_iter()
                    .map(|row| ServiceUse {
                        service_id: row.get(0),
                        hits: row.get::<_, i64>(1) as u64,
                        last_at: row.get(2),
                    })
                    .collect();
                let failed_30d: i64 = c
                    .query_one(
                        "SELECT COUNT(*) FROM audit_events WHERE kind = 'login_failed' AND login = $1 AND ts >= $2",
                        &[login, &first_day],
                    )?
                    .get(0);
                let last_login: Option<i64> = c
                    .query_one(
                        "SELECT MAX(started_at) FROM sessions_log
                          WHERE login = $1 AND superadmin = $2 AND NOT impersonated",
                        &[login, superadmin],
                    )?
                    .get(0);
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
            snapshot.recent_events = c
                .query(
                    "SELECT ts, kind, login, actor, ip FROM audit_events ORDER BY ts DESC, id DESC LIMIT $1",
                    &[&RECENT_EVENTS],
                )?
                .into_iter()
                .map(|row| AuditRow {
                    ts: row.get(0),
                    kind: row.get(1),
                    login: row.get(2),
                    actor: row.get(3),
                    ip: row.get(4),
                })
                .collect();
            let unknown: i64 = c
                .query_one(
                    "SELECT COUNT(*) FROM audit_events WHERE kind = 'login_failed' AND login IS NULL AND ts >= $1",
                    &[&first_day],
                )?
                .get(0);
            snapshot.unknown_failures_30d = unknown as u32;
            Ok(snapshot)
        })
    }

    // Shared sessions -------------------------------------------------------------------------

    /// Records a new session and its line of the history in the same transaction.
    /// Therefore, no other instance can see a session whose history row does not exist yet.
    /// Returns `false` when `max_active` sessions are already live.
    /// The count is read without a lock. Two instances that race at the limit can overshoot it by one each.
    pub fn session_insert(
        &self,
        digest: &[u8; 32],
        binding: &[u8; 32],
        session: &SharedSession,
        start: &SessionStart,
        max_active: usize,
        generation: &[u8; 32],
    ) -> Result<bool, String> {
        self.run(|c| {
            let mut tx = c.transaction()?;
            let inserted = tx.execute(
                "INSERT INTO sessions
                   (digest, binding, sid, login, impersonated, created_at, expires_at, last_seen_at,
                    admin_generation)
                 SELECT $1::bytea, $2::bytea, $3::bigint, $4::text, $5::boolean, $6::bigint, $7::bigint, $6::bigint,
                        $9::bytea
                  WHERE (SELECT COUNT(*) FROM sessions WHERE expires_at > $6) < $8",
                &[
                    &digest.as_slice(),
                    &binding.as_slice(),
                    &(session.sid as i64),
                    &session.login,
                    &session.impersonated,
                    &start.at,
                    &session.expires_at,
                    &(max_active as i64),
                    &generation.as_slice(),
                ],
            )?;
            if inserted == 0 {
                return Ok(false);
            }
            tx.execute(
                "INSERT INTO sessions_log
                   (sid, login, superadmin, impersonated, ip, user_agent, started_at, last_seen_at)
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $7)
                 ON CONFLICT (sid) DO NOTHING",
                &[
                    &(start.sid as i64),
                    &start.login,
                    &start.superadmin,
                    &start.impersonated,
                    &start.ip,
                    &start.user_agent,
                    &start.at,
                ],
            )?;
            tx.commit()?;
            Ok(true)
        })
    }

    /// Returns the live session of `digest` that was issued for `binding`. Its idle timer is renewed to `now`.
    /// Returns `None` when there is no such session, when it ended, or when the binding differs. In the last case, the session is not changed.
    /// A session of the superadmin, or an impersonated session, must also carry `generation`.
    /// A session of a database user does not depend on the superadmin credentials.
    pub fn session_touch(
        &self,
        digest: &[u8; 32],
        binding: &[u8; 32],
        now: i64,
        idle_secs: i64,
        generation: &[u8; 32],
    ) -> Result<Option<SharedSession>, String> {
        self.run(|c| {
            Ok(c.query_opt(
                "UPDATE sessions SET last_seen_at = $3
                  WHERE digest = $1 AND binding = $2 AND expires_at > $3 AND last_seen_at > $3 - $4
                    AND ((login IS NOT NULL AND NOT impersonated) OR admin_generation = $5)
              RETURNING sid, login, impersonated, expires_at",
                &[
                    &digest.as_slice(),
                    &binding.as_slice(),
                    &now,
                    &idle_secs,
                    &generation.as_slice(),
                ],
            )?
            .map(shared_session))
        })
    }

    /// Ends the superadmin and impersonated sessions whose generation is not `generation`.
    /// Returns the identifiers that this call ended.
    pub fn sessions_end_other_generations(
        &self,
        generation: &[u8; 32],
    ) -> Result<Vec<u64>, String> {
        self.run(|c| {
            Ok(c.query(
                "DELETE FROM sessions
                  WHERE (login IS NULL OR impersonated)
                    AND admin_generation IS DISTINCT FROM $1
              RETURNING sid",
                &[&generation.as_slice()],
            )?
            .into_iter()
            .map(|row| row.get::<_, i64>(0) as u64)
            .collect())
        })
    }

    /// Ends the session of `digest`. Returns the session only when this call is the one that ended it.
    pub fn session_delete(&self, digest: &[u8; 32]) -> Result<Option<SharedSession>, String> {
        self.run(|c| {
            Ok(c.query_opt(
                "DELETE FROM sessions WHERE digest = $1
                 RETURNING sid, login, impersonated, expires_at",
                &[&digest.as_slice()],
            )?
            .map(shared_session))
        })
    }

    /// Ends every session of one user. Returns the identifiers that this call ended.
    pub fn sessions_delete_user(&self, login: &str) -> Result<Vec<u64>, String> {
        self.run(|c| {
            Ok(c.query(
                "DELETE FROM sessions WHERE login = $1 RETURNING sid",
                &[&login],
            )?
            .into_iter()
            .map(|row| row.get::<_, i64>(0) as u64)
            .collect())
        })
    }

    /// Ends every session, whoever owns it (the superadmin too). Returns the identifiers that this call ended.
    pub fn sessions_delete_all(&self) -> Result<Vec<u64>, String> {
        self.run(|c| {
            Ok(c.query("DELETE FROM sessions RETURNING sid", &[])?
                .into_iter()
                .map(|row| row.get::<_, i64>(0) as u64)
                .collect())
        })
    }

    /// Ends the sessions past their lifetime or unused for `idle_secs`. Each result states whether the session expired, or whether it idled out.
    /// Exactly one caller gets any given session back.
    pub fn sessions_expire(&self, now: i64, idle_secs: i64) -> Result<Vec<(u64, bool)>, String> {
        self.run(|c| {
            Ok(c.query(
                "DELETE FROM sessions WHERE expires_at <= $1 OR last_seen_at <= $1 - $2
             RETURNING sid, expires_at <= $1",
                &[&now, &idle_secs],
            )?
            .into_iter()
            .map(|row| (row.get::<_, i64>(0) as u64, row.get::<_, bool>(1)))
            .collect())
        })
    }

    pub fn sessions_count(&self) -> Result<usize, String> {
        self.run(|c| {
            Ok(c.query_one("SELECT COUNT(*) FROM sessions", &[])?
                .get::<_, i64>(0) as usize)
        })
    }

    // Import ----------------------------------------------------------------------------------

    /// Copies a SQLite dump into this database, in one transaction. The database must be empty.
    /// The sessions of the dump that were still open end where they were last seen.
    /// Sessions do not survive the move. They were in the memory of the old process.
    pub fn import(&self, dump: &Dump) -> Result<ImportReport, String> {
        self.run(|c| {
            let mut tx = c.transaction()?;
            tx.execute("SELECT pg_advisory_xact_lock($1)", &[&LOCK_APPLY])?;
            tx.batch_execute(
                "LOCK TABLE users, user_grants, sessions_log, service_access, audit_events, login_attempts
                   IN ACCESS EXCLUSIVE MODE",
            )?;
            for table in IMPORTED_TABLES {
                let rows: i64 = tx
                    .query_one(&format!("SELECT COUNT(*) FROM {table}"), &[])?
                    .get(0);
                if rows > 0 {
                    return Ok(Err(format!(
                        "the target database is not empty ({table} has {rows} rows): nothing was imported"
                    )));
                }
            }
            let user = tx.prepare("INSERT INTO users (id, login, password_hash, created_at) VALUES ($1, $2, $3, $4)")?;
            for (id, row) in &dump.users {
                tx.execute(&user, &[id, &row.login, &row.password_hash, &row.created_at])?;
                insert_grants(&mut tx, *id, &row.grants)?;
            }
            // The keys keep their id and their digest. The rows of the counters are not copied: each
            // instance makes the rows that the grants need at its start.
            for key in &dump.keys {
                let owner_id: Option<i64> = match &key.owner {
                    None => None,
                    Some(login) => dump
                        .users
                        .iter()
                        .find(|(_, user)| user.login == *login)
                        .map(|(id, _)| *id),
                };
                tx.execute(
                    "INSERT INTO keys (id, owner_id, name, digest, hint, created_at, expires_at,
                                       last_check_at, check_count)
                     VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)",
                    &[
                        &key.id,
                        &owner_id,
                        &key.name,
                        &key.digest.as_slice(),
                        &key.hint,
                        &key.created_at,
                        &key.expires_at,
                        &key.last_check_at,
                        &key.check_count,
                    ],
                )?;
                keys::insert_key_grants(&mut tx, &key.id, &key.grants)?;
            }
            for row in &dump.key_history {
                let owner_id: Option<i64> = match &row.owner {
                    None => None,
                    Some(login) => dump
                        .users
                        .iter()
                        .find(|(_, user)| user.login == *login)
                        .map(|(id, _)| *id),
                };
                tx.execute(
                    "INSERT INTO key_history
                       (id, owner_id, name, created_at, revoked_at, last_check_at, check_count)
                     VALUES ($1, $2, $3, $4, $5, $6, $7)",
                    &[
                        &row.id,
                        &owner_id,
                        &row.name,
                        &row.created_at,
                        &row.revoked_at,
                        &row.last_check_at,
                        &row.check_count,
                    ],
                )?;
            }
            // The directory changed: the instances read it again (portal 10.7).
            keys::bump(&mut tx)?;
            let session = tx.prepare(
                "INSERT INTO sessions_log
                   (sid, login, superadmin, impersonated, ip, user_agent, started_at, last_seen_at, ended_at, end_reason)
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)",
            )?;
            for row in &dump.sessions {
                let open = row.ended_at.is_none();
                let ended_at = row.ended_at.or(open.then_some(row.last_seen_at));
                let end_reason = match (&row.end_reason, open) {
                    (reason, false) => reason.clone(),
                    (_, true) => Some(EndReason::Restart.as_str().to_string()),
                };
                tx.execute(
                    &session,
                    &[
                        &row.sid,
                        &row.login,
                        &row.superadmin,
                        &row.impersonated,
                        &row.ip,
                        &row.user_agent,
                        &row.started_at,
                        &row.last_seen_at,
                        &ended_at,
                        &end_reason,
                    ],
                )?;
            }
            let access = tx.prepare(
                "INSERT INTO service_access (sid, service_id, first_at, last_at, hits, ip)
                 VALUES ($1, $2, $3, $4, $5, $6)",
            )?;
            for row in &dump.accesses {
                tx.execute(
                    &access,
                    &[&row.sid, &row.service_id, &row.first_at, &row.last_at, &row.hits, &row.ip],
                )?;
            }
            let audit = tx.prepare(
                "INSERT INTO audit_events (id, ts, kind, login, actor, ip, user_agent)
                 VALUES ($1, $2, $3, $4, $5, $6, $7)",
            )?;
            for row in &dump.audit {
                tx.execute(
                    &audit,
                    &[&row.id, &row.ts, &row.kind, &row.login, &row.actor, &row.ip, &row.user_agent],
                )?;
            }
            let login = tx.prepare(
                "INSERT INTO login_attempts (id, ts, ip, login, known, success, user_agent)
                 VALUES ($1, $2, $3, $4, $5, $6, $7)",
            )?;
            for row in &dump.logins {
                tx.execute(
                    &login,
                    &[&row.id, &row.ts, &row.ip, &row.login, &row.known, &row.success, &row.user_agent],
                )?;
            }
            // The rows keep their identifiers. The next rows continue after them.
            for (table, column) in [
                ("users", "id"),
                ("audit_events", "id"),
                ("login_attempts", "id"),
            ] {
                tx.execute(
                    &format!(
                        "SELECT setval(pg_get_serial_sequence('{table}', '{column}'),
                                       GREATEST((SELECT COALESCE(MAX({column}), 0) FROM {table}), 1),
                                       (SELECT COUNT(*) > 0 FROM {table}))"
                    ),
                    &[],
                )?;
            }
            tx.commit()?;
            Ok(Ok(ImportReport {
                users: dump.users.len(),
                sessions: dump.sessions.len(),
                accesses: dump.accesses.len(),
                audit: dump.audit.len(),
                logins: dump.logins.len(),
                keys: dump.keys.len(),
                key_history: dump.key_history.len(),
            }))
        })?
    }

    /// A store in a schema of its own, in the database that `url` names (`postgres://user:password@host/db`).
    #[cfg(test)]
    pub fn open_isolated(url: &str) -> Self {
        use std::sync::atomic::{AtomicU32, Ordering};
        static NEXT: AtomicU32 = AtomicU32::new(0);
        let base: postgres::Config = url.parse().expect("a PostgreSQL URL");
        // The process number repeats from one container to the next. Therefore, the clock tells runs apart.
        let schema = format!(
            "portal_test_{}_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0),
            NEXT.fetch_add(1, Ordering::Relaxed)
        );
        base.connect(NoTls)
            .expect("the test database answers")
            .batch_execute(&format!("CREATE SCHEMA {schema}"))
            .unwrap();
        let mut config = base;
        config.application_name(&schema).options(&format!(
            "-c statement_timeout={STATEMENT_TIMEOUT_MS} -c search_path={schema}"
        ));
        Self::start(config, 4, Duration::from_secs(5)).unwrap()
    }

    /// Another instance of the portal, on the same database. It has its own pool and uses the same schema.
    #[cfg(test)]
    pub fn sibling(&self) -> Self {
        Self::start(
            self.pool.config.clone(),
            self.pool.max,
            Duration::from_secs(5),
        )
        .unwrap()
    }

    /// Makes every new connection fail, as a database that went away does. The pooled connections are dropped.
    #[cfg(test)]
    pub fn go_down(&self) {
        self.pool
            .down
            .store(true, std::sync::atomic::Ordering::Relaxed);
        let mut state = self.pool.state();
        state.open -= state.idle.len();
        state.idle.clear();
    }

    #[cfg(test)]
    pub fn execute_raw(&self, sql: &str) {
        self.run(|c| c.batch_execute(sql)).unwrap();
    }

    #[cfg(test)]
    pub fn count(&self, table: &str) -> i64 {
        self.run(|c| {
            Ok(c.query_one(&format!("SELECT COUNT(*) FROM {table}"), &[])?
                .get(0))
        })
        .unwrap()
    }
}

/// The tables that the import fills. All of them must be empty.
const IMPORTED_TABLES: [&str; 7] = [
    "keys",
    "key_history",
    "users",
    "sessions_log",
    "service_access",
    "audit_events",
    "login_attempts",
];

fn shared_session(row: postgres::Row) -> SharedSession {
    SharedSession {
        sid: row.get::<_, i64>(0) as u64,
        login: row.get(1),
        impersonated: row.get(2),
        expires_at: row.get(3),
    }
}

fn insert_audit(
    tx: &mut postgres::Transaction<'_>,
    audit: &AuditEvent,
) -> Result<(), postgres::Error> {
    tx.execute(
        "INSERT INTO audit_events (ts, kind, login, actor, ip, user_agent) VALUES ($1, $2, $3, $4, $5, $6)",
        &[
            &audit.at,
            &audit.kind.as_str(),
            &audit.login,
            &audit.actor,
            &audit.ip,
            &audit.user_agent,
        ],
    )?;
    Ok(())
}

fn insert_login(
    tx: &mut postgres::Transaction<'_>,
    attempt: &LoginAttempt,
) -> Result<(), postgres::Error> {
    tx.execute(
        "INSERT INTO login_attempts (ts, ip, login, known, success, user_agent) VALUES ($1, $2, $3, $4, $5, $6)",
        &[
            &attempt.at,
            &attempt.ip,
            &attempt.login,
            &attempt.known,
            &attempt.success,
            &attempt.user_agent,
        ],
    )?;
    Ok(())
}

/// Real connections per day, as on the SQLite side. See there.
fn day_stats(
    client: &mut Client,
    first_day: i64,
    account: Option<(&str, bool)>,
) -> Result<Vec<DayStat>, postgres::Error> {
    let mut days = vec![DayStat::default(); GRAPH_DAYS];
    let (login, superadmin) = match account {
        Some((login, superadmin)) => (Some(login), superadmin),
        None => (None, false),
    };
    let rows = client.query(
        "SELECT (started_at - $1) / 86400,
                COUNT(*),
                COALESCE(SUM(GREATEST(COALESCE(ended_at, last_seen_at) - started_at, 0)), 0)::bigint
           FROM sessions_log
          WHERE started_at >= $1 AND NOT impersonated
            AND ($2::text IS NULL OR (login = $2 AND superadmin = $3))
          GROUP BY 1",
        &[&first_day, &login, &superadmin],
    )?;
    for row in rows {
        let (day, sessions, seconds): (i64, i64, i64) = (row.get(0), row.get(1), row.get(2));
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

    #[test]
    fn an_unreachable_database_is_retried_for_the_given_time_then_reported() {
        let mut config = postgres::Config::new();
        config
            .host("127.0.0.1")
            .port(1)
            .user("portal")
            .connect_timeout(Duration::from_secs(1));
        let begun = Instant::now();
        let error = match Pg::start(config, 2, Duration::from_secs(2)) {
            Ok(_) => panic!("nothing listens on port 1"),
            Err(error) => error,
        };
        assert!(error.contains("cannot reach postgres"), "{error}");
        assert!(
            begun.elapsed() >= Duration::from_secs(2),
            "it kept trying for the whole delay"
        );
    }

    #[test]
    fn a_connection_that_never_answered_is_transient() {
        let connection_lost = postgres::Config::new()
            .host("127.0.0.1")
            .port(1)
            .connect(NoTls)
            .err()
            .unwrap();
        assert!(
            is_transient(&connection_lost),
            "no answer from the server at all"
        );
    }

    #[test]
    fn a_connection_the_server_closed_is_replaced_and_the_call_goes_through() {
        use crate::store::testing::{postgres, Backend};
        let Some(Backend::Postgres(url)) = postgres() else {
            return;
        };
        let Store::Pg(pg) = Backend::Postgres(url.clone()).fresh() else {
            unreachable!()
        };
        pg.insert_user(&UserRow {
            login: "alice".into(),
            password_hash: "h".into(),
            grants: vec![],
            created_at: 1,
        })
        .unwrap();
        // This is what a failover does to the connections of the pool. The server ends them.
        let application = pg.pool.config.get_application_name().unwrap().to_string();
        let mut admin = url
            .parse::<postgres::Config>()
            .unwrap()
            .connect(NoTls)
            .unwrap();
        let killed = admin
            .query(
                "SELECT pg_terminate_backend(pid) FROM pg_stat_activity WHERE application_name = $1",
                &[&application],
            )
            .unwrap();
        assert!(!killed.is_empty(), "there were connections to end");
        assert_eq!(
            pg.load_users().unwrap().len(),
            1,
            "a new connection took over"
        );
        assert_eq!(pg.load_users().unwrap().len(), 1);
    }

    #[test]
    fn the_migration_to_version_six_adds_the_tables_of_the_keys_and_keeps_the_rows() {
        use crate::store::testing::{postgres, Backend};
        let Some(Backend::Postgres(url)) = postgres() else {
            return;
        };
        let Store::Pg(fresh) = Backend::Postgres(url).fresh() else {
            unreachable!()
        };
        assert_eq!(SCHEMA.len(), 6);
        let config = fresh.pool.config.clone();
        let mut admin = config.connect(NoTls).unwrap();
        // This database was made by the portal before version 6: the tables and the column did
        // not exist.
        admin
            .batch_execute(
                "DROP TABLE budget_counters; DROP TABLE key_history; DROP TABLE key_grants;
                 DROP TABLE keys; DROP TABLE directory_version;
                 ALTER TABLE user_grants DROP COLUMN limits;
                 DELETE FROM schema_migrations WHERE version > 5;
                 INSERT INTO users (id, login, password_hash, created_at) VALUES (1, 'alice', 'h', 1);",
            )
            .unwrap();
        drop(fresh);
        let migrated = Pg::start(config, 2, Duration::from_secs(5)).unwrap();
        let version: i64 = admin
            .query_one("SELECT MAX(version) FROM schema_migrations", &[])
            .unwrap()
            .get(0);
        assert_eq!(version as usize, SCHEMA.len());
        assert_eq!(migrated.load_users().unwrap().len(), 1, "the account stays");
        let directory: i64 = admin
            .query_one("SELECT version FROM directory_version", &[])
            .unwrap()
            .get(0);
        assert_eq!(directory, 0);
        admin
            .batch_execute(
                "INSERT INTO keys (id, owner_id, name, digest, hint, created_at, expires_at)
                   VALUES ('key1', 1, 'agent', '\\x00ff', 'ab12', 1, 2);
                 INSERT INTO key_grants (key_id, action, resource_type, resource_id, limits)
                   VALUES ('key1', 'access', 'service', 'db',
                           '{\"requests\":[{\"amount\":10,\"window\":\"minute\"}]}');
                 INSERT INTO budget_counters (key_id, service, budget, \"window\", window_start, units)
                   VALUES ('key1', 'db', 'requests', 'minute', 0, 0);
                 INSERT INTO key_history (id, owner_id, name, created_at, revoked_at, check_count)
                   VALUES ('key0', 1, 'old', 1, 2, 0);",
            )
            .unwrap();
        let twice = admin.batch_execute(
            "INSERT INTO keys (id, owner_id, name, digest, hint, created_at)
             VALUES ('key2', NULL, 'x', '\\x00ff', 'cd34', 1)",
        );
        assert!(twice.is_err(), "the same digest cannot name two keys");
        // A key of the superadmin has no owner row (portal 3.7b).
        admin
            .batch_execute(
                "INSERT INTO keys (id, owner_id, name, digest, hint, created_at)
                 VALUES ('key4', NULL, 'root key', '\\x02', 'ef56', 1)",
            )
            .unwrap();
        let two_subjects = admin.batch_execute(
            "INSERT INTO budget_counters (user_id, key_id, service, budget, \"window\", window_start, units)
             VALUES (1, 'key1', 'db', 'requests', 'second', 0, 0)",
        );
        assert!(two_subjects.is_err(), "a counter row has one subject");
        let bad_window = admin.batch_execute(
            "INSERT INTO budget_counters (user_id, service, budget, \"window\", window_start, units)
             VALUES (1, 'db', 'requests', 'week', 0, 0)",
        );
        assert!(
            bad_window.is_err(),
            "a window is second, minute, hour or day"
        );
        let second_row =
            admin.batch_execute("INSERT INTO directory_version (id, version) VALUES (2, 0)");
        assert!(second_row.is_err(), "the directory version has one row");
        // Deleting the user deletes the keys, their grants, their counters and their history.
        assert!(migrated.remove_user("alice").unwrap());
        for (table, left) in [
            ("keys", 1), // the key of the superadmin stays
            ("key_grants", 0),
            ("budget_counters", 0),
            ("key_history", 0),
        ] {
            let rows: i64 = admin
                .query_one(&format!("SELECT COUNT(*) FROM {table}"), &[])
                .unwrap()
                .get(0);
            assert_eq!(rows, left, "{table}");
        }
        // A second start finds nothing to do.
        assert!(Pg::start(migrated.pool.config.clone(), 1, Duration::from_secs(5)).is_ok());
    }

    #[test]
    fn the_migration_to_version_two_drops_the_old_access_data_and_adds_the_grants() {
        use crate::store::testing::{postgres, Backend};
        let Some(Backend::Postgres(url)) = postgres() else {
            return;
        };
        // This database was made by an older portal. It has only version 1, with rows in the old table.
        let Store::Pg(fresh) = Backend::Postgres(url).fresh() else {
            unreachable!()
        };
        let config = fresh.pool.config.clone();
        let mut admin = config.connect(NoTls).unwrap();
        admin
            .batch_execute(
                "DROP TABLE user_grants;
                 ALTER TABLE sessions DROP COLUMN admin_generation;
                 DROP TABLE budget_counters; DROP TABLE key_history; DROP TABLE key_grants; DROP TABLE keys; DROP TABLE directory_version;
                 DELETE FROM schema_migrations WHERE version > 1;
                 CREATE TABLE user_roles (
                   user_id BIGINT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
                   role TEXT NOT NULL,
                   PRIMARY KEY (user_id, role));
                 INSERT INTO users (id, login, password_hash, created_at) VALUES (1, 'alice', 'h', 1);
                 INSERT INTO user_roles (user_id, role) VALUES (1, 'ops'), (1, 'dev');",
            )
            .unwrap();
        drop(fresh);
        let migrated = Pg::start(config, 2, Duration::from_secs(5)).unwrap();
        let version: i64 = admin
            .query_one("SELECT MAX(version) FROM schema_migrations", &[])
            .unwrap()
            .get(0);
        assert_eq!(version as usize, SCHEMA.len());
        let old: i64 = admin
            .query_one(
                "SELECT COUNT(*) FROM information_schema.tables
                  WHERE table_name = 'user_roles' AND table_schema = current_schema()",
                &[],
            )
            .unwrap()
            .get(0);
        assert_eq!(old, 0, "the old data is gone");
        let users = migrated.load_users().unwrap();
        assert_eq!(users.len(), 1, "the account stays");
        assert!(users[0].grants.is_empty(), "a migrated user has no grant");
        let grant = Grant::new("access", "grafana").unwrap();
        assert!(migrated
            .update_user("alice", None, Some(&[grant.clone()]))
            .unwrap());
        assert_eq!(migrated.load_users().unwrap()[0].grants, [grant]);
        // A second start finds nothing to do.
        assert!(Pg::start(migrated.pool.config.clone(), 1, Duration::from_secs(5)).is_ok());
    }

    #[test]
    fn the_migration_to_version_three_keeps_the_grants_and_adds_the_sub_resources() {
        use crate::store::testing::{postgres, Backend};
        let Some(Backend::Postgres(url)) = postgres() else {
            return;
        };
        // This database was made by the portal of version 2.1.0: schema version 2, with grants.
        let Store::Pg(fresh) = Backend::Postgres(url).fresh() else {
            unreachable!()
        };
        let config = fresh.pool.config.clone();
        let mut admin = config.connect(NoTls).unwrap();
        admin
            .batch_execute(
                "DROP TABLE user_grants;
                 ALTER TABLE sessions DROP COLUMN admin_generation;
                 DROP TABLE budget_counters; DROP TABLE key_history; DROP TABLE key_grants; DROP TABLE keys; DROP TABLE directory_version;
                 DELETE FROM schema_migrations WHERE version > 2;
                 CREATE TABLE user_grants (
                   user_id BIGINT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
                   action TEXT NOT NULL,
                   resource_type TEXT NOT NULL,
                   resource_id TEXT NOT NULL,
                   PRIMARY KEY (user_id, action, resource_type, resource_id));
                 INSERT INTO users (id, login, password_hash, created_at)
                   VALUES (1, 'alice', 'h', 1), (2, 'bob', 'h', 1);
                 INSERT INTO user_grants (user_id, action, resource_type, resource_id)
                   VALUES (1, 'access', 'service', 'db'), (1, 'query.run.read', 'service', 'db');",
            )
            .unwrap();
        drop(fresh);
        let migrated = Pg::start(config, 2, Duration::from_secs(5)).unwrap();
        let version: i64 = admin
            .query_one("SELECT MAX(version) FROM schema_migrations", &[])
            .unwrap()
            .get(0);
        assert_eq!(version as usize, SCHEMA.len());
        let kept = vec![
            Grant::new("access", "db").unwrap(),
            Grant::new("query.run.read", "db").unwrap(),
        ];
        let users = migrated.load_users().unwrap();
        assert_eq!(users.len(), 2, "a user without a grant is kept");
        assert_eq!(users[0].grants, kept, "the grants stay");
        assert!(users[1].grants.is_empty());
        let mut next = kept.clone();
        next.push(Grant::parse("query.run.write on service:db/database:analytics").unwrap());
        next.push(Grant::parse("query.run.write on service:db/database:logs").unwrap());
        next.sort();
        assert!(migrated.update_user("alice", None, Some(&next)).unwrap());
        assert_eq!(migrated.load_users().unwrap()[0].grants, next);
        assert!(Pg::start(migrated.pool.config.clone(), 1, Duration::from_secs(5)).is_ok());
    }

    /// Version 4 adds the credential generation of the superadmin. A superadmin or impersonated
    /// row of version 3 has no generation: it is refused, and the start deletes it. A row of a
    /// database user stays.
    #[test]
    fn the_migration_to_version_four_ends_the_superadmin_sessions_of_version_three() {
        use crate::store::testing::{postgres, Backend};
        let Some(Backend::Postgres(url)) = postgres() else {
            return;
        };
        let Store::Pg(fresh) = Backend::Postgres(url).fresh() else {
            unreachable!()
        };
        let config = fresh.pool.config.clone();
        let mut admin = config.connect(NoTls).unwrap();
        admin
            .batch_execute(
                "ALTER TABLE sessions DROP COLUMN admin_generation;
                 DROP TABLE budget_counters; DROP TABLE key_history; DROP TABLE key_grants; DROP TABLE keys; DROP TABLE directory_version;
                 DELETE FROM schema_migrations WHERE version > 3;
                 DROP TABLE user_grants;
                 CREATE TABLE user_grants (
                   user_id BIGINT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
                   action TEXT NOT NULL,
                   resource_type TEXT NOT NULL,
                   resource_id TEXT NOT NULL,
                   sub_resource_type TEXT NOT NULL DEFAULT '',
                   sub_resource_id TEXT NOT NULL DEFAULT '',
                   PRIMARY KEY (user_id, action, resource_type, resource_id, sub_resource_type, sub_resource_id));
                 INSERT INTO sessions
                   (digest, binding, sid, login, impersonated, created_at, expires_at, last_seen_at)
                 VALUES ('\\x01', '\\x07', 1, NULL, false, 1, 9999999999, 1),
                        ('\\x02', '\\x07', 2, 'alice', true, 1, 9999999999, 1),
                        ('\\x03', '\\x07', 3, 'bob', false, 1, 9999999999, 1);",
            )
            .unwrap();
        drop(fresh);
        let migrated = Pg::start(config, 2, Duration::from_secs(5)).unwrap();
        let version: i64 = admin
            .query_one("SELECT MAX(version) FROM schema_migrations", &[])
            .unwrap()
            .get(0);
        assert_eq!(version as usize, SCHEMA.len());
        let generation = [5u8; 32];
        let mut ended = migrated
            .sessions_end_other_generations(&generation)
            .unwrap();
        ended.sort();
        assert_eq!(ended, [1, 2], "the superadmin and impersonated rows end");
        assert_eq!(
            migrated.sessions_count().unwrap(),
            1,
            "the row of bob stays"
        );
    }

    /// The table `user_grants` of portal 4.1.0 (schema version 4), with `rows`.
    fn version_four(admin: &mut Client, rows: &str) {
        admin
            .batch_execute(&format!(
                "DROP TABLE budget_counters; DROP TABLE key_history; DROP TABLE key_grants; DROP TABLE keys; DROP TABLE directory_version;
                 DELETE FROM schema_migrations WHERE version > 4;
                 DROP TABLE user_grants;
                 CREATE TABLE user_grants (
                   user_id BIGINT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
                   action TEXT NOT NULL,
                   resource_type TEXT NOT NULL,
                   resource_id TEXT NOT NULL,
                   sub_resource_type TEXT NOT NULL DEFAULT '',
                   sub_resource_id TEXT NOT NULL DEFAULT '',
                   PRIMARY KEY (user_id, action, resource_type, resource_id, sub_resource_type, sub_resource_id));
                 INSERT INTO users (id, login, password_hash, created_at) VALUES (1, 'alice', 'h', 1);
                 {rows}"
            ))
            .unwrap();
    }

    #[test]
    fn the_migration_to_version_five_moves_each_grant_to_a_path() {
        use crate::store::testing::{postgres, Backend};
        let Some(Backend::Postgres(url)) = postgres() else {
            return;
        };
        let Store::Pg(fresh) = Backend::Postgres(url).fresh() else {
            unreachable!()
        };
        let config = fresh.pool.config.clone();
        let mut admin = config.connect(NoTls).unwrap();
        version_four(
            &mut admin,
            "INSERT INTO user_grants
               (user_id, action, resource_type, resource_id, sub_resource_type, sub_resource_id)
             VALUES (1, 'access', 'service', 'db', '', ''),
                    (1, 'query.run.read', 'service', 'db', 'database', 'analytics'),
                    (1, 'query.run.write', 'service', 'db', 'database', 'a/b:c ~x'),
                    (1, 'query.run.write', 'service', 'db', 'database', 'été');",
        );
        drop(fresh);
        let migrated = Pg::start(config, 2, Duration::from_secs(5)).unwrap();
        let version: i64 = admin
            .query_one("SELECT MAX(version) FROM schema_migrations", &[])
            .unwrap()
            .get(0);
        assert_eq!(version as usize, SCHEMA.len());
        let paths: Vec<String> = admin
            .query(
                "SELECT resource_path FROM user_grants ORDER BY resource_path COLLATE \"C\"",
                &[],
            )
            .unwrap()
            .iter()
            .map(|row| row.get(0))
            .collect();
        assert_eq!(
            paths,
            [
                "",
                "database:%C3%A9t%C3%A9",
                "database:a%2Fb%3Ac%20~x",
                "database:analytics"
            ],
            "the SQL encoding is the encoding of the grants"
        );
        for (path, id) in [
            ("database:%C3%A9t%C3%A9", "été"),
            ("database:a%2Fb%3Ac%20~x", "a/b:c ~x"),
        ] {
            assert_eq!(
                crate::grants::Segment::new("database", id)
                    .unwrap()
                    .to_string(),
                path
            );
        }
        let mut expected: Vec<Grant> = [
            "access on service:db",
            "query.run.read on service:db/database:analytics",
            "query.run.write on service:db/database:a%2Fb%3Ac%20~x",
            "query.run.write on service:db/database:%C3%A9t%C3%A9",
        ]
        .iter()
        .map(|text| Grant::parse(text).unwrap())
        .collect();
        expected.sort();
        assert_eq!(migrated.load_users().unwrap()[0].grants, expected);
        let mut next = expected.clone();
        next.push(Grant::parse("query.run.read on service:db/host:h/database:x").unwrap());
        next.sort();
        assert!(migrated.update_user("alice", None, Some(&next)).unwrap());
        assert_eq!(migrated.load_users().unwrap()[0].grants, next);
        // A second start finds nothing to do.
        assert!(Pg::start(migrated.pool.config.clone(), 1, Duration::from_secs(5)).is_ok());
    }

    #[test]
    fn a_grant_that_does_not_fit_stops_the_migration_and_the_database_stays_at_version_four() {
        use crate::store::testing::{postgres, Backend};
        let Some(Backend::Postgres(url)) = postgres() else {
            return;
        };
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
            (
                "(1, 'q', 'service', 'db', '', 'x')".to_string(),
                "an id without a type",
            ),
        ] {
            let Store::Pg(fresh) = Backend::Postgres(url.clone()).fresh() else {
                unreachable!()
            };
            let config = fresh.pool.config.clone();
            let mut admin = config.connect(NoTls).unwrap();
            version_four(
                &mut admin,
                &format!(
                    "INSERT INTO user_grants
                       (user_id, action, resource_type, resource_id, sub_resource_type, sub_resource_id)
                     VALUES {row};"
                ),
            );
            drop(fresh);
            let error = match Pg::start(config, 1, Duration::from_secs(5)) {
                Ok(_) => panic!("{why}: the migration must stop"),
                Err(error) => error,
            };
            assert!(error.contains("resource_path"), "{why}: {error}");
            let version: i64 = admin
                .query_one("SELECT MAX(version) FROM schema_migrations", &[])
                .unwrap()
                .get(0);
            assert_eq!(version, 4, "{why}: the database did not change");
            let rows: i64 = admin
                .query_one(
                    "SELECT COUNT(*) FROM user_grants WHERE sub_resource_type IS NOT NULL",
                    &[],
                )
                .unwrap()
                .get(0);
            assert_eq!(rows, 1, "{why}: the old columns stay");
        }
    }
}
