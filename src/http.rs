use std::convert::Infallible;
use std::net::{IpAddr, SocketAddr};
use std::str::FromStr;
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use http_body_util::{BodyExt, Full};
use hyper::body::{Body as _, Bytes, Incoming};
use hyper::header::{
    HeaderMap, HeaderName, HeaderValue, CONTENT_TYPE, COOKIE, HOST, LOCATION,
    ORIGIN, REFERER, RETRY_AFTER, SET_COOKIE, USER_AGENT,
};
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{Method, Request, Response, StatusCode, Uri};
use hyper_util::rt::{TokioIo, TokioTimer};
use tokio::net::TcpListener;
use tokio::sync::Semaphore;
use zeroize::{Zeroize, Zeroizing};

use crate::access::{self, service_allows, Decision};
use crate::catalog::Catalog;
use crate::config::{Config, LogLevel};
use crate::discovery;
use crate::icons::{Icons, Set};
use crate::crypto::{
    hash_password, new_csrf_token, sign_action_token, sign_return_uri, verify_action_token,
    verify_csrf, verify_password, verify_return_uri_signature,
};
use crate::metrics::Metrics;
use crate::rate_limit::RateLimiter;
use crate::session::{
    Binding, NewSession, Principal, Session, SessionIssueError, SessionStore,
};
use crate::store::{InsertError, LoginPage, Snapshot, Store, UserRow};
use crate::tracking::{unix_now, AuditEvent, AuditKind, EndReason, Event, LoginAttempt, Tracker};
use crate::transport;
use crate::ui;
use crate::users::{check_new_password, normalize_login, Directory, UserRecord};
use crate::util::{
    blocking, constant_time_eq, html_escape, json_escape, parse_form, path_only,
    percent_decode_form, percent_encode_query_component, typed_login, validate_return_uri,
};

const HOME_PATH: &str = "/";
const LOGIN_PATH: &str = "/auth/login";
const LOGOUT_PATH: &str = "/auth/logout";
const VERIFY_PATH: &str = "/auth/verify";
const HEALTH_PATH: &str = "/healthz";
const SUCCESS_PATH: &str = "/auth/success";
const DENIED_PATH: &str = "/auth/denied";
const USERS_ADD_PATH: &str = "/auth/users/add";
const USERS_DELETE_PATH: &str = "/auth/users/delete";
const USERS_UPDATE_PATH: &str = "/auth/users/update";
const IMPERSONATE_PATH: &str = "/auth/users/impersonate";
const IMPERSONATE_END_PATH: &str = "/auth/users/impersonate-end";
/// Purpose of the anti-CSRF token of the account administration forms.
const USERS_TOKEN: &str = "users";
const LOGOUT_TOKEN: &str = "logout";

const STYLE_PATH: &str = "/auth/style.css";
const SETTINGS_STYLE_PATH: &str = "/auth/settings.css";
const THEME_JS_PATH: &str = "/auth/assets/theme.js";
const SETTINGS_JS_PATH: &str = "/auth/assets/settings.js";
const ICONS_PREFIX: &str = "/auth/icons/";
const GLOBAL_RATE_LIMIT_KEY: &str = "global";
/// With a shared database, how often at most a login the directory does not know makes a request
/// read the accounts again.
const UNKNOWN_LOGIN_REFRESH_GAP: Duration = Duration::from_secs(1);

type ResponseBody = Full<Bytes>;
type HttpResponse = Response<ResponseBody>;

/// What the portal knows about the browser behind one request.
struct Client {
    /// The browser used HTTPS: decides the cookie names and the `Secure` attribute.
    https: bool,
    ip: Option<IpAddr>,
    /// What a session issued to this client is tied to.
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

/// The account defined by the environment: never in the database, never deletable.
pub struct SuperAdmin {
    /// Lower case; what a login is compared with.
    pub login: String,
    pub password_hash: String,
}

pub struct AppState {
    pub config: Config,
    pub superadmin_login: String,
    pub superadmin_hash: String,
    /// Checked when the login does not exist, so an unknown login costs what a known one does.
    pub dummy_hash: String,
    pub signing_secret: Vec<u8>,
    pub sessions: SessionStore,
    pub users: Directory,
    pub store: Arc<Store>,
    pub tracker: Arc<Tracker>,
    pub ip_limiter: RateLimiter,
    pub global_limiter: RateLimiter,
    pub password_gate: Arc<Semaphore>,
    pub metrics: Metrics,
    /// The current service catalogue; discovery replaces the whole snapshot.
    pub catalog: RwLock<Arc<Catalog>>,
    pub discovery: Arc<discovery::Stats>,
    pub icons: Icons,
}

impl AppState {
    pub fn new(
        config: Config,
        superadmin: SuperAdmin,
        signing_secret: Vec<u8>,
        store: Arc<Store>,
        tracker: Arc<Tracker>,
    ) -> Result<Self, String> {
        let mut sessions = SessionStore::new(
            config.session.ttl,
            config.session.idle_timeout,
            config.session.max_active_sessions,
            Arc::clone(&tracker),
        );
        if store.is_shared() {
            sessions = sessions.shared(Arc::clone(&store));
        }
        let users = Directory::new(store.load_users()?, config.users.max_users);
        let dummy_hash = hash_password(b"a password nobody has chosen")?;
        Ok(Self {
            ip_limiter: RateLimiter::new(
                config.security.login_window,
                config.security.login_lockout,
                config.security.max_rate_limit_entries,
            ),
            global_limiter: RateLimiter::new(
                config.security.global_login_window,
                config.security.global_login_lockout,
                1,
            ),
            password_gate: Arc::new(Semaphore::new(
                config.security.max_concurrent_password_checks,
            )),
            metrics: Metrics::new(),
            catalog: RwLock::new(Arc::new(Catalog::default())),
            discovery: Arc::new(discovery::Stats::default()),
            icons: Icons::load(&config.ui.icons_dir),
            sessions,
            users,
            store,
            tracker,
            config,
            superadmin_login: superadmin.login,
            superadmin_hash: superadmin.password_hash,
            dummy_hash,
            signing_secret,
        })
    }
}

impl Drop for AppState {
    fn drop(&mut self) {
        self.superadmin_hash.zeroize();
        self.signing_secret.zeroize();
    }
}

pub fn serve(state: Arc<AppState>) -> Result<(), String> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(state.config.server.workers)
        .enable_io()
        .enable_time()
        .build()
        .map_err(|e| format!("cannot create Tokio runtime: {e}"))?;
    runtime.block_on(serve_async(state))
}

