//! The second factor in the pages: the second step of the sign-in (`POST /auth/login/code`), the forced enrolment
//! (`POST /auth/login/enrol`), the section "Security" of the settings and its forms, and the reset of the second factor
//! of an account by the superadmin.
//!
//! The state between the password step and the second step is a signed token in a hidden field (see [`crate::mfa`]).
//! No instance keeps a state. The counters of the failed codes are in the database, and the counter of an address is
//! the limiter of the sign-in, with its own key.

use super::*;
use crate::mfa::{self, Enrolled, Pending, PendingError, Stage, Verdict};
use crate::ui::mfa as ui_mfa;

/// The purpose of the anti-forgery tokens of the forms of the section "Security".
pub(super) const SECURITY_TOKEN: &str = "security";

/// The login that the second factor of a principal is kept under.
fn totp_login(state: &AppState, principal: &Principal) -> String {
    match principal {
        Principal::Superadmin => state.superadmin_login.clone(),
        Principal::User(login) => login.clone(),
    }
}

/// The key of the limiter of the second-factor attempts of one address.
fn address_key(client: &Client) -> String {
    format!(
        "mfa:{}",
        client
            .ip
            .map(|v| v.to_string())
            .unwrap_or_else(|| "unknown".into())
    )
}

fn internal_error() -> HttpResponse {
    text_response(StatusCode::INTERNAL_SERVER_ERROR, "authentication error\n")
}

// The sign-in -----------------------------------------------------------------------------------

/// What follows a good password. `None`: the account opens its session now. `Some`: the page of the second step, or
/// an error. A policy `off` never gives a page. `credential` is the stamp of the credential that the password step
/// checked. The pending token holds it with the second factor of the account (see [`mfa::proof_of`]).
#[allow(clippy::too_many_arguments)]
pub(super) async fn after_password(
    state: &Arc<AppState>,
    client: &Client,
    principal: &Principal,
    credential: &Credential,
    keep: bool,
    csrf: &str,
    rd: &str,
    sig: &str,
) -> Option<HttpResponse> {
    state.mfa.as_ref()?;
    let login = totp_login(state, principal);
    let store = Arc::clone(&state.store);
    let row = match tokio::task::spawn_blocking(move || store.totp_get(&login)).await {
        Ok(Ok(row)) => row,
        Ok(Err(error)) => {
            log(
                state,
                LogLevel::Error,
                "mfa_read_failed",
                &[("error", error)],
            );
            state.metrics.login_internal_error();
            return Some(text_response(
                StatusCode::SERVICE_UNAVAILABLE,
                "session store unavailable\n",
            ));
        }
        Err(_) => return Some(internal_error()),
    };
    let stage = if row.is_some() {
        Stage::Code
    } else if state.config.authentication.mfa == crate::config::MfaPolicy::Required {
        Stage::Enrol
    } else {
        return None;
    };
    let Ok(proof) = mfa::proof_of(&state.signing_secret, credential, row.as_ref()) else {
        return Some(internal_error());
    };
    let pending = Pending {
        stage,
        principal: principal.clone(),
        keep,
        tries: 0,
        issued: unix_now(),
        proof,
    };
    Some(step_page(
        state,
        client,
        &pending,
        &Posted {
            csrf,
            rd,
            sig,
            enrol: None,
        },
        None,
        StatusCode::OK,
    ))
}

/// The fields that a page of the second step posts again.
struct Posted<'a> {
    csrf: &'a str,
    rd: &'a str,
    sig: &'a str,
    /// The token of the enrolment that the page shows, when the person has one already.
    enrol: Option<&'a str>,
}

