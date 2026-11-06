use std::convert::Infallible;
use std::net::{IpAddr, SocketAddr};
use std::str::FromStr;
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use http_body_util::{BodyExt, Full};
use hyper::body::{Body as _, Bytes, Incoming};
use hyper::header::{
    HeaderMap, HeaderName, HeaderValue, CONTENT_TYPE, COOKIE, HOST, LOCATION, ORIGIN, REFERER,
    RETRY_AFTER, SET_COOKIE, USER_AGENT,
};
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{Method, Request, Response, StatusCode, Uri};
use hyper_util::rt::{TokioIo, TokioTimer};
use tokio::net::TcpListener;
use tokio::sync::Semaphore;
use zeroize::{Zeroize, Zeroizing};

use crate::access::{self, may_open, Decision};
use crate::actions;
use crate::catalog::Catalog;
use crate::config::{Config, LogLevel, Secret};
use crate::crypto::{
    check_form_token, hash_password, new_csrf_token, sign_action_token, sign_form_token,
    sign_return_uri, verify_action_token, verify_csrf, verify_password,
    verify_return_uri_signature, FormToken,
};
use crate::discovery;
use crate::grants::{self, Grant, Policy};
use crate::icons::{Icons, Set};
use crate::identity::Signer;
use crate::key_forms;
use crate::keyauth;
use crate::metrics::Metrics;
use crate::rate_limit::RateLimiter;
use crate::session::{
    Binding, Credential, NewSession, Principal, Session, SessionIssueError, SessionStore,
};
use crate::store::{AccountActivity, InsertError, LoginPage, Snapshot, Store, UserRow};
use crate::tracking::{unix_now, AuditEvent, AuditKind, EndReason, Event, LoginAttempt, Tracker};
use crate::transport;
use crate::ui;
use crate::users::{check_new_password, normalize_login, Directory, KeyOwner, UserRecord};
use crate::util::{
    blocking, constant_time_eq, json_escape, parse_form, path_only, percent_decode_form,
    percent_encode_query_component, typed_login, validate_return_uri,
};

const HOME_PATH: &str = "/";
const LOGIN_PATH: &str = "/auth/login";
const LOGOUT_PATH: &str = "/auth/logout";
const VERIFY_PATH: &str = "/auth/verify";
const VERIFY_KEY_PATH: &str = "/auth/verify-key";
const HEALTH_PATH: &str = "/healthz";
/// The readiness of the instance. With the block `inbox`, the inbox answers (see `crate::inbox`).
const READY_PATH: &str = "/readyz";
const SUCCESS_PATH: &str = "/auth/success";
const DENIED_PATH: &str = "/auth/denied";
const USERS_ADD_PATH: &str = "/auth/users/add";
const USERS_DELETE_PATH: &str = "/auth/users/delete";
const USERS_UPDATE_PATH: &str = "/auth/users/update";
const IMPERSONATE_PATH: &str = "/auth/users/impersonate";
const IMPERSONATE_END_PATH: &str = "/auth/users/impersonate-end";
const END_SESSIONS_PATH: &str = "/auth/users/end-sessions";
/// The value of the field `confirm` of the incident form. A POST without it does nothing.
const END_SESSIONS_CONFIRM: &str = "end-all-sessions";
/// The anti-CSRF token protects the account administration forms.
const USERS_TOKEN: &str = "users";
const LOGOUT_TOKEN: &str = "logout";

const STYLE_PATH: &str = "/auth/style.css";
const SETTINGS_STYLE_PATH: &str = "/auth/settings.css";
const THEME_JS_PATH: &str = "/auth/assets/theme.js";
const SETTINGS_JS_PATH: &str = "/auth/assets/settings.js";
const NOTIFY_JS_PATH: &str = "/auth/assets/notify.js";
const ICONS_PREFIX: &str = "/auth/icons/";
const GLOBAL_RATE_LIMIT_KEY: &str = "global";
/// The hidden field that holds the signed token of the sign-in form.
const FORM_TOKEN_FIELD: &str = "token";
/// The name of the trap field of the sign-in form. A person never fills it.
const TRAP_FIELD: &str = "website";
/// The box "Remember me" of the sign-in form, and the value that a ticked box sends.
const KEEP_FIELD: &str = "keep";
const KEEP_VALUE: &str = "1";
/// The budget of the global limiter for the sign-ins that name the superadmin. Failures against
/// other logins never use it. Anonymous failures thus cannot refuse the sign-in of the superadmin.
const SUPERADMIN_RATE_LIMIT_KEY: &str = "superadmin";
/// With a shared database, this sets the maximum rate at which a request reads the accounts again.
/// The trigger is a login that the directory does not know.
const UNKNOWN_LOGIN_REFRESH_GAP: Duration = Duration::from_secs(1);

type ResponseBody = Full<Bytes>;
type HttpResponse = Response<ResponseBody>;

/// The data that the portal knows about the browser of one request.
struct Client {
    /// True when the browser uses HTTPS. This value sets the cookie names and the `Secure` attribute.
    https: bool,
    ip: Option<IpAddr>,
    /// The value that a session issued to this client is bound to.
    binding: Binding,
}

impl Client {
    fn new(state: &AppState, headers: &HeaderMap, ip: Option<IpAddr>) -> Self {
        Self {
            https: transport::is_https(headers),
            ip,
            binding: transport::binding(state.config.session.bind, headers, ip),
        }
    }
}

/// The account that the environment defines. It is never in the database and cannot be deleted.
pub struct SuperAdmin {
    /// The login in lower case. The login of a request is compared with this value.
    pub login: String,
    pub password_hash: String,
    /// The credential generation (see [`crate::crypto::superadmin_generation`]). With a shared
    /// database, a superadmin or impersonated session of another generation is refused.
    pub generation: [u8; 32],
}

pub struct AppState {
    pub config: Config,
    pub superadmin_login: String,
    pub superadmin_hash: String,
    /// The credential generation of the superadmin. It is the stamp of the credential of a superadmin session (see
    /// [`credential_stamp`]).
    pub superadmin_generation: Credential,
    /// The hash that the portal checks when the login does not exist. An unknown login then takes the same time as a known login.
    pub dummy_hash: String,
    pub signing_secret: Vec<u8>,
    /// The second factor: the keys that seal the TOTP secrets. `None` when `authentication.mfa` is `off`: the
    /// portal then shows no page of the second factor and never asks for a code.
    pub mfa: Option<crate::mfa::Mfa>,
    /// The names, the stylesheet and the script of the pages that load only the shared stylesheet.
    /// Every instance with the same secret makes the same ones.
    pub mask: ui::mask::Mask,
    pub sessions: SessionStore,
    pub users: Directory,
    pub store: Arc<Store>,
    pub tracker: Arc<Tracker>,
    pub ip_limiter: RateLimiter,
    pub global_limiter: RateLimiter,
    pub password_gate: Arc<Semaphore>,
    /// The sign-in bodies that the server reads at the same time (`server.max_login_body_reads`).
    /// Slow bodies cannot take every connection that ForwardAuth needs.
    pub login_body_gate: Arc<Semaphore>,
    /// The bodies of the forms of the keys that the server reads at the same time: at most 4
    /// (portal 5.8c). A fifth request gets the status 503.
    pub key_body_gate: Arc<Semaphore>,

    pub metrics: Metrics,
    /// The current service catalog. Discovery replaces the whole snapshot.
    pub catalog: RwLock<Arc<Catalog>>,
    pub discovery: Arc<discovery::Stats>,
    pub icons: Icons,
    /// The source of the grants, and the grants of the configuration.
    pub policy: Policy,
    /// The key that signs the identity tokens. The value is `None` without the `identity` block.
    pub signer: Option<Signer>,
    /// The actions that each service publishes.
    pub actions: Arc<actions::Registry>,
    /// The options of the forms of grants: the children that a service lists (portal 3.1a).
    pub options: crate::options::Options,
    /// The servers that can ask the PDP. Each server has the hash of its token.
    pub pdp_clients: pdp::Clients,
    pub pdp_stats: pdp::Stats,
    /// The key checks that passed, not yet written to the database (portal 5.5a).
    pub key_checks: crate::keyauth::KeyChecks,
    /// The count of the key checks of the edge, by result (portal 10.5).
    pub key_stats: crate::keyauth::CheckStats,
    /// The limit of the lines of the failed key checks (portal 10.5a).
    pub key_failure_log: crate::keyauth::FailureLog,
    /// The count of the uses of the budgets, with the refusal memo (portal 7.2b, 7.6).
    pub counting: crate::counting::Counting,
    /// The counters whose rows this instance made or found (portal 7.2a).
    /// The number of limit values that the documents do not declare (portal 7.8).
    pub undeclared_values: std::sync::atomic::AtomicU64,
    /// The stop on SIGTERM: the live connections and the signal that closes them (portal 7.3a).
    pub stop: crate::shutdown::Stop,
    /// The inbox. It runs without a session of the portal.
    pub inbox: crate::inbox::Slot,
    /// True when the portal serves its own routes: the catalogue is complete (portal 7.3b). With the
    /// block `inbox`, the listener opens before that moment and serves the API of the inbox only,
    /// so that an alert never waits for the catalogue. Every other route answers 503 until then.
    pub serving: std::sync::atomic::AtomicBool,
    /// The counters of the alerts that the portal sends about itself.
    pub alerts: crate::watch::Stats,
    pub ensured_counters: std::sync::Mutex<
        std::collections::HashSet<(crate::decision::CounterOwner, crate::grants::CounterId)>,
    >,
}

/// The secrets of the access model: the key of the identity tokens and the tokens of the PDP clients.
/// The server keeps only the key pair and the hashes of the tokens.
#[derive(Default)]
pub struct AccessSecrets {
    pub identity_key: Option<Secret>,
    pub pdp_tokens: Vec<(String, Secret)>,
    /// The key of the TOTP secrets. The value is `None` exactly when `authentication.mfa` is `off`.
    pub totp_key: Option<Secret>,
}

impl AppState {
    /// What the tab of the browser shows on the pages after sign-in: `server.name` in the title, and the icon when
    /// `ui.favicon` is `default`.
    pub fn tab_look(&self) -> ui::TabLook<'_> {
        ui::TabLook {
            name: &self.config.server.name,
            icon: self.config.ui.favicon == crate::config::Favicon::Default,
        }
    }

    pub fn new(
        config: Config,
        superadmin: SuperAdmin,
        signing_secret: Vec<u8>,
        store: Arc<Store>,
        tracker: Arc<Tracker>,
        access: AccessSecrets,
    ) -> Result<Self, String> {
        let signer = match (&config.identity, &access.identity_key) {
            (Some(identity), Some(key)) => Some(Signer::new(
                key.as_bytes(),
                &identity.issuer,
                identity.token_ttl,
            )?),
            (None, None) => None,
            _ => return Err("internal error: the identity block and its key do not agree".into()),
        };
        let pdp_clients = pdp::clients(
            access
                .pdp_tokens
                .iter()
                .map(|(name, token)| pdp::Client::new(name.clone(), token.as_bytes()))
                .collect(),
        )?;
        let mfa = match (config.authentication.mfa.is_on(), &access.totp_key) {
            (true, Some(key)) => {
                let key = crate::secretbox::SecretBox::parse_key(key.as_bytes())
                    .map_err(|e| format!("authentication.totp_key: {e}"))?;
                let sealbox = crate::secretbox::SecretBox::new(&key)?;
                // The database keeps the check value of the key that sealed its secrets. The server stops, and
                // changes no secret, when the key of the file is another key.
                if !store.totp_bind_key(&sealbox.check_value())? {
                    return Err("authentication.totp_key is not the key that sealed the TOTP secrets of this database: use that key, or run `portal reset-two-factor --all` to remove all the TOTP secrets before a new key".into());
                }
                Some(crate::mfa::Mfa::new(sealbox))
            }
            (false, None) => None,
            _ => {
                return Err(
                    "internal error: authentication.mfa and authentication.totp_key do not agree"
                        .into(),
                )
            }
        };
        drop(access);
        let policy = Policy::new(
            config.access.grants_mode,
            config.access.configured_grants.clone(),
        );
        let mut sessions = SessionStore::new(
            config.session.ttl,
            config.session.idle_timeout,
            config.session.max_active_sessions,
            Arc::clone(&tracker),
        )
        .per_account(config.session.max_sessions_per_account);
        if let Some(remember) = config.session.remember {
            sessions = sessions.remember(remember);
        }
        if store.is_shared() {
            sessions = sessions.shared(Arc::clone(&store), superadmin.generation);
            // A new superadmin login, password or session secret ends the old superadmin and
            // impersonated sessions on every instance.
            sessions.end_other_generations();
        }
        let snapshot = store.load_directory()?;
        crate::bootstrap::check_superadmin_against_database(&superadmin.login, &snapshot)?;
        crate::bootstrap::check_file_against_database(&config, &snapshot)?;
        crate::bootstrap::check_user_grants(&config, &snapshot)?;
        let users = Directory::from_snapshot(snapshot, config.users.max_users)
            .with_file_keys(crate::bootstrap::file_keys(&config))
            .with_file_accounts(
                config
                    .access
                    .accounts
                    .iter()
                    .map(|(login, account)| (login, account.grants.as_slice())),
            );
        let ensured = crate::bootstrap::prepare_counters(&config, &store, &users, unix_now())?;
        let dummy_hash = hash_password(b"a password nobody has chosen")?;
        let mask = ui::mask::Mask::new(&signing_secret)?;
        Ok(Self {
            mask,
            ip_limiter: RateLimiter::new(
                config.security.login_window,
                config.security.login_lockout,
                config.security.max_rate_limit_entries,
            ),
            // Two budgets: one for the superadmin login, one for every other login.
            global_limiter: RateLimiter::new(
                config.security.global_login_window,
                config.security.global_login_lockout,
                2,
            ),
            password_gate: Arc::new(Semaphore::new(
                config.security.max_concurrent_password_checks,
            )),
            login_body_gate: Arc::new(Semaphore::new(config.server.max_login_body_reads)),
            key_body_gate: Arc::new(Semaphore::new(4)),
            metrics: Metrics::new(),
            catalog: RwLock::new(Arc::new(Catalog::default())),
            discovery: Arc::new(discovery::Stats::default()),
            icons: Icons::load(&config.ui.icons_dir),
            policy,
            signer,
            actions: Arc::new(
                actions::Registry::new(config.access.actions_max_age)
                    .with_bounds(config.access.bounds)
                    .with_portal(crate::portal_actions::document(&config.portal_features())),
            ),
            options: crate::options::Options::new(config.access.actions_timeout)?,
            pdp_clients,
            pdp_stats: pdp::Stats::default(),
            key_checks: crate::keyauth::KeyChecks::default(),
            key_stats: crate::keyauth::CheckStats::default(),
            key_failure_log: crate::keyauth::FailureLog::default(),
            counting: crate::counting::Counting::default(),
            undeclared_values: std::sync::atomic::AtomicU64::new(0),
            stop: crate::shutdown::Stop::default(),
            inbox: crate::inbox::Slot::default(),
            serving: std::sync::atomic::AtomicBool::new(false),
            alerts: crate::watch::Stats::default(),
            ensured_counters: std::sync::Mutex::new(ensured),
            sessions,
            users,
            store,
            tracker,
            config,
            superadmin_login: superadmin.login,
            superadmin_hash: superadmin.password_hash,
            superadmin_generation: superadmin.generation,
            dummy_hash,
            signing_secret,
            mfa,
        })
    }
}

impl Drop for AppState {
    fn drop(&mut self) {
        self.superadmin_hash.zeroize();
        self.signing_secret.zeroize();
    }
}

/// Serves until SIGTERM or SIGINT, or until `also_stop` ends. The inbox uses `also_stop`: an
/// instance that loses its replica lease stops the whole process.
pub fn serve(
    state: Arc<AppState>,
    also_stop: impl std::future::Future<Output = ()> + Send + 'static,
) -> Result<(), String> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(state.config.server.workers)
        .enable_io()
        .enable_time()
        .build()
        .map_err(|e| format!("cannot create Tokio runtime: {e}"))?;
    runtime.block_on(serve_until(state, async move {
        tokio::select! {
            _ = termination() => {}
            _ = also_stop => {}
        }
    }))
}

/// Like [`serve`], with a stop that the caller gives in place of the signals. A test uses it.
#[cfg(test)]
pub fn serve_with(
    state: Arc<AppState>,
    stop: impl std::future::Future<Output = ()> + Send + 'static,
) -> Result<(), String> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(state.config.server.workers)
        .enable_io()
        .enable_time()
        .build()
        .map_err(|e| format!("cannot create Tokio runtime: {e}"))?;
    runtime.block_on(serve_until(state, stop))
}

/// Ends on SIGTERM or SIGINT. When the process cannot listen for a signal, it never ends.
async fn termination() {
    use tokio::signal::unix::{signal, SignalKind};
    let notify = Arc::new(tokio::sync::Notify::new());
    for kind in [SignalKind::terminate(), SignalKind::interrupt()] {
        if let Ok(mut stream) = signal(kind) {
            let notify = Arc::clone(&notify);
            tokio::spawn(async move {
                stream.recv().await;
                notify.notify_one();
            });
        }
    }
    notify.notified().await;
}

/// How often the start looks at the tasks that it waits for.
const START_TICK: std::time::Duration = std::time::Duration::from_millis(50);
/// How often the start says that it waits for the catalogue.
const START_LOG_EVERY: std::time::Duration = std::time::Duration::from_secs(10);

/// Waits for the tasks that must end before the ports open (portal 7.3b): the complete list of
/// the services, then the first reads of their documents, at most `access.start_read_deadline`.
/// The directory is loaded when the state exists. Gives `false` when the stop came first.
async fn wait_ready(
    state: &AppState,
    catalog_complete: &std::sync::atomic::AtomicBool,
    stopped: &tokio::sync::Notify,
) -> bool {
    let mut next_log = Instant::now() + START_LOG_EVERY;
    while !catalog_complete.load(std::sync::atomic::Ordering::SeqCst) {
        if tokio::time::timeout(START_TICK, stopped.notified())
            .await
            .is_ok()
        {
            return false;
        }
        if Instant::now() >= next_log {
            next_log += START_LOG_EVERY;
            log(
                state,
                LogLevel::Warn,
                "start_waits",
                &[("detail", "the catalogue is not complete yet".into())],
            );
        }
    }
    let deadline = Instant::now() + state.config.access.start_read_deadline;
    loop {
        if state.actions.first_reads_done(&current_catalog(state)) {
            return true;
        }
        if Instant::now() >= deadline {
            log(
                state,
                LogLevel::Warn,
                "start_read_deadline",
                &[(
                    "detail",
                    "the first reads of the documents go on in the background".into(),
                )],
            );
            return true;
        }
        if tokio::time::timeout(START_TICK, stopped.notified())
            .await
            .is_ok()
        {
            return false;
        }
    }
}

