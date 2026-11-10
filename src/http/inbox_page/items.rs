//! The pages of the items under `/auth/settings/inbox`: the panel "Open items" of the main page, the page History at
//! `/items`, the page of one request at `/items/{id}`, the page of the limits at `/limits`, the forms that answer, reply
//! and cancel, and the route of the administrator that reads a content.
//!
//! The owner of a request reads its content and answers. An account with `approval.read` reads the metadata of all the
//! requests, and an account with `approval.cancel` cancels an open one. The content for an administrator exists only
//! with `inbox.at_rest.admin_body_read = true`, only for the superadmin, and each read writes one audit line. A session
//! that the superadmin opens as another account reads no item and answers nothing.
//!
//! Every page that shows a content carries `Cache-Control: no-store`.

use std::time::Duration;

use inbox_core::effect::{resolve_view, View};
use inbox_core::proto::{Action, ExplanationFormat, Intent, Kind, Language};

use super::*;
use crate::inbox::page_items::{
    AgentTurn, Answer, AnswerVia, Answered, Caller, CancelBy, Cancelled, ItemError, Items,
    ListFilter, Loaded, OwnerAnswer, Served, TurnOut,
};
use crate::ui::items::{
    self as ui_items, Body, Detail, History, HistoryQuery, Line, Mode, OpenItems, StateFilter,
    Text, Title, Turn, TurnText,
};

/// The requests that the History reads, at most.
const HISTORY_ROWS: u32 = 500;
/// The requests that the panel "Open items" reads, at most.
const OPEN_ROWS: u32 = 100;
const DAY_MS: i64 = 86_400_000;
const CONSUME_WITHIN: Duration = Duration::from_secs(300);

pub(super) fn no_store(mut response: HttpResponse) -> HttpResponse {
    response.headers_mut().insert(
        HeaderName::from_static("cache-control"),
        HeaderValue::from_static("no-store"),
    );
    response
}

/// Whether the engine of the items runs.
fn engine(ctx: &Context<'_>) -> bool {
    ctx.app.items().is_some()
}

/// Whether the viewer sees the pages of the items. A session that the superadmin opens as another account does not.
pub(super) fn allowed(ctx: &Context<'_>) -> bool {
    ctx.held.ui.items && !ctx.viewer.session.impersonated && engine(ctx)
}

pub(super) fn id_ok(id: &str) -> bool {
    id.len() == 32 && id.bytes().all(|b| b.is_ascii_hexdigit())
}

/// The text of an approval from its unsealed body: the view with the values of `op`, the language and the explanation.
fn parse_text(plain: &[u8]) -> Option<Text> {
    let value: serde_json::Value = serde_json::from_slice(plain).ok()?;
    let view: View = serde_json::from_value(value.get("view")?.clone()).ok()?;
    let resolved = resolve_view(value.get("op")?, &view).ok()?;
    let language = value
        .get("language")
        .and_then(|l| serde_json::from_value::<Language>(l.clone()).ok())
        .unwrap_or_default();
    let format = value
        .get("explanation_format")
        .and_then(|f| serde_json::from_value::<ExplanationFormat>(f.clone()).ok())
        .unwrap_or(ExplanationFormat::Plain);
    Some(Text {
        title: resolved.title,
        fields: resolved
            .fields
            .into_iter()
            .map(|f| (f.label, f.value))
            .collect(),
        command: resolved.body,
        command_label: resolved.body_label,
        language,
        explanation: value
            .get("explanation")
            .and_then(|e| e.as_str())
            .map(str::to_string),
        format,
    })
}

/// The body of a request for its owner.
pub(super) async fn body_of(items: &Items<'_>, l: &Loaded) -> Result<Body, ItemError> {
    Ok(match items.serve_body(l).await? {
        Served::Body(plain) => parse_text(&plain).map_or(Body::Mismatch, Body::Shown),
        Served::Purged => Body::Purged,
        Served::Mismatch => Body::Mismatch,
        Served::NotFound => Body::Hidden,
    })
}

