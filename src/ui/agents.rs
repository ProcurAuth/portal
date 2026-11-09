//! The pages of the agents in the tab "Inbox": the first-use notice, the agent page and the list of the agents (lot G3).
//!
//! An agent is one program, or one conversation of a model, that asks. Several agents can share one key. The server gives
//! each agent a token, and the owner gives it a name. The pages work without script. Each form is a POST with the token
//! `csrf`, and it is for the owner of the agent only.
//!
//! Two kinds of text are on these pages, and they never mix:
//!
//! - **Text of the server.** The name that the owner gave, else "<key name> #<number>". The key name, the owner, the state
//!   and the times. They go through [`html_escape`].
//! - **Text of the client.** What the agent says about itself, its client and its address range. They are not verified.
//!   They go through [`html::escape_untrusted`]: they are escaped, a hidden character has a visible mark, and a letter
//!   that looks like ASCII has a mark too. A box with a dashed edge and the label "Not verified" holds them. They are
//!   never the title of an agent.

use std::fmt::Write as _;

use super::components::icon;
use super::items::{ago, segmented, title_html, when, Line, View, ITEMS};
use super::markdown_html as html;
use super::settings::{fill_page, notice_html, Frame, Notice, Tab};
use crate::inbox::agents::{AgentEventRow, AgentKind, AgentState, Observed};
use crate::util::html_escape;

pub const AGENTS: &str = "/auth/settings/inbox/agents";
pub const RENAME: &str = "/auth/settings/inbox/agents/rename";
pub const ALLOW: &str = "/auth/settings/inbox/agents/allow";
pub const BLOCK: &str = "/auth/settings/inbox/agents/block";
pub const UNBLOCK: &str = "/auth/settings/inbox/agents/unblock";
pub const MERGE: &str = "/auth/settings/inbox/agents/merge";
pub const ENROLL: &str = "/auth/settings/inbox/agents/enroll";
pub const RESET: &str = "/auth/settings/inbox/agents/reset";

/// The text of the outcome of a form of the agents, from a fixed list.
pub fn notice_for(code: &str) -> Option<Notice> {
    let (text, ok) = match code {
        "agent_allowed" => ("The agent is allowed.", true),
        "agent_renamed" => ("The name is saved.", true),
        "agent_blocked" => (
            "The agent is blocked. Its open requests are cancelled. Its calls are refused.",
            true,
        ),
        "agent_unblocked" => (
            "The agent is not blocked now. The requests that were cancelled stay cancelled.",
            true,
        ),
        "agent_merged" => (
            "The agents are merged. The requests and the token of the other agent now belong to this agent.",
            true,
        ),
        "agent_gone" => ("That agent does not exist.", false),
        "agent_bad_name" => (
            "A name has 1 to 32 characters on one line. It has no control character, no hidden character and no space at its ends.",
            false,
        ),
        "agent_name_taken" => (
            "Another agent of this key has this name. A name is unique inside a key. A difference of case or of form does not count.",
            false,
        ),
        "agent_bad_state" => (
            "The state of this agent does not allow this action.",
            false,
        ),
        "agent_cross_key" => ("You can merge only agents of the same key.", false),
        "agent_no_target" => ("Choose the agent to merge into.", false),
        "agent_no_key" => (
            "Choose a key in the mode Enrolled. A key that you own and that is in another mode cannot enroll an agent.",
            false,
        ),
        "agent_cap" => (
            "This key has the most agents that the file allows. Merge or block an agent, or ask the operator to raise the limit.",
            false,
        ),
        _ => return None,
    };
    Some(Notice { text, ok })
}

// What a screen shows of an agent ------------------------------------------------------------------

/// What the screens show of an agent. `label` and `key_name` are text of the server. `observed` is text of the client.
#[derive(Clone, Debug)]
pub struct AgentView {
    pub agent_id: String,
    /// The name of the owner, else "<key name> #<number>". For a bucket: the key name, or "Unattributed · <key name>".
    pub label: String,
    pub key_name: String,
    pub kind: AgentKind,
    pub state: AgentState,
    /// The owner gave a name.
    pub named: bool,
    pub created_at: i64,
    pub last_seen_at: i64,
    /// What the agent said about itself and what its connection showed. Not verified.
    pub observed: Option<Observed>,
    /// The bucket of a key that has no agent: the key is the one agent. The group has the name of the key, no key chip
    /// and no pill, as the panel had before the agents.
    pub plain: bool,
}

impl AgentView {
    pub fn is_bucket(&self) -> bool {
        self.kind == AgentKind::Bucket
    }

    /// A `new` agent with `inbox.agents.lock_new`: the server refuses `accept` for its items.
    pub fn locked(&self, lock_new: bool) -> bool {
        lock_new && self.kind == AgentKind::Agent && self.state == AgentState::New
    }

    pub fn href(&self) -> String {
        format!("{AGENTS}/{}", self.agent_id)
    }
}

/// The chip of a key. The key name is text of the owner: it is escaped and marked.
pub fn key_chip(name: &str) -> String {
    format!(
        r#"<span class="keychip" title="Key">{}{}</span>"#,
        icon("key"),
        html::escape(name)
    )
}

/// The state of an agent as a pill. A known agent has no pill in a group header: the name is enough.
pub fn state_pill(a: &AgentView, with_known: bool) -> String {
    match (a.kind, a.state) {
        (AgentKind::Bucket, _) if a.plain => String::new(),
        (AgentKind::Bucket, _) => r#"<span class="pill pill--un">Unattributed</span>"#.to_string(),
        (AgentKind::Agent, AgentState::New) if !a.named => {
            r#"<span class="pill pill--new">New, not named</span>"#.to_string()
        }
        (AgentKind::Agent, AgentState::New) => {
            r#"<span class="pill pill--new">New</span>"#.to_string()
        }
        (AgentKind::Agent, AgentState::Known) if with_known => {
            r#"<span class="pill pill--ok">Known</span>"#.to_string()
        }
        (AgentKind::Agent, AgentState::Known) => String::new(),
        (AgentKind::Agent, AgentState::Blocked) => {
            r#"<span class="pill pill--bad">Blocked</span>"#.to_string()
        }
        (AgentKind::Agent, AgentState::Merged) => r#"<span class="pill">Merged</span>"#.to_string(),
    }
}

/// The line "Observed, not verified" under the header of a group. All its values are not verified.
pub fn hints_line(a: &AgentView, now_ms: i64) -> String {
    if a.plain {
        return String::new();
    }
    if a.is_bucket() {
        return r#"<div class="agrp__hints"><span class="tag">Which agent asked is not known.</span><span>The agent sent the request without the agent channel, or it passed no agent token.</span></div>"#.to_string();
    }
    let mut out =
        String::from(r#"<div class="agrp__hints"><span class="tag">Observed, not verified</span>"#);
    if let Some(o) = &a.observed {
        if let Some(says) = &o.says {
            let _ = write!(
                out,
                "<span>Says it is <q>{}</q></span>",
                html::escape_untrusted(says)
            );
        }
        if let Some(client) = o.client.as_ref().or(o.user_agent.as_ref()) {
            let _ = write!(
                out,
                "<span>Client {}</span>",
                html::escape_untrusted(client)
            );
        }
        if let Some(net) = &o.net {
            let _ = write!(out, "<span>From {}</span>", html::escape_untrusted(net));
        }
    }
    let _ = write!(
        out,
        "<span>First seen {}</span><span>Last call {}</span></div>",
        when(a.created_at),
        ago(now_ms, a.last_seen_at)
    );
    out
}

/// A known agent whose name is the text that a new agent says about itself.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Lookalike {
    /// The name of the known agent: text of the owner.
    pub label: String,
    pub same_client: bool,
    pub same_net: bool,
}