/// Runs the server until `stop` ends (portal 7.3a, 7.3b).
async fn serve_until(
    state: Arc<AppState>,
    stop: impl std::future::Future<Output = ()> + Send + 'static,
) -> Result<(), String> {
    // The value of each key that applies, with its source. The portal hides the secrets.
    for line in &state.config.effective {
        log(
            &state,
            LogLevel::Info,
            "config",
            &[
                ("key", line.key.clone()),
                ("value", line.value.clone()),
                ("source", line.source.clone()),
            ],
        );
    }
    let stopped = Arc::new(tokio::sync::Notify::new());
    {
        let stopped = Arc::clone(&stopped);
        tokio::spawn(async move {
            stop.await;
            stopped.notify_one();
        });
    }

    // The inbox starts first. It does not wait for the catalogue: an alert must not wait for it.
    state.inbox.start().await?;
    // The inbox asks the portal for the rights of keys and accounts (see `inbox_rights.rs`).
    state
        .inbox
        .set_authorizer(Arc::new(inbox_rights::PortalAuthorizer::new(&state)));

    // With the block `inbox`, the listener opens now and serves the API of the inbox only. The
    // inbox must take alerts while the catalogue is not complete. Without the block, the
    // ports open when the catalogue is complete (portal 7.3b).
    let mut accepting = Vec::new();
    let mut listener = None;
    if state.config.inbox.is_some() {
        accepting.push(tokio::spawn(accept_loop(
            Arc::clone(&state),
            bind_main(&state).await?,
        )));
    }

    let catalog_complete = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let publisher = Arc::clone(&state);
    let complete = Arc::clone(&catalog_complete);
    tokio::spawn(discovery::run(
        state.config.discovery.clone(),
        Arc::clone(&state.discovery),
        move |catalog| {
            let is_complete = catalog.is_complete();
            *publisher.catalog.write().unwrap_or_else(|p| p.into_inner()) = Arc::new(catalog);
            if is_complete {
                complete.store(true, std::sync::atomic::Ordering::SeqCst);
                // The first reads need no wait for the end of a tick.
                publisher.actions.wake_loop();
            }
        },
    ));
    let reader = Arc::clone(&state);
    let checker = Arc::clone(&state);
    tokio::spawn(actions::run(
        Arc::clone(&state.actions),
        move || current_catalog(&reader),
        move |service, document| log_grants_to_check(&checker, service, document),
        state.config.access.actions_timeout,
        state.config.access.actions_refresh,
    ));
    if !wait_ready(&state, &catalog_complete, &stopped).await {
        for task in &accepting {
            task.abort();
        }
        state.stop.close_connections();
        state.inbox.begin_drain();
        tokio::join!(
            state.stop.drain(crate::shutdown::DRAIN_LIMIT),
            state.inbox.finish(crate::shutdown::DRAIN_LIMIT)
        );
        return Ok(());
    }
    // From here the portal serves its own routes (see `AppState::serving`).
    state
        .serving
        .store(true, std::sync::atomic::Ordering::SeqCst);
    if accepting.is_empty() {
        listener = Some(bind_main(&state).await?);
    }
    let pdp_listener = match &state.config.pdp {
        Some(pdp) => Some(TcpListener::bind(pdp.listen_address).await.map_err(|e| {
            format!(
                "cannot listen on {} (pdp.listen_address): {e}",
                pdp.listen_address
            )
        })?),
        None => None,
    };
    // The alerts of the portal about itself. The task ends at once without the block.
    accepting.push(tokio::spawn(crate::watch::run(Arc::clone(&state))));
    if let Some(listener) = pdp_listener {
        accepting.push(tokio::spawn(pdp::serve(Arc::clone(&state), listener)));
    }
    log(
        &state,
        LogLevel::Info,
        "access_model",
        &[
            ("grants_mode", state.policy.mode.as_str().to_string()),
            (
                "configured_users",
                state.policy.configured.len().to_string(),
            ),
            ("identity_token", state.signer.is_some().to_string()),
            (
                "identity_key_id",
                state
                    .signer
                    .as_ref()
                    .map(|s| s.kid().to_string())
                    .unwrap_or_default(),
            ),
            ("pdp", state.config.pdp.is_some().to_string()),
        ],
    );
    warn_about_unused_grants(&state);

    log(
        &state,
        LogLevel::Info,
        "server_started",
        &[
            ("listen", state.config.server.listen_address.to_string()),
            ("workers", state.config.server.workers.to_string()),
            ("icons", state.icons.len().to_string()),
            (
                "max_connections",
                state.config.server.max_connections.to_string(),
            ),
        ],
    );

    if let Some(listener) = listener {
        accepting.push(tokio::spawn(accept_loop(Arc::clone(&state), listener)));
    }
    stopped.notified().await;
    state.inbox.begin_drain();
    log(
        &state,
        LogLevel::Info,
        "stopping",
        &[(
            "serve_seconds",
            crate::shutdown::SERVE_AFTER_SIGNAL.as_secs().to_string(),
        )],
    );
    tokio::time::sleep(crate::shutdown::SERVE_AFTER_SIGNAL).await;
    for task in &accepting {
        task.abort();
    }
    state.stop.close_connections();
    let (left, ()) = tokio::join!(
        state.stop.drain(crate::shutdown::DRAIN_LIMIT),
        state.inbox.finish(crate::shutdown::DRAIN_LIMIT)
    );
    log(
        &state,
        LogLevel::Info,
        "stopped",
        &[("connections_left", left.to_string())],
    );
    Ok(())
}

/// Opens the main listener (`server.listen_address`).
async fn bind_main(state: &AppState) -> Result<TcpListener, String> {
    TcpListener::bind(state.config.server.listen_address)
        .await
        .map_err(|e| {
            format!(
                "cannot listen on {}: {e}",
                state.config.server.listen_address
            )
        })
}

/// Accepts the connections of the main listener. It never ends on its own.
async fn accept_loop(state: Arc<AppState>, listener: TcpListener) {
    let connection_gate = Arc::new(Semaphore::new(state.config.server.max_connections));
    loop {
        let (stream, remote_addr) = match listener.accept().await {
            Ok(v) => v,
            Err(error) => {
                log(
                    &state,
                    LogLevel::Error,
                    "accept_error",
                    &[("error", error.to_string())],
                );
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                continue;
            }
        };

        let permit = match Arc::clone(&connection_gate).try_acquire_owned() {
            Ok(v) => v,
            Err(_) => {
                state.metrics.connection_rejected();
                drop(stream);
                continue;
            }
        };

        state.metrics.connection_accepted();
        let state_for_connection = Arc::clone(&state);
        tokio::spawn(async move {
            let _permit = permit;
            let _live = state_for_connection.stop.enter();
            let _ = stream.set_nodelay(true);
            let io = TokioIo::new(stream);
            let service_state = Arc::clone(&state_for_connection);
            let service = service_fn(move |request| {
                handle_request(Arc::clone(&service_state), remote_addr, request)
            });

            let mut builder = http1::Builder::new();
            builder
                .timer(TokioTimer::new())
                .header_read_timeout(state_for_connection.config.server.header_read_timeout)
                .max_headers(state_for_connection.config.server.max_headers)
                .max_buf_size(state_for_connection.config.server.max_header_bytes)
                .half_close(false)
                .keep_alive(true);

            // A phone upgrades its connection to a WebSocket. The connection then leaves this
            // task, and the permit of the pages goes back: the session has its own budget.
            if let Err(error) = state_for_connection
                .stop
                .drive_upgradable(builder.serve_connection(io, service).with_upgrades())
                .await
            {
                log(
                    &state_for_connection,
                    LogLevel::Debug,
                    "connection_closed_with_error",
                    &[
                        ("remote_addr", remote_addr.to_string()),
                        ("error", error.to_string()),
                    ],
                );
            }
            state_for_connection.metrics.connection_closed();
        });
    }
}

async fn handle_request(
    state: Arc<AppState>,
    remote_addr: SocketAddr,
    request: Request<Incoming>,
) -> Result<HttpResponse, Infallible> {
    let started = Instant::now();
    let method = request.method().clone();
    let method_metric = method_metric_label(&method);
    let uri = request
        .uri()
        .path_and_query()
        .map(|v| v.as_str())
        .unwrap_or(request.uri().path())
        .to_string();
    let path = request.uri().path().to_string();
    let route_metric = route_metric_label(&state, &path);
    let found_ip = transport::client_ip(
        request.headers(),
        remote_addr.ip(),
        &state.config.server.trusted_proxies,
    );
    let client_ip = Some(found_ip);
    let client = Client::new(&state, request.headers(), client_ip);
    // The inbox owns `/v1/`, `/api/v1/`, `/readyz` and `/api/version` on this listener: the phones
    // (WebSocket), the API of the producers and the webhooks. No session and no other handler apply
    // to them. Without the block `inbox`, these paths are not found.
    let inbox_app = if crate::inbox::owns(&path) {
        state.inbox.app()
    } else {
        None
    };

    // The listener is open for the inbox before the catalogue is complete (see `AppState::serving`).
    // Any other route fails closed: the gate must not decide with an incomplete catalogue.
    let starting = inbox_app.is_none() && !state.serving.load(std::sync::atomic::Ordering::SeqCst);

    let response = if starting {
        text_response(StatusCode::SERVICE_UNAVAILABLE, "starting\n")
    } else if state.config.metrics.enabled
        && method == Method::GET
        && path == state.config.metrics.path
    {
        handle_metrics(&state)
    } else if path == VERIFY_PATH {
        handle_verify(&state, request.headers(), &client)
    } else if path == VERIFY_KEY_PATH {
        edge::handle_verify_key(&state, request.headers(), &client)
    } else if method == Method::GET && path == HEALTH_PATH {
        text_response(StatusCode::OK, "ok\n")
    } else if let Some(app) = inbox_app {
        crate::inbox::serve(app, found_ip, request).await
    } else if method == Method::GET && path == READY_PATH && state.config.inbox.is_none() {
        // Without the block `inbox`, the instance is ready whenever it runs.
        text_response(StatusCode::OK, "ready\n")
    } else if inbox_page::is_inbox_path(&path) {
        inbox_page::handle(&state, request, &client).await
    } else if method == Method::GET && path == ui::keys::NEW_PATH {
        keys_page::handle_keys(
            &state,
            &uri,
            request.headers(),
            &client,
            keys_page::Page::New,
        )
        .await
    } else if method == Method::GET && path == options_page::PATH {
        options_page::handle_options(&state, &uri, request.headers(), &client).await
    } else if method == Method::GET && path == ui::keys::EDIT_PATH {
        keys_page::handle_keys(
            &state,
            &uri,
            request.headers(),
            &client,
            keys_page::Page::Edit,
        )
        .await
    } else if method == Method::POST && path == ui::keys::CREATE_PATH {
        keys_page::handle_keys_create(&state, request, &client).await
    } else if method == Method::POST && path == ui::keys::EDIT_PATH {
        keys_page::handle_keys_edit(&state, request, &client).await
    } else if method == Method::POST && path == ui::keys::REVOKE_PATH {
        keys_page::handle_keys_revoke(&state, request, &client).await
    } else if method == Method::GET && path == HOME_PATH {
        handle_home(&state, request.headers(), &client).await
    } else if method == Method::GET && path == state.mask.assets().css_path {
        hashed_response("text/css; charset=utf-8", &state.mask.assets().css)
    } else if method == Method::GET && path == state.mask.assets().js_path {
        hashed_response("text/javascript; charset=utf-8", &state.mask.assets().js)
    } else if method == Method::GET && path == ui::assets().css_path {
        hashed_response("text/css; charset=utf-8", &ui::assets().css)
    } else if method == Method::GET && path == ui::assets().js_path {
        hashed_response("text/javascript; charset=utf-8", &ui::assets().js)
    } else if method == Method::GET && path == state.mask.assets().notify_path {
        hashed_response(
            "text/javascript; charset=utf-8",
            &state.mask.assets().notify,
        )
    } else if method == Method::GET && path == ui::favicon_path() && state.tab_look().icon {
        handle_favicon(&state, &uri, request.headers(), &client)
    } else if method == Method::GET && path == STYLE_PATH {
        static_response("text/css; charset=utf-8", &ui::assets().css)
    } else if method == Method::GET && path.starts_with(ICONS_PREFIX) {
        handle_icon(&state, &path)
    } else if method == Method::GET && path == SETTINGS_STYLE_PATH {
        static_response("text/css; charset=utf-8", ui::SETTINGS_CSS)
    } else if method == Method::GET && path == THEME_JS_PATH {
        static_response("text/javascript; charset=utf-8", &ui::assets().js)
    } else if method == Method::GET && path == SETTINGS_JS_PATH {
        static_response("text/javascript; charset=utf-8", ui::SETTINGS_JS)
    } else if method == Method::GET && path == NOTIFY_JS_PATH {
        static_response("text/javascript; charset=utf-8", ui::NOTIFY_JS)
    } else if method == Method::GET && is_settings_path(&path) {
        handle_settings(&state, &path, &uri, request.headers(), &client).await
    } else if method == Method::GET && path == SUCCESS_PATH {
        handle_success(&state, request.headers(), &client)
    } else if method == Method::GET && path == DENIED_PATH {
        handle_denied(&state, request.headers(), &client)
    } else if method == Method::POST && path == USERS_ADD_PATH {
        handle_users_add(&state, request, &client).await
    } else if method == Method::POST && path == USERS_DELETE_PATH {
        handle_users_delete(&state, request, &client).await
    } else if method == Method::POST && path == USERS_UPDATE_PATH {
        handle_users_update(&state, request, &client).await
    } else if method == Method::POST && path == IMPERSONATE_PATH {
        handle_impersonate(&state, request, &client).await
    } else if method == Method::POST && path == IMPERSONATE_END_PATH {
        handle_impersonate_end(&state, request, &client).await
    } else if method == Method::POST && path == END_SESSIONS_PATH {
        handle_end_sessions(&state, request, &client).await
    } else if method == Method::POST && path == ui::mfa::CODE_PATH {
        mfa_page::handle_code_post(&state, request, &client).await
    } else if method == Method::POST && path == ui::mfa::ENROL_PATH {
        mfa_page::handle_enrol_post(&state, request, &client).await
    } else if method == Method::POST && path == ui::mfa::SECURITY_ENABLE_PATH {
        mfa_page::handle_security_enable(&state, request, &client).await
    } else if method == Method::POST && path == ui::mfa::SECURITY_DISABLE_PATH {
        mfa_page::handle_security_disable(&state, request, &client).await
    } else if method == Method::POST && path == ui::mfa::SECURITY_RECOVERY_PATH {
        mfa_page::handle_security_recovery(&state, request, &client).await
    } else if method == Method::POST && path == ui::mfa::RESET_PATH {
        mfa_page::handle_reset(&state, request, &client).await
    } else if method == Method::GET && path == LOGIN_PATH {
        handle_login_get(&state, &uri, request.headers(), &client)
    } else if method == Method::POST && path == LOGIN_PATH {
        handle_login_post(&state, request, &client).await
    } else if method == Method::GET && path == LOGOUT_PATH {
        // The sign-out action is the POST of the launcher button. A plain visit changes nothing.
        redirect_response(StatusCode::SEE_OTHER, HOME_PATH)
    } else if method == Method::POST && path == LOGOUT_PATH {
        handle_logout_post(&state, request, &client).await
    } else {
        handle_not_found(&state, &method, &uri, request.headers(), &client)
    };
    // A form of the portal that changed the database waits for the copy of the replica, as a
    // write of the inbox does: a takeover must not bring back a deleted account, a revoked key or a
    // used recovery code. The sign-in and the sign-out do not wait. Nothing to copy: no wait.
    if method == Method::POST
        && !starting
        && !crate::inbox::owns(&path)
        && path != LOGIN_PATH
        && path != LOGOUT_PATH
    {
        state.inbox.sync_replica().await;
    }

    let status = response.status().as_u16();
    state
        .metrics
        .observe_http_request(method_metric, route_metric, status, started.elapsed());

    let request_log_level = if matches!(route_metric, "metrics" | "health" | "ready") {
        LogLevel::Debug
    } else {
        LogLevel::Info
    };
    log(
        &state,
        request_log_level,
        "request",
        &[
            ("method", method.as_str().to_string()),
            ("path", path),
            ("status", status.to_string()),
            (
                "client_ip",
                client_ip
                    .map(|v| v.to_string())
                    .unwrap_or_else(|| "unknown".into()),
            ),
        ],
    );

    Ok(response)
}

/// In the mode `static`, the server does not use the grants that the database keeps.
/// The server says so at start. Therefore, no user thinks that the grants apply.
fn warn_about_unused_grants(state: &AppState) {
    if state.policy.mode.uses_stored() {
        return;
    }
    let users = state
        .users
        .list()
        .into_iter()
        .filter(|(_, record)| !record.grants.is_empty())
        .count();
    if users > 0 {
        log(
            state,
            LogLevel::Warn,
            "stored_grants_not_used",
            &[
                ("users", users.to_string()),
                (
                    "detail",
                    "access.grants_mode is static: the grants that the settings pages gave are kept but not used".into(),
                ),
            ],
        );
    }
}

fn handle_metrics(state: &AppState) -> HttpResponse {
    let mut body = state.metrics.render(
        build_version(),
        state.ip_limiter.entry_count(),
        state.global_limiter.entry_count(),
        state.config.security.login_attempts_per_ip,
        state.config.security.global_login_attempts,
        state.config.server.workers,
        state.config.security.max_concurrent_password_checks,
        state.sessions.active_count(),
        state.sessions.capacity(),
    );
    body.push_str(&state.discovery.render());
    body.push_str(&state.inbox.render_metrics());
    body.push_str(
        &state.alerts.render(
            state
                .config
                .inbox
                .as_ref()
                .is_some_and(|p| p.watch.is_some()),
        ),
    );
    if state.config.pdp.is_some() {
        body.push_str(&state.pdp_stats.render());
    }
    body.push_str(&crate::keys_metrics::render(state));
    body.push_str(&state.metrics.render_accounts(
        state.users.count(),
        state.tracker.dropped(),
        state.tracker.flush_errors(),
    ));
    if state.store.is_shared() {
        body.push_str(&state.metrics.render_shared(
            state.sessions.shared_errors(),
            state.users.refresh_errors(),
            state.users.age(),
        ));
    }
    metrics_response(StatusCode::OK, body)
}

fn handle_verify(state: &AppState, headers: &HeaderMap, client: &Client) -> HttpResponse {
    let original_uri = match unique_header_str(headers, "x-forwarded-uri") {
        Ok(Some(v)) if validate_return_uri(v, state.config.security.max_forwarded_uri_bytes) => v,
        _ => {
            state.metrics.verify_bad_request();
            return text_response(StatusCode::BAD_REQUEST, "invalid forwarded uri\n");
        }
    };

    let original_path = path_only(original_uri);
    if matches!(
        original_path,
        LOGIN_PATH
            | LOGOUT_PATH
            | ui::mfa::CODE_PATH
            | ui::mfa::ENROL_PATH
            | STYLE_PATH
            | THEME_JS_PATH
    ) || is_neutral_asset(state, original_path)
    {
        state.metrics.verify_allowed();
        return empty_response(StatusCode::NO_CONTENT);
    }

    // A path that a service declares with `portal.exclude` passes without a session, for example a webhook or a health probe.
    // Its caller cannot sign in. All other paths need a session.
    let catalog = current_catalog(state);
    let method = unique_header_str(headers, "x-forwarded-method")
        .ok()
        .flatten();
    if access::excluded(&catalog, method, original_uri).is_some() {
        state.metrics.verify_excluded();
        return empty_response(StatusCode::NO_CONTENT);
    }

    let Some(viewer) = current_viewer(state, headers, client) else {
        return match login_location(state, original_uri) {
            Ok(location) => {
                state.metrics.verify_redirected();
                redirect_response(StatusCode::FOUND, &location)
            }
            Err(()) => {
                state.metrics.verify_internal_error();
                text_response(StatusCode::INTERNAL_SERVER_ERROR, "authentication error\n")
            }
        };
    };

    // The `access` decision runs in the process. The grants are in memory.
    match access::decide(&catalog, viewer.grants.as_deref(), original_uri) {
        Decision::Allow { service } => {
            let mut response = empty_response(StatusCode::NO_CONTENT);
            if let Some(id) = service {
                let now = unix_now();
                state
                    .tracker
                    .service_hit(viewer.session.sid, &id, ip_text(client), now);
                // The identity token of the service (contract 11.3). Traefik copies it to the
                // request with `authResponseHeaders`.
                if let (Some(signer), Some(identity)) = (&state.signer, &state.config.identity) {
                    let token = signer.token(viewer.session.sid, &viewer.login, &id, now);
                    if let Ok(name) = HeaderName::from_bytes(identity.header.as_bytes()) {
                        if set_dynamic_header(&mut response, name, &token).is_err() {
                            state.metrics.verify_internal_error();
                            return text_response(
                                StatusCode::INTERNAL_SERVER_ERROR,
                                "authentication error\n",
                            );
                        }
                    }
                }
                log(
                    state,
                    LogLevel::Debug,
                    "access_decision",
                    &[
                        ("subject", viewer.login.clone()),
                        ("action", grants::ACCESS.to_string()),
                        ("resource", format!("{}:{id}", grants::SERVICE_TYPE)),
                        ("decision", "true".into()),
                    ],
                );
            }
            state.metrics.verify_allowed();
            response
        }
        Decision::Deny => {
            state.metrics.verify_denied();
            log(
                state,
                LogLevel::Info,
                "access_decision",
                &[
                    ("subject", viewer.login.clone()),
                    ("action", grants::ACCESS.to_string()),
                    ("path", path_only(original_uri).chars().take(256).collect()),
                    ("decision", "false".into()),
                ],
            );
            // A browser that navigates to a page gets the "access denied" page.
            // The answer to ForwardAuth cannot carry a page, because Traefik limits its body. Any other request gets a plain 403.
            // The page does not name the service. The user does not see a service that is not open to them.
            let navigating = matches!(
                unique_header_str(headers, "x-forwarded-method"),
                Ok(Some("GET")) | Ok(Some("HEAD"))
            );
            if navigating {
                redirect_response(StatusCode::FOUND, DENIED_PATH)
            } else {
                text_response(StatusCode::FORBIDDEN, "Forbidden\n")
            }
        }
    }
}