async fn serve_async(state: Arc<AppState>) -> Result<(), String> {
    let listener = TcpListener::bind(state.config.server.listen_address)
        .await
        .map_err(|e| {
            format!(
                "cannot listen on {}: {e}",
                state.config.server.listen_address
            )
        })?;
    let connection_gate = Arc::new(Semaphore::new(state.config.server.max_connections));

    let publisher = Arc::clone(&state);
    tokio::spawn(discovery::run(
        state.config.discovery.clone(),
        Arc::clone(&state.discovery),
        move |catalog| {
            *publisher.catalog.write().unwrap_or_else(|p| p.into_inner()) = Arc::new(catalog);
        },
    ));

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

            if let Err(error) = builder.serve_connection(io, service).await {
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
    let client_ip = client_ip(request.headers(), remote_addr.ip());
    let client = Client::new(&state, request.headers(), client_ip);

    let response = if state.config.metrics.enabled
        && method == Method::GET
        && path == state.config.metrics.path
    {
        handle_metrics(&state)
    } else {
        if path == VERIFY_PATH {
            handle_verify(&state, request.headers(), &client)
        } else if method == Method::GET && path == HEALTH_PATH {
            text_response(StatusCode::OK, "ok\n")
        } else if method == Method::GET && path == HOME_PATH {
            handle_home(&state, request.headers(), &client).await
        } else if method == Method::GET && path == STYLE_PATH {
            static_response("text/css; charset=utf-8", ui::CSS)
        } else if method == Method::GET && path.starts_with(ICONS_PREFIX) {
            handle_icon(&state, &path)
        } else if method == Method::GET && path == SETTINGS_STYLE_PATH {
            static_response("text/css; charset=utf-8", ui::SETTINGS_CSS)
        } else if method == Method::GET && path == THEME_JS_PATH {
            static_response("text/javascript; charset=utf-8", ui::THEME_JS)
        } else if method == Method::GET && path == SETTINGS_JS_PATH {
            static_response("text/javascript; charset=utf-8", ui::SETTINGS_JS)
        } else if method == Method::GET && is_settings_path(&path) {
            handle_settings(&state, &path, &uri, request.headers(), &client).await
        } else if method == Method::GET && path == SUCCESS_PATH {
            handle_success(&state, request.headers(), &client)
        } else if method == Method::GET && path == DENIED_PATH {
            handle_denied(&state, &uri, request.headers(), &client)
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
        } else if method == Method::GET && path == LOGIN_PATH {
            handle_login_get(&state, &uri, request.headers(), &client)
        } else if method == Method::POST && path == LOGIN_PATH {
            handle_login_post(&state, request, &client).await
        } else if method == Method::GET && path == LOGOUT_PATH {
            // Signing out is the POST of the launcher button; a plain visit changes nothing.
            redirect_response(StatusCode::SEE_OTHER, HOME_PATH)
        } else if method == Method::POST && path == LOGOUT_PATH {
            handle_logout_post(&state, request, &client).await
        } else {
            handle_not_found(&state, &method, &uri, request.headers(), &client)
        }
    };

    let status = response.status().as_u16();
    state.metrics.observe_http_request(
        method_metric,
        route_metric,
        status,
        started.elapsed(),
    );

    let request_log_level = if route_metric == "metrics" || route_metric == "health" {
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
    body.push_str(&state.metrics.render_accounts(
        state.users.count(),
        state.tracker.dropped(),
        state.tracker.flush_errors(),
    ));
    if state.store.is_shared() {
        body.push_str(
            &state
                .metrics
                .render_shared(state.sessions.shared_errors(), state.users.refresh_errors()),
        );
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
    if matches!(original_path, LOGIN_PATH | LOGOUT_PATH | STYLE_PATH | THEME_JS_PATH) {
        state.metrics.verify_allowed();
        return empty_response(StatusCode::NO_CONTENT);
    }

    // A path a service declared with `portal.exclude` goes through without a session (a webhook, a
    // health probe): its caller cannot sign in. Everything else needs one.
    let catalog = current_catalog(state);
    let method = unique_header_str(headers, "x-forwarded-method").ok().flatten();
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

    match access::decide(
        &catalog,
        &state.config.roles.names,
        viewer.roles.as_deref(),
        original_uri,
    ) {
        Decision::Allow { service } => {
            if let Some(id) = service {
                state
                    .tracker
                    .service_hit(viewer.session.sid, &id, ip_text(client), unix_now());
            }
            state.metrics.verify_allowed();
            empty_response(StatusCode::NO_CONTENT)
        }
        Decision::Deny { service } => {
            state.metrics.verify_denied();
            // A browser navigating gets the "access denied" page (the answer to ForwardAuth cannot
            // carry a page: Traefik caps its body); anything else gets a plain 403.
            let navigating = matches!(
                unique_header_str(headers, "x-forwarded-method"),
                Ok(Some("GET")) | Ok(Some("HEAD"))
            );
            if navigating {
                // The page names the service from the catalogue; the id is data, never text to show.
                let location = match service {
                    Some(id) => format!("{DENIED_PATH}?s={}", percent_encode_query_component(&id)),
                    None => DENIED_PATH.to_string(),
                };
                redirect_response(StatusCode::FOUND, &location)
            } else {
                text_response(StatusCode::FORBIDDEN, "Forbidden\n")
            }
        }
    }
}

/// Any other path. Without a session the visitor is sent to the login page whether or not the
/// path exists: signed out, a missing page cannot be told from a protected one.
fn handle_not_found(
    state: &AppState,
    method: &Method,
    uri: &str,
    headers: &HeaderMap,
    client: &Client,
) -> HttpResponse {
    if *method == Method::GET
        && validate_return_uri(uri, state.config.security.max_forwarded_uri_bytes)
        && !has_valid_session(state, headers, client)
    {
        if let Ok(location) = login_location(state, uri) {
            return redirect_response(StatusCode::FOUND, &location);
        }
    }
    if *method == Method::GET {
        html_response(
            StatusCode::NOT_FOUND,
            ui::message_page("Page not found", "There is nothing at this address.", "Back to panel"),
        )
    } else {
        text_response(StatusCode::NOT_FOUND, "Not Found\n")
    }
}

/// `/auth/denied`: where the gate sends a browser whose roles do not open the service it asked for.
fn handle_denied(state: &AppState, uri: &str, headers: &HeaderMap, client: &Client) -> HttpResponse {
    let Some(viewer) = current_viewer(state, headers, client) else {
        return redirect_response(StatusCode::SEE_OTHER, LOGIN_PATH);
    };
    let Ok(logout_token) = sign_action_token(&state.signing_secret, LOGOUT_TOKEN, &viewer.token) else {
        return text_response(StatusCode::INTERNAL_SERVER_ERROR, "authentication error\n");
    };
    // Only a service of the catalogue is ever named: an unknown id gets the generic sentence.
    let catalog = current_catalog(state);
    let service = query_value_up_to(uri, "s", 128).and_then(|id| {
        catalog
            .groups()
            .iter()
            .flat_map(|group| group.services.iter())
            .find(|service| service.id == id)
            .map(|service| service.name.clone())
    });
    let text = match service {
        Some(name) => format!(
            "You do not have access to {name}. Ask the administrator to give your account a role for it."
        ),
        None => "You do not have access to this service. Ask the administrator to give your account a role for it.".to_string(),
    };
    html_response(
        StatusCode::FORBIDDEN,
        ui::denied_page(&text, &viewer.login, &logout_token),
    )
}

/// The login URL that brings the browser back to `original_uri` once authenticated.
fn login_location(state: &AppState, original_uri: &str) -> Result<String, ()> {
    let signature = sign_return_uri(&state.signing_secret, original_uri).map_err(|_| ())?;
    Ok(format!(
        "{LOGIN_PATH}?rd={}&sig={}",
        percent_encode_query_component(original_uri),
        percent_encode_query_component(&signature)
    ))
}

/// `/auth/icons/<set>/<name>.svg`: one icon of the offline library.
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
    response.headers_mut().insert(CONTENT_TYPE, HeaderValue::from_static("image/svg+xml"));
    response.headers_mut().insert(
        HeaderName::from_static("cache-control"),
        HeaderValue::from_static("public, max-age=86400"),
    );
    apply_secure_headers(&mut response, false);
    // Opened on its own, an SVG could run script: the icons are data, so allow nothing.
    response.headers_mut().insert(
        HeaderName::from_static("content-security-policy"),
        HeaderValue::from_static("default-src 'none'; style-src 'unsafe-inline'; sandbox"),
    );
    response
}

/// The launcher. The portal checks the session itself (the route does not go through ForwardAuth).
async fn handle_home(
    state: &Arc<AppState>,
    headers: &HeaderMap,
    client: &Client,
) -> HttpResponse {
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

    let catalog = current_catalog(state);
    let registry = &state.config.roles.names;
    let shown = match &viewer.roles {
        Some(roles) => Arc::new(catalog.filtered(|s| service_allows(&s.roles, registry, Some(roles)))),
        None => Arc::clone(&catalog),
    };

    let page = ui::home_page(
        &shown,
        &state.icons,
        &ui::HomeContext {
            viewer: &viewer.login,
            impersonating: viewer.session.impersonated,
            logout_token: &logout_token,
            users_token: &users_token,
            hidden_services: catalog.service_count() - shown.service_count(),
            settings: is_superadmin(&viewer),
        },
    );
    html_response(StatusCode::OK, page)
}

/// The superadmin in its own right: not while browsing as a user.
fn is_superadmin(viewer: &Viewer) -> bool {
    viewer.session.principal == Principal::Superadmin && !viewer.session.impersonated
}

fn is_settings_path(path: &str) -> bool {
    matches!(
        path,
        ui::settings::USERS_PATH
            | ui::settings::ROLES_PATH
            | ui::settings::SERVICES_PATH
            | ui::settings::LOGINS_PATH
    )
}

/// One page of the settings, whose route is its tab. The portal checks the session itself (the route
/// does not go through ForwardAuth); to anyone but the superadmin the page does not exist.
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
    if !is_superadmin(&viewer) {
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
    };
    let catalog = current_catalog(state);
    let registry = &state.config.roles.names;
    let page = match path {
        ui::settings::ROLES_PATH => {
            let holders: Vec<(String, Vec<String>)> = state
                .users
                .list()
                .into_iter()
                .map(|(login, record)| (login, record.roles))
                .collect();
            ui::roles_page(
                &frame,
                &ui::RolesView {
                    registry,
                    catalog: &catalog,
                    holders: &holders,
                },
            )
        }
        ui::settings::SERVICES_PATH => ui::services_page(
            &frame,
            &ui::ServicesView {
                registry,
                catalog: &catalog,
            },
        ),
        ui::settings::LOGINS_PATH => {
            let only_failed = query_value(uri, "r").as_deref() == Some("failed");
            let asked = query_value(uri, "p")
                .and_then(|p| p.parse::<u32>().ok())
                .unwrap_or(1)
                .max(1)
                - 1;
            let data = load_login_page(state, asked, only_failed).await;
            ui::logins_page(
                &frame,
                &ui::LoginsView {
                    page: &data,
                    only_failed,
                    retention_days: state.config.storage.retention_days,
                },
            )
        }
        _ => {
            let (users, snapshot) = load_admin_data(state).await;
            let view = users_view(state, &catalog, uri, &users_token, users, snapshot);
            ui::users_page(&frame, &view)
        }
    };
    html_response(StatusCode::OK, page)
}

/// One page of the sign-in log; an unreadable database gives an empty page and a line in the logs.
async fn load_login_page(state: &Arc<AppState>, page: u32, only_failed: bool) -> LoginPage {
    let store = Arc::clone(&state.store);
    let now = unix_now();
    match tokio::task::spawn_blocking(move || store.login_attempts(now, page, only_failed)).await {
        Ok(Ok(data)) => data,
        Ok(Err(error)) => {
            log(state, LogLevel::Error, "history_unreadable", &[("error", error)]);
            LoginPage::default()
        }
        Err(_) => LoginPage::default(),
    }
}

/// The accounts and their history, for the Users page.
async fn load_admin_data(state: &Arc<AppState>) -> (Vec<(String, UserRecord)>, Snapshot) {
    let users = state.users.list();
    let mut accounts: Vec<(String, bool)> = users.iter().map(|(login, _)| (login.clone(), false)).collect();
    accounts.push((state.superadmin_login.clone(), true));
    let now = unix_now();
    let store = Arc::clone(&state.store);
    let asked = accounts.clone();
    let snapshot = match tokio::task::spawn_blocking(move || store.snapshot(&asked, now)).await {
        Ok(Ok(snapshot)) => snapshot,
        Ok(Err(error)) => {
            log(state, LogLevel::Error, "history_unreadable", &[("error", error)]);
            Snapshot::blank(&accounts, now)
        }
        Err(_) => Snapshot::blank(&accounts, now),
    };
    (users, snapshot)
}

fn users_view<'a>(
    state: &'a AppState,
    catalog: &'a Catalog,
    uri: &str,
    users_token: &'a str,
    users: Vec<(String, UserRecord)>,
    snapshot: Snapshot,
) -> ui::UsersView<'a> {
    let mut activities = snapshot.accounts.into_iter();
    let cards = users
        .into_iter()
        .map(|(login, record)| ui::UserCard {
            activity: activities.next().unwrap_or_default(),
            login,
            roles: record.roles,
            created_at: Some(record.created_at),
            superadmin: false,
        })
        .collect();
    ui::UsersView {
        notice: query_value(uri, "s").and_then(|code| ui::notice_for(&code)),
        registry: &state.config.roles.names,
        catalog,
        users: cards,
        superadmin: ui::UserCard {
            login: state.superadmin_login.clone(),
            roles: Vec::new(),
            created_at: None,
            superadmin: true,
            activity: activities.next().unwrap_or_default(),
        },
        days: snapshot.days,
        first_day: snapshot.first_day,
        recent_events: snapshot.recent_events,
        unknown_failures_30d: snapshot.unknown_failures_30d,
        max_users: state.config.users.max_users,
        token: users_token,
    }
}