impl Lookalike {
    /// The warning of the first-use notice, as one text with the name in bold.
    fn warning(&self, says: &str) -> String {
        let also = match (self.same_client, self.same_net) {
            (true, true) => " Same client and same address range too.",
            (true, false) => " Same client too.",
            (false, true) => " Same address range too.",
            (false, false) => "",
        };
        format!(
            "The text <q>{}</q> is the same as the name of your agent <b>{}</b>.{also} This can be that agent after it lost its token. It can also be a copy. The server cannot tell.",
            html::escape_untrusted(says),
            html::escape(&self.label)
        )
    }
}

/// A known agent that a new agent looks like (lot G5): the hint "similar". It is help for the owner. The portal never
/// merges by itself, and the hints can be copied.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Similar {
    pub agent_id: String,
    /// The name of the known agent: text of the server.
    pub label: String,
    /// Why: it says the same text, or it has the same client and the same address range.
    pub why: crate::inbox::agents::SimilarWhy,
}

impl Similar {
    /// The line of the first-use notice. The name is text of the server, so it is escaped as such.
    fn line(&self) -> String {
        use crate::inbox::agents::SimilarWhy;
        let why = match self.why {
            SimilarWhy::Says => "it says the same text",
            SimilarWhy::ClientAndNet => "it has the same client and the same address range",
        };
        format!(
            r#"<p class="note similar">Looks like your agent <a href="{AGENTS}/{}">{}</a>: {why}. Hints can be copied. Nothing merges by itself.</p>"#,
            html_escape(&self.agent_id),
            html::escape(&self.label)
        )
    }
}

// The first-use notice -----------------------------------------------------------------------------

/// The notice for one `new` agent: what is known of it, and the forms to name it, allow it, block it or merge it.
pub struct FirstUse<'a> {
    pub agent: AgentView,
    pub open: usize,
    /// `inbox.agents.lock_new`: the server refuses `accept` for the items of this agent until the owner allows it.
    pub lock: bool,
    pub lookalike: Option<Lookalike>,
    /// A known agent of the same key that this agent looks like: a hint, never a merge.
    pub similar: Option<Similar>,
    /// The known agents of the same key: the choices of the merge. Empty: the merge form is absent.
    pub targets: Vec<(String, String)>,
    pub csrf: &'a str,
    /// The name that the owner typed in a form that the server refused.
    pub typed: &'a str,
    pub now_ms: i64,
}

fn hidden(csrf: &str, agent_id: &str) -> String {
    format!(
        r#"<input type="hidden" name="csrf" value="{}"><input type="hidden" name="agent_id" value="{}">"#,
        html_escape(csrf),
        html_escape(agent_id)
    )
}

fn short(id: &str) -> String {
    id.chars().take(8).collect()
}

/// One notice. The agent id gives unique ids to the elements, so a page can hold several notices.
pub fn first_use(n: &FirstUse) -> String {
    let a = &n.agent;
    let sid = short(&a.agent_id);
    let o = a.observed.as_ref();
    let mut facts = String::new();
    let mut fact = |k: &str, v: String| {
        let _ = write!(facts, "<div><dt>{k}</dt><dd>{v}</dd></div>");
    };
    if let Some(says) = o.and_then(|o| o.says.as_ref()) {
        fact(
            "Says it is",
            format!("<q>{}</q>", html::escape_untrusted(says)),
        );
    }
    if let Some(client) = o.and_then(|o| o.client.as_ref().or(o.user_agent.as_ref())) {
        fact("Client", html::escape_untrusted(client));
    }
    if let Some(net) = o.and_then(|o| o.net.as_ref()) {
        fact("From address", html::escape_untrusted(net));
    }
    fact("First call", when(a.created_at));
    let waits = match (n.open, n.lock) {
        (0, _) => "None now.".to_string(),
        (c, true) => format!("{c} (below). Approve is locked."),
        (c, false) => format!("{c} (below)."),
    };
    fact("Open requests", waits);
    let warn = match (&n.lookalike, o.and_then(|o| o.says.as_ref())) {
        (Some(l), Some(says)) => format!(
            r#"<p class="warnline">{} {}</p>"#,
            icon("alert-triangle"),
            l.warning(says)
        ),
        _ => String::new(),
    };
    let warn = format!(
        "{warn}{}",
        n.similar.as_ref().map(Similar::line).unwrap_or_default()
    );
    let merge = if n.targets.is_empty() {
        String::new()
    } else {
        let mut options = String::new();
        for (id, label) in &n.targets {
            let _ = write!(
                options,
                r#"<option value="{}">{}</option>"#,
                html_escape(id),
                html::escape(label)
            );
        }
        format!(
            r#"<div class="acts__rule"></div><form method="post" action="{MERGE}">{}<div class="field"><label class="field__label" for="mg-{sid}">It is the same agent as one that you know</label><div class="row"><select class="uiInput" id="mg-{sid}" name="into">{options}</select><button class="button" type="submit">{}Merge into it</button></div><span class="note">The requests move to that agent. The token of this agent then works as the token of the other.</span></div></form>"#,
            hidden(n.csrf, &a.agent_id),
            icon("git-merge")
        )
    };
    let lead = if n.lock {
        "Name it, allow it or block it. Approve stays locked until you allow it."
    } else {
        "Name it, allow it or block it."
    };
    format!(
        r#"<section class="firstuse" aria-labelledby="fu-{sid}"><div class="firstuse__head">{}<h3 id="fu-{sid}">A new agent is using the key {}</h3><span class="grow"></span><span class="muted">{}</span></div><div class="firstuse__body"><div class="obs"><h4>{} Observed, not verified</h4><dl>{facts}</dl>{warn}<p class="note">Nothing here proves who the agent is. {lead}</p></div><div class="acts"><form method="post" action="{ALLOW}">{}<div class="field"><label class="field__label" for="nm-{sid}">Name for this agent</label><div class="row"><input class="uiInput" id="nm-{sid}" name="name" maxlength="32" required placeholder="For example: Refund cleaner" autocomplete="off" value="{}"><button class="button button--primary" type="submit">Name and allow</button></div></div></form><div class="row"><form class="inline" method="post" action="{ALLOW}">{}<button class="button" type="submit">Allow as &quot;{}&quot;</button></form><form class="inline" method="post" action="{BLOCK}">{}<button class="button button--danger" type="submit">{}Block</button></form></div>{merge}</div></div></section>"#,
        icon("alert-triangle"),
        key_chip(&a.key_name),
        ago(n.now_ms, a.created_at),
        icon("eye"),
        hidden(n.csrf, &a.agent_id),
        html::escape_attr(n.typed),
        hidden(n.csrf, &a.agent_id),
        html::escape_attr(&a.label),
        hidden(n.csrf, &a.agent_id),
        icon("ban"),
    )
}

// The agent page -----------------------------------------------------------------------------------

/// The counts of the agents of one key, for the box "Key".
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct KeyCounts {
    pub known: usize,
    pub new: usize,
    pub blocked: usize,
    /// `inbox.agents.max_per_key`.
    pub max: Option<u32>,
}

impl KeyCounts {
    fn total(&self) -> usize {
        self.known + self.new + self.blocked
    }
}

pub struct AgentPage<'a> {
    pub agent: AgentView,
    pub csrf: &'a str,
    pub lock: bool,
    pub now_ms: i64,
    /// The open requests of the agent, with their titles.
    pub open: Vec<Line<'a>>,
    /// The requests that the owner answered.
    pub answered: usize,
    pub events: Vec<AgentEventRow>,
    /// The login that named the agent, and when.
    pub named_by: Option<(String, i64)>,
    /// The agents of the same key that can take a merge: `(id, label, state)`.
    pub targets: Vec<(String, String, AgentState)>,
    pub counts: KeyCounts,
    /// With the state `merged`: the agent that took its items.
    pub merged_into: Option<(String, String)>,
    /// The name that the owner typed in a form that the server refused.
    pub typed: Option<&'a str>,
    /// The owner made this agent and the key is in the mode `enrolled`: the box "Reset token" shows.
    pub can_reset: bool,
}

