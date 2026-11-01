//! The inbox: it sends alerts to phones and makes sure that each phone shows each alert one time.
//!
//! The inbox is a module of the portal. It runs in the runtime of the portal, and it starts and
//! stops with the portal. It does not use the sessions or the directory of the portal: it can
//! deliver an alert when the rest of the portal is not ready.
//!
//! - `device`: enrollment and sessions of the phones (WebSocket, protocol 4).
//! - `enrollment`: the enrollment codes that the accounts make, and the devices of an account.
//! - `api`: the API of the producers and the webhooks.
//! - `admin`: the admin calls. The portal serves them for the session of the superadmin.
//! - `db`, `pg`, `store`: SQLite and PostgreSQL. The inbox uses the database of the portal.
//! - `spool`, `peer`: the degraded mode, when PostgreSQL has no writable primary.
//! - `replica`: the standby copy of the SQLite file.
//! - `http`, `ws`: the listeners, on hyper.
//! - `metrics`, `log`: the Prometheus text and the JSON log.

pub mod admin;
mod api;
pub mod cli;
mod db;
mod device;
mod enrollment;
mod http;
mod hub;
mod internal;
pub mod log;
pub mod metrics;
mod net;
mod peer;
mod pg;
mod ratelimit;
mod replica;
mod shutdown;
mod spool;
mod state;
mod store;
mod tasks;
mod util;
mod ws;

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;

use inbox_core::proto::GroupInfo;
use tokio::net::TcpListener;
use tokio::task::JoinHandle;
use zeroize::Zeroizing;

use crate::config::{Config, InboxConfig, Secrets as PortalSecrets, StorageBackend};
pub use api::ApiError;
use db::Db;
pub use http::json as json_response;
use hub::Hub;
use peer::Cluster;
use pg::{PgSettings, PgStore};
use ratelimit::AuthLimiter;
use replica::Replica;
pub use shutdown::Shutdown;
pub use state::App;
use state::{HttpLimits, Secrets};
use store::Store;

/// The version of the program that the inbox reports.
pub const VERSION: &str = match option_env!("PORTAL_RELEASE_VERSION") {
    Some(version) => version,
    None => env!("CARGO_PKG_VERSION"),
};

/// The code of the close frame that tells the phones that the server restarts.
const SERVER_RESTARTING: u16 = 1012;

/// What the portal needs from the inbox at start.
pub enum Boot {
    /// The file has no block `inbox`.
    Off,
    /// The process got the signal to stop while this instance waited for the lease.
    Stopped,
    /// The secrets are valid, and the instance owns the lease when there is a replica.
    Ready(Box<Booted>),
}

/// The inbox before the portal opens its database.
pub struct Booted {
    config: InboxConfig,
    secrets: Secrets,
    replica: Option<replica::Boot>,
}

/// Decodes the secrets of the inbox and, with a replica, waits for the lease and restores the
/// SQLite file. The portal must call this function before it opens the SQLite file.
/// `stop` becomes true when the process must stop.
pub fn boot(
    config: &Config,
    secrets: &mut PortalSecrets,
    stop: &dyn Fn() -> bool,
) -> Result<Boot, String> {
    let (Some(inbox), Some(inbox_secrets)) = (&config.inbox, secrets.inbox.take()) else {
        return Ok(Boot::Off);
    };
    let decoded = Secrets::load(&inbox_secrets, inbox)?;
    drop(inbox_secrets);
    let replica = match (&inbox.replica, &config.storage.backend) {
        (Some(replica_config), StorageBackend::Sqlite(path)) => {
            match replica::boot(replica_config, path, stop)? {
                Some(boot) => Some(boot),
                None => return Ok(Boot::Stopped),
            }
        }
        _ => None,
    };
    Ok(Boot::Ready(Box::new(Booted {
        config: inbox.clone(),
        secrets: decoded,
        replica,
    })))
}

