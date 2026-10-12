//! `inbox serve`: startup, listeners, background tasks and graceful shutdown.

use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
use std::time::Duration;

use axum::Router;
use axum::extract::{DefaultBodyLimit, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::{get, post};
use inbox_core::proto::GroupInfo;
use serde_json::json;
use tokio::net::TcpListener;

use crate::admin;
use crate::api;
use crate::config::Config;
use crate::db::Db;
use crate::device;
use crate::hub::Hub;
use crate::pg::{Event, PgStore};
use crate::ratelimit::AuthLimiter;
use crate::replica::{self, Replica};
use crate::shutdown::Shutdown;
use crate::state::{AppState, Secrets};
use crate::store::{Stats, Store};
use crate::telemetry::{self, Gauges, Metrics};
use crate::util::now_ms;
use crate::{GIT_SHA, VERSION};

pub fn serve(config: Config) -> Result<(), String> {
    // Everything that can be checked is checked before anything starts.
    let secrets = Secrets::load(&config)?;
    let (guard, meter) = telemetry::init(&config)?;
    // SQLite: a standby waits here, before the database is opened and before any port is bound: it serves nothing.
    let boot = match (&config.replica, &config.storage.path) {
        (Some(r), Some(path)) => match replica::boot(r, path)? {
            Some(boot) => Some(boot),
            None => {
                guard.shutdown();
                return Ok(());
            }
        },
        _ => None,
    };
    let sqlite = config.storage.path.as_deref().map(Db::open).transpose()?;
    let shutdown = Shutdown::new();
    let stats = Arc::new(replica::Stats::default());
    let running = match (boot, &config.storage.path) {
        (Some(b), Some(path)) => Some(b.start(path, shutdown.clone(), stats.clone())?),
        _ => None,
    };
    let replica = running.as_ref().map_or_else(Replica::disabled, |r| r.replica());
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .thread_name("inbox")
        .build()
        .map_err(|e| format!("cannot start runtime: {e}"))?;
    let (store, db_closed) = match (sqlite, &config.postgres) {
        (Some((db, closed)), _) => (Store::Sqlite(db), Some(closed)),
        (None, Some(pg)) => (Store::Pg(Arc::new(runtime.block_on(PgStore::connect(pg))?)), None),
        (None, None) => return Err("storage.path or a postgres block is required".into()),
    };
    let result = runtime.block_on(run(config, secrets, store, meter, shutdown, replica, stats));
    // Dropping the runtime drops every task, hence every database handle.
    runtime.shutdown_timeout(Duration::from_secs(5));
    if let Some(closed) = db_closed
        && !closed.wait(Duration::from_secs(10))
    {
        tracing::warn!(event = "database_close_timeout");
    }
    // The database is closed: the last snapshot is complete, then the next instance can take over at once.
    let fenced = running.as_ref().is_some_and(|r| r.fenced());
    if let Some(running) = running {
        running.finish();
    }
    guard.shutdown();
    if fenced {
        return Err("replica: another instance owns the lease, stopped".into());
    }
    result
}

async fn run(
    config: Config,
    secrets: Secrets,
    db: Store,
    meter: opentelemetry::metrics::Meter,
    shutdown: Shutdown,
    replica: Replica,
    replica_stats: Arc<replica::Stats>,
) -> Result<(), String> {
    let group_ids: Vec<String> = config.group.keys().cloned().collect();
    let pruned = db.prune_subscriptions(group_ids).await.map_err(|e| format!("storage: {e}"))?;
    if pruned > 0 {
        tracing::warn!(
            event = "subscriptions_pruned",
            count = pruned,
            reason = "group removed from config"
        );
    }

    let public = TcpListener::bind(config.server.listen)
        .await
        .map_err(|e| format!("server.listen {}: {e}", config.server.listen))?;
    let admin_listener = TcpListener::bind(config.server.admin_listen)
        .await
        .map_err(|e| format!("server.admin_listen {}: {e}", config.server.admin_listen))?;

    let groups: Vec<GroupInfo> = config
        .group
        .iter()
        .map(|(id, g)| GroupInfo {
            id: id.clone(),
            name: g.name.clone(),
            description: g.description.clone(),
        })
        .collect();
    let group_names = config.group.iter().map(|(id, g)| (id.clone(), g.name.clone())).collect();
    let gauges = Arc::new(Gauges::default());
    telemetry::register_gauges(&meter, gauges.clone());
    if config.replica.is_some() {
        telemetry::register_replica_gauges(&meter, replica_stats);
    }
    let app = Arc::new(AppState {
        limiter: AuthLimiter::new(
            config.security.max_auth_failures,
            config.security.auth_failure_window,
        ),
        secrets,
        db,
        replica,
        hub: Hub::default(),
        metrics: Metrics::new(&meter),
        gauges,
        shutdown: shutdown.clone(),
        groups,
        group_names,
        next_session_id: AtomicU64::new(1),
        config,
    });
    shutdown.listen_for_signals();
    spawn_background(app.clone());

    let body_limit = DefaultBodyLimit::max(app.config.limits.max_request_bytes);
    let public_router = Router::new()
        .route("/v1/enroll", get(device::enroll_ws))
        .route("/v1/session", get(device::session_ws))
        .route("/api/v1/alerts", post(api::post_alert))
        .route("/api/v1/alerts/{id}", get(api::get_alert))
        .route("/api/v1/webhooks/alertmanager/{group}", post(api::alertmanager))
        .route("/api/v1/notify/{group}", post(api::notify))
        .route("/healthz", get(|| async { "ok" }))
        .route("/readyz", get(readyz))
        .route("/api/version", get(version))
        .layer(body_limit)
        .with_state(app.clone());
    let admin_router = Router::new()
        .route("/api/admin/overview", get(admin::overview))
        .route("/api/admin/alerts", get(admin::alerts).post(admin::send_alert))
        .route("/api/admin/alerts/{id}", get(admin::alert))
        .route("/api/admin/connections", get(admin::connections))
        .route("/api/admin/devices/{id}/revoke", post(admin::revoke))
        .route("/api/version", get(version))
        .fallback(get(admin::static_asset))
        .layer(axum::middleware::from_fn_with_state(app.clone(), admin::guard))
        .layer(body_limit)
        .with_state(app.clone());

    tracing::info!(
        event = "started",
        version = VERSION,
        git_sha = GIT_SHA,
        listen = %app.config.server.listen,
        admin_listen = %app.config.server.admin_listen,
        groups = app.groups.len(),
        producers = app.secrets.producers.len(),
        telemetry = app.config.telemetry.enabled,
        replica = app.config.replica.is_some(),
        storage = if app.db.is_shared() { "postgres" } else { "sqlite" },
    );

    let grace = app.config.server.shutdown_grace;
    let stop = {
        let app = app.clone();
        move || {
            let app = app.clone();
            async move {
                app.shutdown.wait().await;
                // Sessions close with 1012 so apps reconnect as soon as the server is back.
                app.hub.kick_all(1012);
            }
        }
    };
    let public_srv =
        axum::serve(public, public_router.into_make_service_with_connect_info::<SocketAddr>())
            .with_graceful_shutdown(stop());
    // Prefix rewriting must happen before routing, so it wraps the router.
    let admin_app =
        tower::Layer::layer(&axum::middleware::map_request(admin::rewrite_request), admin_router);
    let admin_srv =
        axum::serve(
            admin_listener,
            axum::ServiceExt::<axum::extract::Request>::into_make_service_with_connect_info::<
                SocketAddr,
            >(admin_app),
        )
        .with_graceful_shutdown(stop());
    let servers = async { tokio::try_join!(async { public_srv.await }, async { admin_srv.await }) };
    tokio::pin!(servers);

    let result = tokio::select! {
        r = &mut servers => r.map(|_| ()).map_err(|e| format!("listener failed: {e}")),
        _ = shutdown.wait() => {
            match tokio::time::timeout(grace, &mut servers).await {
                Ok(r) => r.map(|_| ()).map_err(|e| format!("listener failed: {e}")),
                Err(_) => {
                    tracing::warn!(event = "shutdown_grace_exceeded", grace_ms = grace.as_millis() as u64);
                    Ok(())
                }
            }
        }
    };
    shutdown.trigger();
    flush_last_seen(&app).await;
    if let Store::Pg(pg) = &app.db
        && let Err(e) = pg.shutdown().await
    {
        tracing::warn!(event = "instance_shutdown_failed", error = %e);
    }
    tracing::info!(event = "stopped");
    result
}

async fn readyz(State(app): State<Arc<AppState>>) -> impl IntoResponse {
    if app.shutdown.is_triggered() {
        return (StatusCode::SERVICE_UNAVAILABLE, "draining");
    }
    match tokio::time::timeout(Duration::from_secs(2), app.db.ping()).await {
        Ok(Ok(())) => (StatusCode::OK, "ready"),
        _ => (StatusCode::SERVICE_UNAVAILABLE, "storage unavailable"),
    }
}

async fn version() -> impl IntoResponse {
    axum::Json(
        json!({ "version": VERSION, "git_sha": GIT_SHA, "protocol": inbox_core::PROTOCOL_VERSION }),
    )
}

async fn flush_last_seen(app: &AppState) {
    let seen: Vec<(String, i64)> =
        app.hub.snapshot().into_iter().map(|(id, l)| (id, l.last_seen_at)).collect();
    if seen.is_empty() {
        return;
    }
    if let Err(e) = app.db.touch_seen(seen).await {
        tracing::warn!(event = "last_seen_flush_failed", error = %e);
    }
}

/// With a shared database: the heartbeat of this instance, and the notifications of the other instances.
fn spawn_cluster(app: Arc<AppState>) {
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
            match beat_pg.heartbeat().await {
                Ok(0) => {}
                Ok(closed) => {
                    tracing::warn!(event = "lost_instance_sessions_closed", count = closed)
                }
                Err(e) => tracing::warn!(event = "heartbeat_failed", error = %e),
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
                Event::Replaced { device, instance, connected_at } => {
                    if instance != pg.instance() {
                        app.hub.kick_older(&device, connected_at, close::REPLACED);
                    }
                }
                Event::Resync => app.hub.wake_all(),
            }
        }
    });
}

fn spawn_background(app: Arc<AppState>) {
    spawn_cluster(app.clone());
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
                Ok(Stats { pending: (pending, oldest), subscribed, enrolled }) => {
                    let live = a.live_sessions().await;
                    let cutoff = now_ms() - a.config.devices.unreachable_after.as_millis() as i64;
                    let unreachable = subscribed
                        .iter()
                        .filter(|(id, seen)| {
                            !live.contains_key(id) && seen.is_none_or(|t| t < cutoff)
                        })
                        .count();
                    a.gauges.pending_deliveries.store(pending, Relaxed);
                    a.gauges.oldest_pending_ms.store(oldest, Relaxed);
                    a.gauges.unreachable_devices.store(unreachable as i64, Relaxed);
                    a.gauges.enrolled_devices.store(enrolled as i64, Relaxed);
                }
                Err(e) => tracing::warn!(event = "stats_failed", error = %e),
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
                    tracing::info!(event = "retention_purged", alerts, connections)
                }
                Ok(_) => {}
                Err(e) => tracing::warn!(event = "retention_failed", error = %e),
            }
        }
    });
}
