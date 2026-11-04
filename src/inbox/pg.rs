//! The PostgreSQL storage of the inbox. Any number of active instances share it.
//!
//! Each operation is one transaction. An alert and its delivery rows are committed together.
//! `INSERT ... ON CONFLICT` makes the idempotency key hold across instances. The instances talk
//! to each other through `LISTEN` and `NOTIFY`: they wake a phone that is connected elsewhere,
//! and they drop a revoked or replaced session. The table `instances` holds a heartbeat: the
//! survivors close the sessions of a lost instance.
//!
//! The connections use plain TCP. The inbox has no TLS stack, and the database is inside the
//! private network. The inbox uses the database of the portal. Its tables have their own chain
//! of migrations, with its own version table `inbox_schema_version`.

use std::collections::HashMap;
use std::error::Error as _;
use std::future::Future;
use std::path::Path;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::RwLock;
use std::time::Duration;

use deadpool_postgres::{
    Manager, ManagerConfig, Object, Pool, PoolError, RecyclingMethod, Runtime,
};
use futures_util::StreamExt;
use inbox_core::proto::Kind;
use tokio::sync::mpsc;
use tokio_postgres::{AsyncMessage, NoTls};

use super::db::{
    AckOutcome, AlertDetail, AlertSummary, CodeInsert, ConnectionView, DbError, DeliveryView,
    DeviceKey, DeviceView, InsertOutcome, NewAlert, PendingDelivery, Redeem, Refusal, CODE_KEEP_MS,
};
use super::hub::LiveInfo;
use super::log;
use super::notify::{Ask, Item, Level, Prefs, Scope, View, UNREAD_CAP};
use super::spool::{DbAck, Entry, Spool};
use super::store::Stats;
use super::target::Target;
use super::util::{now_ms, random_id};

/// An instance that has not updated its heartbeat for this long is lost: its sessions are closed.
const INSTANCE_TTL_MS: i64 = 30_000;
/// At start the database may not be reachable yet (the network of a new pod, a database that is being created or failing over):
/// the connection is retried for this long before giving up.
const STARTUP_RETRY: Duration = Duration::from_secs(60);
/// A lost instance row is forgotten after this long.
const INSTANCE_FORGET_MS: i64 = 3_600_000;
/// Device ids per `inbox_wake` notification (a payload is limited to 8000 bytes).
const WAKE_CHUNK: usize = 100;
/// No call to the database waits longer than this: a primary that does not answer (a lost node drops packets silently) must
/// not hold an alert or a phone back. The next calls fail at once until the probe sees a writable primary again.
const OP_TIMEOUT: Duration = Duration::from_secs(2);
/// A probe of the database gives up after this, for each of its steps.
const PROBE_TIMEOUT: Duration = Duration::from_secs(1);
/// The alerts accepted this long ago or less are kept in memory, to answer a producer's retry during an outage.
const RECENT_ALERTS_MS: i64 = 3_600_000;
const RECENT_ALERTS_MAX: i64 = 5_000;

/// Advisory lock keys: schema changes, enrollment limit and retention must not run twice at once.
const LOCK_SCHEMA: i64 = 0x5041_4745_0001; // does not collide with the locks of the portal (0x7074_6c..)
const LOCK_ENROLL: i64 = 0x5041_4745_0002;
const LOCK_PURGE: i64 = 0x5041_4745_0003;
const LOCK_CODES: i64 = 0x5041_4745_0004;

const SCHEMA: &[&str] = &[
    r#"
CREATE TABLE devices (
    id                TEXT PRIMARY KEY,
    name              TEXT NOT NULL,
    user_name         TEXT NOT NULL,
    public_key        BYTEA NOT NULL UNIQUE,
    enrolled_at       BIGINT NOT NULL,
    enrolled_ip       TEXT NOT NULL,
    app_version       TEXT,
    last_connected_at BIGINT,
    last_seen_at      BIGINT,
    last_ip           TEXT,
    revoked_at        BIGINT
);
CREATE INDEX devices_user ON devices(user_name);

CREATE TABLE subscriptions (
    device_id  TEXT NOT NULL REFERENCES devices(id),
    group_id   TEXT NOT NULL,
    updated_at BIGINT NOT NULL,
    PRIMARY KEY (device_id, group_id)
);

CREATE TABLE alerts (
    id              TEXT PRIMARY KEY,
    group_id        TEXT NOT NULL,
    title           TEXT NOT NULL,
    source          TEXT NOT NULL,
    idempotency_key TEXT NOT NULL,
    created_at      BIGINT NOT NULL,
    UNIQUE (source, idempotency_key)
);
CREATE INDEX alerts_created ON alerts(created_at);

CREATE TABLE deliveries (
    seq           BIGSERIAL PRIMARY KEY,
    alert_id      TEXT NOT NULL REFERENCES alerts(id),
    device_id     TEXT NOT NULL REFERENCES devices(id),
    attempts      BIGINT NOT NULL DEFAULT 0,
    first_sent_at BIGINT,
    last_sent_at  BIGINT,
    received_at   BIGINT,
    UNIQUE (alert_id, device_id)
);
CREATE INDEX deliveries_pending ON deliveries(device_id, seq) WHERE received_at IS NULL;
CREATE INDEX deliveries_device ON deliveries(device_id);

CREATE TABLE instances (
    id           TEXT PRIMARY KEY,
    host         TEXT NOT NULL,
    started_at   BIGINT NOT NULL,
    heartbeat_at BIGINT NOT NULL
);

CREATE TABLE connections (
    id              BIGSERIAL PRIMARY KEY,
    device_id       TEXT NOT NULL REFERENCES devices(id),
    user_name       TEXT NOT NULL,
    ip              TEXT NOT NULL,
    app_version     TEXT NOT NULL,
    connected_at    BIGINT NOT NULL,
    disconnected_at BIGINT,
    close_reason    TEXT,
    instance        TEXT NOT NULL
);
CREATE INDEX connections_device ON connections(device_id, connected_at);
CREATE INDEX connections_time ON connections(connected_at);
CREATE INDEX connections_user ON connections(user_name, connected_at);
CREATE INDEX connections_open ON connections(instance) WHERE disconnected_at IS NULL;
"#,
    r#"
-- Protocol 4: an account of the portal makes a one-time enrollment code. The table keeps the
-- SHA-256 digest of the secret, never the secret. `login` is the account of the code and, later,
-- the user name of the device.
CREATE TABLE enrollment_codes (
    digest        BYTEA PRIMARY KEY,
    login         TEXT NOT NULL,
    created_at_ms BIGINT NOT NULL,
    expires_at_ms BIGINT NOT NULL,
    used_at_ms    BIGINT
);
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
    read_up_to BIGINT NOT NULL,
    level      TEXT NOT NULL DEFAULT 'all' CHECK (level IN ('all', 'important', 'off'))
);
CREATE INDEX alerts_target ON alerts(target_id, created_at);
"#,
];

/// The kind that a stored text names. An unknown text is a storage error.
fn kind_of(text: String) -> Result<Kind, DbError> {
    Kind::parse(&text).ok_or_else(|| DbError::Pg(format!("unknown message kind {text:?}")))
}

/// The target that two stored columns name. An unknown kind is a storage error.
fn target_of(kind: String, id: String) -> Result<Target, DbError> {
    Target::from_columns(&kind, id)
        .ok_or_else(|| DbError::Pg(format!("unknown target kind {kind:?}")))
}

/// The database clock, in milliseconds: liveness is judged by one clock, not by the instances' own.
const DB_NOW: &str = "(extract(epoch FROM clock_timestamp()) * 1000)::bigint";

/// Turns a failure of the driver or of the pool into a [`DbError`]: `Unavailable` when the database cannot be used right now
/// (no connection, a failover in progress, a standby that refuses writes), `Pg` for everything else.
trait Failure {
    fn into_db_error(self) -> DbError;
}

fn db_error(e: impl Failure) -> DbError {
    e.into_db_error()
}

/// SQLSTATEs of a database that is going away or is not a primary: connection exceptions (class 08), admin/crash shutdown,
/// "cannot connect now" (starting up) and "read-only transaction".
fn unavailable_state(code: &str) -> bool {
    code.starts_with("08") || matches!(code, "57P01" | "57P02" | "57P03" | "25006")
}

impl Failure for tokio_postgres::Error {
    fn into_db_error(self) -> DbError {
        let text = self.to_string();
        let down = match self.as_db_error() {
            Some(db) => unavailable_state(db.code().code()),
            // No answer from the server: the connection is closed, broken or never made.
            None => {
                self.is_closed()
                    || self.source().is_some_and(|s| s.is::<std::io::Error>())
                    || text.contains("connect")
                    || text.contains("timeout")
            }
        };
        if down {
            DbError::Unavailable(text)
        } else {
            DbError::Pg(text)
        }
    }
}

impl Failure for PoolError {
    fn into_db_error(self) -> DbError {
        match self {
            PoolError::Backend(e) => e.into_db_error(),
            other => DbError::Unavailable(other.to_string()),
        }
    }
}

/// `Some(true)` on a primary, `Some(false)` on a standby, `None` when the connection does not answer.
async fn is_writable(client: &tokio_postgres::Client) -> Option<bool> {
    let rows = tokio::time::timeout(
        PROBE_TIMEOUT,
        client.simple_query("SELECT pg_is_in_recovery()"),
    )
    .await
    .ok()?
    .ok()?;
    rows.iter().find_map(|m| match m {
        tokio_postgres::SimpleQueryMessage::Row(r) => r.get(0).map(|v| v == "f"),
        _ => None,
    })
}

/// What this instance remembers of the database, to serve phones and accept alerts while it is away.
#[derive(Default)]
struct Cache {
    devices: HashMap<String, CachedDevice>,
    /// Alerts stored in the database lately, by (source, idempotency key): a producer's retry during an outage finds them.
    recent: HashMap<(String, String), RecentAlert>,
}

struct CachedDevice {
    key: DeviceKey,
    subscriptions: Vec<String>,
}

#[derive(Clone)]
pub struct RecentAlert {
    pub id: String,
    pub target: Target,
    pub title: String,
    /// Unix ms: when the alert was stored.
    pub at: i64,
}

/// What the other instances tell this one.
#[derive(Debug, PartialEq, Eq)]
pub enum Event {
    /// New deliveries exist for these devices.
    Wake(Vec<String>),
    /// The device was revoked: close its session.
    Revoked(String),
    /// The device opened a session on another instance at `connected_at`: close an older one here.
    Replaced {
        device: String,
        instance: String,
        connected_at: i64,
    },
    /// The notification connection was lost and is back: anything may have been missed.
    Resync,
}

pub fn parse_notification(channel: &str, payload: &str) -> Option<Event> {
    let mut words = payload.split_whitespace();
    match channel {
        "inbox_wake" => Some(Event::Wake(words.map(str::to_string).collect())),
        "inbox_kick" => match words.next()? {
            "revoked" => Some(Event::Revoked(words.next()?.to_string())),
            "replaced" => Some(Event::Replaced {
                device: words.next()?.to_string(),
                instance: words.next()?.to_string(),
                connected_at: words.next()?.parse().ok()?,
            }),
            _ => None,
        },
        _ => None,
    }
}

/// How the inbox reaches the database of the portal. The values come from the block `postgres`
/// and the block `inbox.storage`.
pub struct PgSettings {
    pub host: String,
    pub port: u16,
    pub database: String,
    pub user: String,
    pub password: zeroize::Zeroizing<String>,
    pub pool_size: usize,
    pub connect_timeout: Duration,
}

pub struct PgStore {
    pool: Pool,
    config: tokio_postgres::Config,
    instance: String,
    /// Since when (Unix ms) the database cannot be used, 0 while it can. Every call fails at once meanwhile.
    down_since: AtomicI64,
    /// The alerts accepted while it could not (`spool.rs`).
    spool: Spool,
    cache: RwLock<Cache>,
    /// A connection kept for the probe, apart from the pool.
    probe_conn: tokio::sync::Mutex<Option<tokio_postgres::Client>>,
}

