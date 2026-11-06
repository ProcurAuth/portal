//! The SQLite storage of the inbox.
//!
//! One thread owns the connection. The async code sends closures to it through a bounded
//! channel. With one writer there is no lock contention. A confirmed write is durable (WAL,
//! `synchronous = FULL`) before the caller sees success. The producer API therefore answers
//! "accepted" only when the alert and all its delivery rows are on disk.
//!
//! The database is the database of the portal. The tables of the inbox have their own chain of
//! migrations, with its own version table `inbox_schema_version`.

use std::path::Path;

use inbox_core::proto::Kind;
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use serde::Serialize;
use tokio::sync::{mpsc, oneshot};

use super::log;
use super::notify::{Ask, Item, Level, Prefs, Scope, View, UNREAD_CAP};
use super::target::Target;
use super::util::{now_ms, random_id};

type Job = Box<dyn FnOnce(&mut Connection) + Send>;

#[derive(Clone)]
pub struct Db {
    tx: mpsc::Sender<Job>,
}

/// Signals when the database thread has checkpointed and closed the connection,
/// which happens once every [`Db`] handle is dropped.
pub struct Closed(std::sync::mpsc::Receiver<()>);

impl Closed {
    /// Waits for the database thread to finish, at most `limit`.
    pub fn wait(self, limit: std::time::Duration) -> bool {
        self.0.recv_timeout(limit).is_ok()
    }
}

#[derive(Debug)]
pub enum DbError {
    Sqlite(rusqlite::Error),
    /// PostgreSQL (see `pg.rs`): the message of the driver or of the pool.
    Pg(String),
    /// PostgreSQL cannot be used right now (no connection, no writable primary, a call that did not answer in time): the
    /// caller falls back to the degraded mode (`spool.rs`) where it has one.
    Unavailable(String),
    Closed,
}

impl DbError {
    pub fn is_unavailable(&self) -> bool {
        matches!(self, DbError::Unavailable(_))
    }
}

impl std::fmt::Display for DbError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DbError::Sqlite(e) => write!(f, "sqlite: {e}"),
            DbError::Pg(e) => write!(f, "postgres: {e}"),
            DbError::Unavailable(e) => write!(f, "postgres unavailable: {e}"),
            DbError::Closed => f.write_str("database thread stopped"),
        }
    }
}

impl From<rusqlite::Error> for DbError {
    fn from(e: rusqlite::Error) -> Self {
        DbError::Sqlite(e)
    }
}

const MIGRATIONS: &[&str] = &[
    r#"
CREATE TABLE devices (
    id                TEXT PRIMARY KEY,
    name              TEXT NOT NULL,
    public_key        BLOB NOT NULL UNIQUE,
    enrolled_at       INTEGER NOT NULL,
    enrolled_ip       TEXT NOT NULL,
    app_version       TEXT,
    last_connected_at INTEGER,
    last_seen_at      INTEGER,
    last_ip           TEXT,
    revoked_at        INTEGER
) STRICT;

CREATE TABLE subscriptions (
    device_id  TEXT NOT NULL REFERENCES devices(id),
    group_id   TEXT NOT NULL,
    action     TEXT NOT NULL CHECK (action IN ('alarm', 'ring', 'notify')),
    updated_at INTEGER NOT NULL,
    PRIMARY KEY (device_id, group_id)
) STRICT;

CREATE TABLE alerts (
    id              TEXT PRIMARY KEY,
    group_id        TEXT NOT NULL,
    title           TEXT,
    message         TEXT,
    source          TEXT NOT NULL,
    idempotency_key TEXT NOT NULL,
    created_at      INTEGER NOT NULL,
    UNIQUE (source, idempotency_key)
) STRICT;
CREATE INDEX alerts_created ON alerts(created_at);

CREATE TABLE deliveries (
    seq             INTEGER PRIMARY KEY AUTOINCREMENT,
    alert_id        TEXT NOT NULL REFERENCES alerts(id),
    device_id       TEXT NOT NULL REFERENCES devices(id),
    action          TEXT NOT NULL CHECK (action IN ('alarm', 'ring', 'notify')),
    attempts        INTEGER NOT NULL DEFAULT 0,
    first_sent_at   INTEGER,
    last_sent_at    INTEGER,
    received_at     INTEGER,
    displayed_at    INTEGER,
    acknowledged_at INTEGER,
    UNIQUE (alert_id, device_id)
) STRICT;
CREATE INDEX deliveries_pending ON deliveries(device_id, seq) WHERE received_at IS NULL;
CREATE INDEX deliveries_device ON deliveries(device_id);

CREATE TABLE connections (
    id              INTEGER PRIMARY KEY AUTOINCREMENT,
    device_id       TEXT NOT NULL REFERENCES devices(id),
    ip              TEXT NOT NULL,
    app_version     TEXT NOT NULL,
    connected_at    INTEGER NOT NULL,
    disconnected_at INTEGER,
    close_reason    TEXT
) STRICT;
CREATE INDEX connections_device ON connections(device_id, connected_at);
CREATE INDEX connections_time ON connections(connected_at);
"#,
    r#"
-- Alerts are one-way: no displayed / acknowledged stages any more.
ALTER TABLE deliveries DROP COLUMN displayed_at;
ALTER TABLE deliveries DROP COLUMN acknowledged_at;
"#,
    r#"
-- Pseudonym of the person using a device; connections keep the value they had.
ALTER TABLE devices ADD COLUMN user_name TEXT;
ALTER TABLE connections ADD COLUMN user_name TEXT;
CREATE INDEX devices_user ON devices(user_name);
CREATE INDEX connections_user ON connections(user_name, connected_at);
"#,
    r#"
-- Alert levels are device-local; alerts carry a short description and Markdown details.
ALTER TABLE subscriptions DROP COLUMN action;
ALTER TABLE deliveries DROP COLUMN action;
ALTER TABLE alerts RENAME COLUMN title TO description;
ALTER TABLE alerts RENAME COLUMN message TO details;
"#,
    r#"
-- Every device has a pseudonym: devices enrolled before pseudonyms take their name.
UPDATE devices SET user_name = name WHERE user_name IS NULL;
UPDATE connections SET user_name =
    (SELECT d.user_name FROM devices d WHERE d.id = connections.device_id)
    WHERE user_name IS NULL;
"#,
    r#"
-- An alert is a group and a title: no more details; the title is never empty.
UPDATE alerts SET description = group_id WHERE description IS NULL OR description = '';
ALTER TABLE alerts DROP COLUMN details;
ALTER TABLE alerts RENAME COLUMN description TO title;
"#,
    r#"
-- Protocol 4: an account of the portal makes a one-time enrollment code. The table keeps the
-- SHA-256 digest of the secret, never the secret. `login` is the account of the code and, later,
-- the user name of the device.
CREATE TABLE enrollment_codes (
    digest        BLOB PRIMARY KEY,
    login         TEXT NOT NULL,
    created_at_ms INTEGER NOT NULL,
    expires_at_ms INTEGER NOT NULL,
    used_at_ms    INTEGER
) STRICT;
CREATE INDEX enrollment_codes_login ON enrollment_codes(login, expires_at_ms);
CREATE INDEX enrollment_codes_expires ON enrollment_codes(expires_at_ms);
"#,
    r#"
-- Each message has a kind. Protocol 4 knows one kind: `alert`.
ALTER TABLE alerts ADD COLUMN kind TEXT NOT NULL DEFAULT 'alert';
"#,
    r#"
-- A message has one target: a group or one account. The column `group_id` becomes the id of the
-- target (the id of the group, or the login). The rows that exist are messages for a group.
ALTER TABLE alerts RENAME COLUMN group_id TO target_id;
ALTER TABLE alerts ADD COLUMN target_kind TEXT NOT NULL DEFAULT 'group'
    CHECK (target_kind IN ('group', 'user'));
"#,
    r#"
-- The notifications of the web pages: for each account, the marker "read up to" and the level of
-- the toasts. The index serves the queries of the alerts that one account receives.
CREATE TABLE web_notify (
    login      TEXT PRIMARY KEY,
    read_up_to INTEGER NOT NULL,
    level      TEXT NOT NULL DEFAULT 'all' CHECK (level IN ('all', 'important', 'off'))
) STRICT;
CREATE INDEX alerts_target ON alerts(target_id, created_at);
"#,
];

/// The schema version this binary writes: the number of SQLite migrations. The inbox keeps it in
/// the table `inbox_schema_version`. The portal has its own chain: it keeps its version in
/// `PRAGMA user_version`.
pub const SCHEMA_VERSION: i64 = MIGRATIONS.len() as i64;

impl Db {
    /// Opens (creating if needed) and migrates the database, then starts its thread.
    pub fn open(path: &Path) -> Result<(Db, Closed), String> {
        if let Some(dir) = path.parent() {
            if !dir.as_os_str().is_empty() && !dir.is_dir() {
                return Err(format!(
                    "storage.database: directory {} does not exist",
                    dir.display()
                ));
            }
        }
        let mut conn = Connection::open(path)
            .map_err(|e| format!("storage.database: cannot open {}: {e}", path.display()))?;
        configure(&mut conn).map_err(|e| format!("inbox storage: {e}"))?;
        migrate(&mut conn).map_err(|e| format!("inbox storage migration: {e}"))?;
        let closed = conn
            .execute(
                "UPDATE connections SET disconnected_at = ?1, close_reason = 'server_restart'
                 WHERE disconnected_at IS NULL",
                params![now_ms()],
            )
            .map_err(|e| format!("storage: {e}"))?;
        if closed > 0 {
            log::info!(event = "stale_connections_closed", count = closed);
        }

        let (tx, mut rx) = mpsc::channel::<Job>(1024);
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        std::thread::Builder::new()
            .name("inbox-db".into())
            .spawn(move || {
                while let Some(job) = rx.blocking_recv() {
                    job(&mut conn);
                }
                if let Err(e) = conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);") {
                    log::warn!(event = "wal_checkpoint_failed", error = %e);
                }
                drop(conn);
                log::info!(event = "database_closed");
                let _ = done_tx.send(());
            })
            .map_err(|e| format!("cannot start database thread: {e}"))?;
        Ok((Db { tx }, Closed(done_rx)))
    }

    /// Runs `f` on the database thread.
    pub async fn call<R, F>(&self, f: F) -> Result<R, DbError>
    where
        R: Send + 'static,
        F: FnOnce(&mut Connection) -> Result<R, DbError> + Send + 'static,
    {
        let (tx, rx) = oneshot::channel();
        self.tx
            .send(Box::new(move |conn| {
                let _ = tx.send(f(conn));
            }))
            .await
            .map_err(|_| DbError::Closed)?;
        rx.await.map_err(|_| DbError::Closed)?
    }
}

fn configure(conn: &mut Connection) -> rusqlite::Result<()> {
    conn.busy_timeout(std::time::Duration::from_secs(5))?;
    let mode: String = conn.query_row("PRAGMA journal_mode = WAL", [], |r| r.get(0))?;
    if !mode.eq_ignore_ascii_case("wal") {
        return Err(rusqlite::Error::InvalidParameterName(format!(
            "journal_mode is {mode}, expected wal"
        )));
    }
    // temp_store: the container root filesystem is read-only.
    conn.execute_batch(
        "PRAGMA synchronous = FULL; PRAGMA foreign_keys = ON; PRAGMA temp_store = MEMORY;",
    )
}