/// The page of the second step for `pending`. It keeps the anti-CSRF token of the first page: the pending token is bound
/// to it, and the cookie of the browser holds it.
fn step_page(
    state: &AppState,
    client: &Client,
    pending: &Pending,
    posted: &Posted<'_>,
    error: Option<&str>,
    status: StatusCode,
) -> HttpResponse {
    let Some(mfa) = state.mfa.as_ref() else {
        return internal_error();
    };
    let Ok(token) = mfa::sign_pending(&state.signing_secret, pending, &client.binding, posted.csrf)
    else {
        return internal_error();
    };
    let fields = ui_mfa::Fields {
        csrf: posted.csrf,
        rd: posted.rd,
        sig: posted.sig,
        pending: &token,
    };
    let body = match pending.stage {
        Stage::Code => ui_mfa::code_card(&fields, error),
        Stage::Enrol => {
            let login = totp_login(state, &pending.principal);
            let now = unix_now();
            let issuer = &state.config.server.name;
            let page = posted
                .enrol
                .and_then(|token| mfa.resume(&login, token, issuer, now))
                .map(Ok)
                .unwrap_or_else(|| mfa.begin(&login, issuer, now));
            match page {
                Ok(page) => ui_mfa::enrol_card(&fields, &page, error),
                Err(_) => return internal_error(),
            }
        }
    };
    let mut response = masked_response(state, status, body);
    if append_dynamic_header(
        &mut response,
        SET_COOKIE,
        &csrf_cookie(state, posted.csrf, client.https),
    )
    .is_err()
    {
        return internal_error();
    }
    response
}

/// The answer to a second step that cannot go on: the person starts again at the password step.
fn start_again(
    state: &AppState,
    client: &Client,
    rd: &str,
    sig: &str,
    message: &str,
) -> HttpResponse {
    let Ok(fresh) = new_csrf_token() else {
        return internal_error();
    };
    login_page(
        state,
        client,
        rd,
        sig,
        &fresh,
        Some(message),
        StatusCode::FORBIDDEN,
    )
}

/// What a post of the second step holds, after the checks that it needs.
struct Step {
    headers: HeaderMap,
    pending: Pending,
    /// The stamp of the credential that the password step checked. It is the stamp of now.
    credential: Credential,
    csrf: String,
    rd: String,
    sig: String,
    code: String,
    /// The token of the enrolment. Only the forced enrolment has it.
    enrol: Option<String>,
}

/// The checks that both second steps share: the form, the return target, the anti-CSRF token and the pending token.
/// A pending token that is damaged, old, or made for another browser or page gives the page "open too long": the person
/// cannot tell these causes apart. The same page answers a token whose account changed its credential or its second
/// factor after the password step. Thus a new password refuses every pending token made before it. A reset of the
/// second factor, a new enrolment and a new superadmin credential do the same.
#[allow(clippy::result_large_err)] // the error is the response, sent as is
async fn read_step(
    state: &Arc<AppState>,
    request: Request<Incoming>,
    client: &Client,
    stage: Stage,
) -> Result<Step, HttpResponse> {
    let LoginForm {
        headers,
        csrf_cookie,
        mut form,
        body_permit,
        ..
    } = login_form(state, request, client).await?;
    // The second step runs no Argon2 check: the body permit goes back now.
    drop(body_permit);
    let csrf = form.remove("csrf").unwrap_or_default();
    let rd = form.remove("rd").unwrap_or_else(|| "/".to_string());
    let sig = form.remove("sig").unwrap_or_default();
    let token = form.remove("pending").unwrap_or_default();
    let code = form.remove("code").unwrap_or_default();
    let enrol = form.remove("enrol");
    if !form.is_empty()
        || code.len() > 64
        || !validate_return_uri(&rd, state.config.security.max_forwarded_uri_bytes)
        || !verify_return_uri_signature(&state.signing_secret, &rd, &sig)
    {
        state.metrics.login_invalid_request();
        return Err(text_response(StatusCode::BAD_REQUEST, "Bad Request\n"));
    }
    let stale = |state: &AppState| {
        state.metrics.login_csrf_failure();
        start_again(
            state,
            client,
            &rd,
            &sig,
            "The page was open too long. Enter your login and password again.",
        )
    };
    if !verify_csrf(csrf_cookie.as_deref().unwrap_or(""), &csrf) {
        return Err(stale(state));
    }
    let pending = match mfa::open_pending(
        &state.signing_secret,
        &token,
        &client.binding,
        &csrf,
        unix_now(),
    ) {
        Ok(pending) if pending.stage == stage => pending,
        Ok(_) | Err(PendingError::Invalid | PendingError::Expired) => return Err(stale(state)),
    };
    // The password step proved a credential and saw a second factor. Both must be the ones of now. The accounts of
    // the shared database are read as the password step reads them.
    refresh_directory_if_due(state);
    let row = match read_row(state, &totp_login(state, &pending.principal)).await {
        Ok(row) => row,
        Err(error) => {
            log(
                state,
                LogLevel::Error,
                "mfa_read_failed",
                &[("error", error)],
            );
            state.metrics.login_internal_error();
            return Err(text_response(
                StatusCode::SERVICE_UNAVAILABLE,
                "session store unavailable\n",
            ));
        }
    };
    let mut credential = None;
    let fresh = fresh_against_directory(state, |state| {
        credential = credential_stamp(state, &pending.principal).filter(|now| {
            mfa::proof_of(&state.signing_secret, now, row.as_ref())
                .is_ok_and(|proof| constant_time_eq(&proof, &pending.proof))
        });
        credential.is_some()
    });
    let credential = match (fresh, credential) {
        (Fresh::Current, Some(credential)) => credential,
        (Fresh::Unknown, _) => {
            state.metrics.login_internal_error();
            return Err(text_response(
                StatusCode::SERVICE_UNAVAILABLE,
                "session store unavailable\n",
            ));
        }
        _ => return Err(stale(state)),
    };
    Ok(Step {
        headers,
        pending,
        credential,
        csrf,
        rd,
        sig,
        code,
        enrol,
    })
}

