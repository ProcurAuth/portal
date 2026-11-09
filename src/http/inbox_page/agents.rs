//! The pages of the agents under `/auth/settings/inbox/agents` and the forms that act on an agent (lot G3).
//!
//! The pages: the list of the agents at `/agents`, and the page of one agent at `/agents/{id}`. The forms: `rename`, `allow`,
//! `block`, `unblock` and `merge`, each one a POST with the token `csrf` and the `Origin` of the portal. They are for the
//! owner of the agent only. An agent of another owner is an unknown agent: the page answers `404`, and a form changes
//! nothing and answers as for an agent that does not exist. A session that the superadmin opens as another account reads no
//! agent and acts on none.
//!
//! The engine decides each action (`Items::agent_op`). This file reads the form, calls the engine and shows the outcome. A
//! refusal is a status and a notice from a fixed list: 422 for a name that is not valid or taken, 409 for a state that does
//! not allow the action.
//!
//! The pages that show an agent carry `Cache-Control: no-store`: they show what the agent said about itself.

use std::collections::{HashMap, HashSet};

use super::items::{allowed, body_of, id_ok, key_text, no_store};
use super::*;
use crate::inbox::agents::{AgentKind, AgentMode, AgentRow, AgentState, AgentSummary, Observed};
use crate::inbox::page_items::{AgentOp, ItemError, Items, ListFilter, Loaded, OpDone, OpError};
use crate::ui::agents::{
    self as ui_agents, AgentPage, AgentView, AgentsList, FirstUse, KeyCounts, ListRow, Lookalike,
    Sender,
};
use crate::ui::items::{Body, Line, StateFilter, Title};
use inbox_core::proto::Kind;

type Fields = HashMap<String, String>;

/// The events that the page of an agent shows, at most.
const EVENT_ROWS: u32 = 100;
/// The requests that the page of an agent reads, at most.
const REQUEST_ROWS: u32 = 1000;

/// Whether the block `inbox.agents` is in the file.
pub(super) fn agents_on(ctx: &Context<'_>) -> bool {
    ctx.app.config.agents.is_some()
}

/// `inbox.agents.lock_new`: the server refuses `accept` for the items of a new agent.
pub(super) fn lock_new(ctx: &Context<'_>) -> bool {
    ctx.app.config.agents.as_ref().is_some_and(|a| a.lock_new)
}

// The agents of an owner ------------------------------------------------------------------------------

/// The agents of one owner, read once for a page.
pub(super) struct Roster {
    rows: Vec<AgentSummary>,
    /// The keys that have an agent that is not the bucket. A key that has none is in the mode `key`: its group has the name
    /// of the key.
    with_agents: HashSet<String>,
}

impl Roster {
    pub(super) async fn of(items: &Items<'_>, owner: &str, key_id: Option<&str>) -> Option<Roster> {
        let rows = items.list_agents(owner, key_id).await.ok()?;
        let with_agents = rows
            .iter()
            .filter(|s| s.agent.kind == AgentKind::Agent)
            .map(|s| s.agent.key_id.clone())
            .collect();
        Some(Roster { rows, with_agents })
    }

    pub(super) fn find(&self, id: &str) -> Option<&AgentSummary> {
        self.rows.iter().find(|s| s.agent.agent_id == id)
    }

    /// The agent as the screens show it. With `hints`, the sealed hints are open: only the owner reads them.
    pub(super) fn view(
        &self,
        state: &AppState,
        items: &Items<'_>,
        row: &AgentRow,
        hints: bool,
    ) -> AgentView {
        let key_name = key_text(state, &row.key_id);
        let plain = row.is_bucket() && !self.with_agents.contains(&row.key_id);
        let mode = if plain {
            AgentMode::Key
        } else {
            AgentMode::Discover
        };
        AgentView {
            agent_id: row.agent_id.clone(),
            label: row.label(&key_name, mode),
            key_name,
            kind: row.kind,
            state: row.state,
            named: row.name.is_some(),
            created_at: row.created_at,
            last_seen_at: row.last_seen_at,
            observed: if hints && row.kind == AgentKind::Agent {
                items.observed(row)
            } else {
                None
            },
            plain,
        }
    }

