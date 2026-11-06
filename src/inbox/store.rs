//! The storage front: SQLite or PostgreSQL.
//!
//! The handlers do not know which one they use. Each operation is one storage transaction, named
//! after what the protocol does with it. The SQLite side runs the functions of `db.rs`. The
//! PostgreSQL side runs the functions of `pg.rs`.

use std::collections::HashMap;
use std::sync::Arc;

use super::db::{
    self, AckOutcome, AlertDetail, AlertSummary, CodeInsert, ConnectionView, Db, DbError,
    DeviceKey, DeviceLimits, DeviceView, InsertOutcome, NewAlert, PendingDelivery, Redeem,
};
use super::hub::LiveInfo;
use super::notify::{Ask, Level, Scope, View};
use super::pg::{PgStore, RecentAlert};
use super::spool::Spool;
use super::util::now_ms;

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

    /// The alerts accepted without the database: only PostgreSQL has a failover window to bridge.
    pub fn spool(&self) -> Option<&Spool> {
        match self {
            Store::Sqlite(_) => None,
            Store::Pg(pg) => Some(pg.spool()),
        }
    }

    pub fn pg(&self) -> Option<&Arc<PgStore>> {
        match self {
            Store::Sqlite(_) => None,
            Store::Pg(pg) => Some(pg),
        }
    }

    /// PostgreSQL has no usable primary right now and this instance runs on its memory.
    pub fn is_degraded(&self) -> bool {
        matches!(self, Store::Pg(pg) if pg.is_down())
    }

    /// Cheap round trip, for readiness.
    pub async fn ping(&self) -> Result<(), DbError> {
        match self {
            Store::Sqlite(db) => db
                .call(|c| Ok(c.query_row("SELECT 1", [], |r| r.get::<_, i64>(0))?))
                .await
                .map(|_| ()),
            Store::Pg(pg) => pg.guard(pg.ping()).await,
        }
    }

    // ------------------------------------------------------------ devices

    /// Stores the digest of a new enrollment code of `login`, unless the account has `max_active`
    /// codes that are not used and not expired. With `stored`, the account is an account of the
    /// database, and the code needs its row (see `db::insert_code`).
    pub async fn create_code(
        &self,
        digest: [u8; 32],
        login: String,
        ttl_ms: i64,
        max_active: u32,
        stored: bool,
    ) -> Result<CodeInsert, DbError> {
        match self {
            Store::Sqlite(db) => {
                db.call(move |c| {
                    Ok(db::insert_code(
                        c,
                        &digest,
                        &login,
                        super::util::now_ms(),
                        ttl_ms,
                        max_active,
                        stored,
                    )?)
                })
                .await
            }
            Store::Pg(pg) => {
                pg.guard(pg.create_code(&digest, &login, ttl_ms, max_active, stored))
                    .await
            }
        }
    }

    /// Removes a code that is not used.
    pub async fn delete_code(&self, digest: [u8; 32]) -> Result<bool, DbError> {
        match self {
            Store::Sqlite(db) => db.call(move |c| Ok(db::delete_code(c, &digest)?)).await,
            Store::Pg(pg) => pg.guard(pg.delete_code(&digest)).await,
        }
    }

    /// Uses a code and stores the device of its account, in one transaction.
    pub async fn redeem_code(
        &self,
        digest: [u8; 32],
        limits: DeviceLimits,
        name: String,
        public_key: Vec<u8>,
        ip: String,
    ) -> Result<Redeem, DbError> {
        match self {
            Store::Sqlite(db) => {
                db.call(move |c| {
                    Ok(db::redeem_code(
                        c,
                        &digest,
                        super::util::now_ms(),
                        limits,
                        &name,
                        &public_key,
                        &ip,
                    )?)
                })
                .await
            }
            Store::Pg(pg) => {
                pg.guard(pg.redeem_code(&digest, limits, &name, &public_key, &ip))
                    .await
            }
        }
    }

    /// Ends the inbox state of the account `login`: its unused codes, its devices and the rows that
    /// name the login (see `db::end_account`). The result is the ids that this call revoked.
    pub async fn end_account(&self, login: &str) -> Result<Vec<String>, DbError> {
        match self {
            Store::Sqlite(db) => {
                let login = login.to_string();
                db.call(move |c| Ok(db::end_account(c, &login)?)).await
            }
            Store::Pg(pg) => {
                let ids = pg.guard(pg.end_account(login)).await?;
                for id in &ids {
                    pg.cache_revoked(id);
                }
                Ok(ids)
            }
        }
    }

    pub async fn find_device(&self, id: &str) -> Result<Option<DeviceKey>, DbError> {
        match self {
            Store::Sqlite(db) => {
                let id = id.to_string();
                db.call(move |c| Ok(db::find_device(c, &id)?)).await
            }
            Store::Pg(pg) => match pg.guard(pg.find_device(id)).await {
                Ok(found) => {
                    pg.cache_device(id, found.clone());
                    Ok(found)
                }
                // A phone this instance has seen (or read from the database lately) can still open a session.
                Err(e) if e.is_unavailable() => pg.cached_device(id).map(Some).ok_or(e),
                Err(e) => Err(e),
            },
        }
    }

    /// Revokes a device and drops its subscriptions. True when it was active.
    pub async fn revoke_device(&self, id: &str) -> Result<bool, DbError> {
        match self {
            Store::Sqlite(db) => {
                let id = id.to_string();
                db.call(move |c| Ok(db::revoke_device(c, &id)?)).await
            }
            Store::Pg(pg) => {
                let revoked = pg.guard(pg.revoke_device(id)).await?;
                pg.cache_revoked(id);
                Ok(revoked)
            }
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
                db.call(move |c| Ok(db::record_connect(c, &id, &ip, &ver)?))
                    .await
            }
            // Without the database the session goes on unrecorded: a negative id says so.
            Store::Pg(pg) => match pg.guard(pg.record_connect(id, ip, app_version)).await {
                Err(e) if e.is_unavailable() => Ok(-1),
                other => other,
            },
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
                db.call(move |c| Ok(db::record_disconnect(c, connection_id, &id, &reason)?))
                    .await
            }
            Store::Pg(pg) => {
                if connection_id < 0 {
                    return Ok(());
                }
                match pg
                    .guard(pg.record_disconnect(connection_id, device_id, reason))
                    .await
                {
                    Err(e) if e.is_unavailable() => Ok(()),
                    other => other,
                }
            }
        }
    }

    pub async fn touch_seen(&self, seen: Vec<(String, i64)>) -> Result<(), DbError> {
        match self {
            Store::Sqlite(db) => db.call(move |c| Ok(db::touch_seen(c, &seen)?)).await,
            Store::Pg(pg) => match pg.guard(pg.touch_seen(&seen)).await {
                Err(e) if e.is_unavailable() => Ok(()),
                other => other,
            },
        }
    }

    // ------------------------------------------------------------ subscriptions

    /// What a session needs to say welcome: the subscribed groups and the login of the account.
    pub async fn profile(&self, id: &str) -> Result<(Vec<String>, String), DbError> {
        match self {
            Store::Sqlite(db) => {
                let id = id.to_string();
                db.call(move |c| {
                    let user_name = db::find_device(c, &id)?
                        .map(|d| d.user_name)
                        .unwrap_or_default();
                    Ok((db::subscriptions(c, &id)?, user_name))
                })
                .await
            }
            Store::Pg(pg) => match pg.guard(pg.profile(id)).await {
                Err(e) if e.is_unavailable() => pg.cached_profile(id).ok_or(e),
                other => other,
            },
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
            Store::Pg(pg) => {
                let subs = pg.guard(pg.subscribe(id, group)).await?;
                pg.cache_subscriptions(id, subs.clone());
                Ok(subs)
            }
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
            Store::Pg(pg) => {
                let subs = pg.guard(pg.unsubscribe(id, group)).await?;
                pg.cache_subscriptions(id, subs.clone());
                Ok(subs)
            }
        }
    }

    /// Removes subscriptions to groups no longer declared in the configuration.
    pub async fn prune_subscriptions(&self, groups: Vec<String>) -> Result<usize, DbError> {
        match self {
            Store::Sqlite(db) => {
                db.call(move |c| Ok(db::prune_subscriptions(c, &groups)?))
                    .await
            }
            Store::Pg(pg) => pg.guard(pg.prune_subscriptions(&groups)).await,
        }
    }

    // ------------------------------------------------------------ alerts and deliveries

    /// Stores the alert and its deliveries. With `audience`, only the phones of these accounts get
    /// a delivery (see `db::insert_alert`).
    pub async fn insert_alert(
        &self,
        alert: NewAlert,
        audience: Option<Vec<String>>,
    ) -> Result<InsertOutcome, DbError> {
        match self {
            Store::Sqlite(db) => {
                db.call(move |c| Ok(db::insert_alert(c, &alert, audience.as_deref())?))
                    .await
            }
            Store::Pg(pg) => {
                let outcome = pg
                    .guard(pg.insert_alert(&alert, audience.as_deref()))
                    .await?;
                if let InsertOutcome::Created { alert_id, .. }
                | InsertOutcome::Duplicate { alert_id, .. } = &outcome
                {
                    // A retry of this alert during an outage finds it under the id the phones know.
                    pg.cache_alert(
                        &alert.source,
                        &alert.idempotency_key,
                        RecentAlert {
                            id: alert_id.clone(),
                            target: alert.target,
                            title: alert.title,
                            at: now_ms(),
                        },
                    );
                }
                Ok(outcome)
            }
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
                db.call(move |c| Ok(db::pending_deliveries(c, &id, limit, cutoff)?))
                    .await
            }
            Store::Pg(pg) => {
                let mut rows = match pg.guard(pg.pending_deliveries(id, limit, cutoff)).await {
                    Ok(rows) => rows,
                    Err(e) if e.is_unavailable() => Vec::new(),
                    Err(e) => return Err(e),
                };
                // The alerts accepted without the database come after the stored ones; one that is in both is sent once.
                for p in pg.spool().pending_for(id, limit as usize, cutoff) {
                    if rows.len() >= limit as usize {
                        break;
                    }
                    if !rows.iter().any(|r| r.alert_id == p.alert_id) {
                        rows.push(p);
                    }
                }
                Ok(rows)
            }
        }
    }

    pub async fn mark_sent(&self, seqs: Vec<i64>) -> Result<(), DbError> {
        match self {
            Store::Sqlite(db) => db.call(move |c| Ok(db::mark_sent(c, &seqs)?)).await,
            Store::Pg(pg) => {
                // Negative numbers are deliveries of the spool, which has its own record of what was sent.
                let (spooled, stored): (Vec<i64>, Vec<i64>) =
                    seqs.into_iter().partition(|s| *s < 0);
                if !spooled.is_empty() {
                    pg.spool().mark_sent(&spooled, now_ms());
                }
                if stored.is_empty() {
                    return Ok(());
                }
                match pg.guard(pg.mark_sent(&stored)).await {
                    Err(e) if e.is_unavailable() => Ok(()),
                    other => other,
                }
            }
        }
    }

    /// Removes the deliveries that their phone has not received: the account may no longer see the
    /// service of the alert (see `db::withdraw_deliveries`). Without the database, a delivery of
    /// the database stays, and the next try removes it.
    pub async fn withdraw(&self, seqs: Vec<i64>) -> Result<usize, DbError> {
        match self {
            Store::Sqlite(db) => {
                db.call(move |c| Ok(db::withdraw_deliveries(c, &seqs)?))
                    .await
            }
            Store::Pg(pg) => {
                // Negative numbers are deliveries of the spool.
                let (spooled, stored): (Vec<i64>, Vec<i64>) =
                    seqs.into_iter().partition(|s| *s < 0);
                let mut removed = pg.spool().withdraw(&spooled);
                if !stored.is_empty() {
                    match pg.guard(pg.withdraw_deliveries(&stored)).await {
                        Ok(n) => removed += n,
                        Err(e) if e.is_unavailable() => {}
                        Err(e) => return Err(e),
                    }
                }
                Ok(removed)
            }
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
            Store::Pg(pg) => {
                let now = now_ms();
                if let Some(outcome) = pg.spool().ack(device_id, alert_id, now) {
                    return Ok(Some(outcome));
                }
                match pg.guard(pg.ack(device_id, alert_id)).await {
                    // The receipt is written when the database is back.
                    Err(e) if e.is_unavailable() => {
                        pg.spool().note_db_ack(device_id, alert_id, now);
                        Ok(None)
                    }
                    other => other,
                }
            }
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
            Store::Pg(pg) => pg.guard(pg.overview(cutoff)).await,
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
                db.call(move |c| Ok(db::recent_alerts(c, limit, since_ms, cutoff)?))
                    .await
            }
            Store::Pg(pg) => pg.guard(pg.recent_alerts(limit, since_ms, cutoff)).await,
        }
    }

    // ------------------------------------------------------------ notifications of the web

    /// The alerts that an account receives, with its preferences (see `notify.rs`).
    pub async fn notify_view(&self, scope: &Scope, ask: Ask) -> Result<View, DbError> {
        match self {
            Store::Sqlite(db) => {
                let scope = scope.clone();
                db.call(move |c| Ok(db::notify_view(c, &scope, &ask, now_ms())?))
                    .await
            }
            Store::Pg(pg) => pg.guard(pg.notify_view(scope, &ask)).await,
        }
    }

    /// Moves the marker "read up to" of an account to its newest alert.
    pub async fn notify_mark_read(&self, scope: &Scope) -> Result<i64, DbError> {
        match self {
            Store::Sqlite(db) => {
                let scope = scope.clone();
                db.call(move |c| Ok(db::notify_mark_read(c, &scope, now_ms())?))
                    .await
            }
            Store::Pg(pg) => pg.guard(pg.notify_mark_read(scope)).await,
        }
    }

    /// Sets the level of the toasts of an account.
    pub async fn notify_set_level(&self, login: &str, level: Level) -> Result<(), DbError> {
        match self {
            Store::Sqlite(db) => {
                let login = login.to_string();
                db.call(move |c| Ok(db::notify_set_level(c, &login, level, now_ms())?))
                    .await
            }
            Store::Pg(pg) => pg.guard(pg.notify_set_level(login, level)).await,
        }
    }

    /// The number of phones that are not revoked, for each login that has one at least.
    pub async fn active_phones_by_login(
        &self,
    ) -> Result<std::collections::BTreeMap<String, u32>, DbError> {
        match self {
            Store::Sqlite(db) => db.call(move |c| Ok(db::active_phones_by_login(c)?)).await,
            Store::Pg(pg) => pg.guard(pg.active_phones_by_login()).await,
        }
    }

    /// The alerts created at or after `since_ms`, counted for each bucket of `bucket_ms` and each
    /// group.
    pub async fn alert_counts(
        &self,
        since_ms: i64,
        bucket_ms: i64,
    ) -> Result<Vec<db::AlertCount>, DbError> {
        match self {
            Store::Sqlite(db) => {
                db.call(move |c| Ok(db::alert_counts(c, since_ms, bucket_ms)?))
                    .await
            }
            Store::Pg(pg) => pg.guard(pg.alert_counts(since_ms, bucket_ms)).await,
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
                db.call(move |c| Ok(db::alert_detail(c, &id, cutoff)?))
                    .await
            }
            Store::Pg(pg) => pg.guard(pg.alert_detail(id, cutoff)).await,
        }
    }

    pub async fn recent_connections(
        &self,
        limit: u32,
        user: Option<String>,
    ) -> Result<Vec<ConnectionView>, DbError> {
        match self {
            Store::Sqlite(db) => {
                db.call(move |c| Ok(db::recent_connections(c, limit, user.as_deref())?))
                    .await
            }
            Store::Pg(pg) => {
                pg.guard(pg.recent_connections(limit, user.as_deref()))
                    .await
            }
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
            Store::Pg(pg) => pg.guard(pg.stats(cutoff)).await,
        }
    }

    /// Sessions open on the other instances (empty with SQLite: there is only this instance).
    pub async fn remote_sessions(&self) -> Result<HashMap<String, LiveInfo>, DbError> {
        match self {
            Store::Sqlite(_) => Ok(HashMap::new()),
            Store::Pg(pg) => pg.guard(pg.live_sessions()).await,
        }
    }

    // ------------------------------------------------------------ retention

    pub async fn purge(
        &self,
        alert_cutoff: i64,
        connection_cutoff: i64,
    ) -> Result<(usize, usize, usize), DbError> {
        match self {
            Store::Sqlite(db) => {
                db.call(move |c| Ok(db::purge(c, alert_cutoff, connection_cutoff)?))
                    .await
            }
            Store::Pg(pg) => pg.guard(pg.purge(alert_cutoff, connection_cutoff)).await,
        }
    }
}