fn crumbs(last: &str) -> String {
    format!(
        r#"<nav class="crumbs" aria-label="Path"><a href="{}">{}Inbox</a><span>/</span><a href="{AGENTS}">Agents</a><span>/</span><b>{last}</b></nav>"#,
        super::inbox::PAGE,
        icon("arrow-left")
    )
}

fn event_text(e: &AgentEventRow) -> String {
    let who = html_escape(&e.actor);
    let number = |key: &str| -> Option<u64> {
        e.detail
            .as_deref()?
            .split(' ')
            .find_map(|part| part.strip_prefix(key)?.strip_prefix('=')?.parse().ok())
    };
    let plural = |n: u64, one: &str, many: &str| format!("{n} {}", if n == 1 { one } else { many });
    match e.event.as_str() {
        "created" => "Made by a first call of the key".to_string(),
        "enrolled" => format!("Added by {who}. The token was shown one time."),
        "token_reset" => format!("Token reset by {who}. The old token stopped at once."),
        "allowed" => format!("Allowed by {who}"),
        "named" => format!("Named by {who}"),
        "renamed" => format!("Renamed by {who}"),
        "blocked" => {
            let cancelled = number("cancelled").unwrap_or(0);
            format!(
                "Blocked by {who}. {} cancelled.",
                plural(cancelled, "open request", "open requests")
            )
        }
        "unblocked" => format!("Unblocked by {who}"),
        "merged_in" => format!(
            "Another agent was merged into this agent by {who}. {} moved.",
            plural(number("items").unwrap_or(0), "request", "requests")
        ),
        "merged_into" => format!(
            "Merged into another agent by {who}. {} moved.",
            plural(number("items").unwrap_or(0), "request", "requests")
        ),
        "says_changed" => "The text that the agent says about itself changed".to_string(),
        "net_changed" => "A call came from a new address range".to_string(),
        "refused_cap" => {
            "The key has the most agents that it can have. A call went to Unattributed.".to_string()
        }
        "refused_rate" => {
            "The key made too many agents in one hour. A call went to Unattributed.".to_string()
        }
        other => html_escape(other),
    }
}

fn open_table(p: &AgentPage) -> String {
    let body = if p.open.is_empty() {
        r#"<p class="exp__none">No request waits for this agent.</p>"#.to_string()
    } else {
        let mut rows = String::new();
        for line in &p.open {
            let l = line.loaded;
            let code = super::items::code_of(l);
            let href = super::items::detail_href(l);
            let _ = write!(
                rows,
                r#"<tr><td><span class="code">{}</span></td><td><a href="{}">{}</a></td><td class="muted">{}</td><td><a class="button button--small" href="{}">Review</a></td></tr>"#,
                html_escape(&code),
                html_escape(&href),
                title_html(&line.title, &code),
                ago(p.now_ms, l.item.created_at),
                html_escape(&href),
            );
        }
        format!(
            r#"<table class="agTbl"><thead><tr><th>Code</th><th>Request</th><th>Asked</th><th></th></tr></thead><tbody>{rows}</tbody></table>"#
        )
    };
    format!(
        r#"<section class="sec2" aria-labelledby="ao-t"><div class="sec2__head"><h2 class="title" id="ao-t">Open requests</h2><span class="count">{}</span></div>{body}</section>"#,
        p.open.len()
    )
}

fn observed_section(p: &AgentPage) -> String {
    let a = &p.agent;
    if a.is_bucket() {
        return String::new();
    }
    let mut facts = String::new();
    let mut fact = |k: &str, v: String| {
        let _ = write!(facts, "<div><dt>{k}</dt><dd>{v}</dd></div>");
    };
    let none = r#"<span class="muted">nothing yet</span>"#.to_string();
    match a.observed.as_ref() {
        Some(o) => {
            let says = match (&o.says, &o.says_first) {
                (Some(now), first) => {
                    let was = match first {
                        Some(f) if f != now && o.says_changes > 0 => format!(
                            r#" <span class="muted">(first: &quot;{}&quot;, changed {} time{})</span>"#,
                            html::escape_untrusted(f),
                            o.says_changes,
                            if o.says_changes == 1 { "" } else { "s" }
                        ),
                        _ => String::new(),
                    };
                    format!("<i>&quot;{}&quot;</i>{was}", html::escape_untrusted(now))
                }
                _ => none.clone(),
            };
            fact("Says it is", says);
            fact(
                "Client",
                o.client
                    .as_ref()
                    .map_or(none.clone(), |c| html::escape_untrusted(c)),
            );
            fact(
                "User agent",
                o.user_agent.as_ref().map_or(none.clone(), |u| {
                    format!("<code>{}</code>", html::escape_untrusted(u))
                }),
            );
            fact(
                "From address",
                o.net
                    .as_ref()
                    .map_or(none.clone(), |n| html::escape_untrusted(n)),
            );
            fact("Address ranges seen", o.nets_seen().to_string());
        }
        None => {
            for k in ["Says it is", "Client", "User agent", "From address"] {
                fact(k, none.clone());
            }
        }
    }
    fact("First seen", when(a.created_at));
    format!(
        r#"<section class="sec2 sec2--exp" aria-labelledby="ob-t"><div class="sec2__head"><h2 class="title" id="ob-t">Observed, not verified</h2><span class="trust trust--no">{}Not checked</span><span class="sec2__note">From the agent or from its connection. It can be wrong or copied.</span></div><dl class="sec2__facts agFacts">{facts}</dl><p class="exp__fixed">Anyone who holds the key can send these texts. They are no proof. The name in the title is the name that you gave. The server never takes a name from the agent.</p></section>"#,
        icon("alert-triangle")
    )
}

