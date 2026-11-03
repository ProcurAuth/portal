//! The page of the tab "Inbox" and its forms, under `/auth/settings/inbox`.
//!
//! The page needs a session. The rights come from the service `portal` (see
//! [`crate::portal_actions`]): the superadmin has all of them, and any other account has the
//! permissions that it holds. A page that the account may not open answers 403, and a page of a
//! block `inbox` that does not exist answers 404. The page calls the inbox in process, with no
//! HTTP call to itself.
//!
//! A form is a POST with the field `csrf` (the token of the purpose `inbox`). It answers with a
//! redirect to the page that shows the outcome as a code, except the form of an enrollment code:
//! it answers with the page itself, with the code in a popover that shows it one time, with
//! `Cache-Control: no-store`. Every other address under the prefix answers 404.
//! A session that "Log in as" opens can read the pages and make a code for the account. It does
//! not send an alert and does not revoke a phone.

use super::pdp::{decide_person_for, UseVerdict};
use super::*;
use crate::inbox::admin::{self, DeviceRow, Target};
use crate::inbox::App as InboxApp;
use crate::portal_actions as actions;
use crate::ui::inbox as ui_inbox;
use crate::ui::inbox::{
    AlertRow, CodeData, ConnData, DocsData, GroupChoice, GroupLine, InboxView, Origin, PhonesData,
    Query, Rights, SentData, UserChoice,
};

pub(super) const PREFIX: &str = ui_inbox::PAGE;
/// The purpose of the anti-CSRF tokens of the forms of the inbox.
const INBOX_TOKEN: &str = "inbox";
/// The largest body of a form of the inbox.
const MAX_FORM_BYTES: usize = 16 * 1024;
/// The alerts that the zone "Sent" asks for, and the lines of the connection log.
const ALERTS_SHOWN: u32 = 500;
const CONNECTIONS_SHOWN: u32 = 200;

/// Whether the path belongs to the pages of the inbox.
pub(super) fn is_inbox_path(path: &str) -> bool {
    path == PREFIX
        || path
            .strip_prefix(PREFIX)
            .is_some_and(|r| r.starts_with('/'))
}

/// What an account holds in the inbox features of the service `portal`.
pub(super) struct Held {
    pub ui: Rights,
    /// The groups with `alert.read`. `None`: all the groups (the superadmin).
    read_groups: Option<Vec<String>>,
    /// The groups that the page names: the groups that the account may read or send to.
    named: Option<Vec<String>>,
}

/// The rights of the viewer. `None`: the file has no block `inbox`.
pub(super) fn held(state: &AppState, viewer: &Viewer) -> Option<Held> {
    let inbox = state.config.inbox.as_ref()?;
    let ids: Vec<String> = inbox.groups.keys().cloned().collect();
    let members = super::group_members(state);
    let choice = |id: &String| GroupChoice {
        id: id.clone(),
        name: inbox.groups[id].name.clone(),
        critical: inbox.groups[id].critical,
        members: members.get(id).copied().unwrap_or(0),
    };
    // A critical group comes last: the form never starts on it.
    let ordered = |mut ids: Vec<String>| {
        ids.sort_by_key(|id| inbox.groups[id].critical);
        ids
    };
    let Some(grants) = &viewer.grants else {
        // The superadmin has every right.
        return Some(Held {
            ui: Rights {
                sent: !ids.is_empty(),
                phones: true,
                revoke: true,
                connections: true,
                own_phones: !ids.is_empty(),
                send: ordered(ids.clone()).iter().map(choice).collect(),
                send_user: true,
            },
            read_groups: None,
            named: None,
        });
    };
    let document = state.actions.declared(actions::SERVICE)?;
    let login = viewer.login.as_str();
    let on =
        |action: &str| actions::groups_with(&document, login, grants, action, ids.iter().cloned());
    let at_service = |action: &str| actions::allows(&document, login, grants, action, None);
    let send_ids = on(actions::ALERT_SEND);
    let read_ids = on(actions::ALERT_READ);
    let devices = at_service(actions::PHONE_READ);
    let send_user = at_service(actions::ALERT_SEND_USER);
    let mut named: Vec<String> = read_ids.iter().chain(send_ids.iter()).cloned().collect();
    named.sort();
    named.dedup();
    Some(Held {
        ui: Rights {
            // An account that may alert any account sees the alerts that it sent.
            sent: !read_ids.is_empty() || send_user,
            phones: devices,
            revoke: devices && at_service(actions::PHONE_REVOKE),
            connections: at_service(actions::PHONE_LOG),
            own_phones: !on(actions::INBOX_RECEIVE).is_empty(),
            send: ordered(send_ids.clone()).iter().map(choice).collect(),
            send_user,
        },
        read_groups: Some(read_ids),
        named: Some(named),
    })
}

