//! The background tasks of the inbox: the heartbeat of the instance, the notifications of the
//! other instances, the watch of the database, the statistics and the retention.

use std::sync::atomic::Ordering::Relaxed;
use std::sync::Arc;
use std::time::Duration;

use super::log;
use super::pg::Event;
use super::state::App;
use super::store::{Stats, Store};
use super::util::now_ms;

/// Writes the time of the last frame of each open session to the database.
pub async fn flush_last_seen(app: &App) {
    let seen: Vec<(String, i64)> = app
        .hub
        .snapshot()
        .into_iter()
        .map(|(id, l)| (id, l.last_seen_at))
        .collect();
    if seen.is_empty() {
        return;
    }
    if let Err(e) = app.db.touch_seen(seen).await {
        log::warn!(event = "last_seen_flush_failed", error = %e);
    }
}

/// With a shared database: the heartbeat of this instance, and the notifications of the other instances.
fn spawn_cluster(app: Arc<App>) {
    let Store::Pg(pg) = &app.db else { return };
    let pg = pg.clone();

    let (beat_pg, a) = (pg.clone(), app.clone());
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(Duration::from_secs(5));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tokio::select! {
                _ = tick.tick() => {}
                _ = a.shutdown.wait() => return,
            }
            match beat_pg.guard(beat_pg.heartbeat()).await {
                Ok(0) => {}
                Ok(closed) => {
                    log::warn!(event = "lost_instance_sessions_closed", count = closed)
                }
                Err(e) => log::warn!(event = "heartbeat_failed", error = %e),
            }
        }
    });

    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let (listen_pg, a) = (pg.clone(), app.clone());
    tokio::spawn(async move {
        let stop = a.shutdown.clone();
        listen_pg.listen(tx, async move { stop.wait().await }).await;
    });
    tokio::spawn(async move {
        use inbox_core::proto::close;
        while let Some(event) = rx.recv().await {
            match event {
                Event::Wake(devices) => app.hub.wake(&devices),
                Event::Revoked(device) => app.hub.kick(&device, close::REVOKED),
                Event::Replaced {
                    device,
                    instance,
                    connected_at,
                } => {
                    if instance != pg.instance() {
                        app.hub.kick_older(&device, connected_at, close::REPLACED);
                    }
                }
                Event::Resync => app.hub.wake_all(),
            }
        }
    });
}

/// With a shared database: watches for a writable primary, writes what was accepted without one as soon as there is one, keeps
/// the copy of devices and subscriptions fresh, and looks for the other instances.
fn spawn_resilience(app: Arc<App>) {
    let Some(pg) = app.db.pg().cloned() else {
        return;
    };
    let cluster = app.cluster.clone();
    let a = app.clone();
    tokio::spawn(async move { cluster.run_discovery(a).await });
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(Duration::from_secs(1));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut refreshed = tokio::time::Instant::now();
        // One failed look is not enough to leave a working database; a lost primary is also noticed by the calls that fail.
        let mut failures = 0u32;
        loop {
            tokio::select! {
                _ = tick.tick() => {}
                _ = app.shutdown.wait() => return,
            }
            let was_down = pg.is_down();
            if pg.writable().await {
                failures = 0;
                pg.mark_up();
            } else {
                failures += 1;
                if failures >= 2 {
                    pg.mark_down(&super::db::DbError::Unavailable(
                        "probe: no writable primary".into(),
                    ));
                }
            }
            app.metrics
                .storage_degraded
                .store(pg.is_down() as i64, Relaxed);
            app.metrics
                .spool_alerts
                .store(pg.spool().len() as i64, Relaxed);
            if pg.is_down() {
                continue;
            }
            if was_down {
                // Every session reads its deliveries again from the database.
                app.hub.wake_all();
            }
            pg.spool().expire(app.delivery_cutoff());
            if pg.spool().has_work() {
                match pg.guard(pg.replay()).await {
                    Ok((alerts, devices)) => {
                        if alerts > 0 {
                            log::info!(event = "spool_replayed", alerts);
                        }
                        app.hub.wake(&devices);
                    }
                    Err(e) => log::warn!(event = "spool_replay_failed", error = %e),
                }
                app.metrics
                    .spool_alerts
                    .store(pg.spool().len() as i64, Relaxed);
            }
            if refreshed.elapsed() >= Duration::from_secs(5)
                && pg.guard(pg.refresh_cache()).await.is_ok()
            {
                refreshed = tokio::time::Instant::now();
            }
        }
    });
}

pub fn spawn_background(app: Arc<App>) {
    spawn_cluster(app.clone());
    spawn_resilience(app.clone());
    // Gauges and last-seen persistence.
    let a = app.clone();
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(Duration::from_secs(15));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut n: u64 = 0;
        loop {
            tokio::select! {
                _ = tick.tick() => {}
                _ = a.shutdown.wait() => return,
            }
            n += 1;
            if n.is_multiple_of(4) {
                flush_last_seen(&a).await;
            }
            match a.db.stats(a.delivery_cutoff()).await {
                Ok(Stats {
                    pending: (pending, oldest),
                    subscribed,
                    enrolled,
                }) => {
                    let live = a.live_sessions().await;
                    let cutoff = now_ms() - a.config.devices.unreachable_after.as_millis() as i64;
                    let unreachable = subscribed
                        .iter()
                        .filter(|(id, seen)| {
                            !live.contains_key(id) && seen.is_none_or(|t| t < cutoff)
                        })
                        .count();
                    a.metrics.pending_deliveries.store(pending, Relaxed);
                    a.metrics.oldest_pending_ms.store(oldest, Relaxed);
                    a.metrics
                        .unreachable_devices
                        .store(unreachable as i64, Relaxed);
                    a.metrics.enrolled_devices.store(enrolled as i64, Relaxed);
                }
                Err(e) => log::warn!(event = "stats_failed", error = %e),
            }
        }
    });

    // Retention.
    let a = app;
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(Duration::from_secs(3600));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tokio::select! {
                _ = tick.tick() => {}
                _ = a.shutdown.wait() => return,
            }
            let now = now_ms();
            let alert_cutoff = now - a.config.storage.alert_retention.as_millis() as i64;
            let conn_cutoff = now - a.config.storage.connection_log_retention.as_millis() as i64;
            match a.db.purge(alert_cutoff, conn_cutoff).await {
                Ok((alerts, connections)) if alerts + connections > 0 => {
                    log::info!(event = "retention_purged", alerts, connections)
                }
                Ok(_) => {}
                Err(e) => log::warn!(event = "retention_failed", error = %e),
            }
        }
    });
}