/// Checks the secrets of the inbox, as `boot` does, and does nothing else. `check-config` uses it.
pub fn check_secrets(config: &Config, secrets: &mut PortalSecrets) -> Result<(), String> {
    if let (Some(inbox), Some(inbox_secrets)) = (&config.inbox, secrets.inbox.take()) {
        Secrets::load(&inbox_secrets, inbox)?;
    }
    Ok(())
}

/// What the runtime needs to start the inbox.
pub struct Init {
    config: InboxConfig,
    secrets: Secrets,
    store: StoreInit,
    replica: Replica,
    replica_stats: Option<Arc<replica::Stats>>,
    http: HttpLimits,
}

enum StoreInit {
    Sqlite(Db),
    Postgres(PgSettings),
}

/// The part of the inbox that outlives the runtime: the threads of the replica and of SQLite.
pub struct Lifecycle {
    running: Option<replica::Running>,
    closed: Option<db::Closed>,
    fence: Shutdown,
}

impl Booted {
    /// Opens the inbox storage on the database of the portal and starts the threads of the
    /// replica. The portal has opened its database before. The password is the password of
    /// the block `postgres`.
    pub fn start(
        self,
        config: &Config,
        postgres_password: Option<Zeroizing<String>>,
    ) -> Result<(Init, Lifecycle), String> {
        let Booted {
            config: inbox,
            secrets,
            replica,
        } = self;
        let fence = Shutdown::new();
        let stats = Arc::new(replica::Stats::default());
        let mut closed = None;
        let mut running = None;
        let (store, replica_handle, replica_stats) = match &config.storage.backend {
            StorageBackend::Sqlite(path) => {
                let (db, db_closed) = Db::open(path)?;
                closed = Some(db_closed);
                let handle = match replica {
                    Some(boot) => {
                        let run = boot.start(path, fence.clone(), Arc::clone(&stats))?;
                        let handle = run.replica();
                        running = Some(run);
                        handle
                    }
                    None => Replica::disabled(),
                };
                let stats = inbox.replica.is_some().then_some(stats);
                (StoreInit::Sqlite(db), handle, stats)
            }
            StorageBackend::Postgres(pg) => {
                let password = postgres_password
                    .ok_or("internal error: the postgres block has no password")?;
                let settings = PgSettings {
                    host: pg.host.clone(),
                    port: pg.port,
                    database: pg.database.clone(),
                    user: pg.user.clone(),
                    password,
                    pool_size: inbox.storage.pool_size,
                    connect_timeout: pg.connect_timeout,
                };
                (StoreInit::Postgres(settings), Replica::disabled(), None)
            }
        };
        let http = HttpLimits {
            header_read_timeout: config.server.header_read_timeout,
            body_read_timeout: config.server.body_read_timeout,
            max_headers: config.server.max_headers,
            max_header_bytes: config.server.max_header_bytes,
            max_connections: config.server.max_connections + 2 * inbox.devices.max_devices as usize,
        };
        let init = Init {
            config: inbox,
            secrets,
            store,
            replica: replica_handle,
            replica_stats,
            http,
        };
        Ok((
            init,
            Lifecycle {
                running,
                closed,
                fence,
            },
        ))
    }
}

impl Lifecycle {
    /// The signal of a lost lease. It becomes ready when another instance took the lease: the
    /// process must stop.
    pub fn fence(&self) -> Shutdown {
        self.fence.clone()
    }

    /// Ends the threads after the runtime stopped. The last snapshot holds all the data, and the
    /// next instance can take over at once. The result is an error when another instance took
    /// the lease: the process must report a failure.
    pub fn finish(self) -> Result<(), String> {
        if let Some(closed) = self.closed {
            if !closed.wait(Duration::from_secs(10)) {
                log::warn!(event = "database_close_timeout");
            }
        }
        let fenced = self.running.as_ref().is_some_and(|r| r.fenced());
        if let Some(running) = self.running {
            running.finish();
        }
        if fenced {
            return Err("inbox.replica: another instance owns the lease, stopped".into());
        }
        Ok(())
    }
}