impl Held {
    fn may_read(&self, group: &str) -> bool {
        self.read_groups
            .as_ref()
            .is_none_or(|groups| groups.iter().any(|g| g == group))
    }

    fn names(&self, group: &str) -> bool {
        self.named
            .as_ref()
            .is_none_or(|groups| groups.iter().any(|g| g == group))
    }
}

/// Whether the viewer may read the alert. An alert for a group needs `alert.read` on the group. An
/// alert for an account can be read by its sender, by the target account and by the superadmin: a
/// right on a group does not fit a target that is not a group. The sender is the account that used
/// the form, or the owner of the key that sent the alert.
fn may_read_alert(state: &AppState, ctx: &Context<'_>, summary: &admin::AlertSummary) -> bool {
    match &summary.target {
        Target::Group(group) => ctx.held.may_read(group),
        Target::User(login) => {
            is_superadmin(&ctx.viewer)
                || *login == ctx.viewer.login
                || sender_login(state, &summary.source).is_some_and(|l| l == ctx.viewer.login)
        }
    }
}

/// The login of the account that sent an alert, from its `source`. The form of the page writes
/// `admin:<login>`. A key of the database gives its id: the sender is the owner of the key. A key
/// of the file gives its name: the sender is the owner that the file names.
fn sender_login(state: &AppState, source: &str) -> Option<String> {
    use crate::users::KeyOwner;
    if let Some(login) = admin::account_of_source(source) {
        return Some(login.to_string());
    }
    if let Some(id) = admin::key_of_source(source) {
        return match &state.users.key_by_id(id)?.owner {
            KeyOwner::User(login) | KeyOwner::File(login) => Some(login.clone()),
            KeyOwner::Superadmin => Some(state.superadmin_login.clone()),
        };
    }
    let name = source.split_once(':').map(|(_, name)| name)?;
    state
        .config
        .inbox
        .as_ref()?
        .api_keys
        .get(name)
        .map(|key| key.owner.clone())
}

/// Whether the viewer sees the tab "Inbox".
pub(super) fn tab_visible(state: &AppState, viewer: &Viewer) -> bool {
    held(state, viewer).is_some_and(|held| held.ui.any())
}

fn not_found() -> HttpResponse {
    text_response(StatusCode::NOT_FOUND, "Not Found\n")
}

fn forbidden() -> HttpResponse {
    text_response(StatusCode::FORBIDDEN, "Forbidden\n")
}

fn bad_request() -> HttpResponse {
    text_response(StatusCode::BAD_REQUEST, "Bad Request\n")
}

fn unavailable() -> HttpResponse {
    text_response(StatusCode::SERVICE_UNAVAILABLE, "Service Unavailable\n")
}

fn back_with(path: &str, code: &str) -> HttpResponse {
    redirect_response(StatusCode::SEE_OTHER, &format!("{path}?s={code}"))
}

/// What a page needs around its content.
struct Context<'a> {
    app: Arc<InboxApp>,
    viewer: Viewer,
    held: Held,
    logout_token: String,
    inbox_token: String,
    uri: &'a str,
    /// The address of the portal as the browser sees it (scheme, host, port). The producers use it.
    base: String,
}