/// The refusal of a second step that a limiter blocks: the same page, status 429, `Retry-After`.
fn limited_step(state: &AppState, client: &Client, step: &Step, wait_secs: u64) -> HttpResponse {
    state.metrics.login_mfa_failure();
    let mut response = step_page(
        state,
        client,
        &step.pending,
        &Posted {
            csrf: &step.csrf,
            rd: &step.rd,
            sig: &step.sig,
            enrol: step.enrol.as_deref(),
        },
        Some("Too many attempts. Try again later."),
        StatusCode::TOO_MANY_REQUESTS,
    );
    if set_dynamic_header(&mut response, RETRY_AFTER, &wait_secs.max(1).to_string()).is_err() {
        return internal_error();
    }
    response
}

/// Counts an attempt of the second factor against the address, and checks the limit in the same step. The answer is
/// the time to wait when the address is blocked: the attempt then does not go on. Attempts that arrive at the same
/// time therefore never pass the limit. A good code or a good proof clears the count (see [`address_success`]).
fn address_acquire(state: &AppState, client: &Client) -> Option<Duration> {
    if !state.config.security.login_rate_limit_enabled {
        return None;
    }
    state
        .ip_limiter
        .acquire(
            &address_key(client),
            state.config.security.login_attempts_per_ip,
        )
        .err()
}

/// A good code or a good proof: the attempts of the second factor of the address end. The failures of the password
/// step of the address stay: they can name other logins, and they expire with `security.login_window`.
fn address_success(state: &AppState, client: &Client) {
    if state.config.security.login_rate_limit_enabled {
        state.ip_limiter.success(&address_key(client));
    }
}

/// The log, the metric, the audit event and the sign-in list for a wrong code.
fn record_wrong_code(state: &AppState, client: &Client, step: &Step, login: &str) {
    state.metrics.login_mfa_failure();
    log(
        state,
        LogLevel::Warn,
        "login_failed",
        &[
            (
                "client_ip",
                client
                    .ip
                    .map(|v| v.to_string())
                    .unwrap_or_else(|| "unknown".into()),
            ),
            ("login", login.to_string()),
            ("known", "true".to_string()),
            ("reason", "mfa".to_string()),
        ],
    );
    state.tracker.emit(Event::Login(LoginAttempt {
        login: login.to_string(),
        known: true,
        success: false,
        ip: ip_text(client),
        user_agent: user_agent_text(&step.headers),
        at: unix_now(),
    }));
    audit(
        state,
        AuditKind::MfaFailed,
        Some(login.to_string()),
        None,
        client,
        &step.headers,
    );
}