    /// The same, for a request whose agent the roster does not list: the key is the agent.
    pub(super) fn fallback(&self, state: &AppState, l: &Loaded) -> AgentView {
        let key_name = key_text(state, &l.item.requester_key_id);
        AgentView {
            agent_id: l.item.agent_id.clone(),
            label: key_name.clone(),
            key_name,
            kind: AgentKind::Bucket,
            state: AgentState::Known,
            named: false,
            created_at: l.item.created_at,
            last_seen_at: l.item.agent_seen_at,
            observed: None,
            plain: true,
        }
    }

    /// The agents of a key that can take a merge, with their names. `known_only` leaves out the new agents.
    fn targets(
        &self,
        state: &AppState,
        key_id: &str,
        except: &str,
        known_only: bool,
    ) -> Vec<(String, String, AgentState)> {
        let key_name = key_text(state, key_id);
        let mut out: Vec<(String, String, AgentState)> = self
            .rows
            .iter()
            .map(|s| &s.agent)
            .filter(|a| {
                a.key_id == key_id
                    && a.agent_id != except
                    && a.kind == AgentKind::Agent
                    && match a.state {
                        AgentState::Known => true,
                        AgentState::New => !known_only,
                        _ => false,
                    }
            })
            .map(|a| {
                (
                    a.agent_id.clone(),
                    a.label(&key_name, AgentMode::Discover),
                    a.state,
                )
            })
            .collect();
        out.sort_by_key(|t| t.1.to_lowercase());
        out
    }
}

/// A known agent whose name is the text that `agent` says, with the hints that it shares with it.
async fn lookalike_of(
    items: &Items<'_>,
    row: &AgentRow,
    seen: &Option<Observed>,
) -> Option<Lookalike> {
    let other = items.lookalike(row).await.ok().flatten()?;
    let theirs = items.observed(&other);
    let same = |a: Option<&String>, b: Option<&String>| a.is_some() && a == b;
    Some(Lookalike {
        label: other.name.clone().unwrap_or_default(),
        same_client: same(
            seen.as_ref().and_then(|o| o.client.as_ref()),
            theirs.as_ref().and_then(|o| o.client.as_ref()),
        ),
        same_net: same(
            seen.as_ref().and_then(|o| o.net.as_ref()),
            theirs.as_ref().and_then(|o| o.net.as_ref()),
        ),
    })
}

/// The notices for the new agents of the owner: one for each, even when it has no open request.
pub(super) async fn notices<'a>(
    state: &AppState,
    ctx: &'a Context<'_>,
    items: &Items<'_>,
    roster: &Roster,
    now_ms: i64,
) -> Vec<FirstUse<'a>> {
    let lock = lock_new(ctx);
    let mut out = Vec::new();
    for s in &roster.rows {
        let a = &s.agent;
        if a.kind != AgentKind::Agent || a.state != AgentState::New {
            continue;
        }
        let view = roster.view(state, items, a, true);
        let lookalike = lookalike_of(items, a, &view.observed).await;
        let targets = roster
            .targets(state, &a.key_id, &a.agent_id, true)
            .into_iter()
            .map(|(id, label, _)| (id, label))
            .collect();
        out.push(FirstUse {
            agent: view,
            open: s.open.max(0) as usize,
            lock,
            lookalike,
            targets,
            csrf: &ctx.inbox_token,
            typed: "",
            now_ms,
        });
    }
    out
}