impl Context<'_> {
    fn frame(&self, state: &AppState) -> ui::Frame<'_> {
        ui::Frame {
            logout_token: &self.logout_token,
            keys: super::keys_page::list_visible(state, &self.viewer),
            inbox: true,
            superadmin: is_superadmin(&self.viewer),
        }
    }
}

/// The checks that every page has in common. The error is the response.
#[allow(clippy::result_large_err)] // the error is the response, sent as is
fn open<'a>(
    state: &AppState,
    uri: &'a str,
    headers: &HeaderMap,
    client: &Client,
) -> Result<Context<'a>, HttpResponse> {
    let Some(app) = state.inbox.app() else {
        return Err(not_found());
    };
    let Some(viewer) = current_viewer(state, headers, client) else {
        let path = uri.split('?').next().unwrap_or(PREFIX);
        return Err(match login_location(state, path) {
            Ok(location) => redirect_response(StatusCode::SEE_OTHER, &location),
            Err(()) => redirect_response(StatusCode::SEE_OTHER, LOGIN_PATH),
        });
    };
    let Some(held) = held(state, &viewer) else {
        return Err(not_found());
    };
    let (Ok(logout_token), Ok(inbox_token)) = (
        sign_action_token(&state.signing_secret, LOGOUT_TOKEN, &viewer.token),
        sign_action_token(&state.signing_secret, INBOX_TOKEN, &viewer.token),
    ) else {
        return Err(text_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            "authentication error\n",
        ));
    };
    Ok(Context {
        app,
        viewer,
        held,
        logout_token,
        inbox_token,
        uri,
        base: portal_base(headers),
    })
}

/// The address of the portal as the request shows it: `X-Forwarded-Proto` and `X-Forwarded-Host`
/// from the proxy, or the header `Host` over `http`. The producers send their requests to the
/// same host and port as the pages. The address of the phones (`inbox.devices.public_address`)
/// is another one.
fn portal_base(headers: &HeaderMap) -> String {
    match expected_origin(headers) {
        Some(origin) => {
            let default = if origin.scheme == "https" { 443 } else { 80 };
            if origin.port == default {
                format!("{}://{}", origin.scheme, origin.host)
            } else {
                format!("{}://{}:{}", origin.scheme, origin.host, origin.port)
            }
        }
        None => "http://<address of the portal>".to_string(),
    }
}