/// Any other path. Without a session, the portal sends the visitor to the login page.
/// This occurs whether or not the path exists. Without a session, a missing page cannot be told apart from a protected page.
fn handle_not_found(
    state: &AppState,
    method: &Method,
    uri: &str,
    headers: &HeaderMap,
    client: &Client,
) -> HttpResponse {
    not_found_response(state, method, uri, || {
        has_valid_session(state, headers, client)
    })
}

/// The answer for a path that does not exist. `signed_in` tells whether the visitor has a session. It runs only when
/// the answer depends on it, so a caller that already knows the visitor has none gives a function with no check.
fn not_found_response(
    state: &AppState,
    method: &Method,
    uri: &str,
    signed_in: impl FnOnce() -> bool,
) -> HttpResponse {
    if *method == Method::GET
        && validate_return_uri(uri, state.config.security.max_forwarded_uri_bytes)
        && !signed_in()
    {
        if let Ok(location) = login_location(state, uri) {
            return redirect_response(StatusCode::FOUND, &location);
        }
    }
    if *method == Method::GET {
        masked_response(
            state,
            StatusCode::NOT_FOUND,
            ui::message_page(
                "Page not found",
                "There is nothing at this address.",
                "Back to panel",
            ),
        )
    } else {
        text_response(StatusCode::NOT_FOUND, "Not Found\n")
    }
}

/// `/auth/denied`: the gate sends a browser here when its grants do not open the requested service.
/// The page never names a service. The user does not see a service that is not open to them.
fn handle_denied(state: &AppState, headers: &HeaderMap, client: &Client) -> HttpResponse {
    let Some(viewer) = current_viewer(state, headers, client) else {
        return redirect_response(StatusCode::SEE_OTHER, LOGIN_PATH);
    };
    let Ok(logout_token) = sign_action_token(&state.signing_secret, LOGOUT_TOKEN, &viewer.token)
    else {
        return text_response(StatusCode::INTERNAL_SERVER_ERROR, "authentication error\n");
    };
    masked_response(
        state,
        StatusCode::FORBIDDEN,
        ui::denied_page(ui::DENIED_TEXT, &viewer.login, &logout_token),
    )
}

/// The login URL. After authentication, it returns the browser to `original_uri`.
/// The home page is the return address of a login without a destination: the URL has no parameter then.
fn login_location(state: &AppState, original_uri: &str) -> Result<String, ()> {
    if original_uri == HOME_PATH {
        return Ok(LOGIN_PATH.to_string());
    }
    let signature = sign_return_uri(&state.signing_secret, original_uri).map_err(|_| ())?;
    Ok(format!(
        "{LOGIN_PATH}?rd={}&sig={}",
        percent_encode_query_component(original_uri),
        percent_encode_query_component(&signature)
    ))
}

/// `/auth/icons/<set>/<name>.svg`: one icon from the offline library.
fn handle_icon(state: &AppState, path: &str) -> HttpResponse {
    let found = path
        .strip_prefix(ICONS_PREFIX)
        .and_then(|rest| rest.split_once('/'))
        .and_then(|(set, file)| Some((Set::from_dir(set)?, file.strip_suffix(".svg")?)))
        .and_then(|(set, name)| state.icons.svg(set, name));
    let Some(svg) = found else {
        return text_response(StatusCode::NOT_FOUND, "Not Found\n");
    };
    let mut response = response_with_body(StatusCode::OK, Bytes::from(svg));
    response
        .headers_mut()
        .insert(CONTENT_TYPE, HeaderValue::from_static("image/svg+xml"));
    response.headers_mut().insert(
        HeaderName::from_static("cache-control"),
        HeaderValue::from_static("public, max-age=86400"),
    );
    apply_secure_headers(&mut response, false);
    // An SVG that opens on its own can run script. The icons are data, so the policy allows no source at all.
    response.headers_mut().insert(
        HeaderName::from_static("content-security-policy"),
        HeaderValue::from_static("default-src 'none'; style-src 'unsafe-inline'; sandbox"),
    );
    response
}

/// The icon of the tab. Only a signed-in browser gets it: the file shows the logo, and a visitor who is not signed in
/// must not find the product with it. A visitor gets the answer of any unknown path, so the path of the icon, which
/// follows from the content of the file, does not look different from a path that never existed. The use of the
/// icon does not renew the idle timer, as the poll of the notifications.
fn handle_favicon(
    state: &AppState,
    uri: &str,
    headers: &HeaderMap,
    client: &Client,
) -> HttpResponse {
    if current_viewer_passive(state, headers, client).is_none() {
        // The check above found no session: the answer needs no second check, which would renew the idle timer.
        return not_found_response(state, &Method::GET, uri, || false);
    }
    let mut response = hashed_response("image/svg+xml", ui::FAVICON_SVG);
    // The cache is private: a shared cache must not keep a file that needs a session.
    response.headers_mut().insert(
        HeaderName::from_static("cache-control"),
        HeaderValue::from_static("private, max-age=31536000, immutable"),
    );
    response
}

/// The launcher. The portal checks the session itself. The route does not go through ForwardAuth.
async fn handle_home(state: &Arc<AppState>, headers: &HeaderMap, client: &Client) -> HttpResponse {
    let Some(viewer) = current_viewer(state, headers, client) else {
        return match login_location(state, HOME_PATH) {
            Ok(location) => redirect_response(StatusCode::FOUND, &location),
            Err(()) => text_response(StatusCode::INTERNAL_SERVER_ERROR, "authentication error\n"),
        };
    };
    let (Ok(logout_token), Ok(users_token)) = (
        sign_action_token(&state.signing_secret, LOGOUT_TOKEN, &viewer.token),
        sign_action_token(&state.signing_secret, USERS_TOKEN, &viewer.token),
    ) else {
        return text_response(StatusCode::INTERNAL_SERVER_ERROR, "authentication error\n");
    };

    // The list has only the services that the viewer can open.
    // The discovery problems name workloads, so only the superadmin sees them.
    // The inbox is a feature of the portal, as the keys are: it has no tile. The gear opens its pages.
    let catalog = current_catalog(state);
    let superadmin = is_superadmin(&viewer);
    let shown = match &viewer.grants {
        Some(grants) => Arc::new(catalog.filtered(|s| may_open(Some(grants), &s.id), superadmin)),
        None if superadmin => Arc::clone(&catalog),
        None => Arc::new(catalog.filtered(|_| true, false)),
    };
    let gear = if superadmin || keys_page::list_visible(state, &viewer) {
        Some(ui::settings::ACCESS_PATH)
    } else if inbox_page::tab_visible(state, &viewer) {
        Some(ui::settings::INBOX_PATH)
    } else if mfa_page::tab_visible(state, &viewer) {
        Some(ui::mfa::SECURITY_PATH)
    } else {
        None
    };

    let bell_data = inbox_page::launcher_bell(state, &viewer).await;
    let bell = bell_data.as_ref().map(|b| ui::notify::Bell {
        header: b.header,
        rows: &b.rows,
        now_ms: b.now_ms,
        csrf: &b.csrf,
        forms: !viewer.session.impersonated,
    });
    let page = ui::home_page(
        &shown,
        &state.icons,
        &ui::HomeContext {
            bell: bell.as_ref(),
            viewer: &viewer.login,
            impersonating: viewer.session.impersonated,
            logout_token: &logout_token,
            users_token: &users_token,
            hidden_services: catalog.service_count() - shown.service_count(),
            settings: gear,
            tab: state.tab_look(),
        },
    );
    masked_response(state, StatusCode::OK, page)
}

/// The superadmin in its own right. This does not apply while the superadmin browses as a user.
fn is_superadmin(viewer: &Viewer) -> bool {
    viewer.session.principal == Principal::Superadmin && !viewer.session.impersonated
}

fn is_settings_path(path: &str) -> bool {
    matches!(
        path,
        ui::settings::ACCESS_PATH
            | ui::settings::SERVICES_PATH
            | ui::settings::ACTIVITY_PATH
            | ui::settings::NEW_USER_PATH
            | ui::settings::GIVE_PATH
            | ui::mfa::SECURITY_PATH
    ) || path.starts_with(ui::settings::USER_PATH_PREFIX)
}

/// One page of the settings. The route of the page is its tab. The portal checks the session itself.
/// The route does not go through ForwardAuth. For any user other than the superadmin, the page does not exist.
async fn handle_settings(
    state: &Arc<AppState>,
    path: &str,
    uri: &str,
    headers: &HeaderMap,
    client: &Client,
) -> HttpResponse {
    let Some(viewer) = current_viewer(state, headers, client) else {
        return match login_location(state, uri) {
            Ok(location) => redirect_response(StatusCode::FOUND, &location),
            Err(()) => text_response(StatusCode::INTERNAL_SERVER_ERROR, "authentication error\n"),
        };
    };
    // An account that is not the superadmin sees the tab Access with the list of its keys, and
    // nothing else: the other pages do not exist for it.
    let superadmin = is_superadmin(&viewer);
    if path == ui::mfa::SECURITY_PATH {
        // The page of the second factor is the person's own page: every account has it, and a session of
        // "Log in as" has none.
        if !mfa_page::tab_visible(state, &viewer) {
            return handle_not_found(state, &Method::GET, uri, headers, client);
        }
        return mfa_page::handle_security_get(state, uri, &viewer).await;
    }
    if !superadmin
        && !(path == ui::settings::ACCESS_PATH && keys_page::list_visible(state, &viewer))
    {
        state.metrics.user_admin("settings", "forbidden");
        return handle_not_found(state, &Method::GET, uri, headers, client);
    }
    let (Ok(logout_token), Ok(users_token)) = (
        sign_action_token(&state.signing_secret, LOGOUT_TOKEN, &viewer.token),
        sign_action_token(&state.signing_secret, USERS_TOKEN, &viewer.token),
    ) else {
        return text_response(StatusCode::INTERNAL_SERVER_ERROR, "authentication error\n");
    };
    let frame = ui::Frame {
        logout_token: &logout_token,
        keys: keys_page::list_visible(state, &viewer),
        inbox: inbox_page::tab_visible(state, &viewer),
        superadmin,
        notify: inbox_page::notify_header(state, &viewer).await,
        security: mfa_page::tab_visible(state, &viewer),
        tab: state.tab_look(),
    };
    // The portal features are not in the catalogue: the pages of the services list the workloads
    // only.
    let workloads = current_catalog(state);
    let page = match path {
        ui::settings::SERVICES_PATH => {
            let services = service_grants(state, &workloads);
            let holders = holders_of(state);
            ui::services_page(
                &frame,
                &ui::ServicesView {
                    catalog: &workloads,
                    services: &services,
                    holders: &holders,
                    problems: state.actions.problems(),
                    mode: state.policy.mode,
                    selected: query_value_up_to(uri, "svc", grants::MAX_SERVICE_CHARS * 4),
                    only_check: query_value(uri, "f").as_deref() == Some("check"),
                    checks: query_value(uri, "v").as_deref() == Some("checks"),
                },
            )
        }
        ui::settings::ACCESS_PATH => {
            let keys = match keys_page::list(state, &viewer).await {
                Ok(keys) => keys,
                Err(response) => return response,
            };
            let accounts = if superadmin {
                let (users, snapshot) = load_admin_data(state).await;
                let two_factor = mfa_page::enabled_logins(state).await;
                Some(accounts_data(
                    state,
                    &workloads,
                    &users_token,
                    users,
                    &snapshot,
                    two_factor,
                ))
            } else {
                None
            };
            ui::access_page(
                &frame,
                &ui::AccessView {
                    notice: query_value(uri, "s").and_then(|code| {
                        ui::notice_for(&code).or_else(|| ui::keys::notice_for(&code))
                    }),
                    query: access_query(uri),
                    accounts,
                    keys,
                    groups: inbox_groups(state),
                },
            )
        }
        ui::settings::ACTIVITY_PATH => {
            let (_, snapshot) = load_admin_data(state).await;
            let only_refused = query_value(uri, "r").as_deref() == Some("failed");
            let logins = load_login_page(state, snapshot.first_day, only_refused).await;
            ui::activity_page(
                &frame,
                &ui::ActivityView {
                    notice: query_value(uri, "s").and_then(|code| ui::notice_for(&code)),
                    logins: &logins,
                    changes: query_value(uri, "v").as_deref() == Some("changes"),
                    only_refused,
                    days: &snapshot.days,
                    first_day: snapshot.first_day,
                    recent_events: &snapshot.recent_events,
                },
            )
        }
        // "New user": the page of an account in create mode. The route does not exist when the file
        // forbids the creation or the grants are read-only.
        ui::settings::NEW_USER_PATH => {
            if !state.config.users.allow_creation || !state.policy.mode.editable() {
                return handle_not_found(state, &Method::GET, uri, headers, client);
            }
            ui::new_user_page(
                &frame,
                &ui::NewUserView {
                    notice: query_value(uri, "s").and_then(|code| ui::notice_for(&code)),
                    field: query_value(uri, "s")
                        .and_then(|code| ui::settings::user_field_of(&code)),
                    services: service_grants(state, &workloads),
                    features: features_service(state),
                    groups: group_chips(state),
                    token: &users_token,
                    refused: None,
                },
            )
        }
        // "Give access" in the panel "Opened by": the page of the account opens with the service
        // chosen in the editor of grants.
        ui::settings::GIVE_PATH => {
            let login = query_value(uri, "user");
            let service = query_value_up_to(uri, "service", grants::MAX_SERVICE_CHARS * 4);
            match (login, service) {
                (Some(login), Some(service)) if state.users.get(&login).is_some() => {
                    return redirect_response(
                        StatusCode::SEE_OTHER,
                        &ui::settings::give_target(&login, &service),
                    );
                }
                _ => return handle_not_found(state, &Method::GET, uri, headers, client),
            }
        }
        _ => {
            // The page of one account: the login must be an account or the superadmin.
            let asked = path
                .strip_prefix(ui::settings::USER_PATH_PREFIX)
                .unwrap_or_default();
            let (users, snapshot) = load_admin_data(state).await;
            let two_factor = mfa_page::enabled_logins(state).await;
            match user_view(
                state,
                &workloads,
                uri,
                &users_token,
                asked,
                users,
                &snapshot,
                two_factor.as_ref(),
            ) {
                Some(view) => ui::user_page(&frame, &view),
                None => return handle_not_found(state, &Method::GET, uri, headers, client),
            }
        }
    };
    html_response(StatusCode::OK, page)
}

/// The sign-ins since `since` (the first day of the graph), and the figures of the chips. If the
/// database cannot be read, the panel is empty and the logs get one line.
async fn load_login_page(state: &Arc<AppState>, since: i64, only_failed: bool) -> LoginPage {
    let store = Arc::clone(&state.store);
    match tokio::task::spawn_blocking(move || store.login_attempts(since, only_failed)).await {
        Ok(Ok(data)) => data,
        Ok(Err(error)) => {
            log(
                state,
                LogLevel::Error,
                "history_unreadable",
                &[("error", error)],
            );
            LoginPage::default()
        }
        Err(_) => LoginPage::default(),
    }
}

/// The accounts and their history. The Users page uses them.
async fn load_admin_data(state: &Arc<AppState>) -> (Vec<(String, UserRecord)>, Snapshot) {
    let users = state.users.list();
    let mut accounts: Vec<(String, bool)> = users
        .iter()
        .map(|(login, _)| (login.clone(), false))
        .collect();
    // The accounts of the file sign in too: the Users page shows their history.
    accounts.extend(
        state
            .config
            .access
            .accounts
            .keys()
            .map(|login| (login.clone(), false)),
    );
    accounts.push((state.superadmin_login.clone(), true));
    let now = unix_now();
    let store = Arc::clone(&state.store);
    let asked = accounts.clone();
    let snapshot = match tokio::task::spawn_blocking(move || store.snapshot(&asked, now)).await {
        Ok(Ok(snapshot)) => snapshot,
        Ok(Err(error)) => {
            log(
                state,
                LogLevel::Error,
                "history_unreadable",
                &[("error", error)],
            );
            Snapshot::blank(&accounts, now)
        }
        Err(_) => Snapshot::blank(&accounts, now),
    };
    (users, snapshot)
}

/// Writes one warning for each grant below `service` that does not follow its new document: the
/// PDP does not apply this grant. The refresh loop of the documents calls it at the first answer
/// of a service after the start and at each change of its document. The Services page shows the
/// same grants under "Permissions to check".
fn log_grants_to_check(state: &AppState, service: &str, document: &actions::Document) {
    crate::keys_metrics::refresh_undeclared(state);
    for (login, grant, problem) in grants_to_check(state, service, document) {
        log(
            state,
            LogLevel::Warn,
            "grant_to_check",
            &[
                ("login", login),
                ("grant", grant.to_string()),
                ("problem", problem),
                (
                    "detail",
                    "the PDP does not apply this grant: give it again on the tree of the service"
                        .into(),
                ),
            ],
        );
    }
}

/// The grants below `service` that do not follow `document`, with the login and the problem. The
/// accounts come first, then the `user` blocks without an account.
fn grants_to_check(
    state: &AppState,
    service: &str,
    document: &actions::Document,
) -> Vec<(String, Grant, String)> {
    let mut holders: Vec<(String, Vec<Grant>)> = state
        .users
        .list()
        .into_iter()
        .map(|(login, record)| {
            let grants = state.policy.effective(&login, &record.grants);
            (login, grants)
        })
        .collect();
    for (login, grants) in &state.policy.configured {
        if !holders.iter().any(|(known, _)| known == login) {
            holders.push((login.clone(), grants.clone()));
        }
    }
    let mut out = Vec::new();
    for (login, grants) in holders {
        for grant in grants {
            if grant.service != service || grant.path.is_empty() {
                continue;
            }
            if let Err(problem) = document.check_grant(&grant) {
                out.push((login.clone(), grant, problem));
            }
        }
    }
    out
}

/// Each service of the catalog, with the actions that the settings pages offer.
/// The first action is `access`. The other actions are the actions that the service publishes.
/// The tree of types of the last known document gives the grants below the service that the
/// pages offer.
fn service_grants(state: &AppState, catalog: &Catalog) -> Vec<ui::ServiceActions> {
    catalog
        .groups()
        .iter()
        .flat_map(|group| group.services.iter())
        .map(|service| {
            let declared = state.actions.declared(&service.id);
            let known = declared.is_some();
            let document = declared.unwrap_or_default();
            ui::ServiceActions {
                id: service.id.clone(),
                name: service.name.clone(),
                actions: state
                    .actions
                    .actions(&service.id)
                    .into_iter()
                    .map(|a| (a.name, a.description))
                    .collect(),
                document,
                known,
                fetch: state.signer.is_some()
                    && state.config.identity.is_some()
                    && service.actions_url.is_some(),
                critical: Vec::new(),
                source: state.actions.source(&service.id),
            }
        })
        .collect()
}

/// The history of each account of a snapshot, by login.
fn activities_of(snapshot: &Snapshot) -> std::collections::HashMap<String, AccountActivity> {
    snapshot
        .accounts
        .iter()
        .map(|activity| (activity.login.clone(), activity.clone()))
        .collect()
}

/// The number of keys of each account of the database. One pass over the keys.
fn key_counts(state: &AppState) -> std::collections::HashMap<String, usize> {
    let mut counts = std::collections::HashMap::new();
    for key in state.users.keys() {
        if let KeyOwner::User(owner) = &key.owner {
            *counts.entry(owner.clone()).or_insert(0) += 1;
        }
    }
    counts
}

/// The card of an account of the file. The pages show it and change none of it.
fn file_card(
    state: &AppState,
    login: &str,
    account: &crate::config::FileAccount,
    activity: Option<AccountActivity>,
) -> ui::UserCard {
    ui::UserCard {
        activity: activity.unwrap_or_default(),
        groups: groups_of(state, &account.grants),
        grants: account.grants.clone(),
        configured: state.policy.configured(login).to_vec(),
        login: login.to_string(),
        created_at: None,
        superadmin: false,
        file: true,
        keys: account.keys.len(),
    }
}