/// The box "From agent" of the page of one request.
pub(super) async fn sender(
    state: &AppState,
    ctx: &Context<'_>,
    items: &Items<'_>,
    l: &Loaded,
    owner: bool,
) -> Sender {
    let roster = Roster::of(
        items,
        &l.item.requester_owner,
        Some(&l.item.requester_key_id),
    )
    .await;
    let row = items.agent(&l.item.agent_id).await.ok().flatten();
    let (agent, lookalike) = match (&roster, &row) {
        (Some(r), Some(row)) => {
            let view = r.view(state, items, row, owner);
            let lookalike = if owner {
                lookalike_of(items, row, &view.observed).await
            } else {
                None
            };
            (view, lookalike)
        }
        _ => (
            Roster {
                rows: Vec::new(),
                with_agents: HashSet::new(),
            }
            .fallback(state, l),
            None,
        ),
    };
    Sender {
        agent,
        lookalike,
        owner,
        lock: lock_new(ctx),
    }
}

/// The agent of each row of the History, by the id of the agent. One read for each owner that the rows name.
pub(super) async fn tags(
    state: &AppState,
    items: &Items<'_>,
    rows: &[Loaded],
) -> HashMap<String, crate::ui::items::AgentTag> {
    let owners: HashSet<&str> = rows
        .iter()
        .map(|l| l.item.requester_owner.as_str())
        .collect();
    let mut out = HashMap::new();
    for owner in owners {
        let Some(roster) = Roster::of(items, owner, None).await else {
            continue;
        };
        for s in &roster.rows {
            let view = roster.view(state, items, &s.agent, false);
            out.insert(
                s.agent.agent_id.clone(),
                crate::ui::items::AgentTag {
                    agent_id: view.agent_id,
                    label: view.label,
                    plain: view.plain,
                },
            );
        }
    }
    out
}

/// The filter `agent=` of the History: the id to filter with (a merged agent gives its target), and the name for the chip.
/// The name is `None` when the viewer does not know the agent: an owner knows only its own agents.
pub(super) async fn history_filter(
    state: &AppState,
    ctx: &Context<'_>,
    items: &Items<'_>,
    id: &str,
    all: bool,
) -> (String, Option<String>) {
    let Ok(Some(mut row)) = items.agent(id).await else {
        return (id.to_string(), None);
    };
    if row.state == AgentState::Merged {
        if let Some(target) = row.merged_into.clone() {
            if let Ok(Some(next)) = items.agent(&target).await {
                row = next;
            }
        }
    }
    if row.owner != ctx.viewer.login && !all {
        return (row.agent_id, None);
    }
    let Some(roster) = Roster::of(items, &row.owner, Some(&row.key_id)).await else {
        return (row.agent_id, None);
    };
    let label = roster.view(state, items, &row, false).label;
    (row.agent_id, Some(label))
}

// The pages -------------------------------------------------------------------------------------------

fn forbidden_page(state: &Arc<AppState>, ctx: &Context<'_>) -> HttpResponse {
    html_response(
        StatusCode::FORBIDDEN,
        ui_inbox::status_page(&ctx.frame(state), "Forbidden"),
    )
}

async fn frame<'a>(state: &'a Arc<AppState>, ctx: &'a Context<'_>) -> crate::ui::Frame<'a> {
    let header = header_for(&ctx.app, &ctx.held, &ctx.viewer.login).await;
    let mut frame = ctx.frame(state);
    frame.notify = header;
    frame
}

/// `GET /auth/settings/inbox/agents`: the list of the agents of the owner.
pub(super) async fn list(state: &Arc<AppState>, ctx: &Context<'_>) -> HttpResponse {
    if !allowed(ctx) || !agents_on(ctx) {
        return if ctx.viewer.session.impersonated {
            forbidden_page(state, ctx)
        } else {
            not_found()
        };
    }
    let Some(items) = ctx.app.items() else {
        return not_found();
    };
    let Some(pairs) = query_pairs(ctx.uri) else {
        return bad_request();
    };
    let mut code = None;
    for (name, value) in &pairs {
        match name.as_str() {
            "s" => code = Some(value.as_str()),
            _ => return bad_request(),
        }
    }
    let notice = code.and_then(ui_agents::notice_for);
    if code.is_some() && notice.is_none() {
        return bad_request();
    }
    let Some(roster) = Roster::of(&items, &ctx.viewer.login, None).await else {
        return unavailable();
    };
    let rows = roster
        .rows
        .iter()
        .map(|s| ListRow {
            agent: roster.view(state, &items, &s.agent, false),
            open: s.open.max(0) as usize,
            idle: s.idle,
        })
        .collect();
    let page = ui_agents::agents_page(
        &frame(state, ctx).await,
        &AgentsList {
            rows,
            now_ms: unix_now() * 1000,
        },
        notice,
    );
    no_store(html_response(StatusCode::OK, page))
}

