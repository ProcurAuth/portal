//! The pages of the tab "Inbox" and their forms, under `/auth/settings/inbox`.
//!
//! The pages need a session. The rights come from the service `inbox` (see
//! [`crate::inbox_actions`]): the superadmin has all of them, and any other account has the
//! permissions that it holds. A page that the account may not open answers 403, and a page of a
//! block `inbox` that does not exist answers 404. The pages call the inbox in process, with no
//! HTTP call to itself.
//!
//! A form is a POST with the field `csrf` (the token of the purpose `inbox`). It answers with a
//! redirect to the page that shows the outcome as a code, except the form of an enrollment code:
//! it answers with the page that shows the code one time, with `Cache-Control: no-store`.
//! A session that "Log in as" opens can read the pages and make a code for the account. It does
//! not send an alert and does not revoke a phone.

use super::pdp::{decide_person_for, UseVerdict};
use super::*;
use crate::inbox::admin::{self, DeviceRow};
use crate::inbox::App as InboxApp;
use crate::inbox_actions as actions;
use crate::portal_actions as portal;
use crate::ui::inbox as ui_inbox;
use crate::ui::inbox::{GroupChoice, GroupLine, Rights};

pub(super) const PREFIX: &str = actions::PAGE;
/// The purpose of the anti-CSRF tokens of the forms of the inbox.
const INBOX_TOKEN: &str = "inbox";
/// The largest body of a form of the inbox.
const MAX_FORM_BYTES: usize = 16 * 1024;
/// The lines of the last alerts, and of the connection log.
const ALERTS_SHOWN: u32 = 20;
const CONNECTIONS_SHOWN: u32 = 100;

/// Whether the path belongs to the pages of the inbox.
pub(super) fn is_inbox_path(path: &str) -> bool {
    path == PREFIX
        || path
            .strip_prefix(PREFIX)
            .is_some_and(|r| r.starts_with('/'))
}

/// What an account holds in the service `inbox`.
pub(super) struct Held {
    pub ui: Rights,
    /// The groups with `alert.read`. `None`: all the groups (the superadmin).
    read_groups: Option<Vec<String>>,
}

/// The rights of the viewer. `None`: the file has no block `inbox`.
pub(super) fn held(state: &AppState, viewer: &Viewer) -> Option<Held> {
    let inbox = state.config.inbox.as_ref()?;
    let ids: Vec<String> = inbox.groups.keys().cloned().collect();
    let choice = |id: &String| GroupChoice {
        id: id.clone(),
        name: inbox.groups[id].name.clone(),
    };
    let Some(grants) = &viewer.grants else {
        // The superadmin has every right.
        return Some(Held {
            ui: Rights {
                overview: true,
                devices: true,
                revoke: true,
                connections: true,
                phones: true,
                send: ids.iter().map(choice).collect(),
            },
            read_groups: None,
        });
    };
    let document = state.actions.declared(actions::SERVICE)?;
    let login = viewer.login.as_str();
    let on =
        |action: &str| actions::groups_with(&document, login, grants, action, ids.iter().cloned());
    let at_service = |action: &str| actions::allows(&document, login, grants, action, None);
    let send_ids = on(actions::ALERT_SEND);
    let read_ids = on(actions::ALERT_READ);
    let devices = at_service(actions::DEVICE_READ);
    let connections = at_service(actions::CONNECTION_READ);
    Some(Held {
        ui: Rights {
            overview: !read_ids.is_empty() || devices || connections || !send_ids.is_empty(),
            devices,
            revoke: devices && at_service(actions::DEVICE_REVOKE),
            connections,
            phones: !on(actions::GROUP_SUBSCRIBE).is_empty(),
            send: send_ids.iter().map(choice).collect(),
        },
        read_groups: Some(read_ids),
    })
}

impl Held {
    fn may_read(&self, group: &str) -> bool {
        self.read_groups
            .as_ref()
            .is_none_or(|groups| groups.iter().any(|g| g == group))
    }