impl PgStore {
    /// Connects, migrates the schema and registers this instance.
    pub async fn connect(cfg: &PgSettings) -> Result<PgStore, String> {
        let password = &cfg.password;
        let mut config = tokio_postgres::Config::new();
        config
            .host(&cfg.host)
            .port(cfg.port)
            .dbname(&cfg.database)
            .user(&cfg.user)
            .password(password.as_bytes())
            .connect_timeout(cfg.connect_timeout)
            .keepalives(true)
            .keepalives_idle(Duration::from_secs(10))
            .keepalives_interval(Duration::from_secs(2))
            .keepalives_retries(2)
            // A primary on a lost node drops packets without a reset: unanswered data ends the connection after this.
            .tcp_user_timeout(OP_TIMEOUT + Duration::from_secs(1))
            .application_name("inbox");
        let manager = Manager::from_config(
            config.clone(),
            NoTls,
            ManagerConfig {
                recycling_method: RecyclingMethod::Fast,
            },
        );
        let pool = Pool::builder(manager)
            .max_size(cfg.pool_size)
            .runtime(Runtime::Tokio1)
            .wait_timeout(Some(Duration::from_secs(5)))
            .create_timeout(Some(cfg.connect_timeout))
            .recycle_timeout(Some(cfg.connect_timeout))
            .build()
            .map_err(|e| format!("postgres: {e}"))?;
        let store = PgStore {
            pool,
            config,
            instance: random_id(),
            down_since: AtomicI64::new(0),
            spool: Spool::default(),
            cache: RwLock::new(Cache::default()),
            probe_conn: tokio::sync::Mutex::new(None),
        };
        store.wait_until_reachable().await?;
        store
            .migrate()
            .await
            .map_err(|e| format!("inbox postgres migration: {e}"))?;
        store
            .register_instance()
            .await
            .map_err(|e| format!("inbox postgres: {e}"))?;
        log::info!(event = "postgres_ready", instance = %store.instance, host = %cfg.host, database = %cfg.database);
        Ok(store)
    }

    /// Retries the first connection: a pod starts before its network is ready, and the database may be failing over.
    async fn wait_until_reachable(&self) -> Result<(), String> {
        let give_up = tokio::time::Instant::now() + STARTUP_RETRY;
        let mut announced = false;
        loop {
            match self.pool.get().await {
                Ok(_) => return Ok(()),
                Err(e) if tokio::time::Instant::now() < give_up => {
                    if !announced {
                        log::warn!(event = "postgres_unreachable", error = %e, retry_for_s = STARTUP_RETRY.as_secs());
                        announced = true;
                    }
                    tokio::time::sleep(Duration::from_secs(1)).await;
                }
                Err(e) => return Err(format!("postgres: {e}")),
            }
        }
    }

    pub fn instance(&self) -> &str {
        &self.instance
    }

    async fn client(&self) -> Result<Object, DbError> {
        if self.is_down() {
            return Err(DbError::Unavailable("the database is away".into()));
        }
        self.pool.get().await.map_err(db_error)
    }

    pub async fn ping(&self) -> Result<(), DbError> {
        let c = self.client().await?;
        c.query_one("SELECT 1", &[])
            .await
            .map(|_| ())
            .map_err(db_error)
    }

    // ------------------------------------------------------------ degraded mode

    pub fn spool(&self) -> &Spool {
        &self.spool
    }

    /// The database cannot be used right now: the instance runs on its memory (`spool.rs`).
    pub fn is_down(&self) -> bool {
        self.down_since.load(Ordering::Relaxed) != 0
    }

    /// Bounds a call to the database and keeps the breaker: a call that fails because the database is away, or does not
    /// answer in time, makes every following call fail at once until [`PgStore::writable`] finds a primary again.
    pub async fn guard<T>(
        &self,
        op: impl Future<Output = Result<T, DbError>>,
    ) -> Result<T, DbError> {
        if self.is_down() {
            return Err(DbError::Unavailable("the database is away".into()));
        }
        let result = match tokio::time::timeout(OP_TIMEOUT, op).await {
            Ok(r) => r,
            Err(_) => Err(DbError::Unavailable(
                "the database did not answer in time".into(),
            )),
        };
        if let Err(e @ DbError::Unavailable(_)) = &result {
            self.mark_down(e);
        }
        result
    }

    pub fn mark_down(&self, reason: &DbError) {
        if self
            .down_since
            .compare_exchange(0, now_ms(), Ordering::Relaxed, Ordering::Relaxed)
            .is_ok()
        {
            log::warn!(event = "storage_degraded", reason = %reason, spooled = self.spool.len());
            // Whatever the pool holds points at the primary that was lost.
            self.pool.retain(|_, _| false);
        }
    }

    pub fn mark_up(&self) {
        let since = self.down_since.swap(0, Ordering::Relaxed);
        if since != 0 {
            log::info!(
                event = "storage_recovered",
                down_ms = now_ms() - since,
                spooled = self.spool.len()
            );
            self.pool.retain(|_, _| false);
        }
    }

    /// One look at the database, on a connection of its own: is there a writable primary?
    pub async fn writable(&self) -> bool {
        self.writable_primary().await
    }

    async fn writable_primary(&self) -> bool {
        let mut slot = self.probe_conn.lock().await;
        if let Some(client) = slot.as_ref() {
            if let Some(writable) = is_writable(client).await {
                return writable;
            }
            // The connection went stale: make another one.
            *slot = None;
        }
        let Ok(Ok((client, connection))) =
            tokio::time::timeout(PROBE_TIMEOUT, self.config.connect(NoTls)).await
        else {
            return false;
        };
        tokio::spawn(async move {
            let _ = connection.await;
        });
        let answer = is_writable(&client).await;
        if answer.is_some() {
            *slot = Some(client);
        }
        answer.unwrap_or(false)
    }

    /// Reloads what the instance keeps of the database: devices with their subscriptions, and the alerts of the last hour.
    pub async fn refresh_cache(&self) -> Result<(), DbError> {
        let c = self.client().await?;
        let devices = c
            .query(
                "SELECT id, public_key, revoked_at IS NOT NULL, name, user_name FROM devices",
                &[],
            )
            .await
            .map_err(db_error)?;
        let subs = c
            .query(
                "SELECT device_id, group_id FROM subscriptions ORDER BY group_id",
                &[],
            )
            .await
            .map_err(db_error)?;
        let alerts = c
            .query(
                "SELECT source, idempotency_key, id, target_kind, target_id, title, created_at FROM alerts
                 WHERE created_at >= $1 ORDER BY created_at DESC LIMIT $2",
                &[&(now_ms() - RECENT_ALERTS_MS), &RECENT_ALERTS_MAX],
            )
            .await
            .map_err(db_error)?;
        let mut fresh = Cache::default();
        for r in &devices {
            fresh.devices.insert(
                r.get(0),
                CachedDevice {
                    key: DeviceKey {
                        public_key: r.get(1),
                        revoked: r.get(2),
                        name: r.get(3),
                        user_name: r.get(4),
                    },
                    subscriptions: Vec::new(),
                },
            );
        }
        for r in &subs {
            let (device, group): (String, String) = (r.get(0), r.get(1));
            if let Some(d) = fresh.devices.get_mut(&device) {
                d.subscriptions.push(group);
            }
        }
        for r in &alerts {
            fresh.recent.insert(
                (r.get(0), r.get(1)),
                RecentAlert {
                    id: r.get(2),
                    target: target_of(r.get(3), r.get(4))?,
                    title: r.get(5),
                    at: r.get(6),
                },
            );
        }
        // An alert this instance stored while the queries ran is not in their answer: it is kept.
        let horizon = now_ms() - RECENT_ALERTS_MS;
        let mut cache = self.cache.write().expect("cache lock");
        for (k, v) in cache.recent.drain().filter(|(_, v)| v.at >= horizon) {
            fresh.recent.entry(k).or_insert(v);
        }
        *cache = fresh;
        Ok(())
    }

    /// The devices that the target names, as last seen in the database: the devices subscribed to
    /// the group, or the phones of the account. A revoked device is never in the list.
    pub fn cached_targets(&self, target: &Target) -> Vec<String> {
        let cache = self.cache.read().expect("cache lock");
        let mut ids: Vec<String> = cache
            .devices
            .iter()
            .filter(|(_, d)| {
                !d.key.revoked
                    && match target {
                        Target::Group(group) => d.subscriptions.iter().any(|g| g == group),
                        Target::User(login) => d.key.user_name == *login,
                    }
            })
            .map(|(id, _)| id.clone())
            .collect();
        ids.sort();
        ids
    }

    pub fn cached_device(&self, id: &str) -> Option<DeviceKey> {
        self.cache
            .read()
            .expect("cache lock")
            .devices
            .get(id)
            .map(|d| d.key.clone())
    }

    /// Subscriptions and login of a device, as last seen in the database.
    pub fn cached_profile(&self, id: &str) -> Option<(Vec<String>, String)> {
        let cache = self.cache.read().expect("cache lock");
        cache
            .devices
            .get(id)
            .map(|d| (d.subscriptions.clone(), d.key.user_name.clone()))
    }

    pub fn cache_device(&self, id: &str, key: Option<DeviceKey>) {
        let mut cache = self.cache.write().expect("cache lock");
        match key {
            Some(key) => {
                let subscriptions = cache
                    .devices
                    .get(id)
                    .map(|d| d.subscriptions.clone())
                    .unwrap_or_default();
                cache
                    .devices
                    .insert(id.to_string(), CachedDevice { key, subscriptions });
            }
            None => {
                cache.devices.remove(id);
            }
        }
    }

    pub fn cache_subscriptions(&self, id: &str, subscriptions: Vec<String>) {
        if let Some(d) = self.cache.write().expect("cache lock").devices.get_mut(id) {
            d.subscriptions = subscriptions;
        }
    }

    pub fn cache_revoked(&self, id: &str) {
        if let Some(d) = self.cache.write().expect("cache lock").devices.get_mut(id) {
            d.key.revoked = true;
            d.subscriptions.clear();
        }
    }

    pub fn cache_alert(&self, source: &str, key: &str, alert: RecentAlert) {
        let mut cache = self.cache.write().expect("cache lock");
        if cache.recent.len() >= RECENT_ALERTS_MAX as usize * 2 {
            cache.recent.clear();
        }
        cache
            .recent
            .insert((source.to_string(), key.to_string()), alert);
    }

    pub fn cached_alert(&self, source: &str, key: &str) -> Option<RecentAlert> {
        self.cache
            .read()
            .expect("cache lock")
            .recent
            .get(&(source.to_string(), key.to_string()))
            .cloned()
    }

    /// Writes what the instance held while the database was away: the alerts of the spool, then the receipts. Idempotent:
    /// another instance may replay the same alert, and a crash half way replays it again. Returns the alerts written and the
    /// devices that still wait for one (to wake).
    pub async fn replay(&self) -> Result<(usize, Vec<String>), DbError> {
        let mut written = 0;
        let mut wake: Vec<String> = Vec::new();
        for entry in self.spool.snapshot() {
            if let Some(waiting) = self.replay_alert(&entry).await? {
                written += 1;
                wake.extend(waiting);
            }
        }
        let acks = self.spool.take_db_acks();
        if !acks.is_empty() {
            if let Err(e) = self.replay_acks(&acks).await {
                self.spool.restore_db_acks(acks);
                return Err(e);
            }
        }
        wake.sort();
        wake.dedup();
        Ok((written, wake))
    }

    /// One alert of the spool into the database, in one transaction. `Some(devices still waiting)` when it was written.
    async fn replay_alert(&self, e: &Entry) -> Result<Option<Vec<String>>, DbError> {
        let mut c = self.client().await?;
        let tx = c.transaction().await.map_err(db_error)?;
        tx.execute(
            "INSERT INTO alerts (id, target_kind, target_id, title, source, idempotency_key, created_at, kind)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8) ON CONFLICT (source, idempotency_key) DO NOTHING",
            &[
                &e.id,
                &e.target.kind(),
                &e.target.id(),
                &e.title,
                &e.source,
                &e.key,
                &e.created_at,
                &e.kind.as_str(),
            ],
        )
        .await
        .map_err(db_error)?;
        // Another instance (or this one, before the outage) may have stored it under another id.
        let row = tx
            .query_one(
                "SELECT id, target_kind, target_id, title FROM alerts WHERE source = $1 AND idempotency_key = $2",
                &[&e.source, &e.key],
            )
            .await
            .map_err(db_error)?;
        let (stored, kind, target, title): (String, String, String, String) =
            (row.get(0), row.get(1), row.get(2), row.get(3));
        if kind != e.target.kind() || target != e.target.id() || title != e.title {
            log::warn!(event = "spool_alert_conflict", alert.id = %e.id, alert.source = %e.source);
            self.spool.remove_if_unchanged(&e.id, e.rev);
            return Ok(None);
        }
        let devices: Vec<&str> = e.deliveries.iter().map(|d| d.device.as_str()).collect();
        let attempts: Vec<i64> = e.deliveries.iter().map(|d| d.attempts).collect();
        let first: Vec<Option<i64>> = e.deliveries.iter().map(|d| d.first_sent_at).collect();
        let last: Vec<Option<i64>> = e.deliveries.iter().map(|d| d.last_sent_at).collect();
        let received: Vec<Option<i64>> = e.deliveries.iter().map(|d| d.received_at).collect();
        // Revoked or unknown devices are skipped; a delivery that exists keeps what it knows and gains what the spool knows.
        tx.execute(
            "INSERT INTO deliveries (alert_id, device_id, attempts, first_sent_at, last_sent_at, received_at)
             SELECT $1, d.id, v.attempts, v.first_sent, v.last_sent, v.received
             FROM unnest($2::text[], $3::bigint[], $4::bigint[], $5::bigint[], $6::bigint[])
                  AS v(device, attempts, first_sent, last_sent, received)
             JOIN devices d ON d.id = v.device AND d.revoked_at IS NULL
             ORDER BY d.id
             ON CONFLICT (alert_id, device_id) DO UPDATE SET
                 received_at   = COALESCE(deliveries.received_at, EXCLUDED.received_at),
                 attempts      = GREATEST(deliveries.attempts, EXCLUDED.attempts),
                 first_sent_at = COALESCE(deliveries.first_sent_at, EXCLUDED.first_sent_at),
                 last_sent_at  = GREATEST(deliveries.last_sent_at, EXCLUDED.last_sent_at)",
            &[&stored, &devices, &attempts, &first, &last, &received],
        )
        .await
        .map_err(db_error)?;
        let waiting = e.waiting();
        for chunk in waiting.chunks(WAKE_CHUNK) {
            tx.execute("SELECT pg_notify('inbox_wake', $1)", &[&chunk.join(" ")])
                .await
                .map_err(db_error)?;
        }
        tx.commit().await.map_err(db_error)?;
        self.cache_alert(
            &e.source,
            &e.key,
            RecentAlert {
                id: stored,
                target: e.target.clone(),
                title: e.title.clone(),
                at: e.created_at,
            },
        );
        self.spool.remove_if_unchanged(&e.id, e.rev);
        Ok(Some(waiting))
    }