/// The name of a key for a page. A key that does not exist any more has its short id.
pub(super) fn key_text(state: &AppState, key_id: &str) -> String {
    key_name(state, key_id).unwrap_or_else(|| {
        let short: String = key_id.chars().take(8).collect();
        format!("key {short}")
    })
}

fn key_label(state: &AppState, l: &Loaded) -> String {
    key_text(state, &l.item.requester_key_id)
}

// The panel -----------------------------------------------------------------------------------------

/// The panel "Open items" for the main page: the HTML, or an empty text when nothing waits and no agent is new. The count is
/// for the badge of the tab. The requests are in one group for each agent.
pub(super) async fn open_panel(state: &AppState, ctx: &Context<'_>, now_ms: i64) -> (String, u32) {
    let (Some(items), true) = (ctx.app.items(), allowed(ctx)) else {
        return (String::new(), 0);
    };
    let filter = ListFilter {
        owner: Some(ctx.viewer.login.clone()),
        open: Some(true),
        limit: OPEN_ROWS,
        agent_id: None,
        ..ListFilter::default()
    };
    let Ok(rows) = items.list(filter).await else {
        return (String::new(), 0);
    };
    // The questions have their own page in a later version: the panel lists the approvals.
    let rows: Vec<Loaded> = rows
        .into_iter()
        .filter(|l| l.item.kind == Kind::Approval)
        .collect();
    let count = rows.len() as u32;
    let Some(roster) = super::agents::Roster::of(&items, &ctx.viewer.login, None).await else {
        return (String::new(), 0);
    };
    let mut groups: Vec<ui_items::Group> = Vec::new();
    for l in &rows {
        let (title, first_line) = match body_of(&items, l).await {
            Ok(Body::Shown(t)) => {
                let first = t
                    .command
                    .lines()
                    .next()
                    .unwrap_or_default()
                    .chars()
                    .take(200)
                    .collect();
                (Title::Text(t.title), Some(first))
            }
            Ok(Body::Purged) => (Title::Purged, None),
            _ => (Title::Hidden, None),
        };
        let line = Line {
            loaded: l,
            title,
            first_line,
            key_name: key_label(state, l),
            agent: None,
        };
        match groups
            .iter_mut()
            .find(|g| g.agent.agent_id == l.item.agent_id)
        {
            Some(g) => g.lines.push(line),
            None => {
                let agent = match roster.find(&l.item.agent_id) {
                    Some(s) => roster.view(state, &items, &s.agent, true),
                    None => roster.fallback(state, l),
                };
                groups.push(ui_items::Group {
                    agent,
                    lines: vec![line],
                });
            }
        }
    }
    let notices = super::agents::notices(state, ctx, &items, &roster, now_ms).await;
    let stale_after_ms = ctx
        .app
        .config
        .items
        .as_ref()
        .map_or(i64::MAX, |c| c.stale_after.as_millis() as i64);
    let html = ui_items::open_items(&OpenItems {
        groups,
        notices,
        lock: super::agents::lock_new(ctx),
        agents_link: super::agents::agents_on(ctx),
        stale_after_ms,
        now_ms,
    });
    (html, count)
}

// History ---------------------------------------------------------------------------------------------

