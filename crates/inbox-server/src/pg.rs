//! PostgreSQL storage: the state of Inbox shared by any number of active instances.
//!
//! Every operation is one transaction, so the delivery guarantees are those of PostgreSQL: an alert and its delivery rows
//! are committed together, and `INSERT ... ON CONFLICT` makes the idempotency key hold across instances. Instances talk to
//! each other through `LISTEN`/`NOTIFY` (wake a device that is connected elsewhere, drop a revoked or replaced session) and
//! through the `instances` table (a heartbeat, so the sessions of a lost instance are closed by the survivors).
//!
//! Connections are plain TCP: Inbox has no TLS stack, and the database is reached inside the private network.

use std::collections::HashMap;
use std::fmt::Display;
use std::time::Duration;

use deadpool_postgres::{Manager, ManagerConfig, Object, Pool, RecyclingMethod, Runtime};
use futures_util::StreamExt;
use tokio::sync::mpsc;
use tokio_postgres::{AsyncMessage, NoTls};

use crate::config::Postgres;
use crate::db::{
    AckOutcome, AlertDetail, AlertSummary, ConnectionView, DbError, DeliveryView, DeviceKey,
    DeviceView, InsertOutcome, NewAlert, PendingDelivery,
};
use crate::hub::LiveInfo;
use crate::secrets::read_secret;
use crate::store::Stats;
use crate::util::{now_ms, random_id};

/// An instance that has not updated its heartbeat for this long is lost: its sessions are closed.
const INSTANCE_TTL_MS: i64 = 30_000;
/// A lost instance row is forgotten after this long.
const INSTANCE_FORGET_MS: i64 = 3_600_000;
/// Device ids per `inbox_wake` notification (a payload is limited to 8000 bytes).
const WAKE_CHUNK: usize = 100;

/// Advisory lock keys: schema changes, enrollment limit and retention must not run twice at once.
const LOCK_SCHEMA: i64 = 0x5041_4745_0001;
const LOCK_ENROLL: i64 = 0x5041_4745_0002;
const LOCK_PURGE: i64 = 0x5041_4745_0003;