    async fn replay_acks(&self, acks: &[DbAck]) -> Result<(), DbError> {
        let devices: Vec<&str> = acks.iter().map(|a| a.device.as_str()).collect();
        let alerts: Vec<&str> = acks.iter().map(|a| a.alert_id.as_str()).collect();
        let at: Vec<i64> = acks.iter().map(|a| a.at).collect();
        let c = self.client().await?;
        c.execute(
            "UPDATE deliveries d SET received_at = COALESCE(d.received_at, v.at)
             FROM unnest($1::text[], $2::text[], $3::bigint[]) AS v(device, alert, at)
             WHERE d.device_id = v.device AND d.alert_id = v.alert",
            &[&devices, &alerts, &at],
        )
        .await
        .map_err(db_error)?;
        Ok(())
    }

    async fn migrate(&self) -> Result<(), DbError> {
        let mut c = self.client().await?;
        let tx = c.transaction().await.map_err(db_error)?;
        tx.execute("SELECT pg_advisory_xact_lock($1)", &[&LOCK_SCHEMA])
            .await
            .map_err(db_error)?;
        // The portal has its own chain in the table `schema_migrations`: the two chains are independent.
        tx.batch_execute("CREATE TABLE IF NOT EXISTS inbox_schema_version (version BIGINT PRIMARY KEY, applied_at BIGINT NOT NULL)")
            .await
            .map_err(db_error)?;
        let current: i64 = tx
            .query_one(
                "SELECT COALESCE(MAX(version), 0)::bigint FROM inbox_schema_version",
                &[],
            )
            .await
            .map_err(db_error)?
            .get(0);
        if current as usize > SCHEMA.len() {
            return Err(DbError::Pg(format!(
                "the inbox schema is version {current}, this portal understands {}",
                SCHEMA.len()
            )));
        }
        for (i, sql) in SCHEMA.iter().enumerate().skip(current as usize) {
            tx.batch_execute(sql).await.map_err(db_error)?;
            tx.execute(
                "INSERT INTO inbox_schema_version (version, applied_at) VALUES ($1, $2)",
                &[&((i + 1) as i64), &now_ms()],
            )
            .await
            .map_err(db_error)?;
            log::info!(event = "migration_applied", version = i + 1);
        }
        tx.commit().await.map_err(db_error)
    }

    // ------------------------------------------------------------ instances

    async fn register_instance(&self) -> Result<(), DbError> {
        let c = self.client().await?;
        // The host name is for the table of the instances only. The file of the kernel gives it: the
        // inbox reads no environment variable that the configuration does not name.
        let host = std::fs::read_to_string("/proc/sys/kernel/hostname")
            .map(|h| h.trim().to_string())
            .ok()
            .filter(|h| !h.is_empty())
            .unwrap_or_else(|| "unknown".into());
        c.execute(
            &format!("INSERT INTO instances (id, host, started_at, heartbeat_at) VALUES ($1, $2, {DB_NOW}, {DB_NOW})"),
            &[&self.instance, &host],
        )
        .await
        .map_err(db_error)?;
        Ok(())
    }

    /// Heartbeat of this instance, then closes the sessions of the instances that stopped beating. Returns how many.
    pub async fn heartbeat(&self) -> Result<u64, DbError> {
        let mut c = self.client().await?;
        let tx = c.transaction().await.map_err(db_error)?;
        let beat = tx
            .execute(
                &format!("UPDATE instances SET heartbeat_at = {DB_NOW} WHERE id = $1"),
                &[&self.instance],
            )
            .await
            .map_err(db_error)?;
        if beat == 0 {
            // This instance was forgotten (the database was unreachable for longer than the ttl): announce it again.
            tx.execute(
                &format!("INSERT INTO instances (id, host, started_at, heartbeat_at) VALUES ($1, 'unknown', {DB_NOW}, {DB_NOW})"),
                &[&self.instance],
            )
            .await
            .map_err(db_error)?;
        }
        let closed = tx
            .execute(
                &format!(
                    "UPDATE connections SET disconnected_at = {DB_NOW}, close_reason = 'instance_lost'
                     WHERE disconnected_at IS NULL AND instance IN
                       (SELECT id FROM instances WHERE heartbeat_at < {DB_NOW} - $1)"
                ),
                &[&INSTANCE_TTL_MS],
            )
            .await
            .map_err(db_error)?;
        tx.execute(
            &format!("DELETE FROM instances WHERE heartbeat_at < {DB_NOW} - $1 AND id <> $2"),
            &[&INSTANCE_FORGET_MS, &self.instance],
        )
        .await
        .map_err(db_error)?;
        tx.commit().await.map_err(db_error)?;
        Ok(closed)
    }

    /// Clean stop: the sessions still open here are closed and the instance is forgotten.
    pub async fn shutdown(&self) -> Result<(), DbError> {
        let mut c = self.client().await?;
        let tx = c.transaction().await.map_err(db_error)?;
        tx.execute(
            &format!(
                "UPDATE connections SET disconnected_at = {DB_NOW}, close_reason = 'server_restart'
                 WHERE disconnected_at IS NULL AND instance = $1"
            ),
            &[&self.instance],
        )
        .await
        .map_err(db_error)?;
        tx.execute("DELETE FROM instances WHERE id = $1", &[&self.instance])
            .await
            .map_err(db_error)?;
        tx.commit().await.map_err(db_error)
    }

    /// Sessions open on any live instance, by device.
    pub async fn live_sessions(&self) -> Result<HashMap<String, LiveInfo>, DbError> {
        let c = self.client().await?;
        let rows = c
            .query(
                &format!(
                    "SELECT DISTINCT ON (c.device_id) c.device_id, c.ip, c.app_version, c.connected_at,
                            COALESCE(d.last_seen_at, c.connected_at)
                     FROM connections c
                     JOIN instances i ON i.id = c.instance
                     JOIN devices d ON d.id = c.device_id
                     WHERE c.disconnected_at IS NULL AND i.heartbeat_at >= {DB_NOW} - $1
                     ORDER BY c.device_id, c.connected_at DESC"
                ),
                &[&INSTANCE_TTL_MS],
            )
            .await
            .map_err(db_error)?;
        Ok(rows
            .iter()
            .map(|r| {
                (
                    r.get::<_, String>(0),
                    LiveInfo {
                        ip: r.get(1),
                        app_version: r.get(2),
                        connected_at: r.get(3),
                        last_seen_at: r.get(4),
                    },
                )
            })
            .collect())
    }

    // ------------------------------------------------------------ notifications

    /// Receives the notifications of the other instances until `stop` resolves, reconnecting as needed.
    pub async fn listen(
        &self,
        events: mpsc::UnboundedSender<Event>,
        stop: impl std::future::Future<Output = ()>,
    ) {
        tokio::pin!(stop);
        let mut announced_down = false;
        loop {
            match self.listen_once(&events, &mut stop).await {
                Ok(()) => return,
                Err(e) => {
                    if !announced_down {
                        log::warn!(event = "postgres_listen_lost", error = %e);
                        announced_down = true;
                    }
                }
            }
            tokio::select! {
                _ = tokio::time::sleep(Duration::from_secs(1)) => {}
                _ = &mut stop => return,
            }
            if self.ping().await.is_ok() {
                announced_down = false;
            }
        }
    }

    /// One notification connection. `Ok` when `stop` resolved, `Err` when the connection broke.
    async fn listen_once(
        &self,
        events: &mpsc::UnboundedSender<Event>,
        stop: &mut (impl std::future::Future<Output = ()> + Unpin),
    ) -> Result<(), String> {
        let (client, mut connection) = self
            .config
            .connect(NoTls)
            .await
            .map_err(|e| e.to_string())?;
        let (tx, mut rx) = mpsc::unbounded_channel();
        let driver = tokio::spawn(async move {
            let mut messages = futures_util::stream::poll_fn(move |cx| connection.poll_message(cx));
            while let Some(message) = messages.next().await {
                match message {
                    Ok(AsyncMessage::Notification(n)) => {
                        if tx.send(Ok(n)).is_err() {
                            return;
                        }
                    }
                    Ok(_) => {}
                    Err(e) => {
                        let _ = tx.send(Err(e.to_string()));
                        return;
                    }
                }
            }
            let _ = tx.send(Err("the connection closed".to_string()));
        });
        let result = async {
            client
                .batch_execute("LISTEN inbox_wake; LISTEN inbox_kick")
                .await
                .map_err(|e| e.to_string())?;
            // Whatever happened while this connection was down is caught up by one full flush.
            let _ = events.send(Event::Resync);
            let mut probe = tokio::time::interval(Duration::from_secs(20));
            probe.tick().await;
            loop {
                tokio::select! {
                    message = rx.recv() => match message {
                        Some(Ok(n)) => {
                            if let Some(event) = parse_notification(n.channel(), n.payload()) {
                                let _ = events.send(event);
                            }
                        }
                        Some(Err(e)) => return Err(e),
                        None => return Err("the connection closed".to_string()),
                    },
                    _ = probe.tick() => {
                        // A silent connection that died is only noticed by using it.
                        tokio::time::timeout(Duration::from_secs(5), client.simple_query("SELECT 1"))
                            .await
                            .map_err(|_| "the connection does not answer".to_string())?
                            .map_err(|e| e.to_string())?;
                    }
                    _ = &mut *stop => return Ok(()),
                }
            }
        }
        .await;
        driver.abort();
        result
    }

    // ------------------------------------------------------------ devices

    /// Stores the digest of a new code of `login`. Two instances that make codes for one account
    /// at the same time must not both take the last place: the transaction holds a lock.
    pub async fn create_code(
        &self,
        digest: &[u8],
        login: &str,
        ttl_ms: i64,
        max_active: u32,
    ) -> Result<CodeInsert, DbError> {
        let mut c = self.client().await?;
        let tx = c.transaction().await.map_err(db_error)?;
        tx.execute("SELECT pg_advisory_xact_lock($1)", &[&LOCK_CODES])
            .await
            .map_err(db_error)?;
        let now = now_ms();
        let active: i64 = tx
            .query_one(
                "SELECT COUNT(*) FROM enrollment_codes
                 WHERE login = $1 AND used_at_ms IS NULL AND expires_at_ms > $2",
                &[&login, &now],
            )
            .await
            .map_err(db_error)?
            .get(0);
        if active >= i64::from(max_active) {
            return Ok(CodeInsert::TooMany);
        }
        let expires_at_ms = now + ttl_ms;
        tx.execute(
            "INSERT INTO enrollment_codes (digest, login, created_at_ms, expires_at_ms) VALUES ($1, $2, $3, $4)",
            &[&digest, &login, &now, &expires_at_ms],
        )
        .await
        .map_err(db_error)?;
        tx.commit().await.map_err(db_error)?;
        Ok(CodeInsert::Created { expires_at_ms })
    }

    /// Removes a code that is not used.
    pub async fn delete_code(&self, digest: &[u8]) -> Result<bool, DbError> {
        let c = self.client().await?;
        let n = c
            .execute(
                "DELETE FROM enrollment_codes WHERE digest = $1 AND used_at_ms IS NULL",
                &[&digest],
            )
            .await
            .map_err(db_error)?;
        Ok(n > 0)
    }