/// Draws the page of one agent. `typed` is the name that a refused form carried.
async fn draw(
    state: &Arc<AppState>,
    ctx: &Context<'_>,
    items: &Items<'_>,
    row: &AgentRow,
    notice: Option<crate::ui::settings::Notice>,
    status: StatusCode,
    typed: Option<&str>,
) -> HttpResponse {
    let login = &ctx.viewer.login;
    let Some(roster) = Roster::of(items, login, Some(&row.key_id)).await else {
        return unavailable();
    };
    let view = roster.view(state, items, row, true);
    let open_rows = match items
        .list(ListFilter {
            owner: Some(login.clone()),
            agent_id: Some(row.agent_id.clone()),
            open: Some(true),
            limit: 100,
            ..ListFilter::default()
        })
        .await
    {
        Ok(rows) => rows,
        Err(_) => return unavailable(),
    };
    let open_rows: Vec<Loaded> = open_rows
        .into_iter()
        .filter(|l| l.item.kind == Kind::Approval)
        .collect();
    let mut open = Vec::new();
    for l in &open_rows {
        let title = match body_of(items, l).await {
            Ok(Body::Shown(t)) => Title::Text(t.title),
            Ok(Body::Purged) => Title::Purged,
            _ => Title::Hidden,
        };
        open.push(Line {
            loaded: l,
            title,
            first_line: None,
            key_name: view.key_name.clone(),
            agent: None,
        });
    }
    let answered = match items
        .list(ListFilter {
            owner: Some(login.clone()),
            agent_id: Some(row.agent_id.clone()),
            open: Some(false),
            limit: REQUEST_ROWS,
            ..ListFilter::default()
        })
        .await
    {
        Ok(rows) => rows
            .iter()
            .filter(|l| {
                matches!(
                    StateFilter::of(l),
                    Some(StateFilter::Approved | StateFilter::Declined)
                )
            })
            .count(),
        Err(_) => return unavailable(),
    };
    let events = match items.agent_events(&row.agent_id, EVENT_ROWS).await {
        Ok(e) => e,
        Err(_) => return unavailable(),
    };
    let key_agents = |state_is: AgentState| {
        roster
            .rows
            .iter()
            .filter(|s| {
                s.agent.key_id == row.key_id
                    && s.agent.kind == AgentKind::Agent
                    && s.agent.state == state_is
            })
            .count()
    };
    let counts = KeyCounts {
        known: key_agents(AgentState::Known),
        new: key_agents(AgentState::New),
        blocked: key_agents(AgentState::Blocked),
        max: ctx.app.config.agents.as_ref().map(|a| a.max_per_key),
    };
    let merged_into = match (&row.state, &row.merged_into) {
        (AgentState::Merged, Some(id)) => items.agent(id).await.ok().flatten().map(|a| {
            (
                a.agent_id.clone(),
                roster.view(state, items, &a, false).label,
            )
        }),
        _ => None,
    };
    let targets = roster.targets(state, &row.key_id, &row.agent_id, false);
    let page = ui_agents::agent_page(
        &frame(state, ctx).await,
        &AgentPage {
            agent: view,
            csrf: &ctx.inbox_token,
            lock: lock_new(ctx),
            now_ms: unix_now() * 1000,
            open,
            answered,
            events,
            named_by: row.named_at.map(|at| (row.owner.clone(), at)),
            targets,
            counts,
            merged_into,
            typed,
        },
        notice,
    );
    no_store(html_response(status, page))
}