/// The person gave a wrong code: the same card again with a new token that counts the try, or the password step when the
/// token has no try left.
fn wrong_code(state: &AppState, client: &Client, step: &Step, message: &str) -> HttpResponse {
    let tries = step.pending.tries.saturating_add(1);
    if tries >= mfa::MAX_TRIES {
        let Ok(fresh) = new_csrf_token() else {
            return internal_error();
        };
        return login_page(
            state,
            client,
            &step.rd,
            &step.sig,
            &fresh,
            Some("Too many wrong codes. Enter your login and password again."),
            StatusCode::UNAUTHORIZED,
        );
    }
    let pending = Pending {
        tries,
        ..step.pending.clone()
    };
    step_page(
        state,
        client,
        &pending,
        &Posted {
            csrf: &step.csrf,
            rd: &step.rd,
            sig: &step.sig,
            enrol: step.enrol.as_deref(),
        },
        Some(message),
        StatusCode::UNAUTHORIZED,
    )
}

fn signed_in(step: &Step, login: &str) -> SignedIn {
    SignedIn {
        principal: step.pending.principal.clone(),
        credential: step.credential,
        session_login: login.to_string(),
        typed: typed_login(login),
        known: true,
        keep: step.pending.keep,
        mfa: true,
    }
}

/// `POST /auth/login/code`: the second step of an account that has a second factor.
pub(super) async fn handle_code_post(
    state: &Arc<AppState>,
    request: Request<Incoming>,
    client: &Client,
) -> HttpResponse {
    if state.mfa.is_none() {
        return handle_not_found_simple();
    }
    let step = match read_step(state, request, client, Stage::Code).await {
        Ok(step) => step,
        Err(response) => return response,
    };
    let login = totp_login(state, &step.pending.principal);
    if let Some(wait) = address_acquire(state, client) {
        log(
            state,
            LogLevel::Warn,
            "login_rate_limited",
            &[("scope", "mfa".to_string())],
        );
        return limited_step(state, client, &step, wait.as_secs());
    }
    let (worker, name, code) = (Arc::clone(state), login.clone(), step.code.clone());
    let verdict = tokio::task::spawn_blocking(move || {
        let mfa = worker.mfa.as_ref().ok_or("the second factor is off")?;
        mfa.verify(&worker.store, &name, &code, unix_now())
    })
    .await;
    let verdict = match verdict {
        Ok(Ok(verdict)) => verdict,
        Ok(Err(error)) => {
            log(
                state,
                LogLevel::Error,
                "mfa_check_failed",
                &[("error", error)],
            );
            state.metrics.login_internal_error();
            return internal_error();
        }
        Err(_) => return internal_error(),
    };
    match verdict {
        Verdict::Totp | Verdict::Recovery => {
            address_success(state, client);
            if verdict == Verdict::Recovery {
                audit(
                    state,
                    AuditKind::RecoveryUsed,
                    Some(login.clone()),
                    None,
                    client,
                    &step.headers,
                );
            }
            finish_sign_in(
                state,
                client,
                &step.headers,
                &step.rd,
                signed_in(&step, &login),
                Landing::Redirect,
            )
        }
        Verdict::Wrong { locked_until } => {
            record_wrong_code(state, client, &step, &login);
            match locked_until {
                Some(until) => limited_step(
                    state,
                    client,
                    &step,
                    u64::try_from(until - unix_now()).unwrap_or(1),
                ),
                None => wrong_code(state, client, &step, ui_mfa::INVALID_CODE),
            }
        }
        Verdict::Locked { until } => {
            record_wrong_code(state, client, &step, &login);
            limited_step(
                state,
                client,
                &step,
                u64::try_from(until - unix_now()).unwrap_or(1),
            )
        }
        // The second factor of the account went away after the check of the pending token: an administrator reset it.
        // The proof of the password step is stale, as for a page that was open too long.
        Verdict::NotEnrolled => start_again(
            state,
            client,
            &step.rd,
            &step.sig,
            "The page was open too long. Enter your login and password again.",
        ),
    }
}

fn handle_not_found_simple() -> HttpResponse {
    text_response(StatusCode::NOT_FOUND, "Not Found\n")
}