pub(super) async fn history(state: &Arc<AppState>, ctx: &Context<'_>) -> HttpResponse {
    let refuse = |status: StatusCode, text: &str| {
        html_response(status, ui_inbox::status_page(&ctx.frame(state), text))
    };
    if !allowed(ctx) {
        return refuse(StatusCode::FORBIDDEN, "Forbidden");
    }
    let (Some(pairs), Some(items)) = (query_pairs(ctx.uri), ctx.app.items()) else {
        return refuse(StatusCode::BAD_REQUEST, "Bad request");
    };
    let Some(query) = HistoryQuery::parse(&pairs) else {
        return refuse(StatusCode::BAD_REQUEST, "Bad request");
    };
    let notice = pairs
        .iter()
        .find(|(n, _)| n == "s")
        .and_then(|(_, c)| ui_items::notice_for(c));
    let all = ctx.held.approval_read;
    // The filter `agent=` names an agent. A merged agent gives its target.
    let (agent_id, agent_filter) = match &query.agent {
        Some(id) => {
            let (id, name) = super::agents::history_filter(state, ctx, &items, id, all).await;
            (Some(id), Some(name))
        }
        None => (None, None),
    };
    let filter = ListFilter {
        owner: (!all).then(|| ctx.viewer.login.clone()),
        open: Some(false),
        limit: HISTORY_ROWS,
        agent_id,
        ..ListFilter::default()
    };
    let Ok(rows) = items.list(filter).await else {
        return unavailable();
    };
    let now_ms = unix_now() * 1000;
    let since = now_ms - i64::from(query.days) * DAY_MS;
    let key_part = query.key.to_lowercase();
    let svc_part = query.service.to_lowercase();
    let truncated = rows.len() as u32 >= HISTORY_ROWS;
    let rows: Vec<Loaded> = rows
        .into_iter()
        .filter(|l| l.item.kind == Kind::Approval && l.item.created_at >= since)
        .filter(|l| key_part.is_empty() || key_label(state, l).to_lowercase().contains(&key_part))
        .filter(|l| {
            svc_part.is_empty()
                || l.approval
                    .as_ref()
                    .is_some_and(|a| a.service.to_lowercase().contains(&svc_part))
        })
        .collect();
    let counts: Vec<(StateFilter, usize)> = StateFilter::ALL
        .iter()
        .map(|s| {
            (
                *s,
                rows.iter()
                    .filter(|l| StateFilter::of(l) == Some(*s))
                    .count(),
            )
        })
        .collect();
    let total = rows.len();
    let shown: Vec<&Loaded> = rows
        .iter()
        .filter(|l| query.state.is_none() || StateFilter::of(l) == query.state)
        .collect();
    let tags = super::agents::tags(state, &items, &rows).await;
    let mut lines: Vec<(Line, bool)> = Vec::new();
    for (l, chained) in chain(&shown) {
        let title = if l.item.requester_owner != ctx.viewer.login {
            Title::Hidden
        } else {
            match body_of(&items, l).await {
                Ok(Body::Shown(t)) => Title::Text(t.title),
                Ok(Body::Purged) => Title::Purged,
                _ => Title::Hidden,
            }
        };
        lines.push((
            Line {
                loaded: l,
                title,
                first_line: None,
                key_name: key_label(state, l),
                agent: tags.get(&l.item.agent_id).map(|t| ui_items::AgentTag {
                    agent_id: t.agent_id.clone(),
                    label: t.label.clone(),
                    plain: t.plain,
                }),
            },
            chained,
        ));
    }
    let h = History {
        lines,
        counts,
        total,
        query,
        now_ms,
        all_accounts: all,
        truncated,
        limits_link: ctx.held.approval_read,
        agents_link: super::agents::agents_on(ctx),
        agent_filter,
    };
    let header = header_for(&ctx.app, &ctx.held, &ctx.viewer.login).await;
    let mut frame = ctx.frame(state);
    frame.notify = header;
    no_store(html_response(
        StatusCode::OK,
        ui_items::history_page(&frame, &h, notice),
    ))
}