/// The accounts of the tab Access.
fn accounts_data<'a>(
    state: &'a AppState,
    catalog: &'a Catalog,
    users_token: &'a str,
    users: Vec<(String, UserRecord)>,
    snapshot: &Snapshot,
    two_factor: Option<std::collections::HashSet<String>>,
) -> ui::Accounts<'a> {
    let mut activities = activities_of(snapshot);
    let counts = key_counts(state);
    let mut cards: Vec<ui::UserCard> = users
        .into_iter()
        .map(|(login, record)| {
            let grants = state.policy.effective(&login, &record.grants);
            ui::UserCard {
                activity: activities.remove(&login).unwrap_or_default(),
                groups: groups_of(state, &grants),
                grants,
                configured: state.policy.configured(&login).to_vec(),
                keys: counts.get(&login).copied().unwrap_or(0),
                login,
                created_at: Some(record.created_at),
                superadmin: false,
                file: false,
            }
        })
        .collect();
    for (login, account) in &state.config.access.accounts {
        cards.push(file_card(state, login, account, activities.remove(login)));
    }
    ui::Accounts {
        allow_creation: state.config.users.allow_creation,
        mode: state.policy.mode,
        catalog,
        services: service_grants(state, catalog),
        users: cards,
        features: features_service(state),
        max_users: state.config.users.max_users,
        token: users_token,
        two_factor,
    }
}

/// The query of the tab Access. A value that is too long names nothing: the lists are then empty.
fn access_query(uri: &str) -> ui::access::Query {
    let text = |key: &str| query_value_up_to(uri, key, 400).unwrap_or_default();
    ui::access::Query {
        user: text("user"),
        group: text("group"),
        service: text("service"),
        kind: query_value(uri, "k").and_then(|name| crate::keys::KeyKind::from_name(&name)),
        mine: query_value(uri, "mine").as_deref() == Some("1"),
    }
}

/// The members of each group of the inbox: one pass over the grants of the accounts. The superadmin
/// is a member of each group.
fn group_members(state: &AppState) -> std::collections::HashMap<String, usize> {
    let mut members: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    let mut count = |grants: &[Grant]| {
        for id in groups_of(state, grants) {
            *members.entry(id).or_insert(0) += 1;
        }
    };
    for (login, record) in state.users.list() {
        count(&state.policy.effective(&login, &record.grants));
    }
    for account in state.config.access.accounts.values() {
        count(&account.grants);
    }
    if let Some(inbox) = state.config.inbox.as_ref() {
        for id in inbox.groups.keys() {
            *members.entry(id.clone()).or_insert(0) += 1;
        }
    }
    members
}

/// The groups of the inbox with their members, for the chips of the forms of an account. The
/// critical groups come last. Empty without the block `inbox`.
fn group_chips(state: &AppState) -> Vec<ui::GroupChip> {
    let members = group_members(state);
    inbox_groups(state)
        .into_iter()
        .map(|group| ui::GroupChip {
            members: members.get(&group.id).copied().unwrap_or(0),
            id: group.id,
            name: group.name,
            critical: group.critical,
        })
        .collect()
}

/// The keys of an account, by name, for the chips of its page.
fn key_chips(state: &AppState, login: &str) -> Vec<ui::KeyChip> {
    let mut keys: Vec<ui::KeyChip> = state
        .users
        .keys()
        .iter()
        .filter(|key| {
            matches!(&key.owner, KeyOwner::User(owner) | KeyOwner::File(owner) if owner == login)
        })
        .map(|key| ui::KeyChip {
            name: key.name.clone(),
            kind: key.kind,
        })
        .collect();
    keys.sort_by(|a, b| a.name.cmp(&b.name));
    keys
}

/// The groups of the inbox, the critical groups last. Empty without the block `inbox`.
fn inbox_groups(state: &AppState) -> Vec<ui::access::GroupLine> {
    let Some(inbox) = state.config.inbox.as_ref() else {
        return Vec::new();
    };
    let mut groups: Vec<ui::access::GroupLine> = inbox
        .groups
        .iter()
        .map(|(id, group)| ui::access::GroupLine {
            id: id.clone(),
            name: group.name.clone(),
            critical: group.critical,
        })
        .collect();
    groups.sort_by_key(|g| g.critical);
    groups
}

/// The groups of the inbox that the grants put an account in: `inbox.receive` on the group, or on
/// the whole service (portal 15). One group chip for each group.
fn groups_of(state: &AppState, grants: &[Grant]) -> Vec<String> {
    let Some(inbox) = state.config.inbox.as_ref() else {
        return Vec::new();
    };
    inbox
        .groups
        .keys()
        .filter(|id| {
            crate::portal_actions::group_chain(id).is_some_and(|chain| {
                grants::covers(
                    grants,
                    crate::portal_actions::INBOX_RECEIVE,
                    crate::portal_actions::SERVICE,
                    &chain,
                )
            })
        })
        .cloned()
        .collect()
}

/// The data of the page of one account. `None`: no account and not the superadmin has the login
/// `asked`. The login of the address must be the login as the server keeps it.
#[allow(clippy::too_many_arguments)]
fn user_view<'a>(
    state: &'a AppState,
    catalog: &'a Catalog,
    uri: &str,
    users_token: &'a str,
    asked: &str,
    users: Vec<(String, UserRecord)>,
    snapshot: &Snapshot,
    two_factor: Option<&std::collections::HashSet<String>>,
) -> Option<ui::UserView<'a>> {
    let mut activities = activities_of(snapshot);
    let mut found = None;
    for (login, record) in users {
        if login == asked {
            let grants = state.policy.effective(&login, &record.grants);
            found = Some(ui::UserCard {
                activity: activities.remove(&login).unwrap_or_default(),
                groups: groups_of(state, &grants),
                grants,
                configured: state.policy.configured(&login).to_vec(),
                keys: owned_keys(state, &login),
                login,
                created_at: Some(record.created_at),
                superadmin: false,
                file: false,
            });
        }
    }
    if found.is_none() {
        if let Some(account) = state.config.access.accounts.get(asked) {
            found = Some(file_card(state, asked, account, activities.remove(asked)));
        }
    }
    if found.is_none() && asked == state.superadmin_login {
        found = Some(ui::UserCard {
            login: state.superadmin_login.clone(),
            grants: Vec::new(),
            configured: Vec::new(),
            created_at: None,
            superadmin: true,
            file: false,
            keys: 0,
            groups: Vec::new(),
            activity: activities.remove(asked).unwrap_or_default(),
        });
    }
    let user = found?;
    Some(ui::UserView {
        notice: query_value(uri, "s").and_then(|code| ui::notice_for(&code)),
        mode: state.policy.mode,
        catalog,
        services: service_grants(state, catalog),
        features: features_service(state),
        owned_keys: user.keys,
        groups: group_chips(state),
        keys: key_chips(state, &user.login),
        two_factor: two_factor.map(|logins| logins.contains(&user.login)),
        user,
        token: users_token,
    })
}

/// Every account with the grants that apply to it: the accounts of the database, then those of
/// the file.
fn holders_of(state: &AppState) -> Vec<ui::Holder> {
    let mut holders: Vec<ui::Holder> = state
        .users
        .list()
        .into_iter()
        .map(|(login, record)| ui::Holder {
            grants: state.policy.effective(&login, &record.grants),
            login,
            file: false,
        })
        .collect();
    holders.extend(
        state
            .config
            .access
            .accounts
            .iter()
            .map(|(login, account)| ui::Holder {
                login: login.clone(),
                grants: account.grants.clone(),
                file: true,
            }),
    );
    holders
}

/// The portal features as the editors of permissions offer them: the service `portal` with the
/// document of the features that the file turns on. `None`: the file turns none on.
fn features_service(state: &AppState) -> Option<ui::ServiceActions> {
    let features = state.config.portal_features();
    if !features.has_keys() && !features.has_inbox() {
        return None;
    }
    Some(feature_actions(
        state,
        crate::portal_actions::document(&features),
    ))
}

/// The service `portal` for an editor, from a document of its features.
fn feature_actions(
    state: &AppState,
    document: Arc<crate::actions::Document>,
) -> ui::ServiceActions {
    let critical = state
        .config
        .inbox
        .as_ref()
        .map(|inbox| {
            inbox
                .groups
                .iter()
                .filter(|(_, group)| group.critical)
                .map(|(id, _)| id.clone())
                .collect()
        })
        .unwrap_or_default();
    ui::ServiceActions {
        id: crate::portal_actions::SERVICE.to_string(),
        name: "Server features".to_string(),
        actions: document
            .actions
            .iter()
            .map(|a| (a.name.clone(), a.description.clone()))
            .collect(),
        document,
        known: true,
        fetch: false,
        critical,
        source: crate::actions::Source::Absent,
    }
}

/// The number of keys that an account of the database owns. They go with the account.
fn owned_keys(state: &AppState, login: &str) -> usize {
    state
        .users
        .keys()
        .iter()
        .filter(|key| matches!(&key.owner, KeyOwner::User(owner) if owner == login))
        .count()
}

/// One decoded query parameter that is short. The portal ignores anything else.
fn query_value(uri: &str, key: &str) -> Option<String> {
    query_value_up_to(uri, key, 32)
}

fn query_value_up_to(uri: &str, key: &str, max: usize) -> Option<String> {
    let query = uri.split_once('?')?.1;
    query.split('&').find_map(|pair| {
        let (name, value) = pair.split_once('=').unwrap_or((pair, ""));
        (name == key)
            .then(|| percent_decode_form(value).ok())
            .flatten()
            .filter(|value| value.len() <= max)
    })
}

/// The current catalogue, for the other modules.
pub fn current_catalog_of(state: &AppState) -> Arc<Catalog> {
    current_catalog(state)
}

fn current_catalog(state: &AppState) -> Arc<Catalog> {
    Arc::clone(
        &state
            .catalog
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner()),
    )
}

/// A browser with a session. The data has the session that the cookie represents, and what that session can do.
struct Viewer {
    session: Session,
    /// The cookie value, which is a secret. The anti-CSRF tokens are bound to this value.
    token: Zeroizing<String>,
    /// The grants that apply. The value is `None` for the superadmin, who has all grants.
    grants: Option<Vec<Grant>>,
    login: String,
}

fn current_viewer(state: &AppState, headers: &HeaderMap, client: &Client) -> Option<Viewer> {
    viewer_of(state, headers, client, false)
}

/// The same viewer, and the use does not renew the idle timer of the session. The request of a
/// page that asks in the background uses this.
fn current_viewer_passive(
    state: &AppState,
    headers: &HeaderMap,
    client: &Client,
) -> Option<Viewer> {
    viewer_of(state, headers, client, true)
}

fn viewer_of(
    state: &AppState,
    headers: &HeaderMap,
    client: &Client,
    passive: bool,
) -> Option<Viewer> {
    let Some(token) = session_cookie(state, headers, client) else {
        state.metrics.session_missing();
        return None;
    };
    let session = if passive {
        state.sessions.peek(token, &client.binding)
    } else {
        state.sessions.validate(token, &client.binding)
    };
    let Some(session) = session else {
        state.metrics.session_invalid();
        return None;
    };
    // The session ends when its credential is not the credential of now. Examples are a new password, a removed
    // password and another account of the login. A session that a sign-in issued after the revocation of the account
    // ends here too.
    match credential_current(state, &session.principal, &session.credential) {
        Fresh::Current => {}
        Fresh::Stale => {
            state.metrics.session_invalid();
            state.sessions.revoke(token, EndReason::Revoked);
            return None;
        }
        // The database did not answer. The session stays: the next request asks again.
        Fresh::Unknown => return None,
    }
    state.metrics.session_valid();
    let (grants, login) = match &session.principal {
        Principal::Superadmin => (None, state.superadmin_login.clone()),
        Principal::User(login) => match grants_of_known_user(state, login) {
            Known::Grants(grants) => (Some(grants), login.clone()),
            Known::Deleted => {
                // The account was deleted while the session was open.
                state.sessions.revoke(token, EndReason::Revoked);
                return None;
            }
            // The database did not answer. The session stays: the next request asks again.
            Known::Unknown => return None,
        },
    };
    Some(Viewer {
        session,
        token: Zeroizing::new(token.to_string()),
        grants,
        login,
    })
}

/// What the portal knows about the account of a session.
enum Known {
    Grants(Vec<Grant>),
    /// The database has no such account.
    Deleted,
    /// The database did not answer: the portal cannot tell.
    Unknown,
}

/// The stored grants of an account. When the directory does not know the account, the portal reads the grants again first from the shared database.
/// The account can have been created on another instance a moment ago, and its session can have been opened there.
/// A session must not end for this reason. The read has no minimum gap: the gap of the unknown logins would end the session of a new account when another request read the accounts less than a second before.
/// A request needs a valid session for this read. It is not a way to make the database work with unknown logins.
fn grants_of_known_user(state: &AppState, login: &str) -> Known {
    let grants = |state: &AppState| keyauth::person_grants(state, login).map(|(grants, _)| grants);
    if let Some(grants) = grants(state) {
        return Known::Grants(grants);
    }
    if !state.store.is_shared() {
        return Known::Deleted;
    }
    let since = crate::users::Directory::clock_millis();
    if !blocking(|| state.users.refresh_since(&state.store, since)) {
        return Known::Unknown;
    }
    grants(state).map_or(Known::Deleted, Known::Grants)
}

/// A rate-limited read of the accounts from the shared database, for a request with a login that the directory does not hold.
/// The result is always `false` with one instance, which is the only writer.
fn refresh_directory_if_due(state: &AppState) -> bool {
    state.store.is_shared()
        && blocking(|| {
            state
                .users
                .refresh_if_due(&state.store, UNKNOWN_LOGIN_REFRESH_GAP)
        })
}

// The binding of the sign-in proofs and of the sessions to the credential ----------------------------------------

/// Where the password of an account comes from. The stamp names it: an account of the file and an account of the
/// database never have the same stamp.
#[derive(Clone, Copy)]
enum Source {
    Database,
    File,
}

/// The stamp of the password `hash` of the account `login` (see [`Credential`]). The stamp is a SHA-256 digest, not a
/// MAC: a new session secret does not end the sessions of the accounts. The digest tells nothing that helps a guess
/// of the password: the hash holds a random salt, and the digest does not give the salt.
fn account_stamp(source: Source, login: &str, hash: &str) -> Credential {
    use sha2::Digest;
    let source = match source {
        Source::Database => "db",
        Source::File => "file",
    };
    // A login has no line end: the line ends separate the fields.
    sha2::Sha256::digest(format!("credential-stamp\n{source}\n{login}\n{hash}").as_bytes()).into()
}

/// The stamp of the credential of `principal` now. For the superadmin, it is the credential generation. For an account
/// of the database or of the file, it is the stamp of the password hash. `None`: the principal has no password now.
/// The login of the superadmin names no account, so `Principal::User` with that login has no stamp either.
///
/// Each sign-in proof and each session carries the stamp of the credential that the proof checked. The proofs are the
/// password step and the pending token of the second step. A proof opens a session, and a session acts, only while
/// the stamp is the stamp of now (see [`credential_current`]). Thus a new password refuses every proof and every
/// session made before it. A removed password, a new superadmin credential and a new account of the login do the
/// same. The order of the revocation and of the sign-in does not change this result.
fn credential_stamp(state: &AppState, principal: &Principal) -> Option<Credential> {
    match principal {
        Principal::Superadmin => Some(state.superadmin_generation),
        Principal::User(login) if *login == state.superadmin_login => None,
        Principal::User(login) => match state.users.password_hash(login) {
            Some(hash) => Some(account_stamp(Source::Database, login, &hash)),
            None => {
                let hash = state
                    .config
                    .access
                    .accounts
                    .get(login)?
                    .password_hash
                    .as_deref()?;
                Some(account_stamp(Source::File, login, hash))
            }
        },
    }
}

/// What a check of a stamp found.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Fresh {
    /// The stamp is the stamp of now.
    Current,
    /// The credential changed, or the principal has no password now.
    Stale,
    /// The shared database did not answer: the portal cannot tell.
    Unknown,
}

/// Whether `credential` is the stamp of the credential of `principal` now. This is the one check of the binding of
/// a proof or of a session to the credential. With a shared database, the directory of this instance can lack a
/// change of another instance. The directory is then read again before the answer `Stale`.
fn credential_current(state: &AppState, principal: &Principal, credential: &Credential) -> Fresh {
    fresh_against_directory(state, |state| {
        credential_stamp(state, principal).is_some_and(|now| constant_time_eq(&now, credential))
    })
}

/// `Current` when `holds` is true for the directory of this instance. Otherwise, with a shared database, the
/// directory is read again and `holds` decides.
fn fresh_against_directory(state: &AppState, mut holds: impl FnMut(&AppState) -> bool) -> Fresh {
    if holds(state) {
        return Fresh::Current;
    }
    if !state.store.is_shared() {
        return Fresh::Stale;
    }
    let since = crate::users::Directory::clock_millis();
    if !blocking(|| state.users.refresh_since(&state.store, since)) {
        return Fresh::Unknown;
    }
    if holds(state) {
        Fresh::Current
    } else {
        Fresh::Stale
    }
}

/// The time in milliseconds since the Unix epoch.
fn unix_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn ip_text(client: &Client) -> Option<String> {
    client.ip.map(|ip| ip.to_string())
}

/// The User-Agent of the browser, for the history. The value has a length limit and no control characters.
fn user_agent_text(headers: &HeaderMap) -> String {
    headers
        .get(USER_AGENT)
        .and_then(|value| value.to_str().ok())
        .map(|value| {
            value
                .chars()
                .filter(|c| !c.is_control())
                .take(160)
                .collect()
        })
        .unwrap_or_default()
}

fn handle_login_get(
    state: &AppState,
    uri: &str,
    headers: &HeaderMap,
    client: &Client,
) -> HttpResponse {
    let (rd, sig) = match query_return_pair(uri) {
        Ok(v) => v,
        Err(_) => return text_response(StatusCode::BAD_REQUEST, "Bad Request\n"),
    };
    let rd = rd.unwrap_or_else(|| HOME_PATH.to_string());
    let sig = sig.unwrap_or_default();
    let return_is_valid = validate_return_uri(&rd, state.config.security.max_forwarded_uri_bytes)
        && verify_return_uri_signature(&state.signing_secret, &rd, &sig);
    let rd = if return_is_valid {
        rd
    } else {
        HOME_PATH.to_string()
    };
    let sig = if return_is_valid {
        sig
    } else {
        match sign_return_uri(&state.signing_secret, HOME_PATH) {
            Ok(v) => v,
            Err(_) => {
                return text_response(StatusCode::INTERNAL_SERVER_ERROR, "authentication error\n")
            }
        }
    };

    if has_valid_session(state, headers, client) {
        return redirect_response(StatusCode::SEE_OTHER, &rd);
    }

    // A browser that already holds a token keeps it.
    // Another request for the login page can come from a second tab, or from a favicon fetch that Traefik sends through ForwardAuth. The redirect of that fetch lands here.
    // Such a request must not replace the cookie under the open form. If it does, the submit fails the CSRF check.
    let csrf = match existing_csrf_token(state, headers, client) {
        Some(v) => v,
        None => match new_csrf_token() {
            Ok(v) => v,
            Err(_) => {
                return text_response(StatusCode::INTERNAL_SERVER_ERROR, "authentication error\n")
            }
        },
    };
    login_page(state, client, &rd, &sig, &csrf, None, StatusCode::OK)
}

/// The anti-CSRF token that the browser already holds. The token must be one that this portal issued.
/// It has the shape of [`new_csrf_token`]: 32 URL-safe base64 characters.
fn existing_csrf_token(state: &AppState, headers: &HeaderMap, client: &Client) -> Option<String> {
    cookie_value(
        headers,
        transport::csrf_cookie_name(client.https),
        state.config.security.max_cookie_header_bytes,
    )
    .and_then(csrf_shape)
}