/// `POST /auth/login/enrol`: the forced enrolment. The session starts only after a good code.
pub(super) async fn handle_enrol_post(
    state: &Arc<AppState>,
    request: Request<Incoming>,
    client: &Client,
) -> HttpResponse {
    if state.mfa.is_none() {
        return handle_not_found_simple();
    }
    let step = match read_step(state, request, client, Stage::Enrol).await {
        Ok(step) => step,
        Err(response) => return response,
    };
    let login = totp_login(state, &step.pending.principal);
    if let Some(wait) = address_acquire(state, client) {
        return limited_step(state, client, &step, wait.as_secs());
    }
    let enrol = step.enrol.clone().unwrap_or_default();
    let (worker, name, code) = (Arc::clone(state), login.clone(), step.code.clone());
    let outcome = tokio::task::spawn_blocking(move || {
        let mfa = worker.mfa.as_ref().ok_or("the second factor is off")?;
        mfa.confirm(&worker.store, &name, &enrol, &code, unix_now())
    })
    .await;
    let outcome = match outcome {
        Ok(Ok(outcome)) => outcome,
        Ok(Err(error)) => {
            log(
                state,
                LogLevel::Error,
                "mfa_enrol_failed",
                &[("error", error)],
            );
            state.metrics.login_internal_error();
            return internal_error();
        }
        Err(_) => return internal_error(),
    };
    match outcome {
        Enrolled::Done(codes) => {
            address_success(state, client);
            audit(
                state,
                AuditKind::TotpEnabled,
                Some(login.clone()),
                Some(login.clone()),
                client,
                &step.headers,
            );
            finish_sign_in(
                state,
                client,
                &step.headers,
                &step.rd,
                signed_in(&step, &login),
                Landing::Codes(codes),
            )
        }
        Enrolled::WrongCode => {
            record_wrong_code(state, client, &step, &login);
            wrong_code(state, client, &step, ui_mfa::INVALID_CODE)
        }
        // A page that nobody made, or an old page, or an account that has a second factor already.
        Enrolled::BadPage | Enrolled::Already => start_again(
            state,
            client,
            &step.rd,
            &step.sig,
            "The page was open too long. Enter your login and password again.",
        ),
    }
}

// The section "Security" -----------------------------------------------------------------------

/// Whether the viewer sees the tab "Security": the second factor is on, and the session is not one of "Log in as".
pub(super) fn tab_visible(state: &AppState, viewer: &Viewer) -> bool {
    state.mfa.is_some() && !viewer.session.impersonated
}

async fn read_row(
    state: &Arc<AppState>,
    login: &str,
) -> Result<Option<crate::store::TotpRow>, String> {
    let store = Arc::clone(&state.store);
    let login = login.to_string();
    tokio::task::spawn_blocking(move || store.totp_get(&login))
        .await
        .map_err(|e| e.to_string())?
}

/// The logins that have a second factor, for the pages of the accounts. `None`: the second factor is off, or the
/// database cannot be read.
pub(super) async fn enabled_logins(
    state: &Arc<AppState>,
) -> Option<std::collections::HashSet<String>> {
    state.mfa.as_ref()?;
    let store = Arc::clone(&state.store);
    match tokio::task::spawn_blocking(move || store.totp_logins()).await {
        Ok(Ok(logins)) => Some(logins.into_iter().collect()),
        Ok(Err(error)) => {
            log(
                state,
                LogLevel::Error,
                "mfa_read_failed",
                &[("error", error)],
            );
            None
        }
        Err(_) => None,
    }
}

/// One page of the section, for the viewer. The tokens and the frame are made here.
async fn render(
    state: &Arc<AppState>,
    viewer: &Viewer,
    notice: Option<ui::settings::Notice>,
    pick: impl FnOnce(Option<&crate::store::TotpRow>) -> Pick,
    row: Option<crate::store::TotpRow>,
    status: StatusCode,
) -> HttpResponse {
    let (Ok(logout_token), Ok(users_token), Ok(token)) = (
        sign_action_token(&state.signing_secret, LOGOUT_TOKEN, &viewer.token),
        sign_action_token(&state.signing_secret, USERS_TOKEN, &viewer.token),
        sign_action_token(&state.signing_secret, SECURITY_TOKEN, &viewer.token),
    ) else {
        return internal_error();
    };
    let _ = users_token;
    let frame = ui::Frame {
        logout_token: &logout_token,
        keys: keys_page::list_visible(state, viewer),
        inbox: inbox_page::tab_visible(state, viewer),
        superadmin: is_superadmin(viewer),
        notify: inbox_page::notify_header(state, viewer).await,
        security: true,
        links: links_page::tab_visible(state, viewer),
        tab: state.tab_look(),
    };
    let picked = pick(row.as_ref());
    let required = state.config.authentication.mfa == crate::config::MfaPolicy::Required;
    let ui_state = match &picked {
        Pick::Off => ui_mfa::State::Off,
        Pick::Enrol(page) => ui_mfa::State::Enrol { enrolment: page },
        Pick::On => match &row {
            Some(row) => ui_mfa::State::On {
                since: row.created_at,
                recovery_left: row.recovery_left,
                locked_on: required,
            },
            None => ui_mfa::State::Off,
        },
        Pick::Codes(codes) => ui_mfa::State::Codes { codes },
    };
    let page = ui_mfa::security_page(
        &frame,
        &ui_mfa::SecurityView {
            notice,
            token: &token,
            state: ui_state,
        },
    );
    html_response(status, page)
}