const SCHEMA: &[&str] = &[r#"
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
"#];

/// The database clock, in milliseconds: liveness is judged by one clock, not by the instances' own.
const DB_NOW: &str = "(extract(epoch FROM clock_timestamp()) * 1000)::bigint";

fn db_error(e: impl Display) -> DbError {
    DbError::Pg(e.to_string())
}

/// What the other instances tell this one.
#[derive(Debug, PartialEq, Eq)]
pub enum Event {
    /// New deliveries exist for these devices.
    Wake(Vec<String>),
    /// The device was revoked: close its session.
    Revoked(String),
    /// The device opened a session on another instance at `connected_at`: close an older one here.
    Replaced { device: String, instance: String, connected_at: i64 },
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

pub struct PgStore {
    pool: Pool,
    config: tokio_postgres::Config,
    instance: String,
}

impl PgStore {
    /// Connects, migrates the schema and registers this instance.
    pub async fn connect(cfg: &Postgres) -> Result<PgStore, String> {
        let password = read_secret(&cfg.password_file, "postgres.password_file")?;
        let mut config = tokio_postgres::Config::new();
        config
            .host(&cfg.host)
            .port(cfg.port)
            .dbname(&cfg.database)
            .user(&cfg.user)
            .password(password.as_bytes())
            .connect_timeout(cfg.connect_timeout)
            .keepalives(true)
            .keepalives_idle(Duration::from_secs(20))
            .application_name("inbox");
        let manager = Manager::from_config(
            config.clone(),
            NoTls,
            ManagerConfig { recycling_method: RecyclingMethod::Fast },
        );
        let pool = Pool::builder(manager)
            .max_size(cfg.pool_size as usize)
            .runtime(Runtime::Tokio1)
            .wait_timeout(Some(Duration::from_secs(5)))
            .create_timeout(Some(cfg.connect_timeout))
            .recycle_timeout(Some(cfg.connect_timeout))
            .build()
            .map_err(|e| format!("postgres: {e}"))?;
        let store = PgStore { pool, config, instance: random_id() };
        store.migrate().await.map_err(|e| format!("postgres migration: {e}"))?;
        store.register_instance().await.map_err(|e| format!("postgres: {e}"))?;
        tracing::info!(event = "postgres_ready", instance = %store.instance, host = %cfg.host, database = %cfg.database);
        Ok(store)
    }

    pub fn instance(&self) -> &str {
        &self.instance
    }

    async fn client(&self) -> Result<Object, DbError> {
        self.pool.get().await.map_err(db_error)
    }

    pub async fn ping(&self) -> Result<(), DbError> {
        let c = self.client().await?;
        c.query_one("SELECT 1", &[]).await.map(|_| ()).map_err(db_error)
    }

    async fn migrate(&self) -> Result<(), DbError> {
        let mut c = self.client().await?;
        let tx = c.transaction().await.map_err(db_error)?;
        tx.execute("SELECT pg_advisory_xact_lock($1)", &[&LOCK_SCHEMA]).await.map_err(db_error)?;
        tx.batch_execute("CREATE TABLE IF NOT EXISTS schema_migrations (version BIGINT PRIMARY KEY, applied_at BIGINT NOT NULL)")
            .await
            .map_err(db_error)?;
        let current: i64 = tx
            .query_one("SELECT COALESCE(MAX(version), 0)::bigint FROM schema_migrations", &[])
            .await
            .map_err(db_error)?
            .get(0);
        if current as usize > SCHEMA.len() {
            return Err(DbError::Pg(format!(
                "database schema version {current} is newer than this binary ({})",
                SCHEMA.len()
            )));
        }
        for (i, sql) in SCHEMA.iter().enumerate().skip(current as usize) {
            tx.batch_execute(sql).await.map_err(db_error)?;
            tx.execute(
                "INSERT INTO schema_migrations (version, applied_at) VALUES ($1, $2)",
                &[&((i + 1) as i64), &now_ms()],
            )
            .await
            .map_err(db_error)?;
            tracing::info!(event = "migration_applied", version = i + 1);
        }
        tx.commit().await.map_err(db_error)
    }

    // ------------------------------------------------------------ instances

    async fn register_instance(&self) -> Result<(), DbError> {
        let c = self.client().await?;
        let host = std::env::var("HOSTNAME").unwrap_or_else(|_| "unknown".into());
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
                        tracing::warn!(event = "postgres_listen_lost", error = %e);
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
        let (client, mut connection) =
            self.config.connect(NoTls).await.map_err(|e| e.to_string())?;
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

    pub async fn enroll_device(
        &self,
        max_devices: u32,
        name: &str,
        user_name: &str,
        public_key: &[u8],
        ip: &str,
    ) -> Result<Option<String>, DbError> {
        let mut c = self.client().await?;
        let tx = c.transaction().await.map_err(db_error)?;
        // Two instances enrolling at the same time must not both take the last place.
        tx.execute("SELECT pg_advisory_xact_lock($1)", &[&LOCK_ENROLL]).await.map_err(db_error)?;
        let active: i64 = tx
            .query_one("SELECT COUNT(*) FROM devices WHERE revoked_at IS NULL", &[])
            .await
            .map_err(db_error)?
            .get(0);
        if active >= i64::from(max_devices) {
            return Ok(None);
        }
        let id = random_id();
        tx.execute(
            "INSERT INTO devices (id, name, user_name, public_key, enrolled_at, enrolled_ip) VALUES ($1, $2, $3, $4, $5, $6)",
            &[&id, &name, &user_name, &public_key, &now_ms(), &ip],
        )
        .await
        .map_err(db_error)?;
        tx.commit().await.map_err(db_error)?;
        Ok(Some(id))
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

    pub async fn set_user_name(&self, id: &str, user_name: &str) -> Result<(), DbError> {
        let mut c = self.client().await?;
        let tx = c.transaction().await.map_err(db_error)?;
        tx.execute("UPDATE devices SET user_name = $2 WHERE id = $1", &[&id, &user_name])
            .await
            .map_err(db_error)?;
        // The open connection is attributed to the new pseudonym too.
        tx.execute(
            "UPDATE connections SET user_name = $2 WHERE device_id = $1 AND disconnected_at IS NULL",
            &[&id, &user_name],
        )
        .await
        .map_err(db_error)?;
        tx.commit().await.map_err(db_error)
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
            tx.execute("SELECT pg_notify('inbox_kick', $1)", &[&format!("revoked {id}")])
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
        Ok((Self::subscriptions_of(&**c, id).await?, user_name.unwrap_or_default()))
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
            .execute("DELETE FROM subscriptions WHERE NOT (group_id = ANY($1::text[]))", &[&groups])
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
                "INSERT INTO alerts (id, group_id, title, source, idempotency_key, created_at)
                 VALUES ($1, $2, $3, $4, $5, $6)
                 ON CONFLICT (source, idempotency_key) DO NOTHING RETURNING id",
                &[
                    &id,
                    &alert.group,
                    &alert.title,
                    &alert.source,
                    &alert.idempotency_key,
                    &now_ms(),
                ],
            )
            .await
            .map_err(db_error)?;
        if created.is_none() {
            // Another request, possibly on another instance, stored it first: it has committed by now.
            let row = tx
                .query_one(
                    "SELECT id, group_id, title FROM alerts WHERE source = $1 AND idempotency_key = $2",
                    &[&alert.source, &alert.idempotency_key],
                )
                .await
                .map_err(db_error)?;
            let (existing, group, title): (String, String, String) =
                (row.get(0), row.get(1), row.get(2));
            if group != alert.group || title != alert.title {
                return Ok(InsertOutcome::Conflict);
            }
            let devices = Self::delivery_devices(&*tx, &existing).await?;
            return Ok(InsertOutcome::Duplicate { alert_id: existing, devices });
        }
        tx.execute(
            "INSERT INTO deliveries (alert_id, device_id)
             SELECT $1, s.device_id FROM subscriptions s
             JOIN devices d ON d.id = s.device_id
             WHERE s.group_id = $2 AND d.revoked_at IS NULL
             ORDER BY s.device_id",
            &[&id, &alert.group],
        )
        .await
        .map_err(db_error)?;
        let devices = Self::delivery_devices(&*tx, &id).await?;
        // Delivered at commit: the devices connected to the other instances are woken only for a stored alert.
        for chunk in devices.chunks(WAKE_CHUNK) {
            tx.execute("SELECT pg_notify('inbox_wake', $1)", &[&chunk.join(" ")])
                .await
                .map_err(db_error)?;
        }
        tx.commit().await.map_err(db_error)?;
        Ok(InsertOutcome::Created { alert_id: id, devices })
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
                "SELECT d.seq, a.id, a.group_id, a.title, a.created_at
                 FROM deliveries d JOIN alerts a ON a.id = d.alert_id
                 WHERE d.device_id = $1 AND d.received_at IS NULL AND a.created_at >= $3
                 ORDER BY d.seq LIMIT $2",
                &[&id, &i64::from(limit), &cutoff],
            )
            .await
            .map_err(db_error)?;
        Ok(rows
            .iter()
            .map(|r| PendingDelivery {
                seq: r.get(0),
                alert_id: r.get(1),
                group: r.get(2),
                title: r.get(3),
                created_at: r.get(4),
            })
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
            AckOutcome { seq, first_receipt_latency_ms: first.then_some(now - created_at) }
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
            .query("SELECT device_id, group_id FROM subscriptions ORDER BY group_id", &[])
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

    fn summary(r: &tokio_postgres::Row) -> AlertSummary {
        AlertSummary {
            id: r.get(0),
            group: r.get(1),
            title: r.get(2),
            source: r.get(3),
            created_at: r.get(4),
            targets: r.get(5),
            received: r.get(6),
            expired: 0,
            missing: Vec::new(),
        }
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
                "SELECT a.id, a.group_id, a.title, a.source, a.created_at, COUNT(d.seq), COUNT(d.received_at)
                 FROM alerts a LEFT JOIN deliveries d ON d.alert_id = a.id
                 WHERE a.created_at >= $2 GROUP BY a.id ORDER BY a.created_at DESC LIMIT $1",
                &[&i64::from(limit), &since_ms],
            )
            .await
            .map_err(db_error)?;
        let mut alerts: Vec<AlertSummary> =
            rows.iter().map(Self::summary).map(|a| crate::db::with_expiry(a, cutoff)).collect();
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

    pub async fn alert_detail(
        &self,
        id: &str,
        cutoff: i64,
    ) -> Result<Option<AlertDetail>, DbError> {
        let c = self.client().await?;
        let Some(row) = c
            .query_opt(
                "SELECT a.id, a.group_id, a.title, a.source, a.created_at, COUNT(d.seq), COUNT(d.received_at)
                 FROM alerts a LEFT JOIN deliveries d ON d.alert_id = a.id WHERE a.id = $1 GROUP BY a.id",
                &[&id],
            )
            .await
            .map_err(db_error)?
        else {
            return Ok(None);
        };
        let mut summary = crate::db::with_expiry(Self::summary(&row), cutoff);
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
        Ok(Some(AlertDetail { summary, deliveries }))
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
        Ok(Stats { pending, subscribed, enrolled: enrolled as u32 })
    }

    // ------------------------------------------------------------ retention

    pub async fn purge(
        &self,
        alert_cutoff: i64,
        connection_cutoff: i64,
    ) -> Result<(usize, usize), DbError> {
        let mut c = self.client().await?;
        let tx = c.transaction().await.map_err(db_error)?;
        // One instance purges at a time; the others skip this round.
        let locked: bool = tx
            .query_one("SELECT pg_try_advisory_xact_lock($1)", &[&LOCK_PURGE])
            .await
            .map_err(db_error)?
            .get(0);
        if !locked {
            return Ok((0, 0));
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
        tx.commit().await.map_err(db_error)?;
        Ok((alerts as usize, connections as usize))
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
}