fn csrf_shape(value: &str) -> Option<String> {
    (value.len() == 32
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_'))
    .then(|| value.to_string())
}

fn handle_success(state: &AppState, headers: &HeaderMap, client: &Client) -> HttpResponse {
    if !has_valid_session(state, headers, client) {
        return redirect_response(StatusCode::SEE_OTHER, LOGIN_PATH);
    }
    masked_response(
        state,
        StatusCode::OK,
        ui::auth_document(
            "Authenticated",
            r#"<h1>Authenticated</h1><p>The password was accepted and the session cookie is valid.</p><a class="button" href="/">Continue</a>"#,
        ),
    )
}

/// The sign-in form after the checks that every post of the sign-in needs: the same origin, a form, a limited number of
/// bodies read at the same time, a limited body, and fields that parse. The sign-in and the second step share them.
struct LoginForm {
    headers: HeaderMap,
    /// The anti-CSRF cookie of the browser.
    csrf_cookie: Option<String>,
    form: std::collections::HashMap<String, String>,
    /// The permit of `server.max_login_body_reads`. The sign-in keeps it until it has the Argon2
    /// permit. Thus the sign-ins that read a body or wait for a check hold at most this number of
    /// connections, and ForwardAuth keeps the others.
    body_permit: tokio::sync::OwnedSemaphorePermit,
    /// `server.body_read_timeout` after the start of the body read. The wait for the Argon2 permit
    /// ends at this time too, so a sign-in holds its connection for a bounded time.
    deadline: tokio::time::Instant,
}

/// `503` with `Retry-After: 1`: the server has no room for this sign-in now.
fn sign_in_busy() -> HttpResponse {
    let mut response = text_response(StatusCode::SERVICE_UNAVAILABLE, "Service Unavailable\n");
    if set_dynamic_header(&mut response, RETRY_AFTER, "1").is_err() {
        return text_response(StatusCode::INTERNAL_SERVER_ERROR, "authentication error\n");
    }
    response
}

#[allow(clippy::result_large_err)] // the error is the response, sent as is
async fn login_form(
    state: &Arc<AppState>,
    request: Request<Incoming>,
    client: &Client,
) -> Result<LoginForm, HttpResponse> {
    let client_ip = client.ip;
    if !validate_post_origin(request.headers()) {
        state.metrics.login_origin_failure();
        log(
            state,
            LogLevel::Warn,
            "origin_check_failed",
            &[
                ("route", "login".to_string()),
                (
                    "client_ip",
                    client_ip
                        .map(|v| v.to_string())
                        .unwrap_or_else(|| "unknown".into()),
                ),
            ],
        );
        return Err(text_response(
            StatusCode::FORBIDDEN,
            "Origin validation failed\n",
        ));
    }
    if !content_type_is_form(request.headers()) {
        state.metrics.login_invalid_request();
        return Err(text_response(
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "Unsupported Media Type\n",
        ));
    }

    let csrf_cookie = cookie_value(
        request.headers(),
        transport::csrf_cookie_name(client.https),
        state.config.security.max_cookie_header_bytes,
    )
    .map(str::to_string);
    let headers = request.headers().clone();

    // An anonymous client can send a sign-in body slowly. Each slow body holds a connection for up
    // to `server.body_read_timeout`. A bounded number of bodies is read at once, so these requests
    // never hold every connection that ForwardAuth needs. The others are refused at once.
    let Ok(body_permit) = Arc::clone(&state.login_body_gate).try_acquire_owned() else {
        state.metrics.login_body_rejected();
        log(
            state,
            LogLevel::Warn,
            "login_body_capacity_reached",
            &[
                (
                    "client_ip",
                    client_ip
                        .map(|v| v.to_string())
                        .unwrap_or_else(|| "unknown".into()),
                ),
                (
                    "max_login_body_reads",
                    state.config.server.max_login_body_reads.to_string(),
                ),
            ],
        );
        return Err(sign_in_busy());
    };
    let deadline = tokio::time::Instant::now() + state.config.server.body_read_timeout;
    let body = read_limited_body(
        request.into_body(),
        state.config.server.max_body_bytes,
        state.config.server.body_read_timeout,
    )
    .await;
    let body = match body {
        Ok(v) => Zeroizing::new(v),
        Err(BodyReadError::TooLarge) => {
            state.metrics.login_invalid_request();
            return Err(text_response(
                StatusCode::PAYLOAD_TOO_LARGE,
                "Payload Too Large\n",
            ));
        }
        Err(BodyReadError::Timeout) => {
            state.metrics.login_invalid_request();
            return Err(text_response(
                StatusCode::REQUEST_TIMEOUT,
                "Request Timeout\n",
            ));
        }
        Err(BodyReadError::Io) => {
            state.metrics.login_invalid_request();
            return Err(text_response(StatusCode::BAD_REQUEST, "Bad Request\n"));
        }
    };

    let form = match parse_form(&body) {
        Ok(v) => v,
        Err(_) => {
            state.metrics.login_invalid_request();
            return Err(text_response(StatusCode::BAD_REQUEST, "Bad Request\n"));
        }
    };
    Ok(LoginForm {
        headers,
        csrf_cookie,
        form,
        body_permit,
        deadline,
    })
}

async fn handle_login_post(
    state: &Arc<AppState>,
    request: Request<Incoming>,
    client: &Client,
) -> HttpResponse {
    let client_ip = client.ip;
    let LoginForm {
        headers,
        csrf_cookie,
        mut form,
        body_permit,
        deadline,
    } = match login_form(state, request, client).await {
        Ok(v) => v,
        Err(response) => return response,
    };

    let login_input = form.remove("login").unwrap_or_default();
    let password = Zeroizing::new(form.remove("password").unwrap_or_default());
    let csrf = form.remove("csrf").unwrap_or_default();
    let rd = form.remove("rd").unwrap_or_else(|| "/".to_string());
    let sig = form.remove("sig").unwrap_or_default();
    let form_token = form.remove(FORM_TOKEN_FIELD).unwrap_or_default();
    // A person never sees this field. A script that fills every field of the form fills it.
    let trap_filled = form.remove(TRAP_FIELD).is_some_and(|v| !v.is_empty());
    // The box "Remember me". It is read here, with the other fields, so that it never makes
    // the form unknown. Only the value of a ticked box counts. With `session.remember = false`, the
    // server reads the field and ignores it.
    let keep = form.remove(KEEP_FIELD).as_deref() == Some(KEEP_VALUE);

    if !form.is_empty()
        || password.len() > state.config.security.max_password_bytes
        || !validate_return_uri(&rd, state.config.security.max_forwarded_uri_bytes)
        || !verify_return_uri_signature(&state.signing_secret, &rd, &sig)
    {
        state.metrics.login_invalid_request();
        return text_response(StatusCode::BAD_REQUEST, "Bad Request\n");
    }

    let stale_page = |state: &AppState| {
        // The cookie of the token expired under an open page, or the browser dropped it. The
        // answer is the sign-in page again, with a new token and a plain sentence.
        state.metrics.login_csrf_failure();
        let Ok(fresh) = new_csrf_token() else {
            return text_response(StatusCode::INTERNAL_SERVER_ERROR, "authentication error\n");
        };
        login_page(
            state,
            client,
            &rd,
            &sig,
            &fresh,
            Some("The page was open too long. Enter your login and password again."),
            StatusCode::FORBIDDEN,
        )
    };
    if !verify_csrf(csrf_cookie.as_deref().unwrap_or(""), &csrf) {
        return stale_page(state);
    }
    // The anti-bot check of the form. A form that is too old is a page that stayed open: it gets
    // the answer of an expired anti-CSRF token. Any other failure of the check (no token, a
    // damaged token, a submit that is too fast, a filled trap) must look exactly like a wrong
    // password: it goes the whole way of a sign-in, Argon2 check and limiters included, and it
    // is refused at the end. See `docs/login-hardening.md`.
    let form_state = check_form_token(&state.signing_secret, &csrf, &form_token, unix_millis());
    if form_state == FormToken::Expired {
        return stale_page(state);
    }
    let form_ok = form_state == FormToken::Valid && !trap_filled;

    let client_ip_text = client_ip
        .map(|v| v.to_string())
        .unwrap_or_else(|| "unknown".into());
    let ip_key = format!("ip:{client_ip_text}");
    // The global limiter has a separate budget for the superadmin login.
    let global_key = if names_superadmin(state, &login_input) {
        SUPERADMIN_RATE_LIMIT_KEY
    } else {
        GLOBAL_RATE_LIMIT_KEY
    };

    if let Some(response) = login_rate_limit_response(
        state,
        client,
        &ip_key,
        global_key,
        &client_ip_text,
        &rd,
        &sig,
    ) {
        return response;
    }

    // The wait for the Argon2 permit keeps the body permit and ends at the deadline of the body.
    // Thus waiting sign-ins hold at most `server.max_login_body_reads` connections, each for a
    // bounded time. The answer at the deadline tells nothing about the login: no check ran.
    let gate = Arc::clone(&state.password_gate).acquire_owned();
    let permit = match tokio::time::timeout_at(deadline, gate).await {
        Ok(Ok(v)) => v,
        Ok(Err(_)) => {
            state.metrics.login_internal_error();
            return text_response(StatusCode::INTERNAL_SERVER_ERROR, "authentication error\n");
        }
        Err(_) => {
            log(
                state,
                LogLevel::Warn,
                "login_password_check_wait_timeout",
                &[("client_ip", client_ip_text.clone())],
            );
            return sign_in_busy();
        }
    };
    drop(body_permit);

    // Requests can wait in a queue behind the Argon2 semaphore. Check again after the request gets the permit.
    // This check stops a burst that the portal admitted before the first failures were recorded. Such a burst cannot drain the expensive password-check budget afterwards.
    if let Some(response) = login_rate_limit_response(
        state,
        client,
        &ip_key,
        global_key,
        &client_ip_text,
        &rd,
        &sig,
    ) {
        drop(permit);
        return response;
    }

    // Exactly one Argon2 check runs, whatever account the login names.
    // The check uses the superadmin hash, the account hash, or a dummy hash when the login does not exist. Time and budget tell nothing about the login.
    // The portal reads the shared accounts here, for every login. If it read them only for an unknown login, the time would tell.
    refresh_directory_if_due(state);
    let who = identify(state, &login_input);
    // The portal keeps this information about the attempt: the login as typed, made safe, and whether it names an account.
    // The password is not kept.
    let typed = typed_login(&login_input);
    let known = !matches!(who, Who::Unknown);
    let target_hash = match &who {
        Who::Superadmin => state.superadmin_hash.clone(),
        Who::User(_, record, _) => record.password_hash.clone(),
        Who::Unknown => state.dummy_hash.clone(),
    };
    let authenticated = {
        let _metric_guard = state.metrics.password_check_started();
        match tokio::task::spawn_blocking(move || {
            let _permit = permit;
            verify_password(password.as_bytes(), &target_hash)
        })
        .await
        {
            Ok(v) => v && form_ok && !matches!(who, Who::Unknown),
            Err(_) => {
                state.metrics.login_internal_error();
                return text_response(StatusCode::INTERNAL_SERVER_ERROR, "authentication error\n");
            }
        }
    };

    if !authenticated {
        if state.config.security.login_rate_limit_enabled {
            state
                .ip_limiter
                .failure(ip_key.clone(), state.config.security.login_attempts_per_ip);
        }
        if state.config.security.global_login_rate_limit_enabled {
            state.global_limiter.failure(
                global_key.to_string(),
                state.config.security.global_login_attempts,
            );
        }
        state.metrics.login_failure();
        if !form_ok {
            state.metrics.login_form_check_failure();
        }
        // A person can type the password in the login field. The text is recorded only when it names an account.
        let recorded = if known {
            typed
        } else {
            String::from("(unknown login)")
        };
        log(
            state,
            LogLevel::Warn,
            "login_failed",
            &[
                ("client_ip", client_ip_text),
                ("login", recorded.clone()),
                ("known", known.to_string()),
                (
                    "reason",
                    if form_ok { "credentials" } else { "form_check" }.to_string(),
                ),
            ],
        );
        state.tracker.emit(Event::Login(LoginAttempt {
            login: recorded,
            known,
            success: false,
            ip: ip_text(client),
            user_agent: user_agent_text(&headers),
            at: unix_now(),
        }));
        // The audit event names an account only when the typed login names an account. Therefore, the per-account
        // counters never count a typo or a probe.
        state.tracker.emit(Event::Audit(AuditEvent {
            kind: AuditKind::LoginFailed,
            login: who.login(state),
            actor: None,
            ip: ip_text(client),
            user_agent: user_agent_text(&headers),
            at: unix_now(),
        }));
        let csrf = match new_csrf_token() {
            Ok(v) => v,
            Err(_) => {
                return text_response(StatusCode::INTERNAL_SERVER_ERROR, "authentication error\n")
            }
        };
        return login_page(
            state,
            client,
            &rd,
            &sig,
            &csrf,
            Some(ui::signin::INVALID_CREDENTIALS),
            StatusCode::UNAUTHORIZED,
        );
    }

    let (principal, session_login) = match &who {
        Who::Superadmin => (Principal::Superadmin, state.superadmin_login.clone()),
        Who::User(login, _, _) => (Principal::User(login.clone()), login.clone()),
        Who::Unknown => unreachable!("an unknown login never authenticates"),
    };
    // The proof of the password holds for the copy of the credential that the check used. A new password that
    // arrived during the check makes the proof stale (see `finish_sign_in`).
    let Some(credential) = who.credential(state) else {
        unreachable!("an unknown login never authenticates")
    };
    // A good password clears no failure of the address: the failures of the address can name other logins. They
    // expire with `security.login_window`. With the second factor on, the password opens no session: the person
    // goes on to the second step.
    if let Some(response) = mfa_page::after_password(
        state,
        client,
        &principal,
        &credential,
        keep,
        &csrf,
        &rd,
        &sig,
    )
    .await
    {
        return response;
    }
    finish_sign_in(
        state,
        client,
        &headers,
        &rd,
        SignedIn {
            principal,
            credential,
            session_login,
            typed,
            known,
            keep,
            mfa: false,
        },
        Landing::Redirect,
    )
}

/// The account that has given all the proofs that the configuration asks, and what the session records about it.
struct SignedIn {
    principal: Principal,
    /// The stamp of the credential that the proofs checked.
    credential: Credential,
    /// The login that the history of the sessions shows.
    session_login: String,
    /// The login as typed, made safe, for the log and the sign-in list.
    typed: String,
    known: bool,
    /// The person ticked "Remember me".
    keep: bool,
    /// The sign-in used the second factor.
    mfa: bool,
}

/// What the person sees when the session starts.
enum Landing {
    /// The page that the person asked for.
    Redirect,
    /// The recovery codes that the forced enrolment made, shown once, with a link to the page that the person asked for.
    Codes(Vec<String>),
}

/// Opens the session and sets its cookie. The password step, and the second step when the account has one, call this.
fn finish_sign_in(
    state: &Arc<AppState>,
    client: &Client,
    headers: &HeaderMap,
    rd: &str,
    signed: SignedIn,
    landing: Landing,
) -> HttpResponse {
    let SignedIn {
        principal,
        credential,
        session_login,
        typed,
        known,
        keep,
        mfa,
    } = signed;
    let client_ip_text = client
        .ip
        .map(|v| v.to_string())
        .unwrap_or_else(|| "unknown".into());
    // The proofs must hold for the credential of now. A new password, a removed password, a new superadmin
    // credential or a new account of the login that arrived after the proof refuses it. A change after this check
    // gives a session that ends at its first use (see `viewer_of`).
    match credential_current(state, &principal, &credential) {
        Fresh::Current => {}
        Fresh::Stale => {
            state.metrics.login_failure();
            log(
                state,
                LogLevel::Warn,
                "login_failed",
                &[
                    ("client_ip", client_ip_text),
                    ("login", typed),
                    ("known", known.to_string()),
                    ("reason", "stale_credential".to_string()),
                ],
            );
            let (Ok(fresh), Ok(sig)) =
                (new_csrf_token(), sign_return_uri(&state.signing_secret, rd))
            else {
                return text_response(StatusCode::INTERNAL_SERVER_ERROR, "authentication error\n");
            };
            return login_page(
                state,
                client,
                rd,
                &sig,
                &fresh,
                Some("The page was open too long. Enter your login and password again."),
                StatusCode::FORBIDDEN,
            );
        }
        Fresh::Unknown => {
            state.metrics.login_internal_error();
            return text_response(
                StatusCode::SERVICE_UNAVAILABLE,
                "session store unavailable\n",
            );
        }
    }
    let token = Zeroizing::new(
        match state.sessions.issue(
            client.binding,
            NewSession {
                principal,
                credential,
                login: session_login,
                impersonated: false,
                remember: keep,
                mfa,
                ip: ip_text(client),
                user_agent: user_agent_text(headers),
                // A sign-in with the password gets the full `session.ttl`, or the full
                // `session.remember_ttl` for a remembered session.
                not_after: None,
            },
        ) {
            Ok(v) => v,
            Err(SessionIssueError::Capacity) => {
                state.metrics.session_capacity_rejected();
                state.metrics.login_internal_error();
                log(
                    state,
                    LogLevel::Error,
                    "session_capacity_reached",
                    &[("capacity", state.sessions.capacity().to_string())],
                );
                return text_response(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "session capacity reached\n",
                );
            }
            Err(SessionIssueError::Random) => {
                state.metrics.login_internal_error();
                return text_response(StatusCode::INTERNAL_SERVER_ERROR, "authentication error\n");
            }
            Err(SessionIssueError::Storage) => {
                state.metrics.login_internal_error();
                return text_response(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "session store unavailable\n",
                );
            }
        },
    );

    let mut response = match landing {
        Landing::Redirect => redirect_response(StatusCode::SEE_OTHER, rd),
        Landing::Codes(codes) => {
            masked_response(state, StatusCode::OK, ui::mfa::codes_card(&codes, rd))
        }
    };
    // A remembered session has a persistent cookie with the life of the session. Any other session
    // has a cookie that the browser deletes at the end of its session.
    let cookie_life = match state.config.session.remember {
        Some(remember) if keep => Some(remember.ttl.as_secs()),
        _ => None,
    };
    let cookie = session_cookie_header(state, client, token.as_str(), cookie_life);
    if append_dynamic_header(&mut response, SET_COOKIE, cookie.as_str()).is_err()
        || append_dynamic_header(
            &mut response,
            SET_COOKIE,
            &expired_csrf_cookie(state, client.https),
        )
        .is_err()
    {
        state.sessions.revoke(token.as_str(), EndReason::Revoked);
        state.metrics.login_internal_error();
        return text_response(StatusCode::INTERNAL_SERVER_ERROR, "authentication error\n");
    }

    state.metrics.login_success();
    state.metrics.session_issued();
    state.tracker.emit(Event::Login(LoginAttempt {
        login: typed.clone(),
        known,
        success: true,
        ip: ip_text(client),
        user_agent: user_agent_text(headers),
        at: unix_now(),
    }));
    log(
        state,
        LogLevel::Info,
        "login_success",
        &[
            ("client_ip", client_ip_text),
            ("login", typed),
            ("second_factor", mfa.to_string()),
        ],
    );
    response
}

/// The account that a login names.
enum Who {
    Superadmin,
    /// An account of the database or of the file. The record is a copy, and the stamp is the stamp of the password
    /// of this copy (see [`credential_stamp`]).
    User(String, UserRecord, Credential),
    Unknown,
}

impl Who {
    /// The account that a failed attempt is recorded against. The value is none when the login does not exist.
    fn login(&self, state: &AppState) -> Option<String> {
        match self {
            Self::Superadmin => Some(state.superadmin_login.clone()),
            Self::User(login, _, _) => Some(login.clone()),
            Self::Unknown => None,
        }
    }

    /// The stamp of the credential that a check of the password of this copy proves.
    fn credential(&self, state: &AppState) -> Option<Credential> {
        match self {
            Self::Superadmin => Some(state.superadmin_generation),
            Self::User(_, _, credential) => Some(*credential),
            Self::Unknown => None,
        }
    }
}

/// Whether the typed login names the superadmin. The comparison is the one of [`identify`].
fn names_superadmin(state: &AppState, typed: &str) -> bool {
    let login = typed.trim().to_ascii_lowercase();
    login.len() <= 128 && constant_time_eq(login.as_bytes(), state.superadmin_login.as_bytes())
}

fn identify(state: &AppState, typed: &str) -> Who {
    // Convert the login to lower case and trim it, as the logins are stored. A name far longer than any login is unknown.
    let login = typed.trim().to_ascii_lowercase();
    if login.len() > 128 {
        return Who::Unknown;
    }
    if constant_time_eq(login.as_bytes(), state.superadmin_login.as_bytes()) {
        return Who::Superadmin;
    }
    // The caller reads the accounts of the shared database before, for every login: see `handle_login_post`.
    let found = match state.users.get(&login) {
        Some(record) => Some((record, Source::Database)),
        // An account of the file has a password only when its block gives the digest (portal 3.4b).
        None => state
            .config
            .access
            .accounts
            .get(&login)
            .and_then(|account| account.password_hash.clone())
            .map(|digest| {
                (
                    UserRecord {
                        password_hash: digest,
                        grants: Vec::new(),
                        created_at: 0,
                    },
                    Source::File,
                )
            }),
    };
    let Some((record, source)) = found else {
        return Who::Unknown;
    };
    let credential = account_stamp(source, &login, &record.password_hash);
    Who::User(login, record, credential)
}

/// The `Set-Cookie` value of a session. `max_age_secs` is the time left before the session ends.
/// `None` makes a cookie without `Max-Age`.
fn session_cookie_header(
    state: &AppState,
    client: &Client,
    token: &str,
    max_age_secs: Option<u64>,
) -> Zeroizing<String> {
    Zeroizing::new(format!(
        "{}={}; {}",
        transport::session_cookie_name(&state.config.session.cookie_name, client.https),
        token,
        transport::cookie_attributes(
            client.https,
            state.config.session.same_site.as_str(),
            max_age_secs,
        ),
    ))
}

/// The refusal of a sign-in that a limiter blocks. `global_key` selects the budget of the global
/// limiter: the superadmin login has its own budget.
#[allow(clippy::too_many_arguments)]
fn login_rate_limit_response(
    state: &AppState,
    client: &Client,
    ip_key: &str,
    global_key: &str,
    client_ip_text: &str,
    rd: &str,
    sig: &str,
) -> Option<HttpResponse> {
    if state.config.security.login_rate_limit_enabled {
        if let Err(wait) = state
            .ip_limiter
            .check(ip_key, state.config.security.login_attempts_per_ip)
        {
            state.metrics.login_rate_limited_ip();
            log(
                state,
                LogLevel::Warn,
                "login_rate_limited",
                &[
                    ("scope", "ip".to_string()),
                    ("client_ip", client_ip_text.to_string()),
                    ("retry_after_seconds", wait.as_secs().max(1).to_string()),
                ],
            );
            return Some(rate_limited_login_page(
                state,
                client,
                rd,
                sig,
                "Too many sign-in attempts from this address. Try again later.",
                wait.as_secs().max(1),
            ));
        }
    }

    if state.config.security.global_login_rate_limit_enabled {
        if let Err(wait) = state
            .global_limiter
            .check(global_key, state.config.security.global_login_attempts)
        {
            state.metrics.login_rate_limited_global();
            log(
                state,
                LogLevel::Warn,
                "login_rate_limited",
                &[
                    ("scope", global_key.to_string()),
                    ("client_ip", client_ip_text.to_string()),
                    ("retry_after_seconds", wait.as_secs().max(1).to_string()),
                ],
            );
            return Some(rate_limited_login_page(
                state,
                client,
                rd,
                sig,
                "Authentication is temporarily rate limited. Try again shortly.",
                wait.as_secs().max(1),
            ));
        }
    }

    None
}

fn rate_limited_login_page(
    state: &AppState,
    client: &Client,
    rd: &str,
    sig: &str,
    message: &str,
    retry_after: u64,
) -> HttpResponse {
    let csrf = match new_csrf_token() {
        Ok(v) => v,
        Err(_) => {
            return text_response(StatusCode::INTERNAL_SERVER_ERROR, "authentication error\n")
        }
    };
    let mut response = login_page(
        state,
        client,
        rd,
        sig,
        &csrf,
        Some(message),
        StatusCode::TOO_MANY_REQUESTS,
    );
    if set_dynamic_header(&mut response, RETRY_AFTER, &retry_after.to_string()).is_err() {
        return text_response(StatusCode::INTERNAL_SERVER_ERROR, "authentication error\n");
    }
    response
}

async fn handle_logout_post(
    state: &Arc<AppState>,
    request: Request<Incoming>,
    client: &Client,
) -> HttpResponse {
    if !validate_post_origin(request.headers()) {
        state.metrics.logout_origin_failure();
        log(
            state,
            LogLevel::Warn,
            "origin_check_failed",
            &[("route", "logout".to_string())],
        );
        return text_response(StatusCode::FORBIDDEN, "Origin validation failed\n");
    }
    if !content_type_is_form(request.headers()) {
        return text_response(
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "Unsupported Media Type\n",
        );
    }

    let session_token = match cookie_value(
        request.headers(),
        transport::session_cookie_name(&state.config.session.cookie_name, client.https),
        state.config.security.max_cookie_header_bytes,
    ) {
        Some(v) if state.sessions.validate(v, &client.binding).is_some() => {
            state.metrics.session_valid();
            Zeroizing::new(v.to_string())
        }
        Some(_) => {
            state.metrics.session_invalid();
            return redirect_response(StatusCode::SEE_OTHER, LOGIN_PATH);
        }
        None => {
            state.metrics.session_missing();
            return redirect_response(StatusCode::SEE_OTHER, LOGIN_PATH);
        }
    };

    let body = match read_limited_body(
        request.into_body(),
        state.config.server.max_body_bytes,
        state.config.server.body_read_timeout,
    )
    .await
    {
        Ok(v) => Zeroizing::new(v),
        Err(BodyReadError::TooLarge) => {
            return text_response(StatusCode::PAYLOAD_TOO_LARGE, "Payload Too Large\n")
        }
        Err(BodyReadError::Timeout) => {
            return text_response(StatusCode::REQUEST_TIMEOUT, "Request Timeout\n")
        }
        Err(BodyReadError::Io) => return text_response(StatusCode::BAD_REQUEST, "Bad Request\n"),
    };

    let mut form = match parse_form(&body) {
        Ok(v) => v,
        Err(_) => return text_response(StatusCode::BAD_REQUEST, "Bad Request\n"),
    };
    let csrf = form.remove("csrf").unwrap_or_default();
    if !form.is_empty() {
        return text_response(StatusCode::BAD_REQUEST, "Bad Request\n");
    }
    if !verify_action_token(
        &state.signing_secret,
        LOGOUT_TOKEN,
        session_token.as_str(),
        &csrf,
    ) {
        return text_response(StatusCode::FORBIDDEN, "CSRF validation failed\n");
    }

    state
        .sessions
        .revoke(session_token.as_str(), EndReason::Logout);
    let response = signed_out_response(state, client, "/");
    if response.status() == StatusCode::SEE_OTHER {
        state.metrics.logout();
    }
    response
}

// Account administration ----------------------------------------------------------------------

/// Every administration POST starts with these checks: the same origin, a form, a browser with a session, a limited
/// body, and a valid anti-CSRF token. The function returns the viewer and the remaining form fields, or the answer.
#[allow(clippy::result_large_err)] // the error is the response, sent as is
async fn admin_request(
    state: &Arc<AppState>,
    request: Request<Incoming>,
    client: &Client,
    action: &'static str,
) -> Result<(Viewer, std::collections::HashMap<String, String>), HttpResponse> {
    form_request(state, request, client, action, USERS_TOKEN).await
}

/// The same checks for a form whose anti-CSRF token has the purpose `purpose`.
#[allow(clippy::result_large_err)] // the error is the response, sent as is
async fn form_request(
    state: &Arc<AppState>,
    request: Request<Incoming>,
    client: &Client,
    action: &'static str,
    purpose: &'static str,
) -> Result<(Viewer, std::collections::HashMap<String, String>), HttpResponse> {
    if !validate_post_origin(request.headers()) {
        state.metrics.users_origin_failure();
        log(
            state,
            LogLevel::Warn,
            "origin_check_failed",
            &[("route", "users".to_string())],
        );
        return Err(text_response(
            StatusCode::FORBIDDEN,
            "Origin validation failed\n",
        ));
    }
    if !content_type_is_form(request.headers()) {
        return Err(text_response(
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "Unsupported Media Type\n",
        ));
    }
    // The session first: the portal reads no body for a request that has no session.
    let headers = request.headers().clone();
    let Some(first) = current_viewer(state, &headers, client) else {
        return Err(redirect_response(StatusCode::SEE_OTHER, LOGIN_PATH));
    };
    let body = match read_limited_body(
        request.into_body(),
        state.config.server.max_body_bytes,
        state.config.server.body_read_timeout,
    )
    .await
    {
        Ok(v) => Zeroizing::new(v),
        Err(BodyReadError::TooLarge) => {
            return Err(text_response(
                StatusCode::PAYLOAD_TOO_LARGE,
                "Payload Too Large\n",
            ))
        }
        Err(BodyReadError::Timeout) => {
            return Err(text_response(
                StatusCode::REQUEST_TIMEOUT,
                "Request Timeout\n",
            ))
        }
        Err(BodyReadError::Io) => {
            return Err(text_response(StatusCode::BAD_REQUEST, "Bad Request\n"))
        }
    };
    let Some(viewer) = still_current(state, &headers, client, &first) else {
        return Err(redirect_response(StatusCode::SEE_OTHER, LOGIN_PATH));
    };
    let mut form = match parse_form(&body) {
        Ok(v) => v,
        Err(_) => return Err(text_response(StatusCode::BAD_REQUEST, "Bad Request\n")),
    };
    let csrf = form.remove("csrf").unwrap_or_default();
    if !verify_action_token(&state.signing_secret, purpose, &viewer.token, &csrf) {
        state.metrics.user_admin(action, "forbidden");
        return Err(text_response(
            StatusCode::FORBIDDEN,
            "CSRF validation failed\n",
        ));
    }
    Ok((viewer, form))
}

/// The viewer of now, when the session of `first` is still live. A form checks the session before its body and
/// acts after it. The session can end while the body arrives: a logout, End all sessions, a new password or a
/// deleted account. The action then needs this second check. The grants and the rights are the ones of now.
fn still_current(
    state: &AppState,
    headers: &HeaderMap,
    client: &Client,
    first: &Viewer,
) -> Option<Viewer> {
    current_viewer(state, headers, client).filter(|now| now.session.sid == first.session.sid)
}

/// Only the superadmin in its own right manages accounts. This does not apply while the superadmin browses as a user.
#[allow(clippy::result_large_err)] // the error is the response, sent as is
fn require_superadmin(
    state: &AppState,
    viewer: &Viewer,
    action: &'static str,
) -> Result<(), HttpResponse> {
    if is_superadmin(viewer) {
        Ok(())
    } else {
        state.metrics.user_admin(action, "forbidden");
        Err(text_response(StatusCode::FORBIDDEN, "Forbidden\n"))
    }
}

/// The response returns to the Users page. It shows the outcome of the action, as a code from a fixed list.
fn settings_redirect(code: &str) -> HttpResponse {
    redirect_response(
        StatusCode::SEE_OTHER,
        &format!("{}?s={code}", ui::settings::ACCESS_PATH),
    )
}

/// The response returns to the page "New user", with the outcome of a refused add.
fn new_user_redirect(code: &str) -> HttpResponse {
    redirect_response(
        StatusCode::SEE_OTHER,
        &format!("{}?s={code}", ui::settings::NEW_USER_PATH),
    )
}

/// The page "New user" again after a form that the server refuses because a right has no `access`.
/// It keeps the login and the grants, and the status is 422. The password is not sent back.
async fn new_user_refused(
    state: &AppState,
    viewer: &Viewer,
    refused: ui::settings::NeedsAccess,
) -> HttpResponse {
    let (Ok(logout_token), Ok(users_token)) = (
        sign_action_token(&state.signing_secret, LOGOUT_TOKEN, &viewer.token),
        sign_action_token(&state.signing_secret, USERS_TOKEN, &viewer.token),
    ) else {
        return text_response(StatusCode::INTERNAL_SERVER_ERROR, "authentication error\n");
    };
    let frame = ui::Frame {
        logout_token: &logout_token,
        keys: keys_page::list_visible(state, viewer),
        inbox: inbox_page::tab_visible(state, viewer),
        superadmin: is_superadmin(viewer),
        notify: inbox_page::notify_header(state, viewer).await,
        security: mfa_page::tab_visible(state, viewer),
        tab: state.tab_look(),
    };
    let workloads = current_catalog(state);
    let html = ui::new_user_page(
        &frame,
        &ui::NewUserView {
            notice: None,
            field: None,
            services: service_grants(state, &workloads),
            features: features_service(state),
            groups: group_chips(state),
            token: &users_token,
            refused: Some(refused),
        },
    );
    let mut response = html_response(StatusCode::UNPROCESSABLE_ENTITY, html);
    response.headers_mut().insert(
        HeaderName::from_static("cache-control"),
        HeaderValue::from_static("no-store"),
    );
    response
}

/// The response returns to the page of the account `login`, with the outcome of the action. The
/// login is the login as the server keeps it: it needs no encoding in a path.
fn user_redirect(login: &str, code: &str) -> HttpResponse {
    redirect_response(
        StatusCode::SEE_OTHER,
        &format!("{}?s={code}", ui::settings::user_path(login)),
    )
}

fn audit(
    state: &AppState,
    kind: AuditKind,
    login: Option<String>,
    actor: Option<String>,
    client: &Client,
    headers: &HeaderMap,
) {
    state.tracker.emit(Event::Audit(AuditEvent {
        kind,
        login,
        actor,
        ip: ip_text(client),
        user_agent: user_agent_text(headers),
        at: unix_now(),
    }));
}

/// Removes the grants that another grant of the list covers: the same action and service, on a
/// node above. The boxes of a page show a grant on a host ticked when the whole service is ticked,
/// so a form sends both. The account keeps the broad grant only.
///
/// A narrower grant with limit values stays: the PDP takes the lowest ceiling of the grants that
/// cover a node (`decision::ceilings_of`), so this grant restricts its node.
fn drop_covered(mut grants: Vec<Grant>) -> Vec<Grant> {
    grants.sort();
    grants.dedup();
    let all = grants.clone();
    grants.retain(|grant| {
        !grant.limits.is_empty()
            || !all.iter().any(|other| {
                other != grant
                    && other.action == grant.action
                    && other.service == grant.service
                    && grant.path.starts_with(&other.path)
            })
    });
    grants
}

/// Why the grants of a form are refused.
enum GrantsError {
    /// A field that is not known: a bad request.
    Bad,
    /// A grant or a limit value that the rules refuse: the code of a notice.
    Refused(&'static str),
}

/// The grants of a form of an account: the fields of the editor of grants, `grant<n>` and
/// `limit<n>.<name>[.<window>]`. Each grant follows the document of its service, and each limit
/// value follows the declaration of the document (portal 3.2).
fn account_grants(
    state: &AppState,
    fields: std::collections::HashMap<String, String>,
    old_grants: Option<&[Grant]>,
    unknown_code: &'static str,
) -> Result<Vec<Grant>, GrantsError> {
    let raw = key_forms::read(fields, true).map_err(|_| GrantsError::Bad)?;
    account_context(state, old_grants, |context| {
        key_forms::check_grants(&raw, context, key_forms::Holder::Account)
            .map(drop_covered)
            .map_err(|code| {
                GrantsError::Refused(match code {
                    key_forms::BAD_LIMITS => "bad-limits",
                    key_forms::NEEDS_ACCESS => "needs-access",
                    _ => unknown_code,
                })
            })
    })
}

/// The grants of a form of an account that the check refuses because a right has no `access`, and
/// the service. `None`: the form has another fault.
fn account_needs_access(
    state: &AppState,
    fields: std::collections::HashMap<String, String>,
) -> Option<(Vec<Grant>, String)> {
    let raw = key_forms::read(fields, true).ok()?;
    account_context(state, None, |context| {
        key_forms::grants_without_access(&raw, context, key_forms::Holder::Account)
    })
}

/// Runs `run` with the context of the check of the grants of an account.
fn account_context<R>(
    state: &AppState,
    old_grants: Option<&[Grant]>,
    run: impl FnOnce(&key_forms::Context<'_>) -> R,
) -> R {
    let catalog = current_catalog(state);
    let known: std::collections::HashSet<&str> = catalog
        .groups()
        .iter()
        .flat_map(|g| g.services.iter())
        .map(|s| s.id.as_str())
        .collect();
    let lookup = |service: &str| state.actions.declared(service);
    let exists = |service: &str| known.contains(service);
    let context = key_forms::Context {
        now: unix_now(),
        owner_grants: None,
        max_lifetime: None,
        document: &lookup,
        service_exists: &exists,
        old_grants,
    };
    run(&context)
}

async fn handle_users_add(
    state: &Arc<AppState>,
    request: Request<Incoming>,
    client: &Client,
) -> HttpResponse {
    // `users.allow_creation = false`: the route does not exist.
    if !state.config.users.allow_creation {
        return text_response(StatusCode::NOT_FOUND, "Not Found\n");
    }
    let headers = request.headers().clone();
    let (viewer, mut form) = match admin_request(state, request, client, "add").await {
        Ok(v) => v,
        Err(response) => return response,
    };
    if let Err(response) = require_superadmin(state, &viewer, "add") {
        return response;
    }

    let login = form.remove("login").unwrap_or_default();
    let password = Zeroizing::new(form.remove("password").unwrap_or_default());
    // The page "New user" sends the grants of the editor with the login and the password. A form
    // without `grants` has the login and the password only: the account then has no grant.
    let with_grants = form.remove("grants").as_deref() == Some("1");
    let fields = std::mem::take(&mut form);
    if !with_grants && !fields.is_empty() {
        return text_response(StatusCode::BAD_REQUEST, "Bad Request\n");
    }

    let rejected = |code: &str| {
        state.metrics.user_admin("add", "rejected");
        new_user_redirect(code)
    };
    let login = match normalize_login(&login) {
        Ok(login) => login,
        Err(code) => return rejected(code),
    };
    if login == state.superadmin_login {
        return rejected("reserved");
    }
    if let Err(code) = check_new_password(&password, state.config.security.max_password_bytes) {
        return rejected(code);
    }
    let grants = if with_grants {
        let typed = fields.clone();
        let mut ticked = match account_grants(state, fields, None, "unknown-grant") {
            Ok(grants) => grants,
            Err(GrantsError::Bad) => {
                return text_response(StatusCode::BAD_REQUEST, "Bad Request\n")
            }
            Err(GrantsError::Refused("needs-access")) => {
                // The form comes back with its login and its grants, and names the service.
                if let Some((grants, service)) = account_needs_access(state, typed) {
                    state.metrics.user_admin("add", "rejected");
                    return new_user_refused(
                        state,
                        &viewer,
                        ui::settings::NeedsAccess {
                            service,
                            login,
                            grants,
                        },
                    )
                    .await;
                }
                return rejected("needs-access");
            }
            Err(GrantsError::Refused(code)) => return rejected(code),
        };
        ticked.sort();
        ticked.dedup();
        if ticked.len() > state.config.users.max_grants {
            return rejected("grant-limit");
        }
        // Each action of the portal features needs the actions that it needs (portal 6.3a).
        if crate::portal_actions::check_user_grants(&state.config.portal_features(), &ticked)
            .is_err()
        {
            return rejected("keys-needs");
        }
        ticked
    } else {
        Vec::new()
    };
    // The file holds this login: an account comes from one source (portal 3.4a).
    if state.users.get(&login).is_some() || state.config.access.accounts.contains_key(&login) {
        return rejected("exists");
    }
    if state.users.is_full() {
        return rejected("limit");
    }
    // A hash calculation costs the same as a login check. It takes the same permit.
    let permit = match Arc::clone(&state.password_gate).acquire_owned().await {
        Ok(permit) => permit,
        Err(_) => return rejected("error"),
    };
    let hashed = tokio::task::spawn_blocking(move || {
        let _permit = permit;
        hash_password(password.as_bytes())
    })
    .await;
    let password_hash = match hashed {
        Ok(Ok(hash)) => hash,
        _ => return rejected("error"),
    };

    let row = UserRow {
        login: login.clone(),
        password_hash: password_hash.clone(),
        grants: grants.clone(),
        created_at: unix_now(),
    };
    let store = Arc::clone(&state.store);
    let created_at = row.created_at;
    match tokio::task::spawn_blocking(move || store.insert_user(&row)).await {
        Ok(Ok(())) => {}
        Ok(Err(InsertError::Exists)) => return rejected("exists"),
        Ok(Err(InsertError::Db(error))) => {
            log(
                state,
                LogLevel::Error,
                "user_insert_failed",
                &[("error", error)],
            );
            return rejected("error");
        }
        Err(_) => return rejected("error"),
    }
    // The new account starts with no inbox state: a phone, a code, an alert or a notification
    // state that an earlier holder of the login left goes. An account of the file that left the
    // file, or an old login of the superadmin, is such a holder. The row exists and nobody can use
    // the account yet. If the inbox fails, the account goes again.
    if let Err(error) = state.inbox.end_account(&login).await {
        log(
            state,
            LogLevel::Error,
            "user_add_inbox_failed",
            &[("error", error)],
        );
        let store = Arc::clone(&state.store);
        let target = login.clone();
        if let Ok(Err(error)) =
            tokio::task::spawn_blocking(move || store.remove_user(&target)).await
        {
            log(
                state,
                LogLevel::Error,
                "user_insert_undo_failed",
                &[("error", error)],
            );
        }
        return rejected("error");
    }
    let login_for_page = login.clone();
    state.users.insert(
        login.clone(),
        UserRecord {
            password_hash,
            grants,
            created_at,
        },
    );
    audit(
        state,
        AuditKind::UserAdded,
        Some(login.clone()),
        Some(state.superadmin_login.clone()),
        client,
        &headers,
    );
    state.metrics.user_admin("add", "ok");
    user_redirect(&login_for_page, "user-added")
}

async fn handle_users_delete(
    state: &Arc<AppState>,
    request: Request<Incoming>,
    client: &Client,
) -> HttpResponse {
    let headers = request.headers().clone();
    let (viewer, mut form) = match admin_request(state, request, client, "delete").await {
        Ok(v) => v,
        Err(response) => return response,
    };
    if let Err(response) = require_superadmin(state, &viewer, "delete") {
        return response;
    }
    let login = form.remove("login").unwrap_or_default();
    if !form.is_empty() {
        return text_response(StatusCode::BAD_REQUEST, "Bad Request\n");
    }
    let login = match normalize_login(&login) {
        Ok(login) if state.users.get(&login).is_some() => login,
        _ => {
            state.metrics.user_admin("delete", "rejected");
            return settings_redirect("no-user");
        }
    };

    // The inbox ends the state of the account first: it revokes the phones, deletes the unused
    // enrollment codes and detaches the rows that name the login. If the inbox fails, the account
    // stays: a phone must not outlive its account. Without the block `inbox`, this call does
    // nothing.
    if let Err(error) = state.inbox.end_account(&login).await {
        log(
            state,
            LogLevel::Error,
            "user_delete_inbox_failed",
            &[("error", error)],
        );
        state.metrics.user_admin("delete", "rejected");
        return settings_redirect("error");
    }
    let store = Arc::clone(&state.store);
    let target = login.clone();
    match tokio::task::spawn_blocking(move || store.remove_user(&target)).await {
        Ok(Ok(_)) => {}
        Ok(Err(error)) => {
            log(
                state,
                LogLevel::Error,
                "user_delete_failed",
                &[("error", error)],
            );
            state.metrics.user_admin("delete", "rejected");
            return settings_redirect("error");
        }
        Err(_) => return settings_redirect("error"),
    }
    state.users.remove(&login);
    state.sessions.revoke_user(&login, EndReason::UserDeleted);
    // A request of the account that passed its checks before the end of its sessions can have
    // made a code or sent an alert after the first call. The code needed the row of the account
    // (see `inbox::Slot::create_enrollment_code`), so this second call finds every such code.
    if let Err(error) = state.inbox.end_account(&login).await {
        log(
            state,
            LogLevel::Error,
            "user_delete_inbox_failed",
            &[("error", error)],
        );
    }
    audit(
        state,
        AuditKind::UserDeleted,
        Some(login),
        Some(state.superadmin_login.clone()),
        client,
        &headers,
    );
    state.metrics.user_admin("delete", "ok");
    settings_redirect("user-deleted")
}

/// Changes an account: its grants, and its password when a new password is given. The session of an
/// account whose password changed ends at once. A change of grants takes effect at the next request,
/// because the grants are read live.
async fn handle_users_update(
    state: &Arc<AppState>,
    request: Request<Incoming>,
    client: &Client,
) -> HttpResponse {
    let headers = request.headers().clone();
    let (viewer, mut form) = match admin_request(state, request, client, "update").await {
        Ok(v) => v,
        Err(response) => return response,
    };
    if let Err(response) = require_superadmin(state, &viewer, "update") {
        return response;
    }
    let login = form.remove("login").unwrap_or_default();
    // An empty value means "keep the password".
    let password = Zeroizing::new(form.remove("password").unwrap_or_default());
    // The form of the boxes carries `grants`. Without it, the request changes no grant: the
    // password form of the same page sends no box.
    let edit_grants = form.remove("grants").as_deref() == Some("1");
    // The grants are the remaining fields, as the editor of grants writes them.
    let fields = std::mem::take(&mut form);

    let rejected = |code: &str| {
        state.metrics.user_admin("update", "rejected");
        settings_redirect(code)
    };
    let login = match normalize_login(&login) {
        Ok(login) => login,
        Err(_) => return rejected("no-user"),
    };
    let Some(current) = state.users.get(&login) else {
        return rejected("no-user");
    };
    let back = |code: &str| user_redirect(&login, code);
    let rejected = |code: &str| {
        state.metrics.user_admin("update", "rejected");
        back(code)
    };
    let grants = if !edit_grants {
        if !fields.is_empty() {
            return text_response(StatusCode::BAD_REQUEST, "Bad Request\n");
        }
        None
    } else if state.policy.mode.editable() {
        let ticked =
            match account_grants(state, fields, Some(&current.grants), "edit-unknown-grant") {
                Ok(grants) => grants,
                Err(GrantsError::Bad) => {
                    return text_response(StatusCode::BAD_REQUEST, "Bad Request\n")
                }
                Err(GrantsError::Refused(code)) => return rejected(code),
            };
        // The form carries every grant that the account holds, with the grants on the portal
        // features (the service `portal`). The form replaces them all.
        let mut next: Vec<Grant> = ticked;
        next.sort();
        next.dedup();
        if next.len() > state.config.users.max_grants {
            return rejected("grant-limit");
        }
        // Each action of the portal features needs the actions that it needs (portal 6.3a).
        if crate::portal_actions::check_user_grants(&state.config.portal_features(), &next).is_err()
        {
            return rejected("keys-needs");
        }
        Some(next)
    } else {
        if !fields.is_empty() {
            return rejected("grants-static");
        }
        None
    };
    let change_password = !password.is_empty();
    if change_password {
        if let Err(code) = check_new_password(&password, state.config.security.max_password_bytes) {
            return rejected(&format!("edit-{code}"));
        }
    }
    let grants_changed = grants.as_ref().is_some_and(|next| *next != current.grants);
    let grants = grants.filter(|_| grants_changed);
    if !change_password && !grants_changed {
        state.metrics.user_admin("update", "ok");
        return back("unchanged");
    }

    let new_hash = if change_password {
        // A hash calculation costs the same as a login check. It takes the same permit.
        let permit = match Arc::clone(&state.password_gate).acquire_owned().await {
            Ok(permit) => permit,
            Err(_) => return rejected("error"),
        };
        match tokio::task::spawn_blocking(move || {
            let _permit = permit;
            hash_password(password.as_bytes())
        })
        .await
        {
            Ok(Ok(hash)) => Some(hash),
            _ => return rejected("error"),
        }
    } else {
        None
    };

    let store = Arc::clone(&state.store);
    let (target, hash_for_store, grants_for_store) =
        (login.clone(), new_hash.clone(), grants.clone());
    match tokio::task::spawn_blocking(move || {
        store.update_user(
            &target,
            hash_for_store.as_deref(),
            grants_for_store.as_deref(),
        )
    })
    .await
    {
        Ok(Ok(true)) => {}
        Ok(Ok(false)) => return rejected("no-user"),
        Ok(Err(error)) => {
            log(
                state,
                LogLevel::Error,
                "user_update_failed",
                &[("error", error)],
            );
            return rejected("error");
        }
        Err(_) => return rejected("error"),
    }
    state.users.update(&login, new_hash, grants);
    if change_password {
        state.sessions.revoke_user(&login, EndReason::Revoked);
        audit(
            state,
            AuditKind::PasswordReset,
            Some(login.clone()),
            Some(state.superadmin_login.clone()),
            client,
            &headers,
        );
    }
    if grants_changed {
        audit(
            state,
            AuditKind::UserUpdated,
            Some(login.clone()),
            Some(state.superadmin_login.clone()),
            client,
            &headers,
        );
    }
    state.metrics.user_admin("update", "ok");
    back(if change_password {
        "password-reset"
    } else {
        "user-updated"
    })
}

/// "Log in as": the session of the superadmin becomes a session of the user. The session is flagged as impersonated.
async fn handle_impersonate(
    state: &Arc<AppState>,
    request: Request<Incoming>,
    client: &Client,
) -> HttpResponse {
    let headers = request.headers().clone();
    let (viewer, mut form) = match admin_request(state, request, client, "impersonate").await {
        Ok(v) => v,
        Err(response) => return response,
    };
    if let Err(response) = require_superadmin(state, &viewer, "impersonate") {
        return response;
    }
    let login = form.remove("login").unwrap_or_default();
    if !form.is_empty() {
        return text_response(StatusCode::BAD_REQUEST, "Bad Request\n");
    }
    let login = match normalize_login(&login) {
        Ok(login) if state.users.get(&login).is_some() => login,
        _ => {
            state.metrics.user_admin("impersonate", "rejected");
            return settings_redirect("no-user");
        }
    };
    let principal = Principal::User(login.clone());
    let Some(credential) = credential_stamp(state, &principal) else {
        state.metrics.user_admin("impersonate", "rejected");
        return settings_redirect("no-user");
    };
    let response = switch_session(
        state,
        client,
        &headers,
        &viewer,
        Switch {
            principal,
            credential,
            login: login.clone(),
            impersonated: true,
        },
        redirect_response(StatusCode::SEE_OTHER, HOME_PATH),
    );
    let response = match response {
        Ok(response) => response,
        Err(response) => return response,
    };
    audit(
        state,
        AuditKind::ImpersonationStarted,
        Some(login),
        Some(state.superadmin_login.clone()),
        client,
        &headers,
    );
    state.metrics.user_admin("impersonate", "ok");
    response
}

/// "Back to superadmin": ends the impersonated session and opens a new superadmin session. The
/// impersonated session proves that the superadmin opened it. The portal asks for no password.
async fn handle_impersonate_end(
    state: &Arc<AppState>,
    request: Request<Incoming>,
    client: &Client,
) -> HttpResponse {
    let headers = request.headers().clone();
    let (viewer, form) = match admin_request(state, request, client, "impersonate_end").await {
        Ok(v) => v,
        Err(response) => return response,
    };
    if !form.is_empty() {
        return text_response(StatusCode::BAD_REQUEST, "Bad Request\n");
    }
    if !viewer.session.impersonated {
        state.metrics.user_admin("impersonate_end", "forbidden");
        return text_response(StatusCode::FORBIDDEN, "Forbidden\n");
    }
    let impersonated = viewer.login.clone();
    let response = switch_session(
        state,
        client,
        &headers,
        &viewer,
        Switch {
            principal: Principal::Superadmin,
            credential: state.superadmin_generation,
            login: state.superadmin_login.clone(),
            impersonated: false,
        },
        redirect_response(StatusCode::SEE_OTHER, ui::settings::ACCESS_PATH),
    );
    let response = match response {
        Ok(response) => response,
        Err(response) => return response,
    };
    audit(
        state,
        AuditKind::ImpersonationEnded,
        Some(impersonated),
        Some(state.superadmin_login.clone()),
        client,
        &headers,
    );
    state.metrics.user_admin("impersonate_end", "ok");
    response
}

/// The incident action: ends every session, the superadmin sessions too. Only the superadmin in its own
/// right can do it, and the form must carry the confirmation field. The browser of the superadmin
/// loses its session as well, so the answer sends it to the sign-in page and deletes its cookies.
async fn handle_end_sessions(
    state: &Arc<AppState>,
    request: Request<Incoming>,
    client: &Client,
) -> HttpResponse {
    let headers = request.headers().clone();
    let (viewer, mut form) = match admin_request(state, request, client, "end_sessions").await {
        Ok(v) => v,
        Err(response) => return response,
    };
    if let Err(response) = require_superadmin(state, &viewer, "end_sessions") {
        return response;
    }
    let confirmed = form.remove("confirm").as_deref() == Some(END_SESSIONS_CONFIRM);
    if !form.is_empty() || !confirmed {
        state.metrics.user_admin("end_sessions", "rejected");
        return text_response(StatusCode::BAD_REQUEST, "Bad Request\n");
    }
    let ended = match state.sessions.end_all(EndReason::Revoked) {
        Ok(ended) => ended,
        Err(error) => {
            log(
                state,
                LogLevel::Error,
                "end_sessions_failed",
                &[("error", error)],
            );
            state.metrics.user_admin("end_sessions", "rejected");
            return settings_redirect("error");
        }
    };
    audit(
        state,
        AuditKind::SessionsEnded,
        None,
        Some(state.superadmin_login.clone()),
        client,
        &headers,
    );
    log(
        state,
        LogLevel::Warn,
        "sessions_ended",
        &[("count", ended.to_string())],
    );
    state.metrics.user_admin("end_sessions", "ok");
    signed_out_response(state, client, LOGIN_PATH)
}

/// A redirect that deletes the session cookie and the anti-CSRF cookie of the browser.
fn signed_out_response(state: &AppState, client: &Client, location: &str) -> HttpResponse {
    let mut response = redirect_response(StatusCode::SEE_OTHER, location);
    let expired_session = format!(
        "{}=; {}",
        transport::session_cookie_name(&state.config.session.cookie_name, client.https),
        transport::cookie_attributes(
            client.https,
            state.config.session.same_site.as_str(),
            Some(0),
        ),
    );
    if append_dynamic_header(&mut response, SET_COOKIE, &expired_session).is_err()
        || append_dynamic_header(
            &mut response,
            SET_COOKIE,
            &expired_csrf_cookie(state, client.https),
        )
        .is_err()
    {
        return text_response(StatusCode::INTERNAL_SERVER_ERROR, "authentication error\n");
    }
    response
}

/// The session that a switch opens.
struct Switch {
    principal: Principal,
    /// The stamp of the credential of `principal` now.
    credential: Credential,
    /// The login that the history shows.
    login: String,
    impersonated: bool,
}

/// Opens a session for `next.principal` and sets its cookie on `response`. Only after that, the function ends the
/// current session. If a failure occurs, the browser keeps the session that it had, and the error is the answer.
/// Another action can end the current session while the request runs, for example a logout or End all sessions.
/// The new session then ends too: a switch never gives a session in place of a session that is already over.
///
/// The new session ends when the current session would have ended. A switch never gives a fresh
/// `session.ttl`: only a sign-in with the password does.
#[allow(clippy::result_large_err)] // the error is the response, sent as is
fn switch_session(
    state: &AppState,
    client: &Client,
    headers: &HeaderMap,
    current: &Viewer,
    next: Switch,
    mut response: HttpResponse,
) -> Result<HttpResponse, HttpResponse> {
    let Switch {
        principal,
        credential,
        login,
        impersonated,
    } = next;
    let token = match state.sessions.issue(
        client.binding,
        NewSession {
            principal,
            credential,
            login,
            impersonated,
            // A switch of session never gives the remembered rule.
            remember: false,
            mfa: false,
            ip: ip_text(client),
            user_agent: user_agent_text(headers),
            not_after: Some(current.session.expires_at),
        },
    ) {
        Ok(token) => Zeroizing::new(token),
        Err(SessionIssueError::Capacity) => {
            state.metrics.session_capacity_rejected();
            return Err(text_response(
                StatusCode::SERVICE_UNAVAILABLE,
                "session capacity reached\n",
            ));
        }
        Err(SessionIssueError::Random) => {
            return Err(text_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "authentication error\n",
            ));
        }
        Err(SessionIssueError::Storage) => {
            return Err(text_response(
                StatusCode::SERVICE_UNAVAILABLE,
                "session store unavailable\n",
            ));
        }
    };
    let left = current.session.expires_at.saturating_sub(unix_now()).max(0) as u64;
    let cookie = session_cookie_header(
        state,
        client,
        token.as_str(),
        Some(left.min(state.config.session.ttl.as_secs())),
    );
    if append_dynamic_header(&mut response, SET_COOKIE, cookie.as_str()).is_err() {
        state.sessions.revoke(token.as_str(), EndReason::Revoked);
        return Err(text_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            "authentication error\n",
        ));
    }
    if state
        .sessions
        .revoke(current.token.as_str(), EndReason::Impersonation)
        .is_none()
    {
        state.sessions.revoke(token.as_str(), EndReason::Revoked);
        return Err(signed_out_response(state, client, LOGIN_PATH));
    }
    state.metrics.session_issued();
    Ok(response)
}

fn login_page(
    state: &AppState,
    client: &Client,
    rd: &str,
    sig: &str,
    csrf: &str,
    error: Option<&str>,
    status: StatusCode,
) -> HttpResponse {
    let Ok(form_token) = sign_form_token(&state.signing_secret, csrf, unix_millis()) else {
        return text_response(StatusCode::INTERNAL_SERVER_ERROR, "authentication error\n");
    };
    // The box is never ticked, also on the page that follows a refusal: the page of a refusal does
    // not depend on what the person sent. It needs no script and no identifier. The label holds the box.
    let keep_for = state
        .config
        .session
        .remember
        .map(|remember| crate::util::format_duration_words(remember.ttl.as_secs()));
    let body = ui::signin::login_page(&ui::signin::Login {
        csrf,
        rd,
        sig,
        form_token: &form_token,
        error,
        max_password_bytes: state.config.security.max_password_bytes,
        keep_for: keep_for.as_deref(),
        names: ui::signin::LoginNames {
            form_token: FORM_TOKEN_FIELD,
            trap: TRAP_FIELD,
            keep: KEEP_FIELD,
            keep_value: KEEP_VALUE,
        },
    });
    let mut response = masked_response(state, status, body);
    if append_dynamic_header(
        &mut response,
        SET_COOKIE,
        &csrf_cookie(state, csrf, client.https),
    )
    .is_err()
    {
        return text_response(StatusCode::INTERNAL_SERVER_ERROR, "authentication error\n");
    }
    response
}

/// The session cookie of the request, valid or not.
fn session_cookie<'a>(
    state: &AppState,
    headers: &'a HeaderMap,
    client: &Client,
) -> Option<&'a str> {
    cookie_value(
        headers,
        transport::session_cookie_name(&state.config.session.cookie_name, client.https),
        state.config.security.max_cookie_header_bytes,
    )
}