/// The rows in the order of the page. A request that follows an earlier one (`supersedes` or `previous_id`) stands right
/// after it, and the flag says so. The list is newest first, so a chain reads from its first request to its last.
fn chain<'a>(rows: &[&'a Loaded]) -> Vec<(&'a Loaded, bool)> {
    let ids: std::collections::HashSet<&str> =
        rows.iter().map(|l| l.item.request_id.as_str()).collect();
    let before = |l: &Loaded| -> Option<String> {
        l.item
            .supersedes
            .clone()
            .or_else(|| l.item.previous_id.clone())
            .filter(|p| ids.contains(p.as_str()))
    };
    let mut out: Vec<(&Loaded, bool)> = Vec::new();
    fn push<'a>(
        out: &mut Vec<(&'a Loaded, bool)>,
        rows: &[&'a Loaded],
        before: &dyn Fn(&Loaded) -> Option<String>,
        root: &'a Loaded,
        chained: bool,
    ) {
        out.push((root, chained));
        let mut next: Vec<&&Loaded> = rows
            .iter()
            .filter(|l| before(l).as_deref() == Some(root.item.request_id.as_str()))
            .collect();
        next.sort_by_key(|l| l.item.created_at);
        for l in next {
            push(out, rows, before, l, true);
        }
    }
    for l in rows.iter().filter(|l| before(l).is_none()) {
        push(&mut out, rows, &before, l, false);
    }
    out
}

// One request -----------------------------------------------------------------------------------------

fn turns_of(items_turns: Vec<crate::inbox::page_items::TurnPlain>) -> Vec<Turn> {
    items_turns
        .into_iter()
        .map(|t| {
            let agent = t.row.author == "agent";
            let text = match (&t.text, t.row.payload.is_some()) {
                (Some(plain), _) if agent => {
                    let turn = AgentTurn::from_plain(plain);
                    TurnText::Agent {
                        explanation: turn.explanation,
                        format: turn.explanation_format.unwrap_or(ExplanationFormat::Plain),
                        command: turn
                            .command
                            .map(|c| (c.text, c.language.unwrap_or_default())),
                    }
                }
                (Some(plain), _) if matches!(t.row.kind.as_str(), "answer" | "decline") => {
                    let a = OwnerAnswer::from_plain(plain);
                    match a.text {
                        Some(text) => TurnText::Plain(text),
                        None => TurnText::Empty,
                    }
                }
                (Some(plain), _) => TurnText::Plain(String::from_utf8_lossy(plain).into_owned()),
                (None, true) => TurnText::Unreadable,
                (None, false) => TurnText::Empty,
            };
            let kind = match t.row.kind.as_str() {
                "answer" => "Approved",
                "decline" => "Declined",
                "revive" => "The agent is back",
                other => other,
            }
            .to_string();
            Turn {
                agent,
                at: t.row.created_at,
                kind,
                via: t.row.via.clone(),
                text,
            }
        })
        .collect()
}

/// The page of one request: `rest` is the id. `body` is the route of the administrator.
pub(super) async fn detail(
    state: &Arc<AppState>,
    ctx: &Context<'_>,
    id: &str,
    admin_body: bool,
    notice: Option<crate::ui::settings::Notice>,
    status: StatusCode,
) -> HttpResponse {
    let refuse = |status: StatusCode, text: &str| {
        html_response(status, ui_inbox::status_page(&ctx.frame(state), text))
    };
    if !id_ok(id) {
        return not_found();
    }
    // The route of the administrator exists only with the switch of the file, and only for the superadmin.
    if admin_body
        && !(ctx
            .app
            .config
            .at_rest
            .as_ref()
            .is_some_and(|a| a.admin_body_read)
            && is_superadmin(&ctx.viewer))
    {
        return not_found();
    }
    if !allowed(ctx) {
        return if ctx.viewer.session.impersonated {
            refuse(StatusCode::FORBIDDEN, "Forbidden")
        } else {
            not_found()
        };
    }
    let Some(items) = ctx.app.items() else {
        return not_found();
    };
    let Some(pairs) = query_pairs(ctx.uri) else {
        return refuse(StatusCode::BAD_REQUEST, "Bad request");
    };
    let mut full = false;
    let mut code = None;
    for (name, value) in &pairs {
        match (name.as_str(), value.as_str()) {
            ("full", "1") => full = true,
            ("s", c) => code = Some(c.to_string()),
            _ => return refuse(StatusCode::BAD_REQUEST, "Bad request"),
        }
    }
    let notice = notice.or_else(|| code.as_deref().and_then(ui_items::notice_for));
    let own = match items
        .get(id, &Caller::Owner(ctx.viewer.login.clone()))
        .await
    {
        Ok(found) => found,
        Err(_) => return unavailable(),
    };
    let (loaded, mut mode) = match own {
        Some(l) => (l, Mode::Owner),
        None if ctx.held.approval_read || admin_body => {
            match items.get(id, &Caller::System).await {
                Ok(Some(l)) => (l, Mode::Admin),
                Ok(None) => return not_found(),
                Err(_) => return unavailable(),
            }
        }
        None => return not_found(),
    };
    if loaded.item.kind != Kind::Approval {
        return not_found();
    }
    let body = if admin_body {
        match items.serve_body(&loaded).await {
            Ok(Served::NotFound) => return not_found(),
            Ok(Served::Body(plain)) => {
                // One audit line and one count for each read. The line holds no content.
                if items
                    .record_body_read(id, loaded.item.kind, &ctx.viewer.login)
                    .await
                    .is_err()
                {
                    return unavailable();
                }
                mode = Mode::AdminBody;
                parse_text(&plain).map_or(Body::Mismatch, Body::Shown)
            }
            Ok(Served::Purged) => Body::Purged,
            Ok(Served::Mismatch) => Body::Mismatch,
            Err(_) => return unavailable(),
        }
    } else if mode == Mode::Owner {
        match body_of(&items, &loaded).await {
            Ok(body) => body,
            Err(_) => return unavailable(),
        }
    } else {
        Body::Hidden
    };
    let (turns, previous) = if mode == Mode::Owner {
        let turns = match items.serve_turns(id).await {
            Ok(t) => turns_of(t),
            Err(_) => return unavailable(),
        };
        (turns, items.previous_approval(&loaded).await.ok().flatten())
    } else {
        (Vec::new(), None)
    };
    let approvals = ctx.app.config.approvals.as_ref();
    let sender = super::agents::sender(state, ctx, &items, &loaded, mode == Mode::Owner).await;
    let d = Detail {
        loaded: &loaded,
        body,
        sender,
        turns,
        previous,
        now_ms: unix_now() * 1000,
        stale_after_ms: ctx
            .app
            .config
            .items
            .as_ref()
            .map_or(i64::MAX, |c| c.stale_after.as_millis() as i64),
        csrf: &ctx.inbox_token,
        mode,
        web_approval: approvals.is_some_and(|a| a.web_approval),
        can_cancel: ctx.held.approval_cancel,
        body_link: !admin_body
            && mode == Mode::Admin
            && is_superadmin(&ctx.viewer)
            && ctx
                .app
                .config
                .at_rest
                .as_ref()
                .is_some_and(|a| a.admin_body_read),
        max_comment: approvals.map_or(2000, |a| a.max_comment_chars),
        full,
    };
    let header = header_for(&ctx.app, &ctx.held, &ctx.viewer.login).await;
    let mut frame = ctx.frame(state);
    frame.notify = header;
    no_store(html_response(
        status,
        ui_items::detail_page(&frame, &d, notice),
    ))
}

// The limits ------------------------------------------------------------------------------------------

pub(super) async fn limits(state: &Arc<AppState>, ctx: &Context<'_>) -> HttpResponse {
    if !ctx.held.approval_read || !engine(ctx) {
        return html_response(
            StatusCode::FORBIDDEN,
            ui_inbox::status_page(&ctx.frame(state), "Forbidden"),
        );
    }
    let rows: Vec<ui_items::LimitRow> = state
        .config
        .effective
        .iter()
        .filter(|e| {
            ["inbox.items.", "inbox.approvals.", "inbox.at_rest."]
                .iter()
                .any(|p| e.key.starts_with(p))
        })
        .map(|e| ui_items::LimitRow {
            key: e.key.clone(),
            value: e.value.clone(),
            source: e.source.clone(),
        })
        .collect();
    let on = ctx
        .app
        .config
        .at_rest
        .as_ref()
        .is_some_and(|a| a.admin_body_read);
    let header = header_for(&ctx.app, &ctx.held, &ctx.viewer.login).await;
    let mut frame = ctx.frame(state);
    frame.notify = header;
    html_response(StatusCode::OK, ui_items::limits_page(&frame, &rows, on))
}

// The forms -------------------------------------------------------------------------------------------

type Fields = std::collections::HashMap<String, String>;

fn back(id: &str, code: &str) -> HttpResponse {
    redirect_response(
        StatusCode::SEE_OTHER,
        &format!("{}/{id}?s={code}", ui_items::ITEMS),
    )
}

fn text_ok(text: &str, max: usize) -> bool {
    text.chars().count() <= max
        && !text
            .chars()
            .any(|c| c.is_control() && !matches!(c, '\n' | '\r' | '\t'))
}

/// `POST /auth/settings/inbox/items/answer`: `request_id`, `action` (`accept` or `decline`), `shown_effect`, and with a
/// decline `text` and `intent`, and with an old request `stale_ack`. The server takes the effect from the stored request
/// and checks that the page showed it. An approval needs the content to open and to give the stored effect.
pub(super) async fn answer(
    state: &Arc<AppState>,
    ctx: &Context<'_>,
    mut f: Fields,
) -> HttpResponse {
    let (Some(id), Some(action), Some(shown)) = (
        f.remove("request_id"),
        f.remove("action"),
        f.remove("shown_effect"),
    ) else {
        return bad_request();
    };
    let text = f.remove("text").unwrap_or_default();
    let intent = f.remove("intent");
    let stale_ack = f.remove("stale_ack");
    if !f.is_empty() || !id_ok(&id) {
        return bad_request();
    }
    let action = match action.as_str() {
        "accept" => Action::Accept,
        "decline" => Action::Decline,
        _ => return bad_request(),
    };
    if ctx.viewer.session.impersonated || !allowed(ctx) {
        return forbidden();
    }
    let approvals = ctx.app.config.approvals.as_ref();
    if !approvals.is_some_and(|a| a.web_approval) {
        return back(&id, "phone_only");
    }
    let Some(items) = ctx.app.items() else {
        return not_found();
    };
    let login = ctx.viewer.login.clone();
    let loaded = match items.get(&id, &Caller::Owner(login.clone())).await {
        Ok(Some(l)) if l.item.kind == Kind::Approval => l,
        Ok(_) => return back(&id, "gone"),
        Err(_) => return back(&id, "failed"),
    };
    let Some(approval) = &loaded.approval else {
        return back(&id, "gone");
    };
    // The page showed the stored effect. Another value means that the form is not the one of this request.
    if shown != approval.effect {
        return back(&id, "changed");
    }
    let now_ms = unix_now() * 1000;
    let stale_after = ctx
        .app
        .config
        .items
        .as_ref()
        .map_or(i64::MAX, |c| c.stale_after.as_millis() as i64);
    let refused_with = |code: &'static str, status: StatusCode| {
        let notice = ui_items::notice_for(code);
        detail(state, ctx, &id, false, notice, status)
    };
    let refused = |code: &'static str| refused_with(code, StatusCode::UNPROCESSABLE_ENTITY);
    if action == Action::Accept {
        if !text.is_empty() {
            return refused("no_text").await;
        }
        if loaded.item.is_open()
            && now_ms - loaded.item.created_at > stale_after
            && stale_ack.as_deref() != Some("1")
        {
            return refused("stale").await;
        }
        // The content must open and give the stored effect: the same check as for the phone.
        if !matches!(items.serve_body(&loaded).await, Ok(Served::Body(_))) {
            return refused("no_body").await;
        }
    }
    let max = approvals.map_or(2000, |a| a.max_comment_chars);
    let payload = if text.is_empty() {
        None
    } else {
        if !text_ok(&text, max) {
            return refused("too_long").await;
        }
        let intent = match intent.as_deref() {
            Some("change_request") => Intent::ChangeRequest,
            _ => Intent::Comment,
        };
        let plain = OwnerAnswer {
            text: Some(text),
            intent: Some(intent),
            ..OwnerAnswer::default()
        }
        .to_plain();
        Some(zeroize::Zeroizing::new(plain))
    };
    let given = Answer {
        login: login.clone(),
        via: AnswerVia::Web,
        action,
        shown_effect: (action == Action::Accept).then(|| approval.effect.clone()),
        client_msg_id: None,
        payload,
        consume_within: approvals.map_or(CONSUME_WITHIN, |a| a.consume_within),
    };
    match items.answer(&id, given).await {
        Ok(Answered::Accepted { .. }) => {
            log(
                state,
                LogLevel::Info,
                "inbox_web_answer",
                &[
                    ("actor", login),
                    ("request", id.clone()),
                    (
                        "action",
                        if action == Action::Accept {
                            "accept"
                        } else {
                            "decline"
                        }
                        .to_string(),
                    ),
                ],
            );
            back(
                &id,
                if action == Action::Accept {
                    "approved"
                } else {
                    "declined"
                },
            )
        }
        Ok(Answered::Refused { status, .. }) => {
            use inbox_core::proto::AnswerStatus as S;
            back(
                &id,
                match status {
                    S::AlreadyAnswered => "taken",
                    S::Abandoned => "abandoned",
                    S::Expired => "expired",
                    S::EffectMismatch => "changed",
                    S::CustomNotAllowed => "no_text",
                    _ => "closed",
                },
            )
        }
        // The agent is new and `lock_new` is true. The server refuses `accept`: the page of the request shows why, with the
        // status 409. A decline, a question and a name stay open.
        Ok(Answered::AgentLocked(_)) => refused_with("locked", StatusCode::CONFLICT).await,
        Ok(Answered::NotFound) => back(&id, "gone"),
        Ok(Answered::Invalid(_)) => back(&id, "closed"),
        Err(_) => back(&id, "failed"),
    }
}