/// One query parameter, decoded, short. Anything else is ignored.
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

fn current_catalog(state: &AppState) -> Arc<Catalog> {
    Arc::clone(&state.catalog.read().unwrap_or_else(|poisoned| poisoned.into_inner()))
}

/// A signed-in browser: the session its cookie stands for and what that session may do.
struct Viewer {
    session: Session,
    /// The cookie value (a secret): what the anti-CSRF tokens are bound to.
    token: Zeroizing<String>,
    /// The roles held; `None` for the superadmin, whom roles never hold back.
    roles: Option<Vec<String>>,
    login: String,
}

fn current_viewer(state: &AppState, headers: &HeaderMap, client: &Client) -> Option<Viewer> {
    let Some(token) = session_cookie(state, headers, client) else {
        state.metrics.session_missing();
        return None;
    };
    let Some(session) = state.sessions.validate(token, &client.binding) else {
        state.metrics.session_invalid();
        return None;
    };
    state.metrics.session_valid();
    let (roles, login) = match &session.principal {
        Principal::Superadmin => (None, state.superadmin_login.clone()),
        Principal::User(login) => match roles_of_known_user(state, login) {
            Some(roles) => (Some(roles), login.clone()),
            None => {
                // The account was deleted while this session was open.
                state.sessions.revoke(token, EndReason::Revoked);
                return None;
            }
        },
    };
    Some(Viewer {
        session,
        token: Zeroizing::new(token.to_string()),
        roles,
        login,
    })
}