/// The version of the chain of the inbox. The table holds one row.
fn schema_version(conn: &Connection) -> rusqlite::Result<i64> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS inbox_schema_version (version INTEGER NOT NULL) STRICT;",
    )?;
    conn.query_row(
        "SELECT COALESCE(MAX(version), 0) FROM inbox_schema_version",
        [],
        |r| r.get(0),
    )
}

fn migrate(conn: &mut Connection) -> rusqlite::Result<()> {
    let current = schema_version(conn)?;
    if current as usize > MIGRATIONS.len() {
        return Err(rusqlite::Error::InvalidParameterName(format!(
            "the inbox schema is version {current}, this portal understands {}",
            MIGRATIONS.len()
        )));
    }
    for (i, sql) in MIGRATIONS.iter().enumerate().skip(current as usize) {
        let tx = conn.transaction_with_behavior(TransactionBehavior::Exclusive)?;
        tx.execute_batch(sql)?;
        tx.execute("DELETE FROM inbox_schema_version", [])?;
        tx.execute(
            "INSERT INTO inbox_schema_version (version) VALUES (?1)",
            [(i + 1) as i64],
        )?;
        tx.commit()?;
        log::info!(event = "migration_applied", version = i + 1);
    }
    Ok(())
}

// ---------------------------------------------------------------- devices

#[derive(Clone)]
pub struct DeviceKey {
    pub public_key: Vec<u8>,
    pub revoked: bool,
    pub name: String,
    pub user_name: String,
}

/// The number of phones that are not revoked, for each login that has one at least.
pub fn active_phones_by_login(
    conn: &Connection,
) -> rusqlite::Result<std::collections::BTreeMap<String, u32>> {
    let mut stmt = conn.prepare_cached(
        "SELECT user_name, COUNT(*) FROM devices WHERE revoked_at IS NULL GROUP BY user_name",
    )?;
    let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, u32>(1)?)))?;
    rows.collect()
}

pub fn count_active_devices(conn: &Connection) -> rusqlite::Result<u32> {
    conn.query_row(
        "SELECT COUNT(*) FROM devices WHERE revoked_at IS NULL",
        [],
        |r| r.get(0),
    )
}

pub fn insert_device(
    conn: &Connection,
    name: &str,
    user_name: &str,
    public_key: &[u8],
    ip: &str,
) -> rusqlite::Result<String> {
    let id = random_id();
    conn.execute(
        "INSERT INTO devices (id, name, user_name, public_key, enrolled_at, enrolled_ip)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![id, name, user_name, public_key, now_ms(), ip],
    )?;
    Ok(id)
}

// ---------------------------------------------------------------- enrollment codes

/// What `insert_code` did.
#[derive(Debug, PartialEq, Eq)]
pub enum CodeInsert {
    /// The code is stored. The value is the end of its life, in Unix milliseconds.
    Created { expires_at_ms: i64 },
    /// The account has `max_active` codes that are not used and not expired.
    TooMany,
    /// The account of the database has no row in the table `users` any more: it was deleted.
    NoAccount,
}

/// Stores the digest of a new code of `login`. It refuses the code when the account already has
/// `max_active` codes that are not used and not expired. `stored` tells that the account is an
/// account of the database: the transaction then also reads its row in the table `users` of the
/// portal. A code is stored only while that row exists, so a delete of the account and its
/// [`end_account`] leave no code.
pub fn insert_code(
    conn: &mut Connection,
    digest: &[u8],
    login: &str,
    now: i64,
    ttl_ms: i64,
    max_active: u32,
    stored: bool,
) -> rusqlite::Result<CodeInsert> {
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    if stored {
        let exists: bool = tx.query_row(
            "SELECT EXISTS (SELECT 1 FROM users WHERE login = ?1)",
            params![login],
            |r| r.get(0),
        )?;
        if !exists {
            return Ok(CodeInsert::NoAccount);
        }
    }
    let active: i64 = tx.query_row(
        "SELECT COUNT(*) FROM enrollment_codes
         WHERE login = ?1 AND used_at_ms IS NULL AND expires_at_ms > ?2",
        params![login, now],
        |r| r.get(0),
    )?;
    if active >= i64::from(max_active) {
        return Ok(CodeInsert::TooMany);
    }
    let expires_at_ms = now + ttl_ms;
    tx.execute(
        "INSERT INTO enrollment_codes (digest, login, created_at_ms, expires_at_ms)
         VALUES (?1, ?2, ?3, ?4)",
        params![digest, login, now, expires_at_ms],
    )?;
    tx.commit()?;
    Ok(CodeInsert::Created { expires_at_ms })
}

/// Removes a code that is not used. It returns true when a row went away.
pub fn delete_code(conn: &Connection, digest: &[u8]) -> rusqlite::Result<bool> {
    let n = conn.execute(
        "DELETE FROM enrollment_codes WHERE digest = ?1 AND used_at_ms IS NULL",
        params![digest],
    )?;
    Ok(n > 0)
}

/// Why a code does not work. The phone never learns it: every cause gives the same close code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refusal {
    Unknown,
    Used,
    Expired,
}

impl Refusal {
    pub fn as_str(self) -> &'static str {
        match self {
            Refusal::Unknown => "unknown_code",
            Refusal::Used => "used_code",
            Refusal::Expired => "expired_code",
        }
    }
}

/// What `redeem_code` did.
#[derive(Debug, PartialEq, Eq)]
pub enum Redeem {
    /// The code is used and the device is stored, in one transaction.
    Enrolled {
        device_id: String,
        login: String,
    },
    Refused(Refusal),
    /// `max_devices` devices that are not revoked exist. The code stays unused.
    TooManyDevices,
    /// The account of the code has `max_per_account` devices that are not revoked. The code stays
    /// unused.
    TooManyAccountDevices,
}

/// The limits of the devices that are not revoked, for an enrollment.
#[derive(Clone, Copy, Debug)]
pub struct DeviceLimits {
    /// All the devices of the server (`inbox.devices.max_devices`).
    pub max_devices: u32,
    /// The devices of one account (`inbox.devices.max_per_account`).
    pub max_per_account: u32,
}

/// Uses a code and stores the device of its account, in one transaction. The code is single use:
/// a second call with the same digest finds it used. A refusal changes nothing.
pub fn redeem_code(
    conn: &mut Connection,
    digest: &[u8],
    now: i64,
    limits: DeviceLimits,
    name: &str,
    public_key: &[u8],
    ip: &str,
) -> rusqlite::Result<Redeem> {
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let row: Option<(String, i64, Option<i64>)> = tx
        .query_row(
            "SELECT login, expires_at_ms, used_at_ms FROM enrollment_codes WHERE digest = ?1",
            params![digest],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()?;
    let Some((login, expires_at_ms, used_at_ms)) = row else {
        return Ok(Redeem::Refused(Refusal::Unknown));
    };
    if used_at_ms.is_some() {
        return Ok(Redeem::Refused(Refusal::Used));
    }
    if expires_at_ms <= now {
        return Ok(Redeem::Refused(Refusal::Expired));
    }
    let of_account: u32 = tx.query_row(
        "SELECT COUNT(*) FROM devices WHERE user_name = ?1 AND revoked_at IS NULL",
        params![login],
        |r| r.get(0),
    )?;
    if of_account >= limits.max_per_account {
        return Ok(Redeem::TooManyAccountDevices);
    }
    if count_active_devices(&tx)? >= limits.max_devices {
        return Ok(Redeem::TooManyDevices);
    }
    tx.execute(
        "UPDATE enrollment_codes SET used_at_ms = ?2 WHERE digest = ?1 AND used_at_ms IS NULL",
        params![digest, now],
    )?;
    let device_id = insert_device(&tx, name, &login, public_key, ip)?;
    tx.commit()?;
    Ok(Redeem::Enrolled { device_id, login })
}

/// The name that the rows of an ended account carry in place of its login. It holds the character
/// `:`, which no login can have (`subject_id::SEPARATOR`), so it never names a later account.
/// Each call gives a new name.
pub fn ended_name(login: &str) -> String {
    format!("{login}:ended:{}", &random_id()[..12])
}

/// Ends the inbox state of the account `login`, in one transaction. It deletes the unused codes,
/// revokes the devices and drops their subscriptions. Then it detaches the rows that name the
/// login, so that a later account with the same login gets none of them: the devices, the alerts
/// to the account and the alerts that the account sent with the form get [`ended_name`], and the
/// notification state goes. The connection log keeps the login. A phone that enrolls at the same
/// time either finds its code gone or has its device revoked. The result is the ids of the devices
/// that this call revoked. A second call finds nothing to do.
pub fn end_account(conn: &mut Connection, login: &str) -> rusqlite::Result<Vec<String>> {
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    tx.execute(
        "DELETE FROM enrollment_codes WHERE login = ?1 AND used_at_ms IS NULL",
        params![login],
    )?;
    let ids: Vec<String> = {
        let mut stmt =
            tx.prepare("SELECT id FROM devices WHERE user_name = ?1 AND revoked_at IS NULL")?;
        let rows = stmt.query_map(params![login], |r| r.get(0))?;
        rows.collect::<rusqlite::Result<_>>()?
    };
    let now = now_ms();
    for id in &ids {
        tx.execute(
            "UPDATE devices SET revoked_at = ?2 WHERE id = ?1 AND revoked_at IS NULL",
            params![id, now],
        )?;
        tx.execute(
            "DELETE FROM subscriptions WHERE device_id = ?1",
            params![id],
        )?;
    }
    let ended = ended_name(login);
    tx.execute(
        "UPDATE devices SET user_name = ?2 WHERE user_name = ?1",
        params![login, ended],
    )?;
    tx.execute(
        "UPDATE alerts SET target_id = ?2 WHERE target_kind = 'user' AND target_id = ?1",
        params![login, ended],
    )?;
    tx.execute(
        "UPDATE alerts SET source = ?2 WHERE source = ?1",
        params![admin_source(login), admin_source(&ended)],
    )?;
    tx.execute("DELETE FROM web_notify WHERE login = ?1", params![login])?;
    tx.commit()?;
    Ok(ids)
}

/// The source of an alert that the account `login` sent with the form of the page.
pub fn admin_source(login: &str) -> String {
    format!("admin:{login}")
}

pub fn find_device(conn: &Connection, id: &str) -> rusqlite::Result<Option<DeviceKey>> {
    conn.query_row(
        "SELECT public_key, revoked_at IS NOT NULL, name, user_name FROM devices WHERE id = ?1",
        params![id],
        |r| {
            Ok(DeviceKey {
                public_key: r.get(0)?,
                revoked: r.get(1)?,
                name: r.get(2)?,
                user_name: r.get(3)?,
            })
        },
    )
    .optional()
}

/// Revokes a device and drops its subscriptions. Its undelivered alerts can never be
/// delivered any more; pending statistics and retention ignore revoked devices.
pub fn revoke_device(conn: &mut Connection, id: &str) -> rusqlite::Result<bool> {
    let tx = conn.transaction()?;
    let n = tx.execute(
        "UPDATE devices SET revoked_at = ?2 WHERE id = ?1 AND revoked_at IS NULL",
        params![id, now_ms()],
    )?;
    tx.execute(
        "DELETE FROM subscriptions WHERE device_id = ?1",
        params![id],
    )?;
    tx.commit()?;
    Ok(n > 0)
}

pub fn record_connect(
    conn: &Connection,
    device_id: &str,
    ip: &str,
    app_version: &str,
) -> rusqlite::Result<i64> {
    let now = now_ms();
    conn.execute(
        "UPDATE devices SET last_connected_at = ?2, last_seen_at = ?2, last_ip = ?3, app_version = ?4 WHERE id = ?1",
        params![device_id, now, ip, app_version],
    )?;
    conn.execute(
        "INSERT INTO connections (device_id, user_name, ip, app_version, connected_at)
         SELECT ?1, user_name, ?2, ?3, ?4 FROM devices WHERE id = ?1",
        params![device_id, ip, app_version, now],
    )?;
    Ok(conn.last_insert_rowid())
}

pub fn record_disconnect(
    conn: &Connection,
    connection_id: i64,
    device_id: &str,
    reason: &str,
) -> rusqlite::Result<()> {
    let now = now_ms();
    conn.execute(
        "UPDATE connections SET disconnected_at = ?2, close_reason = ?3 WHERE id = ?1",
        params![connection_id, now, reason],
    )?;
    conn.execute(
        "UPDATE devices SET last_seen_at = ?2 WHERE id = ?1",
        params![device_id, now],
    )?;
    Ok(())
}

pub fn touch_seen(conn: &mut Connection, seen: &[(String, i64)]) -> rusqlite::Result<()> {
    let tx = conn.transaction()?;
    {
        let mut stmt = tx.prepare_cached(
            "UPDATE devices SET last_seen_at = MAX(COALESCE(last_seen_at, 0), ?2) WHERE id = ?1",
        )?;
        for (id, at) in seen {
            stmt.execute(params![id, at])?;
        }
    }
    tx.commit()
}

// ---------------------------------------------------------------- subscriptions

/// Ids of the groups `device_id` is subscribed to. The server never knows how the
/// device reacts to them: that is a setting of the device.
pub fn subscriptions(conn: &Connection, device_id: &str) -> rusqlite::Result<Vec<String>> {
    let mut stmt = conn.prepare_cached(
        "SELECT group_id FROM subscriptions WHERE device_id = ?1 ORDER BY group_id",
    )?;
    let rows = stmt.query_map(params![device_id], |r| r.get(0))?;
    rows.collect()
}

pub fn set_subscription(conn: &Connection, device_id: &str, group: &str) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO subscriptions (device_id, group_id, updated_at) VALUES (?1, ?2, ?3)
         ON CONFLICT (device_id, group_id) DO UPDATE SET updated_at = excluded.updated_at",
        params![device_id, group, now_ms()],
    )?;
    Ok(())
}