/// `POST /auth/settings/inbox/items/reply`: `request_id` and `text`. The owner asks the agent. The request stays open.
pub(super) async fn reply(state: &Arc<AppState>, ctx: &Context<'_>, mut f: Fields) -> HttpResponse {
    let (Some(id), Some(text)) = (f.remove("request_id"), f.remove("text")) else {
        return bad_request();
    };
    if !f.is_empty() || !id_ok(&id) {
        return bad_request();
    }
    if ctx.viewer.session.impersonated || !allowed(ctx) {
        return forbidden();
    }
    if text.trim().is_empty() {
        return back(&id, "no_text_given");
    }
    if !text_ok(&text, ui_items::MAX_REPLY_CHARS) {
        return back(&id, "too_long");
    }
    let Some(items) = ctx.app.items() else {
        return not_found();
    };
    let out = items
        .reply(
            &id,
            &ctx.viewer.login,
            None,
            super::random_key(),
            text.as_bytes(),
        )
        .await;
    let code = match out {
        Ok(TurnOut::Stored { .. } | TurnOut::Duplicate { .. }) => {
            log(
                state,
                LogLevel::Info,
                "inbox_web_reply",
                &[("actor", ctx.viewer.login.clone()), ("request", id.clone())],
            );
            "replied"
        }
        Ok(TurnOut::NotFound) => "gone",
        Ok(TurnOut::Closed(_)) => "closed",
        Ok(TurnOut::AskBackNotAllowed) => "no_reply",
        Ok(TurnOut::TooFast) => "slow",
        Ok(_) => "failed",
        Err(_) => "failed",
    };
    back(&id, code)
}

/// `POST /auth/settings/inbox/items/cancel`: `request_id`. An account with `approval.cancel` withdraws an open request.
pub(super) async fn cancel(
    state: &Arc<AppState>,
    ctx: &Context<'_>,
    mut f: Fields,
) -> HttpResponse {
    let Some(id) = f.remove("request_id") else {
        return bad_request();
    };
    if !f.is_empty() || !id_ok(&id) {
        return bad_request();
    }
    if ctx.viewer.session.impersonated || !ctx.held.approval_cancel || !engine(ctx) {
        return forbidden();
    }
    let Some(items) = ctx.app.items() else {
        return not_found();
    };
    let by = CancelBy::System {
        reason: "admin_cancelled",
        actor: format!("user:{}", ctx.viewer.login),
    };
    match items.cancel(&id, by).await {
        Ok(Cancelled::Done(_)) => {
            log(
                state,
                LogLevel::Info,
                "inbox_web_cancel",
                &[("actor", ctx.viewer.login.clone()), ("request", id.clone())],
            );
            back(&id, "cancelled")
        }
        Ok(Cancelled::Final(_)) => back(&id, "closed"),
        Ok(Cancelled::NotFound) => back(&id, "gone"),
        Err(_) => back(&id, "failed"),
    }
}