/// The roles of an account, read again from the shared database first when the directory does not
/// know it: the account may have been created, and its session opened, on another instance a moment
/// ago, and a session must not be ended for that.
fn roles_of_known_user(state: &AppState, login: &str) -> Option<Vec<String>> {
    state.users.roles_of(login).or_else(|| {
        refresh_directory_if_due(state).then(|| state.users.roles_of(login)).flatten()
    })
}

/// A throttled read of the accounts from the shared database, for a request that met a login the
/// directory does not hold. Always `false` with one instance, which is the only writer.
fn refresh_directory_if_due(state: &AppState) -> bool {
    state.store.is_shared()
        && blocking(|| state.users.refresh_if_due(&state.store, UNKNOWN_LOGIN_REFRESH_GAP))
}

fn ip_text(client: &Client) -> Option<String> {
    client.ip.map(|ip| ip.to_string())
}

/// The browser's User-Agent for the history: bounded and free of control characters.
fn user_agent_text(headers: &HeaderMap) -> String {
    headers
        .get(USER_AGENT)
        .and_then(|value| value.to_str().ok())
        .map(|value| value.chars().filter(|c| !c.is_control()).take(160).collect())
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
                return text_response(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "authentication error\n",
                )
            }
        }
    };

    if has_valid_session(state, headers, client) {
        return redirect_response(StatusCode::SEE_OTHER, &rd);
    }

    // A browser that already holds a token keeps it: another request for the login page (a second tab,
    // a favicon fetch that Traefik sends through ForwardAuth, whose redirect lands here) must not
    // replace the cookie under the form that is open, or its submit fails the CSRF check.
    let csrf = match existing_csrf_token(state, headers, client) {
        Some(v) => v,
        None => match new_csrf_token() {
            Ok(v) => v,
            Err(_) => {
                return text_response(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "authentication error\n",
                )
            }
        },
    };
    login_page(state, client.https, &rd, &sig, &csrf, None, StatusCode::OK)
}

/// The anti-CSRF token the browser already holds, when it is one this portal issued (the shape of
/// [`new_csrf_token`]: 32 URL-safe base64 characters).
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
    html_response(
        StatusCode::OK,
        ui::auth_document(
            "Authenticated",
            r#"<h1>Authenticated</h1><p>The password was accepted and the session cookie is valid.</p><a class="button" href="/">Continue</a>"#,
        ),
    )
}