/// `GET /auth/settings/inbox/agents/{id}`: the page of one agent of the owner.
pub(super) async fn page(state: &Arc<AppState>, ctx: &Context<'_>, id: &str) -> HttpResponse {
    if !id_ok(id) {
        return not_found();
    }
    if !allowed(ctx) || !agents_on(ctx) {
        return if ctx.viewer.session.impersonated {
            forbidden_page(state, ctx)
        } else {
            not_found()
        };
    }
    let Some(items) = ctx.app.items() else {
        return not_found();
    };
    let Some(pairs) = query_pairs(ctx.uri) else {
        return bad_request();
    };
    let mut code = None;
    for (name, value) in &pairs {
        match name.as_str() {
            "s" => code = Some(value.as_str()),
            _ => return bad_request(),
        }
    }
    let notice = code.and_then(ui_agents::notice_for);
    if code.is_some() && notice.is_none() {
        return bad_request();
    }
    // An agent of another owner is an agent that does not exist.
    let row = match items.agent(id).await {
        Ok(Some(row)) if row.owner == ctx.viewer.login => row,
        Ok(_) => return not_found(),
        Err(_) => return unavailable(),
    };
    draw(state, ctx, &items, &row, notice, StatusCode::OK, None).await
}

// The forms -------------------------------------------------------------------------------------------

fn to_agent(id: &str, code: &str) -> HttpResponse {
    redirect_response(
        StatusCode::SEE_OTHER,
        &format!("{}/{id}?s={code}", ui_agents::AGENTS),
    )
}

fn to_inbox(code: &str) -> HttpResponse {
    back_with(PREFIX, code)
}

/// The notice and the status of a refusal of the engine.
fn refusal(e: OpError) -> (&'static str, StatusCode) {
    match e {
        OpError::NotFound => ("agent_gone", StatusCode::NOT_FOUND),
        OpError::InvalidName(_) => ("agent_bad_name", StatusCode::UNPROCESSABLE_ENTITY),
        OpError::NameTaken => ("agent_name_taken", StatusCode::UNPROCESSABLE_ENTITY),
        OpError::BadState => ("agent_bad_state", StatusCode::CONFLICT),
        OpError::CrossKey => ("agent_cross_key", StatusCode::UNPROCESSABLE_ENTITY),
    }
}

/// Runs one action of the owner and shows the outcome. `typed` is the name of the form, for a refusal.
async fn act(
    state: &Arc<AppState>,
    ctx: &Context<'_>,
    id: &str,
    op: AgentOp,
    typed: Option<&str>,
    done: impl FnOnce(&OpDone) -> HttpResponse,
) -> HttpResponse {
    if ctx.viewer.session.impersonated || !allowed(ctx) || !agents_on(ctx) {
        return forbidden();
    }
    let Some(items) = ctx.app.items() else {
        return not_found();
    };
    match items.agent_op(&ctx.viewer.login, id, op, "web").await {
        Ok(Ok(out)) => done(&out),
        // An agent that does not exist, or that another owner has: the same answer, and nothing changed.
        Ok(Err(OpError::NotFound)) => to_inbox("agent_gone"),
        Ok(Err(e)) => {
            let (code, status) = refusal(e);
            match items.agent(id).await {
                Ok(Some(row)) if row.owner == ctx.viewer.login => {
                    let notice = ui_agents::notice_for(code);
                    draw(state, ctx, &items, &row, notice, status, typed).await
                }
                _ => to_inbox("agent_gone"),
            }
        }
        Err(ItemError::Degraded | ItemError::Replica) => unavailable(),
        Err(_) => to_inbox("failed"),
    }
}