fn has_valid_session(state: &AppState, headers: &HeaderMap, client: &Client) -> bool {
    current_viewer(state, headers, client).is_some()
}

fn content_type_is_form(headers: &HeaderMap) -> bool {
    match unique_header_str_name(headers, CONTENT_TYPE) {
        Ok(Some(v)) => v.split(';').next().is_some_and(|v| {
            v.trim()
                .eq_ignore_ascii_case("application/x-www-form-urlencoded")
        }),
        _ => false,
    }
}

fn unique_header_str<'a>(
    headers: &'a HeaderMap,
    name: &'static str,
) -> Result<Option<&'a str>, ()> {
    let name = HeaderName::from_static(name);
    unique_header_str_name(headers, name)
}

fn unique_header_str_name(headers: &HeaderMap, name: HeaderName) -> Result<Option<&str>, ()> {
    let mut values = headers.get_all(name).iter();
    let Some(first) = values.next() else {
        return Ok(None);
    };
    if values.next().is_some() {
        return Err(());
    }
    first.to_str().map(Some).map_err(|_| ())
}

fn cookie_value<'a>(headers: &'a HeaderMap, name: &str, max_header: usize) -> Option<&'a str> {
    let raw = unique_header_str_name(headers, COOKIE).ok().flatten()?;
    if raw.len() > max_header {
        return None;
    }
    let mut found = None;
    for item in raw.split(';') {
        let item = item.trim();
        let Some((key, value)) = item.split_once('=') else {
            continue;
        };
        if key != name {
            continue;
        }
        if found.is_some()
            || value
                .bytes()
                .any(|b| b <= 0x20 || b == 0x7f || b == b';' || b == b',')
        {
            return None;
        }
        found = Some(value);
    }
    found
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct CanonicalOrigin {
    scheme: String,
    host: String,
    port: u16,
}

