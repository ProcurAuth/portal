//! The pages of the items in the tab "Inbox": the panel "Open items" (one group for each agent), the page "History", the
//! page of one request and the page "Inbox limits". The pages of the agents are in `agents.rs`.
//!
//! Every page is HTML from the server and works without script. A form is a POST with the token `csrf`. The script of
//! `settings.js` only shows the copy buttons: a button that needs it has the attribute `hidden`.
//!
//! The text of a person or of an agent goes through [`html::escape`]: it is escaped, and a hidden character has a
//! visible mark. The explanation of an agent goes through [`html::explanation`]. The command goes through
//! [`html::command`]. No other function writes such a text.

use std::fmt::Write as _;

use inbox_core::proto::{Action, ExplanationFormat, ItemState, Kind, Language};

use super::agents::{
    about_sender, first_use, hints_line, key_chip, sender_box, state_pill, AgentView, FirstUse,
    Sender,
};
use super::components::{chips_stable, icon, Chip};
use super::markdown_html as html;
use super::settings::{fill_page, notice_html, Frame, Notice, Tab};
use crate::inbox::agents::AgentState;
use crate::inbox::page_items::Loaded;
use crate::util::{html_escape, time_tag};

pub const ITEMS: &str = "/auth/settings/inbox/items";
pub const ANSWER: &str = "/auth/settings/inbox/items/answer";
pub const REPLY: &str = "/auth/settings/inbox/items/reply";
pub const CANCEL: &str = "/auth/settings/inbox/items/cancel";
pub const LIMITS: &str = "/auth/settings/inbox/limits";
/// The explanation folds after this many characters.
pub const FOLD_CHARS: usize = 400;
/// The longest text of a reply, in characters.
pub const MAX_REPLY_CHARS: usize = 2000;

/// The text of the outcome of a form of the items, from a fixed list.
pub fn notice_for(code: &str) -> Option<Notice> {
    let (text, ok) = match code {
        "approved" => ("You approved the request. The agent can run it now.", true),
        "declined" => ("You declined the request.", true),
        "replied" => ("Your question is sent to the agent.", true),
        "cancelled" => ("The request is cancelled.", true),
        "taken" => ("Another answer came first. Nothing changed.", false),
        "closed" => ("The request is closed. It cannot take an answer.", false),
        "abandoned" => (
            "The agent no longer answers. The request cannot take an answer.",
            false,
        ),
        "expired" => ("The request expired.", false),
        "changed" => (
            "The request is not the one that you saw. Read it again.",
            false,
        ),
        "stale" => ("Tick the box to approve an old request.", false),
        "no_body" => (
            "The content is not available, so you cannot approve it here.",
            false,
        ),
        "no_text" => ("This agent does not take a text with a refusal.", false),
        "locked" => (
            "Approve is locked until you allow this agent. You can decline now.",
            false,
        ),
        "no_reply" => ("This agent does not take a question.", false),
        "too_long" => ("The text is too long.", false),
        "no_text_given" => ("Type a text.", false),
        "slow" => (
            "The agent has a question to read first. Try again in a moment.",
            false,
        ),
        "gone" => ("That request does not exist.", false),
        "phone_only" => ("This server takes answers on the phone only.", false),
        "failed" => ("The inbox could not do this. Try again.", false),
        _ => return super::agents::notice_for(code),
    };
    Some(Notice { text, ok })
}

// Words ------------------------------------------------------------------------------------------

/// A length of time in one unit: `40 s`, `3 min`, `2 h`, `6 d`.
fn span(ms: i64) -> String {
    let secs = ms.max(0) / 1000;
    match secs {
        0..=59 => format!("{secs} s"),
        60..=3599 => format!("{} min", secs / 60),
        3600..=86_399 => format!("{} h", secs / 3600),
        _ => format!("{} d", secs / 86_400),
    }
}

/// The same in words: `6 days`, `3 hours`.
pub(super) fn long_span(ms: i64) -> String {
    let secs = ms.max(0) / 1000;
    let (n, unit) = match secs {
        0..=59 => (secs, "second"),
        60..=3599 => (secs / 60, "minute"),
        3600..=86_399 => (secs / 3600, "hour"),
        _ => (secs / 86_400, "day"),
    };
    format!("{n} {unit}{}", if n == 1 { "" } else { "s" })
}

pub(super) fn ago(now: i64, then: i64) -> String {
    format!("{} ago", span(now - then))
}

pub(super) fn when(ms: i64) -> String {
    time_tag(ms / 1000, false).replacen("<time ", r#"<time data-short="1" "#, 1)
}

pub(super) fn b(text: &str) -> String {
    format!("<b>{text}</b>")
}

fn word(state: ItemState, action: Option<Action>) -> &'static str {
    match (state, action) {
        (ItemState::Answered, Some(Action::Accept)) => "Approved",
        (ItemState::Answered, _) => "Declined",
        (ItemState::Pending, _) | (ItemState::Replied, _) => "Open",
        (ItemState::Abandoned, _) => "Abandoned",
        (ItemState::Superseded, _) => "Replaced",
        (ItemState::Expired, _) => "Expired",
        (ItemState::Cancelled, _) => "Cancelled",
    }
}

fn state_icon(l: &Loaded) -> String {
    let (class, name) = match (l.item.state, l.item.answer_action) {
        (ItemState::Answered, Some(Action::Accept)) => ("st--ok", "circle-check"),
        (ItemState::Answered, _) => ("st--bad", "x"),
        (ItemState::Abandoned, _) => ("st--warn", "alert-triangle"),
        (ItemState::Superseded, _) => ("st--off", "refresh"),
        (ItemState::Expired, _) => ("st--off", "clock"),
        (ItemState::Cancelled, _) => ("st--off", "ban"),
        _ => ("st--warn", "clock"),
    };
    let label = word(l.item.state, l.item.answer_action);
    format!(
        r#"<span class="st {class}" role="img" aria-label="{label}" title="{label}">{}</span>"#,
        icon(name)
    )
}

/// How the owner answered: `alice (phone)` or `alice (web)`.
fn answered_by(l: &Loaded) -> String {
    let who = html_escape(l.item.answered_by.as_deref().unwrap_or("?"));
    let via = match l.item.answered_via.as_deref() {
        Some(v) if v.starts_with("web:") => "web",
        Some(_) => "phone",
        None => "?",
    };
    format!("{who} ({via})")
}

/// What happened to a request that is final, in one sentence. The text of the owner is not here.
fn outcome(l: &Loaded, now: i64) -> String {
    let i = &l.item;
    let mut out = match i.state {
        ItemState::Answered => {
            let after = i
                .answered_at
                .map(|at| format!(", after {}", span(at - i.created_at)))
                .unwrap_or_default();
            format!(
                "{} by {}{after}",
                word(i.state, i.answer_action),
                answered_by(l)
            )
        }
        ItemState::Abandoned => format!(
            "Abandoned: the agent no longer answers. Last sign {} ago.",
            span(now - i.agent_seen_at)
        ),
        ItemState::Superseded => "Replaced by a new request.".to_string(),
        ItemState::Expired => "Expired: the time limit is reached. No decision.".to_string(),
        ItemState::Cancelled => match i.settle_reason.as_deref() {
            Some("agent_cancelled") => "Cancelled by the agent.".to_string(),
            Some("key_revoked") => "Cancelled: the key was revoked.".to_string(),
            Some("agent_blocked") => "Cancelled: the owner blocked the agent.".to_string(),
            Some("owner_deleted") => "Cancelled: the owner was deleted.".to_string(),
            Some("admin_cancelled") => "Cancelled by an administrator.".to_string(),
            _ => "Cancelled.".to_string(),
        },
        ItemState::Pending | ItemState::Replied => "Waiting for an answer.".to_string(),
    };
    if let Some(a) = &l.approval {
        if a.consumed_at.is_some() {
            out.push_str(" The service used it.");
        } else if a.lapsed_at.is_some() {
            out.push_str(" The time to use it ended.");
        }
    }
    out
}

// The rows ---------------------------------------------------------------------------------------

/// What a row shows of the content.
pub enum Title {
    Text(String),
    /// The viewer reads metadata only.
    Hidden,
    /// The content is erased (`body_purge_after`).
    Purged,
}

/// The agent of a row of the History: its id for the filter, and its name for the cell.
pub struct AgentTag {
    pub agent_id: String,
    /// Text of the server: the name of the owner, else "<key name> #<number>".
    pub label: String,
    /// The group has the name of the key: the cell shows the name once.
    pub plain: bool,
}

/// One request in a list. `first_line` is the start of the command.
pub struct Line<'a> {
    pub loaded: &'a Loaded,
    pub title: Title,
    pub first_line: Option<String>,
    /// The name of the key of the agent.
    pub key_name: String,
    /// The agent of the request, for the History. The panel shows it in the header of the group.
    pub agent: Option<AgentTag>,
}

fn kind_chip(kind: Kind) -> &'static str {
    match kind {
        Kind::Approval => r#"<span class="kind kind--approval">Approval</span>"#,
        _ => r#"<span class="kind">Question</span>"#,
    }
}

pub(super) fn title_html(t: &Title, code: &str) -> String {
    match t {
        Title::Text(text) => html::escape(text),
        Title::Hidden => format!(r#"<span class="muted">Request {code}: content hidden</span>"#),
        Title::Purged => format!(r#"<span class="muted">Request {code}: content deleted</span>"#),
    }
}

pub(super) fn code_of(l: &Loaded) -> String {
    l.approval.as_ref().map_or_else(
        || {
            l.item
                .request_id
                .chars()
                .take(4)
                .collect::<String>()
                .to_uppercase()
        },
        |a| a.code.clone(),
    )
}

pub(super) fn detail_href(l: &Loaded) -> String {
    format!("{ITEMS}/{}", l.item.request_id)
}

/// The requests of one agent, in the panel "Open items".
pub struct Group<'a> {
    pub agent: AgentView,
    pub lines: Vec<Line<'a>>,
}

impl Group<'_> {
    /// The time of the oldest request of the group.
    fn oldest(&self) -> i64 {
        self.lines
            .iter()
            .map(|l| l.loaded.item.created_at)
            .min()
            .unwrap_or(i64::MAX)
    }
}