/// The two fields that every form has. `None`: the form is not one of this page.
fn take_id(f: &mut Fields) -> Option<String> {
    let id = f.remove("agent_id")?;
    id_ok(&id).then_some(id)
}

/// `POST /auth/settings/inbox/agents/rename`: `agent_id` and `name`.
pub(super) async fn rename(
    state: &Arc<AppState>,
    ctx: &Context<'_>,
    mut f: Fields,
) -> HttpResponse {
    let (Some(id), Some(name)) = (take_id(&mut f), f.remove("name")) else {
        return bad_request();
    };
    if !f.is_empty() {
        return bad_request();
    }
    let typed = name.clone();
    let target = id.clone();
    act(
        state,
        ctx,
        &id,
        AgentOp::Rename { name },
        Some(&typed),
        move |_| to_agent(&target, "agent_renamed"),
    )
    .await
}

/// `POST /auth/settings/inbox/agents/allow`: `agent_id` and, optional, `name`. An empty name allows the agent as it is.
pub(super) async fn allow(state: &Arc<AppState>, ctx: &Context<'_>, mut f: Fields) -> HttpResponse {
    let Some(id) = take_id(&mut f) else {
        return bad_request();
    };
    let name = f.remove("name").filter(|n| !n.is_empty());
    if !f.is_empty() {
        return bad_request();
    }
    let typed = name.clone();
    act(
        state,
        ctx,
        &id,
        AgentOp::Allow { name },
        typed.as_deref(),
        |_| to_inbox("agent_allowed"),
    )
    .await
}

/// `POST /auth/settings/inbox/agents/block`: `agent_id`. The open requests of the agent are cancelled.
pub(super) async fn block(state: &Arc<AppState>, ctx: &Context<'_>, mut f: Fields) -> HttpResponse {
    let Some(id) = take_id(&mut f) else {
        return bad_request();
    };
    if !f.is_empty() {
        return bad_request();
    }
    act(state, ctx, &id, AgentOp::Block, None, |_| {
        to_inbox("agent_blocked")
    })
    .await
}

/// `POST /auth/settings/inbox/agents/unblock`: `agent_id`.
pub(super) async fn unblock(
    state: &Arc<AppState>,
    ctx: &Context<'_>,
    mut f: Fields,
) -> HttpResponse {
    let Some(id) = take_id(&mut f) else {
        return bad_request();
    };
    if !f.is_empty() {
        return bad_request();
    }
    let target = id.clone();
    act(state, ctx, &id, AgentOp::Unblock, None, move |_| {
        to_agent(&target, "agent_unblocked")
    })
    .await
}

/// `POST /auth/settings/inbox/agents/merge`: `agent_id` and `into`. The agent is removed, and its requests and its token go
/// to the other agent of the same key.
pub(super) async fn merge(state: &Arc<AppState>, ctx: &Context<'_>, mut f: Fields) -> HttpResponse {
    let (Some(id), Some(into)) = (take_id(&mut f), f.remove("into")) else {
        return bad_request();
    };
    if !f.is_empty() {
        return bad_request();
    }
    if ctx.viewer.session.impersonated || !allowed(ctx) || !agents_on(ctx) {
        return forbidden();
    }
    if !id_ok(&into) {
        let Some(items) = ctx.app.items() else {
            return not_found();
        };
        return match items.agent(&id).await {
            Ok(Some(row)) if row.owner == ctx.viewer.login => {
                let notice = ui_agents::notice_for("agent_no_target");
                draw(
                    state,
                    ctx,
                    &items,
                    &row,
                    notice,
                    StatusCode::UNPROCESSABLE_ENTITY,
                    None,
                )
                .await
            }
            _ => to_inbox("agent_gone"),
        };
    }
    act(
        state,
        ctx,
        &id,
        AgentOp::Merge { into },
        None,
        |out| match &out.other {
            Some(target) => to_agent(&target.agent_id, "agent_merged"),
            None => to_inbox("agent_merged"),
        },
    )
    .await
}