/// What a page of the section shows.
enum Pick {
    Off,
    Enrol(mfa::Enrolment),
    On,
    Codes(Vec<String>),
}

/// `GET /auth/settings/security`.
pub(super) async fn handle_security_get(
    state: &Arc<AppState>,
    uri: &str,
    viewer: &Viewer,
) -> HttpResponse {
    let Some(mfa) = state.mfa.as_ref() else {
        return handle_not_found_simple();
    };
    let row = match read_row(state, &viewer.login).await {
        Ok(row) => row,
        Err(error) => {
            log(
                state,
                LogLevel::Error,
                "mfa_read_failed",
                &[("error", error)],
            );
            return text_response(
                StatusCode::SERVICE_UNAVAILABLE,
                "session store unavailable\n",
            );
        }
    };
    let notice = query_value(uri, "s").and_then(|code| ui::notice_for(&code));
    let want_enrol = query_value(uri, "enable").as_deref() == Some("1") && row.is_none();
    let page = if want_enrol {
        match mfa.begin(&viewer.login, &state.config.server.name, unix_now()) {
            Ok(page) => Some(page),
            Err(_) => return internal_error(),
        }
    } else {
        None
    };
    render(
        state,
        viewer,
        notice,
        |row| match (page, row) {
            (Some(page), _) => Pick::Enrol(page),
            (None, Some(_)) => Pick::On,
            (None, None) => Pick::Off,
        },
        row,
        StatusCode::OK,
    )
    .await
}

fn security_redirect(code: &str) -> HttpResponse {
    redirect_response(
        StatusCode::SEE_OTHER,
        &format!("{}?s={code}", ui_mfa::SECURITY_PATH),
    )
}

/// The checks of a form of the section: the second factor is on, the session is the person's own, and the token is good.
#[allow(clippy::result_large_err)] // the error is the response, sent as is
async fn security_request(
    state: &Arc<AppState>,
    request: Request<Incoming>,
    client: &Client,
    action: &'static str,
) -> Result<(Viewer, std::collections::HashMap<String, String>, HeaderMap), HttpResponse> {
    let headers = request.headers().clone();
    let (viewer, form) =
        form_request(state, request, client, action, SECURITY_TOKEN, 0, false).await?;
    if !tab_visible(state, &viewer) {
        state.metrics.user_admin(action, "forbidden");
        return Err(text_response(StatusCode::FORBIDDEN, "Forbidden\n"));
    }
    Ok((viewer, form, headers))
}

/// The password of the viewer, checked as a sign-in checks it: one Argon2 check, behind the same gate.
async fn password_is_right(
    state: &Arc<AppState>,
    viewer: &Viewer,
    password: Zeroizing<String>,
) -> Option<bool> {
    if password.len() > state.config.security.max_password_bytes {
        return Some(false);
    }
    let known = identify(state, &viewer.login);
    let hash = match &known {
        Who::Superadmin => state.superadmin_hash.clone(),
        Who::User(_, record, _) => record.password_hash.clone(),
        Who::Unknown => state.dummy_hash.clone(),
    };
    let permit = Arc::clone(&state.password_gate)
        .acquire_owned()
        .await
        .ok()?;
    let ok = tokio::task::spawn_blocking(move || {
        let _permit = permit;
        verify_password(password.as_bytes(), &hash)
    })
    .await
    .ok()?;
    Some(ok && !matches!(known, Who::Unknown))
}

/// What a form that asks the password and a code found.
enum Proof {
    Good,
    Refused,
    Locked,
    Error,
}

