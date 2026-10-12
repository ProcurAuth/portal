//! Storage front: SQLite (one instance owns the file) or PostgreSQL (any number of instances share the database).
//!
//! Handlers never know which one they talk to. Each operation here is one storage transaction, named after what the
//! protocol does with it; the SQLite side runs the functions of `db.rs`, the PostgreSQL side those of `pg.rs`.

use std::collections::HashMap;
use std::sync::Arc;

use crate::db::{
    self, AckOutcome, AlertDetail, AlertSummary, ConnectionView, Db, DbError, DeviceKey,
    DeviceView, InsertOutcome, NewAlert, PendingDelivery,
};
use crate::hub::LiveInfo;
use crate::pg::PgStore;

#[derive(Clone)]
pub enum Store {
    Sqlite(Db),
    Pg(Arc<PgStore>),
}

/// Everything the periodic statistics task needs, read together.
pub struct Stats {
    /// (pending deliveries, age in ms of the oldest one)
    pub pending: (i64, i64),
    /// Non-revoked devices with a subscription: (id, last_seen_at)
    pub subscribed: Vec<(String, Option<i64>)>,
    pub enrolled: u32,
}

impl Store {
    pub fn is_shared(&self) -> bool {
        matches!(self, Store::Pg(_))
    }

    /// Cheap round trip, for readiness.
    pub async fn ping(&self) -> Result<(), DbError> {
        match self {
            Store::Sqlite(db) => db
                .call(|c| Ok(c.query_row("SELECT 1", [], |r| r.get::<_, i64>(0))?))
                .await
                .map(|_| ()),
            Store::Pg(pg) => pg.ping().await,
        }
    }

    // ------------------------------------------------------------ devices

    /// Stores a new device unless `max_devices` active ones exist already (`None`).
    pub async fn enroll_device(
        &self,
        max_devices: u32,
        name: String,
        user_name: String,
        public_key: Vec<u8>,
        ip: String,
    ) -> Result<Option<String>, DbError> {
        match self {
            Store::Sqlite(db) => {
                db.call(move |c| {
                    if db::count_active_devices(c)? >= max_devices {
                        return Ok(None);
                    }
                    Ok(Some(db::insert_device(c, &name, &user_name, &public_key, &ip)?))
                })
                .await
            }
            Store::Pg(pg) => {
                pg.enroll_device(max_devices, &name, &user_name, &public_key, &ip).await
            }
        }
    }

    pub async fn find_device(&self, id: &str) -> Result<Option<DeviceKey>, DbError> {
        match self {
            Store::Sqlite(db) => {
                let id = id.to_string();
                db.call(move |c| Ok(db::find_device(c, &id)?)).await
            }
            Store::Pg(pg) => pg.find_device(id).await,
        }
    }

    pub async fn set_user_name(&self, id: &str, user_name: &str) -> Result<(), DbError> {
        match self {
            Store::Sqlite(db) => {
                let (id, name) = (id.to_string(), user_name.to_string());
                db.call(move |c| Ok(db::set_user_name(c, &id, &name)?)).await
            }
            Store::Pg(pg) => pg.set_user_name(id, user_name).await,
        }
    }

    /// Revokes a device and drops its subscriptions. True when it was active.
    pub async fn revoke_device(&self, id: &str) -> Result<bool, DbError> {
        match self {
            Store::Sqlite(db) => {
                let id = id.to_string();
                db.call(move |c| Ok(db::revoke_device(c, &id)?)).await
            }
            Store::Pg(pg) => pg.revoke_device(id).await,
        }
    }

    /// Opens a connection record and returns its id.
    pub async fn record_connect(
        &self,
        id: &str,
        ip: &str,
        app_version: &str,
    ) -> Result<i64, DbError> {
        match self {
            Store::Sqlite(db) => {
                let (id, ip, ver) = (id.to_string(), ip.to_string(), app_version.to_string());
                db.call(move |c| Ok(db::record_connect(c, &id, &ip, &ver)?)).await
            }
            Store::Pg(pg) => pg.record_connect(id, ip, app_version).await,
        }
    }

    pub async fn record_disconnect(
        &self,
        connection_id: i64,
        device_id: &str,
        reason: &str,
    ) -> Result<(), DbError> {
        match self {
            Store::Sqlite(db) => {
                let (id, reason) = (device_id.to_string(), reason.to_string());
                db.call(move |c| Ok(db::record_disconnect(c, connection_id, &id, &reason)?)).await
            }
            Store::Pg(pg) => pg.record_disconnect(connection_id, device_id, reason).await,
        }
    }

    pub async fn touch_seen(&self, seen: Vec<(String, i64)>) -> Result<(), DbError> {
        match self {
            Store::Sqlite(db) => db.call(move |c| Ok(db::touch_seen(c, &seen)?)).await,
            Store::Pg(pg) => pg.touch_seen(&seen).await,
        }
    }

    // ------------------------------------------------------------ subscriptions

    /// What a session needs to say welcome: subscribed groups and the pseudonym.
    pub async fn profile(&self, id: &str) -> Result<(Vec<String>, String), DbError> {
        match self {
            Store::Sqlite(db) => {
                let id = id.to_string();
                db.call(move |c| {
                    let user_name =
                        db::find_device(c, &id)?.map(|d| d.user_name).unwrap_or_default();
                    Ok((db::subscriptions(c, &id)?, user_name))
                })
                .await
            }
            Store::Pg(pg) => pg.profile(id).await,
        }
    }

    /// Subscribes and returns the groups the device is now subscribed to.
    pub async fn subscribe(&self, id: &str, group: &str) -> Result<Vec<String>, DbError> {
        match self {
            Store::Sqlite(db) => {
                let (id, group) = (id.to_string(), group.to_string());
                db.call(move |c| {
                    db::set_subscription(c, &id, &group)?;
                    Ok(db::subscriptions(c, &id)?)
                })
                .await
            }
            Store::Pg(pg) => pg.subscribe(id, group).await,
        }
    }