fn validate_post_origin(headers: &HeaderMap) -> bool {
    let expected = match expected_origin(headers) {
        Some(v) => v,
        None => return false,
    };

    match unique_header_str_name(headers, ORIGIN) {
        Ok(Some(origin)) => canonical_origin(origin, true).is_some_and(|v| v == expected),
        Ok(None) => match unique_header_str_name(headers, REFERER) {
            Ok(Some(referer)) => canonical_origin(referer, false).is_some_and(|v| v == expected),
            _ => false,
        },
        Err(_) => false,
    }
}

fn expected_origin(headers: &HeaderMap) -> Option<CanonicalOrigin> {
    let forwarded_proto = unique_header_str(headers, "x-forwarded-proto").ok()?;
    let forwarded_host = unique_header_str(headers, "x-forwarded-host").ok()?;

    match (forwarded_proto, forwarded_host) {
        (Some(proto), Some(host)) => canonical_origin_from_parts(proto, host),
        (None, None) => {
            let host = unique_header_str_name(headers, HOST).ok()??;
            canonical_origin_from_parts("http", host)
        }
        _ => None,
    }
}

fn canonical_origin_from_parts(scheme: &str, authority: &str) -> Option<CanonicalOrigin> {
    if authority.is_empty()
        || authority.contains('/')
        || authority.contains('?')
        || authority.contains('#')
        || authority.contains('@')
        || authority.contains(',')
    {
        return None;
    }
    canonical_origin(&format!("{}://{}", scheme.trim(), authority.trim()), true)
}