pub fn delete_subscription(
    conn: &Connection,
    device_id: &str,
    group: &str,
) -> rusqlite::Result<()> {
    conn.execute(
        "DELETE FROM subscriptions WHERE device_id = ?1 AND group_id = ?2",
        params![device_id, group],
    )?;
    Ok(())
}

/// Removes subscriptions to groups no longer declared in the configuration.
pub fn prune_subscriptions(conn: &Connection, groups: &[String]) -> rusqlite::Result<usize> {
    let list = serde_json::to_string(groups).expect("strings serialize");
    conn.execute(
        "DELETE FROM subscriptions WHERE group_id NOT IN (SELECT value FROM json_each(?1))",
        params![list],
    )
}

// ---------------------------------------------------------------- alerts

#[derive(Clone)]
pub struct NewAlert {
    pub kind: Kind,
    pub target: Target,
    pub title: String,
    pub source: String,
    pub idempotency_key: String,
}

/// The start of the source of an alert of the webhook of Alertmanager and Grafana.
pub const WEBHOOK_SOURCE: &str = "webhook:";

/// Whether a new title fits the alert that the store holds under the same source and key. The
/// title must be the same, except for an alert of the webhook: its key names the alert, and a
/// later notification of the same alert can carry a new summary. The first title stays.
pub fn same_title(source: &str, stored: &str, new: &str) -> bool {
    stored == new || source.starts_with(WEBHOOK_SOURCE)
}

pub enum InsertOutcome {
    Created {
        alert_id: String,
        devices: Vec<String>,
    },
    Duplicate {
        alert_id: String,
        devices: Vec<String>,
    },
    /// Same idempotency key, different content.
    Conflict,
    /// The target is an account that has no phone to use: the alert is not stored.
    NoPhone,
}

/// Stores the alert and one delivery row per subscribed, non-revoked device, in a
/// single transaction. Devices subscribed at this instant are the targets, whether
/// they are online or not.
pub fn insert_alert(conn: &mut Connection, alert: &NewAlert) -> rusqlite::Result<InsertOutcome> {
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let existing = tx
        .query_row(
            "SELECT id, target_kind, target_id, title FROM alerts WHERE source = ?1 AND idempotency_key = ?2",
            params![alert.source, alert.idempotency_key],
            |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                ))
            },
        )
        .optional()?;
    if let Some((id, kind, target, title)) = existing {
        if kind != alert.target.kind()
            || target != alert.target.id()
            || !same_title(&alert.source, &title, &alert.title)
        {
            return Ok(InsertOutcome::Conflict);
        }
        let devices = delivery_devices(&tx, &id)?;
        return Ok(InsertOutcome::Duplicate {
            alert_id: id,
            devices,
        });
    }
    // An account with no phone to use gets nothing: the alert would wait for no one.
    if let Target::User(login) = &alert.target {
        let phones: i64 = tx.query_row(
            "SELECT COUNT(*) FROM devices WHERE user_name = ?1 AND revoked_at IS NULL",
            params![login],
            |r| r.get(0),
        )?;
        if phones == 0 {
            return Ok(InsertOutcome::NoPhone);
        }
    }
    let id = random_id();
    let now = now_ms();
    tx.execute(
        "INSERT INTO alerts (id, kind, target_kind, target_id, title, source, idempotency_key, created_at)
         VALUES (?1, ?7, ?8, ?2, ?3, ?4, ?5, ?6)",
        params![
            id,
            alert.target.id(),
            alert.title,
            alert.source,
            alert.idempotency_key,
            now,
            alert.kind.as_str(),
            alert.target.kind()
        ],
    )?;
    match &alert.target {
        Target::Group(group) => tx.execute(
            "INSERT INTO deliveries (alert_id, device_id)
             SELECT ?1, s.device_id FROM subscriptions s
             JOIN devices d ON d.id = s.device_id
             WHERE s.group_id = ?2 AND d.revoked_at IS NULL
             ORDER BY s.device_id",
            params![id, group],
        )?,
        Target::User(login) => tx.execute(
            "INSERT INTO deliveries (alert_id, device_id)
             SELECT ?1, d.id FROM devices d
             WHERE d.user_name = ?2 AND d.revoked_at IS NULL
             ORDER BY d.id",
            params![id, login],
        )?,
    };
    let devices = delivery_devices(&tx, &id)?;
    tx.commit()?;
    Ok(InsertOutcome::Created {
        alert_id: id,
        devices,
    })
}

fn delivery_devices(conn: &Connection, alert_id: &str) -> rusqlite::Result<Vec<String>> {
    let mut stmt = conn.prepare_cached(
        "SELECT device_id FROM deliveries WHERE alert_id = ?1 ORDER BY device_id",
    )?;
    let rows = stmt.query_map(params![alert_id], |r| r.get(0))?;
    rows.collect()
}

#[derive(Clone)]
pub struct PendingDelivery {
    pub seq: i64,
    pub alert_id: String,
    pub kind: Kind,
    pub target: Target,
    pub title: String,
    pub created_at: i64,
}

/// Oldest deliveries of `device_id` whose receipt the device has not sent yet,
/// among alerts created at or after `cutoff` (older ones have expired).
pub fn pending_deliveries(
    conn: &Connection,
    device_id: &str,
    limit: u32,
    cutoff: i64,
) -> rusqlite::Result<Vec<PendingDelivery>> {
    let mut stmt = conn.prepare_cached(
        "SELECT d.seq, a.id, a.target_kind, a.target_id, a.title, a.created_at, a.kind
         FROM deliveries d JOIN alerts a ON a.id = d.alert_id
         WHERE d.device_id = ?1 AND d.received_at IS NULL AND a.created_at >= ?3
         ORDER BY d.seq LIMIT ?2",
    )?;
    let rows = stmt.query_map(params![device_id, limit, cutoff], |r| {
        Ok(PendingDelivery {
            seq: r.get(0)?,
            alert_id: r.get(1)?,
            target: target_columns(r, 2)?,
            title: r.get(4)?,
            created_at: r.get(5)?,
            kind: kind_column(r, 6)?,
        })
    })?;
    rows.collect()
}

/// Reads the two columns of a target: the kind at `index`, the id at `index + 1`. An unknown
/// kind is a conversion error.
fn target_columns(r: &rusqlite::Row<'_>, index: usize) -> rusqlite::Result<Target> {
    let kind: String = r.get(index)?;
    let id: String = r.get(index + 1)?;
    Target::from_columns(&kind, id).ok_or_else(|| {
        rusqlite::Error::FromSqlConversionFailure(
            index,
            rusqlite::types::Type::Text,
            format!("unknown target kind {kind:?}").into(),
        )
    })
}

/// Reads a column that holds the kind of a message. An unknown text is a conversion error.
fn kind_column(r: &rusqlite::Row<'_>, index: usize) -> rusqlite::Result<Kind> {
    let text: String = r.get(index)?;
    Kind::parse(&text).ok_or_else(|| {
        rusqlite::Error::FromSqlConversionFailure(
            index,
            rusqlite::types::Type::Text,
            format!("unknown message kind {text:?}").into(),
        )
    })
}

pub fn mark_sent(conn: &mut Connection, seqs: &[i64]) -> rusqlite::Result<()> {
    let now = now_ms();
    let tx = conn.transaction()?;
    {
        let mut stmt = tx.prepare_cached(
            "UPDATE deliveries SET attempts = attempts + 1,
                 first_sent_at = COALESCE(first_sent_at, ?2), last_sent_at = ?2
             WHERE seq = ?1",
        )?;
        for seq in seqs {
            stmt.execute(params![seq, now])?;
        }
    }
    tx.commit()
}

pub struct AckOutcome {
    pub seq: i64,
    /// Set when this receipt is the first one for the delivery.
    pub first_receipt_latency_ms: Option<i64>,
}

/// Records a device receipt. Repeated receipts are idempotent. Returns `None` if
/// the device has no such delivery.
pub fn ack(
    conn: &Connection,
    device_id: &str,
    alert_id: &str,
) -> rusqlite::Result<Option<AckOutcome>> {
    let now = now_ms();
    let row = conn
        .query_row(
            "SELECT d.seq, d.received_at IS NULL, a.created_at FROM deliveries d JOIN alerts a ON a.id = d.alert_id
             WHERE d.device_id = ?1 AND d.alert_id = ?2",
            params![device_id, alert_id],
            |r| Ok((r.get::<_, i64>(0)?, r.get::<_, bool>(1)?, r.get::<_, i64>(2)?)),
        )
        .optional()?;
    let Some((seq, not_received, created_at)) = row else {
        return Ok(None);
    };
    conn.execute(
        "UPDATE deliveries SET received_at = COALESCE(received_at, ?2) WHERE seq = ?1",
        params![seq, now],
    )?;
    Ok(Some(AckOutcome {
        seq,
        first_receipt_latency_ms: not_received.then_some(now - created_at),
    }))
}

// ---------------------------------------------------------------- admin views