/// The data of the panel "Open items".
pub struct OpenItems<'a> {
    pub groups: Vec<Group<'a>>,
    /// One notice for each `new` agent, even when the agent has no open request.
    pub notices: Vec<FirstUse<'a>>,
    /// `inbox.agents.lock_new`.
    pub lock: bool,
    /// The block `inbox.agents` is on: the segmented control has the link "Agents".
    pub agents_link: bool,
    pub stale_after_ms: i64,
    pub now_ms: i64,
}

/// The groups in the order of the page: the `new` agents first, then the others by the time of their oldest request, and
/// the bucket of a key last.
fn ordered<'g, 'a>(groups: &'g [Group<'a>]) -> Vec<&'g Group<'a>> {
    let mut list: Vec<&Group> = groups.iter().collect();
    list.sort_by(|a, b| {
        let key = |g: &Group| {
            (
                g.agent.is_bucket(),
                g.agent.state != AgentState::New,
                g.oldest(),
            )
        };
        key(a)
            .cmp(&key(b))
            .then_with(|| a.agent.agent_id.cmp(&b.agent.agent_id))
    });
    list
}

/// The panel "Open items": the first-use notices, then one group for each agent that has a request that waits. The panel is
/// absent when nothing waits and no agent is new.
pub fn open_items(p: &OpenItems) -> String {
    if p.groups.is_empty() && p.notices.is_empty() {
        return String::new();
    }
    let mut out = String::new();
    let mut notices: Vec<&FirstUse> = p.notices.iter().collect();
    notices.sort_by_key(|n| (n.agent.created_at, n.agent.agent_id.clone()));
    for n in notices {
        out.push_str(&first_use(n));
    }
    if p.groups.is_empty() {
        return out;
    }
    let total: usize = p.groups.iter().map(|g| g.lines.len()).sum();
    let mut body = String::new();
    for g in ordered(&p.groups) {
        body.push_str(&group(g, p));
    }
    let note = if p.groups.iter().any(|g| !g.agent.plain) {
        r#"<p class="note agNote">Groups are by agent. The names are yours. The lines &quot;Observed, not verified&quot; come from the agent or from its connection. They can be wrong or copied.</p>"#
    } else {
        ""
    };
    let _ = write!(
        out,
        r#"<section class="zone zone--items" aria-labelledby="oi-t"><div class="zone__head"><h2 class="title" id="oi-t">Open items</h2><span class="count">{total}</span><span class="grow"></span>{}</div><div class="agGroups">{body}</div>{note}</section>"#,
        segmented(View::Open, p.agents_link)
    );
    out
}

/// The three views of the Inbox, for the control at the top of each page.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum View {
    Open,
    History,
    Agents,
}

pub(super) fn segmented(active: View, agents: bool) -> String {
    let on = |v: View| if active == v { " is-active" } else { "" };
    let agents = if agents {
        format!(
            r#"<a class="segmented__option{}" href="{}">Agents</a>"#,
            on(View::Agents),
            super::agents::AGENTS
        )
    } else {
        String::new()
    };
    format!(
        r#"<span class="segmented" role="group" aria-label="View"><a class="segmented__option{}" href="{}">Open items</a><a class="segmented__option{}" href="{ITEMS}">History</a>{agents}</span>"#,
        on(View::Open),
        super::inbox::PAGE,
        on(View::History),
    )
}

/// One group: the header with the name, the key, the state and the count, the line of the hints, and the rows.
fn group(g: &Group, p: &OpenItems) -> String {
    let a = &g.agent;
    let locked = a.locked(p.lock);
    let mut rows = String::new();
    for line in &g.lines {
        rows.push_str(&open_row(line, p, locked));
    }
    let class = match (a.is_bucket(), a.state) {
        (true, _) if !a.plain => " agrp--bucket",
        (false, AgentState::New) => " agrp--new",
        _ => "",
    };
    let chip = if a.plain {
        String::new()
    } else {
        key_chip(&a.key_name)
    };
    // A new agent has its actions in the notice above. A bucket cannot be managed.
    let manage = if a.is_bucket() || a.state == AgentState::New {
        String::new()
    } else {
        format!(
            r#"<a class="button button--small" href="{}">Manage</a>"#,
            html_escape(&a.href())
        )
    };
    format!(
        r#"<details class="agrp{class}" open><summary><span class="agrp__chev">{}</span><span class="agrp__name">{}</span>{chip}{}<span class="agrp__sum"><b>{}</b> open</span><span class="grow"></span>{manage}</summary>{}<ul class="oiList">{rows}</ul></details>"#,
        icon("chevron-right"),
        html_escape(&a.label),
        state_pill(a, false),
        g.lines.len(),
        hints_line(a, p.now_ms),
    )
}

fn open_row(line: &Line, p: &OpenItems, locked: bool) -> String {
    let l = line.loaded;
    let i = &l.item;
    let stale = p.now_ms - i.created_at > p.stale_after_ms;
    let code = code_of(l);
    let icon_name = if i.kind == Kind::Approval {
        "shield-check"
    } else {
        "help-circle"
    };
    // The key and the agent are in the header of the group: the row shows the service and the action.
    let who = l
        .approval
        .as_ref()
        .map(|a| {
            format!(
                "{} · <code>{}</code>",
                html_escape(&a.service),
                html_escape(&a.action)
            )
        })
        .unwrap_or_default();
    let mut meta = format!(
        r#"<span class="lease{}">Asked {}</span><span class="dot lease">Agent active {}</span>"#,
        if stale { " lease--old" } else { "" },
        b(&ago(p.now_ms, i.created_at)),
        b(&ago(p.now_ms, i.agent_seen_at))
    );
    match (i.lease_until, i.until_at) {
        (Some(at), None) => {
            let _ = write!(
                meta,
                r#"<span class="dot lease">Lease until {}</span>"#,
                b(&when(at))
            );
        }
        (_, Some(at)) => {
            let _ = write!(
                meta,
                r#"<span class="dot lease">Waits until {}</span>"#,
                b(&when(at))
            );
        }
        _ => {}
    }
    let cmd = line
        .first_line
        .as_ref()
        .map(|c| {
            format!(
                r#"<div class="oi__cmd" title="Start of the command">{}</div>"#,
                html::escape(c)
            )
        })
        .unwrap_or_default();
    let lock = if locked && i.kind == Kind::Approval {
        format!(
            r#"<div class="lockline">{}<span>Approve is locked until you allow this agent. You can decline now.</span></div>"#,
            icon("lock")
        )
    } else {
        String::new()
    };
    let (pill, action) = match i.state {
        ItemState::Replied => (r#"<span class="pill">Question sent</span>"#, "Open"),
        _ => (
            r#"<span class="pill pill--wait">Waiting for you</span>"#,
            "Review",
        ),
    };
    let old = if stale {
        r#"<span class="pill pill--warn">Old request</span>"#
    } else {
        ""
    };
    format!(
        r#"<li class="oi"><span class="oi__icon">{}</span><div class="oi__line1">{}<span class="code">{}</span><a class="oi__title" href="{}">{}</a></div><div class="oi__who">{who}</div><div class="oi__meta">{meta}</div>{cmd}{lock}<div class="oi__side">{old}{pill}<a class="button button--small" href="{}">{action}</a></div></li>"#,
        icon(icon_name),
        kind_chip(i.kind),
        html_escape(&code),
        html_escape(&detail_href(l)),
        title_html(&line.title, &code),
        html_escape(&detail_href(l)),
    )
}

// History ----------------------------------------------------------------------------------------

/// The state filter of the history.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StateFilter {
    Approved,
    Declined,
    Abandoned,
    Replaced,
    Expired,
    Cancelled,
}

impl StateFilter {
    pub const ALL: [StateFilter; 6] = [
        StateFilter::Approved,
        StateFilter::Declined,
        StateFilter::Abandoned,
        StateFilter::Replaced,
        StateFilter::Expired,
        StateFilter::Cancelled,
    ];

    pub fn name(self) -> &'static str {
        match self {
            StateFilter::Approved => "approved",
            StateFilter::Declined => "declined",
            StateFilter::Abandoned => "abandoned",
            StateFilter::Replaced => "replaced",
            StateFilter::Expired => "expired",
            StateFilter::Cancelled => "cancelled",
        }
    }

    pub fn parse(text: &str) -> Option<StateFilter> {
        Self::ALL.into_iter().find(|s| s.name() == text)
    }

    pub fn label(self) -> &'static str {
        match self {
            StateFilter::Approved => "Approved",
            StateFilter::Declined => "Declined",
            StateFilter::Abandoned => "Abandoned",
            StateFilter::Replaced => "Replaced",
            StateFilter::Expired => "Expired",
            StateFilter::Cancelled => "Cancelled",
        }
    }

    pub fn of(l: &Loaded) -> Option<StateFilter> {
        Some(match (l.item.state, l.item.answer_action) {
            (ItemState::Answered, Some(Action::Accept)) => StateFilter::Approved,
            (ItemState::Answered, _) => StateFilter::Declined,
            (ItemState::Abandoned, _) => StateFilter::Abandoned,
            (ItemState::Superseded, _) => StateFilter::Replaced,
            (ItemState::Expired, _) => StateFilter::Expired,
            (ItemState::Cancelled, _) => StateFilter::Cancelled,
            _ => return None,
        })
    }
}