fn canonical_origin(value: &str, origin_header: bool) -> Option<CanonicalOrigin> {
    let value = value.trim();
    if value.eq_ignore_ascii_case("null") || value.contains('#') {
        return None;
    }
    let uri = Uri::from_str(value).ok()?;
    let scheme = uri.scheme_str()?.to_ascii_lowercase();
    if scheme != "http" && scheme != "https" {
        return None;
    }
    let authority = uri.authority()?;
    if authority.as_str().contains('@') {
        return None;
    }
    if origin_header && (uri.query().is_some() || !matches!(uri.path(), "" | "/")) {
        return None;
    }
    let host = authority.host().to_ascii_lowercase();
    if host.is_empty() {
        return None;
    }
    let port = authority
        .port_u16()
        .unwrap_or(if scheme == "https" { 443 } else { 80 });
    Some(CanonicalOrigin { scheme, host, port })
}

fn query_return_pair(uri: &str) -> Result<(Option<String>, Option<String>), String> {
    let query = match uri.split_once('?') {
        Some((_, query)) => query,
        None => return Ok((None, None)),
    };
    let mut rd = None;
    let mut sig = None;
    for pair in query.split('&') {
        if pair.is_empty() {
            continue;
        }
        let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
        let key = crate::util::percent_decode_form(key)?;
        let value = crate::util::percent_decode_form(value)?;
        let duplicate = match key.as_str() {
            "rd" => rd.replace(value).is_some().then_some("rd"),
            "sig" => sig.replace(value).is_some().then_some("sig"),
            _ => None,
        };
        if let Some(name) = duplicate {
            return Err(format!("duplicate {name} query parameter"));
        }
    }
    Ok((rd, sig))
}

#[derive(Debug)]
enum BodyReadError {
    TooLarge,
    Timeout,
    Io,
}

async fn read_limited_body(
    body: Incoming,
    max: usize,
    timeout: std::time::Duration,
) -> Result<Vec<u8>, BodyReadError> {
    if body
        .size_hint()
        .upper()
        .is_some_and(|upper| upper > max as u64)
    {
        return Err(BodyReadError::TooLarge);
    }
    tokio::time::timeout(timeout, read_limited_body_inner(body, max))
        .await
        .map_err(|_| BodyReadError::Timeout)?
}

async fn read_limited_body_inner(mut body: Incoming, max: usize) -> Result<Vec<u8>, BodyReadError> {
    let mut out = Vec::with_capacity(body.size_hint().lower().min(max as u64) as usize);
    while let Some(frame) = body.frame().await {
        let frame = frame.map_err(|_| BodyReadError::Io)?;
        let Ok(data) = frame.into_data() else {
            continue;
        };
        if out.len().saturating_add(data.len()) > max {
            out.zeroize();
            return Err(BodyReadError::TooLarge);
        }
        out.extend_from_slice(&data);
    }
    Ok(out)
}

fn empty_response(status: StatusCode) -> HttpResponse {
    secure_headers(Response::new(Full::new(Bytes::new())), status)
}

fn json_response(status: StatusCode, body: String) -> HttpResponse {
    let mut response = response_with_body(status, Bytes::from(body));
    response.headers_mut().insert(
        CONTENT_TYPE,
        HeaderValue::from_static("application/json; charset=utf-8"),
    );
    response.headers_mut().insert(
        HeaderName::from_static("cache-control"),
        HeaderValue::from_static("no-store"),
    );
    apply_secure_headers(&mut response, true);
    response
}

/// A page that loads only the shared stylesheet and the theme script. The names and the structure
/// of the page change before the server sends it: see `ui::mask`.
fn masked_response(state: &AppState, status: StatusCode, body: String) -> HttpResponse {
    html_response(status, state.mask.page(&body))
}

fn html_response(status: StatusCode, body: String) -> HttpResponse {
    let mut response = response_with_body(status, Bytes::from(body));
    response.headers_mut().insert(
        CONTENT_TYPE,
        HeaderValue::from_static("text/html; charset=utf-8"),
    );
    apply_secure_headers(&mut response, true);
    response
}

/// An embedded asset, such as a stylesheet or a script. The content does not change per request. Therefore, it can be cached.
fn static_response(content_type: &'static str, body: &str) -> HttpResponse {
    let mut response = response_with_body(StatusCode::OK, Bytes::copy_from_slice(body.as_bytes()));
    response
        .headers_mut()
        .insert(CONTENT_TYPE, HeaderValue::from_static(content_type));
    response.headers_mut().insert(
        HeaderName::from_static("cache-control"),
        HeaderValue::from_static("public, max-age=3600"),
    );
    apply_secure_headers(&mut response, false);
    response
}

/// Whether `path` is the neutral path of the stylesheet or of the script that every page loads.
/// The pages of the sign-in and of the launcher load the files with the mapped names. The
/// settings pages load the plain files.
fn is_neutral_asset(state: &AppState, path: &str) -> bool {
    let mapped = state.mask.assets();
    [
        &ui::assets().css_path,
        &ui::assets().js_path,
        &ui::assets().notify_path,
        &mapped.css_path,
        &mapped.js_path,
        &mapped.notify_path,
    ]
    .iter()
    .any(|known| path == known.as_str())
}

/// An asset with a hashed path. The path changes when the content changes. Therefore, the browser
/// can keep the file for a long time.
fn hashed_response(content_type: &'static str, body: &str) -> HttpResponse {
    let mut response = static_response(content_type, body);
    response.headers_mut().insert(
        HeaderName::from_static("cache-control"),
        HeaderValue::from_static("public, max-age=31536000, immutable"),
    );
    response
}

fn text_response(status: StatusCode, body: &str) -> HttpResponse {
    let mut response = response_with_body(status, Bytes::copy_from_slice(body.as_bytes()));
    response.headers_mut().insert(
        CONTENT_TYPE,
        HeaderValue::from_static("text/plain; charset=utf-8"),
    );
    apply_secure_headers(&mut response, true);
    response
}

fn metrics_response(status: StatusCode, body: String) -> HttpResponse {
    let mut response = response_with_body(status, Bytes::from(body));
    response.headers_mut().insert(
        CONTENT_TYPE,
        HeaderValue::from_static("text/plain; version=0.0.4; charset=utf-8"),
    );
    response.headers_mut().insert(
        HeaderName::from_static("cache-control"),
        HeaderValue::from_static("no-store, max-age=0"),
    );
    response.headers_mut().insert(
        HeaderName::from_static("x-content-type-options"),
        HeaderValue::from_static("nosniff"),
    );
    response
}

fn redirect_response(status: StatusCode, location: &str) -> HttpResponse {
    let value = match HeaderValue::from_str(location) {
        Ok(v) => v,
        Err(_) => return text_response(StatusCode::INTERNAL_SERVER_ERROR, "invalid redirect\n"),
    };
    let mut response = response_with_body(status, Bytes::new());
    response.headers_mut().insert(LOCATION, value);
    apply_secure_headers(&mut response, true);
    response
}

fn response_with_body(status: StatusCode, body: Bytes) -> HttpResponse {
    let mut response = Response::new(Full::new(body));
    *response.status_mut() = status;
    response
}

fn secure_headers(mut response: HttpResponse, status: StatusCode) -> HttpResponse {
    *response.status_mut() = status;
    apply_secure_headers(&mut response, true);
    response
}

fn apply_secure_headers(response: &mut HttpResponse, no_store: bool) {
    if no_store {
        response.headers_mut().insert(
            HeaderName::from_static("cache-control"),
            HeaderValue::from_static("no-store, max-age=0"),
        );
        response.headers_mut().insert(
            HeaderName::from_static("pragma"),
            HeaderValue::from_static("no-cache"),
        );
    }
    response.headers_mut().insert(
        HeaderName::from_static("x-content-type-options"),
        HeaderValue::from_static("nosniff"),
    );
    response.headers_mut().insert(
        HeaderName::from_static("referrer-policy"),
        HeaderValue::from_static("same-origin"),
    );
    response.headers_mut().insert(
        HeaderName::from_static("x-frame-options"),
        HeaderValue::from_static("DENY"),
    );
    response.headers_mut().insert(
        HeaderName::from_static("content-security-policy"),
        HeaderValue::from_static(
            "default-src 'none'; style-src 'self'; script-src 'self'; img-src 'self' data:; connect-src 'self'; form-action 'self'; frame-ancestors 'none'; base-uri 'none'; object-src 'none'",
        ),
    );
}

fn set_dynamic_header(
    response: &mut HttpResponse,
    name: HeaderName,
    value: &str,
) -> Result<(), ()> {
    let value = HeaderValue::from_str(value).map_err(|_| ())?;
    response.headers_mut().insert(name, value);
    Ok(())
}

fn append_dynamic_header(
    response: &mut HttpResponse,
    name: HeaderName,
    value: &str,
) -> Result<(), ()> {
    let value = HeaderValue::from_str(value).map_err(|_| ())?;
    response.headers_mut().append(name, value);
    Ok(())
}

/// How long the browser keeps the token of the sign-in page: 12 hours. A person can leave the page
/// open, and the token still works. The token is not a secret: it only binds the form to the cookie.
const CSRF_COOKIE_SECS: u64 = 12 * 3600;

fn csrf_cookie(state: &AppState, csrf: &str, https: bool) -> String {
    format!(
        "{}={csrf}; {}",
        transport::csrf_cookie_name(https),
        transport::cookie_attributes(
            https,
            state.config.session.same_site.as_str(),
            Some(CSRF_COOKIE_SECS),
        )
    )
}

fn expired_csrf_cookie(state: &AppState, https: bool) -> String {
    format!(
        "{}=; {}",
        transport::csrf_cookie_name(https),
        transport::cookie_attributes(https, state.config.session.same_site.as_str(), Some(0))
    )
}

fn method_metric_label(method: &Method) -> &'static str {
    if method == Method::GET {
        "GET"
    } else if method == Method::POST {
        "POST"
    } else if method == Method::HEAD {
        "HEAD"
    } else {
        "OTHER"
    }
}

fn route_metric_label(state: &AppState, path: &str) -> &'static str {
    if state.config.metrics.enabled && path == state.config.metrics.path {
        return "metrics";
    }
    match path {
        HOME_PATH => "home",
        VERIFY_PATH => "verify",
        VERIFY_KEY_PATH => "verify_key",
        HEALTH_PATH => "health",
        READY_PATH => "ready",
        _ if crate::inbox::owns(path) => "inbox_api",
        STYLE_PATH | SETTINGS_STYLE_PATH | THEME_JS_PATH | SETTINGS_JS_PATH | NOTIFY_JS_PATH => {
            "assets"
        }
        _ if is_neutral_asset(state, path)
            || (path == ui::favicon_path() && state.tab_look().icon) =>
        {
            "assets"
        }
        _ if is_settings_path(path) => "settings",
        _ if path.starts_with(ICONS_PREFIX) => "icons",
        SUCCESS_PATH => "success",
        LOGIN_PATH => "login",
        ui::mfa::CODE_PATH => "login_code",
        ui::mfa::ENROL_PATH => "login_enrol",
        ui::mfa::SECURITY_ENABLE_PATH
        | ui::mfa::SECURITY_DISABLE_PATH
        | ui::mfa::SECURITY_RECOVERY_PATH => "security",
        ui::mfa::RESET_PATH => "users_reset_two_factor",
        LOGOUT_PATH => "logout",
        DENIED_PATH => "denied",
        USERS_ADD_PATH => "users_add",
        USERS_DELETE_PATH => "users_delete",
        USERS_UPDATE_PATH => "users_update",
        IMPERSONATE_PATH => "impersonate",
        IMPERSONATE_END_PATH => "impersonate_end",
        _ if inbox_page::is_inbox_path(path) => "inbox_page",
        _ => "not_found",
    }
}

fn build_version() -> &'static str {
    option_env!("PORTAL_RELEASE_VERSION").unwrap_or(env!("CARGO_PKG_VERSION"))
}

fn log(state: &AppState, level: LogLevel, event: &str, fields: &[(&str, String)]) {
    if level > state.config.logging.level {
        return;
    }
    let level_name = match level {
        LogLevel::Error => "error",
        LogLevel::Warn => "warn",
        LogLevel::Info => "info",
        LogLevel::Debug => "debug",
    };
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|v| v.as_secs())
        .unwrap_or(0);
    let mut line = format!(
        "{{\"ts\":{},\"level\":\"{}\",\"event\":\"{}\"",
        ts,
        level_name,
        json_escape(event)
    );
    for (key, value) in fields {
        line.push_str(&format!(
            ",\"{}\":\"{}\"",
            json_escape(key),
            json_escape(value)
        ));
    }
    line.push('}');
    eprintln!("{line}");
}

mod edge;
mod inbox_page;
mod inbox_rights;
mod keys_page;
mod mfa_page;
mod options_page;
mod pdp;

#[cfg(test)]
mod cluster_tests;

#[cfg(test)]
mod tests {
    use super::*;

    /// The paths of the inbox and the paths of the portal never overlap: `/v1/`, `/api/v1/`,
    /// `/readyz` and `/api/version` are the inbox's, and no route of the portal is one of them.
    #[test]
    fn the_paths_of_the_inbox_are_not_shadowed_by_a_route_of_the_portal() {
        for inbox in [
            "/v1/enroll",
            "/v1/session",
            "/v1/other",
            "/api/v1/alerts",
            "/api/v1/alerts/0123abcd",
            "/api/v1/notify/infra",
            "/api/v1/webhooks/alertmanager/infra",
            "/readyz",
            "/api/version",
        ] {
            assert!(crate::inbox::owns(inbox), "{inbox}");
        }
        let portal = [
            HOME_PATH,
            LOGIN_PATH,
            LOGOUT_PATH,
            VERIFY_PATH,
            VERIFY_KEY_PATH,
            HEALTH_PATH,
            SUCCESS_PATH,
            DENIED_PATH,
            USERS_ADD_PATH,
            USERS_DELETE_PATH,
            USERS_UPDATE_PATH,
            IMPERSONATE_PATH,
            IMPERSONATE_END_PATH,
            END_SESSIONS_PATH,
            STYLE_PATH,
            SETTINGS_STYLE_PATH,
            THEME_JS_PATH,
            SETTINGS_JS_PATH,
            NOTIFY_JS_PATH,
            ICONS_PREFIX,
            ui::keys::PATH,
            ui::keys::NEW_PATH,
            ui::keys::CREATE_PATH,
            ui::keys::EDIT_PATH,
            ui::keys::REVOKE_PATH,
            options_page::PATH,
            ui::settings::ACCESS_PATH,
            ui::settings::SERVICES_PATH,
            ui::settings::INBOX_PATH,
            "/metrics",
        ];
        for path in portal {
            assert!(!crate::inbox::owns(path), "{path}");
        }
        // The prefix needs the slash: a path that only starts with the same letters is the portal's.
        for path in [
            "/api/v1",
            "/v1",
            "/v10/session",
            "/x/v1/session",
            "/api/versions",
            "/readyz/x",
            "/api/v2/x",
        ] {
            assert!(!crate::inbox::owns(path), "{path}");
        }
    }

    fn headers(entries: &[(&'static str, &'static str)]) -> HeaderMap {
        let mut out = HeaderMap::new();
        for (name, value) in entries {
            out.append(
                HeaderName::from_static(name),
                HeaderValue::from_static(value),
            );
        }
        out
    }

    #[test]
    fn a_csrf_cookie_is_reused_only_when_it_has_the_shape_of_ours() {
        let ok = "A".repeat(32);
        assert_eq!(csrf_shape(&ok), Some(ok.clone()));
        assert_eq!(csrf_shape("short"), None);
        assert_eq!(csrf_shape(&"A".repeat(33)), None);
        assert_eq!(csrf_shape(&format!("{}\"", "A".repeat(31))), None);
        let issued = new_csrf_token().unwrap();
        assert_eq!(
            csrf_shape(&issued),
            Some(issued),
            "what new_csrf_token issues is accepted"
        );
    }

    #[test]
    fn query_return_pair_rejects_duplicates_and_bad_encoding() {
        assert!(query_return_pair("/auth/login?rd=%2F&rd=%2Fx&sig=a").is_err());
        assert!(query_return_pair("/auth/login?rd=%ZZ&sig=a").is_err());
    }

    #[test]
    fn query_return_pair_decodes_expected_fields() {
        let (rd, sig) = query_return_pair("/auth/login?rd=%2Fgrafana%3Fx%3D1&sig=abc").unwrap();
        assert_eq!(rd.as_deref(), Some("/grafana?x=1"));
        assert_eq!(sig.as_deref(), Some("abc"));
    }

    #[test]
    fn origin_accepts_matching_direct_origin() {
        let h = headers(&[
            ("host", "localhost:4181"),
            ("origin", "http://localhost:4181"),
        ]);
        assert!(validate_post_origin(&h));
    }

    #[test]
    fn origin_accepts_matching_forwarded_origin() {
        let h = headers(&[
            ("host", "portal:4181"),
            ("x-forwarded-proto", "https"),
            ("x-forwarded-host", "example.test"),
            ("origin", "https://example.test"),
        ]);
        assert!(validate_post_origin(&h));
    }

    #[test]
    fn origin_rejects_mismatch_and_null() {
        let mismatch = headers(&[("host", "localhost:4181"), ("origin", "http://evil.test")]);
        assert!(!validate_post_origin(&mismatch));
        let null_origin = headers(&[("host", "localhost:4181"), ("origin", "null")]);
        assert!(!validate_post_origin(&null_origin));
    }

    #[test]
    fn referer_is_used_only_when_origin_is_absent() {
        let h = headers(&[
            ("host", "localhost:4181"),
            ("referer", "http://localhost:4181/auth/login?x=1"),
        ]);
        assert!(validate_post_origin(&h));

        let bad = headers(&[
            ("host", "localhost:4181"),
            ("origin", "http://evil.test"),
            ("referer", "http://localhost:4181/auth/login"),
        ]);
        assert!(!validate_post_origin(&bad));
    }

    #[test]
    fn origin_rejects_missing_headers_and_partial_forwarding() {
        let missing = headers(&[("host", "localhost:4181")]);
        assert!(!validate_post_origin(&missing));

        let partial = headers(&[
            ("host", "localhost:4181"),
            ("x-forwarded-proto", "https"),
            ("origin", "http://localhost:4181"),
        ]);
        assert!(!validate_post_origin(&partial));
    }
}