    /// Uses a code and stores the device of its account, in one transaction. The row of the code
    /// is locked, so a second instance that holds the same secret waits and then finds it used.
    pub async fn redeem_code(
        &self,
        digest: &[u8],
        max_devices: u32,
        name: &str,
        public_key: &[u8],
        ip: &str,
    ) -> Result<Redeem, DbError> {
        let mut c = self.client().await?;
        let tx = c.transaction().await.map_err(db_error)?;
        // The limit of devices is a count: two instances must not both take the last place.
        tx.execute("SELECT pg_advisory_xact_lock($1)", &[&LOCK_ENROLL])
            .await
            .map_err(db_error)?;
        let now = now_ms();
        let row = tx
            .query_opt(
                "SELECT login, expires_at_ms, used_at_ms FROM enrollment_codes WHERE digest = $1 FOR UPDATE",
                &[&digest],
            )
            .await
            .map_err(db_error)?;
        let Some(row) = row else {
            return Ok(Redeem::Refused(Refusal::Unknown));
        };
        let (login, expires_at_ms, used_at_ms): (String, i64, Option<i64>) =
            (row.get(0), row.get(1), row.get(2));
        if used_at_ms.is_some() {
            return Ok(Redeem::Refused(Refusal::Used));
        }
        if expires_at_ms <= now {
            return Ok(Redeem::Refused(Refusal::Expired));
        }
        let active: i64 = tx
            .query_one("SELECT COUNT(*) FROM devices WHERE revoked_at IS NULL", &[])
            .await
            .map_err(db_error)?
            .get(0);
        if active >= i64::from(max_devices) {
            return Ok(Redeem::TooManyDevices);
        }
        tx.execute(
            "UPDATE enrollment_codes SET used_at_ms = $2 WHERE digest = $1 AND used_at_ms IS NULL",
            &[&digest, &now],
        )
        .await
        .map_err(db_error)?;
        let device_id = random_id();
        tx.execute(
            "INSERT INTO devices (id, name, user_name, public_key, enrolled_at, enrolled_ip) VALUES ($1, $2, $3, $4, $5, $6)",
            &[&device_id, &name, &login, &public_key, &now, &ip],
        )
        .await
        .map_err(db_error)?;
        tx.commit().await.map_err(db_error)?;
        Ok(Redeem::Enrolled { device_id, login })
    }

    /// Deletes the unused codes of `login` and revokes its devices, in one transaction. The other
    /// instances close the sessions of the devices. The result is the ids that this call revoked.
    pub async fn revoke_account(&self, login: &str) -> Result<Vec<String>, DbError> {
        let mut c = self.client().await?;
        let tx = c.transaction().await.map_err(db_error)?;
        // The same lock as the enrollment: a phone that enrolls now is before or after this call.
        tx.execute("SELECT pg_advisory_xact_lock($1)", &[&LOCK_ENROLL])
            .await
            .map_err(db_error)?;
        tx.execute(
            "DELETE FROM enrollment_codes WHERE login = $1 AND used_at_ms IS NULL",
            &[&login],
        )
        .await
        .map_err(db_error)?;
        let rows = tx
            .query(
                "UPDATE devices SET revoked_at = $2 WHERE user_name = $1 AND revoked_at IS NULL RETURNING id",
                &[&login, &now_ms()],
            )
            .await
            .map_err(db_error)?;
        let ids: Vec<String> = rows.iter().map(|r| r.get(0)).collect();
        for id in &ids {
            tx.execute("DELETE FROM subscriptions WHERE device_id = $1", &[id])
                .await
                .map_err(db_error)?;
            tx.execute(
                "SELECT pg_notify('inbox_kick', $1)",
                &[&format!("revoked {id}")],
            )
            .await
            .map_err(db_error)?;
        }
        tx.commit().await.map_err(db_error)?;
        Ok(ids)
    }

    pub async fn find_device(&self, id: &str) -> Result<Option<DeviceKey>, DbError> {
        let c = self.client().await?;
        let row = c
            .query_opt(
                "SELECT public_key, revoked_at IS NOT NULL, name, user_name FROM devices WHERE id = $1",
                &[&id],
            )
            .await
            .map_err(db_error)?;
        Ok(row.map(|r| DeviceKey {
            public_key: r.get(0),
            revoked: r.get(1),
            name: r.get(2),
            user_name: r.get(3),
        }))
    }

    pub async fn revoke_device(&self, id: &str) -> Result<bool, DbError> {
        let mut c = self.client().await?;
        let tx = c.transaction().await.map_err(db_error)?;
        let n = tx
            .execute(
                "UPDATE devices SET revoked_at = $2 WHERE id = $1 AND revoked_at IS NULL",
                &[&id, &now_ms()],
            )
            .await
            .map_err(db_error)?;
        tx.execute("DELETE FROM subscriptions WHERE device_id = $1", &[&id])
            .await
            .map_err(db_error)?;
        if n > 0 {
            tx.execute(
                "SELECT pg_notify('inbox_kick', $1)",
                &[&format!("revoked {id}")],
            )
            .await
            .map_err(db_error)?;
        }
        tx.commit().await.map_err(db_error)?;
        Ok(n > 0)
    }

    pub async fn record_connect(
        &self,
        id: &str,
        ip: &str,
        app_version: &str,
    ) -> Result<i64, DbError> {
        let mut c = self.client().await?;
        let tx = c.transaction().await.map_err(db_error)?;
        let now = now_ms();
        tx.execute(
            "UPDATE devices SET last_connected_at = $2, last_seen_at = $2, last_ip = $3, app_version = $4 WHERE id = $1",
            &[&id, &now, &ip, &app_version],
        )
        .await
        .map_err(db_error)?;
        let row = tx
            .query_one(
                "INSERT INTO connections (device_id, user_name, ip, app_version, connected_at, instance)
                 SELECT $1, user_name, $2, $3, $4, $5 FROM devices WHERE id = $1 RETURNING id",
                &[&id, &ip, &app_version, &now, &self.instance],
            )
            .await
            .map_err(db_error)?;
        // A device has one session: the other instances close the one they may still hold.
        tx.execute(
            "SELECT pg_notify('inbox_kick', $1)",
            &[&format!("replaced {id} {} {now}", self.instance)],
        )
        .await
        .map_err(db_error)?;
        tx.commit().await.map_err(db_error)?;
        Ok(row.get(0))
    }

    pub async fn record_disconnect(
        &self,
        connection_id: i64,
        device_id: &str,
        reason: &str,
    ) -> Result<(), DbError> {
        let mut c = self.client().await?;
        let tx = c.transaction().await.map_err(db_error)?;
        let now = now_ms();
        tx.execute(
            "UPDATE connections SET disconnected_at = $2, close_reason = $3 WHERE id = $1 AND disconnected_at IS NULL",
            &[&connection_id, &now, &reason],
        )
        .await
        .map_err(db_error)?;
        tx.execute("UPDATE devices SET last_seen_at = GREATEST(COALESCE(last_seen_at, 0), $2) WHERE id = $1", &[&device_id, &now])
            .await
            .map_err(db_error)?;
        tx.commit().await.map_err(db_error)
    }

    pub async fn touch_seen(&self, seen: &[(String, i64)]) -> Result<(), DbError> {
        let ids: Vec<&str> = seen.iter().map(|(id, _)| id.as_str()).collect();
        let times: Vec<i64> = seen.iter().map(|(_, at)| *at).collect();
        let c = self.client().await?;
        c.execute(
            "UPDATE devices d SET last_seen_at = GREATEST(COALESCE(d.last_seen_at, 0), v.at)
             FROM unnest($1::text[], $2::bigint[]) AS v(id, at) WHERE d.id = v.id",
            &[&ids, &times],
        )
        .await
        .map_err(db_error)?;
        Ok(())
    }

    // ------------------------------------------------------------ subscriptions

    async fn subscriptions_of(
        c: &impl tokio_postgres::GenericClient,
        id: &str,
    ) -> Result<Vec<String>, DbError> {
        let rows = c
            .query(
                "SELECT group_id FROM subscriptions WHERE device_id = $1 ORDER BY group_id",
                &[&id],
            )
            .await
            .map_err(db_error)?;
        Ok(rows.iter().map(|r| r.get(0)).collect())
    }

    pub async fn profile(&self, id: &str) -> Result<(Vec<String>, String), DbError> {
        let c = self.client().await?;
        let user_name: Option<String> = c
            .query_opt("SELECT user_name FROM devices WHERE id = $1", &[&id])
            .await
            .map_err(db_error)?
            .map(|r| r.get(0));
        Ok((
            Self::subscriptions_of(&**c, id).await?,
            user_name.unwrap_or_default(),
        ))
    }

    pub async fn subscribe(&self, id: &str, group: &str) -> Result<Vec<String>, DbError> {
        let c = self.client().await?;
        c.execute(
            "INSERT INTO subscriptions (device_id, group_id, updated_at) VALUES ($1, $2, $3)
             ON CONFLICT (device_id, group_id) DO UPDATE SET updated_at = EXCLUDED.updated_at",
            &[&id, &group, &now_ms()],
        )
        .await
        .map_err(db_error)?;
        Self::subscriptions_of(&**c, id).await
    }

    pub async fn unsubscribe(&self, id: &str, group: &str) -> Result<Vec<String>, DbError> {
        let c = self.client().await?;
        c.execute(
            "DELETE FROM subscriptions WHERE device_id = $1 AND group_id = $2",
            &[&id, &group],
        )
        .await
        .map_err(db_error)?;
        Self::subscriptions_of(&**c, id).await
    }

    pub async fn prune_subscriptions(&self, groups: &[String]) -> Result<usize, DbError> {
        let c = self.client().await?;
        let n = c
            .execute(
                "DELETE FROM subscriptions WHERE NOT (group_id = ANY($1::text[]))",
                &[&groups],
            )
            .await
            .map_err(db_error)?;
        Ok(n as usize)
    }

    // ------------------------------------------------------------ alerts and deliveries

    pub async fn insert_alert(&self, alert: &NewAlert) -> Result<InsertOutcome, DbError> {
        let mut c = self.client().await?;
        let tx = c.transaction().await.map_err(db_error)?;
        let id = random_id();
        let created = tx
            .query_opt(
                "INSERT INTO alerts (id, target_kind, target_id, title, source, idempotency_key, created_at, kind)
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
                 ON CONFLICT (source, idempotency_key) DO NOTHING RETURNING id",
                &[
                    &id,
                    &alert.target.kind(),
                    &alert.target.id(),
                    &alert.title,
                    &alert.source,
                    &alert.idempotency_key,
                    &now_ms(),
                    &alert.kind.as_str(),
                ],
            )
            .await
            .map_err(db_error)?;
        if created.is_none() {
            // Another request, possibly on another instance, stored it first: it has committed by now.
            let row = tx
                .query_one(
                    "SELECT id, target_kind, target_id, title FROM alerts WHERE source = $1 AND idempotency_key = $2",
                    &[&alert.source, &alert.idempotency_key],
                )
                .await
                .map_err(db_error)?;
            let (existing, kind, target, title): (String, String, String, String) =
                (row.get(0), row.get(1), row.get(2), row.get(3));
            if kind != alert.target.kind() || target != alert.target.id() || title != alert.title {
                return Ok(InsertOutcome::Conflict);
            }
            let devices = Self::delivery_devices(&*tx, &existing).await?;
            return Ok(InsertOutcome::Duplicate {
                alert_id: existing,
                devices,
            });
        }
        // An account with no phone to use gets nothing: the alert would wait for no one.
        if let Target::User(login) = &alert.target {
            let phones: i64 = tx
                .query_one(
                    "SELECT COUNT(*) FROM devices WHERE user_name = $1 AND revoked_at IS NULL",
                    &[login],
                )
                .await
                .map_err(db_error)?
                .get(0);
            if phones == 0 {
                tx.rollback().await.map_err(db_error)?;
                return Ok(InsertOutcome::NoPhone);
            }
        }
        match &alert.target {
            Target::Group(group) => tx
                .execute(
                    "INSERT INTO deliveries (alert_id, device_id)
                     SELECT $1, s.device_id FROM subscriptions s
                     JOIN devices d ON d.id = s.device_id
                     WHERE s.group_id = $2 AND d.revoked_at IS NULL
                     ORDER BY s.device_id",
                    &[&id, group],
                )
                .await
                .map_err(db_error)?,
            Target::User(login) => tx
                .execute(
                    "INSERT INTO deliveries (alert_id, device_id)
                     SELECT $1, d.id FROM devices d
                     WHERE d.user_name = $2 AND d.revoked_at IS NULL
                     ORDER BY d.id",
                    &[&id, login],
                )
                .await
                .map_err(db_error)?,
        };
        let devices = Self::delivery_devices(&*tx, &id).await?;
        // Delivered at commit: the devices connected to the other instances are woken only for a stored alert.
        for chunk in devices.chunks(WAKE_CHUNK) {
            tx.execute("SELECT pg_notify('inbox_wake', $1)", &[&chunk.join(" ")])
                .await
                .map_err(db_error)?;
        }
        tx.commit().await.map_err(db_error)?;
        Ok(InsertOutcome::Created {
            alert_id: id,
            devices,
        })
    }

    async fn delivery_devices(
        c: &impl tokio_postgres::GenericClient,
        alert_id: &str,
    ) -> Result<Vec<String>, DbError> {
        let rows = c
            .query(
                "SELECT device_id FROM deliveries WHERE alert_id = $1 ORDER BY device_id",
                &[&alert_id],
            )
            .await
            .map_err(db_error)?;
        Ok(rows.iter().map(|r| r.get(0)).collect())
    }