/// The parameters of the page History. The address omits each default.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HistoryQuery {
    pub state: Option<StateFilter>,
    pub key: String,
    pub service: String,
    /// The period, in days: 7, 30 or 90.
    pub days: u32,
    /// The id of one agent: 32 hexadecimal characters.
    pub agent: Option<String>,
}

impl Default for HistoryQuery {
    fn default() -> Self {
        Self {
            state: None,
            key: String::new(),
            service: String::new(),
            days: 30,
            agent: None,
        }
    }
}

impl HistoryQuery {
    /// Reads the query of the address. `None`: an unknown parameter, a repeated one or a bad value.
    pub fn parse(pairs: &[(String, String)]) -> Option<HistoryQuery> {
        let mut q = HistoryQuery::default();
        let mut seen: Vec<&str> = Vec::new();
        for (name, value) in pairs {
            if seen.contains(&name.as_str()) {
                return None;
            }
            seen.push(name);
            let text = |v: &str| {
                (v.chars().count() <= 64 && !v.chars().any(char::is_control))
                    .then(|| v.trim().to_string())
            };
            match name.as_str() {
                "s" | "full" => {}
                "st" => q.state = Some(StateFilter::parse(value)?),
                "k" => q.key = text(value)?,
                "svc" => q.service = text(value)?,
                "agent" => {
                    if value.len() != 32 || !value.bytes().all(|b| b.is_ascii_hexdigit()) {
                        return None;
                    }
                    q.agent = Some(value.to_ascii_lowercase());
                }
                "p" => {
                    q.days = match value.as_str() {
                        "7" => 7,
                        "30" => 30,
                        "90" => 90,
                        _ => return None,
                    }
                }
                _ => return None,
            }
        }
        Some(q)
    }

    /// The address with one change.
    pub fn href(&self, change: impl FnOnce(&mut HistoryQuery)) -> String {
        let mut q = self.clone();
        change(&mut q);
        let mut parts: Vec<String> = Vec::new();
        if let Some(s) = q.state {
            parts.push(format!("st={}", s.name()));
        }
        if !q.key.is_empty() {
            parts.push(format!(
                "k={}",
                crate::util::percent_encode_query_component(&q.key)
            ));
        }
        if !q.service.is_empty() {
            parts.push(format!(
                "svc={}",
                crate::util::percent_encode_query_component(&q.service)
            ));
        }
        if let Some(agent) = &q.agent {
            parts.push(format!("agent={agent}"));
        }
        if q.days != 30 {
            parts.push(format!("p={}", q.days));
        }
        if parts.is_empty() {
            ITEMS.to_string()
        } else {
            format!("{ITEMS}?{}", parts.join("&"))
        }
    }
}

/// The data of the page History. The lines are in the order of the page: a request that follows an abandoned or
/// replaced one stands right after it.
pub struct History<'a> {
    pub lines: Vec<(Line<'a>, bool)>,
    /// The count of each state, before the state filter.
    pub counts: Vec<(StateFilter, usize)>,
    pub total: usize,
    pub query: HistoryQuery,
    pub now_ms: i64,
    /// The viewer reads the requests of every account: the rows name the owner.
    pub all_accounts: bool,
    pub truncated: bool,
    pub limits_link: bool,
    /// The block `inbox.agents` is on: the segmented control has the link "Agents".
    pub agents_link: bool,
    /// The agent of the filter `agent=`: its name (text of the server), or `None` when the viewer does not know it.
    pub agent_filter: Option<Option<String>>,
}

