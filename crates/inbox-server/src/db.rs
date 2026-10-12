//! SQLite storage.
//!
//! One dedicated thread owns the connection; async code submits closures through a
//! bounded channel. With a single writer there is no lock contention, and every
//! confirmed write is durable (WAL, `synchronous = FULL`) before the caller sees
//! success. That is what lets the producer API answer "accepted" only once the
//! alert and all its delivery rows are on disk.

use std::path::Path;

use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::Serialize;
use tokio::sync::{mpsc, oneshot};

use crate::util::{now_ms, random_id};

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
    Closed,
}

impl std::fmt::Display for DbError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DbError::Sqlite(e) => write!(f, "sqlite: {e}"),
            DbError::Pg(e) => write!(f, "postgres: {e}"),
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
];

/// The schema version this binary writes: the number of SQLite migrations.
pub const SCHEMA_VERSION: i64 = MIGRATIONS.len() as i64;

impl Db {
    /// Opens (creating if needed) and migrates the database, then starts its thread.
    pub fn open(path: &Path) -> Result<(Db, Closed), String> {
        if let Some(dir) = path.parent()
            && !dir.as_os_str().is_empty()
            && !dir.is_dir()
        {
            return Err(format!("storage.path: directory {} does not exist", dir.display()));
        }
        let mut conn = Connection::open(path)
            .map_err(|e| format!("storage.path: cannot open {}: {e}", path.display()))?;
        configure(&mut conn).map_err(|e| format!("storage: {e}"))?;
        migrate(&mut conn).map_err(|e| format!("storage migration: {e}"))?;
        let closed = conn
            .execute(
                "UPDATE connections SET disconnected_at = ?1, close_reason = 'server_restart'
                 WHERE disconnected_at IS NULL",
                params![now_ms()],
            )
            .map_err(|e| format!("storage: {e}"))?;
        if closed > 0 {
            tracing::info!(event = "stale_connections_closed", count = closed);
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
                    tracing::warn!(event = "wal_checkpoint_failed", error = %e);
                }
                drop(conn);
                tracing::info!(event = "database_closed");
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

fn migrate(conn: &mut Connection) -> rusqlite::Result<()> {
    let current: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    if current as usize > MIGRATIONS.len() {
        return Err(rusqlite::Error::InvalidParameterName(format!(
            "database schema version {current} is newer than this binary ({})",
            MIGRATIONS.len()
        )));
    }
    for (i, sql) in MIGRATIONS.iter().enumerate().skip(current as usize) {
        let tx = conn.transaction_with_behavior(TransactionBehavior::Exclusive)?;
        tx.execute_batch(sql)?;
        tx.pragma_update(None, "user_version", (i + 1) as i64)?;
        tx.commit()?;
        tracing::info!(event = "migration_applied", version = i + 1);
    }
    Ok(())
}

// ---------------------------------------------------------------- devices

pub struct DeviceKey {
    pub public_key: Vec<u8>,
    pub revoked: bool,
    pub name: String,
    pub user_name: String,
}

pub fn count_active_devices(conn: &Connection) -> rusqlite::Result<u32> {
    conn.query_row("SELECT COUNT(*) FROM devices WHERE revoked_at IS NULL", [], |r| r.get(0))
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

pub fn set_user_name(conn: &Connection, device_id: &str, user_name: &str) -> rusqlite::Result<()> {
    conn.execute("UPDATE devices SET user_name = ?2 WHERE id = ?1", params![device_id, user_name])?;
    // The open connection is attributed to the new pseudonym too.
    conn.execute(
        "UPDATE connections SET user_name = ?2 WHERE device_id = ?1 AND disconnected_at IS NULL",
        params![device_id, user_name],
    )?;
    Ok(())
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
    tx.execute("DELETE FROM subscriptions WHERE device_id = ?1", params![id])?;
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
    conn.execute("UPDATE devices SET last_seen_at = ?2 WHERE id = ?1", params![device_id, now])?;
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

pub struct NewAlert {
    pub group: String,
    pub title: String,
    pub source: String,
    pub idempotency_key: String,
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
}

/// Stores the alert and one delivery row per subscribed, non-revoked device, in a
/// single transaction. Devices subscribed at this instant are the targets, whether
/// they are online or not.
pub fn insert_alert(conn: &mut Connection, alert: &NewAlert) -> rusqlite::Result<InsertOutcome> {
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let existing = tx
        .query_row(
            "SELECT id, group_id, title FROM alerts WHERE source = ?1 AND idempotency_key = ?2",
            params![alert.source, alert.idempotency_key],
            |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?)),
        )
        .optional()?;
    if let Some((id, group, title)) = existing {
        if group != alert.group || title != alert.title {
            return Ok(InsertOutcome::Conflict);
        }
        let devices = delivery_devices(&tx, &id)?;
        return Ok(InsertOutcome::Duplicate { alert_id: id, devices });
    }
    let id = random_id();
    let now = now_ms();
    tx.execute(
        "INSERT INTO alerts (id, group_id, title, source, idempotency_key, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![id, alert.group, alert.title, alert.source, alert.idempotency_key, now],
    )?;
    tx.execute(
        "INSERT INTO deliveries (alert_id, device_id)
         SELECT ?1, s.device_id FROM subscriptions s
         JOIN devices d ON d.id = s.device_id
         WHERE s.group_id = ?2 AND d.revoked_at IS NULL
         ORDER BY s.device_id",
        params![id, alert.group],
    )?;
    let devices = delivery_devices(&tx, &id)?;
    tx.commit()?;
    Ok(InsertOutcome::Created { alert_id: id, devices })
}

fn delivery_devices(conn: &Connection, alert_id: &str) -> rusqlite::Result<Vec<String>> {
    let mut stmt = conn.prepare_cached(
        "SELECT device_id FROM deliveries WHERE alert_id = ?1 ORDER BY device_id",
    )?;
    let rows = stmt.query_map(params![alert_id], |r| r.get(0))?;
    rows.collect()
}

pub struct PendingDelivery {
    pub seq: i64,
    pub alert_id: String,
    pub group: String,
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
        "SELECT d.seq, a.id, a.group_id, a.title, a.created_at
         FROM deliveries d JOIN alerts a ON a.id = d.alert_id
         WHERE d.device_id = ?1 AND d.received_at IS NULL AND a.created_at >= ?3
         ORDER BY d.seq LIMIT ?2",
    )?;
    let rows = stmt.query_map(params![device_id, limit, cutoff], |r| {
        Ok(PendingDelivery {
            seq: r.get(0)?,
            alert_id: r.get(1)?,
            group: r.get(2)?,
            title: r.get(3)?,
            created_at: r.get(4)?,
        })
    })?;
    rows.collect()
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
    let Some((seq, not_received, created_at)) = row else { return Ok(None) };
    conn.execute(
        "UPDATE deliveries SET received_at = COALESCE(received_at, ?2) WHERE seq = ?1",
        params![seq, now],
    )?;
    Ok(Some(AckOutcome { seq, first_receipt_latency_ms: not_received.then_some(now - created_at) }))
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
    pub group: String,
    pub title: String,
    pub source: String,
    pub created_at: i64,
    pub targets: i64,
    pub received: i64,
    /// Targets that never received the alert before it expired.
    pub expired: i64,
    /// Pseudonyms of non-revoked targets that have not received the alert yet.
    pub missing: Vec<String>,
}

const ALERT_SUMMARY: &str = "SELECT a.id, a.group_id, a.title, a.source, a.created_at,
        COUNT(d.seq), COUNT(d.received_at)
     FROM alerts a LEFT JOIN deliveries d ON d.alert_id = a.id";

fn alert_summary(r: &rusqlite::Row<'_>) -> rusqlite::Result<AlertSummary> {
    Ok(AlertSummary {
        id: r.get(0)?,
        group: r.get(1)?,
        title: r.get(2)?,
        source: r.get(3)?,
        created_at: r.get(4)?,
        targets: r.get(5)?,
        received: r.get(6)?,
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
    let mut alerts: Vec<AlertSummary> =
        rows.map(|r| r.map(|a| with_expiry(a, cutoff))).collect::<rusqlite::Result<_>>()?;
    for a in &mut alerts {
        a.missing = missing_users(conn, &a.id)?;
    }
    Ok(alerts)
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
    let Some(summary) = conn.query_row(&sql, params![id], alert_summary).optional()? else {
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
    Ok(Some(AlertDetail { summary, deliveries }))
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

// ---------------------------------------------------------------- retention

/// Deletes history past retention. Configuration guarantees that
/// `alert_retention >= delivery_ttl`, so nothing deliverable is ever removed.
pub fn purge(
    conn: &mut Connection,
    alert_cutoff: i64,
    connection_cutoff: i64,
) -> rusqlite::Result<(usize, usize)> {
    let tx = conn.transaction()?;
    tx.execute_batch(
        "CREATE TEMP TABLE IF NOT EXISTS purge_ids (id TEXT PRIMARY KEY); DELETE FROM purge_ids;",
    )?;
    tx.execute(
        "INSERT INTO purge_ids SELECT a.id FROM alerts a WHERE a.created_at < ?1",
        params![alert_cutoff],
    )?;
    tx.execute("DELETE FROM deliveries WHERE alert_id IN (SELECT id FROM purge_ids)", [])?;
    let alerts = tx.execute("DELETE FROM alerts WHERE id IN (SELECT id FROM purge_ids)", [])?;
    let connections = tx.execute(
        "DELETE FROM connections WHERE connected_at < ?1 AND disconnected_at IS NOT NULL",
        params![connection_cutoff],
    )?;
    tx.commit()?;
    Ok((alerts, connections))
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
            group: "infra".into(),
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
    fn idempotency_key_deduplicates_and_detects_conflicts() {
        let mut c = mem();
        let a = insert_device(&c, "a", "a", &[1; 32], "ip").unwrap();
        set_subscription(&c, &a, "infra").unwrap();
        let InsertOutcome::Created { alert_id, .. } = insert_alert(&mut c, &alert("k")).unwrap()
        else {
            panic!()
        };
        let InsertOutcome::Duplicate { alert_id: again, .. } =
            insert_alert(&mut c, &alert("k")).unwrap()
        else {
            panic!()
        };
        assert_eq!(alert_id, again);
        let mut other = alert("k");
        other.title = "different".into();
        assert!(matches!(insert_alert(&mut c, &other).unwrap(), InsertOutcome::Conflict));
        assert_eq!(pending_deliveries(&c, &a, 10, 0).unwrap().len(), 1);
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
        assert_eq!(pending_deliveries(&c, &a, 10, before).unwrap().len(), 1, "still deliverable");
        assert_eq!(pending_stats(&c, before).unwrap().0, 1);
        let after = now_ms() + 1_000;
        assert!(pending_deliveries(&c, &a, 10, after).unwrap().is_empty(), "expired: not sent");
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
}