/// The password, then the code. The two failures give the same answer. A wrong password counts against the address, a
/// wrong code against the account, as at the sign-in.
async fn prove(
    state: &Arc<AppState>,
    client: &Client,
    viewer: &Viewer,
    form: &mut std::collections::HashMap<String, String>,
) -> Proof {
    let password = Zeroizing::new(form.remove("password").unwrap_or_default());
    let code = form.remove("code").unwrap_or_default();
    if code.len() > 64 || address_acquire(state, client).is_some() {
        return Proof::Locked;
    }
    match password_is_right(state, viewer, password).await {
        Some(true) => {}
        Some(false) => {
            state.metrics.login_mfa_failure();
            return Proof::Refused;
        }
        None => return Proof::Error,
    }
    let (worker, name) = (Arc::clone(state), viewer.login.clone());
    let verdict = tokio::task::spawn_blocking(move || {
        let mfa = worker.mfa.as_ref().ok_or("the second factor is off")?;
        mfa.verify(&worker.store, &name, &code, unix_now())
    })
    .await;
    match verdict {
        Ok(Ok(Verdict::Totp | Verdict::Recovery)) => {
            address_success(state, client);
            Proof::Good
        }
        Ok(Ok(Verdict::Locked { .. })) => Proof::Locked,
        Ok(Ok(Verdict::Wrong { locked_until })) => {
            state.metrics.login_mfa_failure();
            if locked_until.is_some() {
                Proof::Locked
            } else {
                Proof::Refused
            }
        }
        Ok(Ok(Verdict::NotEnrolled)) => Proof::Refused,
        Ok(Err(error)) => {
            log(
                state,
                LogLevel::Error,
                "mfa_check_failed",
                &[("error", error)],
            );
            Proof::Error
        }
        Err(_) => Proof::Error,
    }
}

/// `POST /auth/security/enable`: the first code confirms the secret of the page.
pub(super) async fn handle_security_enable(
    state: &Arc<AppState>,
    request: Request<Incoming>,
    client: &Client,
) -> HttpResponse {
    let (viewer, mut form, headers) =
        match security_request(state, request, client, "mfa_enable").await {
            Ok(v) => v,
            Err(response) => return response,
        };
    let enrol = form.remove("enrol").unwrap_or_default();
    let code = form.remove("code").unwrap_or_default();
    if code.len() > 64 || address_acquire(state, client).is_some() {
        return security_redirect("mfa-locked");
    }
    let (worker, name, token, typed) =
        (Arc::clone(state), viewer.login.clone(), enrol.clone(), code);
    let outcome = tokio::task::spawn_blocking(move || {
        let mfa = worker.mfa.as_ref().ok_or("the second factor is off")?;
        mfa.confirm(&worker.store, &name, &token, &typed, unix_now())
    })
    .await;
    match outcome {
        Ok(Ok(Enrolled::Done(codes))) => {
            address_success(state, client);
            audit(
                state,
                AuditKind::TotpEnabled,
                Some(viewer.login.clone()),
                Some(viewer.login.clone()),
                client,
                &headers,
            );
            state.metrics.user_admin("mfa_enable", "ok");
            render(
                state,
                &viewer,
                None,
                |_| Pick::Codes(codes),
                None,
                StatusCode::OK,
            )
            .await
        }
        Ok(Ok(Enrolled::WrongCode)) => {
            state.metrics.login_mfa_failure();
            let Some(mfa) = state.mfa.as_ref() else {
                return internal_error();
            };
            match mfa.resume(&viewer.login, &enrol, &state.config.server.name, unix_now()) {
                Some(page) => {
                    render(
                        state,
                        &viewer,
                        ui::notice_for("mfa-bad-code"),
                        |_| Pick::Enrol(page),
                        None,
                        StatusCode::OK,
                    )
                    .await
                }
                None => security_redirect("mfa-stale"),
            }
        }
        Ok(Ok(Enrolled::BadPage | Enrolled::Already)) => security_redirect("mfa-stale"),
        Ok(Err(error)) => {
            log(
                state,
                LogLevel::Error,
                "mfa_enrol_failed",
                &[("error", error)],
            );
            security_redirect("error")
        }
        Err(_) => security_redirect("error"),
    }
}