/// The place of the inbox in the state of the portal.
#[derive(Default)]
pub struct Slot {
    init: Mutex<Option<Init>>,
    /// The running inbox. The portal keeps its state for the whole life of the process, so the
    /// slot lets the inbox go at the end of `finish`: the database threads can then close.
    app: RwLock<Option<Arc<App>>>,
    tasks: Mutex<Vec<JoinHandle<()>>>,
}

impl Slot {
    /// Gives the inbox what it needs to start. The portal calls it before the runtime starts.
    pub fn prepare(&self, init: Init) {
        *self.init.lock().unwrap_or_else(|p| p.into_inner()) = Some(init);
    }

    /// The running inbox. `None` without the block `inbox`, or before the start.
    pub fn app(&self) -> Option<Arc<App>> {
        self.app.read().unwrap_or_else(|p| p.into_inner()).clone()
    }

    /// Makes an enrollment code for `login`. `None` without the block `inbox` or before the start.
    pub async fn create_enrollment_code(
        &self,
        login: &str,
    ) -> Option<Result<enrollment::NewCode, ApiError>> {
        let app = self.app()?;
        Some(enrollment::create_code(&app, login).await)
    }

    /// Deletes the unused enrollment codes of `login` and revokes its devices. Without the block
    /// `inbox`, there is nothing to do and the result is `Ok`.
    pub async fn revoke_devices_of(&self, login: &str) -> Result<usize, String> {
        match self.app() {
            Some(app) => enrollment::revoke_devices_of(&app, login).await,
            None => Ok(0),
        }
    }

    /// Sends an alert that the portal makes about itself (source `portal`). The result is `true`
    /// when the inbox already had the key. `Err` tells why the inbox did not accept the alert.
    pub async fn send_portal_alert(
        &self,
        group: &str,
        title: String,
        key: String,
    ) -> Result<bool, String> {
        let app = self.app().ok_or("the inbox is not running")?;
        internal::send_alert(&app, group, title, key)
            .await
            .map_err(|e| e.summary())
    }

    /// The text of the metrics of the inbox. Empty without the block `inbox`.
    pub fn render_metrics(&self) -> String {
        match self.app() {
            Some(app) => app.metrics.render(app.replica_stats.as_deref()),
            None => String::new(),
        }
    }

    /// Starts the storage, the listeners and the background tasks. The portal calls it first in
    /// its start, so the inbox does not wait for the catalogue of the portal.
    pub async fn start(&self) -> Result<(), String> {
        let Some(init) = self.init.lock().unwrap_or_else(|p| p.into_inner()).take() else {
            return Ok(());
        };
        let Init {
            config,
            secrets,
            store,
            replica,
            replica_stats,
            http,
        } = init;
        let db = match store {
            StoreInit::Sqlite(db) => Store::Sqlite(db),
            StoreInit::Postgres(settings) => {
                Store::Pg(Arc::new(PgStore::connect(&settings).await?))
            }
        };
        let group_ids: Vec<String> = config.groups.keys().cloned().collect();
        let pruned = db
            .prune_subscriptions(group_ids)
            .await
            .map_err(|e| format!("inbox storage: {e}"))?;
        if pruned > 0 {
            log::warn!(
                event = "subscriptions_pruned",
                count = pruned,
                reason = "group removed from config"
            );
        }
        // The copy of what was accepted without the database must be warm before anything is accepted.
        if let Some(pg) = db.pg() {
            if let Err(e) = pg.refresh_cache().await {
                log::warn!(event = "cache_refresh_failed", error = %e);
            }
        }
        let cluster = match (&config.cluster, db.pg()) {
            (Some(c), Some(pg)) => Cluster::new(c, pg.instance().to_string(), &secrets.server_key),
            _ => Cluster::disabled(),
        };
        let cluster_listener = match &config.cluster {
            Some(c) => Some(TcpListener::bind(c.listen).await.map_err(|e| {
                format!("cannot listen on {} (inbox.cluster.listen): {e}", c.listen)
            })?),
            None => None,
        };
        let devices_listener = TcpListener::bind(config.devices.listen)
            .await
            .map_err(|e| {
                format!(
                    "cannot listen on {} (inbox.devices.listen): {e}",
                    config.devices.listen
                )
            })?;
        let groups: Vec<GroupInfo> = config
            .groups
            .iter()
            .map(|(id, g)| GroupInfo {
                id: id.clone(),
                name: g.name.clone(),
                description: g.description.clone(),
            })
            .collect();
        let group_names = config
            .groups
            .iter()
            .map(|(id, g)| (id.clone(), g.name.clone()))
            .collect();
        let app = Arc::new(App {
            limiter: AuthLimiter::new(
                config.devices.max_auth_failures,
                config.devices.auth_failure_window,
            ),
            secrets,
            db,
            replica,
            replica_stats,
            cluster,
            hub: Hub::default(),
            metrics: metrics::InboxMetrics::default(),
            shutdown: Shutdown::new(),
            draining: AtomicBool::new(false),
            groups,
            group_names,
            next_session_id: AtomicU64::new(1),
            http,
            stop: crate::shutdown::Stop::default(),
            sockets: Default::default(),
            config,
        });
        tasks::spawn_background(Arc::clone(&app));
        let mut handles = vec![tokio::spawn(http::accept_loop(
            Arc::clone(&app),
            devices_listener,
            http::Kind::Devices,
        ))];
        if let Some(listener) = cluster_listener {
            handles.push(tokio::spawn(http::accept_loop(
                Arc::clone(&app),
                listener,
                http::Kind::Cluster,
            )));
        }
        *self.tasks.lock().unwrap_or_else(|p| p.into_inner()) = handles;
        log::info!(
            event = "inbox_started",
            devices_listen = %app.config.devices.listen,
            groups = app.groups.len(),
            producers = app.secrets.producers.len(),
            replica = app.config.replica.is_some(),
            cluster = app.cluster.enabled(),
            storage = if app.db.is_shared() { "postgres" } else { "sqlite" },
        );
        *self.app.write().unwrap_or_else(|p| p.into_inner()) = Some(app);
        Ok(())
    }