#[derive(Debug, Serialize)]
pub struct DeviceView {
    pub id: String,
    pub name: String,
    pub user_name: String,
    pub enrolled_at: i64,
    pub enrolled_ip: String,
    pub app_version: Option<String>,
    pub last_connected_at: Option<i64>,
    pub last_seen_at: Option<i64>,
    pub last_ip: Option<String>,
    pub revoked_at: Option<i64>,
    pub subscriptions: Vec<String>,
    pub pending: i64,
}

/// `cutoff`: alerts created before it have expired and no longer count as pending.
pub fn list_devices(conn: &Connection, cutoff: i64) -> rusqlite::Result<Vec<DeviceView>> {
    let mut stmt = conn.prepare_cached(
        "SELECT d.id, d.name, d.enrolled_at, d.enrolled_ip, d.app_version, d.last_connected_at,
                d.last_seen_at, d.last_ip, d.revoked_at,
                (SELECT COUNT(*) FROM deliveries x JOIN alerts y ON y.id = x.alert_id
                 WHERE x.device_id = d.id AND x.received_at IS NULL AND y.created_at >= ?1),
                d.user_name
         FROM devices d ORDER BY d.revoked_at IS NOT NULL, d.user_name, d.name",
    )?;
    let mut devices: Vec<DeviceView> = stmt
        .query_map(params![cutoff], |r| {
            Ok(DeviceView {
                id: r.get(0)?,
                name: r.get(1)?,
                enrolled_at: r.get(2)?,
                enrolled_ip: r.get(3)?,
                app_version: r.get(4)?,
                last_connected_at: r.get(5)?,
                last_seen_at: r.get(6)?,
                last_ip: r.get(7)?,
                revoked_at: r.get(8)?,
                subscriptions: Vec::new(),
                pending: r.get(9)?,
                user_name: r.get(10)?,
            })
        })?
        .collect::<rusqlite::Result<_>>()?;
    for d in &mut devices {
        d.subscriptions = subscriptions(conn, &d.id)?;
    }
    Ok(devices)
}

#[derive(Debug, Serialize)]
pub struct AlertSummary {
    pub id: String,
    pub kind: Kind,
    /// Who the alert is for: a group, or one account.
    pub target: Target,
    pub title: String,
    pub source: String,
    pub created_at: i64,
    /// The number of phones that the alert goes to.
    pub targets: i64,
    pub received: i64,
    /// Targets that never received the alert before it expired.
    pub expired: i64,
    /// Pseudonyms of non-revoked targets that have not received the alert yet.
    pub missing: Vec<String>,
}

const ALERT_SUMMARY: &str =
    "SELECT a.id, a.target_kind, a.target_id, a.title, a.source, a.created_at,
        COUNT(d.seq), COUNT(d.received_at), a.kind
     FROM alerts a LEFT JOIN deliveries d ON d.alert_id = a.id";

fn alert_summary(r: &rusqlite::Row<'_>) -> rusqlite::Result<AlertSummary> {
    Ok(AlertSummary {
        id: r.get(0)?,
        target: target_columns(r, 1)?,
        title: r.get(3)?,
        source: r.get(4)?,
        created_at: r.get(5)?,
        targets: r.get(6)?,
        received: r.get(7)?,
        kind: kind_column(r, 8)?,
        expired: 0,
        missing: Vec::new(),
    })
}

/// Newest alerts first, at most `limit`, created at or after `since_ms`.
pub fn recent_alerts(
    conn: &Connection,
    limit: u32,
    since_ms: i64,
    cutoff: i64,
) -> rusqlite::Result<Vec<AlertSummary>> {
    let sql = format!(
        "{ALERT_SUMMARY} WHERE a.created_at >= ?2 GROUP BY a.id ORDER BY a.created_at DESC LIMIT ?1"
    );
    let mut stmt = conn.prepare_cached(&sql)?;
    let rows = stmt.query_map(params![limit, since_ms], alert_summary)?;
    let mut alerts: Vec<AlertSummary> = rows
        .map(|r| r.map(|a| with_expiry(a, cutoff)))
        .collect::<rusqlite::Result<_>>()?;
    for a in &mut alerts {
        a.missing = missing_users(conn, &a.id)?;
    }
    Ok(alerts)
}

/// The number of alerts of one group in one bucket of time: the data of the graph of the page.
/// The alerts for an account are not in the graph.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AlertCount {
    /// The number of the bucket. Bucket 0 starts at `since_ms`.
    pub bucket: i64,
    pub group: String,
    pub count: i64,
}

/// The alerts created at or after `since_ms`, counted for each bucket of `bucket_ms` and each
/// group. The result has at most one row for each bucket and group, so it is small. The index of
/// `created_at` serves the range.
pub fn alert_counts(
    conn: &Connection,
    since_ms: i64,
    bucket_ms: i64,
) -> rusqlite::Result<Vec<AlertCount>> {
    let mut stmt = conn.prepare_cached(
        "SELECT (created_at - ?1) / ?2, target_id, COUNT(*) FROM alerts
         WHERE created_at >= ?1 AND target_kind = 'group'
         GROUP BY 1, 2 ORDER BY 1, 2",
    )?;
    let rows = stmt.query_map(params![since_ms, bucket_ms.max(1)], |r| {
        Ok(AlertCount {
            bucket: r.get(0)?,
            group: r.get(1)?,
            count: r.get(2)?,
        })
    })?;
    rows.collect()
}

/// Pseudonyms of the non-revoked devices that have not received `alert_id` yet.
fn missing_users(conn: &Connection, alert_id: &str) -> rusqlite::Result<Vec<String>> {
    let mut stmt = conn.prepare_cached(
        "SELECT DISTINCT v.user_name FROM deliveries d JOIN devices v ON v.id = d.device_id
         WHERE d.alert_id = ?1 AND d.received_at IS NULL AND v.revoked_at IS NULL
         ORDER BY v.user_name",
    )?;
    let rows = stmt.query_map(params![alert_id], |r| r.get(0))?;
    rows.collect()
}

/// Fills `expired`: an alert created before `cutoff` is no longer delivered.
pub(crate) fn with_expiry(mut a: AlertSummary, cutoff: i64) -> AlertSummary {
    if a.created_at < cutoff {
        a.expired = a.targets - a.received;
    }
    a
}

#[derive(Debug, Serialize)]
pub struct DeliveryView {
    pub device_id: String,
    pub device_name: String,
    pub user_name: String,
    pub attempts: i64,
    pub first_sent_at: Option<i64>,
    pub last_sent_at: Option<i64>,
    pub received_at: Option<i64>,
    /// Not received before the alert expired: it will not be sent any more.
    pub expired: bool,
    pub device_revoked: bool,
}

#[derive(Debug, Serialize)]
pub struct AlertDetail {
    #[serde(flatten)]
    pub summary: AlertSummary,
    pub deliveries: Vec<DeliveryView>,
}

pub fn alert_detail(
    conn: &Connection,
    id: &str,
    cutoff: i64,
) -> rusqlite::Result<Option<AlertDetail>> {
    let sql = format!("{ALERT_SUMMARY} WHERE a.id = ?1 GROUP BY a.id");
    let Some(summary) = conn
        .query_row(&sql, params![id], alert_summary)
        .optional()?
    else {
        return Ok(None);
    };
    let mut summary = with_expiry(summary, cutoff);
    summary.missing = missing_users(conn, id)?;
    let is_expired = summary.created_at < cutoff;
    let mut stmt = conn.prepare_cached(
        "SELECT d.device_id, v.name, d.attempts, d.first_sent_at, d.last_sent_at,
                d.received_at, v.revoked_at IS NOT NULL, v.user_name
         FROM deliveries d JOIN devices v ON v.id = d.device_id
         WHERE d.alert_id = ?1 ORDER BY v.user_name, v.name",
    )?;
    let deliveries = stmt
        .query_map(params![id], |r| {
            Ok(DeliveryView {
                device_id: r.get(0)?,
                device_name: r.get(1)?,
                attempts: r.get(2)?,
                first_sent_at: r.get(3)?,
                last_sent_at: r.get(4)?,
                received_at: r.get(5)?,
                expired: is_expired && r.get::<_, Option<i64>>(5)?.is_none(),
                device_revoked: r.get(6)?,
                user_name: r.get(7)?,
            })
        })?
        .collect::<rusqlite::Result<_>>()?;
    Ok(Some(AlertDetail {
        summary,
        deliveries,
    }))
}

#[derive(Debug, Serialize)]
pub struct ConnectionView {
    pub device_id: String,
    pub device_name: String,
    /// Pseudonym the device had during this connection.
    pub user_name: String,
    pub ip: String,
    pub app_version: String,
    pub connected_at: i64,
    pub disconnected_at: Option<i64>,
    pub close_reason: Option<String>,
}

/// Newest connections first; only those of `user_name` when given.
pub fn recent_connections(
    conn: &Connection,
    limit: u32,
    user_name: Option<&str>,
) -> rusqlite::Result<Vec<ConnectionView>> {
    let mut stmt = conn.prepare_cached(
        "SELECT c.device_id, d.name, c.ip, c.app_version, c.connected_at, c.disconnected_at, c.close_reason,
                c.user_name
         FROM connections c JOIN devices d ON d.id = c.device_id
         WHERE ?2 IS NULL OR c.user_name = ?2
         ORDER BY c.connected_at DESC LIMIT ?1",
    )?;
    let rows = stmt.query_map(params![limit, user_name], |r| {
        Ok(ConnectionView {
            device_id: r.get(0)?,
            device_name: r.get(1)?,
            ip: r.get(2)?,
            app_version: r.get(3)?,
            connected_at: r.get(4)?,
            disconnected_at: r.get(5)?,
            close_reason: r.get(6)?,
            user_name: r.get(7)?,
        })
    })?;
    rows.collect()
}

/// Gauges for telemetry: (pending deliveries, oldest pending alert age in ms).
/// Expired deliveries (alerts created before `cutoff`) are not pending.
pub fn pending_stats(conn: &Connection, cutoff: i64) -> rusqlite::Result<(i64, i64)> {
    conn.query_row(
        "SELECT COUNT(*), COALESCE(?1 - MIN(a.created_at), 0)
         FROM deliveries d JOIN alerts a ON a.id = d.alert_id
         JOIN devices v ON v.id = d.device_id
         WHERE d.received_at IS NULL AND v.revoked_at IS NULL AND a.created_at >= ?2",
        params![now_ms(), cutoff],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )
}

/// Non-revoked devices with at least one subscription: (id, last_seen_at).
pub fn subscribed_devices(conn: &Connection) -> rusqlite::Result<Vec<(String, Option<i64>)>> {
    let mut stmt = conn.prepare_cached(
        "SELECT d.id, d.last_seen_at FROM devices d
         WHERE d.revoked_at IS NULL AND EXISTS (SELECT 1 FROM subscriptions s WHERE s.device_id = d.id)",
    )?;
    let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
    rows.collect()
}

// ---------------------------------------------------------------- notifications of the web

/// The condition on `target_kind` and `target_id` for the alerts that `scope` receives, with `?`
/// placeholders, and the values of the placeholders in order. The index `alerts_target` serves it.
fn scope_sql(scope: &Scope) -> (String, Vec<String>) {
    let mut values: Vec<String> = Vec::new();
    let groups = match &scope.groups {
        None => "target_kind = 'group'".to_string(),
        Some(ids) if ids.is_empty() => "0".to_string(),
        Some(ids) => {
            values.extend(ids.iter().cloned());
            let marks = vec!["?"; ids.len()].join(", ");
            format!("target_kind = 'group' AND target_id IN ({marks})")
        }
    };
    values.push(scope.login.clone());
    (
        format!("(({groups}) OR (target_kind = 'user' AND target_id = ?))"),
        values,
    )
}