pub fn history_page(frame: &Frame, h: &History, notice: Option<Notice>) -> String {
    let mut out = String::new();
    notice_html(&mut out, notice);
    let q = &h.query;
    let all_href = q.href(|q| q.state = None);
    let mut chips = vec![Chip {
        label_html: "All",
        name: "",
        count: h.total,
        href: &all_href,
        on: q.state.is_none(),
    }];
    let hrefs: Vec<String> = StateFilter::ALL
        .iter()
        .map(|s| q.href(|q| q.state = Some(*s)))
        .collect();
    for (n, s) in StateFilter::ALL.iter().enumerate() {
        chips.push(Chip {
            label_html: s.label(),
            name: "",
            count: h.counts.iter().find(|(x, _)| x == s).map_or(0, |(_, c)| *c),
            href: &hrefs[n],
            on: q.state == Some(*s),
        });
    }
    let mut days = String::from(r#"<span class="segmented" role="group" aria-label="Period">"#);
    for d in [7u32, 30, 90] {
        let _ = write!(
            days,
            r#"<a class="segmented__option{}" href="{}">{d} d</a>"#,
            if q.days == d { " is-active" } else { "" },
            html_escape(&q.href(|q| q.days = d))
        );
    }
    days.push_str("</span>");
    let state_field = q
        .state
        .map(|s| format!(r#"<input type="hidden" name="st" value="{}">"#, s.name()))
        .unwrap_or_default();
    let period_field = if q.days != 30 {
        format!(r#"<input type="hidden" name="p" value="{}">"#, q.days)
    } else {
        String::new()
    };
    let agent_field = q
        .agent
        .as_ref()
        .map(|a| {
            format!(
                r#"<input type="hidden" name="agent" value="{}">"#,
                html_escape(a)
            )
        })
        .unwrap_or_default();
    let agent_chip = match &h.agent_filter {
        Some(name) => format!(
            r#"<a class="pill pill--wait histAgent" href="{}" title="Show the requests of all agents">Agent <b>{}</b> {}</a>"#,
            html_escape(&q.href(|q| q.agent = None)),
            name.as_deref().map_or("unknown".to_string(), html_escape),
            icon("x").replace(r#"class="icon""#, r#"class="icon icon--sm""#)
        ),
        None => String::new(),
    };
    let filter = format!(
        r#"<form class="inline histFilter" method="get" action="{ITEMS}" role="search">{state_field}{period_field}{agent_field}<input class="uiInput" type="search" name="k" value="{}" placeholder="Key" aria-label="Key" maxlength="64"><input class="uiInput" type="search" name="svc" value="{}" placeholder="Service" aria-label="Service" maxlength="64"><button class="visually-hidden" type="submit">Filter</button></form>"#,
        html_escape(&q.key),
        html_escape(&q.service)
    );
    let count = if h.truncated {
        format!("{}+", h.total)
    } else {
        h.total.to_string()
    };
    let mut body = String::new();
    for (line, chained) in &h.lines {
        body.push_str(&history_row(line, *chained, h));
    }
    let table = if h.lines.is_empty() {
        r#"<div class="uiState uiState--left"><p class="uiState__body">No request matches.</p></div>"#.to_string()
    } else {
        format!(
            r#"<div class="histWrap"><table class="grid hist"><colgroup><col class="w28"><col class="wtime"><col><col class="w64"><col class="w152"><col></colgroup><thead><tr><th></th><th>When</th><th>Item</th><th>Code</th><th>Agent</th><th>Outcome</th></tr></thead><tbody>{body}</tbody></table></div>"#
        )
    };
    let note = if h.all_accounts {
        r#"<p class="note">You read the metadata of all accounts. The content stays hidden, except for the owner.</p>"#
    } else {
        ""
    };
    let limits = if h.limits_link {
        format!(r#"<a class="button button--small" href="{LIMITS}">Limits</a>"#)
    } else {
        String::new()
    };
    let _ = write!(
        out,
        r#"<div class="page"><section class="zone" aria-labelledby="hi-t"><div class="zone__head"><h2 class="title" id="hi-t">History</h2><span class="count">{}</span><span class="grow"></span>{limits}{}</div><div class="tbar tbar--hist">{}{agent_chip}<span class="grow"></span>{filter}{days}</div><div class="scroll">{table}</div></section>{note}</div>"#,
        html_escape(&count),
        segmented(View::History, h.agents_link),
        chips_stable(&chips),
    );
    fill_page(Tab::Inbox, frame, "Inbox: history", &out)
}

fn history_row(line: &Line, chained: bool, h: &History) -> String {
    let l = line.loaded;
    let i = &l.item;
    let code = code_of(l);
    let at = i.final_at.unwrap_or(i.created_at);
    let mut who = match &line.agent {
        Some(a) => {
            let name = format!(
                r#"<a href="{ITEMS}?agent={}">{}</a>"#,
                html_escape(&a.agent_id),
                html_escape(&a.label)
            );
            if a.plain {
                name
            } else {
                format!(
                    r#"{name} <span class="muted">· {}</span>"#,
                    html::escape(&line.key_name)
                )
            }
        }
        None => html::escape(&line.key_name),
    };
    if h.all_accounts {
        let _ = write!(who, " ({})", html_escape(&i.requester_owner));
    }
    let service = l
        .approval
        .as_ref()
        .filter(|_| h.all_accounts)
        .map(|a| format!(" · {}", html_escape(&a.service)))
        .unwrap_or_default();
    format!(
        r#"<tr{}><td>{}</td><td class="muted">{}</td><td><a href="{}">{}</a></td><td class="code">{}</td><td class="who">{who}</td><td class="out">{}{service}</td></tr>"#,
        if chained { r#" class="chain""# } else { "" },
        state_icon(l),
        when(at),
        html_escape(&detail_href(l)),
        title_html(&line.title, &code),
        html_escape(&code),
        html_escape(&outcome(l, h.now_ms)),
    )
}

// One request ------------------------------------------------------------------------------------

/// The content of an approval, as the page shows it.
pub struct Text {
    /// The title of the view: one line from the service.
    pub title: String,
    /// The fields of the view with their values.
    pub fields: Vec<(String, String)>,
    pub command: String,
    pub command_label: Option<String>,
    pub language: Language,
    pub explanation: Option<String>,
    pub format: ExplanationFormat,
}

pub enum Body {
    Shown(Text),
    /// The viewer reads metadata only.
    Hidden,
    Purged,
    /// The seal does not open, or the effect of the command is not the stored one.
    Mismatch,
}

/// What a turn of the thread holds.
pub enum TurnText {
    Plain(String),
    Agent {
        explanation: Option<String>,
        format: ExplanationFormat,
        command: Option<(String, Language)>,
    },
    Unreadable,
    Empty,
}

pub struct Turn {
    pub agent: bool,
    pub at: i64,
    pub kind: String,
    pub via: Option<String>,
    pub text: TurnText,
}

/// What the viewer may do on the page of one request.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// The owner of the request.
    Owner,
    /// An account with `approval.read`: the metadata.
    Admin,
    /// The superadmin that reads the content (`admin_body_read`).
    AdminBody,
}

pub struct Detail<'a> {
    pub loaded: &'a Loaded,
    pub body: Body,
    /// The agent that sent the request: the box "From agent" and the lock of the approval.
    pub sender: Sender,
    pub turns: Vec<Turn>,
    /// The time of the newest earlier approval of the same effect and whether the service used it.
    pub previous: Option<(i64, bool)>,
    pub now_ms: i64,
    pub stale_after_ms: i64,
    pub csrf: &'a str,
    pub mode: Mode,
    /// `inbox.approvals.web_approval`.
    pub web_approval: bool,
    pub can_cancel: bool,
    /// The superadmin may open the content with `admin_body_read`.
    pub body_link: bool,
    pub max_comment: usize,
    /// The whole explanation, not the first 400 characters.
    pub full: bool,
}

fn zone_head(title: &str, id: &str, trust: &str, note: &str, extra: &str) -> String {
    format!(
        r#"<div class="sec2__head"><h2 class="title" id="{id}">{title}</h2>{trust}<span class="sec2__note">{note}</span><span class="grow"></span>{extra}</div>"#
    )
}

fn marks_banner(m: &html::Marks) -> String {
    if m.marked == 0 {
        return String::new();
    }
    let code = |list: &[u32]| {
        list.iter()
            .map(|c| format!("U+{c:04X}"))
            .collect::<Vec<_>>()
            .join(", ")
    };
    let mut parts: Vec<String> = Vec::new();
    if !m.hidden.is_empty() {
        parts.push(format!(
            "{} hidden control character{} ({})",
            m.hidden.len(),
            if m.hidden.len() == 1 { "" } else { "s" },
            code(&m.hidden)
        ));
    }
    if !m.lookalike.is_empty() {
        parts.push(format!(
            "{} letter{} that look like ASCII ({})",
            m.lookalike.len(),
            if m.lookalike.len() == 1 { "" } else { "s" },
            code(&m.lookalike)
        ));
    }
    format!(
        r#"<div class="banner banner--bad" role="alert">{}<p><b>This command has hidden or look-alike characters.</b> {} marked: {}. Look at each mark before you approve.</p></div>"#,
        icon("alert-triangle"),
        m.marked,
        parts.join(" and ")
    )
}

fn command_zone(d: &Detail, t: &Text) -> String {
    let l = d.loaded;
    let m = html::marks(&t.command);
    let lines = t.command.lines().count().max(1);
    let colors = if t.language == Language::Plain {
        String::new()
    } else {
        format!(
            r#"<span class="kind kind--auto" title="The hint of the agent only sets the colors. It does not change the text.">{} colors</span>"#,
            t.language.as_str().to_uppercase()
        )
    };
    let copy = format!(
        r#"<button class="button button--small" type="button" data-copy-text="{}" hidden>{}Copy command</button>"#,
        inbox_core::b64::encode(t.command.as_bytes()),
        icon("copy")
    );
    let mut facts = String::new();
    let mut fact = |k: &str, v_html: String| {
        let _ = write!(
            facts,
            "<div><dt>{}</dt><dd>{v_html}</dd></div>",
            html::escape(k)
        );
    };
    if let Some(a) = &l.approval {
        fact("Service", html_escape(&a.service));
        fact("Action", format!("<code>{}</code>", html_escape(&a.action)));
        if !a.resource_path.is_empty() {
            fact(
                "Resource",
                format!("<code>{}</code>", html_escape(&a.resource_path)),
            );
        }
        for (label, value) in &t.fields {
            fact(label, format!("<code>{}</code>", html::escape(value)));
        }
        fact(
            "Effect",
            format!("<code>{}</code>", html_escape(&short_effect(&a.effect))),
        );
    }
    let label = t.command_label.as_deref().unwrap_or("Command");
    let ascii = if m.non_ascii == 0 {
        "ASCII only".to_string()
    } else {
        format!(
            "{} non-ASCII character{}",
            m.non_ascii,
            if m.non_ascii == 1 { "" } else { "s" }
        )
    };
    let attention = if m.marked > 0 {
        format!(
            " · {}",
            b(&format!(
                "{} character{} need attention",
                m.marked,
                if m.marked == 1 { "" } else { "s" }
            ))
        )
    } else {
        String::new()
    };
    let effect = l
        .approval
        .as_ref()
        .map(|a| {
            format!(
                "<span>Effect {}</span>",
                b(&html_escape(&short_effect(&a.effect)))
            )
        })
        .unwrap_or_default();
    format!(
        r#"{}<section class="sec2 sec2--cmd" aria-labelledby="cmd-t">{}<dl class="sec2__facts">{facts}</dl><div class="cmdscroll" tabindex="0" role="region" aria-label="{}">{}</div><div class="sec2__foot"><span>{lines} line{} · {} byte{} · {ascii}{attention}</span>{effect}<span>Asked {}</span></div></section>"#,
        marks_banner(&m),
        zone_head(
            "Command",
            "cmd-t",
            &format!(
                r#"<span class="trust trust--ok">{}From the service</span>{colors}"#,
                icon("shield-check")
            ),
            "This is what will run. Read all of it.",
            &copy
        ),
        html::escape_attr(label),
        html::command(&t.command, t.language),
        if lines == 1 { "" } else { "s" },
        t.command.len(),
        if t.command.len() == 1 { "" } else { "s" },
        ago(d.now_ms, l.item.created_at),
    )
}

/// `sha256:` and the first 8 digits.
fn short_effect(effect: &str) -> String {
    format!(
        "sha256:{}",
        inbox_core::effect::short(effect).unwrap_or(effect)
    )
}

fn explanation_zone(d: &Detail, t: &Text) -> String {
    let head = |tag: &str, note: &str| {
        zone_head(
            "Agent explanation",
            "exp-t",
            &format!(
                r#"<span class="trust trust--no">{}Not checked</span>{tag}"#,
                icon("alert-triangle")
            ),
            note,
            "",
        )
    };
    let fixed = r#"<p class="exp__fixed">This text does not change what will run. It is not part of the effect.</p>"#;
    let Some(text) = &t.explanation else {
        return format!(
            r#"<section class="sec2 sec2--exp" aria-labelledby="exp-t">{}<p class="exp__none">No explanation given by the agent.</p></section>"#,
            head("", "Text of the agent, not verified.")
        );
    };
    let markdown = t.format == ExplanationFormat::Markdown;
    let long = text.chars().count() > FOLD_CHARS;
    let (shown, more) = if long && !d.full {
        let cut: String = text.chars().take(FOLD_CHARS).collect();
        (
            cut,
            format!(
                r#"<p class="exp__more"><a href="{}?full=1#exp-t">Show all ({} characters)</a></p>"#,
                html_escape(&detail_href(d.loaded)),
                text.chars().count()
            ),
        )
    } else {
        (text.clone(), String::new())
    };
    let tag = if markdown {
        r#"<span class="kind kind--auto">Markdown</span>"#
    } else {
        ""
    };
    let foot = if markdown {
        r#"<p class="exp__fixed">Links do not open. No images. No HTML.</p>"#
    } else {
        ""
    };
    format!(
        r#"<section class="sec2 sec2--exp" aria-labelledby="exp-t">{}<div class="exp__text">{}</div>{more}{foot}{fixed}</section>"#,
        head(
            tag,
            if markdown {
                "Text of the agent, not verified. Shown as Markdown."
            } else {
                "Text of the agent, not verified."
            }
        ),
        html::explanation(&shown, t.format)
    )
}

fn decision(d: &Detail) -> String {
    let l = d.loaded;
    let i = &l.item;
    if !i.is_open() {
        return format!(
            r#"<div class="decide"><div class="sec2__head"><h2 class="title">Outcome</h2></div><div class="decide__body"><p>{}</p></div></div>"#,
            html_escape(&outcome(l, d.now_ms))
        );
    }
    if d.mode != Mode::Owner {
        return String::new();
    }
    let Some(a) = &l.approval else {
        return String::new();
    };
    let hidden = format!(
        r#"<input type="hidden" name="csrf" value="{}"><input type="hidden" name="request_id" value="{}"><input type="hidden" name="shown_effect" value="{}">"#,
        html_escape(d.csrf),
        html_escape(&i.request_id),
        html_escape(&a.effect)
    );
    let mut out = String::new();
    if !d.web_approval {
        out.push_str(r#"<div class="decide"><div class="sec2__head"><h2 class="title">Your decision</h2></div><div class="decide__body"><p class="note">This server takes answers on the phone only. Answer from your phone.</p></div></div>"#);
    } else {
        let age = d.now_ms - i.created_at;
        let stale = if age > d.stale_after_ms {
            format!(
                r#"<div class="stale"><label><input type="checkbox" name="stale_ack" value="1" required> <span>I know this request is {} old. <span class="req">Required to approve.</span></span></label></div>"#,
                long_span(age)
            )
        } else {
            String::new()
        };
        let comment = if i.allow_custom {
            format!(
                r#"<details><summary>{}Decline with a comment</summary><div class="inner"><div class="field"><label class="field__label" for="cm">Comment for the agent <span class="muted">(optional)</span></label><textarea class="uiInput" id="cm" name="text" rows="3" maxlength="{}" placeholder="For example: use staging_b, not prod"></textarea><span class="note">The agent reads this text. Do not type a password or a secret.</span></div><div class="radios" role="radiogroup" aria-label="Kind of comment"><label><input type="radio" name="intent" value="comment" checked> Comment</label><label><input type="radio" name="intent" value="change_request"> Propose a change</label></div><p class="note">You cannot edit the command. If you propose a change, the agent can send a new request.</p><div class="decide__row"><span class="grow"></span><button class="button" type="submit" name="action" value="decline" formnovalidate>Decline with comment</button></div></div></details>"#,
                icon("chevron-right"),
                d.max_comment,
            )
        } else {
            String::new()
        };
        // The server decides the lock. The page shows it, so the owner knows why the button is off.
        let (lock_line, approve) = if d.sender.locked() {
            (
                format!(
                    r#"<div class="lockline">{}<span>Approve is locked. This agent is new. Name it or allow it first. You can decline now.</span></div>"#,
                    icon("lock")
                ),
                format!(
                    r#"<button class="button button--approve button--off" type="button" disabled>{}Approve</button>"#,
                    icon("lock")
                ),
            )
        } else {
            (
                String::new(),
                format!(
                    r#"<button class="button button--approve" type="submit" name="action" value="accept">{}Approve</button>"#,
                    icon("check")
                ),
            )
        };
        let _ = write!(
            out,
            r#"<form class="decide" method="post" action="{ANSWER}" aria-label="Decision"><div class="sec2__head"><h2 class="title">Your decision</h2><span class="sec2__note">Approve sends no text.</span></div><div class="decide__body">{hidden}{stale}{lock_line}<div class="decide__row">{approve}<button class="button" type="submit" name="action" value="decline" formnovalidate>{}Decline</button><span class="grow"></span><span class="note">The agent must run the command within the time of the file after you approve.</span></div><p class="note">{}This page cannot check that you read the whole command. The phone can. Read the command to the end before you approve.</p>{comment}</div></form>"#,
            icon("x"),
            icon("alert-triangle").replace("class=\"icon\"", "class=\"icon icon--sm\""),
        );
    }
    if i.allow_ask_back {
        let _ = write!(
            out,
            r#"<form class="decide decide--ask" method="post" action="{REPLY}"><div class="decide__body"><input type="hidden" name="csrf" value="{}"><input type="hidden" name="request_id" value="{}"><details><summary>{}Ask the agent a question</summary><div class="inner"><div class="field"><label class="field__label" for="rp">Question</label><textarea class="uiInput" id="rp" name="text" rows="3" maxlength="{MAX_REPLY_CHARS}" required></textarea><span class="note">The agent reads this text. Do not type a password or a secret. A question is not an answer: the request stays open.</span></div><div class="decide__row"><span class="grow"></span><button class="button" type="submit">{}Send</button></div></div></details></div></form>"#,
            html_escape(d.csrf),
            html_escape(&i.request_id),
            icon("chevron-right"),
            icon("send"),
        );
    }
    out
}

fn thread(d: &Detail) -> String {
    if d.turns.is_empty() {
        return String::new();
    }
    let mut items = String::new();
    for t in &d.turns {
        let who = if t.agent { "Agent" } else { "You" };
        let via = t
            .via
            .as_deref()
            .map(|v| format!(" · {}", html_escape(v)))
            .unwrap_or_default();
        let body = match &t.text {
            TurnText::Plain(s) => format!(r#"<p class="md-raw">{}</p>"#, html::escape(s)),
            TurnText::Agent {
                explanation,
                format,
                command,
            } => {
                let mut s = String::new();
                if let Some(e) = explanation {
                    s.push_str(&html::explanation(e, *format));
                }
                if let Some((c, lang)) = command {
                    let _ = write!(
                        s,
                        r#"<div class="cmdscroll" tabindex="0" role="region" aria-label="Command of the agent">{}</div>"#,
                        html::command(c, *lang)
                    );
                }
                s
            }
            TurnText::Unreadable => r#"<p class="muted">This text cannot be read.</p>"#.to_string(),
            TurnText::Empty => format!(r#"<p class="muted">{}</p>"#, html_escape(&t.kind)),
        };
        let tag = if t.agent {
            r#"<span class="turn__tag">Agent text, not checked</span>"#
        } else {
            ""
        };
        let _ = write!(
            items,
            r#"<div class="turn turn--{}"><div class="turn__who"><b>{who}</b>{tag}<span>{}{via}</span></div><div class="turn__bubble">{body}</div></div>"#,
            if t.agent { "agent" } else { "user" },
            when(t.at),
        );
    }
    format!(
        r#"<section class="sec2" aria-labelledby="th-t"><div class="sec2__head"><h2 class="title" id="th-t">Thread</h2></div><div class="thread">{items}</div></section>"#
    )
}

fn side(d: &Detail) -> String {
    let l = d.loaded;
    let i = &l.item;
    let mut req = format!(
        "<dl class=\"facts\"><div><dt>Code</dt><dd><code>{}</code></dd></div><div><dt>Interaction</dt><dd>{}</dd></div></dl>",
        html_escape(&code_of(l)),
        match i.interaction {
            inbox_core::proto::Interaction::Conversational => "Conversational",
            _ => "One shot",
        }
    );
    if let Some((at, used)) = d.previous {
        let _ = write!(
            req,
            r#"<p>Same effect approved {}{}. The server compares the effect hash.</p>"#,
            if used { "and used " } else { "" },
            ago(d.now_ms, at)
        );
    }
    let line = |class: &str, at: i64, text: &str| {
        format!(
            r#"<li class="{class}"><b>{}</b> {}</li>"#,
            when(at),
            html_escape(text)
        )
    };
    let mut tl = line("is-now", i.created_at, "Request made");
    if i.agent_seen_at > i.created_at {
        tl.push_str(&line(
            "",
            i.agent_seen_at,
            &format!("Agent active ({})", ago(d.now_ms, i.agent_seen_at)),
        ));
    }
    if let Some(at) = i.answered_at {
        tl.push_str(&line(
            "is-now",
            at,
            &format!("Answer: {}", word(i.state, i.answer_action).to_lowercase()),
        ));
    }
    if let Some(at) = i.abandoned_at {
        tl.push_str(&line(
            "is-bad",
            at,
            "The agent no longer answered: abandoned",
        ));
    }
    if i.is_open() {
        if let Some(at) = i.lease_until {
            tl.push_str(&line("", at, "Lease ends"));
            tl.push_str(r#"<li>After that the item becomes <b>abandoned</b>.</li>"#);
        }
    }
    let who = match d.mode {
        Mode::Owner => "The service sees only the verdict. Your comment and your questions go to the agent, never to the service.",
        _ => "The service sees only the verdict. The content is for the owner. An administrator reads metadata only.",
    };
    let about = about_sender(
        &d.sender,
        &i.requester_owner,
        &ago(d.now_ms, i.agent_seen_at),
    );
    format!(
        r#"<aside class="detail__side">{about}<div class="sidebox"><h3>Request</h3>{req}</div><div class="sidebox"><h3>Time line</h3><ul class="timeline">{tl}</ul></div><div class="sidebox"><h3>Who sees what</h3><p>{who}</p></div></aside>"#
    )
}

/// The page of one request.
pub fn detail_page(frame: &Frame, d: &Detail, notice: Option<Notice>) -> String {
    let l = d.loaded;
    let i = &l.item;
    let code = code_of(l);
    let mut out = String::new();
    notice_html(&mut out, notice);
    let stale = i.is_open() && d.now_ms - i.created_at > d.stale_after_ms;
    let state_pill = match i.state {
        ItemState::Pending | ItemState::Replied if d.mode != Mode::Owner => {
            r#"<span class="pill">Open</span>"#.to_string()
        }
        ItemState::Pending => r#"<span class="pill pill--wait">Waiting for you</span>"#.to_string(),
        ItemState::Replied => r#"<span class="pill">Question sent</span>"#.to_string(),
        s => format!(r#"<span class="pill">{}</span>"#, word(s, i.answer_action)),
    };
    let old = if stale {
        format!(
            r#"<span class="pill pill--warn">{}Old request</span>"#,
            icon("clock")
        )
    } else {
        String::new()
    };
    let title = match &d.body {
        Body::Shown(t) => html::escape(&t.title),
        _ => format!("Request {}", html_escape(&code)),
    };
    let lease = match (i.lease_until, i.until_at, i.is_open()) {
        (Some(at), None, true) => {
            format!(r#"<span class="lease">Lease until {}</span>"#, b(&when(at)))
        }
        (_, Some(at), true) => {
            format!(r#"<span class="lease">Waits until {}</span>"#, b(&when(at)))
        }
        _ => String::new(),
    };
    let head = format!(
        r#"<div class="head"><div class="chipStack">{}<span class="code">{}</span>{old}{state_pill}</div><h2>{title}</h2><div class="head__row"><span class="lease">Asked {}</span><span class="lease">Agent active {}</span>{lease}</div></div>{}"#,
        kind_chip(i.kind),
        html_escape(&code),
        b(&ago(d.now_ms, i.created_at)),
        b(&ago(d.now_ms, i.agent_seen_at)),
        sender_box(&d.sender, &i.requester_owner),
    );
    let stale_banner = if stale && d.mode == Mode::Owner {
        format!(
            r#"<div class="banner banner--warn" role="alert">{}<p><b>This request is old.</b> The data may have changed since {}. Check the command again. You must tick the box below to approve.</p></div>"#,
            icon("alert-triangle"),
            when(i.created_at)
        )
    } else {
        String::new()
    };
    let mut main = format!("{head}{stale_banner}");
    match &d.body {
        Body::Shown(t) => {
            if d.mode == Mode::AdminBody {
                let _ = write!(
                    main,
                    r#"<div class="banner banner--info">{}<p>You read this content as the administrator. The server wrote one line in the audit. The line has no content.</p></div>"#,
                    icon("shield-check")
                );
            }
            main.push_str(&command_zone(d, t));
            main.push_str(&explanation_zone(d, t));
            main.push_str(&thread(d));
            main.push_str(&decision(d));
        }
        other => {
            let text = match other {
                Body::Purged => "The content of this request is deleted. The server keeps the metadata and the effect hash.",
                Body::Mismatch => "The server cannot show this content: the seal does not open, or the command does not give the stored effect hash. Do not approve.",
                _ => "You read the metadata only. The content of a request is for its owner.",
            };
            let _ = write!(
                main,
                r#"<div class="banner banner--info">{}<p>{text}</p></div>"#,
                icon("shield-check")
            );
            if d.body_link {
                let _ = write!(
                    main,
                    r#"<p><a class="button button--small" href="{}/body">Read the content</a> <span class="note">The server writes one line in the audit.</span></p>"#,
                    html_escape(&detail_href(l))
                );
            }
            if let Some(a) = &l.approval {
                let _ = write!(
                    main,
                    r#"<div class="sec2"><dl class="sec2__facts"><div><dt>Service</dt><dd>{}</dd></div><div><dt>Action</dt><dd><code>{}</code></dd></div><div><dt>Effect</dt><dd><code>{}</code></dd></div></dl></div>"#,
                    html_escape(&a.service),
                    html_escape(&a.action),
                    html_escape(&short_effect(&a.effect))
                );
            }
            main.push_str(&decision(d));
        }
    }
    if d.can_cancel && i.is_open() {
        let _ = write!(
            main,
            r#"<form class="decide" method="post" action="{CANCEL}"><div class="decide__body"><input type="hidden" name="csrf" value="{}"><input type="hidden" name="request_id" value="{}"><div class="decide__row"><button class="button" type="submit">{}Cancel this request</button><span class="note">The agent and the service see a closed request. This is not a refusal.</span></div></div></form>"#,
            html_escape(d.csrf),
            html_escape(&i.request_id),
            icon("ban"),
        );
    }
    let _ = write!(
        out,
        r#"<div class="page page--narrow"><nav class="crumbs" aria-label="Path"><a href="{ITEMS}">{}History</a><span>/</span><b>Approval {}</b></nav><div class="detail"><div class="detail__main">{main}</div>{}</div></div>"#,
        icon("arrow-left"),
        html_escape(&code),
        side(d),
    );
    fill_page(Tab::Inbox, frame, &format!("Inbox: request {code}"), &out)
}

// The limits -------------------------------------------------------------------------------------

/// One key of the file with the value that applies and its source.
pub struct LimitRow {
    pub key: String,
    pub value: String,
    pub source: String,
}

pub fn limits_page(frame: &Frame, rows: &[LimitRow], admin_body_read: bool) -> String {
    let mut body = String::from(
        r#"<div class="lim"><div class="lim__row lim__row--head"><span>Key</span><span>Value</span><span>Source</span></div>"#,
    );
    for r in rows {
        let hidden = r.value == crate::config::HIDDEN;
        let _ = write!(
            body,
            r#"<div class="lim__row"><span class="lim__k">{}</span><span class="lim__v{}">{}</span><span class="lim__s">{}</span></div>"#,
            html_escape(&r.key),
            if hidden { " is-mask" } else { "" },
            if hidden {
                "value hidden".to_string()
            } else {
                html_escape(&r.value)
            },
            html_escape(&r.source),
        );
    }
    body.push_str("</div>");
    let reading = if admin_body_read {
        "The administrator can read the content of items: <code>inbox.at_rest.admin_body_read</code> is true. Each read leaves a line in the audit."
    } else {
        "Administrators cannot read the content of items. The key <code>inbox.at_rest.admin_body_read</code> is false."
    };
    let out = format!(
        r#"<div class="page page--narrow"><nav class="crumbs" aria-label="Path"><a href="{ITEMS}">{}History</a><span>/</span><b>Inbox limits</b></nav><section class="zone"><div class="zone__head"><h2 class="title">Inbox limits</h2><span class="count">{}</span><span class="grow"></span><span class="pill">{}Read only</span></div><div class="scroll">{body}</div></section><p class="note">These are the values that apply, as the command <code>check-config</code> shows them. To change one, edit the configuration file and restart the server. A secret key shows its id and its source, never its value.</p><div class="banner banner--info">{}<p>{reading}</p></div></div>"#,
        icon("arrow-left"),
        rows.len(),
        icon("lock"),
        icon("shield-check"),
    );
    fill_page(Tab::Inbox, frame, "Inbox: limits", &out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::inbox::agents::AgentKind;
    use crate::inbox::page_items::{ApprovalRow, ItemRow, Loaded};
    use inbox_core::proto::{Interaction, Urgency, Via};

    const NOW: i64 = 1_800_000_000_000;

    fn frame() -> Frame<'static> {
        Frame {
            logout_token: "out",
            keys: true,
            inbox: true,
            superadmin: false,
            notify: None,
            security: false,
            tab: crate::ui::TabLook::DEFAULT,
        }
    }

    /// An approval that waits, made `created` ms (Unix time) ago.
    fn approval(code: &str, created: i64) -> Loaded {
        Loaded {
            item: ItemRow {
                request_id: "0123456789abcdef0123456789abcdef".into(),
                kind: Kind::Approval,
                chain_id: "c".into(),
                supersedes: None,
                superseded_by: None,
                previous_id: None,
                requester_key_id: "k1".into(),
                requester_owner: "alice".into(),
                agent_id: "a1".into(),
                via: Via::Mcp,
                interaction: Interaction::Conversational,
                urgency: Urgency::Normal,
                input_type: None,
                state: ItemState::Pending,
                settle_reason: None,
                body_kid: None,
                body_size: 0,
                body_purged_at: None,
                created_at: created,
                chain_created_at: created,
                lease_s: Some(60),
                lease_until: Some(NOW + 180_000),
                until_at: None,
                agent_seen_at: NOW - 180_000,
                last_agent_turn_at: None,
                abandoned_at: None,
                revivals: 0,
                last_seq: 0,
                allow_custom: false,
                allow_ask_back: false,
                answer_action: None,
                answered_at: None,
                answered_by: None,
                answered_via: None,
                final_at: None,
                idempotency_key: "i".into(),
            },
            approval: Some(ApprovalRow {
                request_id: "0123456789abcdef0123456789abcdef".into(),
                service: "db".into(),
                subject: "key:k1".into(),
                action: "data.write".into(),
                resource_path: "host:prod".into(),
                effect: format!("sha256:{}", "ab".repeat(32)),
                key_hash: "h".into(),
                evaluation_id: None,
                code: code.into(),
                decision: None,
                shown_effect: None,
                consume_before: None,
                consumed_at: None,
                lapsed_at: None,
            }),
        }
    }

    fn text(command: &str) -> Text {
        Text {
            title: "Write <b>to</b> ClickHouse".into(),
            fields: vec![("Host".into(), "prod".into())],
            command: command.into(),
            command_label: Some("SQL".into()),
            language: Language::Sql,
            explanation: Some("Clean up. Safe to approve\u{202E}.".into()),
            format: ExplanationFormat::Plain,
        }
    }

    /// The agent view that the tests use: a known agent of the key `ops-agent`.
    fn agent(id: &str, label: &str, state: AgentState) -> AgentView {
        AgentView {
            agent_id: id.into(),
            label: label.into(),
            key_name: "ops-agent".into(),
            kind: AgentKind::Agent,
            state,
            named: !label.contains('#'),
            created_at: NOW - 86_400_000,
            last_seen_at: NOW - 60_000,
            observed: None,
            plain: false,
        }
    }

    fn bucket(plain: bool) -> AgentView {
        AgentView {
            kind: AgentKind::Bucket,
            state: AgentState::Known,
            label: if plain {
                "ops-agent".into()
            } else {
                "Unattributed · ops-agent".into()
            },
            plain,
            ..agent("b1", "", AgentState::Known)
        }
    }

    fn sender(a: AgentView) -> Sender {
        Sender {
            agent: a,
            lookalike: None,
            owner: true,
            lock: false,
        }
    }

    fn line(l: &Loaded) -> Line<'_> {
        Line {
            loaded: l,
            title: Title::Text("Run <script>".into()),
            first_line: Some("DELETE FROM t".into()),
            key_name: "ops-agent".into(),
            agent: None,
        }
    }

    fn panel<'a>(groups: Vec<Group<'a>>, notices: Vec<FirstUse<'a>>) -> OpenItems<'a> {
        OpenItems {
            groups,
            notices,
            lock: false,
            agents_link: true,
            stale_after_ms: 86_400_000,
            now_ms: NOW,
        }
    }

    fn detail<'a>(l: &'a Loaded, body: Body, mode: Mode) -> Detail<'a> {
        Detail {
            loaded: l,
            body,
            sender: sender(bucket(true)),
            turns: Vec::new(),
            previous: None,
            now_ms: NOW,
            stale_after_ms: 86_400_000,
            csrf: "tok<",
            mode,
            web_approval: true,
            can_cancel: false,
            body_link: false,
            max_comment: 2000,
            full: false,
        }
    }

    #[test]
    fn the_panel_is_absent_when_nothing_waits_and_shows_the_old_mark_when_it_is_old() {
        assert_eq!(open_items(&panel(vec![], vec![])), "");
        let l = approval("K7QF", NOW - 3 * 86_400_000);
        let html = open_items(&panel(
            vec![Group {
                agent: bucket(true),
                lines: vec![line(&l)],
            }],
            vec![],
        ));
        assert!(html.contains("Open items") && html.contains("K7QF"));
        assert!(html.contains("Old request") && html.contains("Waiting for you"));
        assert!(html.contains("Run &lt;script&gt;") && !html.contains("<script"));
        assert!(html.contains("DELETE FROM t"));
        // A link, not a script, opens the request.
        assert!(html.contains(r#"href="/auth/settings/inbox/items/"#));
        // The row shows the service and the action, not the key: the header of the group has the key.
        assert!(html.contains("db · <code>data.write</code>") && !html.contains("key of"));
    }

    #[test]
    fn groups_order_new_first_then_oldest_bucket_last() {
        let (a, b, c, d) = (
            approval("AAAA", NOW - 60_000),
            approval("BBBB", NOW - 3_600_000),
            approval("CCCC", NOW - 600_000),
            approval("DDDD", NOW - 7_200_000),
        );
        fn group(agent: AgentView, l: &Loaded) -> Group<'_> {
            Group {
                agent,
                lines: vec![line(l)],
            }
        }
        let html = open_items(&panel(
            vec![
                group(bucket(false), &d),
                group(agent("g1", "Billing bot", AgentState::Known), &a),
                group(agent("g2", "Night ETL", AgentState::Known), &b),
                group(agent("g3", "ops-agent #4", AgentState::New), &c),
            ],
            vec![],
        ));
        let at = |name: &str| {
            html.find(&format!(r#"<span class="agrp__name">{name}</span>"#))
                .unwrap_or_else(|| panic!("{name}"))
        };
        // The new agent first, then the oldest request, the bucket last.
        let order = [
            at("ops-agent #4"),
            at("Night ETL"),
            at("Billing bot"),
            at("Unattributed · ops-agent"),
        ];
        assert!(order.windows(2).all(|w| w[0] < w[1]), "{order:?}");
        // One group for each agent, each one open, and the count of the panel is the count of the rows.
        assert_eq!(html.matches("<details class=\"agrp").count(), 4);
        assert_eq!(
            html.matches("<details class=\"agrp").count(),
            html.matches(" open><summary>").count()
        );
        assert!(html.contains(r#"<span class="count">4</span>"#));
        // The new group and the bucket have their marks, and the bucket has no button.
        assert!(html.contains("agrp agrp--new") && html.contains("New, not named"));
        assert!(html.contains("agrp agrp--bucket") && html.contains("pill--un"));
        assert_eq!(html.matches("Manage").count(), 2);
        // A key that is the only agent has a group with the name of the key and nothing else.
        let l = approval("EEEE", NOW - 60_000);
        let one = open_items(&panel(
            vec![Group {
                agent: bucket(true),
                lines: vec![line(&l)],
            }],
            vec![],
        ));
        assert!(one.contains(r#"<span class="agrp__name">ops-agent</span>"#));
        assert!(
            !one.contains("keychip")
                && !one.contains("pill--un")
                && !one.contains("Observed, not verified")
        );
        assert!(
            !one.contains("agrp--bucket")
                && !one.contains("Manage")
                && !one.contains("Groups are by agent")
        );
    }

    #[test]
    fn group_html_escapes_name_and_says_and_marks_hidden_characters() {
        let l = approval("K7QF", NOW - 60_000);
        let mut a = agent("g1", "Bot <img src=x onerror=alert(1)>", AgentState::Known);
        a.observed = Some(crate::inbox::agents::Observed {
            says: Some("billing\u{202E}tob <script>alert(2)</script> p\u{430}ypal".into()),
            client: Some("claude\u{200B}-code\"><b>".into()),
            net: Some("203.0.113.0/24".into()),
            ..Default::default()
        });
        a.key_name = "key<i>".into();
        let html = open_items(&panel(
            vec![Group {
                agent: a,
                lines: vec![line(&l)],
            }],
            vec![],
        ));
        assert!(html.contains("Bot &lt;img src=x onerror=alert(1)&gt;"));
        assert!(
            !html.contains("<img") && !html.contains("<script") && !html.contains("-code\"><b>")
        );
        assert!(html.contains("-code&quot;&gt;&lt;b&gt;"));
        assert!(html.contains("key&lt;i&gt;"));
        // The raw hidden characters never reach the page. The marks do, and the Cyrillic letter has its code.
        assert!(!html.contains('\u{202E}') && !html.contains('\u{200B}'));
        assert!(html.contains("U+202E") && html.contains("U+200B") && html.contains("U+0430"));
        assert!(html.contains("Observed, not verified") && html.contains("From 203.0.113.0/24"));
        // The line of the hints holds the text of the agent in a quote, never in the title.
        assert!(html.contains("<q>billing"));
    }

    #[test]
    fn a_locked_new_agent_shows_the_lock_in_its_rows_and_a_free_one_does_not() {
        let l = approval("K7QF", NOW - 60_000);
        let mut p = panel(
            vec![Group {
                agent: agent("g3", "ops-agent #4", AgentState::New),
                lines: vec![line(&l)],
            }],
            vec![],
        );
        assert!(!open_items(&p).contains("Approve is locked"));
        p.lock = true;
        let html = open_items(&p);
        assert!(html.contains("Approve is locked until you allow this agent. You can decline now."));
        assert!(html.contains(r#"class="lockline""#));
        // A known agent and the bucket are never locked.
        p.groups[0].agent.state = AgentState::Known;
        assert!(!open_items(&p).contains("lockline"));
    }

    #[test]
    fn the_approval_page_has_the_box_from_agent_and_the_lock_only_with_the_switch() {
        let l = approval("V5HC", NOW - 60_000);
        let mut a = agent("g3", "ops-agent #4", AgentState::New);
        a.observed = Some(crate::inbox::agents::Observed {
            says: Some("billing-bot".into()),
            client: Some("claude-code 2.1".into()),
            net: Some("203.0.113.0/24".into()),
            ..Default::default()
        });
        let mut d = detail(&l, Body::Shown(text("SELECT 1")), Mode::Owner);
        d.sender = Sender {
            lookalike: Some(crate::ui::agents::Lookalike {
                label: "Billing bot".into(),
                same_client: true,
                same_net: false,
            }),
            ..sender(a)
        };
        let free = detail_page(&frame(), &d, None);
        // The name is portal text. The says text is in the dashed box, with the label "Not verified".
        assert!(free.contains(r#"<span class="sender__name">ops-agent #4</span>"#));
        assert!(
            free.contains("From agent")
                && free.contains("New, not named")
                && free.contains("Name it")
        );
        assert!(free.contains("Not verified") && free.contains("Says it is: <q>billing-bot</q>"));
        assert!(
            free.contains("Same text as the name of your agent Billing bot. This can be a copy.")
        );
        assert!(free.contains("About the sender") && free.contains("written by the server"));
        // Without the lock, Approve works.
        assert!(free.contains(r#"name="action" value="accept""#) && !free.contains("lockline"));
        d.sender.lock = true;
        let locked = detail_page(&frame(), &d, None);
        assert!(locked.contains("Approve is locked. This agent is new."));
        assert!(
            !locked.contains(r#"name="action" value="accept""#),
            "no accept button"
        );
        assert!(
            locked.contains("button--off") && locked.contains(r#"name="action" value="decline""#)
        );
        // An administrator reads the name of the agent and nothing that the agent said.
        let mut d = detail(&l, Body::Hidden, Mode::Admin);
        let mut a = agent("g3", "Billing bot", AgentState::Known);
        a.observed = None;
        d.sender = Sender {
            owner: false,
            ..sender(a)
        };
        let page = detail_page(&frame(), &d, None);
        assert!(
            page.contains("Billing bot")
                && !page.contains("Says it is")
                && !page.contains("Agent page")
        );
    }

    #[test]
    fn the_command_is_first_and_the_copy_text_is_the_exact_bytes() {
        let l = approval("K7QF", NOW - 60_000);
        let command = "SELECT 1\r\nFROM t -- \u{202E}x\u{200B}\n";
        let mut t = text(command);
        // A label with a quote and a hidden character stays inside its attribute.
        t.command_label = Some("SQL\u{202E}\"x".into());
        let d = detail(&l, Body::Shown(t), Mode::Owner);
        let page = detail_page(&frame(), &d, None);
        let cmd = page.find(r#"id="cmd-t""#).unwrap();
        let exp = page.find(r#"id="exp-t""#).unwrap();
        let decide = page.find("Your decision").unwrap();
        assert!(cmd < exp && exp < decide);
        // The button holds the bytes of the command, and it needs a script: it is hidden without one.
        let at = page.find("data-copy-text=\"").unwrap() + "data-copy-text=\"".len();
        let b64 = page[at..].split('"').next().unwrap();
        assert_eq!(inbox_core::b64::decode(b64).unwrap(), command.as_bytes());
        assert!(page[at..].split('>').next().unwrap().contains("hidden"));
        // The raw characters never reach the page, the marks do.
        assert!(!page.contains('\u{202E}') && !page.contains('\u{200B}'));
        assert!(page.contains("U+202E") && page.contains("U+200B"));
        assert!(page.contains("hidden or look-alike characters"));
        assert!(page.contains(r#"aria-label="SQL&lt;U+202E&gt;&quot;x""#));
        assert!(page.contains("Not checked") && page.contains("not verified"));
        assert!(page.contains("does not change what will run"));
        assert!(page.contains("cannot check that you read the whole command"));
        assert!(page.contains("Write &lt;b&gt;to&lt;/b&gt; ClickHouse"));
    }

    #[test]
    fn an_old_request_has_the_banner_and_the_required_box_and_a_new_one_has_neither() {
        let old = approval("K7QF", NOW - 6 * 86_400_000);
        let page = detail_page(
            &frame(),
            &detail(&old, Body::Shown(text("SELECT 1")), Mode::Owner),
            None,
        );
        assert!(page.contains("This request is old."));
        assert!(page.contains(r#"name="stale_ack" value="1" required"#));
        assert!(page.contains("I know this request is 6 days old."));
        let new = approval("K7QF", NOW - 60_000);
        let page = detail_page(
            &frame(),
            &detail(&new, Body::Shown(text("SELECT 1")), Mode::Owner),
            None,
        );
        assert!(!page.contains("stale_ack") && !page.contains("This request is old."));
        // No choice is pre-selected: the buttons carry the action, and the form has no default action.
        assert!(page.contains(r#"name="action" value="accept""#));
        assert!(page.contains(r#"name="shown_effect" value="sha256:"#));
    }

    #[test]
    fn the_comment_field_needs_allow_custom_and_the_question_needs_allow_ask_back() {
        let mut l = approval("K7QF", NOW - 60_000);
        let page = detail_page(
            &frame(),
            &detail(&l, Body::Shown(text("SELECT 1")), Mode::Owner),
            None,
        );
        assert!(
            !page.contains("Decline with a comment") && !page.contains("Ask the agent a question")
        );
        l.item.allow_custom = true;
        l.item.allow_ask_back = true;
        let page = detail_page(
            &frame(),
            &detail(&l, Body::Shown(text("SELECT 1")), Mode::Owner),
            None,
        );
        assert!(
            page.contains("Decline with a comment") && page.contains("Ask the agent a question")
        );
        assert!(page.contains("Do not type a password or a secret."));
    }

    #[test]
    fn the_web_that_does_not_decide_has_no_form_of_decision() {
        let l = approval("K7QF", NOW - 60_000);
        let mut d = detail(&l, Body::Shown(text("SELECT 1")), Mode::Owner);
        d.web_approval = false;
        let page = detail_page(&frame(), &d, None);
        assert!(page.contains("answers on the phone only"));
        assert!(!page.contains(ANSWER) && !page.contains(r#"value="accept""#));
    }

    #[test]
    fn an_administrator_reads_metadata_and_never_a_content_or_a_form_of_decision() {
        let l = approval("K7QF", NOW - 60_000);
        let mut d = detail(&l, Body::Hidden, Mode::Admin);
        d.can_cancel = true;
        d.body_link = true;
        let page = detail_page(&frame(), &d, None);
        assert!(page.contains("metadata only") && !page.contains(r#"id="cmd-t""#));
        assert!(!page.contains(ANSWER) && !page.contains("Approve"));
        assert!(page.contains(CANCEL) && page.contains("/body"));
        // The purged content and the content that does not give its effect have their words.
        let page = detail_page(&frame(), &detail(&l, Body::Purged, Mode::Owner), None);
        assert!(page.contains("is deleted"));
        let page = detail_page(&frame(), &detail(&l, Body::Mismatch, Mode::Owner), None);
        assert!(page.contains("Do not approve"));
    }

    #[test]
    fn the_explanation_folds_after_400_characters_and_markdown_has_its_tag() {
        let l = approval("K7QF", NOW - 60_000);
        let mut t = text("SELECT 1");
        t.explanation = Some(format!("{}END", "a".repeat(500)));
        t.format = ExplanationFormat::Markdown;
        let mut d = detail(&l, Body::Shown(t), Mode::Owner);
        let page = detail_page(&frame(), &d, None);
        assert!(page.contains("Show all (503 characters)") && !page.contains("END"));
        assert!(page.contains("Markdown") && page.contains("Links do not open."));
        d.full = true;
        assert!(detail_page(&frame(), &d, None).contains("END"));
    }

    #[test]
    fn the_history_query_is_closed_and_its_links_keep_the_other_filters() {
        let pairs = |s: &str| -> Vec<(String, String)> {
            s.split('&')
                .filter_map(|p| p.split_once('='))
                .map(|(a, b)| (a.to_string(), b.to_string()))
                .collect()
        };
        let q = HistoryQuery::parse(&pairs("st=abandoned&k=ops&p=7")).unwrap();
        assert_eq!(
            (q.state, q.key.as_str(), q.days),
            (Some(StateFilter::Abandoned), "ops", 7)
        );
        assert_eq!(
            q.href(|q| q.days = 30),
            "/auth/settings/inbox/items?st=abandoned&k=ops"
        );
        assert_eq!(HistoryQuery::default().href(|_| {}), ITEMS);
        let id = "0123456789abcdef0123456789ABCDEF";
        let q = HistoryQuery::parse(&pairs(&format!("agent={id}&p=7"))).unwrap();
        assert_eq!(q.agent.as_deref(), Some(id.to_lowercase().as_str()));
        assert_eq!(
            q.href(|q| q.days = 30),
            format!("/auth/settings/inbox/items?agent={}", id.to_lowercase())
        );
        assert_eq!(q.href(|q| q.agent = None), "/auth/settings/inbox/items?p=7");
        for bad in [
            "x=1",
            "st=open",
            "p=8",
            "st=approved&st=declined",
            "k=a\u{0}b",
            "agent=abc",
            "agent=0123456789abcdef0123456789abcdeg",
            "agent=%27",
        ] {
            assert!(HistoryQuery::parse(&pairs(bad)).is_none(), "{bad}");
        }
    }

    #[test]
    fn the_limits_page_hides_a_secret_and_shows_the_source() {
        let rows = vec![
            LimitRow {
                key: "inbox.items.max_lifetime".into(),
                value: "90d".into(),
                source: "file".into(),
            },
            LimitRow {
                key: "inbox.at_rest.key.k1.secret".into(),
                value: crate::config::HIDDEN.into(),
                source: "file:/run/k1".into(),
            },
        ];
        let page = limits_page(&frame(), &rows, false);
        assert!(
            page.contains("90d") && page.contains("file:/run/k1") && page.contains("value hidden")
        );
        assert!(page.contains("Read only") && page.contains("is false"));
        assert!(!page.contains("&lt;hidden&gt;"));
    }

    #[test]
    fn the_history_names_the_agent_with_a_link_and_the_filter_has_a_chip() {
        let l = approval("K7QF", NOW - 60_000);
        let mut done = l.clone();
        done.item.state = ItemState::Cancelled;
        done.item.settle_reason = Some("agent_blocked".into());
        done.item.final_at = Some(NOW - 1000);
        let mut line = line(&done);
        line.agent = Some(AgentTag {
            agent_id: "0123456789abcdef0123456789abcdef".into(),
            label: "Bill <b>".into(),
            plain: false,
        });
        fn h(
            lines: Vec<(Line<'_>, bool)>,
            filter: Option<Option<String>>,
            agent: Option<String>,
        ) -> History<'_> {
            History {
                lines,
                counts: vec![],
                total: 1,
                query: HistoryQuery {
                    agent,
                    ..HistoryQuery::default()
                },
                now_ms: NOW,
                all_accounts: false,
                truncated: false,
                limits_link: false,
                agents_link: true,
                agent_filter: filter,
            }
        }
        let page = history_page(&frame(), &h(vec![(line, false)], None, None), None);
        assert!(page.contains(r#"<a href="/auth/settings/inbox/items?agent=0123456789abcdef0123456789abcdef">Bill &lt;b&gt;</a>"#));
        assert!(page.contains("· ops-agent") && !page.contains("<b>"));
        assert!(page.contains("Cancelled: the owner blocked the agent."));
        assert!(!page.contains("histAgent"));
        // With the filter the page shows the chip, and a cross removes it. An agent that the viewer does not know has no name.
        let id = "0123456789abcdef0123456789abcdef".to_string();
        let page = history_page(
            &frame(),
            &h(vec![], Some(Some("Bill".into())), Some(id.clone())),
            None,
        );
        assert!(page.contains("histAgent") && page.contains("Agent <b>Bill</b>"));
        assert!(page.contains(r#"name="agent" value="0123456789abcdef0123456789abcdef""#));
        let page = history_page(&frame(), &h(vec![], Some(None), Some(id)), None);
        assert!(page.contains("Agent <b>unknown</b>"));
    }

    #[test]
    fn no_page_of_the_items_names_the_product() {
        let l = approval("K7QF", NOW - 60_000);
        let mut new = agent("g3", "ops-agent #4", AgentState::New);
        new.observed = Some(crate::inbox::agents::Observed {
            says: Some("billing-bot".into()),
            ..Default::default()
        });
        let notice = FirstUse {
            agent: new.clone(),
            open: 1,
            lock: true,
            lookalike: None,
            targets: vec![("g1".into(), "Billing bot".into())],
            csrf: "tok",
            typed: "",
            now_ms: NOW,
        };
        let mut locked = detail(&l, Body::Shown(text("SELECT 1")), Mode::Owner);
        locked.sender = Sender {
            lock: true,
            ..sender(new.clone())
        };
        let pages = [
            open_items(&panel(
                vec![
                    Group {
                        agent: new,
                        lines: vec![line(&l)],
                    },
                    Group {
                        agent: bucket(false),
                        lines: vec![line(&l)],
                    },
                ],
                vec![notice],
            )),
            detail_page(&frame(), &locked, None),
            detail_page(
                &frame(),
                &detail(&l, Body::Shown(text("SELECT 1")), Mode::Owner),
                None,
            ),
            limits_page(&frame(), &[], true),
        ];
        for page in pages {
            let body = page.to_lowercase();
            // The page has the sprite and the stylesheet links: only the text that a person reads is checked.
            let text = body.split("<main").nth(1).unwrap_or(&body);
            for word in ["portal", "inbox", "inbox", "circular"] {
                assert!(!text.contains(word), "{word}");
            }
        }
    }
}