/// A random text of 32 hexadecimal characters: the key of a form that sends an alert.
fn random_key() -> String {
    let bytes: [u8; 16] = inbox_core::rng::Rng::os().bytes();
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn sorted(mut rows: Vec<DeviceRow>) -> Vec<DeviceRow> {
    rows.sort_by(|a, b| {
        (
            a.device.revoked_at.is_some(),
            &a.device.user_name,
            &a.device.name,
        )
            .cmp(&(
                b.device.revoked_at.is_some(),
                &b.device.user_name,
                &b.device.name,
            ))
    });
    rows
}

/// Handles one request under the prefix.
pub(super) async fn handle(
    state: &Arc<AppState>,
    request: Request<Incoming>,
    client: &Client,
) -> HttpResponse {
    let path = request.uri().path().to_string();
    let uri = request
        .uri()
        .path_and_query()
        .map(|p| p.as_str())
        .unwrap_or(&path)
        .to_string();
    let rest = path.strip_prefix(PREFIX).unwrap_or_default();
    let method = request.method().clone();
    if method == Method::GET {
        let ctx = match open(state, &uri, request.headers(), client) {
            Ok(ctx) => ctx,
            Err(response) => return response,
        };
        return get(state, &ctx, rest).await;
    }
    if method == Method::POST {
        return post(state, request, client, rest).await;
    }
    text_response(StatusCode::METHOD_NOT_ALLOWED, "Method Not Allowed\n")
}

/// The query of the address as pairs, decoded. `None`: a bad encoding or a long value.
fn query_pairs(uri: &str) -> Option<Vec<(String, String)>> {
    let Some((_, query)) = uri.split_once('?') else {
        return Some(Vec::new());
    };
    let mut out = Vec::new();
    for pair in query.split('&').filter(|p| !p.is_empty()) {
        let (name, value) = pair.split_once('=').unwrap_or((pair, ""));
        let name = percent_decode_form(name).ok()?;
        let value = percent_decode_form(value).ok()?;
        if name.len() > 16 || value.len() > 200 {
            return None;
        }
        out.push((name, value));
    }
    Some(out)
}

async fn get(state: &Arc<AppState>, ctx: &Context<'_>, rest: &str) -> HttpResponse {
    // The page has one address. Each other address under the prefix is gone.
    if !rest.is_empty() {
        return if matches!(rest, "/send" | "/revoke" | "/code") {
            text_response(StatusCode::METHOD_NOT_ALLOWED, "Method Not Allowed\n")
        } else {
            not_found()
        };
    }
    if !ctx.held.ui.any() {
        return forbidden();
    }
    let Some(pairs) = query_pairs(ctx.uri) else {
        return bad_request();
    };
    let Some(query) = Query::parse(&pairs) else {
        return bad_request();
    };
    let code = pairs.iter().find(|(name, _)| name == "s").map(|(_, v)| v);
    let notice = code.and_then(|code| ui_inbox::notice_for(code));
    if code.is_some() && notice.is_none() {
        return bad_request();
    }
    page(state, ctx, &query, notice, None).await
}

/// The name of the sender of an alert that a key of the database made.
fn key_name(state: &AppState, id: &str) -> Option<String> {
    state.users.key_by_id(id).map(|key| key.name.clone())
}

/// The accounts that the popup may send an alert to, by login, each with its active phones. The
/// accounts are the accounts of the database and of the file. The superadmin is not a row.
/// This is the one place that builds the choices of the popup.
fn user_choices(
    state: &AppState,
    phones: &std::collections::BTreeMap<String, u32>,
) -> Vec<UserChoice> {
    let mut logins: Vec<String> = state
        .users
        .list()
        .into_iter()
        .map(|(login, _)| login)
        .collect();
    logins.extend(state.config.access.accounts.keys().cloned());
    logins.sort();
    logins.dedup();
    logins
        .into_iter()
        .map(|login| UserChoice {
            phones: phones.get(&login).copied().unwrap_or(0) as usize,
            login,
        })
        .collect()
}

/// Draws the page. `code` is a new enrollment code, shown in its popover.
async fn page(
    state: &Arc<AppState>,
    ctx: &Context<'_>,
    query: &Query,
    notice: Option<crate::ui::settings::Notice>,
    code: Option<CodeData<'_>>,
) -> HttpResponse {
    let rights = &ctx.held.ui;
    let now_ms = unix_now() * 1000;
    let Some(inbox) = state.config.inbox.as_ref() else {
        return not_found();
    };
    let mut groups: Vec<GroupLine> = inbox
        .groups
        .iter()
        .filter(|(id, _)| ctx.held.names(id))
        .map(|(id, g)| GroupLine {
            id: id.clone(),
            name: g.name.clone(),
            critical: g.critical,
        })
        .collect();
    // The critical groups come last, as in the permission editor.
    groups.sort_by_key(|g| g.critical);
    let sent = if rights.sent {
        let since_ms = now_ms - query.period.millis();
        let keep = |summary: &admin::AlertSummary| may_read_alert(state, ctx, summary);
        let (alerts, truncated) = match admin::alerts(&ctx.app, ALERTS_SHOWN, since_ms, keep).await
        {
            Ok(v) => v,
            Err(_) => return unavailable(),
        };
        let counts =
            match admin::alert_counts(&ctx.app, since_ms, query.period.bucket_millis()).await {
                Ok(v) => v,
                Err(_) => return unavailable(),
            };
        let opened = match &query.open {
            Some(id) => match admin::alert(&ctx.app, id).await {
                Ok(Some(detail)) if may_read_alert(state, ctx, &detail.summary) => Some(detail),
                Ok(_) => None,
                Err(_) => return unavailable(),
            },
            None => None,
        };
        let resolve = |id: &str| key_name(state, id);
        let alerts = alerts
            .into_iter()
            .map(|summary| AlertRow {
                origin: Origin::of(&summary.source, &resolve),
                summary,
            })
            .collect();
        Some(SentData {
            alerts,
            truncated,
            counts,
            since_ms,
            opened,
        })
    } else {
        None
    };
    // The accounts that the popup may send to, with their active phones.
    let users = if rights.send_user && !ctx.viewer.session.impersonated {
        match admin::active_phone_counts(&ctx.app).await {
            Ok(counts) => user_choices(state, &counts),
            Err(_) => return unavailable(),
        }
    } else {
        Vec::new()
    };
    let phones = if rights.phones || rights.own_phones {
        let mut rows = match admin::devices(&ctx.app).await {
            Ok(rows) => rows,
            Err(_) => return unavailable(),
        };
        if !rights.phones {
            rows.retain(|row| row.device.user_name == ctx.viewer.login);
        }
        Some(PhonesData {
            rows: sorted(rows),
            revoke_all: rights.revoke,
            revoke_own: rights.own_phones,
        })
    } else {
        None
    };
    let connections = if rights.connections {
        let login = (!query.login.is_empty()).then(|| query.login.clone());
        match admin::connections(&ctx.app, CONNECTIONS_SHOWN, login).await {
            Ok(rows) => Some(ConnData { rows }),
            Err(_) => return unavailable(),
        }
    } else {
        None
    };
    let key = random_key();
    let view = InboxView {
        rights,
        notice,
        csrf: &ctx.inbox_token,
        idempotency_key: &key,
        query,
        now_ms,
        login: &ctx.viewer.login,
        groups: &groups,
        users: &users,
        sent,
        phones,
        connections,
        docs: (rights.sent || !rights.send.is_empty()).then(|| DocsData {
            base: ctx.base.clone(),
        }),
        code,
        impersonated: ctx.viewer.session.impersonated,
    };
    let html = ui_inbox::inbox_page(&ctx.frame(state), &view);
    let mut response = html_response(StatusCode::OK, html);
    if view.code.is_some() {
        response.headers_mut().insert(
            HeaderName::from_static("cache-control"),
            HeaderValue::from_static("no-store"),
        );
    }
    response
}

/// The checks of a POST: the origin, the type of the body, the session, the rights, the body and
/// the token. The error is the response.
#[allow(clippy::result_large_err)] // the error is the response, sent as is
async fn form<'a>(
    state: &AppState,
    request: Request<Incoming>,
    client: &Client,
) -> Result<(Context<'a>, std::collections::HashMap<String, String>), HttpResponse> {
    if !validate_post_origin(request.headers()) {
        state.metrics.users_origin_failure();
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
    let ctx = open(state, "", request.headers(), client)?;
    if !ctx.held.ui.any() {
        return Err(forbidden());
    }
    let body = match read_limited_body(
        request.into_body(),
        MAX_FORM_BYTES,
        state.config.server.body_read_timeout,
    )
    .await
    {
        Ok(body) => body,
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
        Err(BodyReadError::Io) => return Err(bad_request()),
    };
    let Ok(mut fields) = parse_form(&body) else {
        return Err(bad_request());
    };
    let csrf = fields.remove("csrf").unwrap_or_default();
    if !verify_action_token(&state.signing_secret, INBOX_TOKEN, &ctx.viewer.token, &csrf) {
        return Err(text_response(
            StatusCode::FORBIDDEN,
            "CSRF validation failed\n",
        ));
    }
    Ok((ctx, fields))
}

async fn post(
    state: &Arc<AppState>,
    request: Request<Incoming>,
    client: &Client,
    rest: &str,
) -> HttpResponse {
    if !matches!(rest, "/send" | "/revoke" | "/code") {
        return not_found();
    }
    let (ctx, fields) = match form(state, request, client).await {
        Ok(v) => v,
        Err(response) => return response,
    };
    match rest {
        "/send" => send(state, &ctx, fields).await,
        "/revoke" => revoke(state, &ctx, fields).await,
        _ => code(state, &ctx, fields).await,
    }
}

/// The target that the field `to` of the form names: `group:<id>` or `user:<login>`. `None`: the
/// text has another form.
fn parse_to(text: &str) -> Option<Target> {
    let (kind, id) = text.split_once(':')?;
    if id.is_empty() || id.len() > 64 || id.chars().any(char::is_control) {
        return None;
    }
    Target::from_columns(kind, id.to_string())
}

/// `POST /auth/settings/inbox/send`: the title and the target of an alert. The field `to` is
/// `group:<id>` or `user:<login>`. For a group, the account needs `alert.send` on the group. The
/// decision counts one use, as for a key, unless the group is critical. For an account, the account
/// needs `alert.send_user`: the decision always counts one use of the same budget, because an alert
/// to an account is never critical. The superadmin may do both.
async fn send(
    state: &Arc<AppState>,
    ctx: &Context<'_>,
    mut fields: std::collections::HashMap<String, String>,
) -> HttpResponse {
    let (Some(to), Some(title), Some(key)) = (
        fields.remove("to"),
        fields.remove("title"),
        fields.remove("key"),
    ) else {
        return bad_request();
    };
    if !fields.is_empty() {
        return bad_request();
    }
    if key.is_empty() || key.len() > 100 || !key.bytes().all(|b| b.is_ascii_graphic()) {
        return bad_request();
    }
    if ctx.viewer.session.impersonated {
        return forbidden();
    }
    let back = ui_inbox::PAGE;
    let Some(target) = parse_to(&to) else {
        return back_with(back, "bad_target");
    };
    let title = title.trim().to_string();
    if title.is_empty() {
        return back_with(back, "no_title");
    }
    if title.chars().count() > ui_inbox::MAX_TITLE || title.chars().any(char::is_control) {
        return back_with(back, "bad_title");
    }
    let (action, chain, count_use) = match &target {
        Target::Group(group) => {
            if !ctx.held.ui.send.iter().any(|g| g.id == *group) {
                return back_with(back, "bad_group");
            }
            let Some(chain) = actions::group_chain(group) else {
                return back_with(back, "bad_group");
            };
            (actions::ALERT_SEND, chain, !ctx.app.is_critical(group))
        }
        Target::User(login) => {
            if !ctx.held.ui.send_user {
                return back_with(back, "bad_target");
            }
            let known = *login == state.superadmin_login || state.users.get(login).is_some();
            if !known {
                return back_with(back, "bad_user");
            }
            // An account with no phone gets nothing, and the refused alert uses no budget.
            match admin::active_phone_counts(&ctx.app).await {
                Ok(phones) if phones.contains_key(login) => {}
                Ok(_) => return back_with(back, "no_phone"),
                Err(_) => return back_with(back, "failed"),
            }
            (actions::ALERT_SEND_USER, Vec::new(), true)
        }
    };
    let verdict = {
        let (state, login) = (Arc::clone(state), ctx.viewer.login.clone());
        decide_person_for(&state, &login, actions::SERVICE, action, &chain, count_use)
    };
    match verdict {
        UseVerdict::Allowed => {}
        UseVerdict::Limited(_) => return back_with(back, "limited"),
        UseVerdict::Denied | UseVerdict::UnknownSubject => {
            return back_with(
                back,
                if matches!(target, Target::Group(_)) {
                    "bad_group"
                } else {
                    "bad_target"
                },
            )
        }
        UseVerdict::NotReady | UseVerdict::Unavailable => return back_with(back, "failed"),
    }
    log(
        state,
        LogLevel::Info,
        "inbox_admin_send",
        &[
            ("actor", ctx.viewer.login.clone()),
            ("target", target.to_string()),
        ],
    );
    let readable = match &target {
        Target::Group(group) => ctx.held.may_read(group),
        // The sender reads the alert that it sent, when the page has the zone "Sent".
        Target::User(_) => ctx.held.ui.sent,
    };
    let accepted = match admin::send_alert(
        &ctx.app,
        &ctx.viewer.login,
        target,
        title,
        format!("admin:{key}"),
    )
    .await
    {
        Ok(accepted) => accepted,
        Err(e) if e.code() == "no_phone" => return back_with(back, "no_phone"),
        Err(_) => return back_with(back, "failed"),
    };
    let code = if accepted.duplicate {
        "duplicate"
    } else {
        "sent"
    };
    if readable {
        // The page opens the new alert in place.
        return redirect_response(
            StatusCode::SEE_OTHER,
            &format!(
                "{}?s={code}&open={}#a-{}",
                ui_inbox::PAGE,
                accepted.alert_id,
                accepted.alert_id
            ),
        );
    }
    back_with(back, code)
}

/// `POST /auth/settings/inbox/revoke`: one phone or more. The form names each phone `id.<id>`, or
/// one phone `id`. The account needs `phone.revoke` and `phone.read`, or it owns the phone and
/// holds `inbox.receive` on a group. A phone that does not exist and a phone of another account
/// give the same answer.
async fn revoke(
    state: &Arc<AppState>,
    ctx: &Context<'_>,
    fields: std::collections::HashMap<String, String>,
) -> HttpResponse {
    let mut ids: Vec<String> = Vec::new();
    for (name, value) in &fields {
        let id = match name.strip_prefix("id.") {
            Some(id) if value == "1" => id,
            None if name == "id" => value.as_str(),
            _ => return bad_request(),
        };
        if id.len() != 32 || !id.bytes().all(|b| b.is_ascii_hexdigit()) {
            return bad_request();
        }
        ids.push(id.to_string());
    }
    if ids.is_empty() || ids.len() > 500 {
        return bad_request();
    }
    ids.sort();
    ids.dedup();
    if ctx.viewer.session.impersonated {
        return forbidden();
    }
    let back = ui_inbox::PAGE;
    let mut revoked = 0usize;
    for id in ids {
        let owner = match admin::owner_of(&ctx.app, &id).await {
            Ok(owner) => owner,
            Err(_) => return back_with(back, "failed"),
        };
        let may = ctx.held.ui.revoke
            || (ctx.held.ui.own_phones && owner.as_deref() == Some(ctx.viewer.login.as_str()));
        if owner.is_none() || !may {
            continue;
        }
        match admin::revoke(&ctx.app, &id).await {
            Ok(_) => {
                revoked += 1;
                log(
                    state,
                    LogLevel::Info,
                    "inbox_admin_revoke",
                    &[("actor", ctx.viewer.login.clone()), ("device", id)],
                );
            }
            Err(_) => return back_with(back, "failed"),
        }
    }
    back_with(
        back,
        if revoked > 0 {
            "revoked"
        } else {
            "unknown_device"
        },
    )
}

/// `POST /auth/settings/inbox/code`: a new enrollment code for the account of the session. The
/// answer is the page, with the code in a popover. The page shows the code one time.
async fn code(
    state: &Arc<AppState>,
    ctx: &Context<'_>,
    fields: std::collections::HashMap<String, String>,
) -> HttpResponse {
    if !fields.is_empty() {
        return bad_request();
    }
    if !ctx.held.ui.own_phones {
        return forbidden();
    }
    log(
        state,
        LogLevel::Info,
        "inbox_enrollment_code",
        &[("actor", ctx.viewer.login.clone())],
    );
    let made = state.inbox.create_enrollment_code(&ctx.viewer.login).await;
    match made {
        Some(Ok(code)) => {
            page(
                state,
                ctx,
                &Query::default(),
                None,
                Some(CodeData {
                    code: &code.code,
                    fingerprint: &code.fingerprint,
                    expires_at_ms: code.expires_at_ms,
                }),
            )
            .await
        }
        Some(Err(e)) if e.status() == StatusCode::TOO_MANY_REQUESTS => {
            back_with(ui_inbox::PAGE, "too_many_codes")
        }
        Some(Err(_)) => back_with(ui_inbox::PAGE, "failed"),
        None => not_found(),
    }
}