/// The preferences of an account. An account that has no row gets one, with the marker at `now`:
/// the unread count starts at the first sight of the account, not at the oldest alert. An account
/// that has a row costs one read and no write.
fn notify_prefs(conn: &Connection, login: &str, now: i64) -> rusqlite::Result<Prefs> {
    let read = |conn: &Connection| {
        conn.query_row(
            "SELECT read_up_to, level FROM web_notify WHERE login = ?1",
            params![login],
            |r| {
                let level: String = r.get(1)?;
                Ok(Prefs {
                    read_up_to: r.get(0)?,
                    level: Level::parse(&level).ok_or_else(|| {
                        rusqlite::Error::FromSqlConversionFailure(
                            1,
                            rusqlite::types::Type::Text,
                            format!("unknown level {level:?}").into(),
                        )
                    })?,
                })
            },
        )
        .optional()
    };
    if let Some(prefs) = read(conn)? {
        return Ok(prefs);
    }
    conn.execute(
        "INSERT INTO web_notify (login, read_up_to) VALUES (?1, ?2) ON CONFLICT(login) DO NOTHING",
        params![login, now],
    )?;
    read(conn)?.ok_or(rusqlite::Error::QueryReturnedNoRows)
}

/// The alerts that `scope` receives: the preferences, the count of unread ones and the newest
/// ones. Each read uses an index.
pub fn notify_view(
    conn: &Connection,
    scope: &Scope,
    ask: &Ask,
    now: i64,
) -> rusqlite::Result<View> {
    let prefs = notify_prefs(conn, &scope.login, now)?;
    if ask.skip_if_off && prefs.level == Level::Off {
        return Ok(View {
            prefs,
            unread: 0,
            items: Vec::new(),
        });
    }
    let (condition, values) = scope_sql(scope);
    let unread: u32 = {
        let sql = format!(
            "SELECT COUNT(*) FROM (SELECT 1 FROM alerts WHERE created_at > ? AND {condition} LIMIT ?)"
        );
        let mut args: Vec<rusqlite::types::Value> = vec![prefs.read_up_to.into()];
        args.extend(values.iter().cloned().map(Into::into));
        args.push(i64::from(UNREAD_CAP).into());
        conn.query_row(&sql, rusqlite::params_from_iter(args), |r| r.get(0))?
    };
    let items = if ask.limit == 0 {
        Vec::new()
    } else {
        let sql = format!(
            "SELECT id, target_kind, target_id, title, created_at FROM alerts
             WHERE created_at > ? AND {condition} ORDER BY created_at DESC, id DESC LIMIT ?"
        );
        let mut args: Vec<rusqlite::types::Value> = vec![ask.since_ms.into()];
        args.extend(values.into_iter().map(Into::into));
        args.push(i64::from(ask.limit).into());
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(rusqlite::params_from_iter(args), |r| {
            Ok(Item {
                id: r.get(0)?,
                target: target_columns(r, 1)?,
                title: r.get(3)?,
                created_at: r.get(4)?,
            })
        })?;
        rows.collect::<rusqlite::Result<_>>()?
    };
    Ok(View {
        prefs,
        unread,
        items,
    })
}

/// Moves the marker of the account to the time of its newest alert. The marker never goes back.
/// The result is the marker.
pub fn notify_mark_read(conn: &Connection, scope: &Scope, now: i64) -> rusqlite::Result<i64> {
    notify_prefs(conn, &scope.login, now)?;
    let (condition, values) = scope_sql(scope);
    let newest: Option<i64> = conn.query_row(
        &format!("SELECT MAX(created_at) FROM alerts WHERE {condition}"),
        rusqlite::params_from_iter(values),
        |r| r.get(0),
    )?;
    conn.execute(
        "UPDATE web_notify SET read_up_to = MAX(read_up_to, ?2) WHERE login = ?1",
        params![scope.login, newest.unwrap_or(0)],
    )?;
    conn.query_row(
        "SELECT read_up_to FROM web_notify WHERE login = ?1",
        params![scope.login],
        |r| r.get(0),
    )
}

/// Sets the level of the toasts of an account.
pub fn notify_set_level(
    conn: &Connection,
    login: &str,
    level: Level,
    now: i64,
) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO web_notify (login, read_up_to, level) VALUES (?1, ?2, ?3)
         ON CONFLICT(login) DO UPDATE SET level = excluded.level",
        params![login, now, level.as_str()],
    )?;
    Ok(())
}

// ---------------------------------------------------------------- retention

/// An enrollment code stays in the table this long after it expires. A used or expired code is
/// refused at once, so the delay only keeps the rows for an operator who looks at them.
pub const CODE_KEEP_MS: i64 = 24 * 3_600_000;