    pub async fn pending_deliveries(
        &self,
        id: &str,
        limit: u32,
        cutoff: i64,
    ) -> Result<Vec<PendingDelivery>, DbError> {
        let c = self.client().await?;
        let rows = c
            .query(
                "SELECT d.seq, a.id, a.target_kind, a.target_id, a.title, a.created_at, a.kind
                 FROM deliveries d JOIN alerts a ON a.id = d.alert_id
                 WHERE d.device_id = $1 AND d.received_at IS NULL AND a.created_at >= $3
                 ORDER BY d.seq LIMIT $2",
                &[&id, &i64::from(limit), &cutoff],
            )
            .await
            .map_err(db_error)?;
        rows.iter()
            .map(|r| {
                Ok(PendingDelivery {
                    seq: r.get(0),
                    alert_id: r.get(1),
                    target: target_of(r.get(2), r.get(3))?,
                    title: r.get(4),
                    created_at: r.get(5),
                    kind: kind_of(r.get(6))?,
                })
            })
            .collect()
    }

    /// The number of phones that are not revoked, for each login that has one at least.
    pub async fn active_phones_by_login(
        &self,
    ) -> Result<std::collections::BTreeMap<String, u32>, DbError> {
        let c = self.client().await?;
        let rows = c
            .query(
                "SELECT user_name, COUNT(*)::bigint FROM devices WHERE revoked_at IS NULL GROUP BY user_name",
                &[],
            )
            .await
            .map_err(db_error)?;
        Ok(rows
            .iter()
            .map(|r| (r.get::<_, String>(0), r.get::<_, i64>(1) as u32))
            .collect())
    }

    pub async fn mark_sent(&self, seqs: &[i64]) -> Result<(), DbError> {
        let c = self.client().await?;
        c.execute(
            "UPDATE deliveries SET attempts = attempts + 1, first_sent_at = COALESCE(first_sent_at, $2), last_sent_at = $2
             WHERE seq = ANY($1::bigint[])",
            &[&seqs, &now_ms()],
        )
        .await
        .map_err(db_error)?;
        Ok(())
    }

    pub async fn ack(
        &self,
        device_id: &str,
        alert_id: &str,
    ) -> Result<Option<AckOutcome>, DbError> {
        let c = self.client().await?;
        let now = now_ms();
        let row = c
            .query_opt(
                "WITH old AS (
                     SELECT d.seq, d.received_at IS NULL AS first, a.created_at
                     FROM deliveries d JOIN alerts a ON a.id = d.alert_id
                     WHERE d.device_id = $1 AND d.alert_id = $2
                     FOR UPDATE OF d
                 )
                 UPDATE deliveries SET received_at = COALESCE(deliveries.received_at, $3)
                 FROM old WHERE deliveries.seq = old.seq
                 RETURNING old.seq, old.first, old.created_at",
                &[&device_id, &alert_id, &now],
            )
            .await
            .map_err(db_error)?;
        Ok(row.map(|r| {
            let (seq, first, created_at): (i64, bool, i64) = (r.get(0), r.get(1), r.get(2));
            AckOutcome {
                seq,
                first_receipt_latency_ms: first.then_some(now - created_at),
            }
        }))
    }

    // ------------------------------------------------------------ admin views and statistics

    pub async fn overview(&self, cutoff: i64) -> Result<(Vec<DeviceView>, (i64, i64)), DbError> {
        let c = self.client().await?;
        let rows = c
            .query(
                "SELECT d.id, d.name, d.enrolled_at, d.enrolled_ip, d.app_version, d.last_connected_at,
                        d.last_seen_at, d.last_ip, d.revoked_at,
                        (SELECT COUNT(*) FROM deliveries x JOIN alerts y ON y.id = x.alert_id
                         WHERE x.device_id = d.id AND x.received_at IS NULL AND y.created_at >= $1),
                        d.user_name
                 FROM devices d ORDER BY d.revoked_at IS NOT NULL, d.user_name, d.name",
                &[&cutoff],
            )
            .await
            .map_err(db_error)?;
        let mut devices: Vec<DeviceView> = rows
            .iter()
            .map(|r| DeviceView {
                id: r.get(0),
                name: r.get(1),
                enrolled_at: r.get(2),
                enrolled_ip: r.get(3),
                app_version: r.get(4),
                last_connected_at: r.get(5),
                last_seen_at: r.get(6),
                last_ip: r.get(7),
                revoked_at: r.get(8),
                subscriptions: Vec::new(),
                pending: r.get(9),
                user_name: r.get(10),
            })
            .collect();
        let subs = c
            .query(
                "SELECT device_id, group_id FROM subscriptions ORDER BY group_id",
                &[],
            )
            .await
            .map_err(db_error)?;
        for s in &subs {
            let (device, group): (String, String) = (s.get(0), s.get(1));
            if let Some(d) = devices.iter_mut().find(|d| d.id == device) {
                d.subscriptions.push(group);
            }
        }
        Ok((devices, Self::pending_stats(&**c, cutoff).await?))
    }

    async fn pending_stats(
        c: &impl tokio_postgres::GenericClient,
        cutoff: i64,
    ) -> Result<(i64, i64), DbError> {
        let r = c
            .query_one(
                "SELECT COUNT(*), COALESCE($1::bigint - MIN(a.created_at), 0)::bigint
                 FROM deliveries d JOIN alerts a ON a.id = d.alert_id
                 JOIN devices v ON v.id = d.device_id
                 WHERE d.received_at IS NULL AND v.revoked_at IS NULL AND a.created_at >= $2",
                &[&now_ms(), &cutoff],
            )
            .await
            .map_err(db_error)?;
        Ok((r.get(0), r.get(1)))
    }

    fn summary(r: &tokio_postgres::Row) -> Result<AlertSummary, DbError> {
        Ok(AlertSummary {
            id: r.get(0),
            kind: kind_of(r.get(8))?,
            target: target_of(r.get(1), r.get(2))?,
            title: r.get(3),
            source: r.get(4),
            created_at: r.get(5),
            targets: r.get(6),
            received: r.get(7),
            expired: 0,
            missing: Vec::new(),
        })
    }

    pub async fn recent_alerts(
        &self,
        limit: u32,
        since_ms: i64,
        cutoff: i64,
    ) -> Result<Vec<AlertSummary>, DbError> {
        let c = self.client().await?;
        let rows = c
            .query(
                "SELECT a.id, a.target_kind, a.target_id, a.title, a.source, a.created_at, COUNT(d.seq), COUNT(d.received_at), a.kind
                 FROM alerts a LEFT JOIN deliveries d ON d.alert_id = a.id
                 WHERE a.created_at >= $2 GROUP BY a.id ORDER BY a.created_at DESC LIMIT $1",
                &[&i64::from(limit), &since_ms],
            )
            .await
            .map_err(db_error)?;
        let mut alerts: Vec<AlertSummary> = rows
            .iter()
            .map(|r| Self::summary(r).map(|a| super::db::with_expiry(a, cutoff)))
            .collect::<Result<_, DbError>>()?;
        let ids: Vec<&str> = alerts.iter().map(|a| a.id.as_str()).collect();
        let missing = c
            .query(
                "SELECT d.alert_id, v.user_name FROM deliveries d JOIN devices v ON v.id = d.device_id
                 WHERE d.alert_id = ANY($1::text[]) AND d.received_at IS NULL AND v.revoked_at IS NULL
                 GROUP BY d.alert_id, v.user_name ORDER BY v.user_name",
                &[&ids],
            )
            .await
            .map_err(db_error)?;
        for m in &missing {
            let (alert, user): (String, String) = (m.get(0), m.get(1));
            if let Some(a) = alerts.iter_mut().find(|a| a.id == alert) {
                a.missing.push(user);
            }
        }
        Ok(alerts)
    }

    // ------------------------------------------------------------ notifications of the web

    /// The preferences of an account. An account that has no row gets one, with the marker at
    /// `now`.
    async fn notify_prefs(c: &Object, login: &str, now: i64) -> Result<Prefs, DbError> {
        c.execute(
            "INSERT INTO web_notify (login, read_up_to) VALUES ($1, $2) ON CONFLICT (login) DO NOTHING",
            &[&login, &now],
        )
        .await
        .map_err(db_error)?;
        let r = c
            .query_one(
                "SELECT read_up_to, level FROM web_notify WHERE login = $1",
                &[&login],
            )
            .await
            .map_err(db_error)?;
        let level: String = r.get(1);
        Ok(Prefs {
            read_up_to: r.get(0),
            level: Level::parse(&level)
                .ok_or_else(|| DbError::Pg(format!("unknown level {level:?}")))?,
        })
    }

    /// The condition of the alerts that `scope` receives, with the parameters from `$first` on.
    /// The second value is the list of groups for the first parameter, or `None` when the scope
    /// has all the groups. The third is the number of the parameter of the login.
    fn scope_sql(scope: &Scope, first: usize) -> (String, Option<Vec<String>>, usize) {
        match &scope.groups {
            None => (
                format!("(target_kind = 'group' OR (target_kind = 'user' AND target_id = ${first}))"),
                None,
                first,
            ),
            Some(ids) => (
                format!(
                    "((target_kind = 'group' AND target_id = ANY(${first}::text[])) OR (target_kind = 'user' AND target_id = ${}))",
                    first + 1
                ),
                Some(ids.clone()),
                first + 1,
            ),
        }
    }

    /// The alerts that `scope` receives: the preferences, the count of unread ones and the newest
    /// ones.
    pub async fn notify_view(&self, scope: &Scope, ask: &Ask) -> Result<View, DbError> {
        let c = self.client().await?;
        let prefs = Self::notify_prefs(&c, &scope.login, now_ms()).await?;
        if ask.skip_if_off && prefs.level == Level::Off {
            return Ok(View {
                prefs,
                unread: 0,
                items: Vec::new(),
            });
        }
        let cap = i64::from(UNREAD_CAP);
        let limit = i64::from(ask.limit);
        // The groups, when the scope has a list, then the login, as the last parameter.
        let (condition, groups, login_at) = Self::scope_sql(scope, 2);
        let unread_sql = format!(
            "SELECT COUNT(*)::bigint FROM (SELECT 1 FROM alerts WHERE created_at > $1 AND {condition} LIMIT ${})",
            login_at + 1
        );
        let unread: i64 = match &groups {
            Some(ids) => {
                c.query_one(&unread_sql, &[&prefs.read_up_to, ids, &scope.login, &cap])
                    .await
            }
            None => {
                c.query_one(&unread_sql, &[&prefs.read_up_to, &scope.login, &cap])
                    .await
            }
        }
        .map_err(db_error)?
        .get(0);
        let items = if ask.limit == 0 {
            Vec::new()
        } else {
            let sql = format!(
                "SELECT id, target_kind, target_id, title, created_at FROM alerts
                 WHERE created_at > $1 AND {condition} ORDER BY created_at DESC, id DESC LIMIT ${}",
                login_at + 1
            );
            let rows = match &groups {
                Some(ids) => {
                    c.query(&sql, &[&ask.since_ms, ids, &scope.login, &limit])
                        .await
                }
                None => c.query(&sql, &[&ask.since_ms, &scope.login, &limit]).await,
            }
            .map_err(db_error)?;
            rows.iter()
                .map(|r| {
                    Ok(Item {
                        id: r.get(0),
                        target: target_of(r.get(1), r.get(2))?,
                        title: r.get(3),
                        created_at: r.get(4),
                    })
                })
                .collect::<Result<_, DbError>>()?
        };
        Ok(View {
            prefs,
            unread: u32::try_from(unread).unwrap_or(UNREAD_CAP),
            items,
        })
    }

    /// Moves the marker of the account to the time of its newest alert. The marker never goes
    /// back. The result is the marker.
    pub async fn notify_mark_read(&self, scope: &Scope) -> Result<i64, DbError> {
        let c = self.client().await?;
        Self::notify_prefs(&c, &scope.login, now_ms()).await?;
        let (condition, groups, _) = Self::scope_sql(scope, 1);
        let sql = format!("SELECT MAX(created_at)::bigint FROM alerts WHERE {condition}");
        let newest: Option<i64> = match &groups {
            Some(ids) => c.query_one(&sql, &[ids, &scope.login]).await,
            None => c.query_one(&sql, &[&scope.login]).await,
        }
        .map_err(db_error)?
        .get(0);
        let r = c
            .query_one(
                "UPDATE web_notify SET read_up_to = GREATEST(read_up_to, $2) WHERE login = $1 RETURNING read_up_to",
                &[&scope.login, &newest.unwrap_or(0)],
            )
            .await
            .map_err(db_error)?;
        Ok(r.get(0))
    }

    /// Sets the level of the toasts of an account.
    pub async fn notify_set_level(&self, login: &str, level: Level) -> Result<(), DbError> {
        let c = self.client().await?;
        c.execute(
            "INSERT INTO web_notify (login, read_up_to, level) VALUES ($1, $2, $3)
             ON CONFLICT (login) DO UPDATE SET level = EXCLUDED.level",
            &[&login, &now_ms(), &level.as_str()],
        )
        .await
        .map_err(db_error)?;
        Ok(())
    }

    /// The alerts created at or after `since_ms`, counted for each bucket and each group.
    pub async fn alert_counts(
        &self,
        since_ms: i64,
        bucket_ms: i64,
    ) -> Result<Vec<super::db::AlertCount>, DbError> {
        let c = self.client().await?;
        let rows = c
            .query(
                "SELECT (created_at - $1) / $2, target_id, COUNT(*) FROM alerts
                 WHERE created_at >= $1 AND target_kind = 'group'
                 GROUP BY 1, 2 ORDER BY 1, 2",
                &[&since_ms, &bucket_ms.max(1)],
            )
            .await
            .map_err(db_error)?;
        Ok(rows
            .iter()
            .map(|r| super::db::AlertCount {
                bucket: r.get(0),
                group: r.get(1),
                count: r.get(2),
            })
            .collect())
    }

    pub async fn alert_detail(
        &self,
        id: &str,
        cutoff: i64,
    ) -> Result<Option<AlertDetail>, DbError> {
        let c = self.client().await?;
        let Some(row) = c
            .query_opt(
                "SELECT a.id, a.target_kind, a.target_id, a.title, a.source, a.created_at, COUNT(d.seq), COUNT(d.received_at), a.kind
                 FROM alerts a LEFT JOIN deliveries d ON d.alert_id = a.id WHERE a.id = $1 GROUP BY a.id",
                &[&id],
            )
            .await
            .map_err(db_error)?
        else {
            return Ok(None);
        };
        let mut summary = super::db::with_expiry(Self::summary(&row)?, cutoff);
        let rows = c
            .query(
                "SELECT d.device_id, v.name, d.attempts, d.first_sent_at, d.last_sent_at,
                        d.received_at, v.revoked_at IS NOT NULL, v.user_name
                 FROM deliveries d JOIN devices v ON v.id = d.device_id
                 WHERE d.alert_id = $1 ORDER BY v.user_name, v.name",
                &[&id],
            )
            .await
            .map_err(db_error)?;
        let is_expired = summary.created_at < cutoff;
        let deliveries: Vec<DeliveryView> = rows
            .iter()
            .map(|r| DeliveryView {
                device_id: r.get(0),
                device_name: r.get(1),
                attempts: r.get(2),
                first_sent_at: r.get(3),
                last_sent_at: r.get(4),
                received_at: r.get(5),
                expired: is_expired && r.get::<_, Option<i64>>(5).is_none(),
                device_revoked: r.get(6),
                user_name: r.get(7),
            })
            .collect();
        let mut missing: Vec<String> = rows
            .iter()
            .filter(|r| r.get::<_, Option<i64>>(5).is_none() && !r.get::<_, bool>(6))
            .map(|r| r.get::<_, String>(7))
            .collect();
        missing.sort();
        missing.dedup();
        summary.missing = missing;
        Ok(Some(AlertDetail {
            summary,
            deliveries,
        }))
    }

    pub async fn recent_connections(
        &self,
        limit: u32,
        user_name: Option<&str>,
    ) -> Result<Vec<ConnectionView>, DbError> {
        let c = self.client().await?;
        let rows = c
            .query(
                "SELECT c.device_id, d.name, c.ip, c.app_version, c.connected_at, c.disconnected_at, c.close_reason,
                        c.user_name
                 FROM connections c JOIN devices d ON d.id = c.device_id
                 WHERE ($2::text IS NULL OR c.user_name = $2)
                 ORDER BY c.connected_at DESC LIMIT $1",
                &[&i64::from(limit), &user_name],
            )
            .await
            .map_err(db_error)?;
        Ok(rows
            .iter()
            .map(|r| ConnectionView {
                device_id: r.get(0),
                device_name: r.get(1),
                ip: r.get(2),
                app_version: r.get(3),
                connected_at: r.get(4),
                disconnected_at: r.get(5),
                close_reason: r.get(6),
                user_name: r.get(7),
            })
            .collect())
    }

    pub async fn stats(&self, cutoff: i64) -> Result<Stats, DbError> {
        let c = self.client().await?;
        let pending = Self::pending_stats(&**c, cutoff).await?;
        let subscribed = c
            .query(
                "SELECT d.id, d.last_seen_at FROM devices d
                 WHERE d.revoked_at IS NULL AND EXISTS (SELECT 1 FROM subscriptions s WHERE s.device_id = d.id)",
                &[],
            )
            .await
            .map_err(db_error)?
            .iter()
            .map(|r| (r.get(0), r.get(1)))
            .collect();
        let enrolled: i64 = c
            .query_one("SELECT COUNT(*) FROM devices WHERE revoked_at IS NULL", &[])
            .await
            .map_err(db_error)?
            .get(0);
        Ok(Stats {
            pending,
            subscribed,
            enrolled: enrolled as u32,
        })
    }

    // ------------------------------------------------------------ retention

    pub async fn purge(
        &self,
        alert_cutoff: i64,
        connection_cutoff: i64,
    ) -> Result<(usize, usize, usize), DbError> {
        let mut c = self.client().await?;
        let tx = c.transaction().await.map_err(db_error)?;
        // One instance purges at a time; the others skip this round.
        let locked: bool = tx
            .query_one("SELECT pg_try_advisory_xact_lock($1)", &[&LOCK_PURGE])
            .await
            .map_err(db_error)?
            .get(0);
        if !locked {
            return Ok((0, 0, 0));
        }
        tx.execute(
            "DELETE FROM deliveries WHERE alert_id IN (SELECT id FROM alerts WHERE created_at < $1)",
            &[&alert_cutoff],
        )
        .await
        .map_err(db_error)?;
        let alerts = tx
            .execute("DELETE FROM alerts WHERE created_at < $1", &[&alert_cutoff])
            .await
            .map_err(db_error)?;
        let connections = tx
            .execute(
                "DELETE FROM connections WHERE connected_at < $1 AND disconnected_at IS NOT NULL",
                &[&connection_cutoff],
            )
            .await
            .map_err(db_error)?;
        let codes = tx
            .execute(
                "DELETE FROM enrollment_codes WHERE expires_at_ms < $1",
                &[&(now_ms() - CODE_KEEP_MS)],
            )
            .await
            .map_err(db_error)?;
        tx.commit().await.map_err(db_error)?;
        Ok((alerts as usize, connections as usize, codes as usize))
    }
}