/// `POST /auth/security/disable`: password and code.
pub(super) async fn handle_security_disable(
    state: &Arc<AppState>,
    request: Request<Incoming>,
    client: &Client,
) -> HttpResponse {
    let (viewer, mut form, headers) =
        match security_request(state, request, client, "mfa_disable").await {
            Ok(v) => v,
            Err(response) => return response,
        };
    if state.config.authentication.mfa == crate::config::MfaPolicy::Required {
        return security_redirect("mfa-required");
    }
    match prove(state, client, &viewer, &mut form).await {
        Proof::Good => {}
        Proof::Refused => return security_redirect("mfa-refused"),
        Proof::Locked => return security_redirect("mfa-locked"),
        Proof::Error => return security_redirect("error"),
    }
    let (store, login) = (Arc::clone(&state.store), viewer.login.clone());
    match tokio::task::spawn_blocking(move || store.totp_remove(&login)).await {
        Ok(Ok(_)) => {
            audit(
                state,
                AuditKind::TotpDisabled,
                Some(viewer.login.clone()),
                Some(viewer.login.clone()),
                client,
                &headers,
            );
            state.metrics.user_admin("mfa_disable", "ok");
            security_redirect("mfa-disabled")
        }
        _ => security_redirect("error"),
    }
}

/// `POST /auth/security/recovery`: new recovery codes, with the password and a code.
pub(super) async fn handle_security_recovery(
    state: &Arc<AppState>,
    request: Request<Incoming>,
    client: &Client,
) -> HttpResponse {
    let (viewer, mut form, headers) =
        match security_request(state, request, client, "mfa_recovery").await {
            Ok(v) => v,
            Err(response) => return response,
        };
    match prove(state, client, &viewer, &mut form).await {
        Proof::Good => {}
        Proof::Refused => return security_redirect("mfa-refused"),
        Proof::Locked => return security_redirect("mfa-locked"),
        Proof::Error => return security_redirect("error"),
    }
    let (worker, login) = (Arc::clone(state), viewer.login.clone());
    let made = tokio::task::spawn_blocking(move || {
        let mfa = worker.mfa.as_ref().ok_or("the second factor is off")?;
        mfa.regenerate(&worker.store, &login)
    })
    .await;
    match made {
        Ok(Ok(Some(codes))) => {
            audit(
                state,
                AuditKind::RecoveryRegenerated,
                Some(viewer.login.clone()),
                Some(viewer.login.clone()),
                client,
                &headers,
            );
            state.metrics.user_admin("mfa_recovery", "ok");
            render(
                state,
                &viewer,
                None,
                |_| Pick::Codes(codes),
                None,
                StatusCode::OK,
            )
            .await
        }
        Ok(Ok(None)) => security_redirect("mfa-none"),
        _ => security_redirect("error"),
    }
}

/// `POST /auth/users/reset-two-factor`: the superadmin removes the second factor of another account and ends its sessions.
pub(super) async fn handle_reset(
    state: &Arc<AppState>,
    request: Request<Incoming>,
    client: &Client,
) -> HttpResponse {
    let headers = request.headers().clone();
    let (viewer, mut form) = match admin_request(state, request, client, "mfa_reset").await {
        Ok(v) => v,
        Err(response) => return response,
    };
    if let Err(response) = require_superadmin(state, &viewer, "mfa_reset") {
        return response;
    }
    if state.mfa.is_none() {
        return handle_not_found_simple();
    }
    let login = match normalize_login(&form.remove("login").unwrap_or_default()) {
        Ok(login) if login != state.superadmin_login => login,
        _ => return settings_redirect("no-user"),
    };
    let (store, target) = (Arc::clone(&state.store), login.clone());
    match tokio::task::spawn_blocking(move || store.totp_remove(&target)).await {
        Ok(Ok(true)) => {}
        Ok(Ok(false)) => return user_redirect(&login, "mfa-none"),
        _ => return user_redirect(&login, "error"),
    }
    // The sessions of the account end, as they do when the password changes.
    state.sessions.revoke_user(&login, EndReason::Revoked);
    audit(
        state,
        AuditKind::TotpReset,
        Some(login.clone()),
        Some(state.superadmin_login.clone()),
        client,
        &headers,
    );
    state.metrics.user_admin("mfa_reset", "ok");
    user_redirect(&login, "mfa-reset")
}