/// Deletes history past retention. Configuration guarantees that
/// `alert_retention >= delivery_ttl`, so nothing deliverable is ever removed. It deletes the
/// enrollment codes that expired more than [`CODE_KEEP_MS`] ago. The result is the number of
/// alerts, connections and codes.
pub fn purge(
    conn: &mut Connection,
    alert_cutoff: i64,
    connection_cutoff: i64,
) -> rusqlite::Result<(usize, usize, usize)> {
    let tx = conn.transaction()?;
    tx.execute_batch(
        "CREATE TEMP TABLE IF NOT EXISTS purge_ids (id TEXT PRIMARY KEY); DELETE FROM purge_ids;",
    )?;
    tx.execute(
        "INSERT INTO purge_ids SELECT a.id FROM alerts a WHERE a.created_at < ?1",
        params![alert_cutoff],
    )?;
    tx.execute(
        "DELETE FROM deliveries WHERE alert_id IN (SELECT id FROM purge_ids)",
        [],
    )?;
    let alerts = tx.execute(
        "DELETE FROM alerts WHERE id IN (SELECT id FROM purge_ids)",
        [],
    )?;
    let connections = tx.execute(
        "DELETE FROM connections WHERE connected_at < ?1 AND disconnected_at IS NOT NULL",
        params![connection_cutoff],
    )?;
    let codes = tx.execute(
        "DELETE FROM enrollment_codes WHERE expires_at_ms < ?1",
        params![now_ms() - CODE_KEEP_MS],
    )?;
    tx.commit()?;
    Ok((alerts, connections, codes))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mem() -> Connection {
        let mut c = Connection::open_in_memory().unwrap();
        c.execute_batch("PRAGMA foreign_keys = ON;").unwrap();
        migrate(&mut c).unwrap();
        c
    }

    fn alert(key: &str) -> NewAlert {
        NewAlert {
            kind: Kind::Alert,
            target: Target::Group("infra".into()),
            title: "t".into(),
            source: "producer:test".into(),
            idempotency_key: key.into(),
        }
    }

    #[test]
    fn fan_out_targets_subscribed_devices_only() {
        let mut c = mem();
        let a = insert_device(&c, "a", "a", &[1; 32], "1.1.1.1").unwrap();
        let b = insert_device(&c, "b", "b", &[2; 32], "1.1.1.1").unwrap();
        let r = insert_device(&c, "r", "r", &[3; 32], "1.1.1.1").unwrap();
        set_subscription(&c, &a, "infra").unwrap();
        set_subscription(&c, &b, "other").unwrap();
        set_subscription(&c, &r, "infra").unwrap();
        revoke_device(&mut c, &r).unwrap();
        let InsertOutcome::Created { devices, .. } = insert_alert(&mut c, &alert("k1")).unwrap()
        else {
            panic!()
        };
        assert_eq!(devices, vec![a.clone()]);
        let p = pending_deliveries(&c, &a, 10, 0).unwrap();
        assert_eq!(p.len(), 1);
        assert!(pending_deliveries(&c, &b, 10, 0).unwrap().is_empty());
    }

    #[test]
    fn an_alert_has_the_kind_alert_in_the_table_and_in_the_rows() {
        let mut c = mem();
        let a = insert_device(&c, "a", "a", &[1; 32], "ip").unwrap();
        set_subscription(&c, &a, "infra").unwrap();
        insert_alert(&mut c, &alert("k")).unwrap();
        let stored: String = c
            .query_row("SELECT kind FROM alerts", [], |r| r.get(0))
            .unwrap();
        assert_eq!(stored, "alert");
        assert_eq!(
            pending_deliveries(&c, &a, 10, 0).unwrap()[0].kind,
            Kind::Alert
        );
        assert_eq!(recent_alerts(&c, 10, 0, 0).unwrap()[0].kind, Kind::Alert);
        // The column has the default `alert`.
        c.execute(
            "INSERT INTO alerts (id, target_id, title, source, idempotency_key, created_at)
             VALUES ('x', 'infra', 't', 's', 'k2', 1)",
            [],
        )
        .unwrap();
        let kind: String = c
            .query_row("SELECT kind FROM alerts WHERE id = 'x'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(kind, "alert");
    }

    #[test]
    fn the_counts_of_the_graph_group_the_alerts_by_bucket_and_group() {
        let mut c = mem();
        for (key, group, at) in [
            ("a", "infra", 1_000),
            ("b", "infra", 1_900),
            ("c", "infra", 2_100),
            ("d", "other", 2_200),
            ("e", "infra", 10),
        ] {
            let mut alert = alert(key);
            alert.target = Target::Group(group.into());
            insert_alert(&mut c, &alert).unwrap();
            c.execute(
                "UPDATE alerts SET created_at = ?1 WHERE idempotency_key = ?2",
                params![at, key],
            )
            .unwrap();
        }
        // Buckets of 1,000 ms from 1,000: the alert at 10 is before the range.
        let counts = alert_counts(&c, 1_000, 1_000).unwrap();
        let text: Vec<(i64, &str, i64)> = counts
            .iter()
            .map(|n| (n.bucket, n.group.as_str(), n.count))
            .collect();
        assert_eq!(
            text,
            vec![(0, "infra", 2), (1, "infra", 1), (1, "other", 1)]
        );
        assert!(alert_counts(&c, 100_000, 1_000).unwrap().is_empty());
    }

    #[test]
    fn a_row_with_an_unknown_kind_is_an_error_and_not_a_message() {
        let mut c = mem();
        let a = insert_device(&c, "a", "a", &[1; 32], "ip").unwrap();
        set_subscription(&c, &a, "infra").unwrap();
        insert_alert(&mut c, &alert("k")).unwrap();
        c.execute("UPDATE alerts SET kind = 'question'", [])
            .unwrap();
        assert!(pending_deliveries(&c, &a, 10, 0).is_err());
        assert!(recent_alerts(&c, 10, 0, 0).is_err());
    }

    #[test]
    fn idempotency_key_deduplicates_and_detects_conflicts() {
        let mut c = mem();
        let a = insert_device(&c, "a", "a", &[1; 32], "ip").unwrap();
        set_subscription(&c, &a, "infra").unwrap();
        let InsertOutcome::Created { alert_id, .. } = insert_alert(&mut c, &alert("k")).unwrap()
        else {
            panic!()
        };
        let InsertOutcome::Duplicate {
            alert_id: again, ..
        } = insert_alert(&mut c, &alert("k")).unwrap()
        else {
            panic!()
        };
        assert_eq!(alert_id, again);
        let mut other = alert("k");
        other.title = "different".into();
        assert!(matches!(
            insert_alert(&mut c, &other).unwrap(),
            InsertOutcome::Conflict
        ));
        assert_eq!(pending_deliveries(&c, &a, 10, 0).unwrap().len(), 1);
    }

    #[test]
    fn a_new_title_of_the_same_webhook_alert_is_a_repeat_and_the_first_title_stays() {
        let mut c = mem();
        let mut first = alert("k");
        first.source = format!("{WEBHOOK_SOURCE}ci");
        first.title = "[FIRING] Load: load 0.91".into();
        let InsertOutcome::Created { alert_id, .. } = insert_alert(&mut c, &first).unwrap() else {
            panic!()
        };
        let mut later = first.clone();
        later.title = "[FIRING] Load: load 0.97".into();
        let InsertOutcome::Duplicate {
            alert_id: again, ..
        } = insert_alert(&mut c, &later).unwrap()
        else {
            panic!("a new summary of the same alert is a repeat")
        };
        assert_eq!(again, alert_id);
        assert_eq!(
            recent_alerts(&c, 10, 0, 0).unwrap()[0].title,
            "[FIRING] Load: load 0.91"
        );
        // The key names one target: another group with the same key is still a conflict.
        let mut elsewhere = later;
        elsewhere.target = Target::Group("other".into());
        assert!(matches!(
            insert_alert(&mut c, &elsewhere).unwrap(),
            InsertOutcome::Conflict
        ));
    }

    #[test]
    fn acks_stop_redelivery_and_are_idempotent() {
        let mut c = mem();
        let a = insert_device(&c, "a", "a", &[1; 32], "ip").unwrap();
        set_subscription(&c, &a, "infra").unwrap();
        let InsertOutcome::Created { alert_id, .. } = insert_alert(&mut c, &alert("k")).unwrap()
        else {
            panic!()
        };
        let first = ack(&c, &a, &alert_id).unwrap().unwrap();
        assert!(first.first_receipt_latency_ms.is_some());
        let second = ack(&c, &a, &alert_id).unwrap().unwrap();
        assert!(second.first_receipt_latency_ms.is_none());
        assert!(pending_deliveries(&c, &a, 10, 0).unwrap().is_empty());
        assert!(ack(&c, &a, "nope").unwrap().is_none());
        let d = alert_detail(&c, &alert_id, 0).unwrap().unwrap();
        assert_eq!((d.summary.targets, d.summary.received), (1, 1));
    }

    #[test]
    fn undelivered_alerts_expire_after_the_delivery_ttl() {
        let mut c = mem();
        let a = insert_device(&c, "a", "a", &[1; 32], "ip").unwrap();
        set_subscription(&c, &a, "infra").unwrap();
        let InsertOutcome::Created { alert_id, .. } = insert_alert(&mut c, &alert("k")).unwrap()
        else {
            panic!()
        };
        let before = now_ms() - 1_000;
        assert_eq!(
            pending_deliveries(&c, &a, 10, before).unwrap().len(),
            1,
            "still deliverable"
        );
        assert_eq!(pending_stats(&c, before).unwrap().0, 1);
        let after = now_ms() + 1_000;
        assert!(
            pending_deliveries(&c, &a, 10, after).unwrap().is_empty(),
            "expired: not sent"
        );
        assert_eq!(pending_stats(&c, after).unwrap().0, 0);
        let d = alert_detail(&c, &alert_id, after).unwrap().unwrap();
        assert_eq!(d.summary.expired, 1);
        assert!(d.deliveries[0].expired);
        let d = alert_detail(&c, &alert_id, before).unwrap().unwrap();
        assert_eq!(d.summary.expired, 0);
    }

    #[test]
    fn purge_removes_history_past_retention() {
        let mut c = mem();
        let a = insert_device(&c, "a", "a", &[1; 32], "ip").unwrap();
        set_subscription(&c, &a, "infra").unwrap();
        insert_alert(&mut c, &alert("k")).unwrap();
        let past = now_ms() - 1_000_000;
        assert_eq!(purge(&mut c, past, past).unwrap().0, 0);
        let future = now_ms() + 1_000_000;
        assert_eq!(purge(&mut c, future, future).unwrap().0, 1);
    }

    const DIGEST_A: [u8; 32] = [0xa1; 32];
    const DIGEST_B: [u8; 32] = [0xb2; 32];
    const MINUTE_MS: i64 = 60_000;

    const LIMITS: DeviceLimits = DeviceLimits {
        max_devices: 10,
        max_per_account: 10,
    };

    fn redeem(c: &mut Connection, digest: &[u8], now: i64, key: u8) -> Redeem {
        redeem_code(c, digest, now, LIMITS, "phone", &[key; 32], "1.1.1.1").unwrap()
    }

    #[test]
    fn a_code_works_one_time_and_gives_its_login_to_the_device() {
        let mut c = mem();
        let now = now_ms();
        let created =
            insert_code(&mut c, &DIGEST_A, "alice", now, 10 * MINUTE_MS, 5, false).unwrap();
        assert_eq!(
            created,
            CodeInsert::Created {
                expires_at_ms: now + 10 * MINUTE_MS
            }
        );
        let Redeem::Enrolled { device_id, login } = redeem(&mut c, &DIGEST_A, now + 1, 1) else {
            panic!("the code must work")
        };
        assert_eq!(login, "alice");
        let device = find_device(&c, &device_id).unwrap().unwrap();
        assert_eq!(
            (device.user_name.as_str(), device.name.as_str()),
            ("alice", "phone")
        );
        assert_eq!(device.public_key, vec![1; 32]);
        assert!(!device.revoked);
        // The second use is refused and stores nothing.
        assert_eq!(
            redeem(&mut c, &DIGEST_A, now + 2, 2),
            Redeem::Refused(Refusal::Used)
        );
        assert_eq!(count_active_devices(&c).unwrap(), 1);
        // An unknown digest is refused too.
        assert_eq!(
            redeem(&mut c, &DIGEST_B, now + 2, 3),
            Redeem::Refused(Refusal::Unknown)
        );
    }

    #[test]
    fn an_expired_code_is_refused_and_a_refusal_changes_nothing() {
        let mut c = mem();
        let now = now_ms();
        insert_code(&mut c, &DIGEST_A, "alice", now, MINUTE_MS, 5, false).unwrap();
        assert_eq!(
            redeem(&mut c, &DIGEST_A, now + MINUTE_MS, 1),
            Redeem::Refused(Refusal::Expired),
            "the code ends at its expiry time"
        );
        assert_eq!(count_active_devices(&c).unwrap(), 0);
        let used: Option<i64> = c
            .query_row("SELECT used_at_ms FROM enrollment_codes", [], |r| r.get(0))
            .unwrap();
        assert!(used.is_none(), "an expired code is not marked as used");
        // A code that works up to the last millisecond.
        assert!(matches!(
            redeem(&mut c, &DIGEST_A, now + MINUTE_MS - 1, 1),
            Redeem::Enrolled { .. }
        ));
    }

    #[test]
    fn the_device_limit_leaves_the_code_unused() {
        let mut c = mem();
        let now = now_ms();
        insert_code(&mut c, &DIGEST_A, "alice", now, 10 * MINUTE_MS, 5, false).unwrap();
        let none = DeviceLimits {
            max_devices: 0,
            ..LIMITS
        };
        let full = redeem_code(&mut c, &DIGEST_A, now, none, "phone", &[1; 32], "ip").unwrap();
        assert_eq!(full, Redeem::TooManyDevices);
        assert!(matches!(
            redeem(&mut c, &DIGEST_A, now, 1),
            Redeem::Enrolled { .. }
        ));
    }

    /// One account cannot use all the places of the server: its phone above `max_per_account` is
    /// refused, the code stays unused, and a code of another account still enrolls. A revoked phone
    /// gives its place back.
    #[test]
    fn the_device_limit_of_an_account_leaves_room_for_the_other_accounts() {
        let mut c = mem();
        let now = now_ms();
        let limits = DeviceLimits {
            max_devices: 10,
            max_per_account: 2,
        };
        let mut phones = Vec::new();
        for i in 0..2u8 {
            insert_code(&mut c, &[i; 32], "mallory", now, 10 * MINUTE_MS, 5, false).unwrap();
            let Redeem::Enrolled { device_id, .. } =
                redeem_code(&mut c, &[i; 32], now, limits, "phone", &[i + 1; 32], "ip").unwrap()
            else {
                panic!("phone {i} of the account must enroll")
            };
            phones.push(device_id);
        }
        insert_code(&mut c, &DIGEST_A, "mallory", now, 10 * MINUTE_MS, 5, false).unwrap();
        assert_eq!(
            redeem_code(&mut c, &DIGEST_A, now, limits, "phone", &[9; 32], "ip").unwrap(),
            Redeem::TooManyAccountDevices
        );
        let used: Option<i64> = c
            .query_row(
                "SELECT used_at_ms FROM enrollment_codes WHERE digest = ?1",
                params![&DIGEST_A[..]],
                |r| r.get(0),
            )
            .unwrap();
        assert!(used.is_none(), "the refused code stays unused");
        assert_eq!(count_active_devices(&c).unwrap(), 2);
        // Another account still enrolls: the server has free places.
        insert_code(&mut c, &DIGEST_B, "victor", now, 10 * MINUTE_MS, 5, false).unwrap();
        assert!(matches!(
            redeem_code(&mut c, &DIGEST_B, now, limits, "phone", &[10; 32], "ip").unwrap(),
            Redeem::Enrolled { login, .. } if login == "victor"
        ));
        // A revoked phone of the account gives its place back to the account.
        assert!(revoke_device(&mut c, &phones[0]).unwrap());
        assert!(matches!(
            redeem_code(&mut c, &DIGEST_A, now, limits, "phone", &[9; 32], "ip").unwrap(),
            Redeem::Enrolled { login, .. } if login == "mallory"
        ));
    }

    #[test]
    fn an_account_has_a_limited_number_of_active_codes() {
        let mut c = mem();
        let now = now_ms();
        for i in 0..2u8 {
            let created = insert_code(&mut c, &[i; 32], "alice", now, MINUTE_MS, 2, false).unwrap();
            assert!(matches!(created, CodeInsert::Created { .. }));
        }
        assert_eq!(
            insert_code(&mut c, &[9; 32], "alice", now, MINUTE_MS, 2, false).unwrap(),
            CodeInsert::TooMany
        );
        // Another account has its own count.
        assert!(matches!(
            insert_code(&mut c, &[8; 32], "bob", now, MINUTE_MS, 2, false).unwrap(),
            CodeInsert::Created { .. }
        ));
        // A used code and an expired code do not count.
        assert!(matches!(
            redeem(&mut c, &[0; 32], now, 1),
            Redeem::Enrolled { .. }
        ));
        assert!(matches!(
            insert_code(&mut c, &[7; 32], "alice", now, MINUTE_MS, 2, false).unwrap(),
            CodeInsert::Created { .. }
        ));
        assert_eq!(
            insert_code(&mut c, &[6; 32], "alice", now, MINUTE_MS, 2, false).unwrap(),
            CodeInsert::TooMany
        );
        assert!(matches!(
            insert_code(
                &mut c,
                &[6; 32],
                "alice",
                now + 2 * MINUTE_MS,
                MINUTE_MS,
                2,
                false
            )
            .unwrap(),
            CodeInsert::Created { .. }
        ));
        // The same digest twice is a storage error, not a second code.
        assert!(insert_code(&mut c, &[7; 32], "bob", now, MINUTE_MS, 9, false).is_err());
    }

    #[test]
    fn a_deleted_account_loses_its_unused_codes_and_its_devices() {
        let mut c = mem();
        let now = now_ms();
        insert_code(&mut c, &DIGEST_A, "alice", now, 10 * MINUTE_MS, 5, false).unwrap();
        insert_code(&mut c, &DIGEST_B, "alice", now, 10 * MINUTE_MS, 5, false).unwrap();
        insert_code(&mut c, &[3; 32], "bob", now, 10 * MINUTE_MS, 5, false).unwrap();
        let Redeem::Enrolled { device_id: a, .. } = redeem(&mut c, &DIGEST_A, now, 1) else {
            panic!()
        };
        let Redeem::Enrolled { device_id: b, .. } = redeem(&mut c, &[3; 32], now, 2) else {
            panic!()
        };
        set_subscription(&c, &a, "infra").unwrap();
        let revoked = end_account(&mut c, "alice").unwrap();
        assert_eq!(revoked, vec![a.clone()]);
        assert!(find_device(&c, &a).unwrap().unwrap().revoked);
        assert!(subscriptions(&c, &a).unwrap().is_empty());
        assert!(
            !find_device(&c, &b).unwrap().unwrap().revoked,
            "another account is not touched"
        );
        // The unused code is gone: it is unknown now.
        assert_eq!(
            redeem(&mut c, &DIGEST_B, now, 4),
            Redeem::Refused(Refusal::Unknown)
        );
        // A second call finds nothing to do.
        assert!(end_account(&mut c, "alice").unwrap().is_empty());
    }

    /// The audit finding portal/inbox/login-reuse-inherits-login-keyed-data: the rows that name the
    /// login of an ended account do not pass to a later account with the same login. The rows of
    /// another account stay as they are.
    #[test]
    fn the_rows_of_an_ended_account_do_not_pass_to_a_new_account_with_the_same_login() {
        let mut c = mem();
        let erin = insert_device(&c, "erin-phone", "erin", &[1; 32], "ip").unwrap();
        let bob = insert_device(&c, "bob-phone", "bob", &[2; 32], "ip").unwrap();
        put(&c, "to-erin", "user", "erin", 100);
        put(&c, "to-bob", "user", "bob", 110);
        // An alert that erin sent to bob with the form, and one that bob sent.
        for (id, sender, at) in [("from-erin", "erin", 120), ("from-bob", "bob", 125)] {
            c.execute(
                "INSERT INTO alerts (id, target_kind, target_id, title, source, idempotency_key, created_at)
                 VALUES (?1, 'user', 'bob', ?1, ?2, 'k1', ?3)",
                params![id, admin_source(sender), at],
            )
            .unwrap();
        }
        let s = scope(Some(&[]), "erin");
        notify_set_level(&c, "erin", Level::Off, 0).unwrap();
        assert_eq!(ids(&notify_view(&c, &s, &ASK, 0).unwrap()), ["to-erin"]);

        assert_eq!(end_account(&mut c, "erin").unwrap(), vec![erin.clone()]);

        // A new account erin: no alert to it, no own phone, and its own notification state.
        let view = notify_view(&c, &s, &ASK, 0).unwrap();
        assert!(view.items.is_empty(), "{view:?}");
        assert_eq!(view.prefs.level, Level::All);
        let old = find_device(&c, &erin).unwrap().unwrap();
        assert!(old.revoked);
        assert_ne!(old.user_name, "erin");
        assert!(old.user_name.starts_with("erin:ended:"));
        assert!(crate::users::normalize_login(&old.user_name).is_err());
        let source: String = c
            .query_row(
                "SELECT source FROM alerts WHERE id = 'from-erin'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_ne!(source, admin_source("erin"));
        assert!(super::super::target::account_of_source(&source).is_some_and(|l| l != "erin"));
        // The new erin can use the key of the old one again.
        c.execute(
            "INSERT INTO alerts (id, target_kind, target_id, title, source, idempotency_key, created_at)
             VALUES ('again', 'user', 'bob', 'again', ?1, 'k1', 130)",
            params![admin_source("erin")],
        )
        .unwrap();
        // Bob keeps his phone, his alert, and the alerts that he received and sent.
        assert_eq!(find_device(&c, &bob).unwrap().unwrap().user_name, "bob");
        let bob_view = notify_view(&c, &scope(Some(&[]), "bob"), &ASK, 0).unwrap();
        assert_eq!(ids(&bob_view), ["again", "from-bob", "from-erin", "to-bob"]);
        let bob_source: String = c
            .query_row("SELECT source FROM alerts WHERE id = 'from-bob'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(bob_source, admin_source("bob"));
        // Two ends of one login give two names.
        assert_ne!(ended_name("erin"), ended_name("erin"));
    }

    /// The audit finding portal/inbox/enrollment-code-outlives-account-delete-race: a code of an
    /// account of the database exists only while the row of the account exists. A code that came
    /// before the delete goes with the end of the account that follows the delete.
    #[test]
    fn a_code_of_a_database_account_needs_the_row_of_the_account() {
        let mut c = mem();
        c.execute_batch("CREATE TABLE users (login TEXT NOT NULL UNIQUE);")
            .unwrap();
        c.execute("INSERT INTO users (login) VALUES ('eve')", [])
            .unwrap();
        let now = now_ms();
        assert!(matches!(
            insert_code(&mut c, &DIGEST_A, "eve", now, MINUTE_MS, 5, true).unwrap(),
            CodeInsert::Created { .. }
        ));
        // The portal deletes the row, then ends the inbox state of the account.
        c.execute("DELETE FROM users WHERE login = 'eve'", [])
            .unwrap();
        assert_eq!(
            insert_code(&mut c, &DIGEST_B, "eve", now, MINUTE_MS, 5, true).unwrap(),
            CodeInsert::NoAccount
        );
        end_account(&mut c, "eve").unwrap();
        for digest in [DIGEST_A, DIGEST_B] {
            assert_eq!(
                redeem(&mut c, &digest, now, 1),
                Redeem::Refused(Refusal::Unknown)
            );
        }
        // An account of the file or the superadmin has no row: the check does not apply.
        assert!(matches!(
            insert_code(&mut c, &[5; 32], "dave", now, MINUTE_MS, 5, false).unwrap(),
            CodeInsert::Created { .. }
        ));
    }

    #[test]
    fn the_retention_removes_the_codes_that_expired_long_ago() {
        let mut c = mem();
        let now = now_ms();
        insert_code(
            &mut c,
            &[1; 32],
            "alice",
            now - 3 * CODE_KEEP_MS,
            MINUTE_MS,
            5,
            false,
        )
        .unwrap();
        insert_code(
            &mut c,
            &[2; 32],
            "alice",
            now - CODE_KEEP_MS / 2,
            MINUTE_MS,
            5,
            false,
        )
        .unwrap();
        insert_code(&mut c, &[3; 32], "alice", now, MINUTE_MS, 5, false).unwrap();
        assert_eq!(purge(&mut c, 0, 0).unwrap().2, 1);
        let left: i64 = c
            .query_row("SELECT COUNT(*) FROM enrollment_codes", [], |r| r.get(0))
            .unwrap();
        assert_eq!(left, 2);
    }

    #[test]
    fn an_unused_code_can_be_withdrawn_a_used_code_stays() {
        let mut c = mem();
        let now = now_ms();
        insert_code(&mut c, &DIGEST_A, "alice", now, MINUTE_MS, 5, false).unwrap();
        assert!(delete_code(&c, &DIGEST_A).unwrap());
        assert!(!delete_code(&c, &DIGEST_A).unwrap());
        insert_code(&mut c, &DIGEST_B, "alice", now, MINUTE_MS, 5, false).unwrap();
        assert!(matches!(
            redeem(&mut c, &DIGEST_B, now, 1),
            Redeem::Enrolled { .. }
        ));
        assert!(
            !delete_code(&c, &DIGEST_B).unwrap(),
            "the used code is the proof of the single use"
        );
    }

    #[test]
    fn the_inbox_chain_has_its_own_version_table_and_refuses_a_newer_schema() {
        let mut c = Connection::open_in_memory().unwrap();
        // The chain of the portal keeps its version in this pragma.
        c.execute_batch("PRAGMA user_version = 6;").unwrap();
        migrate(&mut c).unwrap();
        assert_eq!(schema_version(&c).unwrap(), SCHEMA_VERSION);
        let portal_version: i64 = c
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(
            portal_version, 6,
            "the inbox does not touch the version of the portal"
        );
        // A second start does nothing.
        migrate(&mut c).unwrap();
        assert_eq!(schema_version(&c).unwrap(), SCHEMA_VERSION);
        c.execute("UPDATE inbox_schema_version SET version = 99", [])
            .unwrap();
        let error = migrate(&mut c).unwrap_err().to_string();
        assert!(
            error.contains("version 99") && error.contains("understands"),
            "{error}"
        );
    }

    #[test]
    fn the_portal_and_the_inbox_share_one_sqlite_file_in_any_order() {
        let dir = std::env::temp_dir().join(format!(
            "portal-inbox-db-{}-{}",
            std::process::id(),
            random_id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        for portal_first in [true, false] {
            let path = dir.join(format!("shared-{portal_first}.db"));
            let open_portal = || crate::store::Store::open(&path).unwrap();
            let (portal, inbox) = if portal_first {
                let portal = open_portal();
                (portal, Db::open(&path).unwrap())
            } else {
                let inbox = Db::open(&path).unwrap();
                (open_portal(), inbox)
            };
            // Each side reads and writes its own tables in the same file.
            assert!(portal.load_users().unwrap().is_empty());
            let (db, _closed) = inbox;
            let id = tokio::runtime::Builder::new_current_thread()
                .build()
                .unwrap()
                .block_on(db.call(|c| Ok(insert_device(c, "phone", "pseudonym", &[7; 32], "ip")?)))
                .unwrap();
            assert_eq!(id.len(), 32);
            let versions: (i64, i64) = rusqlite::Connection::open(&path)
                .unwrap()
                .query_row(
                    "SELECT (SELECT COALESCE(MAX(version), 0) FROM inbox_schema_version),
                            (SELECT user_version FROM pragma_user_version)",
                    [],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .unwrap();
            assert_eq!(versions.0, SCHEMA_VERSION);
            assert!(versions.1 > 0, "the portal wrote its own version");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn user_alert(key: &str, login: &str) -> NewAlert {
        NewAlert {
            target: Target::User(login.into()),
            ..alert(key)
        }
    }

    #[test]
    fn an_alert_for_an_account_goes_to_each_active_phone_of_the_account() {
        let mut c = mem();
        let enroll = |c: &Connection, n: u8, login: &str| {
            insert_device(c, &format!("p{n}"), login, &[n; 32], "ip").unwrap()
        };
        let (a1, a2, a3) = (
            enroll(&c, 1, "alice"),
            enroll(&c, 2, "alice"),
            enroll(&c, 3, "alice"),
        );
        let bob = enroll(&c, 4, "bob");
        // The account has no subscription: a direct alert does not need one.
        revoke_device(&mut c, &a3).unwrap();
        let InsertOutcome::Created { devices, .. } =
            insert_alert(&mut c, &user_alert("k", "alice")).unwrap()
        else {
            panic!("created")
        };
        let mut expected = vec![a1.clone(), a2.clone()];
        expected.sort();
        assert_eq!(devices, expected);
        assert!(pending_deliveries(&c, &bob, 10, 0).unwrap().is_empty());
        assert!(pending_deliveries(&c, &a3, 10, 0).unwrap().is_empty());
        let pending = pending_deliveries(&c, &a1, 10, 0).unwrap();
        assert_eq!(pending[0].target, Target::User("alice".into()));
        let (kind, id): (String, String) = c
            .query_row("SELECT target_kind, target_id FROM alerts", [], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .unwrap();
        assert_eq!((kind.as_str(), id.as_str()), ("user", "alice"));
        let summary = &recent_alerts(&c, 10, 0, 0).unwrap()[0];
        assert_eq!(summary.target, Target::User("alice".into()));
        assert_eq!(summary.targets, 2);
    }

    #[test]
    fn an_account_with_no_active_phone_gets_no_alert_and_nothing_is_stored() {
        let mut c = mem();
        let phone = insert_device(&c, "p", "alice", &[1; 32], "ip").unwrap();
        revoke_device(&mut c, &phone).unwrap();
        for login in ["alice", "nobody"] {
            assert!(matches!(
                insert_alert(&mut c, &user_alert("k", login)).unwrap(),
                InsertOutcome::NoPhone
            ));
        }
        let stored: i64 = c
            .query_row("SELECT COUNT(*) FROM alerts", [], |r| r.get(0))
            .unwrap();
        assert_eq!(stored, 0);
    }

    #[test]
    fn the_same_key_for_another_target_kind_is_a_conflict_and_a_retry_is_a_duplicate() {
        let mut c = mem();
        insert_device(&c, "p", "infra", &[1; 32], "ip").unwrap();
        assert!(matches!(
            insert_alert(&mut c, &user_alert("k", "infra")).unwrap(),
            InsertOutcome::Created { .. }
        ));
        assert!(matches!(
            insert_alert(&mut c, &user_alert("k", "infra")).unwrap(),
            InsertOutcome::Duplicate { .. }
        ));
        // A group and an account can have the same id: they are two targets.
        assert!(matches!(
            insert_alert(&mut c, &alert("k")).unwrap(),
            InsertOutcome::Conflict
        ));
    }

    #[test]
    fn the_graph_counts_the_alerts_of_groups_only() {
        let mut c = mem();
        insert_device(&c, "p", "infra", &[1; 32], "ip").unwrap();
        insert_alert(&mut c, &user_alert("u", "infra")).unwrap();
        insert_alert(&mut c, &alert("g")).unwrap();
        let counts = alert_counts(&c, 0, 1_000_000_000_000_000).unwrap();
        assert_eq!(counts.len(), 1);
        assert_eq!((counts[0].group.as_str(), counts[0].count), ("infra", 1));
    }

    #[test]
    fn the_active_phones_are_counted_for_each_login() {
        let mut c = mem();
        let a1 = insert_device(&c, "p1", "alice", &[1; 32], "ip").unwrap();
        insert_device(&c, "p2", "alice", &[2; 32], "ip").unwrap();
        insert_device(&c, "p3", "bob", &[3; 32], "ip").unwrap();
        let gone = insert_device(&c, "p4", "carol", &[4; 32], "ip").unwrap();
        revoke_device(&mut c, &gone).unwrap();
        let counts = active_phones_by_login(&c).unwrap();
        assert_eq!(
            counts
                .iter()
                .map(|(l, n)| (l.as_str(), *n))
                .collect::<Vec<_>>(),
            vec![("alice", 2), ("bob", 1)]
        );
        revoke_device(&mut c, &a1).unwrap();
        assert_eq!(active_phones_by_login(&c).unwrap()["alice"], 1);
    }

    #[test]
    fn the_migration_of_the_target_keeps_the_alerts_that_exist_as_alerts_for_a_group() {
        let mut c = Connection::open_in_memory().unwrap();
        c.execute_batch("PRAGMA foreign_keys = ON;").unwrap();
        // The chain up to the version before the change of the target. The migration of the
        // notifications of the web comes after it.
        let before = MIGRATIONS.len() - 2;
        c.execute_batch("CREATE TABLE inbox_schema_version (version INTEGER NOT NULL) STRICT;")
            .unwrap();
        for sql in &MIGRATIONS[..before] {
            c.execute_batch(sql).unwrap();
        }
        c.execute(
            "INSERT INTO inbox_schema_version (version) VALUES (?1)",
            [before as i64],
        )
        .unwrap();
        c.execute(
            "INSERT INTO alerts (id, group_id, title, source, idempotency_key, created_at)
             VALUES ('old', 'infra', 't', 's', 'k', 1)",
            [],
        )
        .unwrap();
        migrate(&mut c).unwrap();
        assert_eq!(schema_version(&c).unwrap(), SCHEMA_VERSION);
        let (kind, id): (String, String) = c
            .query_row(
                "SELECT target_kind, target_id FROM alerts WHERE id = 'old'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!((kind.as_str(), id.as_str()), ("group", "infra"));
        let refused = c.execute(
            "INSERT INTO alerts (id, target_kind, target_id, title, source, idempotency_key, created_at)
             VALUES ('bad', 'team', 'x', 't', 's', 'k2', 1)",
            [],
        );
        assert!(refused.is_err(), "the kind of the target is checked");
    }

    fn put(c: &Connection, id: &str, kind: &str, target: &str, at: i64) {
        c.execute(
            "INSERT INTO alerts (id, target_kind, target_id, title, source, idempotency_key, created_at)
             VALUES (?1, ?2, ?3, ?1, 'producer:test', ?1, ?4)",
            params![id, kind, target, at],
        )
        .unwrap();
    }

    fn scope(groups: Option<&[&str]>, login: &str) -> Scope {
        Scope {
            groups: groups.map(|g| g.iter().map(|s| s.to_string()).collect()),
            login: login.into(),
        }
    }

    const ASK: Ask = Ask {
        since_ms: 0,
        limit: 10,
        skip_if_off: false,
    };

    fn ids(view: &View) -> Vec<&str> {
        view.items.iter().map(|i| i.id.as_str()).collect()
    }

    #[test]
    fn an_account_gets_the_alerts_of_its_groups_and_its_own_and_no_other() {
        let c = mem();
        put(&c, "g-infra", "group", "infra", 100);
        put(&c, "g-other", "group", "other", 110);
        put(&c, "d-alice", "user", "alice", 120);
        put(&c, "d-bob", "user", "bob", 130);
        let member = notify_view(&c, &scope(Some(&["infra"]), "alice"), &ASK, 0).unwrap();
        assert_eq!(ids(&member), ["d-alice", "g-infra"], "newest first");
        // A non-member sees the alert for the account only.
        let outsider = notify_view(&c, &scope(Some(&[]), "carol"), &ASK, 0).unwrap();
        assert!(outsider.items.is_empty() && outsider.unread == 0);
        // The superadmin has all the groups, and not the alerts for the others.
        let all = notify_view(&c, &scope(None, "root"), &ASK, 0).unwrap();
        assert_eq!(ids(&all), ["g-other", "g-infra"]);
        // The store and the rule of the code agree.
        for s in [
            scope(Some(&["infra"]), "alice"),
            scope(Some(&[]), "bob"),
            scope(None, "alice"),
        ] {
            let got = notify_view(&c, &s, &ASK, 0).unwrap();
            for (id, kind, target) in [
                ("g-infra", "group", "infra"),
                ("g-other", "group", "other"),
                ("d-alice", "user", "alice"),
                ("d-bob", "user", "bob"),
            ] {
                let target = Target::from_columns(kind, target.into()).unwrap();
                assert_eq!(ids(&got).contains(&id), s.receives(&target), "{s:?} {id}");
            }
        }
    }

    #[test]
    fn the_cursor_and_the_limit_cut_the_list_and_the_count_follows_the_marker() {
        let c = mem();
        for n in 1..=5 {
            put(&c, &format!("a{n}"), "group", "infra", 1_000 + n * 10);
        }
        let s = scope(Some(&["infra"]), "alice");
        // The first sight of the account sets the marker at `now`: nothing is unread.
        let first = notify_view(&c, &s, &ASK, 5_000).unwrap();
        assert_eq!(first.prefs.read_up_to, 5_000);
        assert_eq!(first.unread, 0);
        assert_eq!(first.items.len(), 5);
        let after = notify_view(
            &c,
            &s,
            &Ask {
                since_ms: 1_030,
                limit: 2,
                skip_if_off: false,
            },
            5_000,
        )
        .unwrap();
        assert_eq!(ids(&after), ["a5", "a4"]);
        put(&c, "a6", "group", "infra", 6_000);
        let later = notify_view(&c, &s, &ASK, 7_000).unwrap();
        assert_eq!(later.unread, 1);
        // The marker moves to the newest alert, and it never goes back.
        assert_eq!(notify_mark_read(&c, &s, 7_000).unwrap(), 6_000);
        assert_eq!(notify_view(&c, &s, &ASK, 7_000).unwrap().unread, 0);
        let empty = scope(Some(&[]), "alice");
        assert_eq!(notify_mark_read(&c, &empty, 7_000).unwrap(), 6_000);
        c.execute("DELETE FROM alerts", []).unwrap();
        put(&c, "old", "group", "infra", 10);
        assert_eq!(notify_mark_read(&c, &s, 7_000).unwrap(), 6_000);
    }

    #[test]
    fn the_unread_count_stops_at_the_cap() {
        let c = mem();
        let s = scope(Some(&["infra"]), "alice");
        notify_view(&c, &s, &ASK, 0).unwrap();
        for n in 0..(UNREAD_CAP + 20) {
            put(&c, &format!("a{n}"), "group", "infra", 10 + i64::from(n));
        }
        assert_eq!(notify_view(&c, &s, &ASK, 0).unwrap().unread, UNREAD_CAP);
    }

    #[test]
    fn the_level_is_stored_for_each_account_and_off_skips_the_reads() {
        let c = mem();
        put(&c, "a", "group", "infra", 100);
        let s = scope(Some(&["infra"]), "alice");
        assert_eq!(
            notify_view(&c, &s, &ASK, 0).unwrap().prefs.level,
            Level::All
        );
        notify_set_level(&c, "alice", Level::Off, 0).unwrap();
        let off = Ask {
            skip_if_off: true,
            ..ASK
        };
        let view = notify_view(&c, &s, &off, 0).unwrap();
        assert_eq!(view.prefs.level, Level::Off);
        assert!(view.items.is_empty());
        assert_eq!(notify_view(&c, &s, &ASK, 0).unwrap().items.len(), 1);
        // Another account keeps the default.
        let bob = scope(Some(&["infra"]), "bob");
        assert_eq!(
            notify_view(&c, &bob, &off, 0).unwrap().prefs.level,
            Level::All
        );
        // A level for an account that has no row creates it.
        notify_set_level(&c, "carol", Level::Important, 77).unwrap();
        let carol = scope(Some(&[]), "carol");
        let prefs = notify_view(&c, &carol, &ASK, 0).unwrap().prefs;
        assert_eq!((prefs.level, prefs.read_up_to), (Level::Important, 77));
        assert!(c
            .execute("UPDATE web_notify SET level = 'loud'", [])
            .is_err());
    }
}