async fn handle_login_post(
    state: &Arc<AppState>,
    request: Request<Incoming>,
    client: &Client,
) -> HttpResponse {
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
        return text_response(StatusCode::FORBIDDEN, "Origin validation failed\n");
    }
    if !content_type_is_form(request.headers()) {
        state.metrics.login_invalid_request();
        return text_response(
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "Unsupported Media Type\n",
        );
    }

    let csrf_cookie = cookie_value(
        request.headers(),
        transport::csrf_cookie_name(client.https),
        state.config.security.max_cookie_header_bytes,
    )
    .map(str::to_string);
    let headers = request.headers().clone();

    let body = match read_limited_body(
        request.into_body(),
        state.config.server.max_body_bytes,
        state.config.server.body_read_timeout,
    )
    .await
    {
        Ok(v) => Zeroizing::new(v),
        Err(BodyReadError::TooLarge) => {
            state.metrics.login_invalid_request();
            return text_response(StatusCode::PAYLOAD_TOO_LARGE, "Payload Too Large\n");
        }
        Err(BodyReadError::Timeout) => {
            state.metrics.login_invalid_request();
            return text_response(StatusCode::REQUEST_TIMEOUT, "Request Timeout\n");
        }
        Err(BodyReadError::Io) => {
            state.metrics.login_invalid_request();
            return text_response(StatusCode::BAD_REQUEST, "Bad Request\n");
        }
    };

    let mut form = match parse_form(&body) {
        Ok(v) => v,
        Err(_) => {
            state.metrics.login_invalid_request();
            return text_response(StatusCode::BAD_REQUEST, "Bad Request\n");
        }
    };

    let login_input = form.remove("login").unwrap_or_default();
    let password = Zeroizing::new(form.remove("password").unwrap_or_default());
    let csrf = form.remove("csrf").unwrap_or_default();
    let rd = form.remove("rd").unwrap_or_else(|| "/".to_string());
    let sig = form.remove("sig").unwrap_or_default();

    if !form.is_empty()
        || password.len() > state.config.security.max_password_bytes
        || !validate_return_uri(&rd, state.config.security.max_forwarded_uri_bytes)
        || !verify_return_uri_signature(&state.signing_secret, &rd, &sig)
    {
        state.metrics.login_invalid_request();
        return text_response(StatusCode::BAD_REQUEST, "Bad Request\n");
    }

    if !verify_csrf(csrf_cookie.as_deref().unwrap_or(""), &csrf) {
        state.metrics.login_csrf_failure();
        return text_response(StatusCode::FORBIDDEN, "CSRF validation failed\n");
    }

    let client_ip_text = client_ip
        .map(|v| v.to_string())
        .unwrap_or_else(|| "unknown".into());
    let ip_key = format!("ip:{client_ip_text}");

    if let Some(response) = login_rate_limit_response(
        state,
        client.https,
        &ip_key,
        &client_ip_text,
        &rd,
        &sig,
    ) {
        return response;
    }

    let permit = match Arc::clone(&state.password_gate).acquire_owned().await {
        Ok(v) => v,
        Err(_) => {
            state.metrics.login_internal_error();
            return text_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "authentication error\n",
            );
        }
    };

    // Requests can queue behind the Argon2 semaphore. Re-check after acquiring
    // the permit so a burst that was admitted before the first failures were
    // recorded cannot drain the expensive password-check budget afterwards.
    if let Some(response) = login_rate_limit_response(
        state,
        client.https,
        &ip_key,
        &client_ip_text,
        &rd,
        &sig,
    ) {
        drop(permit);
        return response;
    }

    // Exactly one Argon2 check whoever the login names: the superadmin's hash, the account's, or a
    // dummy one when the login does not exist. Time and budget tell nothing about the login.
    let who = identify(state, &login_input);
    // What is kept of the attempt: the login as typed, made safe, and whether it names an account.
    // The password is not part of it.
    let typed = typed_login(&login_input);
    let known = !matches!(who, Who::Unknown);
    let target_hash = match &who {
        Who::Superadmin => state.superadmin_hash.clone(),
        Who::User(_, record) => record.password_hash.clone(),
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
            Ok(v) => v && !matches!(who, Who::Unknown),
            Err(_) => {
                state.metrics.login_internal_error();
                return text_response(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "authentication error\n",
                );
            }
        }
    };

    if !authenticated {
        if state.config.security.login_rate_limit_enabled {
            state.ip_limiter.failure(
                ip_key.clone(),
                state.config.security.login_attempts_per_ip,
            );
        }
        if state.config.security.global_login_rate_limit_enabled {
            state.global_limiter.failure(
                GLOBAL_RATE_LIMIT_KEY.to_string(),
                state.config.security.global_login_attempts,
            );
        }
        state.metrics.login_failure();
        log(
            state,
            LogLevel::Warn,
            "login_failed",
            &[
                ("client_ip", client_ip_text),
                ("login", typed.clone()),
                ("known", known.to_string()),
            ],
        );
        state.tracker.emit(Event::Login(LoginAttempt {
            login: typed,
            known,
            success: false,
            ip: ip_text(client),
            user_agent: user_agent_text(&headers),
            at: unix_now(),
        }));
        // The audit event names an account only when the typed login is one, so the per-account
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
                return text_response(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "authentication error\n",
                )
            }
        };
        return login_page(
            state,
            client.https,
            &rd,
            &sig,
            &csrf,
            Some("Invalid login or password"),
            StatusCode::UNAUTHORIZED,
        );
    }

    if state.config.security.login_rate_limit_enabled {
        state.ip_limiter.success(&ip_key);
    }

    let (principal, session_login) = match &who {
        Who::Superadmin => (Principal::Superadmin, state.superadmin_login.clone()),
        Who::User(login, _) => (Principal::User(login.clone()), login.clone()),
        Who::Unknown => unreachable!("an unknown login never authenticates"),
    };
    let token = Zeroizing::new(match state.sessions.issue(
        client.binding,
        NewSession {
            principal,
            login: session_login,
            impersonated: false,
            ip: ip_text(client),
            user_agent: user_agent_text(&headers),
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
            return text_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "authentication error\n",
            );
        }
        Err(SessionIssueError::Storage) => {
            state.metrics.login_internal_error();
            return text_response(StatusCode::SERVICE_UNAVAILABLE, "session store unavailable\n");
        }
    });

    let mut response = redirect_response(StatusCode::SEE_OTHER, &rd);
    let cookie = session_cookie_header(state, client, token.as_str());
    if append_dynamic_header(&mut response, SET_COOKIE, cookie.as_str()).is_err()
        || append_dynamic_header(&mut response, SET_COOKIE, &expired_csrf_cookie(state, client.https)).is_err()
    {
        state.sessions.revoke(token.as_str(), EndReason::Revoked);
        state.metrics.login_internal_error();
        return text_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            "authentication error\n",
        );
    }

    state.metrics.login_success();
    state.metrics.session_issued();
    state.tracker.emit(Event::Login(LoginAttempt {
        login: typed.clone(),
        known,
        success: true,
        ip: ip_text(client),
        user_agent: user_agent_text(&headers),
        at: unix_now(),
    }));
    log(
        state,
        LogLevel::Info,
        "login_success",
        &[("client_ip", client_ip_text), ("login", typed)],
    );
    response
}

/// Who a login names.
enum Who {
    Superadmin,
    User(String, UserRecord),
    Unknown,
}

impl Who {
    /// The account to record a failed attempt against; none when the login does not exist.
    fn login(&self, state: &AppState) -> Option<String> {
        match self {
            Self::Superadmin => Some(state.superadmin_login.clone()),
            Self::User(login, _) => Some(login.clone()),
            Self::Unknown => None,
        }
    }
}

fn identify(state: &AppState, typed: &str) -> Who {
    // Lower case and trimmed, as logins are stored. A name far longer than any login is unknown.
    let login = typed.trim().to_ascii_lowercase();
    if login.len() > 128 {
        return Who::Unknown;
    }
    if constant_time_eq(login.as_bytes(), state.superadmin_login.as_bytes()) {
        return Who::Superadmin;
    }
    match state.users.get(&login) {
        Some(record) => Some(record),
        // Maybe created a moment ago on another instance.
        None if refresh_directory_if_due(state) => state.users.get(&login),
        None => None,
    }
    .map_or(Who::Unknown, |record| Who::User(login, record))
}

