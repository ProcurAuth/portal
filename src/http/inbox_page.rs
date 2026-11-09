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

mod items;
mod prefs;

use super::pdp::{decide_person_for, UseVerdict};
use super::*;
use crate::inbox::admin::{self, DeviceRow, Target};
use crate::inbox::notify::{self, Ask, Importance, Level, Scope, Services};
use crate::inbox::App as InboxApp;
use crate::portal_actions as actions;
use crate::ui::inbox as ui_inbox;
use crate::ui::inbox::{
    AlertRow, CodeData, ConnData, DocsData, GroupChoice, GroupLine, InboxView, Origin, PhonesData,
    Query, Rights, SendError, SentData, UserChoice,
};
use crate::ui::notify as ui_notify;

pub(super) const PREFIX: &str = ui_inbox::PAGE;
/// The purpose of the anti-CSRF tokens of the forms of the inbox.
const INBOX_TOKEN: &str = "inbox";
/// The address of the poll of the notifications, after the prefix.
const POLL: &str = "/notifications";
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
    /// The groups with `inbox.receive`. `None`: all the groups (the superadmin).
    receive_groups: Option<Vec<String>>,
    /// The groups that the page names: the groups that the account may read or send to.
    named: Option<Vec<String>>,
    /// The services whose alerts the account may see: the services of its launcher.
    services: Services,
    /// `approval.read`: the metadata of the requests of all the accounts.
    approval_read: bool,
    /// `approval.cancel`: an open request of any account can be cancelled.
    approval_cancel: bool,
}

/// The rights of the viewer. `None`: the file has no block `inbox`.
pub(super) fn held(state: &AppState, viewer: &Viewer) -> Option<Held> {
    held_with(state, viewer, true)
}

/// The same, with no count of the members of the groups: the count reads the grants of every
/// account, and a request that comes every few seconds does not need it.
fn held_light(state: &AppState, viewer: &Viewer) -> Option<Held> {
    held_with(state, viewer, false)
}

fn held_with(state: &AppState, viewer: &Viewer, count_members: bool) -> Option<Held> {
    let inbox = state.config.inbox.as_ref()?;
    let ids: Vec<String> = inbox.groups.keys().cloned().collect();
    let members = if count_members {
        super::group_members(state)
    } else {
        Default::default()
    };
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
    // The engine of the items runs: the file has the blocks `inbox.items` and `inbox.at_rest`.
    let engine = inbox.items.is_some() && inbox.at_rest.is_some();
    let Some(grants) = &viewer.grants else {
        // The superadmin has every right.
        return Some(Held {
            ui: Rights {
                items: engine,
                sent: !ids.is_empty(),
                phones: true,
                revoke: true,
                connections: true,
                own_phones: !ids.is_empty(),
                send: ordered(ids.clone()).iter().map(choice).collect(),
                send_user: true,
            },
            read_groups: None,
            receive_groups: None,
            named: None,
            services: Services::of_grants(None),
            approval_read: engine,
            approval_cancel: engine,
        });
    };
    let document = state.actions.declared(actions::SERVICE)?;
    let login = viewer.login.as_str();
    let on =
        |action: &str| actions::groups_with(&document, login, grants, action, ids.iter().cloned());
    let at_service = |action: &str| actions::allows(&document, login, grants, action, None);
    let send_ids = on(actions::ALERT_SEND);
    let read_ids = on(actions::ALERT_READ);
    let receive_ids = on(actions::INBOX_RECEIVE);
    let devices = at_service(actions::PHONE_READ);
    let send_user = at_service(actions::ALERT_SEND_USER);
    let approval_read = engine && at_service(actions::APPROVAL_READ);
    // An account that can use keys of its own can own requests.
    let owns_keys = !actions::kinds_with(grants, actions::KEYS_OWN_USE).is_empty();
    let mut named: Vec<String> = read_ids.iter().chain(send_ids.iter()).cloned().collect();
    named.sort();
    named.dedup();
    Some(Held {
        ui: Rights {
            items: engine && (approval_read || owns_keys),
            // An account that may alert any account sees the alerts that it sent.
            sent: !read_ids.is_empty() || send_user,
            phones: devices,
            revoke: devices && at_service(actions::PHONE_REVOKE),
            connections: at_service(actions::PHONE_LOG),
            own_phones: !receive_ids.is_empty(),
            send: ordered(send_ids.clone()).iter().map(choice).collect(),
            send_user,
        },
        read_groups: Some(read_ids),
        receive_groups: Some(receive_ids),
        named: Some(named),
        services: Services::of_grants(Some(grants)),
        approval_read,
        approval_cancel: approval_read && at_service(actions::APPROVAL_CANCEL),
    })
}

