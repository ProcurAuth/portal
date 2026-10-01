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

use crate::admin::{self, AdminSessions};
use crate::api;
use crate::config::Config;
use crate::db::{self, Db};
use crate::device;
use crate::hub::Hub;
use crate::ratelimit::AuthLimiter;
use crate::shutdown::Shutdown;
use crate::state::{AppState, Secrets};
use crate::telemetry::{self, Gauges, Metrics};
use crate::util::now_ms;
use crate::{GIT_SHA, VERSION};

pub fn serve(config: Config) -> Result<(), String> {
    // Everything that can be checked is checked before anything starts.
    let secrets = Secrets::load(&config)?;
    let (guard, meter) = telemetry::init(&config)?;
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .thread_name("inbox")
        .build()
        .map_err(|e| format!("cannot start runtime: {e}"))?;
    let result = runtime.block_on(run(config, secrets, meter));
    runtime.shutdown_timeout(Duration::from_secs(5));
    guard.shutdown();
    result
}

async fn run(config: Config, secrets: Secrets, meter: opentelemetry::metrics::Meter) -> Result<(), String> {
    let db = Db::open(&config.storage.path)?;
    let group_ids: Vec<String> = config.group.keys().cloned().collect();
    let pruned = db
        .call(move |c| Ok(db::prune_subscriptions(c, &group_ids)?))
        .await
        .map_err(|e| format!("storage: {e}"))?;
    if pruned > 0 {
        tracing::warn!(event = "subscriptions_pruned", count = pruned, reason = "group removed from config");
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
        .map(|(id, g)| GroupInfo { id: id.clone(), name: g.name.clone(), description: g.description.clone() })
        .collect();
    let group_names = config.group.iter().map(|(id, g)| (id.clone(), g.name.clone())).collect();
    let gauges = Arc::new(Gauges::default());
    telemetry::register_gauges(&meter, gauges.clone());
    let shutdown = Shutdown::new();
    let app = Arc::new(AppState {
        limiter: AuthLimiter::new(config.security.max_auth_failures, config.security.auth_failure_window),
        secrets,
        db,
        hub: Hub::default(),
        admin_sessions: AdminSessions::default(),
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
        .route("/healthz", get(|| async { "ok" }))
        .route("/readyz", get(readyz))
        .route("/api/version", get(version))
        .layer(body_limit.clone())
        .with_state(app.clone());
    let admin_router = Router::new()
        .route("/api/admin/login/start", post(admin::login_start))
        .route("/api/admin/login/finish", post(admin::login_finish))
        .route("/api/admin/logout", post(admin::logout))
        .route("/api/admin/overview", get(admin::overview))
        .route("/api/admin/alerts", get(admin::alerts).post(admin::send_alert))
        .route("/api/admin/alerts/{id}", get(admin::alert))
        .route("/api/admin/connections", get(admin::connections))
        .route("/api/admin/devices/{id}/revoke", post(admin::revoke))
        .route("/api/version", get(version))
        .fallback(get(admin::static_asset))
        .layer(axum::middleware::from_fn(admin::security_headers))
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
    let public_srv = axum::serve(public, public_router.into_make_service_with_connect_info::<SocketAddr>())
        .with_graceful_shutdown(stop());
    let admin_srv = axum::serve(admin_listener, admin_router.into_make_service_with_connect_info::<SocketAddr>())
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
    tracing::info!(event = "stopped");
    result
}

async fn readyz(State(app): State<Arc<AppState>>) -> impl IntoResponse {
    if app.shutdown.is_triggered() {
        return (StatusCode::SERVICE_UNAVAILABLE, "draining");
    }
    let probe = app.db.call(|c| Ok(c.query_row("SELECT 1", [], |r| r.get::<_, i64>(0))?));
    match tokio::time::timeout(Duration::from_secs(2), probe).await {
        Ok(Ok(1)) => (StatusCode::OK, "ready"),
        _ => (StatusCode::SERVICE_UNAVAILABLE, "storage unavailable"),
    }
}

async fn version() -> impl IntoResponse {
    axum::Json(json!({ "version": VERSION, "git_sha": GIT_SHA, "protocol": inbox_core::PROTOCOL_VERSION }))
}

async fn flush_last_seen(app: &AppState) {
    let seen: Vec<(String, i64)> = app.hub.snapshot().into_iter().map(|(id, l)| (id, l.last_seen_at)).collect();
    if seen.is_empty() {
        return;
    }
    if let Err(e) = app.db.call(move |c| Ok(db::touch_seen(c, &seen)?)).await {
        tracing::warn!(event = "last_seen_flush_failed", error = %e);
    }
}

fn spawn_background(app: Arc<AppState>) {
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
            if n % 4 == 0 {
                flush_last_seen(&a).await;
            }
            let stats = a
                .db
                .call(|c| Ok((db::pending_stats(c)?, db::subscribed_devices(c)?, db::count_active_devices(c)?)))
                .await;
            match stats {
                Ok(((pending, oldest), subscribed, enrolled)) => {
                    let live = a.hub.snapshot();
                    let cutoff = now_ms() - a.config.devices.unreachable_after.as_millis() as i64;
                    let unreachable = subscribed
                        .iter()
                        .filter(|(id, seen)| !live.contains_key(id) && seen.is_none_or(|t| t < cutoff))
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
            match a.db.call(move |c| Ok(db::purge(c, alert_cutoff, conn_cutoff)?)).await {
                Ok((alerts, connections)) if alerts + connections > 0 => {
                    tracing::info!(event = "retention_purged", alerts, connections)
                }
                Ok(_) => {}
                Err(e) => tracing::warn!(event = "retention_failed", error = %e),
            }
        }
    });
}