/// The `Set-Cookie` value of a session.
fn session_cookie_header(state: &AppState, client: &Client, token: &str) -> Zeroizing<String> {
    Zeroizing::new(format!(
        "{}={}; {}",
        transport::session_cookie_name(&state.config.session.cookie_name, client.https),
        token,
        transport::cookie_attributes(
            client.https,
            state.config.session.same_site.as_str(),
            state.config.session.ttl.as_secs(),
        ),
    ))
}

fn login_rate_limit_response(
    state: &AppState,
    https: bool,
    ip_key: &str,
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
                https,
                rd,
                sig,
                "Too many sign-in attempts from this address. Try again later.",
                wait.as_secs().max(1),
            ));
        }
    }

    if state.config.security.global_login_rate_limit_enabled {
        if let Err(wait) = state.global_limiter.check(
            GLOBAL_RATE_LIMIT_KEY,
            state.config.security.global_login_attempts,
        ) {
            state.metrics.login_rate_limited_global();
            log(
                state,
                LogLevel::Warn,
                "login_rate_limited",
                &[
                    ("scope", "global".to_string()),
                    ("client_ip", client_ip_text.to_string()),
                    ("retry_after_seconds", wait.as_secs().max(1).to_string()),
                ],
            );
            return Some(rate_limited_login_page(
                state,
                https,
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
    https: bool,
    rd: &str,
    sig: &str,
    message: &str,
    retry_after: u64,
) -> HttpResponse {
    let csrf = match new_csrf_token() {
        Ok(v) => v,
        Err(_) => {
            return text_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "authentication error\n",
            )
        }
    };
    let mut response = login_page(
        state,
        https,
        rd,
        sig,
        &csrf,
        Some(message),
        StatusCode::TOO_MANY_REQUESTS,
    );
    if set_dynamic_header(&mut response, RETRY_AFTER, &retry_after.to_string()).is_err() {
        return text_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            "authentication error\n",
        );
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
    if !verify_action_token(&state.signing_secret, LOGOUT_TOKEN, session_token.as_str(), &csrf) {
        return text_response(StatusCode::FORBIDDEN, "CSRF validation failed\n");
    }

    state.sessions.revoke(session_token.as_str(), EndReason::Logout);
    let mut response = redirect_response(StatusCode::SEE_OTHER, "/");
    let expired_session = format!(
        "{}=; {}",
        transport::session_cookie_name(&state.config.session.cookie_name, client.https),
        transport::cookie_attributes(client.https, state.config.session.same_site.as_str(), 0),
    );
    if append_dynamic_header(&mut response, SET_COOKIE, &expired_session).is_err()
        || append_dynamic_header(&mut response, SET_COOKIE, &expired_csrf_cookie(state, client.https)).is_err()
    {
        return text_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            "authentication error\n",
        );
    }
    state.metrics.logout();
    response
}

// Account administration ----------------------------------------------------------------------

/// What every administration POST starts with: same origin, a form, a signed-in browser, a bounded
/// body, a valid anti-CSRF token. Returns the viewer and the remaining form fields, or the answer.
#[allow(clippy::result_large_err)] // the error is the response, sent as is
async fn admin_request(
    state: &Arc<AppState>,
    request: Request<Incoming>,
    client: &Client,
    action: &'static str,
) -> Result<(Viewer, std::collections::HashMap<String, String>), HttpResponse> {
    if !validate_post_origin(request.headers()) {
        state.metrics.users_origin_failure();
        log(
            state,
            LogLevel::Warn,
            "origin_check_failed",
            &[("route", "users".to_string())],
        );
        return Err(text_response(StatusCode::FORBIDDEN, "Origin validation failed\n"));
    }
    if !content_type_is_form(request.headers()) {
        return Err(text_response(
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "Unsupported Media Type\n",
        ));
    }
    let Some(viewer) = current_viewer(state, request.headers(), client) else {
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
            return Err(text_response(StatusCode::PAYLOAD_TOO_LARGE, "Payload Too Large\n"))
        }
        Err(BodyReadError::Timeout) => {
            return Err(text_response(StatusCode::REQUEST_TIMEOUT, "Request Timeout\n"))
        }
        Err(BodyReadError::Io) => return Err(text_response(StatusCode::BAD_REQUEST, "Bad Request\n")),
    };
    let mut form = match parse_form(&body) {
        Ok(v) => v,
        Err(_) => return Err(text_response(StatusCode::BAD_REQUEST, "Bad Request\n")),
    };
    let csrf = form.remove("csrf").unwrap_or_default();
    if !verify_action_token(&state.signing_secret, USERS_TOKEN, &viewer.token, &csrf) {
        state.metrics.user_admin(action, "forbidden");
        return Err(text_response(StatusCode::FORBIDDEN, "CSRF validation failed\n"));
    }
    Ok((viewer, form))
}

/// Only the superadmin in its own right (not while browsing as a user) manages accounts.
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