/// The optional columns of a device row: app version, last connected, last seen, last ip, revoked.
type DeviceTail = (
    Option<String>,
    Option<i64>,
    Option<i64>,
    Option<String>,
    Option<i64>,
);

/// What `import_sqlite` copied.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Imported {
    pub devices: usize,
    pub subscriptions: usize,
    pub alerts: usize,
    pub deliveries: usize,
    pub connections: usize,
    pub enrollment_codes: usize,
}

impl PgStore {
    /// Copies a SQLite database (an Inbox SQLite file or one of its snapshots) into this empty PostgreSQL database, in one
    /// transaction: devices, subscriptions, alerts, deliveries, the connection log and the enrollment codes. Refuses a database
    /// that is not empty.
    pub async fn import_sqlite(&self, path: &Path) -> Result<Imported, String> {
        use rusqlite::{Connection, OpenFlags};
        let lite = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)
            .map_err(|e| format!("cannot open {}: {e}", path.display()))?;
        // A file of the portal keeps the version of the inbox in its own table. A file of the old
        // standalone alert server keeps it in `PRAGMA user_version`.
        let own_table: i64 = lite
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'inbox_schema_version'",
                [],
                |r| r.get(0),
            )
            .map_err(|e| e.to_string())?;
        let version: i64 = if own_table > 0 {
            lite.query_row(
                "SELECT COALESCE(MAX(version), 0) FROM inbox_schema_version",
                [],
                |r| r.get(0),
            )
        } else {
            lite.query_row("PRAGMA user_version", [], |r| r.get(0))
        }
        .map_err(|e| e.to_string())?;
        if version != super::db::SCHEMA_VERSION {
            return Err(format!(
                "{} has schema version {version}, this binary imports version {}: start the SQLite instance once with this binary to migrate it",
                path.display(),
                super::db::SCHEMA_VERSION
            ));
        }
        let mut c = self.client().await.map_err(|e| e.to_string())?;
        let tx = c.transaction().await.map_err(|e| e.to_string())?;
        tx.execute("SELECT pg_advisory_xact_lock($1)", &[&LOCK_ENROLL])
            .await
            .map_err(|e| e.to_string())?;
        for table in ["devices", "alerts", "enrollment_codes"] {
            let n: i64 = tx
                .query_one(&format!("SELECT COUNT(*) FROM {table}"), &[])
                .await
                .map_err(|e| e.to_string())?
                .get(0);
            if n > 0 {
                return Err(format!(
                    "the PostgreSQL database already holds {n} rows in {table}: refusing to import over it"
                ));
            }
        }
        let mut out = Imported::default();

        let mut stmt = lite
            .prepare(
                "SELECT id, name, COALESCE(user_name, name), public_key, enrolled_at, enrolled_ip, app_version,
                        last_connected_at, last_seen_at, last_ip, revoked_at FROM devices",
            )
            .map_err(|e| e.to_string())?;
        let mut rows = stmt.query([]).map_err(|e| e.to_string())?;
        while let Some(r) = rows.next().map_err(|e| e.to_string())? {
            let (id, name, user, key, at, ip): (String, String, String, Vec<u8>, i64, String) = (
                r.get(0).unwrap(),
                r.get(1).unwrap(),
                r.get(2).unwrap(),
                r.get(3).unwrap(),
                r.get(4).unwrap(),
                r.get(5).unwrap(),
            );
            let (ver, connected, seen, last_ip, revoked): DeviceTail = (
                r.get(6).unwrap(),
                r.get(7).unwrap(),
                r.get(8).unwrap(),
                r.get(9).unwrap(),
                r.get(10).unwrap(),
            );
            tx.execute(
                "INSERT INTO devices (id, name, user_name, public_key, enrolled_at, enrolled_ip, app_version, last_connected_at,
                                      last_seen_at, last_ip, revoked_at)
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)",
                &[&id, &name, &user, &key, &at, &ip, &ver, &connected, &seen, &last_ip, &revoked],
            )
            .await
            .map_err(|e| e.to_string())?;
            out.devices += 1;
        }

        let mut stmt = lite
            .prepare("SELECT device_id, group_id, updated_at FROM subscriptions")
            .map_err(|e| e.to_string())?;
        let mut rows = stmt.query([]).map_err(|e| e.to_string())?;
        while let Some(r) = rows.next().map_err(|e| e.to_string())? {
            let (device, group, at): (String, String, i64) =
                (r.get(0).unwrap(), r.get(1).unwrap(), r.get(2).unwrap());
            tx.execute(
                "INSERT INTO subscriptions (device_id, group_id, updated_at) VALUES ($1, $2, $3)",
                &[&device, &group, &at],
            )
            .await
            .map_err(|e| e.to_string())?;
            out.subscriptions += 1;
        }

        let mut stmt = lite
            .prepare(
                "SELECT id, target_kind, target_id, title, source, idempotency_key, created_at, kind FROM alerts",
            )
            .map_err(|e| e.to_string())?;
        let mut rows = stmt.query([]).map_err(|e| e.to_string())?;
        while let Some(r) = rows.next().map_err(|e| e.to_string())? {
            let (id, target_kind, target_id, title, source, key, at): (
                String,
                String,
                String,
                String,
                String,
                String,
                i64,
            ) = (
                r.get(0).unwrap(),
                r.get(1).unwrap(),
                r.get(2).unwrap(),
                r.get(3).unwrap(),
                r.get(4).unwrap(),
                r.get(5).unwrap(),
                r.get(6).unwrap(),
            );
            let kind: String = r.get(7).unwrap();
            let kind = Kind::parse(&kind)
                .ok_or_else(|| format!("alert {id} has the unknown message kind {kind:?}"))?;
            tx.execute(
                "INSERT INTO alerts (id, target_kind, target_id, title, source, idempotency_key, created_at, kind) VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
                &[&id, &target_kind, &target_id, &title, &source, &key, &at, &kind.as_str()],
            )
            .await
            .map_err(|e| e.to_string())?;
            out.alerts += 1;
        }

        let mut stmt = lite
            .prepare("SELECT seq, alert_id, device_id, attempts, first_sent_at, last_sent_at, received_at FROM deliveries")
            .map_err(|e| e.to_string())?;
        let mut rows = stmt.query([]).map_err(|e| e.to_string())?;
        while let Some(r) = rows.next().map_err(|e| e.to_string())? {
            let (seq, alert, device, attempts): (i64, String, String, i64) = (
                r.get(0).unwrap(),
                r.get(1).unwrap(),
                r.get(2).unwrap(),
                r.get(3).unwrap(),
            );
            let (first, last, received): (Option<i64>, Option<i64>, Option<i64>) =
                (r.get(4).unwrap(), r.get(5).unwrap(), r.get(6).unwrap());
            tx.execute(
                "INSERT INTO deliveries (seq, alert_id, device_id, attempts, first_sent_at, last_sent_at, received_at)
                 VALUES ($1, $2, $3, $4, $5, $6, $7)",
                &[&seq, &alert, &device, &attempts, &first, &last, &received],
            )
            .await
            .map_err(|e| e.to_string())?;
            out.deliveries += 1;
        }

        let mut stmt = lite
            .prepare(
                "SELECT id, device_id, COALESCE(user_name, ''), ip, app_version, connected_at, disconnected_at, close_reason
                 FROM connections",
            )
            .map_err(|e| e.to_string())?;
        let mut rows = stmt.query([]).map_err(|e| e.to_string())?;
        while let Some(r) = rows.next().map_err(|e| e.to_string())? {
            let (id, device, user, ip, ver, connected): (i64, String, String, String, String, i64) = (
                r.get(0).unwrap(),
                r.get(1).unwrap(),
                r.get(2).unwrap(),
                r.get(3).unwrap(),
                r.get(4).unwrap(),
                r.get(5).unwrap(),
            );
            let (disconnected, reason): (Option<i64>, Option<String>) =
                (r.get(6).unwrap(), r.get(7).unwrap());
            // A session still open in the file belongs to the instance that wrote it: it is over.
            let disconnected = disconnected.or(Some(connected));
            let reason = reason.or_else(|| Some("imported".to_string()));
            tx.execute(
                "INSERT INTO connections (id, device_id, user_name, ip, app_version, connected_at, disconnected_at, close_reason, instance)
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8, 'imported')",
                &[&id, &device, &user, &ip, &ver, &connected, &disconnected, &reason],
            )
            .await
            .map_err(|e| e.to_string())?;
            out.connections += 1;
        }

        let mut stmt = lite
            .prepare(
                "SELECT digest, login, created_at_ms, expires_at_ms, used_at_ms FROM enrollment_codes",
            )
            .map_err(|e| e.to_string())?;
        let mut rows = stmt.query([]).map_err(|e| e.to_string())?;
        while let Some(r) = rows.next().map_err(|e| e.to_string())? {
            let (digest, login, created, expires): (Vec<u8>, String, i64, i64) = (
                r.get(0).unwrap(),
                r.get(1).unwrap(),
                r.get(2).unwrap(),
                r.get(3).unwrap(),
            );
            let used: Option<i64> = r.get(4).unwrap();
            tx.execute(
                "INSERT INTO enrollment_codes (digest, login, created_at_ms, expires_at_ms, used_at_ms) VALUES ($1, $2, $3, $4, $5)",
                &[&digest, &login, &created, &expires, &used],
            )
            .await
            .map_err(|e| e.to_string())?;
            out.enrollment_codes += 1;
        }

        // The sequences continue after the imported rows.
        for (table, column) in [("deliveries", "seq"), ("connections", "id")] {
            tx.execute(
                &format!(
                    "SELECT setval(pg_get_serial_sequence('{table}', '{column}'), GREATEST(COALESCE((SELECT MAX({column}) FROM {table}), 0), 1))"
                ),
                &[],
            )
            .await
            .map_err(|e| e.to_string())?;
        }
        tx.commit().await.map_err(|e| e.to_string())?;
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn notifications_are_parsed() {
        assert_eq!(
            parse_notification("inbox_wake", "a b c"),
            Some(Event::Wake(vec!["a".into(), "b".into(), "c".into()]))
        );
        assert_eq!(
            parse_notification("inbox_kick", "revoked dev1"),
            Some(Event::Revoked("dev1".into()))
        );
        assert_eq!(
            parse_notification("inbox_kick", "replaced dev1 inst2 1700000000000"),
            Some(Event::Replaced {
                device: "dev1".into(),
                instance: "inst2".into(),
                connected_at: 1_700_000_000_000
            })
        );
        assert_eq!(parse_notification("inbox_kick", "replaced dev1"), None);
        assert_eq!(parse_notification("inbox_kick", "bogus x"), None);
        assert_eq!(parse_notification("other", "x"), None);
    }

    /// A throwaway database on the server of `INBOX_TEST_PG=host:port:user:password`. The tests
    /// that need it end at once without the variable. The database goes away with the value.
    struct TestDatabase {
        host: String,
        port: u16,
        user: String,
        password: String,
        name: String,
    }

    impl TestDatabase {
        async fn admin(&self, database: &str) -> tokio_postgres::Client {
            let mut config = tokio_postgres::Config::new();
            config
                .host(&self.host)
                .port(self.port)
                .user(&self.user)
                .password(&self.password)
                .dbname(database);
            let (client, connection) = config.connect(NoTls).await.unwrap();
            tokio::spawn(connection);
            client
        }

        async fn create() -> Option<TestDatabase> {
            let raw = std::env::var("INBOX_TEST_PG").ok()?;
            let mut parts = raw.splitn(4, ':');
            let mut db = TestDatabase {
                host: parts.next()?.to_string(),
                port: parts.next()?.parse().ok()?,
                user: parts.next()?.to_string(),
                password: parts.next()?.to_string(),
                name: format!("inbox_unit_{}", random_id()),
            };
            db.name = db.name.to_lowercase();
            db.admin("postgres")
                .await
                .batch_execute(&format!("CREATE DATABASE {}", db.name))
                .await
                .unwrap();
            Some(db)
        }

        async fn store(&self) -> PgStore {
            PgStore::connect(&PgSettings {
                host: self.host.clone(),
                port: self.port,
                database: self.name.clone(),
                user: self.user.clone(),
                password: zeroize::Zeroizing::new(self.password.clone()),
                pool_size: 4,
                connect_timeout: Duration::from_secs(5),
            })
            .await
            .unwrap()
        }

        async fn sql(&self, sql: &str) {
            self.admin(&self.name)
                .await
                .batch_execute(sql)
                .await
                .unwrap();
        }
    }

    impl Drop for TestDatabase {
        fn drop(&mut self) {
            let (host, port, user, password, name) = (
                self.host.clone(),
                self.port,
                self.user.clone(),
                self.password.clone(),
                self.name.clone(),
            );
            let _ = std::thread::spawn(move || {
                let rt = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .unwrap();
                rt.block_on(async {
                    let mut config = tokio_postgres::Config::new();
                    config
                        .host(&host)
                        .port(port)
                        .user(&user)
                        .password(&password)
                        .dbname("postgres");
                    if let Ok((client, connection)) = config.connect(NoTls).await {
                        tokio::spawn(connection);
                        let _ = client
                            .batch_execute(&format!("DROP DATABASE IF EXISTS {name} WITH (FORCE)"))
                            .await;
                    }
                });
            })
            .join();
        }
    }

    const MINUTE_MS: i64 = 60_000;

    async fn redeem(store: &PgStore, digest: [u8; 32], key: u8) -> Redeem {
        store
            .redeem_code(&digest, 10, "phone", &[key; 32], "1.1.1.1")
            .await
            .unwrap()
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn postgres_a_code_works_one_time_and_expires() {
        let Some(db) = TestDatabase::create().await else {
            eprintln!("INBOX_TEST_PG is not set: skipped");
            return;
        };
        let store = db.store().await;
        let created = store
            .create_code(&[1; 32], "alice", 10 * MINUTE_MS, 5)
            .await
            .unwrap();
        let CodeInsert::Created { expires_at_ms } = created else {
            panic!("the code must be stored")
        };
        assert!(expires_at_ms > now_ms() + 9 * MINUTE_MS);
        let Redeem::Enrolled { device_id, login } = redeem(&store, [1; 32], 1).await else {
            panic!("the code must work")
        };
        assert_eq!(login, "alice");
        let device = store.find_device(&device_id).await.unwrap().unwrap();
        assert_eq!(
            (device.user_name.as_str(), device.name.as_str()),
            ("alice", "phone")
        );
        assert_eq!(
            redeem(&store, [1; 32], 2).await,
            Redeem::Refused(Refusal::Used)
        );
        assert_eq!(
            redeem(&store, [2; 32], 3).await,
            Redeem::Refused(Refusal::Unknown)
        );
        // Time travel: the code ends at its expiry time.
        store
            .create_code(&[3; 32], "alice", 10 * MINUTE_MS, 5)
            .await
            .unwrap();
        db.sql("UPDATE enrollment_codes SET expires_at_ms = created_at_ms WHERE digest = decode(repeat('03', 32), 'hex')")
            .await;
        assert_eq!(
            redeem(&store, [3; 32], 4).await,
            Redeem::Refused(Refusal::Expired)
        );
        // The device limit leaves the code unused.
        store
            .create_code(&[4; 32], "alice", 10 * MINUTE_MS, 5)
            .await
            .unwrap();
        let full = store
            .redeem_code(&[4; 32], 1, "phone", &[9; 32], "ip")
            .await
            .unwrap();
        assert_eq!(full, Redeem::TooManyDevices);
        assert!(matches!(
            redeem(&store, [4; 32], 5).await,
            Redeem::Enrolled { .. }
        ));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn postgres_an_account_has_a_limited_number_of_active_codes() {
        let Some(db) = TestDatabase::create().await else {
            eprintln!("INBOX_TEST_PG is not set: skipped");
            return;
        };
        let (a, b) = (db.store().await, db.store().await);
        // Two instances make codes at the same time: the account keeps its limit.
        let digests: Vec<[u8; 32]> = (0..6u8).map(|i| [i; 32]).collect();
        let mut made = Vec::new();
        for (i, digest) in digests.iter().enumerate() {
            let store = if i % 2 == 0 { &a } else { &b };
            made.push(store.create_code(digest, "alice", MINUTE_MS, 3));
        }
        let results = futures_util::future::join_all(made).await;
        let created = results
            .iter()
            .filter(|r| matches!(r, Ok(CodeInsert::Created { .. })))
            .count();
        assert_eq!(created, 3, "{results:?}");
        assert!(
            matches!(
                b.create_code(&[9; 32], "bob", MINUTE_MS, 3).await,
                Ok(CodeInsert::Created { .. })
            ),
            "another account has its own count"
        );
        // An expired code does not count.
        db.sql("UPDATE enrollment_codes SET expires_at_ms = created_at_ms WHERE login = 'alice'")
            .await;
        assert!(matches!(
            a.create_code(&[7; 32], "alice", MINUTE_MS, 3).await,
            Ok(CodeInsert::Created { .. })
        ));
        // An unused code can be withdrawn.
        assert!(a.delete_code(&[7; 32]).await.unwrap());
        assert!(!a.delete_code(&[7; 32]).await.unwrap());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn postgres_a_code_used_on_one_instance_cannot_be_used_on_another() {
        let Some(db) = TestDatabase::create().await else {
            eprintln!("INBOX_TEST_PG is not set: skipped");
            return;
        };
        let (a, b) = (db.store().await, db.store().await);
        a.create_code(&[5; 32], "alice", 10 * MINUTE_MS, 5)
            .await
            .unwrap();
        assert!(matches!(
            redeem(&a, [5; 32], 1).await,
            Redeem::Enrolled { .. }
        ));
        assert_eq!(redeem(&b, [5; 32], 2).await, Redeem::Refused(Refusal::Used));
        // Both instances use the code at the same moment: one wins.
        a.create_code(&[6; 32], "alice", 10 * MINUTE_MS, 5)
            .await
            .unwrap();
        let (first, second) = tokio::join!(redeem(&a, [6; 32], 3), redeem(&b, [6; 32], 4));
        let won = [&first, &second]
            .iter()
            .filter(|r| matches!(r, Redeem::Enrolled { .. }))
            .count();
        assert_eq!(won, 1, "{first:?} {second:?}");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn postgres_a_deleted_account_loses_its_codes_and_devices_and_old_codes_are_purged() {
        let Some(db) = TestDatabase::create().await else {
            eprintln!("INBOX_TEST_PG is not set: skipped");
            return;
        };
        let store = db.store().await;
        store
            .create_code(&[1; 32], "alice", 10 * MINUTE_MS, 5)
            .await
            .unwrap();
        store
            .create_code(&[2; 32], "alice", 10 * MINUTE_MS, 5)
            .await
            .unwrap();
        store
            .create_code(&[3; 32], "bob", 10 * MINUTE_MS, 5)
            .await
            .unwrap();
        let Redeem::Enrolled { device_id: a, .. } = redeem(&store, [1; 32], 1).await else {
            panic!()
        };
        let Redeem::Enrolled { device_id: b, .. } = redeem(&store, [3; 32], 2).await else {
            panic!()
        };
        store.subscribe(&a, "infra").await.unwrap();
        assert_eq!(
            store.revoke_account("alice").await.unwrap(),
            vec![a.clone()]
        );
        assert!(store.find_device(&a).await.unwrap().unwrap().revoked);
        assert!(!store.find_device(&b).await.unwrap().unwrap().revoked);
        assert_eq!(
            redeem(&store, [2; 32], 3).await,
            Redeem::Refused(Refusal::Unknown)
        );
        assert!(store.revoke_account("alice").await.unwrap().is_empty());
        // Retention removes the codes that expired long ago.
        store
            .create_code(&[4; 32], "bob", MINUTE_MS, 5)
            .await
            .unwrap();
        db.sql("UPDATE enrollment_codes SET expires_at_ms = 1 WHERE login = 'bob' AND used_at_ms IS NULL")
            .await;
        assert_eq!(store.purge(0, 0).await.unwrap().2, 1);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn postgres_the_counts_of_the_graph_group_the_alerts_by_bucket_and_group() {
        let Some(db) = TestDatabase::create().await else {
            eprintln!("INBOX_TEST_PG is not set: skipped");
            return;
        };
        let store = db.store().await;
        for (key, group, at) in [
            ("a", "infra", 1_000),
            ("b", "infra", 1_900),
            ("c", "infra", 2_100),
            ("d", "other", 2_200),
            ("e", "infra", 10),
        ] {
            store
                .insert_alert(&NewAlert {
                    kind: Kind::Alert,
                    target: Target::Group(group.into()),
                    title: "t".into(),
                    source: "producer:test".into(),
                    idempotency_key: key.into(),
                })
                .await
                .unwrap();
            db.sql(&format!(
                "UPDATE alerts SET created_at = {at} WHERE idempotency_key = '{key}'"
            ))
            .await;
        }
        let counts = store.alert_counts(1_000, 1_000).await.unwrap();
        let text: Vec<(i64, &str, i64)> = counts
            .iter()
            .map(|n| (n.bucket, n.group.as_str(), n.count))
            .collect();
        assert_eq!(
            text,
            vec![(0, "infra", 2), (1, "infra", 1), (1, "other", 1)]
        );
        assert!(store.alert_counts(100_000, 1_000).await.unwrap().is_empty());
    }

    fn alert_for(target: Target, key: &str) -> NewAlert {
        NewAlert {
            kind: Kind::Alert,
            target,
            title: "t".into(),
            source: "producer:test".into(),
            idempotency_key: key.into(),
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn postgres_an_alert_for_an_account_goes_to_each_active_phone_of_the_account() {
        let Some(db) = TestDatabase::create().await else {
            eprintln!("INBOX_TEST_PG is not set: skipped");
            return;
        };
        let store = db.store().await;
        for (n, login) in [(1u8, "alice"), (2, "alice"), (3, "alice"), (4, "bob")] {
            store
                .create_code(&[n; 32], login, 10 * MINUTE_MS, 5)
                .await
                .unwrap();
        }
        let mut ids = Vec::new();
        for n in 1u8..=4 {
            let Redeem::Enrolled { device_id, .. } = redeem(&store, [n; 32], n).await else {
                panic!("enrolled")
            };
            ids.push(device_id);
        }
        assert!(store.revoke_device(&ids[2]).await.unwrap());
        let InsertOutcome::Created { devices, .. } = store
            .insert_alert(&alert_for(Target::User("alice".into()), "k"))
            .await
            .unwrap()
        else {
            panic!("created")
        };
        let mut expected = vec![ids[0].clone(), ids[1].clone()];
        expected.sort();
        assert_eq!(devices, expected);
        // The cache of the degraded mode knows the same phones.
        store.refresh_cache().await.unwrap();
        assert_eq!(
            store.cached_targets(&Target::User("alice".into())),
            expected
        );
        assert_eq!(
            store.cached_targets(&Target::User("bob".into())),
            vec![ids[3].clone()]
        );
        let pending = store.pending_deliveries(&ids[0], 10, 0).await.unwrap();
        assert_eq!(pending[0].target, Target::User("alice".into()));
        assert!(store
            .pending_deliveries(&ids[3], 10, 0)
            .await
            .unwrap()
            .is_empty());
        let alerts = store.recent_alerts(10, 0, 0).await.unwrap();
        assert_eq!(alerts[0].target, Target::User("alice".into()));
        assert_eq!(alerts[0].targets, 2);
        let counts = store.active_phones_by_login().await.unwrap();
        assert_eq!(
            counts
                .iter()
                .map(|(l, n)| (l.as_str(), *n))
                .collect::<Vec<_>>(),
            vec![("alice", 2), ("bob", 1)]
        );
        // The graph counts the alerts of groups only.
        assert!(store
            .alert_counts(0, 1_000_000_000_000_000)
            .await
            .unwrap()
            .is_empty());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn postgres_an_account_with_no_active_phone_gets_no_alert_and_nothing_is_stored() {
        let Some(db) = TestDatabase::create().await else {
            eprintln!("INBOX_TEST_PG is not set: skipped");
            return;
        };
        let store = db.store().await;
        store
            .create_code(&[1; 32], "alice", 10 * MINUTE_MS, 5)
            .await
            .unwrap();
        let Redeem::Enrolled { device_id, .. } = redeem(&store, [1; 32], 1).await else {
            panic!("enrolled")
        };
        store.revoke_device(&device_id).await.unwrap();
        for login in ["alice", "nobody"] {
            assert!(matches!(
                store
                    .insert_alert(&alert_for(Target::User(login.into()), "k"))
                    .await
                    .unwrap(),
                InsertOutcome::NoPhone
            ));
        }
        assert!(store.recent_alerts(10, 0, 0).await.unwrap().is_empty());
        // The same key for a group and for an account with the same id are two alerts.
        store
            .create_code(&[2; 32], "infra", 10 * MINUTE_MS, 5)
            .await
            .unwrap();
        redeem(&store, [2; 32], 2).await;
        assert!(matches!(
            store
                .insert_alert(&alert_for(Target::User("infra".into()), "k2"))
                .await
                .unwrap(),
            InsertOutcome::Created { .. }
        ));
        assert!(matches!(
            store
                .insert_alert(&alert_for(Target::Group("infra".into()), "k2"))
                .await
                .unwrap(),
            InsertOutcome::Conflict
        ));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn postgres_the_import_of_a_sqlite_file_carries_the_enrollment_codes() {
        let Some(db) = TestDatabase::create().await else {
            eprintln!("INBOX_TEST_PG is not set: skipped");
            return;
        };
        let dir = std::env::temp_dir().join(format!("portal-inbox-import-{}", random_id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("inbox.db");
        {
            let (lite, closed) = super::super::db::Db::open(&path).unwrap();
            lite.call(|c| {
                let now = now_ms();
                super::super::db::insert_code(c, &[1; 32], "alice", now, 10 * MINUTE_MS, 5)?;
                super::super::db::insert_code(c, &[2; 32], "alice", now, 10 * MINUTE_MS, 5)?;
                super::super::db::redeem_code(c, &[1; 32], now, 10, "phone", &[7; 32], "ip")?;
                super::super::db::insert_alert(
                    c,
                    &NewAlert {
                        kind: Kind::Alert,
                        target: Target::Group("infra".into()),
                        title: "disk full".into(),
                        source: "admin:root".into(),
                        idempotency_key: "k".into(),
                    },
                )?;
                Ok(())
            })
            .await
            .unwrap();
            drop(lite);
            assert!(closed.wait(Duration::from_secs(10)));
        }
        let store = db.store().await;
        let imported = store.import_sqlite(&path).await.unwrap();
        assert_eq!((imported.devices, imported.enrollment_codes), (1, 2));
        // The kind of the message comes with the alert.
        let alerts = store.recent_alerts(10, 0, 0).await.unwrap();
        assert_eq!(alerts.len(), 1);
        assert_eq!(alerts[0].kind, Kind::Alert);
        // The used code stays used. The other code still works.
        assert_eq!(
            redeem(&store, [1; 32], 1).await,
            Redeem::Refused(Refusal::Used)
        );
        assert!(matches!(
            redeem(&store, [2; 32], 2).await,
            Redeem::Enrolled { .. }
        ));
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn scope(groups: Option<&[&str]>, login: &str) -> Scope {
        Scope {
            groups: groups.map(|g| g.iter().map(|s| s.to_string()).collect()),
            login: login.into(),
        }
    }

    fn ids(view: &View) -> Vec<&str> {
        view.items.iter().map(|i| i.id.as_str()).collect()
    }

    const ASK: Ask = Ask {
        since_ms: 0,
        limit: 10,
        skip_if_off: false,
    };

    async fn put(db: &TestDatabase, id: &str, kind: &str, target: &str, at: i64) {
        db.sql(&format!(
            "INSERT INTO alerts (id, target_kind, target_id, title, source, idempotency_key, created_at)
             VALUES ('{id}', '{kind}', '{target}', '{id}', 'producer:test', '{id}', {at})"
        ))
        .await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn postgres_an_account_gets_the_alerts_of_its_groups_and_its_own_and_no_other() {
        let Some(db) = TestDatabase::create().await else {
            eprintln!("INBOX_TEST_PG is not set: skipped");
            return;
        };
        let store = db.store().await;
        put(&db, "g-infra", "group", "infra", 100).await;
        put(&db, "g-other", "group", "other", 110).await;
        put(&db, "d-alice", "user", "alice", 120).await;
        put(&db, "d-bob", "user", "bob", 130).await;
        let member = store
            .notify_view(&scope(Some(&["infra"]), "alice"), &ASK)
            .await
            .unwrap();
        assert_eq!(ids(&member), ["d-alice", "g-infra"], "newest first");
        let outsider = store
            .notify_view(&scope(Some(&[]), "carol"), &ASK)
            .await
            .unwrap();
        assert!(outsider.items.is_empty());
        let all = store.notify_view(&scope(None, "root"), &ASK).await.unwrap();
        assert_eq!(ids(&all), ["g-other", "g-infra"]);
        // The store and the rule of the code agree.
        for s in [
            scope(Some(&["infra"]), "alice"),
            scope(Some(&[]), "bob"),
            scope(None, "alice"),
        ] {
            let got = store.notify_view(&s, &ASK).await.unwrap();
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

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn postgres_the_cursor_the_marker_and_the_level_work() {
        let Some(db) = TestDatabase::create().await else {
            eprintln!("INBOX_TEST_PG is not set: skipped");
            return;
        };
        let store = db.store().await;
        let s = scope(Some(&["infra"]), "alice");
        // The first sight of the account sets the marker at the time of the call.
        let first = store.notify_view(&s, &ASK).await.unwrap();
        assert!(first.prefs.read_up_to > 0 && first.unread == 0);
        let far = now_ms() + 3_600_000;
        for n in 1..=5 {
            put(&db, &format!("a{n}"), "group", "infra", far + n * 10).await;
        }
        let page = store
            .notify_view(
                &s,
                &Ask {
                    since_ms: far + 30,
                    limit: 2,
                    skip_if_off: false,
                },
            )
            .await
            .unwrap();
        assert_eq!(ids(&page), ["a5", "a4"]);
        assert_eq!(page.unread, 5);
        assert_eq!(store.notify_mark_read(&s).await.unwrap(), far + 50);
        assert_eq!(store.notify_view(&s, &ASK).await.unwrap().unread, 0);
        // The marker never goes back.
        db.sql("DELETE FROM alerts").await;
        put(&db, "old", "group", "infra", 10).await;
        assert_eq!(store.notify_mark_read(&s).await.unwrap(), far + 50);
        // The level is for one account. Off skips the reads.
        store.notify_set_level("alice", Level::Off).await.unwrap();
        let off = Ask {
            skip_if_off: true,
            ..ASK
        };
        assert!(store.notify_view(&s, &off).await.unwrap().items.is_empty());
        assert_eq!(
            store
                .notify_view(&scope(Some(&["infra"]), "bob"), &off)
                .await
                .unwrap()
                .prefs
                .level,
            Level::All
        );
        store
            .notify_set_level("carol", Level::Important)
            .await
            .unwrap();
        let carol = store
            .notify_view(&scope(Some(&[]), "carol"), &ASK)
            .await
            .unwrap();
        assert_eq!(carol.prefs.level, Level::Important);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn postgres_an_alert_made_on_one_instance_shows_on_the_other() {
        let Some(db) = TestDatabase::create().await else {
            eprintln!("INBOX_TEST_PG is not set: skipped");
            return;
        };
        let (a, b) = (db.store().await, db.store().await);
        let s = scope(Some(&["infra"]), "alice");
        assert!(b.notify_view(&s, &ASK).await.unwrap().items.is_empty());
        a.insert_alert(&alert_for(Target::Group("infra".into()), "k1"))
            .await
            .unwrap();
        let seen = b.notify_view(&s, &ASK).await.unwrap();
        assert_eq!(seen.items.len(), 1);
        assert_eq!(seen.unread, 1);
        // The marker that one instance moves is the marker that the other reads.
        a.notify_mark_read(&s).await.unwrap();
        assert_eq!(b.notify_view(&s, &ASK).await.unwrap().unread, 0);
    }
}