    /// Whether the viewer reads the alerts of at least one group.
    fn reads_alerts(&self) -> bool {
        self.read_groups
            .as_ref()
            .is_none_or(|groups| !groups.is_empty())
    }
}

/// Whether the viewer sees the tab "Inbox".
pub(super) fn tab_visible(state: &AppState, viewer: &Viewer) -> bool {
    held(state, viewer).is_some_and(|held| held.ui.any())
}

/// Whether the viewer sees the tab "Keys" in the pages of the inbox.
fn keys_tab(state: &AppState, viewer: &Viewer) -> bool {
    if state.config.keys.is_none() {
        return false;
    }
    match &viewer.grants {
        None => true,
        Some(grants) => {
            portal::effective(grants, portal::KEYS_OWN_READ)
                || portal::effective(grants, portal::KEYS_ALL_READ)
        }
    }
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
}

impl Context<'_> {
    fn frame(&self, state: &AppState) -> ui::Frame<'_> {
        ui::Frame {
            logout_token: &self.logout_token,
            keys: keys_tab(state, &self.viewer),
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
    })
}

/// A random text of 32 hexadecimal characters: the key of a form that sends an alert.
fn random_key() -> String {
    let bytes: [u8; 16] = inbox_core::rng::Rng::os().bytes();
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn group_lines(overview: &admin::Overview) -> Vec<GroupLine> {
    overview
        .groups
        .iter()
        .map(|g| GroupLine {
            id: g.id.clone(),
            name: g.name.clone(),
            description: g.description.clone(),
            critical: g.critical,
            subscribed: g.subscribed,
        })
        .collect()
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

async fn get(state: &Arc<AppState>, ctx: &Context<'_>, rest: &str) -> HttpResponse {
    let rights = &ctx.held.ui;
    if !rights.any() {
        return forbidden();
    }
    let notice = query_value(ctx.uri, "s").and_then(|code| ui_inbox::notice_for(&code));
    if query_value(ctx.uri, "s").is_some() && notice.is_none() {
        return bad_request();
    }
    let frame = ctx.frame(state);
    let html = match rest {
        "" if !rights.overview => {
            return redirect_response(StatusCode::SEE_OTHER, ui_inbox::PHONES_PATH)
        }
        "" => {
            let overview = match admin::overview(&ctx.app).await {
                Ok(overview) => overview,
                Err(_) => return unavailable(),
            };
            let alerts = if ctx.held.reads_alerts() {
                let only = ctx.held.read_groups.as_deref();
                match admin::alerts(&ctx.app, ALERTS_SHOWN, only).await {
                    Ok(list) => Some(list),
                    Err(_) => return unavailable(),
                }
            } else {
                None
            };
            let key = random_key();
            ui_inbox::overview_page(
                &frame,
                rights,
                &ui_inbox::OverviewView {
                    notice,
                    csrf: &ctx.inbox_token,
                    stats: rights.devices.then_some(ui_inbox::Stats {
                        enrolled: overview.enrolled,
                        online: overview.online,
                        unreachable: overview.unreachable,
                        pending: overview.pending_deliveries,
                        oldest_pending_ms: overview.oldest_pending_ms,
                    }),
                    groups: group_lines(&overview),
                    alerts,
                    idempotency_key: &key,
                },
            )
        }
        "/devices" => {
            if !rights.devices {
                return forbidden();
            }
            let overview = match admin::overview(&ctx.app).await {
                Ok(overview) => overview,
                Err(_) => return unavailable(),
            };
            ui_inbox::devices_page(
                &frame,
                rights,
                &ui_inbox::DevicesView {
                    notice,
                    csrf: &ctx.inbox_token,
                    rows: sorted(overview.devices),
                    can_revoke: rights.revoke && !ctx.viewer.session.impersonated,
                    back: ui_inbox::DEVICES_PATH,
                },
            )
        }
        "/phones" => {
            if !rights.phones {
                return forbidden();
            }
            let overview = match admin::overview(&ctx.app).await {
                Ok(overview) => overview,
                Err(_) => return unavailable(),
            };
            let mine: Vec<DeviceRow> = overview
                .devices
                .into_iter()
                .filter(|row| row.device.user_name == ctx.viewer.login)
                .collect();
            ui_inbox::phones_page(
                &frame,
                rights,
                &ui_inbox::PhonesView {
                    devices: ui_inbox::DevicesView {
                        notice,
                        csrf: &ctx.inbox_token,
                        rows: sorted(mine),
                        can_revoke: !ctx.viewer.session.impersonated,
                        back: ui_inbox::PHONES_PATH,
                    },
                    can_add: true,
                },
            )
        }
        "/connections" => {
            if !rights.connections {
                return forbidden();
            }
            let login = query_value_up_to(ctx.uri, "login", 64).unwrap_or_default();
            let rows = match admin::connections(
                &ctx.app,
                CONNECTIONS_SHOWN,
                (!login.is_empty()).then(|| login.clone()),
            )
            .await
            {
                Ok(rows) => rows,
                Err(_) => return unavailable(),
            };
            ui_inbox::connections_page(
                &frame,
                rights,
                &ui_inbox::ConnectionsView {
                    login: &login,
                    rows,
                    limit: CONNECTIONS_SHOWN,
                },
            )
        }
        "/doc" => {
            let overview = match admin::overview(&ctx.app).await {
                Ok(overview) => overview,
                Err(_) => return unavailable(),
            };
            let address = ctx.app.config.devices.public_address.trim_end_matches('/');
            let base = if address.starts_with("http://") || address.starts_with("https://") {
                address.to_string()
            } else {
                format!("http://{address}")
            };
            ui_inbox::doc_page(
                &frame,
                rights,
                &ui_inbox::DocView {
                    base,
                    groups: group_lines(&overview),
                    keys: state.config.keys.is_some(),
                    keys_page: keys_tab(state, &ctx.viewer),
                },
            )
        }
        "/send" | "/revoke" | "/code" => {
            return text_response(StatusCode::METHOD_NOT_ALLOWED, "Method Not Allowed\n")
        }
        other => {
            let Some(id) = other
                .strip_prefix("/alerts/")
                .filter(|id| !id.is_empty() && !id.contains('/'))
            else {
                return not_found();
            };
            let detail = match admin::alert(&ctx.app, id).await {
                Ok(Some(detail)) => detail,
                Ok(None) => return not_found(),
                Err(_) => return unavailable(),
            };
            // An alert of a group that the account may not read is not there for the account.
            if !ctx.held.may_read(&detail.summary.group) {
                return not_found();
            }
            let name = ctx.app.group_name(&detail.summary.group);
            ui_inbox::alert_page(
                &frame,
                &ui_inbox::AlertView {
                    notice,
                    detail: &detail,
                    group_name: &name,
                },
            )
        }
    };
    html_response(StatusCode::OK, html)
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

/// `POST /auth/settings/inbox/send`: the title and the group of an alert. The account needs
/// `alert.send` on the group. The decision counts one use, as for a key, unless the group is
/// critical.
async fn send(
    state: &Arc<AppState>,
    ctx: &Context<'_>,
    mut fields: std::collections::HashMap<String, String>,
) -> HttpResponse {
    let (Some(group), Some(title), Some(key)) = (
        fields.remove("group"),
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
    let back = ui_inbox::OVERVIEW_PATH;
    if !ctx.held.ui.send.iter().any(|g| g.id == group) {
        return back_with(back, "bad_group");
    }
    let title = title.trim().to_string();
    if title.is_empty() {
        return back_with(back, "no_title");
    }
    if title.chars().count() > ui_inbox::MAX_TITLE || title.chars().any(char::is_control) {
        return back_with(back, "bad_title");
    }
    let Some(chain) = actions::group_chain(&group) else {
        return back_with(back, "bad_group");
    };
    let count_use = !ctx.app.is_critical(&group);
    let verdict = {
        let (state, login) = (Arc::clone(state), ctx.viewer.login.clone());
        decide_person_for(
            &state,
            &login,
            actions::SERVICE,
            actions::ALERT_SEND,
            &chain,
            count_use,
        )
    };
    match verdict {
        UseVerdict::Allowed => {}
        UseVerdict::Limited(_) => return back_with(back, "limited"),
        UseVerdict::Denied | UseVerdict::UnknownSubject => return back_with(back, "bad_group"),
        UseVerdict::NotReady | UseVerdict::Unavailable => return back_with(back, "failed"),
    }
    log(
        state,
        LogLevel::Info,
        "inbox_admin_send",
        &[
            ("actor", ctx.viewer.login.clone()),
            ("group", group.clone()),
        ],
    );
    let accepted =
        match admin::send_alert(&ctx.app, group.clone(), title, format!("admin:{key}")).await {
            Ok(accepted) => accepted,
            Err(_) => return back_with(back, "failed"),
        };
    let code = if accepted.duplicate {
        "duplicate"
    } else {
        "sent"
    };
    if ctx.held.may_read(&group) {
        return redirect_response(
            StatusCode::SEE_OTHER,
            &format!(
                "{}{}?s={code}",
                ui_inbox::ALERT_PATH_PREFIX,
                accepted.alert_id
            ),
        );
    }
    back_with(back, code)
}

/// `POST /auth/settings/inbox/revoke`: a phone. The account needs `device.revoke` and
/// `device.read`, or it owns the phone and holds `group.subscribe` on a group.
async fn revoke(
    state: &Arc<AppState>,
    ctx: &Context<'_>,
    mut fields: std::collections::HashMap<String, String>,
) -> HttpResponse {
    let (Some(id), Some(back)) = (fields.remove("id"), fields.remove("back")) else {
        return bad_request();
    };
    if !fields.is_empty() {
        return bad_request();
    }
    let back = match back.as_str() {
        ui_inbox::DEVICES_PATH => ui_inbox::DEVICES_PATH,
        ui_inbox::PHONES_PATH => ui_inbox::PHONES_PATH,
        _ => return bad_request(),
    };
    if id.len() != 32 || !id.bytes().all(|b| b.is_ascii_hexdigit()) {
        return bad_request();
    }
    if ctx.viewer.session.impersonated {
        return forbidden();
    }
    let owner = match admin::owner_of(&ctx.app, &id).await {
        Ok(owner) => owner,
        Err(_) => return back_with(back, "failed"),
    };
    let may = ctx.held.ui.revoke
        || (ctx.held.ui.phones && owner.as_deref() == Some(ctx.viewer.login.as_str()));
    // The same answer for a phone that does not exist and for a phone of another account.
    if owner.is_none() || !may {
        return back_with(back, "unknown_device");
    }
    match admin::revoke(&ctx.app, &id).await {
        Ok(_) => {
            log(
                state,
                LogLevel::Info,
                "inbox_admin_revoke",
                &[("actor", ctx.viewer.login.clone()), ("device", id)],
            );
            back_with(back, "revoked")
        }
        Err(_) => back_with(back, "failed"),
    }
}

/// `POST /auth/settings/inbox/code`: a new enrollment code for the account of the session. The
/// page shows the code one time.
async fn code(
    state: &Arc<AppState>,
    ctx: &Context<'_>,
    fields: std::collections::HashMap<String, String>,
) -> HttpResponse {
    if !fields.is_empty() {
        return bad_request();
    }
    if !ctx.held.ui.phones {
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
            let frame = ctx.frame(state);
            let page = ui_inbox::code_page(
                &frame,
                &ctx.held.ui,
                &ui_inbox::CodeView {
                    code: &code.code,
                    fingerprint: &code.fingerprint,
                    expires_at_ms: code.expires_at_ms,
                },
            );
            let mut response = html_response(StatusCode::OK, page);
            response.headers_mut().insert(
                HeaderName::from_static("cache-control"),
                HeaderValue::from_static("no-store"),
            );
            response
        }
        Some(Err(e)) if e.status() == StatusCode::TOO_MANY_REQUESTS => {
            back_with(ui_inbox::PHONES_PATH, "too_many_codes")
        }
        Some(Err(_)) => back_with(ui_inbox::PHONES_PATH, "failed"),
        None => not_found(),
    }
}