impl Held {
    /// The alerts that the account receives: the groups with `inbox.receive`, and the alerts for
    /// the account (see `inbox::notify`). An alert about a service also needs the service.
    fn scope(&self, login: &str) -> Scope {
        Scope {
            groups: self.receive_groups.clone(),
            login: login.to_string(),
            services: self.services.clone(),
        }
    }

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
/// the form, or the owner of the key that sent the alert. An alert about a service also needs the
/// service in the launcher of the viewer.
fn may_read_alert(state: &AppState, ctx: &Context<'_>, summary: &admin::AlertSummary) -> bool {
    if !ctx.held.services.sees(summary.service.as_deref()) {
        return false;
    }
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
    fn frame<'a>(&'a self, state: &'a AppState) -> ui::Frame<'a> {
        ui::Frame {
            logout_token: &self.logout_token,
            keys: super::keys_page::list_visible(state, &self.viewer),
            inbox: true,
            superadmin: is_superadmin(&self.viewer),
            notify: None,
            security: super::mfa_page::tab_visible(state, &self.viewer),
            tab: state.tab_look(),
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
    open_with(state, uri, headers, client, false)
}

/// The same. With `passive`, the request does not renew the idle timer of the session, and the
/// rights are read without the count of the members of the groups: the poll of the notifications
/// comes every few seconds.
#[allow(clippy::result_large_err)] // the error is the response, sent as is
fn open_with<'a>(
    state: &AppState,
    uri: &'a str,
    headers: &HeaderMap,
    client: &Client,
    passive: bool,
) -> Result<Context<'a>, HttpResponse> {
    let Some(app) = state.inbox.app() else {
        return Err(not_found());
    };
    let viewer = if passive {
        current_viewer_passive(state, headers, client)
    } else {
        current_viewer(state, headers, client)
    };
    let Some(viewer) = viewer else {
        let path = uri.split('?').next().unwrap_or(PREFIX);
        return Err(match login_location(state, path) {
            Ok(location) => redirect_response(StatusCode::SEE_OTHER, &location),
            Err(()) => redirect_response(StatusCode::SEE_OTHER, LOGIN_PATH),
        });
    };
    let held = if passive {
        held_light(state, &viewer)
    } else {
        held(state, &viewer)
    };
    let Some(held) = held else {
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
        None => "http://<address of this server>".to_string(),
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
        // The poll of the notifications must not keep the session alive.
        let poll = rest == POLL;
        let ctx = match open_with(state, &uri, request.headers(), client, poll) {
            Ok(ctx) => ctx,
            Err(response) => return response,
        };
        if poll {
            return poll_answer(state, &ctx).await;
        }
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
    // The pages of the items have their own addresses.
    if rest == "/limits" {
        return items::limits(state, ctx).await;
    }
    if rest == "/items" {
        return items::history(state, ctx).await;
    }
    if rest == "/prefs" {
        return prefs::page(state, ctx).await;
    }
    if let Some(tail) = rest.strip_prefix("/items/") {
        if matches!(tail, "answer" | "reply" | "cancel") {
            return text_response(StatusCode::METHOD_NOT_ALLOWED, "Method Not Allowed\n");
        }
        let (id, body) = match tail.strip_suffix("/body") {
            Some(id) => (id, true),
            None => (tail, false),
        };
        return items::detail(state, ctx, id, body, None, StatusCode::OK).await;
    }
    // The page has one address. Each other address under the prefix is gone.
    if !rest.is_empty() {
        return if matches!(rest, "/send" | "/revoke" | "/code" | "/read" | "/notify") {
            text_response(StatusCode::METHOD_NOT_ALLOWED, "Method Not Allowed\n")
        } else {
            not_found()
        };
    }
    // A refusal of the page has the frame of the settings pages, with the way back.
    let refuse = |status: StatusCode, text: &str| {
        html_response(status, ui_inbox::status_page(&ctx.frame(state), text))
    };
    if !ctx.held.ui.any() {
        return refuse(StatusCode::FORBIDDEN, "Forbidden");
    }
    let Some(pairs) = query_pairs(ctx.uri) else {
        return refuse(StatusCode::BAD_REQUEST, "Bad request");
    };
    let Some(query) = Query::parse(&pairs) else {
        return refuse(StatusCode::BAD_REQUEST, "Bad request");
    };
    let code = pairs.iter().find(|(name, _)| name == "s").map(|(_, v)| v);
    let notice = code.and_then(|code| ui_inbox::notice_for(code));
    if code.is_some() && notice.is_none() {
        return refuse(StatusCode::BAD_REQUEST, "Bad request");
    }
    // Opening the page moves the marker "read up to". The session of "Log in as" changes nothing.
    if !ctx.viewer.session.impersonated {
        let _ = notify::mark_read(&ctx.app, &ctx.held.scope(&ctx.viewer.login)).await;
    }
    page(state, ctx, &query, notice, None, None).await
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
    refused: Option<SendError<'_>>,
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
        // The graph counts the groups that the account may read, as the rows do. A group with
        // `alert.send` only adds nothing, and an alert about a hidden service adds nothing.
        let counts =
            match admin::alert_counts(&ctx.app, since_ms, query.period.bucket_millis()).await {
                Ok(v) => v
                    .into_iter()
                    .filter(|count| {
                        ctx.held.may_read(&count.group)
                            && ctx.held.services.sees(count.service.as_deref())
                    })
                    .collect(),
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
    // A form that the server refused keeps its key: a second try sends one alert.
    let key = refused
        .as_ref()
        .map_or_else(random_key, |refused| refused.key.to_string());
    let header = header_for(&ctx.app, &ctx.held, &ctx.viewer.login).await;
    let (open_items_html, _) = items::open_panel(state, ctx, now_ms).await;
    let view = InboxView {
        open_items_html: &open_items_html,
        items_link: rights.items && !ctx.viewer.session.impersonated,
        notify_level: header.map(|h| h.level),
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
        send_error: refused,
    };
    let mut frame = ctx.frame(state);
    frame.notify = header;
    let html = ui_inbox::inbox_page(&frame, &view);
    let mut response = html_response(StatusCode::OK, html);
    if view.send_error.is_some() {
        *response.status_mut() = StatusCode::UNPROCESSABLE_ENTITY;
    }
    if view.code.is_some() || view.send_error.is_some() || !open_items_html.is_empty() {
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
) -> Result<(Context<'a>, std::collections::HashMap<String, String>, bool), HttpResponse> {
    let json = accepts_json(request.headers());
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
    let headers = request.headers().clone();
    let first = open(state, "", &headers, client)?;
    if !first.held.ui.any() {
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
    // The session can end while the body arrives: a deleted account must not make a code or send an alert. The
    // action needs the session and the rights of now.
    let ctx = open(state, "", &headers, client)?;
    if ctx.viewer.session.sid != first.viewer.session.sid {
        return Err(redirect_response(StatusCode::SEE_OTHER, LOGIN_PATH));
    }
    if !ctx.held.ui.any() {
        return Err(forbidden());
    }
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
    Ok((ctx, fields, json))
}

/// Whether the request asks for JSON: the script of the notifications does.
fn accepts_json(headers: &HeaderMap) -> bool {
    headers
        .get(hyper::header::ACCEPT)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.contains("application/json"))
}

async fn post(
    state: &Arc<AppState>,
    request: Request<Incoming>,
    client: &Client,
    rest: &str,
) -> HttpResponse {
    if !matches!(
        rest,
        "/send"
            | "/revoke"
            | "/code"
            | "/read"
            | "/notify"
            | "/items/answer"
            | "/items/reply"
            | "/items/cancel"
            | "/prefs"
    ) {
        return not_found();
    }
    let (ctx, fields, json) = match form(state, request, client).await {
        Ok(v) => v,
        Err(response) => return response,
    };
    match rest {
        "/send" => send(state, &ctx, fields).await,
        "/revoke" => revoke(state, &ctx, fields).await,
        "/read" => read(state, &ctx, fields, json).await,
        "/notify" => set_level(state, &ctx, fields, json).await,
        "/items/answer" => items::answer(state, &ctx, fields).await,
        "/items/reply" => items::reply(state, &ctx, fields).await,
        "/items/cancel" => items::cancel(state, &ctx, fields).await,
        "/prefs" => prefs::save(state, &ctx, fields).await,
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

/// The answer to a send form that the server refuses: the page, with the popup open, the title
/// that was typed and the reason. The status is 422.
async fn send_refused(
    state: &Arc<AppState>,
    ctx: &Context<'_>,
    code: &str,
    to: &str,
    title: &str,
    key: &str,
) -> HttpResponse {
    let refused = SendError {
        code,
        to,
        title,
        key,
    };
    page(state, ctx, &Query::default(), None, None, Some(refused)).await
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
    // A refusal shows the popup again with what the person typed.
    let (typed_to, typed_title) = (to.clone(), title.clone());
    let refuse = |code: &'static str| send_refused(state, ctx, code, &typed_to, &typed_title, &key);
    let Some(target) = parse_to(&to) else {
        return refuse("bad_target").await;
    };
    let title = title.trim().to_string();
    if title.is_empty() {
        return refuse("no_title").await;
    }
    if title.chars().count() > ui_inbox::MAX_TITLE || title.chars().any(char::is_control) {
        return refuse("bad_title").await;
    }
    let (action, chain, count_use) = match &target {
        Target::Group(group) => {
            if !ctx.held.ui.send.iter().any(|g| g.id == *group) {
                return refuse("bad_group").await;
            }
            let Some(chain) = actions::group_chain(group) else {
                return refuse("bad_group").await;
            };
            (actions::ALERT_SEND, chain, !ctx.app.is_critical(group))
        }
        Target::User(login) => {
            if !ctx.held.ui.send_user {
                return refuse("bad_target").await;
            }
            if !crate::keyauth::person_exists(state, login) {
                return refuse("bad_user").await;
            }
            // An account with no phone gets nothing, and the refused alert uses no budget.
            match admin::active_phone_counts(&ctx.app).await {
                Ok(phones) if phones.contains_key(login) => {}
                Ok(_) => return refuse("no_phone").await,
                Err(_) => return refuse("failed").await,
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
        UseVerdict::Limited(_) => return refuse("limited").await,
        UseVerdict::Denied | UseVerdict::UnknownSubject => {
            return refuse(if matches!(target, Target::Group(_)) {
                "bad_group"
            } else {
                "bad_target"
            })
            .await
        }
        UseVerdict::NotReady | UseVerdict::Unavailable => return refuse("failed").await,
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
        Err(e) if e.code() == "no_phone" => return refuse("no_phone").await,
        Err(_) => return refuse("failed").await,
    };
    if readable {
        // The page opens the new alert in place: the row is the answer. A repeat says so.
        let notice = if accepted.duplicate {
            "s=duplicate&"
        } else {
            ""
        };
        return redirect_response(
            StatusCode::SEE_OTHER,
            &format!(
                "{}?{notice}open={}#a-{}",
                ui_inbox::PAGE,
                accepted.alert_id,
                accepted.alert_id
            ),
        );
    }
    back_with(
        back,
        if accepted.duplicate {
            "duplicate"
        } else {
            "sent"
        },
    )
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
    // A revoked phone shows in its row: the page needs no banner.
    if revoked > 0 {
        return redirect_response(StatusCode::SEE_OTHER, back);
    }
    back_with(back, "unknown_device")
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
    // An account of the database gets a code only while its row exists: a delete of the account
    // at the same time leaves no code (see `inbox::Slot::create_enrollment_code`).
    let login = &ctx.viewer.login;
    let stored =
        *login != state.superadmin_login && !state.config.access.accounts.contains_key(login);
    let made = state.inbox.create_enrollment_code(login, stored).await;
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
                None,
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

// The notifications ---------------------------------------------------------------------------

/// The data that the badge of the tab and the script need. `None`: the account receives no
/// notification, or the store does not answer: the page then has no badge.
async fn header_for(app: &InboxApp, held: &Held, login: &str) -> Option<ui_notify::Header> {
    if !held.ui.any() {
        return None;
    }
    let ask = Ask {
        since_ms: 0,
        limit: 0,
        skip_if_off: true,
    };
    let view = notify::view(app, &held.scope(login), ask).await.ok()?;
    Some(ui_notify::Header {
        level: view.prefs.level,
        unread: view.unread,
        cursor_ms: unix_now() * 1000,
        to_answer: to_answer(app, held, login).await,
    })
}

/// The approvals of the account that wait for an answer. The count is for the badge of the tab.
async fn to_answer(app: &InboxApp, held: &Held, login: &str) -> u32 {
    use crate::inbox::page_items::{ListFilter, Loaded};
    let (true, Some(items)) = (held.ui.items, app.items()) else {
        return 0;
    };
    let filter = ListFilter {
        owner: Some(login.to_string()),
        open: Some(true),
        limit: 100,
        ..ListFilter::default()
    };
    let rows: Vec<Loaded> = items.list(filter).await.unwrap_or_default();
    rows.iter()
        .filter(|l| l.item.kind == inbox_core::proto::Kind::Approval)
        .count() as u32
}

/// The data of the badge for a settings page.
pub(super) async fn notify_header(state: &AppState, viewer: &Viewer) -> Option<ui_notify::Header> {
    let app = state.inbox.app()?;
    let held = held_light(state, viewer)?;
    header_for(&app, &held, &viewer.login).await
}

/// The row of the page Inbox for an alert, when the account can read it there. The page has the
/// zone "Sent" for an account with `alert.read` on a group, or with `alert.send_user`. The rule
/// is the one of [`may_read_alert`]: an alert for the account is readable by its target.
fn alert_href(held: &Held, target: &Target, id: &str) -> Option<String> {
    let readable = held.ui.sent
        && match target {
            Target::Group(group) => held.may_read(group),
            Target::User(_) => true,
        };
    readable.then(|| format!("{}?open={id}#a-{id}", ui_inbox::PAGE))
}

/// The name that the notifications show for an alert: the name of the group, or "Direct".
fn target_name(state: &AppState, target: &Target) -> String {
    match target {
        Target::Group(id) => state
            .config
            .inbox
            .as_ref()
            .and_then(|inbox| inbox.groups.get(id))
            .map_or_else(|| id.clone(), |group| group.name.clone()),
        Target::User(_) => notify::DIRECT_NAME.to_string(),
    }
}

/// What the launcher draws for the inbox icon.
pub(super) struct LauncherBell {
    pub header: ui_notify::Header,
    pub rows: Vec<ui_notify::Row>,
    pub csrf: String,
    pub now_ms: i64,
}

/// The icon of the launcher and its list. `None`: the account has no right of the inbox features,
/// or the store does not answer.
pub(super) async fn launcher_bell(state: &AppState, viewer: &Viewer) -> Option<LauncherBell> {
    let app = state.inbox.app()?;
    let held = held_light(state, viewer)?;
    if !held.ui.any() {
        return None;
    }
    let ask = Ask {
        since_ms: 0,
        limit: ui_notify::ROWS_SHOWN as u32,
        skip_if_off: false,
    };
    let view = notify::view(&app, &held.scope(&viewer.login), ask)
        .await
        .ok()?;
    let now_ms = unix_now() * 1000;
    let rows = view
        .items
        .iter()
        .map(|item| ui_notify::Row {
            id: item.id.clone(),
            title: item.title.clone(),
            group: target_name(state, &item.target),
            importance: Importance::of(&item.target, |g| app.is_critical(g)),
            created_at_ms: item.created_at,
            href: alert_href(&held, &item.target, &item.id),
            unread: item.created_at > view.prefs.read_up_to,
        })
        .collect();
    Some(LauncherBell {
        header: ui_notify::Header {
            level: view.prefs.level,
            unread: view.unread,
            cursor_ms: now_ms,
            to_answer: to_answer(&app, &held, &viewer.login).await,
        },
        rows,
        csrf: sign_action_token(&state.signing_secret, INBOX_TOKEN, &viewer.token).ok()?,
        now_ms,
    })
}

/// The answer of the poll and of the forms of the notifications: the level, the unread count, and
/// with `after` the alerts newer than this time. The alerts come from the store with the scope of
/// the account in the query, so no other alert leaves the database. An alert for the account is
/// level Ring: with the level "Alert and Critical only" it gives no toast, and it stays in the
/// list: each item has the flag `toast`.
#[allow(clippy::result_large_err)] // the error is the response, sent as is
async fn answer(
    state: &AppState,
    ctx: &Context<'_>,
    after: Option<i64>,
) -> Result<serde_json::Value, HttpResponse> {
    let ask = Ask {
        since_ms: after.map_or(0, |a| a.saturating_sub(notify::LOOKBACK_MS)),
        limit: if after.is_some() {
            notify::ITEMS_MAX
        } else {
            0
        },
        skip_if_off: true,
    };
    let scope = ctx.held.scope(&ctx.viewer.login);
    let view = notify::view(&ctx.app, &scope, ask)
        .await
        .map_err(|_| unavailable())?;
    let level = view.prefs.level;
    let mut cursor = after;
    let mut items = Vec::new();
    // The store gives the newest first. The browser wants the oldest first.
    for item in view.items.iter().rev() {
        cursor = cursor.max(Some(item.created_at));
        let importance = Importance::of(&item.target, |g| ctx.app.is_critical(g));
        items.push(serde_json::json!({
            "toast": importance.toasts_at(level),
            "id": item.id,
            "title": item.title,
            "group": target_name(state, &item.target),
            "level": importance.as_str(),
            "word": ui_notify::importance_word(importance),
            "at": item.created_at,
            "href": alert_href(&ctx.held, &item.target, &item.id),
        }));
    }
    let mut out = serde_json::json!({
        "level": level.as_str(),
        "unread": view.unread,
        "now": unix_now() * 1000,
        "items": items,
    });
    if let Some(cursor) = cursor {
        out["cursor"] = cursor.into();
    }
    Ok(out)
}

/// `GET /auth/settings/inbox/notifications?after=<ms>`: the poll. The query has the one field
/// `after`, whole milliseconds. Without it the answer has the count and the level only.
async fn poll_answer(state: &AppState, ctx: &Context<'_>) -> HttpResponse {
    if !ctx.held.ui.any() {
        return forbidden();
    }
    let Some(pairs) = query_pairs(ctx.uri) else {
        return bad_request();
    };
    let mut after = None;
    for (name, value) in &pairs {
        let parsed = (name == "after" && after.is_none())
            .then(|| {
                value
                    .parse::<i64>()
                    .ok()
                    .filter(|v| *v >= 0 && value.len() <= 16)
            })
            .flatten();
        match parsed {
            Some(value) => after = Some(value),
            None => return bad_request(),
        }
    }
    match answer(state, ctx, after).await {
        Ok(body) => json_response(StatusCode::OK, body.to_string()),
        Err(response) => response,
    }
}

/// The page that a form of the notifications returns to: the field `back`.
fn back_target(fields: &mut std::collections::HashMap<String, String>) -> Option<&'static str> {
    match fields.remove("back").as_deref() {
        None | Some(ui_notify::BACK_INBOX) => Some(ui_inbox::PAGE),
        Some(ui_notify::BACK_HOME) => Some("/"),
        Some(_) => None,
    }
}

/// The reply to a form of the notifications: the state as JSON for the script, a redirect for a
/// browser without script.
async fn reply(state: &AppState, ctx: &Context<'_>, json: bool, back: &str) -> HttpResponse {
    if !json {
        return redirect_response(StatusCode::SEE_OTHER, back);
    }
    match answer(state, ctx, None).await {
        Ok(body) => json_response(StatusCode::OK, body.to_string()),
        Err(response) => response,
    }
}

/// `POST /auth/settings/inbox/read`: moves the marker "read up to" of the account to its newest
/// alert. The marker never goes back. The session of "Log in as" cannot do it.
async fn read(
    state: &Arc<AppState>,
    ctx: &Context<'_>,
    mut fields: std::collections::HashMap<String, String>,
    json: bool,
) -> HttpResponse {
    let Some(back) = back_target(&mut fields) else {
        return bad_request();
    };
    if !fields.is_empty() {
        return bad_request();
    }
    if ctx.viewer.session.impersonated {
        return forbidden();
    }
    if notify::mark_read(&ctx.app, &ctx.held.scope(&ctx.viewer.login))
        .await
        .is_err()
    {
        return unavailable();
    }
    reply(state, ctx, json, back).await
}

/// `POST /auth/settings/inbox/notify`: sets the level of the toasts of the account: `all`,
/// `important` or `off`.
async fn set_level(
    state: &Arc<AppState>,
    ctx: &Context<'_>,
    mut fields: std::collections::HashMap<String, String>,
    json: bool,
) -> HttpResponse {
    let Some(back) = back_target(&mut fields) else {
        return bad_request();
    };
    let Some(level) = fields.remove("level").as_deref().and_then(Level::parse) else {
        return bad_request();
    };
    if !fields.is_empty() {
        return bad_request();
    }
    if ctx.viewer.session.impersonated {
        return forbidden();
    }
    if notify::set_level(&ctx.app, &ctx.viewer.login, level)
        .await
        .is_err()
    {
        return unavailable();
    }
    log(
        state,
        LogLevel::Info,
        "inbox_notify_level",
        &[
            ("actor", ctx.viewer.login.clone()),
            ("level", level.as_str().to_string()),
        ],
    );
    reply(state, ctx, json, back).await
}