    pub async fn unsubscribe(&self, id: &str, group: &str) -> Result<Vec<String>, DbError> {
        match self {
            Store::Sqlite(db) => {
                let (id, group) = (id.to_string(), group.to_string());
                db.call(move |c| {
                    db::delete_subscription(c, &id, &group)?;
                    Ok(db::subscriptions(c, &id)?)
                })
                .await
            }
            Store::Pg(pg) => pg.unsubscribe(id, group).await,
        }
    }

    /// Removes subscriptions to groups no longer declared in the configuration.
    pub async fn prune_subscriptions(&self, groups: Vec<String>) -> Result<usize, DbError> {
        match self {
            Store::Sqlite(db) => db.call(move |c| Ok(db::prune_subscriptions(c, &groups)?)).await,
            Store::Pg(pg) => pg.prune_subscriptions(&groups).await,
        }
    }

    // ------------------------------------------------------------ alerts and deliveries

    pub async fn insert_alert(&self, alert: NewAlert) -> Result<InsertOutcome, DbError> {
        match self {
            Store::Sqlite(db) => db.call(move |c| Ok(db::insert_alert(c, &alert)?)).await,
            Store::Pg(pg) => pg.insert_alert(&alert).await,
        }
    }

    pub async fn pending_deliveries(
        &self,
        id: &str,
        limit: u32,
        cutoff: i64,
    ) -> Result<Vec<PendingDelivery>, DbError> {
        match self {
            Store::Sqlite(db) => {
                let id = id.to_string();
                db.call(move |c| Ok(db::pending_deliveries(c, &id, limit, cutoff)?)).await
            }
            Store::Pg(pg) => pg.pending_deliveries(id, limit, cutoff).await,
        }
    }

    pub async fn mark_sent(&self, seqs: Vec<i64>) -> Result<(), DbError> {
        match self {
            Store::Sqlite(db) => db.call(move |c| Ok(db::mark_sent(c, &seqs)?)).await,
            Store::Pg(pg) => pg.mark_sent(&seqs).await,
        }
    }

    pub async fn ack(
        &self,
        device_id: &str,
        alert_id: &str,
    ) -> Result<Option<AckOutcome>, DbError> {
        match self {
            Store::Sqlite(db) => {
                let (id, alert) = (device_id.to_string(), alert_id.to_string());
                db.call(move |c| Ok(db::ack(c, &id, &alert)?)).await
            }
            Store::Pg(pg) => pg.ack(device_id, alert_id).await,
        }
    }

    // ------------------------------------------------------------ admin views and statistics

    /// Devices with their pending counts, and the pending gauges.
    pub async fn overview(&self, cutoff: i64) -> Result<(Vec<DeviceView>, (i64, i64)), DbError> {
        match self {
            Store::Sqlite(db) => {
                db.call(move |c| Ok((db::list_devices(c, cutoff)?, db::pending_stats(c, cutoff)?)))
                    .await
            }
            Store::Pg(pg) => pg.overview(cutoff).await,
        }
    }

    pub async fn recent_alerts(
        &self,
        limit: u32,
        since_ms: i64,
        cutoff: i64,
    ) -> Result<Vec<AlertSummary>, DbError> {
        match self {
            Store::Sqlite(db) => {
                db.call(move |c| Ok(db::recent_alerts(c, limit, since_ms, cutoff)?)).await
            }
            Store::Pg(pg) => pg.recent_alerts(limit, since_ms, cutoff).await,
        }
    }

    pub async fn alert_detail(
        &self,
        id: &str,
        cutoff: i64,
    ) -> Result<Option<AlertDetail>, DbError> {
        match self {
            Store::Sqlite(db) => {
                let id = id.to_string();
                db.call(move |c| Ok(db::alert_detail(c, &id, cutoff)?)).await
            }
            Store::Pg(pg) => pg.alert_detail(id, cutoff).await,
        }
    }

    pub async fn recent_connections(
        &self,
        limit: u32,
        user: Option<String>,
    ) -> Result<Vec<ConnectionView>, DbError> {
        match self {
            Store::Sqlite(db) => {
                db.call(move |c| Ok(db::recent_connections(c, limit, user.as_deref())?)).await
            }
            Store::Pg(pg) => pg.recent_connections(limit, user.as_deref()).await,
        }
    }

    pub async fn stats(&self, cutoff: i64) -> Result<Stats, DbError> {
        match self {
            Store::Sqlite(db) => {
                db.call(move |c| {
                    Ok(Stats {
                        pending: db::pending_stats(c, cutoff)?,
                        subscribed: db::subscribed_devices(c)?,
                        enrolled: db::count_active_devices(c)?,
                    })
                })
                .await
            }
            Store::Pg(pg) => pg.stats(cutoff).await,
        }
    }

    /// Sessions open on the other instances (empty with SQLite: there is only this instance).
    pub async fn remote_sessions(&self) -> Result<HashMap<String, LiveInfo>, DbError> {
        match self {
            Store::Sqlite(_) => Ok(HashMap::new()),
            Store::Pg(pg) => pg.live_sessions().await,
        }
    }

    // ------------------------------------------------------------ retention

    pub async fn purge(
        &self,
        alert_cutoff: i64,
        connection_cutoff: i64,
    ) -> Result<(usize, usize), DbError> {
        match self {
            Store::Sqlite(db) => {
                db.call(move |c| Ok(db::purge(c, alert_cutoff, connection_cutoff)?)).await
            }
            Store::Pg(pg) => pg.purge(alert_cutoff, connection_cutoff).await,
        }
    }
}