/// Back to the Users page with the outcome of the action (a code from a fixed list).
fn settings_redirect(code: &str) -> HttpResponse {
    redirect_response(
        StatusCode::SEE_OTHER,
        &format!("{}?s={code}", ui::settings::USERS_PATH),
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

async fn handle_users_add(
    state: &Arc<AppState>,
    request: Request<Incoming>,
    client: &Client,
) -> HttpResponse {
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
    // The role boxes are the remaining fields, named `r_<role>`.
    let mut roles: Vec<String> = Vec::new();
    for (name, value) in form.drain() {
        match name.strip_prefix("r_") {
            Some(role) if value == "1" => roles.push(role.to_string()),
            _ => return text_response(StatusCode::BAD_REQUEST, "Bad Request\n"),
        }
    }
    roles.sort();
    roles.dedup();

    let registry = &state.config.roles.names;
    let rejected = |code: &str| {
        state.metrics.user_admin("add", "rejected");
        settings_redirect(code)
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
    if roles.iter().any(|role| !registry.contains(role)) {
        return rejected("unknown-role");
    }
    if state.users.get(&login).is_some() {
        return rejected("exists");
    }
    if state.users.is_full() {
        return rejected("limit");
    }

    // Hashing costs what a login check costs: it takes the same permit.
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
        roles: roles.clone(),
        created_at: unix_now(),
    };
    let store = Arc::clone(&state.store);
    let created_at = row.created_at;
    match tokio::task::spawn_blocking(move || store.insert_user(&row)).await {
        Ok(Ok(())) => {}
        Ok(Err(InsertError::Exists)) => return rejected("exists"),
        Ok(Err(InsertError::Db(error))) => {
            log(state, LogLevel::Error, "user_insert_failed", &[("error", error)]);
            return rejected("error");
        }
        Err(_) => return rejected("error"),
    }
    state.users.insert(
        login.clone(),
        UserRecord {
            password_hash,
            roles,
            created_at,
        },
    );
    audit(
        state,
        AuditKind::UserAdded,
        Some(login),
        Some(state.superadmin_login.clone()),
        client,
        &headers,
    );
    state.metrics.user_admin("add", "ok");
    settings_redirect("user-added")
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

    let store = Arc::clone(&state.store);
    let target = login.clone();
    match tokio::task::spawn_blocking(move || store.remove_user(&target)).await {
        Ok(Ok(_)) => {}
        Ok(Err(error)) => {
            log(state, LogLevel::Error, "user_delete_failed", &[("error", error)]);
            state.metrics.user_admin("delete", "rejected");
            return settings_redirect("error");
        }
        Err(_) => return settings_redirect("error"),
    }
    state.users.remove(&login);
    state.sessions.revoke_user(&login, EndReason::UserDeleted);
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

/// Changes an account: its roles, and its password when a new one is given. The session of an
/// account whose password changed ends at once; a change of roles takes effect at the next request
/// (roles are read live).
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
    // Empty means "keep the password".
    let password = Zeroizing::new(form.remove("password").unwrap_or_default());
    let mut roles: Vec<String> = Vec::new();
    for (name, value) in form.drain() {
        match name.strip_prefix("r_") {
            Some(role) if value == "1" => roles.push(role.to_string()),
            _ => return text_response(StatusCode::BAD_REQUEST, "Bad Request\n"),
        }
    }
    roles.sort();
    roles.dedup();

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
    let registry = &state.config.roles.names;
    if roles.iter().any(|role| !registry.contains(role)) {
        return rejected("edit-unknown-role");
    }
    let change_password = !password.is_empty();
    if change_password {
        if let Err(code) = check_new_password(&password, state.config.security.max_password_bytes) {
            return rejected(&format!("edit-{code}"));
        }
    }
    let mut held = current.roles.clone();
    held.sort();
    let roles_changed = held != roles;
    if !change_password && !roles_changed {
        state.metrics.user_admin("update", "ok");
        return settings_redirect("unchanged");
    }

    let new_hash = if change_password {
        // Hashing costs what a login check costs: it takes the same permit.
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
    let (target, hash_for_store, roles_for_store) = (login.clone(), new_hash.clone(), roles.clone());
    match tokio::task::spawn_blocking(move || {
        store.update_user(&target, hash_for_store.as_deref(), &roles_for_store)
    })
    .await
    {
        Ok(Ok(true)) => {}
        Ok(Ok(false)) => return rejected("no-user"),
        Ok(Err(error)) => {
            log(state, LogLevel::Error, "user_update_failed", &[("error", error)]);
            return rejected("error");
        }
        Err(_) => return rejected("error"),
    }
    state.users.update(&login, new_hash, roles);
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
    if roles_changed {
        audit(
            state,
            AuditKind::UserUpdated,
            Some(login),
            Some(state.superadmin_login.clone()),
            client,
            &headers,
        );
    }
    state.metrics.user_admin("update", "ok");
    settings_redirect(if change_password { "password-reset" } else { "user-updated" })
}

/// "Log in as": the superadmin's session becomes a session of the user, flagged as impersonated.
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
    let response = switch_session(
        state,
        client,
        &headers,
        &viewer,
        Principal::User(login.clone()),
        login.clone(),
        true,
        redirect_response(StatusCode::SEE_OTHER, HOME_PATH),
    );
    if response.status() == StatusCode::SEE_OTHER {
        audit(
            state,
            AuditKind::ImpersonationStarted,
            Some(login),
            Some(state.superadmin_login.clone()),
            client,
            &headers,
        );
        state.metrics.user_admin("impersonate", "ok");
    }
    response
}

/// "Back to superadmin": ends the impersonated session and opens a fresh superadmin one. The
/// impersonated session itself proves it was opened by the superadmin: no password is asked.
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
        Principal::Superadmin,
        state.superadmin_login.clone(),
        false,
        redirect_response(StatusCode::SEE_OTHER, ui::settings::USERS_PATH),
    );
    if response.status() == StatusCode::SEE_OTHER {
        audit(
            state,
            AuditKind::ImpersonationEnded,
            Some(impersonated),
            Some(state.superadmin_login.clone()),
            client,
            &headers,
        );
        state.metrics.user_admin("impersonate_end", "ok");
    }
    response
}

/// Opens a session for `principal`, sets its cookie on `response`, and only then ends the current
/// one: a failure leaves the browser with the session it had.
#[allow(clippy::too_many_arguments)]
fn switch_session(
    state: &AppState,
    client: &Client,
    headers: &HeaderMap,
    current: &Viewer,
    principal: Principal,
    login: String,
    impersonated: bool,
    mut response: HttpResponse,
) -> HttpResponse {
    let token = match state.sessions.issue(
        client.binding,
        NewSession {
            principal,
            login,
            impersonated,
            ip: ip_text(client),
            user_agent: user_agent_text(headers),
        },
    ) {
        Ok(token) => Zeroizing::new(token),
        Err(SessionIssueError::Capacity) => {
            state.metrics.session_capacity_rejected();
            return text_response(StatusCode::SERVICE_UNAVAILABLE, "session capacity reached\n");
        }
        Err(SessionIssueError::Random) => {
            return text_response(StatusCode::INTERNAL_SERVER_ERROR, "authentication error\n");
        }
        Err(SessionIssueError::Storage) => {
            return text_response(StatusCode::SERVICE_UNAVAILABLE, "session store unavailable\n");
        }
    };
    let cookie = session_cookie_header(state, client, token.as_str());
    if append_dynamic_header(&mut response, SET_COOKIE, cookie.as_str()).is_err() {
        state.sessions.revoke(token.as_str(), EndReason::Revoked);
        return text_response(StatusCode::INTERNAL_SERVER_ERROR, "authentication error\n");
    }
    state
        .sessions
        .revoke(current.token.as_str(), EndReason::Impersonation);
    state.metrics.session_issued();
    response
}