    /// The signal to stop came. The inbox still serves, and `/readyz` says `draining`, so the
    /// cluster takes this instance out of its Service. The phones get the code 1012 and
    /// connect to another instance.
    pub fn begin_drain(&self) {
        if let Some(app) = self.app() {
            app.draining.store(true, Ordering::SeqCst);
            app.hub.kick_all(SERVER_RESTARTING);
        }
    }

    /// Stops the inbox: it closes the listeners, tells every phone to reconnect, waits for the
    /// sessions, writes what the memory holds to the database and leaves the database.
    pub async fn finish(&self, limit: Duration) {
        let Some(app) = self.app() else { return };
        let app = &app;
        for task in self.tasks.lock().unwrap_or_else(|p| p.into_inner()).iter() {
            task.abort();
        }
        app.stop.close_connections();
        app.shutdown.trigger();
        // A session that started after the first signal gets the code now.
        app.hub.kick_all(SERVER_RESTARTING);
        let end = tokio::time::Instant::now() + limit;
        while (app.sockets.load(Ordering::SeqCst) > 0 || app.stop.live() > 0)
            && tokio::time::Instant::now() < end
        {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        tasks::flush_last_seen(app).await;
        if let Store::Pg(pg) = &app.db {
            // What this instance holds for the database goes there before it stops, if the database is back.
            if pg.spool().has_work() {
                match pg.guard(pg.replay()).await {
                    Ok((alerts, _)) => log::info!(event = "spool_replayed", alerts),
                    Err(e) => log::error!(
                        event = "spool_lost",
                        alerts = pg.spool().len(),
                        error = %e,
                        "alerts accepted without the database are lost with this instance unless another instance holds a copy"
                    ),
                }
            }
            if let Err(e) = pg.guard(pg.shutdown()).await {
                log::warn!(event = "instance_shutdown_failed", error = %e);
            }
        }
        log::info!(
            event = "inbox_stopped",
            sockets_left = app.sockets.load(Ordering::SeqCst)
        );
        // The portal keeps its state until the process ends. The inbox lets go of its own.
        *self.app.write().unwrap_or_else(|p| p.into_inner()) = None;
    }
}