fn events_section(p: &AgentPage) -> String {
    let mut items = String::new();
    for (n, e) in p.events.iter().enumerate() {
        let _ = write!(
            items,
            r#"<li{}><b>{}</b> {}</li>"#,
            if n == 0 { r#" class="is-now""# } else { "" },
            when(e.at),
            event_text(e)
        );
    }
    let body = if p.events.is_empty() {
        r#"<p class="exp__none">No event.</p>"#.to_string()
    } else {
        format!(r#"<div class="agEvents"><ul class="timeline">{items}</ul></div>"#)
    };
    format!(
        r#"<section class="sec2" aria-labelledby="ev-t"><div class="sec2__head"><h2 class="title" id="ev-t">Events</h2></div>{body}</section>"#
    )
}

fn side_forms(p: &AgentPage) -> String {
    let a = &p.agent;
    let mut out = String::new();
    let fields = hidden(p.csrf, &a.agent_id);
    let live = a.kind == AgentKind::Agent && matches!(a.state, AgentState::New | AgentState::Known);
    if live && a.state == AgentState::New {
        let _ = write!(
            out,
            r#"<div class="sidebox"><h3>Allow</h3><form class="acts" method="post" action="{ALLOW}">{fields}<div class="row"><input class="uiInput" name="name" maxlength="32" placeholder="Name (optional)" aria-label="Name" autocomplete="off"><button class="button button--primary" type="submit">Allow</button></div><p>{}</p></form></div>"#,
            if p.lock {
                "Approve is locked until you allow this agent. A name is optional."
            } else {
                "The agent is new. Allow it when you know it. A name is optional."
            }
        );
    }
    if live {
        let typed = p
            .typed
            .map(str::to_string)
            .or_else(|| a.named.then(|| a.label.clone()))
            .unwrap_or_default();
        let _ = write!(
            out,
            r#"<div class="sidebox"><h3>Rename</h3><form class="acts" method="post" action="{RENAME}">{fields}<div class="row"><input class="uiInput" name="name" value="{}" maxlength="32" required aria-label="Name" autocomplete="off"><button class="button" type="submit">{}Save</button></div><p>1 to 32 characters. The name is unique inside the key.</p></form></div>"#,
            html::escape_attr(&typed),
            icon("pencil"),
        );
        if !p.targets.is_empty() {
            let mut options = String::new();
            for (id, label, state) in &p.targets {
                let _ = write!(
                    options,
                    r#"<option value="{}">{}{}</option>"#,
                    html_escape(id),
                    html::escape(label),
                    if *state == AgentState::New {
                        " (new)"
                    } else {
                        ""
                    }
                );
            }
            let _ = write!(
                out,
                r#"<div class="sidebox"><h3>Merge</h3><form class="acts" method="post" action="{MERGE}">{fields}<div class="row"><select class="uiInput" name="into" aria-label="Merge into">{options}</select><button class="button" type="submit">{}Merge into it</button></div><p>Only agents of the same key. This agent is removed. Its requests and its token go to the other agent.</p></form></div>"#,
                icon("git-merge")
            );
        }
        if p.can_reset {
            let _ = write!(
                out,
                r#"<div class="sidebox"><h3>Token</h3><form class="acts" method="post" action="{RESET}">{fields}<p>The server keeps only a digest of the token, so it cannot show the token again. If the agent lost it, make a new one. <b>The old token stops at once.</b> The agent cannot call until you give it the new token.</p><div class="row"><button class="button" type="submit">{}Reset token</button></div></form></div>"#,
                icon("key")
            );
        }
        let cancel = match p.open.len() {
            0 => "No request waits now.".to_string(),
            1 => "The 1 open request is cancelled.".to_string(),
            n => format!("The {n} open requests are cancelled."),
        };
        let _ = write!(
            out,
            r#"<div class="sidebox danger-zone"><h3>Block</h3><form class="acts" method="post" action="{BLOCK}">{fields}<p>{cancel} Each later call with its token is refused with <code>agent_blocked</code>. You can unblock it.</p><div class="row"><button class="button button--danger" type="submit">{}Block this agent</button></div></form></div>"#,
            icon("ban")
        );
    }
    if a.kind == AgentKind::Agent && a.state == AgentState::Blocked {
        let _ = write!(
            out,
            r#"<div class="sidebox"><h3>Unblock</h3><form class="acts" method="post" action="{UNBLOCK}">{fields}<p>Each call with the token of this agent is refused. Unblock lets it call again. The requests that were cancelled stay cancelled.</p><div class="row"><button class="button" type="submit">{}Unblock this agent</button></div></form></div>"#,
            icon("lock-open")
        );
    }
    if a.kind == AgentKind::Agent && a.state == AgentState::Merged {
        let to = p.merged_into.as_ref().map_or(String::new(), |(id, label)| {
            format!(
                r#" <a href="{AGENTS}/{}">{}</a>"#,
                html_escape(id),
                html::escape(label)
            )
        });
        let _ = write!(
            out,
            r#"<div class="sidebox"><h3>Merged</h3><p>This agent is merged into{to}. Its requests and its token belong to that agent. You cannot change this agent.</p></div>"#
        );
    }
    if a.is_bucket() {
        out.push_str(r#"<div class="sidebox"><h3>Unattributed</h3><p>This is not an agent. It holds the requests that no agent claims. You cannot name it, merge it or block it.</p></div>"#);
    }
    out
}

fn key_box(p: &AgentPage) -> String {
    let c = &p.counts;
    let mut parts: Vec<String> = Vec::new();
    for (n, w) in [(c.known, "known"), (c.new, "new"), (c.blocked, "blocked")] {
        if n > 0 {
            parts.push(format!("{n} {w}"));
        }
    }
    let agents = format!("{} ({})", c.total(), parts.join(", "));
    let limit = c.max.map_or(String::new(), |m| {
        format!(r#"<div><dt>Limit</dt><dd>{m} agents</dd></div>"#)
    });
    format!(
        r#"<div class="sidebox"><h3>Key</h3><dl class="facts"><div><dt>Key</dt><dd>{}</dd></div><div><dt>Agents on it</dt><dd>{}</dd></div>{limit}</dl></div>"#,
        html::escape(&p.agent.key_name),
        if c.total() == 0 {
            "0".to_string()
        } else {
            agents
        }
    )
}

/// The page of one agent.
pub fn agent_page(frame: &Frame, p: &AgentPage, notice: Option<Notice>) -> String {
    let a = &p.agent;
    let mut out = String::new();
    notice_html(&mut out, notice);
    let name = html::escape(&a.label);
    let kind = if a.is_bucket() {
        format!(
            r#"<span class="kind">{}Unattributed</span>"#,
            icon("help-circle")
        )
    } else {
        format!(
            r#"<span class="kind kind--approval">{}Agent</span>"#,
            icon("robot").replace(r#"class="icon""#, r#"class="icon icon--sm""#)
        )
    };
    let mut row = String::new();
    if let Some((login, at)) = &p.named_by {
        let _ = write!(
            row,
            "<span>Named by <b>{}</b> on {}</span>",
            html_escape(login),
            when(*at)
        );
    }
    let _ = write!(
        row,
        r#"<span class="lease">Last call <b>{}</b></span><span class="lease">Open requests <b>{}</b></span><span class="lease">Answered <b>{}</b></span><a href="{ITEMS}?agent={}">History of this agent</a>"#,
        ago(p.now_ms, a.last_seen_at),
        p.open.len(),
        p.answered,
        html_escape(&a.agent_id)
    );
    let head = format!(
        r#"<div class="head"><div class="chipStack">{kind}{}{}</div><h2>{name}</h2><div class="head__row">{row}</div></div>"#,
        state_pill(a, true),
        key_chip(&a.key_name),
    );
    let _ = write!(
        out,
        r#"<div class="page page--narrow">{}<div class="detail"><div class="detail__main">{head}{}{}{}</div><div class="detail__side">{}{}</div></div></div>"#,
        crumbs(&name),
        open_table(p),
        observed_section(p),
        events_section(p),
        side_forms(p),
        key_box(p),
    );
    fill_page(
        Tab::Inbox,
        frame,
        &format!("Inbox: agent {}", a.label),
        &out,
    )
}

// The list of the agents ---------------------------------------------------------------------------

pub struct ListRow {
    pub agent: AgentView,
    pub open: usize,
    /// No open item and no call for `inbox.agents.idle_after`.
    pub idle: bool,
}

pub struct AgentsList {
    pub rows: Vec<ListRow>,
    pub now_ms: i64,
    /// The form "Add agent": present when the owner has a key in the mode `enrolled`.
    pub enroll: Option<EnrollForm>,
}

/// The form "Add agent" of the list (lot G6). The owner makes an agent for a key in the mode `enrolled`. The server then shows
/// the token one time.
pub struct EnrollForm {
    pub csrf: String,
    /// The keys of the owner in the mode `enrolled`: `(key id, key name)`.
    pub keys: Vec<(String, String)>,
    /// What the owner typed in a form that the server refused.
    pub typed_key: String,
    pub typed_name: String,
}

fn enroll_form(f: &EnrollForm) -> String {
    let mut options = String::new();
    for (id, name) in &f.keys {
        let _ = write!(
            options,
            r#"<option value="{}"{}>{}</option>"#,
            html_escape(id),
            if *id == f.typed_key { " selected" } else { "" },
            html::escape(name)
        );
    }
    format!(
        r#"<section class="sidebox enrollBox" aria-labelledby="en-t"><h3 id="en-t">Add agent</h3><form class="acts" method="post" action="{ENROLL}"><input type="hidden" name="csrf" value="{}"><div class="field"><label class="field__label" for="en-key">Key</label><select class="uiInput" id="en-key" name="key" required>{options}</select></div><div class="field"><label class="field__label" for="en-name">Name of the agent</label><input class="uiInput" id="en-name" name="name" maxlength="32" required autocomplete="off" placeholder="For example: Refund cleaner" value="{}"></div><div class="row"><button class="button button--primary" type="submit">{}Add agent</button></div><p>The server gives a token for this agent and shows it one time. Put the token in the setup of the agent. A call of this key with no token, or with another token, is refused.</p></form></section>"#,
        html_escape(&f.csrf),
        html::escape_attr(&f.typed_name),
        icon("plus"),
    )
}

/// The page that lists all the agents of the owner, new ones first. It is the way to a blocked or idle agent.
pub fn agents_page(frame: &Frame, l: &AgentsList, notice: Option<Notice>) -> String {
    let mut out = String::new();
    notice_html(&mut out, notice);
    let mut rows: Vec<&ListRow> = l.rows.iter().collect();
    let rank = |r: &ListRow| match (r.agent.kind, r.agent.state) {
        (AgentKind::Bucket, _) => 4,
        (_, AgentState::New) => 0,
        (_, AgentState::Known) => 1,
        (_, AgentState::Blocked) => 2,
        (_, AgentState::Merged) => 3,
    };
    rows.sort_by(|a, b| {
        (&a.agent.key_name, rank(a), &a.agent.label).cmp(&(
            &b.agent.key_name,
            rank(b),
            &b.agent.label,
        ))
    });
    let mut body = String::new();
    for r in &rows {
        let a = &r.agent;
        let idle = if r.idle {
            r#" <span class="muted">idle</span>"#
        } else {
            ""
        };
        let pill = match state_pill(a, true) {
            p if p.is_empty() => r#"<span class="pill pill--ok">Known</span>"#.to_string(),
            p => p,
        };
        let _ = write!(
            body,
            r#"<tr><td><a href="{}">{}</a> {}</td><td>{pill}</td><td>{}</td><td class="muted">{}{idle}</td></tr>"#,
            html_escape(&a.href()),
            html::escape(&a.label),
            key_chip(&a.key_name),
            r.open,
            ago(l.now_ms, a.last_seen_at),
        );
    }
    let table = if rows.is_empty() {
        r#"<div class="uiState uiState--left"><p class="uiState__body">No agent yet. An agent shows here after its first request, or after you add it.</p></div>"#.to_string()
    } else {
        format!(
            r#"<div class="histWrap"><table class="agTbl agTbl--list"><thead><tr><th>Agent</th><th>State</th><th>Open</th><th>Last call</th></tr></thead><tbody>{body}</tbody></table></div>"#
        )
    };
    let add = l.enroll.as_ref().map(enroll_form).unwrap_or_default();
    let _ = write!(
        out,
        r#"<div class="page"><section class="zone" aria-labelledby="ag-t"><div class="zone__head"><h2 class="title" id="ag-t">Agents</h2><span class="count">{}</span><span class="grow"></span>{}</div><div class="scroll">{table}</div></section>{add}<p class="note">The names are yours. An agent that the server does not know gets the name of its key and a number. The text that an agent says about itself is not verified: it is on the page of the agent.</p></div>"#,
        rows.len(),
        segmented(View::Agents, true),
    );
    fill_page(Tab::Inbox, frame, "Inbox: agents", &out)
}

// The token, one time ----------------------------------------------------------------------------

/// The page that shows a token one time: after "Add agent" and after "Reset token" (lot G6). The server keeps only a digest of
/// the token, so no other page can show it again.
pub struct TokenPage<'a> {
    /// The name of the agent: text of the server.
    pub label: &'a str,
    pub key_name: &'a str,
    pub agent_id: &'a str,
    pub token: &'a str,
    /// True after "Reset token": the old token is dead.
    pub reset: bool,
}

/// The header line that an MCP client sends: the owner pastes it in the setup of the client.
pub fn header_line(token: &str) -> String {
    format!("Agent-Token: {token}")
}

fn copy_row(id: &str, label: &str, value: &str) -> String {
    format!(
        r##"<div class="field"><label class="field__label" for="{id}">{label}</label><div class="keyRow"><input class="uiInput uiInput--mono keyText" id="{id}" type="text" readonly value="{}" spellcheck="false" autocomplete="off"><button class="button copyButton" type="button" data-copy="{id}"><svg class="icon icon--sm copyButton__idle" aria-hidden="true"><use href="#i-copy"/></svg><svg class="icon icon--sm copyButton__done" aria-hidden="true"><use href="#i-check"/></svg>Copy</button></div></div>"##,
        html_escape(value)
    )
}

/// The page of the token. It is an answer to a POST and it has `Cache-Control: no-store`. It has no link to another site.
pub fn token_page(frame: &Frame, t: &TokenPage) -> String {
    let name = html::escape(t.label);
    let lead = if t.reset {
        format!(
            "The token of <strong>{name}</strong> is new. The old token stopped at once. Copy the new token now. The server shows it one time and keeps only a digest."
        )
    } else {
        format!(
            "The agent <strong>{name}</strong> is made for the key {}. Copy its token now. The server shows it one time and keeps only a digest.",
            key_chip(t.key_name)
        )
    };
    let id = html_escape(t.agent_id);
    let body = format!(
        r#"<div class="page page--narrow">{}<div class="head"><h2>{}</h2></div><p class="uiBanner uiBanner--inset uiBanner--ok" role="status"><span class="uiBanner__text">{lead}</span></p>{}{}<div class="sidebox"><h3>How the agent sends the token</h3><p>Best: put the header line above in the setup of the MCP client. Then the model never has to copy the token.</p><p>If the client cannot set a header, the model gives the token in the argument <code>agent_token</code> of each tool.</p><p>A token in a query string is refused. Each call that makes a request needs the token. A call with no token, or with a wrong token, is refused.</p></div><p class="uiBanner uiBanner--inset uiBanner--warn"><span class="uiBanner__text">Do not put the token in a text that other people read. The server refuses an explanation that holds a live token.</span></p><div class="row"><a class="button button--primary" href="{AGENTS}/{id}">Done</a></div></div>"#,
        crumbs(&name),
        if t.reset { "New token" } else { "Agent made" },
        copy_row("agent-token", "Token", t.token),
        copy_row("agent-header", "Header line", &header_line(t.token)),
    );
    fill_page(Tab::Inbox, frame, "Inbox: agent token", &body)
}

/// What the page of one request shows of the sender.
pub struct Sender {
    pub agent: AgentView,
    pub lookalike: Option<Lookalike>,
    /// The viewer is the owner: the box shows the text of the agent and links to its page.
    pub owner: bool,
    pub lock: bool,
    /// The other agents that asked for the same effect and got this request (the event `also_requested`): their names, as the
    /// server writes them. It is never text of an agent. Empty for a viewer that is not the owner.
    pub also: Vec<AlsoBy>,
}

/// An agent that asked for the effect of a request that was open already.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AlsoBy {
    pub agent_id: String,
    /// The name of the owner, else "<key name> #<number>": text of the server.
    pub label: String,
}

/// The line "Also asked by": the other agents that asked for the same effect. The names are text of the server.
fn also_line(also: &[AlsoBy]) -> String {
    if also.is_empty() {
        return String::new();
    }
    let links: Vec<String> = also
        .iter()
        .map(|a| {
            format!(
                r#"<a href="{AGENTS}/{}">{}</a>"#,
                html_escape(&a.agent_id),
                html::escape(&a.label)
            )
        })
        .collect();
    format!(
        r#"<div class="sender__also"><span class="sender__label">Also asked by</span><span>{}</span><span class="muted">They asked for the same effect. The server gave them this request.</span></div>"#,
        links.join(", ")
    )
}

impl Sender {
    /// The approval of this agent is locked: the server refuses `accept`.
    pub fn locked(&self) -> bool {
        self.owner && self.agent.locked(self.lock)
    }
}

/// The box "From agent" before the command. The name, the key and the state are text of the server. The dashed box holds
/// what the agent said: it is not verified.
pub fn sender_box(s: &Sender, owner_login: &str) -> String {
    let a = &s.agent;
    let label = if a.plain { "From key" } else { "From agent" };
    let link = match (s.owner, a.is_bucket(), a.state) {
        (true, false, AgentState::New) => {
            format!(r#" · <a href="{}">Name it</a>"#, html_escape(&a.href()))
        }
        (true, false, _) => format!(r#" · <a href="{}">Agent page</a>"#, html_escape(&a.href())),
        _ => String::new(),
    };
    let chip = if a.plain {
        String::new()
    } else {
        key_chip(&a.key_name)
    };
    let main = format!(
        r#"<div class="sender__main"><span class="sender__label">{label}</span><span class="sender__name">{}</span>{chip}{}<span class="sender__of">of <b>{}</b>{link}</span></div>"#,
        html::escape(&a.label),
        state_pill(a, true),
        html_escape(owner_login),
    );
    let says = if a.plain || !s.owner {
        String::new()
    } else if a.is_bucket() {
        r#"<div class="says"><span class="tag">Which agent asked is not known.</span><span>No agent token came with this request.</span></div>"#.to_string()
    } else {
        let o = a.observed.as_ref();
        let mut line = format!(
            r#"<span class="tag">{} Not verified</span>"#,
            icon("alert-triangle")
        );
        match o.and_then(|o| o.says.as_ref()) {
            Some(says) => {
                let _ = write!(
                    line,
                    "<span>Says it is: <q>{}</q></span>",
                    html::escape_untrusted(says)
                );
            }
            None => line.push_str("<span>The agent said nothing about itself.</span>"),
        }
        let mut extra = Vec::new();
        if let Some(c) = o.and_then(|o| o.client.as_ref()) {
            extra.push(format!("client {}", html::escape_untrusted(c)));
        }
        if let Some(n) = o.and_then(|o| o.net.as_ref()) {
            extra.push(html::escape_untrusted(n));
        }
        if !extra.is_empty() {
            let _ = write!(line, "<span>· {}</span>", extra.join(" · "));
        }
        if let Some(l) = &s.lookalike {
            let _ = write!(
                line,
                r#"<span class="says__warn">Same text as the name of your agent {}. This can be a copy.</span>"#,
                html::escape(&l.label)
            );
        }
        format!(r#"<div class="says">{line}</div>"#)
    };
    let also = if s.owner {
        also_line(&s.also)
    } else {
        String::new()
    };
    format!(r#"<div class="sender" aria-label="From">{main}{says}{also}</div>"#)
}

/// The side box "About the sender": what the server wrote, and what the agent wrote.
pub fn about_sender(s: &Sender, owner_login: &str, agent_seen: &str) -> String {
    let a = &s.agent;
    let kind = if a.plain { "Key" } else { "Agent" };
    let key = if a.plain {
        String::new()
    } else {
        format!(
            "<div><dt>Key</dt><dd>{}</dd></div>",
            html::escape(&a.key_name)
        )
    };
    let note = if s.owner && !a.plain && !a.is_bucket() {
        "The agent name, the key and the owner are written by the server. The line &quot;Says it is&quot; is the text of the agent."
    } else {
        "The name, the key and the owner are written by the server."
    };
    format!(
        r#"<div class="sidebox"><h3>About the sender</h3><dl class="facts"><div><dt>{kind}</dt><dd>{}</dd></div>{key}<div><dt>Owner</dt><dd>{}</dd></div><div><dt>Agent seen</dt><dd>{agent_seen}</dd></div></dl><p>{note}</p></div>"#,
        html::escape(&a.label),
        html_escape(owner_login),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::inbox::page_items::{ApprovalRow, ItemRow, Loaded};
    use inbox_core::proto::{Interaction, ItemState, Kind, Urgency, Via};

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

    fn view(id: &str, label: &str, state: AgentState) -> AgentView {
        AgentView {
            agent_id: id.into(),
            label: label.into(),
            key_name: "ops-key".into(),
            kind: AgentKind::Agent,
            state,
            named: !label.contains('#'),
            created_at: NOW - 3_600_000,
            last_seen_at: NOW - 60_000,
            observed: Some(Observed {
                says: Some("billing-bot".into()),
                says_first: Some("billing-agent".into()),
                says_changes: 1,
                client: Some("claude-code 2.1".into()),
                user_agent: Some("claude-code/2.1.4 (linux)".into()),
                net: Some("203.0.113.0/24".into()),
                nets: vec!["203.0.113.0/24".into()],
            }),
            plain: false,
        }
    }

    fn notice(agent: AgentView, lock: bool) -> FirstUse<'static> {
        FirstUse {
            agent,
            open: 1,
            lock,
            lookalike: Some(Lookalike {
                label: "Billing <bot>".into(),
                same_client: true,
                same_net: true,
            }),
            similar: None,
            targets: vec![
                ("g1".into(), "Billing <bot>".into()),
                ("g2".into(), "Night ETL".into()),
            ],
            csrf: "tok<",
            typed: "x\"y",
            now_ms: NOW,
        }
    }

    fn page_for<'a>(agent: AgentView) -> AgentPage<'a> {
        AgentPage {
            agent,
            csrf: "tok<",
            lock: false,
            now_ms: NOW,
            open: vec![],
            answered: 14,
            events: vec![
                AgentEventRow {
                    at: NOW - 1000,
                    event: "says_changed".into(),
                    actor: "agent".into(),
                    detail: None,
                },
                AgentEventRow {
                    at: NOW - 5000,
                    event: "blocked".into(),
                    actor: "alice<".into(),
                    detail: Some("cancelled=2 intents=1".into()),
                },
                AgentEventRow {
                    at: NOW - 9000,
                    event: "created".into(),
                    actor: "agent".into(),
                    detail: Some("ordinal=4".into()),
                },
            ],
            named_by: Some(("alice".into(), NOW - 86_400_000)),
            targets: vec![
                ("g2".into(), "Night ETL".into(), AgentState::Known),
                ("g3".into(), "ops-key #5".into(), AgentState::New),
            ],
            counts: KeyCounts {
                known: 3,
                new: 1,
                blocked: 0,
                max: Some(16),
            },
            merged_into: None,
            typed: None,
            can_reset: false,
        }
    }

    fn enroll_for_test() -> EnrollForm {
        EnrollForm {
            csrf: "tok<".into(),
            keys: vec![("k1".into(), "ops<key".into()), ("k2".into(), "etl".into())],
            typed_key: "k2".into(),
            typed_name: "Re\"fund".into(),
        }
    }

    const SECRET: &str = "agt_0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZ-_abcde";

    fn token_for_test(reset: bool) -> TokenPage<'static> {
        TokenPage {
            label: "Billing <bot>",
            key_name: "ops-key",
            agent_id: "g1",
            token: SECRET,
            reset,
        }
    }

    fn forms(html: &str) -> Vec<&str> {
        html.split("<form").skip(1).collect()
    }

    #[test]
    fn the_notice_names_the_choices_and_every_form_has_the_token_and_the_agent() {
        let html = first_use(&notice(
            view("0123456789abcdef", "ops-key #4", AgentState::New),
            true,
        ));
        assert!(html.contains("A new agent is using the key"));
        for action in [ALLOW, BLOCK, MERGE] {
            assert!(html.contains(&format!(r#"action="{action}""#)), "{action}");
        }
        let all = forms(&html);
        assert_eq!(all.len(), 4, "name, allow as, block, merge");
        for f in all {
            let f = f.split("</form>").next().unwrap();
            assert!(f.contains(r#"name="csrf" value="tok&lt;""#), "{f}");
            assert!(
                f.contains(r#"name="agent_id" value="0123456789abcdef""#),
                "{f}"
            );
            assert!(f.contains(r#"method="post""#));
        }
        // The name of the first form is required, and it keeps what the owner typed in a refused form.
        assert!(html.contains(r#"name="name" maxlength="32" required"#));
        assert!(html.contains(r#"value="x&quot;y""#));
        assert!(
            html.contains("Name and allow")
                && html.contains("Allow as &quot;ops-key #4&quot;")
                && html.contains("Block")
        );
        // The merge choices are the known agents of the key, escaped.
        assert!(html.contains(r#"<option value="g1">Billing &lt;bot&gt;</option>"#));
        // The warning: the text equals the name of a known agent.
        assert!(html.contains("is the same as the name of your agent <b>Billing &lt;bot&gt;</b>. Same client and same address range too."));
        assert!(html.contains("The server cannot tell."));
        assert!(!html.contains("<script"));
    }

    #[test]
    fn the_notice_names_a_similar_agent_with_the_sentence_that_hints_can_be_copied() {
        use crate::inbox::agents::SimilarWhy;
        let a = view("a1", "ops-key #4", AgentState::New);
        let mut n = notice(a, false);
        assert!(
            !first_use(&n).contains("Looks like your agent"),
            "no hint, no line"
        );
        for (why, text) in [
            (SimilarWhy::Says, "it says the same text"),
            (
                SimilarWhy::ClientAndNet,
                "it has the same client and the same address range",
            ),
        ] {
            n.similar = Some(Similar {
                agent_id: "g1".into(),
                label: "Billing <bot>".into(),
                why,
            });
            let html = first_use(&n);
            assert!(
                html.contains(&format!(
                    r#"Looks like your agent <a href="{AGENTS}/g1">Billing &lt;bot&gt;</a>: {text}. Hints can be copied. Nothing merges by itself."#
                )),
                "{html}"
            );
            // The line is a note in the box "Observed, not verified", with no form of its own: nothing merges by itself.
            assert_eq!(forms(&html).len(), 4, "the hint adds no form");
        }
    }

    #[test]
    fn the_notice_says_locked_only_with_the_switch_and_the_default_has_no_locked_row() {
        let new = view("a1", "ops-key #4", AgentState::New);
        let free = first_use(&notice(new.clone(), false));
        assert!(!free.contains("locked") && !free.contains("lockline"));
        assert!(free.contains("Name it, allow it or block it."));
        let locked = first_use(&notice(new, true));
        assert!(
            locked.contains("Approve is locked.")
                && locked.contains("Approve stays locked until you allow it.")
        );
        // No merge form when the key has no known agent.
        let mut alone = notice(view("a1", "ops-key #4", AgentState::New), false);
        alone.targets.clear();
        assert!(!first_use(&alone).contains(MERGE));
    }

    #[test]
    fn the_text_of_the_agent_is_in_the_dashed_box_escaped_and_marked() {
        let mut a = view("a1", "ops-key #4", AgentState::New);
        a.observed = Some(Observed {
            says: Some("bill\u{202E}ing <b>bot</b> p\u{430}ypal".into()),
            client: Some("<i>x</i>".into()),
            net: Some("203.0.113.0/24".into()),
            ..Default::default()
        });
        let mut n = notice(a, false);
        n.lookalike = None;
        let html = first_use(&n);
        assert!(!html.contains('\u{202E}') && !html.contains("<b>bot") && !html.contains("<i>x"));
        assert!(
            html.contains("U+202E")
                && html.contains("U+0430")
                && html.contains("&lt;b&gt;bot&lt;/b&gt;")
        );
        // The notice has the box with its label, and the title has no text of the agent.
        assert!(html.contains("Observed, not verified") && html.contains(r#"<div class="obs">"#));
        let title = html
            .split("<h3")
            .nth(1)
            .unwrap()
            .split("</h3>")
            .next()
            .unwrap();
        assert!(!title.contains("bill") && title.contains("ops-key"));
    }

    #[test]
    fn the_agent_page_has_the_forms_of_its_state() {
        let known = agent_page(
            &frame(),
            &page_for(view("g1", "Billing bot", AgentState::Known)),
            None,
        );
        for action in [RENAME, MERGE, BLOCK] {
            assert!(known.contains(&format!(r#"action="{action}""#)), "{action}");
        }
        assert!(!known.contains(UNBLOCK) && !known.contains(&format!(r#"action="{ALLOW}""#)));
        for f in forms(&known)
            .into_iter()
            .filter(|f| f.contains("/inbox/agents/"))
        {
            let f = f.split("</form>").next().unwrap();
            assert!(
                f.contains(r#"name="csrf" value="tok&lt;""#) && f.contains(r#"name="agent_id""#),
                "{f}"
            );
        }
        // The page shows the facts, the table, the observed box and the events in text of the server.
        assert!(
            known.contains("Open requests") && known.contains("No request waits for this agent.")
        );
        assert!(
            known.contains("Observed, not verified")
                && known.contains("Anyone who holds the key can send these texts.")
        );
        assert!(known.contains("first: &quot;billing-agent&quot;, changed 1 time"));
        assert!(known.contains("<code>claude-code/2.1.4 (linux)</code>"));
        assert!(
            known.contains("Answered <b>14</b>")
                && known.contains("4 (3 known, 1 new)")
                && known.contains("16 agents")
        );
        assert!(known.contains("Blocked by alice&lt;. 2 open requests cancelled."));
        assert!(known.contains("The text that the agent says about itself changed"));
        assert!(known.contains("Made by a first call of the key"));
        assert!(known.contains("(new)"));
        assert!(known.contains(r#"href="/auth/settings/inbox/items?agent=g1""#));
        // A new agent has the box Allow. A blocked agent has Unblock and nothing else. A merged agent has no form.
        let new = agent_page(
            &frame(),
            &page_for(view("g4", "ops-key #4", AgentState::New)),
            None,
        );
        assert!(new.contains(&format!(r#"action="{ALLOW}""#)));
        let blocked = agent_page(
            &frame(),
            &page_for(view("g1", "Billing bot", AgentState::Blocked)),
            None,
        );
        assert!(blocked.contains(&format!(r#"action="{UNBLOCK}""#)));
        assert!(
            !blocked.contains(RENAME) && !blocked.contains(BLOCK) && blocked.contains("Blocked")
        );
        let mut merged = page_for(view("g1", "Billing bot", AgentState::Merged));
        merged.merged_into = Some(("g2".into(), "Night ETL".into()));
        let merged = agent_page(&frame(), &merged, None);
        assert!(
            !merged.contains("<form")
                || !merged.contains(r#"action="/auth/settings/inbox/agents/"#)
        );
        assert!(
            merged.contains("merged into <a href=\"/auth/settings/inbox/agents/g2\">Night ETL</a>")
        );
        // The bucket cannot be named, merged or blocked.
        let mut b = view("b1", "Unattributed · ops-key", AgentState::Known);
        b.kind = AgentKind::Bucket;
        b.observed = None;
        let bucket = agent_page(&frame(), &page_for(b), None);
        assert!(
            !bucket.contains(r#"action="/auth/settings/inbox/agents/"#)
                && !bucket.contains("Observed, not verified")
        );
        assert!(bucket.contains("This is not an agent."));
    }

    #[test]
    fn the_events_hold_no_text_of_the_agent_and_a_rename_keeps_what_was_typed() {
        let mut p = page_for(view("g1", "Billing bot", AgentState::Known));
        p.typed = Some("bad \u{202E}name");
        let html = agent_page(&frame(), &p, None);
        assert!(
            !html.contains('\u{202E}'),
            "the typed name is an attribute with a visible mark"
        );
        assert!(html.contains("&lt;U+202E&gt;"));
        // An event that the page does not know is shown as text, escaped.
        p.events.push(AgentEventRow {
            at: NOW,
            event: "x<y".into(),
            actor: "agent".into(),
            detail: None,
        });
        assert!(agent_page(&frame(), &p, None).contains("x&lt;y"));
    }

    #[test]
    fn the_list_has_every_state_and_links_to_each_page() {
        let mut a = view("g1", "Billing bot", AgentState::Known);
        a.observed = None;
        let rows = vec![
            ListRow {
                agent: a.clone(),
                open: 2,
                idle: false,
            },
            ListRow {
                agent: view("g4", "ops-key #4", AgentState::New),
                open: 1,
                idle: false,
            },
            ListRow {
                agent: view("g5", "Old <i>", AgentState::Blocked),
                open: 0,
                idle: true,
            },
        ];
        let html = agents_page(
            &frame(),
            &AgentsList {
                rows,
                now_ms: NOW,
                enroll: None,
            },
            None,
        );
        assert!(
            html.contains(r#"href="/auth/settings/inbox/agents/g4""#)
                && html.contains("New, not named")
        );
        assert!(
            html.contains("Old &lt;i&gt;") && html.contains("Blocked") && html.contains("idle")
        );
        // The new agent is the first row.
        assert!(html.find("ops-key #4").unwrap() < html.find("Billing bot").unwrap());
        // The list never shows what an agent said.
        assert!(!html.contains("billing-bot"));
        let empty = agents_page(
            &frame(),
            &AgentsList {
                rows: vec![],
                now_ms: NOW,
                enroll: None,
            },
            None,
        );
        assert!(empty.contains("No agent yet."));
    }

    #[test]
    fn the_notices_of_the_forms_are_a_closed_list() {
        for code in [
            "agent_allowed",
            "agent_renamed",
            "agent_blocked",
            "agent_unblocked",
            "agent_merged",
            "agent_gone",
            "agent_bad_name",
            "agent_name_taken",
            "agent_bad_state",
            "agent_cross_key",
            "agent_no_target",
        ] {
            assert!(notice_for(code).is_some(), "{code}");
            // The panel of the items finds them too.
            assert_eq!(super::super::items::notice_for(code), notice_for(code));
        }
        assert!(notice_for("agent_<script>").is_none());
        assert!(!notice_for("agent_gone").unwrap().ok && notice_for("agent_merged").unwrap().ok);
    }

    #[test]
    fn no_page_of_the_agents_names_the_product() {
        let new = view("g4", "ops-key #4", AgentState::New);
        let pages = [
            agent_page(
                &frame(),
                &page_for(view("g1", "Billing bot", AgentState::Known)),
                None,
            ),
            agent_page(&frame(), &page_for(new.clone()), None),
            agents_page(
                &frame(),
                &AgentsList {
                    rows: vec![ListRow {
                        agent: new.clone(),
                        open: 1,
                        idle: false,
                    }],
                    now_ms: NOW,
                    enroll: Some(enroll_for_test()),
                },
                None,
            ),
            token_page(&frame(), &token_for_test(false)),
            token_page(&frame(), &token_for_test(true)),
            agent_page(
                &frame(),
                &AgentPage {
                    can_reset: true,
                    ..page_for(view("g1", "Billing bot", AgentState::Known))
                },
                None,
            ),
            first_use(&notice(new, true)),
        ];
        for page in pages {
            let body = page.to_lowercase();
            let text = body.split("<main").nth(1).unwrap_or(&body);
            for word in ["portal", "inbox", "inbox", "circular"] {
                assert!(!text.contains(word), "{word}");
            }
        }
        let _ = (
            Interaction::OneShot,
            ItemState::Pending,
            Kind::Approval,
            Urgency::Normal,
            Via::Mcp,
        );
        let _: Option<(&ItemRow, &ApprovalRow, &Loaded)> = None;
    }

    #[test]
    fn the_token_page_shows_the_token_and_the_header_line_with_copy_buttons_and_says_one_time() {
        let page = token_page(&frame(), &token_for_test(false));
        assert!(page.contains(&format!(
            r#"id="agent-token" type="text" readonly value="{SECRET}""#
        )));
        assert!(page.contains(&format!(
            r#"id="agent-header" type="text" readonly value="Agent-Token: {SECRET}""#
        )));
        assert!(
            page.contains(r#"data-copy="agent-token""#)
                && page.contains(r#"data-copy="agent-header""#)
        );
        assert!(page.contains("shows it one time") && page.contains("Copy its token now"));
        assert!(page.contains("<code>agent_token</code>"));
        // The name is text of the owner: it is escaped.
        assert!(page.contains("Billing &lt;bot&gt;") && !page.contains("Billing <bot>"));
        // The page links only inside the application.
        let main = page.split("<main").nth(1).unwrap();
        assert!(!main.contains("http://") && !main.contains("https://"));
        assert!(page.contains(r#"href="/auth/settings/inbox/agents/g1""#));
        let reset = token_page(&frame(), &token_for_test(true));
        assert!(reset.contains("The old token stopped at once"));
        assert_eq!(header_line(SECRET), format!("Agent-Token: {SECRET}"));
    }

    #[test]
    fn the_list_has_the_form_add_agent_only_with_a_key_in_the_mode_enrolled() {
        let none = agents_page(
            &frame(),
            &AgentsList {
                rows: vec![],
                now_ms: NOW,
                enroll: None,
            },
            None,
        );
        assert!(!none.contains(ENROLL) && !none.contains("Add agent"));
        let html = agents_page(
            &frame(),
            &AgentsList {
                rows: vec![],
                now_ms: NOW,
                enroll: Some(enroll_for_test()),
            },
            None,
        );
        assert!(html.contains(&format!(r#"method="post" action="{ENROLL}""#)));
        assert!(html.contains(r#"name="csrf" value="tok&lt;""#));
        // The key is a choice, the name is typed, and the refused values come back.
        assert!(html.contains(r#"<option value="k1">ops&lt;key</option>"#));
        assert!(html.contains(r#"<option value="k2" selected>etl</option>"#));
        assert!(html.contains(r#"value="Re&quot;fund""#));
        assert!(html.contains("shows it one time"));
    }

    #[test]
    fn the_box_reset_token_shows_only_when_the_page_says_so_and_it_warns_the_old_token_stops() {
        let plain = agent_page(
            &frame(),
            &page_for(view("g1", "Billing bot", AgentState::Known)),
            None,
        );
        assert!(!plain.contains(RESET) && !plain.contains("Reset token"));
        let page = agent_page(
            &frame(),
            &AgentPage {
                can_reset: true,
                ..page_for(view("g1", "Billing bot", AgentState::Known))
            },
            None,
        );
        assert!(page.contains(&format!(r#"method="post" action="{RESET}""#)));
        assert!(page.contains("Reset token") && page.contains("The old token stops at once."));
        let form = page.split(&format!(r#"action="{RESET}""#)).nth(1).unwrap();
        let form = &form[..form.find("</form>").unwrap()];
        assert!(form.contains(r#"name="csrf""#) && form.contains(r#"name="agent_id" value="g1""#));
    }

    #[test]
    fn the_events_of_the_enrolled_mode_have_a_text_with_no_secret() {
        for (event, text) in [
            ("enrolled", "Added by alice"),
            ("token_reset", "Token reset by alice"),
        ] {
            let t = event_text(&AgentEventRow {
                at: NOW,
                event: event.into(),
                actor: "alice".into(),
                detail: None,
            });
            assert!(t.starts_with(text), "{t}");
        }
        assert!(notice_for("agent_no_key").is_some() && notice_for("agent_cap").is_some());
    }

    #[test]
    fn the_line_also_asked_by_names_the_other_agents_with_text_of_the_server() {
        assert_eq!(also_line(&[]), "");
        let line = also_line(&[
            AlsoBy {
                agent_id: "g2".into(),
                label: "Night <ETL>".into(),
            },
            AlsoBy {
                agent_id: "g3".into(),
                label: "ops-key #5".into(),
            },
        ]);
        assert!(line.contains("Also asked by"));
        assert!(line.contains(r#"<a href="/auth/settings/inbox/agents/g2">Night &lt;ETL&gt;</a>, <a href="/auth/settings/inbox/agents/g3">ops-key #5</a>"#));
    }
}