fn login_page(
    state: &AppState,
    https: bool,
    rd: &str,
    sig: &str,
    csrf: &str,
    error: Option<&str>,
    status: StatusCode,
) -> HttpResponse {
    let error_html = error
        .map(|msg| {
            format!(
                r#"<div class="error" role="alert">{}</div>"#,
                html_escape(msg)
            )
        })
        .unwrap_or_default();
    let redirect_html = if rd != HOME_PATH {
        format!(
            r#"<p class="return-target">After authentication: <code>{}</code></p>"#,
            html_escape(rd)
        )
    } else {
        String::new()
    };
    let body = ui::auth_document(
        "Authentication",
        &format!(
            r#"{}<form method="post" action="/auth/login" autocomplete="on"><input type="hidden" name="csrf" value="{}"><input type="hidden" name="rd" value="{}"><input type="hidden" name="sig" value="{}"><label for="login">Login</label><input id="login" name="login" type="text" autocomplete="username" autocapitalize="off" spellcheck="false" maxlength="128" required autofocus><label for="password">Password</label><input id="password" name="password" type="password" autocomplete="current-password" maxlength="{}" required><button class="button button--primary" type="submit">Continue</button></form>{}"#,
            error_html,
            html_escape(csrf),
            html_escape(rd),
            html_escape(sig),
            state.config.security.max_password_bytes,
            redirect_html,
        ),
    );
    let mut response = html_response(status, body);
    if append_dynamic_header(&mut response, SET_COOKIE, &csrf_cookie(state, csrf, https)).is_err() {
        return text_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            "authentication error\n",
        );
    }
    response
}

/// The session cookie of the request, valid or not.
fn session_cookie<'a>(state: &AppState, headers: &'a HeaderMap, client: &Client) -> Option<&'a str> {
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
        Ok(Some(v)) => v
            .split(';')
            .next()
            .is_some_and(|v| v.trim().eq_ignore_ascii_case("application/x-www-form-urlencoded")),
        _ => false,
    }
}

fn unique_header_str<'a>(headers: &'a HeaderMap, name: &'static str) -> Result<Option<&'a str>, ()> {
    let name = HeaderName::from_static(name);
    unique_header_str_name(headers, name)
}

fn unique_header_str_name<'a>(headers: &'a HeaderMap, name: HeaderName) -> Result<Option<&'a str>, ()> {
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

fn client_ip(headers: &HeaderMap, remote_ip: IpAddr) -> Option<IpAddr> {
    if let Ok(Some(xff)) = unique_header_str(headers, "x-forwarded-for") {
        for part in xff.rsplit(',') {
            if let Ok(ip) = part.trim().parse::<IpAddr>() {
                return Some(ip);
            }
        }
    }
    Some(remote_ip)
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
    if origin_header {
        if uri.query().is_some() || !matches!(uri.path(), "" | "/") {
            return None;
        }
    }
    let host = authority.host().to_ascii_lowercase();
    if host.is_empty() {
        return None;
    }
    let port = authority.port_u16().unwrap_or(if scheme == "https" { 443 } else { 80 });
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
        match key.as_str() {
            "rd" => {
                if rd.replace(value).is_some() {
                    return Err("duplicate rd query parameter".into());
                }
            }
            "sig" => {
                if sig.replace(value).is_some() {
                    return Err("duplicate sig query parameter".into());
                }
            }
            _ => {}
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

async fn read_limited_body_inner(
    mut body: Incoming,
    max: usize,
) -> Result<Vec<u8>, BodyReadError> {
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

fn html_response(status: StatusCode, body: String) -> HttpResponse {
    let mut response = response_with_body(status, Bytes::from(body));
    response.headers_mut().insert(
        CONTENT_TYPE,
        HeaderValue::from_static("text/html; charset=utf-8"),
    );
    apply_secure_headers(&mut response, true);
    response
}

/// An embedded asset (stylesheet, script). Content never varies per request, so it can be cached.
fn static_response(content_type: &'static str, body: &str) -> HttpResponse {
    let mut response = response_with_body(StatusCode::OK, Bytes::copy_from_slice(body.as_bytes()));
    response.headers_mut().insert(
        CONTENT_TYPE,
        HeaderValue::from_static(content_type),
    );
    response.headers_mut().insert(
        HeaderName::from_static("cache-control"),
        HeaderValue::from_static("public, max-age=3600"),
    );
    apply_secure_headers(&mut response, false);
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
            "default-src 'none'; style-src 'self'; script-src 'self'; img-src 'self'; form-action 'self'; frame-ancestors 'none'; base-uri 'none'; object-src 'none'",
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

fn csrf_cookie(state: &AppState, csrf: &str, https: bool) -> String {
    format!(
        "{}={csrf}; {}",
        transport::csrf_cookie_name(https),
        transport::cookie_attributes(https, state.config.session.same_site.as_str(), 600)
    )
}

fn expired_csrf_cookie(state: &AppState, https: bool) -> String {
    format!(
        "{}=; {}",
        transport::csrf_cookie_name(https),
        transport::cookie_attributes(https, state.config.session.same_site.as_str(), 0)
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
        HEALTH_PATH => "health",
        STYLE_PATH | SETTINGS_STYLE_PATH | THEME_JS_PATH | SETTINGS_JS_PATH => "assets",
        _ if is_settings_path(path) => "settings",
        _ if path.starts_with(ICONS_PREFIX) => "icons",
        SUCCESS_PATH => "success",
        LOGIN_PATH => "login",
        LOGOUT_PATH => "logout",
        DENIED_PATH => "denied",
        USERS_ADD_PATH => "users_add",
        USERS_DELETE_PATH => "users_delete",
        USERS_UPDATE_PATH => "users_update",
        IMPERSONATE_PATH => "impersonate",
        IMPERSONATE_END_PATH => "impersonate_end",
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

#[cfg(test)]
mod cluster_tests;

#[cfg(test)]
mod tests {
    use super::*;

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
        assert_eq!(csrf_shape(&issued), Some(issued), "what new_csrf_token issues is accepted");
    }

    #[test]
    fn query_return_pair_rejects_duplicates_and_bad_encoding() {
        assert!(query_return_pair("/auth/login?rd=%2F&rd=%2Fx&sig=a").is_err());
        assert!(query_return_pair("/auth/login?rd=%ZZ&sig=a").is_err());
    }

    #[test]
    fn query_return_pair_decodes_expected_fields() {
        let (rd, sig) =
            query_return_pair("/auth/login?rd=%2Fgrafana%3Fx%3D1&sig=abc").unwrap();
        assert_eq!(rd.as_deref(), Some("/grafana?x=1"));
        assert_eq!(sig.as_deref(), Some("abc"));
    }

    #[test]
    fn origin_accepts_matching_direct_origin() {
        let h = headers(&[("host", "localhost:4181"), ("origin", "http://localhost:4181")]);
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
