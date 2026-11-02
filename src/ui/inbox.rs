//! The page of the tab "Inbox": one dense page with the alerts that were sent, the phones and the
//! connections of the phones.
//!
//! The page shows only the parts that the permissions of the account give. A part that the account
//! may not use is not in the page. The left zone "Sent" has the alerts. The right zones have the
//! phones and the connections. The toolbar has the period, the send form and the documentation.
//!
//! Every filter is a link with a query parameter, so the page works without script. A row of the
//! alerts opens its detail in place with `?open=<id>`. The form that makes an enrollment code is a
//! POST: its answer is the page itself, with the code in a popover that stays open until "Done".
//! Everything that comes from outside is escaped. This includes names, titles and logins.

use std::fmt::Write as _;

use super::components::{
    bulk_all_box, bulk_box, bulk_button, bulk_form, chips, icon, icon_post, popover_with,
    split_zones, zone, Bulk, Chip, Side, Zone,
};
use super::settings::{notice_html, plural, Frame, Notice, Tab};
use crate::inbox::admin::{
    AlertCount, AlertDetail, AlertSummary, ConnectionView, DeviceRow, DeviceState,
};
use crate::util::{html_escape, percent_encode_query_component, time_tag};

/// The page of the inbox in the settings. The gear of the launcher links to it for an account that
/// is not the superadmin.
pub const PAGE: &str = "/auth/settings/inbox";
pub const SEND_PATH: &str = "/auth/settings/inbox/send";
pub const REVOKE_PATH: &str = "/auth/settings/inbox/revoke";
pub const CODE_PATH: &str = "/auth/settings/inbox/code";

/// The longest title of an alert, in characters.
pub const MAX_TITLE: usize = inbox_core::proto::MAX_TITLE_CHARS;

/// A group that the account may send to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GroupChoice {
    pub id: String,
    pub name: String,
}

/// What the account may do in the tab. Each part of the page needs one of these rights.
#[derive(Clone, Debug, Default)]
pub struct Rights {
    /// `alert.read` on one group at least: the zone "Sent".
    pub sent: bool,
    /// `phone.read`: the phones of all the accounts.
    pub phones: bool,
    /// `phone.revoke` together with `phone.read`.
    pub revoke: bool,
    /// `phone.log`: the zone "Connections".
    pub connections: bool,
    /// `inbox.receive` on one group at least: the account has phones of its own.
    pub own_phones: bool,
    /// The groups with `alert.send`.
    pub send: Vec<GroupChoice>,
}

impl Rights {
    /// Whether the account has any right in the tab.
    pub fn any(&self) -> bool {
        self.sent || self.phones || self.connections || self.own_phones || !self.send.is_empty()
    }
}

/// The text of the outcome of a form, from a fixed list.
pub fn notice_for(code: &str) -> Option<Notice> {
    let (text, ok) = match code {
        "sent" => ("The alert is sent.", true),
        "duplicate" => ("This alert was sent before. Nothing is sent again.", true),
        "revoked" => ("Revoked. A revoked phone gets no alert.", true),
        "no_title" => ("Type a title.", false),
        "bad_title" => (
            "The title has 1 to 128 characters, on one line, with no control character.",
            false,
        ),
        "bad_group" => ("Choose a group that you may send to.", false),
        "unknown_device" => ("That phone does not exist, or you cannot revoke it.", false),
        "too_many_codes" => (
            "You have the largest number of codes that are not used. Use one, or wait until it ends.",
            false,
        ),
        "limited" => (
            "The limit of alerts of your account is reached. Try again later.",
            false,
        ),
        "failed" => ("The inbox could not do this. Try again.", false),
        _ => return None,
    };
    Some(Notice { text, ok })
}

// The query ------------------------------------------------------------------------------------

/// The period of the zone "Sent" and of its graph.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Period {
    Hour,
    SixHours,
    Day,
    Week,
    Month,
}

impl Period {
    pub const ALL: [Period; 5] = [
        Self::Hour,
        Self::SixHours,
        Self::Day,
        Self::Week,
        Self::Month,
    ];
    pub const DEFAULT: Period = Self::Day;

    fn name(self) -> &'static str {
        match self {
            Self::Hour => "1h",
            Self::SixHours => "6h",
            Self::Day => "24h",
            Self::Week => "7d",
            Self::Month => "30d",
        }
    }

    fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|p| p.name() == name)
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Hour => "Last 1 h",
            Self::SixHours => "Last 6 h",
            Self::Day => "Last 24 h",
            Self::Week => "Last 7 d",
            Self::Month => "Last 30 d",
        }
    }

    pub fn millis(self) -> i64 {
        const HOUR: i64 = 3_600_000;
        match self {
            Self::Hour => HOUR,
            Self::SixHours => 6 * HOUR,
            Self::Day => 24 * HOUR,
            Self::Week => 7 * 24 * HOUR,
            Self::Month => 30 * 24 * HOUR,
        }
    }

    /// The size of a bar of the graph, in milliseconds: 12 to 30 bars for each period.
    pub fn bucket_millis(self) -> i64 {
        const MINUTE: i64 = 60_000;
        match self {
            Self::Hour => 5 * MINUTE,
            Self::SixHours => 15 * MINUTE,
            Self::Day => 60 * MINUTE,
            Self::Week => 6 * 60 * MINUTE,
            Self::Month => 24 * 60 * MINUTE,
        }
    }
}

/// The filter of the delivery of an alert.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AlertState {
    /// Each phone received it.
    Received,
    /// A phone has not received it yet.
    Pending,
    /// A phone never received it before it expired.
    Expired,
}

impl AlertState {
    fn name(self) -> &'static str {
        match self {
            Self::Received => "ok",
            Self::Pending => "wait",
            Self::Expired => "expired",
        }
    }

    fn of(alert: &AlertSummary) -> Self {
        if alert.expired > 0 {
            Self::Expired
        } else if alert.received < alert.targets {
            Self::Pending
        } else {
            Self::Received
        }
    }
}

/// The filter of the connections.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConnState {
    Open,
    Closed,
}

/// The parameters of the page. Every one has a default that the address omits.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Query {
    pub period: Period,
    pub group: Option<String>,
    pub alert: Option<AlertState>,
    pub phone: Option<DeviceState>,
    pub pending: bool,
    pub mine: bool,
    pub conn: Option<ConnState>,
    pub login: String,
    pub open: Option<String>,
}

impl Default for Query {
    fn default() -> Self {
        Self {
            period: Period::DEFAULT,
            group: None,
            alert: None,
            phone: None,
            pending: false,
            mine: false,
            conn: None,
            login: String::new(),
            open: None,
        }
    }
}

fn id_like(text: &str, max: usize) -> bool {
    (1..=max).contains(&text.len()) && text.bytes().all(|b| b.is_ascii_graphic())
}

impl Query {
    /// Reads the query string of an address. `None`: a parameter that the page does not know, a
    /// parameter that repeats, or a value that is not valid. The page answers 400.
    pub fn parse(pairs: &[(String, String)]) -> Option<Query> {
        let mut query = Query::default();
        let mut seen: Vec<&str> = Vec::new();
        for (name, value) in pairs {
            if seen.contains(&name.as_str()) {
                return None;
            }
            seen.push(name);
            match name.as_str() {
                // The code of the outcome of a form: the page reads it apart.
                "s" => {}
                "p" => query.period = Period::from_name(value)?,
                "g" => {
                    if !id_like(value, 64) {
                        return None;
                    }
                    query.group = Some(value.clone());
                }
                "a" => {
                    query.alert = Some(match value.as_str() {
                        "ok" => AlertState::Received,
                        "wait" => AlertState::Pending,
                        "expired" => AlertState::Expired,
                        _ => return None,
                    })
                }
                "ps" => {
                    query.phone = Some(match value.as_str() {
                        "online" => DeviceState::Online,
                        "unreachable" => DeviceState::Unreachable,
                        "offline" => DeviceState::Offline,
                        "revoked" => DeviceState::Revoked,
                        _ => return None,
                    })
                }
                "pp" if value == "1" => query.pending = true,
                "mine" if value == "1" => query.mine = true,
                "cs" => {
                    query.conn = Some(match value.as_str() {
                        "open" => ConnState::Open,
                        "closed" => ConnState::Closed,
                        _ => return None,
                    })
                }
                "login" => {
                    if value.chars().count() > 64 || value.chars().any(char::is_control) {
                        return None;
                    }
                    query.login = value.trim().to_string();
                }
                "open" => {
                    if value.len() != 32 || !value.bytes().all(|b| b.is_ascii_hexdigit()) {
                        return None;
                    }
                    query.open = Some(value.clone());
                }
                _ => return None,
            }
        }
        Some(query)
    }

    /// The parameters as `name=value` pairs, without the defaults.
    fn pairs(&self) -> Vec<(&'static str, String)> {
        let mut out = Vec::new();
        if self.period != Period::DEFAULT {
            out.push(("p", self.period.name().to_string()));
        }
        if let Some(group) = &self.group {
            out.push(("g", group.clone()));
        }
        if let Some(state) = self.alert {
            out.push(("a", state.name().to_string()));
        }
        if let Some(state) = self.phone {
            out.push(("ps", state.word().to_string()));
        }
        if self.pending {
            out.push(("pp", "1".to_string()));
        }
        if self.mine {
            out.push(("mine", "1".to_string()));
        }
        if let Some(state) = self.conn {
            let word = if state == ConnState::Open {
                "open"
            } else {
                "closed"
            };
            out.push(("cs", word.to_string()));
        }
        if !self.login.is_empty() {
            out.push(("login", self.login.clone()));
        }
        if let Some(id) = &self.open {
            out.push(("open", id.clone()));
        }
        out
    }

    /// The address of the page with this query. It closes the detail of an alert unless `open` is
    /// set again.
    pub fn href(&self, change: impl FnOnce(&mut Query)) -> String {
        let mut query = self.clone();
        query.open = None;
        change(&mut query);
        let pairs = query.pairs();
        if pairs.is_empty() {
            return PAGE.to_string();
        }
        let text: Vec<String> = pairs
            .iter()
            .map(|(name, value)| format!("{name}={}", percent_encode_query_component(value)))
            .collect();
        format!("{PAGE}?{}", text.join("&"))
    }

    /// A filter chip: the address that turns the filter on, or off when it is on already.
    fn toggle(
        &self,
        on: bool,
        set: impl FnOnce(&mut Query),
        clear: impl FnOnce(&mut Query),
    ) -> String {
        self.href(|q| if on { clear(q) } else { set(q) })
    }
}

// The data of the page -------------------------------------------------------------------------

/// One group of the file.
#[derive(Clone, Debug)]
pub struct GroupLine {
    pub id: String,
    pub name: String,
    pub critical: bool,
}

/// Who sent an alert.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Origin {
    /// The portal about itself (watch).
    Watch,
    /// An account that used the form of the page.
    Account,
    /// An API key, with its name.
    Key(String),
}

impl Origin {
    /// The origin from the `source` of an alert. `resolve` gives the name of a key of the database
    /// from its id.
    pub fn of(source: &str, resolve: &dyn Fn(&str) -> Option<String>) -> Origin {
        match source {
            "portal" => Origin::Watch,
            "admin" => Origin::Account,
            other => {
                let name = other.split_once(':').map_or(other, |(_, name)| name);
                match name.strip_prefix("key:") {
                    Some(id) => Origin::Key(resolve(id).unwrap_or_else(|| "API key".to_string())),
                    None => Origin::Key(name.to_string()),
                }
            }
        }
    }
}

pub struct AlertRow {
    pub summary: AlertSummary,
    pub origin: Origin,
}

/// The zone "Sent".
pub struct SentData {
    /// The newest alerts of the period, of the groups that the account may read.
    pub alerts: Vec<AlertRow>,
    /// The list holds the most that the page asks for: older alerts of the period may exist.
    pub truncated: bool,
    /// The counts for the graph: bucket 0 starts at `since_ms`.
    pub counts: Vec<AlertCount>,
    pub since_ms: i64,
    /// The alert that the row opens, with its deliveries.
    pub opened: Option<AlertDetail>,
}

/// The zone "Phones", or "My phones".
pub struct PhonesData {
    /// All the phones, or the phones of the account when it has no `phone.read`.
    pub rows: Vec<DeviceRow>,
    /// The account may revoke any phone.
    pub revoke_all: bool,
    /// The account may revoke its own phones.
    pub revoke_own: bool,
}

pub struct ConnData {
    pub rows: Vec<ConnectionView>,
}

/// What the documentation shows.
pub struct DocsData {
    /// The address of the portal as the request shows it.
    pub base: String,
}

/// A new enrollment code, shown one time.
pub struct CodeData<'a> {
    pub code: &'a str,
    pub fingerprint: &'a str,
    pub expires_at_ms: i64,
}

pub struct InboxView<'a> {
    pub rights: &'a Rights,
    pub notice: Option<Notice>,
    pub csrf: &'a str,
    /// The key of the send form: a double click sends one alert.
    pub idempotency_key: &'a str,
    pub query: &'a Query,
    pub now_ms: i64,
    /// The login of the account.
    pub login: &'a str,
    /// The groups that the account may read, send to or receive, with their names.
    pub groups: &'a [GroupLine],
    pub sent: Option<SentData>,
    pub phones: Option<PhonesData>,
    pub connections: Option<ConnData>,
    pub docs: Option<DocsData>,
    pub code: Option<CodeData<'a>>,
    /// The session is a session of "Log in as": it sends no alert and revokes no phone.
    pub impersonated: bool,
}

// Small parts ----------------------------------------------------------------------------------

fn when(ms: i64) -> String {
    time_tag(ms / 1000, false)
}

fn when_opt(ms: Option<i64>) -> String {
    match ms {
        Some(ms) => when(ms),
        None => r#"<span class="muted">never</span>"#.to_string(),
    }
}

/// A status icon with its word in the tooltip.
fn st(class: &str, name: &str, label: &str) -> String {
    let label = html_escape(label);
    format!(
        r#"<span class="st {class}" role="img" aria-label="{label}" title="{label}">{}</span>"#,
        icon(name)
    )
}

fn alert_icon(state: AlertState, alert: &AlertSummary) -> String {
    match state {
        AlertState::Received => st(
            "st--ok",
            "circle-check",
            &format!("Received by {} of {} phones", alert.received, alert.targets),
        ),
        AlertState::Pending => st(
            "st--warn",
            "clock",
            &format!(
                "Pending: {} of {} phones have it",
                alert.received, alert.targets
            ),
        ),
        AlertState::Expired => st(
            "st--bad",
            "alert-triangle",
            &format!(
                "Expired: {} of {} phones never got it",
                alert.expired, alert.targets
            ),
        ),
    }
}

fn origin_cell(origin: &Origin) -> String {
    let (name, text, label) = match origin {
        Origin::Watch => (
            "activity",
            "portal watch".to_string(),
            "Sent by the portal watch".to_string(),
        ),
        Origin::Account => (
            "user",
            "account".to_string(),
            "Sent by an account".to_string(),
        ),
        Origin::Key(name) => (
            "webhook",
            name.clone(),
            format!("Sent by the API key {name}"),
        ),
    };
    format!(
        r#"<span class="src" title="{}">{}{}</span>"#,
        html_escape(&label),
        icon(name),
        html_escape(&text)
    )
}

/// The age of a time: `now`, `18m`, `2h`, `3d`.
fn age(now_ms: i64, then_ms: i64) -> String {
    let secs = (now_ms - then_ms).max(0) / 1000;
    match secs {
        0..=59 => "now".to_string(),
        60..=3599 => format!("{}m", secs / 60),
        3600..=86_399 => format!("{}h", secs / 3600),
        _ => format!("{}d", secs / 86_400),
    }
}

/// A chip that is not a control: the kind of message that the zone shows.
fn kind_chip(label: &str, icon_name: &str, count: usize) -> String {
    format!(
        r#"<span class="chip is-on" role="img" aria-label="{0}: {count}" title="{0}">{1}<span class="num">{count}</span></span>"#,
        html_escape(label),
        icon(icon_name)
    )
}

/// A chip of the roadmap: dashed, grey, with no behaviour. The place is ready for a later kind.
fn future_chip(label: &str, icon_name: &str) -> String {
    format!(
        r#"<button class="chip chip--future" type="button" disabled aria-label="{0}" title="{0}">{1}</button>"#,
        html_escape(label),
        icon(icon_name)
    )
}

const SEPARATOR: &str = r#"<span class="chips__sep"></span>"#;

// The toolbar ----------------------------------------------------------------------------------

fn period_menu(view: &InboxView) -> String {
    let mut menu = String::new();
    for period in Period::ALL {
        let on = period == view.query.period;
        let _ = write!(
            menu,
            r#"<a class="menu__item" href="{}"{}><span class="check">{}</span>{}</a>"#,
            html_escape(&view.query.href(|q| q.period = period)),
            if on { r#" aria-current="true""# } else { "" },
            if on { icon("check") } else { String::new() },
            period.label()
        );
    }
    popover_with(
        "Period",
        "pick",
        &format!(
            r#"{}<span class="pick__value">{}</span>{}"#,
            icon("clock"),
            view.query.period.label(),
            icon("chevron-down")
        ),
        Side::Start,
        " pop__menu--list",
        &menu,
    )
}

fn docs_popover(view: &InboxView, docs: &DocsData) -> String {
    let mut groups = String::new();
    for g in view.groups {
        let _ = write!(
            groups,
            r#"<div class="lrow"><code>{}</code><span>{}</span><span class="muted">{}</span></div>"#,
            html_escape(&g.id),
            html_escape(&g.name),
            if g.critical { "critical" } else { "-" }
        );
    }
    let body = format!(
        r#"<div class="twoCols"><div><h3 class="sectionLabel">Endpoints</h3><div class="monoLine">POST /api/v1/alerts<br>GET /api/v1/alerts/{{id}}<br>POST /api/v1/notify/{{group}}<br>POST /api/v1/webhooks/alertmanager/{{group}}</div><div class="monoLine">Authorization: Bearer &lt;key&gt;</div><div class="monoLine">{}</div></div><div><h3 class="sectionLabel">Groups</h3>{}</div></div>"#,
        html_escape(&docs.base),
        if groups.is_empty() {
            r#"<span class="muted">No group</span>"#.to_string()
        } else {
            groups
        }
    );
    popover_with(
        "Documentation for senders",
        "ib",
        &icon("book"),
        Side::End,
        " pop__menu--wide pop__menu--pad",
        &body,
    )
}

fn send_popover(view: &InboxView) -> String {
    let choices = &view.rights.send;
    let group_field = if choices.len() == 1 {
        // One group is no choice: the form names it and sends it.
        format!(
            r#"<input type="hidden" name="group" value="{}"><div class="field"><span class="field__label">Group</span><span>{}</span></div>"#,
            html_escape(&choices[0].id),
            html_escape(&choices[0].name)
        )
    } else {
        let mut options = String::new();
        for c in choices {
            let _ = write!(
                options,
                r#"<option value="{}">{}</option>"#,
                html_escape(&c.id),
                html_escape(&c.name)
            );
        }
        format!(
            r#"<div class="field"><label class="field__label" for="send-group">Group</label><select class="uiInput" id="send-group" name="group" required>{options}</select></div>"#
        )
    };
    let body = format!(
        r#"<form class="composer" method="post" action="{SEND_PATH}" data-gate><input type="hidden" name="csrf" value="{}"><input type="hidden" name="key" value="{}">{group_field}<div class="field"><label class="field__label" for="send-title">Title</label><input class="uiInput" type="text" id="send-title" name="title" maxlength="{MAX_TITLE}" autocomplete="off" required></div><div class="composer__row"><button class="button button--primary" type="submit">Send</button></div></form>"#,
        html_escape(view.csrf),
        html_escape(view.idempotency_key),
    );
    popover_with(
        "",
        "button button--primary",
        &format!("{}Send alert", icon("send")),
        Side::End,
        " pop__menu--pad",
        &body,
    )
}

fn bar(view: &InboxView) -> String {
    let mut out = String::from(r#"<div class="tbar tbar--inbox">"#);
    if let Some(sent) = &view.sent {
        out.push_str(&period_menu(view));
        let _ = write!(
            out,
            r#"<span class="bar__kinds" role="group" aria-label="Kinds of message">{}{}{}</span>"#,
            kind_chip("Alerts: one way, no answer", "bell", sent.alerts.len()),
            future_chip("Questions: not enabled", "help-circle"),
            future_chip("Approvals: not enabled", "checkbox"),
        );
    }
    out.push_str(r#"<span class="grow"></span>"#);
    if let Some(sent) = &view.sent {
        let _ = write!(
            out,
            r#"<span class="bar__range">{} - {}</span>"#,
            time_tag(sent.since_ms / 1000, false),
            time_tag(view.now_ms / 1000, false)
        );
    }
    if let Some(docs) = &view.docs {
        out.push_str(&docs_popover(view, docs));
    }
    if !view.rights.send.is_empty() && !view.impersonated {
        out.push_str(&send_popover(view));
    }
    out.push_str("</div>");
    out
}

// Sent -----------------------------------------------------------------------------------------

/// The graph: the alerts for each bar of time. A bar of a critical group is on top, in the colour of
/// a warning. The graph is an inline SVG that the code builds from attributes only, because the CSP
/// forbids `style`.
fn graph(view: &InboxView, sent: &SentData) -> String {
    const HEIGHT: i64 = 52;
    const STEP: i64 = 10;
    let period = view.query.period;
    let bucket = period.bucket_millis();
    let bars = (period.millis() / bucket) as usize;
    let mut normal = vec![0i64; bars];
    let mut critical = vec![0i64; bars];
    for n in &sent.counts {
        let Some(group) = view.groups.iter().find(|g| g.id == n.group) else {
            continue;
        };
        if view.query.group.as_deref().is_some_and(|g| g != group.id) {
            continue;
        }
        let at = usize::try_from(n.bucket)
            .unwrap_or(usize::MAX)
            .min(bars - 1);
        if group.critical {
            critical[at] += n.count;
        } else {
            normal[at] += n.count;
        }
    }
    let max = normal
        .iter()
        .zip(&critical)
        .map(|(a, c)| a + c)
        .max()
        .unwrap_or(0)
        .max(1);
    let total: i64 = normal.iter().chain(&critical).sum();
    let width = bars as i64 * STEP;
    let mut out = format!(
        r#"<div class="chartMini"><svg class="chart" viewBox="0 0 {width} {HEIGHT}" preserveAspectRatio="none" role="img" aria-label="Alerts for each bar of time over the period: {total} in total">"#
    );
    for i in 0..bars {
        let (a, c) = (normal[i], critical[i]);
        if a + c == 0 {
            continue;
        }
        let from = sent.since_ms + i as i64 * bucket;
        let text = crate::util::format_utc(from / 1000);
        let ha = a * (HEIGHT - 2) / max;
        let hc = c * (HEIGHT - 2) / max;
        let x = i as i64 * STEP + 1;
        if a > 0 {
            let _ = write!(
                out,
                r#"<rect class="bar-a" x="{x}" y="{}" width="{}" height="{}" rx="1"><title>{text} UTC: {a} alerts</title></rect>"#,
                HEIGHT - ha.max(1) - hc,
                STEP - 2,
                ha.max(1)
            );
        }
        if c > 0 {
            let _ = write!(
                out,
                r#"<rect class="bar-c" x="{x}" y="{}" width="{}" height="{}" rx="1"><title>{text} UTC: {c} critical</title></rect>"#,
                HEIGHT - hc.max(1),
                STEP - 2,
                hc.max(1)
            );
        }
    }
    out.push_str("</svg></div>");
    out
}

fn group_name<'a>(groups: &'a [GroupLine], id: &'a str) -> &'a str {
    groups
        .iter()
        .find(|g| g.id == id)
        .map_or(id, |g| g.name.as_str())
}

fn sent_zone(view: &InboxView, sent: &SentData) -> String {
    let query = view.query;
    // The group chips count the alerts of the state that is on. The state chips count the alerts
    // of the group that is on.
    let in_group = |a: &&AlertRow| query.group.as_deref().is_none_or(|g| g == a.summary.group);
    let in_state = |a: &&AlertRow| query.alert.is_none_or(|s| s == AlertState::of(&a.summary));
    let shown: Vec<&AlertRow> = sent
        .alerts
        .iter()
        .filter(in_group)
        .filter(in_state)
        .collect();
    let state_count = |state: AlertState| {
        sent.alerts
            .iter()
            .filter(in_group)
            .filter(|a| AlertState::of(&a.summary) == state)
            .count()
    };
    let group_count = |id: &str| {
        sent.alerts
            .iter()
            .filter(in_state)
            .filter(|a| a.summary.group == id)
            .count()
    };
    let total = sent.alerts.iter().filter(in_group).filter(in_state).count();

    let all_href = query.href(|q| q.group = None);
    let mut group_chips = vec![Chip {
        label_html: "All",
        name: "",
        count: sent.alerts.iter().filter(in_state).count(),
        href: &all_href,
        on: query.group.is_none(),
    }];
    let group_labels: Vec<String> = view
        .groups
        .iter()
        .map(|g| {
            if g.critical {
                format!("{}{}", icon("bell-ringing"), html_escape(&g.name))
            } else {
                html_escape(&g.name)
            }
        })
        .collect();
    let group_hrefs: Vec<String> = view
        .groups
        .iter()
        .map(|g| {
            query.toggle(
                query.group.as_deref() == Some(&g.id),
                |q| q.group = Some(g.id.clone()),
                |q| q.group = None,
            )
        })
        .collect();
    for (i, g) in view.groups.iter().enumerate() {
        group_chips.push(Chip {
            label_html: &group_labels[i],
            name: "",
            count: group_count(&g.id),
            href: &group_hrefs[i],
            on: query.group.as_deref() == Some(&g.id),
        });
    }
    let state_defs = [
        (
            AlertState::Received,
            "circle-check",
            "st--ok",
            "Received by all phones",
        ),
        (
            AlertState::Pending,
            "clock",
            "st--warn",
            "Pending: some phones have not received it",
        ),
        (
            AlertState::Expired,
            "alert-triangle",
            "st--bad",
            "Expired: some phones never received it",
        ),
    ];
    let state_labels: Vec<String> = state_defs
        .iter()
        .map(|(_, name, tone, _)| format!(r#"<span class="{tone}">{}</span>"#, icon(name)))
        .collect();
    let state_hrefs: Vec<String> = state_defs
        .iter()
        .map(|(state, ..)| {
            query.toggle(
                query.alert == Some(*state),
                |q| q.alert = Some(*state),
                |q| q.alert = None,
            )
        })
        .collect();
    let state_chips: Vec<Chip> = state_defs
        .iter()
        .enumerate()
        .map(|(i, (state, _, _, name))| Chip {
            label_html: &state_labels[i],
            name,
            count: state_count(*state),
            href: &state_hrefs[i],
            on: query.alert == Some(*state),
        })
        .collect();
    let chips_html = format!(
        "{}{SEPARATOR}{}{}",
        chips(&group_chips),
        chips(&state_chips),
        future_chip("Unanswered: not enabled", "help-circle")
    );

    let mut body = String::new();
    if shown.is_empty() {
        body.push_str(r#"<p class="empty">No alert in this period.</p>"#);
    } else {
        body.push_str(r#"<table class="grid grid--sent"><colgroup><col class="w28"><col class="w152"><col class="w96"><col><col class="w128"></colgroup><thead class="visually-hidden"><tr><th scope="col">State</th><th scope="col">When</th><th scope="col">Group</th><th scope="col">Title</th><th scope="col">Sent by</th></tr></thead><tbody>"#);
        for row in &shown {
            alert_row(&mut body, view, sent, row);
        }
        body.push_str("</tbody></table>");
    }
    zone(
        &Zone {
            id: "sent-title",
            title: "Sent",
            count: &format!("{total}{}", if sent.truncated { "+" } else { "" }),
            chips_html: &chips_html,
            tools_html: "",
            class: " zone--sent",
            above_html: &graph(view, sent),
        },
        &body,
    )
}

fn alert_row(out: &mut String, view: &InboxView, sent: &SentData, row: &AlertRow) {
    let a = &row.summary;
    let state = AlertState::of(a);
    let open = sent.opened.as_ref().is_some_and(|d| d.summary.id == a.id);
    let href = if open {
        format!("{}#a-{}", view.query.href(|_| {}), a.id)
    } else {
        format!(
            "{}#a-{}",
            view.query.href(|q| q.open = Some(a.id.clone())),
            a.id
        )
    };
    let critical = view
        .groups
        .iter()
        .find(|g| g.id == a.group)
        .is_some_and(|g| g.critical);
    let _ = write!(
        out,
        r#"<tr id="a-{id}" class="{open_class}" data-alert="{id}" data-kind="{kind}" data-group="{group}" data-source="{source}" data-title="{title}" data-received="{received}" data-targets="{targets}" data-expired="{expired}" data-created="{created}"><td>{icon}</td><td>{when}</td><td>{name}{crit}</td><td><a class="rowLink" href="{href}" title="{title}"{expanded}>{title}</a></td><td>{origin}</td></tr>"#,
        id = html_escape(&a.id),
        open_class = if open { "is-selected" } else { "" },
        kind = a.kind.as_str(),
        group = html_escape(&a.group),
        source = html_escape(&a.source),
        title = html_escape(&a.title),
        received = a.received,
        targets = a.targets,
        expired = a.expired,
        created = a.created_at,
        icon = alert_icon(state, a),
        when = when(a.created_at),
        name = html_escape(group_name(view.groups, &a.group)),
        crit = if critical {
            format!(
                r#" <span class="crit" title="Critical group">{}</span>"#,
                icon("bell-ringing")
            )
        } else {
            String::new()
        },
        href = html_escape(&href),
        expanded = if open {
            r#" aria-expanded="true""#
        } else {
            r#" aria-expanded="false""#
        },
        origin = origin_cell(&row.origin),
    );
    if open {
        if let Some(detail) = &sent.opened {
            detail_row(out, view, detail, &href, &row.origin);
        }
    }
}

/// The deliveries of the opened alert: the phones that do not have it first, then a few that do.
fn detail_row(
    out: &mut String,
    view: &InboxView,
    detail: &AlertDetail,
    close: &str,
    origin: &Origin,
) {
    const WAITING_SHOWN: usize = 30;
    const RECEIVED_SHOWN: usize = 3;
    let a = &detail.summary;
    let (received, waiting): (Vec<_>, Vec<_>) = detail
        .deliveries
        .iter()
        .partition(|d| d.received_at.is_some());
    let _ = write!(
        out,
        r#"<tr class="adetail" data-detail="{}"><td colspan="5"><div class="adetail__head"><span><b>Sent by</b> {}</span><span><b>Id</b> <code>{}</code></span><span><b>Received</b> {} of {}</span><span class="grow"></span><a class="ib" href="{}" aria-label="Close the detail" title="Close">{}</a></div>"#,
        html_escape(&a.id),
        match origin {
            Origin::Watch => "the portal watch".to_string(),
            Origin::Account => "an account".to_string(),
            Origin::Key(name) => format!("key {}", html_escape(name)),
        },
        html_escape(&a.id),
        a.received,
        a.targets,
        html_escape(close),
        icon("x"),
    );
    if detail.deliveries.is_empty() {
        out.push_str(r#"<p class="empty">No phone had the group when the alert was made.</p>"#);
    } else {
        out.push_str(r#"<table class="grid grid--deliveries"><colgroup><col class="w28"><col><col class="w96"><col class="w64"><col class="w152"></colgroup><thead><tr><th scope="col"></th><th scope="col">Phone</th><th scope="col">Owner</th><th scope="col" class="r">Sent</th><th scope="col" class="r">Received</th></tr></thead><tbody>"#);
        let shown = waiting
            .iter()
            .take(WAITING_SHOWN)
            .chain(received.iter().take(RECEIVED_SHOWN));
        for d in shown {
            let state = if d.received_at.is_some() {
                st("st--ok", "circle-check", "Received")
            } else if d.device_revoked {
                st("st--off", "device-mobile-off", "Phone revoked")
            } else if d.expired {
                st(
                    "st--bad",
                    "alert-triangle",
                    "Expired: the phone never got it",
                )
            } else if d.attempts > 0 {
                st("st--warn", "clock", "Sent, not confirmed")
            } else {
                st("st--warn", "clock", "Waiting")
            };
            let _ = write!(
                out,
                r#"<tr data-device="{}"><td>{state}</td><td>{}</td><td class="muted">{}</td><td class="r num">{}</td><td class="r">{}</td></tr>"#,
                html_escape(&d.device_id),
                html_escape(&d.device_name),
                html_escape(&d.user_name),
                if d.attempts > 0 {
                    format!("{} x", d.attempts)
                } else {
                    "-".to_string()
                },
                when_opt(d.received_at),
            );
        }
        out.push_str("</tbody></table>");
        let hidden = waiting.len().saturating_sub(WAITING_SHOWN)
            + received.len().saturating_sub(RECEIVED_SHOWN);
        if hidden > 0 {
            let _ = write!(
                out,
                r#"<p class="adetail__more">{} more</p>"#,
                plural(hidden, "phone", "phones")
            );
        }
    }
    let _ = view;
    out.push_str("</td></tr>");
}

// Phones ---------------------------------------------------------------------------------------

fn device_state_icon(state: DeviceState) -> String {
    match state {
        DeviceState::Online => st("st--ok", "device-mobile", "Online"),
        DeviceState::Unreachable => st(
            "st--warn",
            "device-mobile",
            "Unreachable: not seen for a long time",
        ),
        DeviceState::Offline => st("st--off", "device-mobile", "Offline"),
        DeviceState::Revoked => st("st--off", "device-mobile-off", "Revoked"),
    }
}

fn phones_zone(view: &InboxView, data: &PhonesData) -> String {
    let query = view.query;
    let all = view.rights.phones;
    let owner_of = |row: &DeviceRow| row.device.user_name == view.login;
    let rows: Vec<&DeviceRow> = data
        .rows
        .iter()
        .filter(|r| query.phone.is_none_or(|s| s == r.state))
        .filter(|r| !query.pending || (r.device.pending > 0 && r.state != DeviceState::Revoked))
        .filter(|r| !query.mine || owner_of(r))
        .collect();
    let count_state = |s: DeviceState| data.rows.iter().filter(|r| r.state == s).count();
    let pending = data
        .rows
        .iter()
        .filter(|r| r.device.pending > 0 && r.state != DeviceState::Revoked)
        .count();
    let mine = data.rows.iter().filter(|r| owner_of(r)).count();

    let defs = [
        (DeviceState::Online, "device-mobile", "st--ok", "Online"),
        (
            DeviceState::Unreachable,
            "device-mobile",
            "st--warn",
            "Unreachable",
        ),
        (DeviceState::Offline, "device-mobile", "st--off", "Offline"),
        (
            DeviceState::Revoked,
            "device-mobile-off",
            "st--off",
            "Revoked",
        ),
    ];
    let labels: Vec<String> = defs
        .iter()
        .map(|(_, name, tone, _)| format!(r#"<span class="{tone}">{}</span>"#, icon(name)))
        .collect();
    let hrefs: Vec<String> = defs
        .iter()
        .map(|(state, ..)| {
            query.toggle(
                query.phone == Some(*state),
                |q| q.phone = Some(*state),
                |q| q.phone = None,
            )
        })
        .collect();
    let all_href = query.href(|q| q.phone = None);
    let mut state_chips = vec![Chip {
        label_html: "All",
        name: "",
        count: data.rows.len(),
        href: &all_href,
        on: query.phone.is_none(),
    }];
    for (i, (state, _, _, name)) in defs.iter().enumerate() {
        state_chips.push(Chip {
            label_html: &labels[i],
            name,
            count: count_state(*state),
            href: &hrefs[i],
            on: query.phone == Some(*state),
        });
    }
    let pending_label = format!(r#"<span class="st--warn">{}</span>"#, icon("clock"));
    let pending_href = query.toggle(query.pending, |q| q.pending = true, |q| q.pending = false);
    let mine_label = format!(r#"<span class="st--off">{}</span>"#, icon("user"));
    let mine_href = query.toggle(query.mine, |q| q.mine = true, |q| q.mine = false);
    let mut extra = vec![Chip {
        label_html: &pending_label,
        name: "Phones with pending alerts",
        count: pending,
        href: &pending_href,
        on: query.pending,
    }];
    if all {
        extra.push(Chip {
            label_html: &mine_label,
            name: "My phones",
            count: mine,
            href: &mine_href,
            on: query.mine,
        });
    }
    let extra_html = chips(&extra);
    let chips_html = format!(
        "{}{}{extra_html}",
        chips(&state_chips),
        if extra_html.is_empty() { "" } else { SEPARATOR }
    );

    // A row has a box when the account may revoke the phone.
    let may_revoke = |r: &DeviceRow| {
        !view.impersonated
            && r.state != DeviceState::Revoked
            && (data.revoke_all || (data.revoke_own && owner_of(r)))
    };
    let bulk = Bulk {
        id: "revoke-phones",
        action: REVOKE_PATH,
        field: "id",
        fields_html: &format!(
            r#"<input type="hidden" name="csrf" value="{}">"#,
            html_escape(view.csrf)
        ),
        verb: "Revoke",
        one: "phone",
        many: "phones",
        danger: true,
    };
    let any_box = data.rows.iter().any(may_revoke);
    let mut tools = String::new();
    if any_box {
        tools.push_str(&bulk_button(&bulk));
    }
    if view.rights.own_phones {
        tools.push_str(&icon_post(
            CODE_PATH,
            "Add a phone",
            "plus",
            &format!(
                r#"<input type="hidden" name="csrf" value="{}">"#,
                html_escape(view.csrf)
            ),
        ));
        if let Some(code) = &view.code {
            tools.push_str(&code_popover(code));
        }
    }

    let mut body = String::new();
    if any_box {
        body.push_str(&bulk_form(&bulk));
    }
    if data.rows.is_empty() {
        body.push_str(r#"<p class="empty">No phone yet.</p>"#);
    } else {
        body.push_str(r#"<table class="grid grid--phones"><colgroup>"#);
        if any_box {
            body.push_str(r#"<col class="w28">"#);
        }
        body.push_str(r#"<col class="w28"><col>"#);
        if all {
            body.push_str(r#"<col class="w88">"#);
        }
        body.push_str(
            r#"<col class="w56"><col class="w56"><col class="w28"></colgroup><thead><tr>"#,
        );
        if any_box {
            let _ = write!(body, r#"<th scope="col">{}</th>"#, bulk_all_box(&bulk));
        }
        body.push_str(r#"<th scope="col"><span class="visually-hidden">State</span></th><th scope="col">Phone</th>"#);
        if all {
            body.push_str(r#"<th scope="col">Owner</th>"#);
        }
        let _ = write!(
            body,
            r#"<th scope="col">App</th><th scope="col" class="r">Seen</th><th scope="col" class="r" title="Alerts pending">{}<span class="visually-hidden">Alerts pending</span></th></tr></thead><tbody>"#,
            icon("clock")
        );
        for row in &rows {
            phone_row(&mut body, view, &bulk, row, any_box, all, may_revoke(row));
        }
        body.push_str("</tbody></table>");
    }
    zone(
        &Zone {
            id: "phones-title",
            title: if all { "Phones" } else { "My phones" },
            count: &rows.len().to_string(),
            chips_html: &chips_html,
            tools_html: &tools,
            class: "",
            above_html: "",
        },
        &body,
    )
}

fn phone_row(
    out: &mut String,
    view: &InboxView,
    bulk: &Bulk,
    row: &DeviceRow,
    boxes: bool,
    owner: bool,
    box_here: bool,
) {
    let d = &row.device;
    let seen = row.live.as_ref().map(|l| l.last_seen_at).or(d.last_seen_at);
    let revoked = row.state == DeviceState::Revoked;
    let _ = write!(
        out,
        r#"<tr{class} data-device="{id}" data-name="{name}" data-login="{login}" data-state="{state}" data-pending="{pending}" data-groups="{groups}">"#,
        class = if revoked {
            r#" class="is-revoked""#
        } else {
            ""
        },
        groups = html_escape(&d.subscriptions.join(",")),
        id = html_escape(&d.id),
        name = html_escape(&d.name),
        login = html_escape(&d.user_name),
        state = row.state.word(),
        pending = d.pending,
    );
    if boxes {
        let _ = write!(
            out,
            "<td>{}</td>",
            if box_here {
                bulk_box(bulk, &d.id, &format!("Select {}", d.name))
            } else {
                String::new()
            }
        );
    }
    let groups = if d.subscriptions.is_empty() {
        "-".to_string()
    } else {
        d.subscriptions.join(", ")
    };
    let _ = write!(
        out,
        r#"<td>{}</td><td title="{}, groups: {}">{}</td>"#,
        device_state_icon(row.state),
        html_escape(&d.name),
        html_escape(&groups),
        html_escape(&d.name),
    );
    if owner {
        let _ = write!(
            out,
            r#"<td class="muted">{}</td>"#,
            html_escape(&d.user_name)
        );
    }
    let version = d.app_version.as_deref().filter(|v| !v.is_empty());
    let _ = write!(
        out,
        r#"<td class="muted">{}</td><td class="r muted"{}>{}</td><td class="r num{}"{}>{}</td></tr>"#,
        version.map_or("-".to_string(), html_escape),
        seen.map(|ms| format!(
            r#" title="{}""#,
            html_escape(&format!("{} UTC", crate::util::format_utc(ms / 1000)))
        ))
        .unwrap_or_default(),
        seen.map_or("-".to_string(), |ms| age(view.now_ms, ms)),
        if d.pending > 0 && !revoked {
            " st--warn"
        } else {
            " muted"
        },
        if d.pending > 0 {
            format!(r#" title="Alerts pending: {}""#, d.pending)
        } else {
            String::new()
        },
        if d.pending > 0 && !revoked {
            d.pending.to_string()
        } else {
            "-".to_string()
        },
    );
}

/// The popover that shows a new enrollment code. It is open at once and stays open until "Done".
/// The page shows the code one time: the code is not in any other page.
fn code_popover(code: &CodeData) -> String {
    let text = html_escape(code.code);
    format!(
        r##"<div class="pop pop--code" data-code-pop><div class="pop__menu pop__menu--end pop__menu--pad pop__menu--code"><dl class="facts"><div><dt>Server key</dt><dd><code>{}</code></dd></div><div><dt>Code valid until</dt><dd>{}</dd></div></dl><div class="codeBox"><textarea class="codeText" id="enroll-code" readonly rows="4" spellcheck="false" aria-label="Enrollment code" data-expires-ms="{}">{text}</textarea><button class="ib copy" type="button" data-copy="enroll-code" aria-label="Copy the code" title="Copy"><span class="copy__idle">{}</span><span class="copy__done">{}</span></button></div><div class="inline"><a class="button" href="{PAGE}" data-code-done>Done</a><a class="button button--ghost" href="{text}">Open in the app</a></div></div></div>"##,
        html_escape(code.fingerprint),
        time_tag(code.expires_at_ms / 1000, false),
        code.expires_at_ms,
        icon("copy"),
        icon("check"),
    )
}

// Connections ----------------------------------------------------------------------------------

fn connections_zone(view: &InboxView, data: &ConnData) -> String {
    let query = view.query;
    let rows: Vec<&ConnectionView> = data
        .rows
        .iter()
        .filter(|c| {
            query
                .conn
                .is_none_or(|s| (s == ConnState::Open) == c.disconnected_at.is_none())
        })
        .collect();
    let open = data
        .rows
        .iter()
        .filter(|c| c.disconnected_at.is_none())
        .count();
    let open_label = format!(r#"<span class="st--ok">{}</span>"#, icon("point"));
    let closed_label = format!(r#"<span class="st--off">{}</span>"#, icon("x"));
    let all_href = query.href(|q| q.conn = None);
    let open_href = query.toggle(
        query.conn == Some(ConnState::Open),
        |q| q.conn = Some(ConnState::Open),
        |q| q.conn = None,
    );
    let closed_href = query.toggle(
        query.conn == Some(ConnState::Closed),
        |q| q.conn = Some(ConnState::Closed),
        |q| q.conn = None,
    );
    let chips_html = chips(&[
        Chip {
            label_html: "All",
            name: "",
            count: data.rows.len(),
            href: &all_href,
            on: query.conn.is_none(),
        },
        Chip {
            label_html: &open_label,
            name: "Open connections",
            count: open,
            href: &open_href,
            on: query.conn == Some(ConnState::Open),
        },
        Chip {
            label_html: &closed_label,
            name: "Closed connections",
            count: data.rows.len() - open,
            href: &closed_href,
            on: query.conn == Some(ConnState::Closed),
        },
    ]);
    // The search is a form with the method GET. It keeps the other parameters of the page.
    let mut hidden = String::new();
    for (name, value) in query.pairs() {
        if name == "login" || name == "open" {
            continue;
        }
        let _ = write!(
            hidden,
            r#"<input type="hidden" name="{name}" value="{}">"#,
            html_escape(&value)
        );
    }
    let tools = format!(
        r#"<form class="searchBox" method="get" action="{PAGE}" role="search">{hidden}{}<input class="uiInput uiInput--small" type="search" name="login" value="{}" maxlength="64" placeholder="Login" aria-label="Filter by login" autocomplete="off"></form>"#,
        icon("search"),
        html_escape(&query.login),
    );
    let mut body = String::new();
    if rows.is_empty() {
        body.push_str(r#"<p class="empty">No connection in the log.</p>"#);
    } else {
        body.push_str(r#"<table class="grid grid--conn"><colgroup><col class="w28"><col class="w152"><col class="w88"><col><col class="w120"></colgroup><thead><tr><th scope="col"><span class="visually-hidden">State</span></th><th scope="col">Start</th><th scope="col">Login</th><th scope="col">Phone</th><th scope="col">End</th></tr></thead><tbody>"#);
        for c in &rows {
            let state = if c.disconnected_at.is_none() {
                st("st--ok", "point", "Open")
            } else {
                String::new()
            };
            let reason = c.close_reason.as_deref().unwrap_or("");
            let _ = write!(
                body,
                r#"<tr data-login="{login}" data-device="{id}" data-reason="{reason_data}"><td>{state}</td><td>{start}</td><td>{login}</td><td title="{name}, {ip}, app {app}">{name}</td><td class="muted"{end_title}>{end}</td></tr>"#,
                login = html_escape(&c.user_name),
                id = html_escape(&c.device_id),
                reason_data = html_escape(reason),
                start = when(c.connected_at),
                name = html_escape(&c.device_name),
                ip = html_escape(&c.ip),
                app = html_escape(&c.app_version),
                end_title = c
                    .disconnected_at
                    .map(|ms| format!(
                        r#" title="Ended {} UTC""#,
                        crate::util::format_utc(ms / 1000)
                    ))
                    .unwrap_or_default(),
                end = match (c.disconnected_at, reason) {
                    (None, _) => "open".to_string(),
                    (Some(_), "") => "ended".to_string(),
                    (Some(_), reason) => html_escape(reason),
                },
            );
        }
        body.push_str("</tbody></table>");
    }
    zone(
        &Zone {
            id: "connections-title",
            title: "Connections",
            count: &rows.len().to_string(),
            chips_html: &chips_html,
            tools_html: &tools,
            class: "",
            above_html: "",
        },
        &body,
    )
}

// The page -------------------------------------------------------------------------------------

pub fn inbox_page(frame: &Frame, view: &InboxView) -> String {
    let mut out = String::new();
    notice_html(&mut out, view.notice);
    let has_bar = view.sent.is_some() || view.docs.is_some() || !view.rights.send.is_empty();
    if has_bar {
        out.push_str(&bar(view));
    }
    let left = view.sent.as_ref().map(|sent| sent_zone(view, sent));
    let mut right = String::new();
    if let Some(phones) = &view.phones {
        right.push_str(&phones_zone(view, phones));
    }
    if let Some(connections) = &view.connections {
        right.push_str(&connections_zone(view, connections));
    }
    let solo = left.is_none() && view.connections.is_none();
    out.push_str(&split_zones(
        if solo { "solo" } else { "inbox" },
        left.as_deref(),
        &right,
    ));
    super::settings::fill_page(Tab::Inbox, frame, "Inbox", &out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::inbox::admin::{DeliveryView, DeviceView};

    const NOW: i64 = 1_700_000_000_000;

    fn frame() -> Frame<'static> {
        Frame {
            logout_token: "out",
            keys: true,
            inbox: true,
            superadmin: false,
        }
    }

    fn choice(id: &str, name: &str) -> GroupChoice {
        GroupChoice {
            id: id.into(),
            name: name.into(),
        }
    }

    fn all_rights() -> Rights {
        Rights {
            sent: true,
            phones: true,
            revoke: true,
            connections: true,
            own_phones: true,
            send: vec![choice("infra", "Infra <b>"), choice("crit", "Critical")],
        }
    }

    /// The account that holds `inbox.receive` only.
    fn receive_rights() -> Rights {
        Rights {
            own_phones: true,
            ..Rights::default()
        }
    }

    fn groups() -> Vec<GroupLine> {
        vec![
            GroupLine {
                id: "infra".into(),
                name: "Infra <b>".into(),
                critical: false,
            },
            GroupLine {
                id: "crit".into(),
                name: "Critical".into(),
                critical: true,
            },
        ]
    }

    fn summary(id: &str, group: &str, title: &str, received: i64, expired: i64) -> AlertSummary {
        AlertSummary {
            id: id.into(),
            kind: inbox_core::proto::Kind::Alert,
            group: group.into(),
            title: title.into(),
            source: "producer:ci".into(),
            created_at: NOW - 60_000,
            targets: 3,
            received,
            expired,
            missing: Vec::new(),
        }
    }

    fn row(summary: AlertSummary, origin: Origin) -> AlertRow {
        AlertRow { summary, origin }
    }

    fn device(id: &str, name: &str, login: &str, state: DeviceState, pending: i64) -> DeviceRow {
        DeviceRow {
            device: DeviceView {
                id: id.into(),
                name: name.into(),
                user_name: login.into(),
                enrolled_at: 1_000,
                enrolled_ip: "127.0.0.1".into(),
                app_version: Some("5.0.0".into()),
                last_connected_at: Some(NOW - 1_000),
                last_seen_at: Some(NOW - 18 * 60_000),
                last_ip: None,
                revoked_at: (state == DeviceState::Revoked).then_some(4_000),
                subscriptions: vec!["infra".into()],
                pending,
            },
            live: None,
            state,
        }
    }

    fn sent(opened: Option<AlertDetail>) -> SentData {
        SentData {
            alerts: vec![
                row(
                    summary("a1", "infra", "Disk <script>", 3, 0),
                    Origin::Key("ci-deploy".into()),
                ),
                row(summary("a2", "crit", "Inbox", 1, 0), Origin::Watch),
                row(summary("a3", "infra", "Old", 1, 2), Origin::Account),
            ],
            truncated: false,
            counts: vec![
                AlertCount {
                    bucket: 23,
                    group: "infra".into(),
                    count: 4,
                },
                AlertCount {
                    bucket: 23,
                    group: "crit".into(),
                    count: 1,
                },
            ],
            since_ms: NOW - Period::Day.millis(),
            opened,
        }
    }

    fn phones() -> PhonesData {
        PhonesData {
            rows: vec![
                device("d1", "Pixel <1>", "alice", DeviceState::Online, 0),
                device("d2", "Galaxy", "bob", DeviceState::Unreachable, 2),
                device("d3", "Old", "bob", DeviceState::Revoked, 0),
            ],
            revoke_all: true,
            revoke_own: true,
        }
    }

    fn conns() -> ConnData {
        ConnData {
            rows: vec![
                ConnectionView {
                    device_id: "d1".into(),
                    device_name: "Pixel".into(),
                    user_name: "alice".into(),
                    ip: "10.0.0.1".into(),
                    app_version: "5".into(),
                    connected_at: NOW - 5_000,
                    disconnected_at: None,
                    close_reason: None,
                },
                ConnectionView {
                    device_id: "d2".into(),
                    device_name: "Galaxy".into(),
                    user_name: "bob".into(),
                    ip: "10.0.0.2".into(),
                    app_version: "5".into(),
                    connected_at: NOW - 50_000,
                    disconnected_at: Some(NOW - 10_000),
                    close_reason: Some("heartbeat timeout".into()),
                },
            ],
        }
    }

    fn full_view<'a>(
        rights: &'a Rights,
        query: &'a Query,
        groups: &'a [GroupLine],
    ) -> InboxView<'a> {
        InboxView {
            rights,
            notice: None,
            csrf: "tok<",
            idempotency_key: "k1",
            query,
            now_ms: NOW,
            login: "alice",
            groups,
            sent: Some(sent(None)),
            phones: Some(phones()),
            connections: Some(conns()),
            docs: Some(DocsData {
                base: "https://portal.example.org".into(),
            }),
            code: None,
            impersonated: false,
        }
    }

    fn page(view: &InboxView) -> String {
        inbox_page(&frame(), view)
    }

    #[test]
    fn the_page_has_every_part_for_an_account_with_every_right() {
        let (rights, query, groups) = (all_rights(), Query::default(), groups());
        let html = page(&full_view(&rights, &query, &groups));
        for part in [
            r#"id="sent-title""#,
            r#"id="phones-title""#,
            r#"id="connections-title""#,
            "Send alert",
            r#"aria-label="Documentation for senders""#,
            r#"aria-label="Add a phone""#,
        ] {
            assert!(html.contains(part), "{part}");
        }
        // The text of a group and of a title is escaped.
        assert!(html.contains("Infra &lt;b&gt;") && !html.contains("Infra <b>"));
        assert!(html.contains("Disk &lt;script&gt;") && !html.contains("<script>"));
        assert!(html.contains("Pixel &lt;1&gt;"));
        // The page is one page: no sub-navigation and no link to an alert page.
        assert!(!html.contains("/auth/settings/inbox/alerts/"));
        assert!(!html.contains("segmented--compact"));
        assert_eq!(html.matches(r#"class="contentTabs__tab"#).count(), 2);
    }

    #[test]
    fn an_account_that_receives_only_has_its_phones_and_the_plus_button() {
        let (rights, query, groups) = (receive_rights(), Query::default(), groups());
        let mut view = full_view(&rights, &query, &groups);
        view.sent = None;
        view.connections = None;
        view.docs = None;
        view.phones = Some(PhonesData {
            rows: vec![device("d1", "Mine", "alice", DeviceState::Offline, 0)],
            revoke_all: false,
            revoke_own: true,
        });
        let html = page(&view);
        assert!(html.contains(r#"id="phones-title">My phones<"#), "{html}");
        assert!(html.contains(r#"aria-label="Add a phone""#));
        // Each part that the permissions do not give is absent, not grey.
        for absent in [
            r#"id="sent-title""#,
            r#"id="connections-title""#,
            "Send alert",
            "Documentation for senders",
            r#"class="tbar"#,
            "Period",
            "<th scope=\"col\">Owner</th>",
            "My phones</span>",
            "chart",
        ] {
            assert!(!html.contains(absent), "{absent}");
        }
        assert!(html.contains("split--solo"));
    }

    #[test]
    fn an_account_with_only_the_send_right_has_the_toolbar_and_nothing_else() {
        let rights = Rights {
            send: vec![choice("infra", "Infra")],
            ..Rights::default()
        };
        let (query, groups) = (Query::default(), groups());
        let mut view = full_view(&rights, &query, &groups);
        view.sent = None;
        view.phones = None;
        view.connections = None;
        let html = page(&view);
        assert!(html.contains("Send alert") && html.contains("Documentation for senders"));
        assert!(!html.contains(r#"id="sent-title""#) && !html.contains("Period"));
        assert!(!html.contains(r#"id="phones-title""#));
        assert!(!html.contains(r#"aria-label="Add a phone""#));
    }

    #[test]
    fn the_send_form_lists_only_the_groups_that_the_account_may_send_to() {
        let (rights, query, groups) = (all_rights(), Query::default(), groups());
        let html = page(&full_view(&rights, &query, &groups));
        assert!(html.contains(r#"action="/auth/settings/inbox/send""#));
        assert!(html.contains(r#"<option value="infra">Infra &lt;b&gt;</option>"#));
        assert!(html.contains(r#"<option value="crit">Critical</option>"#));
        assert!(html.contains(r#"name="key" value="k1""#));
        assert!(html.contains(r#"name="csrf" value="tok&lt;""#));
        // One group is no choice: no select.
        let one = Rights {
            send: vec![choice("infra", "Infra")],
            ..all_rights()
        };
        let html = page(&full_view(&one, &query, &groups));
        assert!(!html.contains("<select"), "{html}");
        assert!(html.contains(r#"<input type="hidden" name="group" value="infra">"#));
        // A session of "Log in as" sends nothing: the button is absent.
        let mut view = full_view(&rights, &query, &groups);
        view.impersonated = true;
        let html = page(&view);
        assert!(!html.contains("Send alert") && !html.contains("revoke-phones"));
        assert!(html.contains(r#"aria-label="Add a phone""#));
    }

    #[test]
    fn a_chip_is_a_link_with_a_query_parameter_and_a_chip_of_zero_is_absent() {
        let (rights, groups) = (all_rights(), groups());
        let query = Query::default();
        let html = page(&full_view(&rights, &query, &groups));
        assert!(
            html.contains(r#"href="/auth/settings/inbox?g=crit""#),
            "{html}"
        );
        assert!(html.contains(r#"href="/auth/settings/inbox?a=expired""#));
        assert!(html.contains(r#"href="/auth/settings/inbox?ps=revoked""#));
        assert!(html.contains(r#"href="/auth/settings/inbox?pp=1""#));
        assert!(html.contains(r#"href="/auth/settings/inbox?mine=1""#));
        assert!(html.contains(r#"href="/auth/settings/inbox?cs=open""#));
        // No phone is offline: the chip is absent.
        assert!(!html.contains("ps=offline"));
        // A chip that is on turns the filter off, and keeps the others.
        let query = Query {
            group: Some("crit".into()),
            period: Period::Week,
            ..Query::default()
        };
        let html = page(&full_view(&rights, &query, &groups));
        assert!(
            html.contains(r#"class="chip is-on" href="/auth/settings/inbox?p=7d""#),
            "{html}"
        );
        assert!(html.contains(r#"href="/auth/settings/inbox?p=7d&amp;g=infra""#));
        // The filter shows only the alerts of the group.
        assert!(html.contains(">Inbox<") && !html.contains(">Disk "));
    }

    #[test]
    fn the_kinds_of_a_later_step_are_dashed_chips_with_no_behaviour() {
        let (rights, query, groups) = (all_rights(), Query::default(), groups());
        let html = page(&full_view(&rights, &query, &groups));
        assert_eq!(html.matches("chip chip--future").count(), 3, "{html}");
        assert!(html.contains("Questions: not enabled"));
        assert!(html.contains("Approvals: not enabled"));
        assert!(html.contains("Unanswered: not enabled"));
        assert_eq!(
            html.matches("chip--future\" type=\"button\" disabled")
                .count(),
            3
        );
    }

    #[test]
    fn the_phones_have_boxes_and_one_bulk_button_and_no_button_in_a_row() {
        let (rights, query, groups) = (all_rights(), Query::default(), groups());
        let html = page(&full_view(&rights, &query, &groups));
        assert!(html.contains(r#"<form id="revoke-phones" class="bulk" method="post" action="/auth/settings/inbox/revoke">"#));
        assert!(html.contains(r#"name="id.d1" value="1""#));
        assert!(html.contains(r#"name="id.d2" value="1""#));
        assert!(
            !html.contains(r#"name="id.d3""#),
            "a revoked phone has no box"
        );
        assert!(html.contains(r#"data-bulk="revoke-phones""#));
        assert!(html.contains("Revoke the ticked phones?"));
        assert_eq!(html.matches("data-bulk=\"revoke-phones\"").count(), 1);
        assert!(!html.contains("Confirm: revoke"));
        // Without the right to revoke and without phones of its own: no box, no button.
        let mut view = full_view(&rights, &query, &groups);
        view.phones = Some(PhonesData {
            revoke_all: false,
            revoke_own: false,
            ..phones()
        });
        let html = page(&view);
        assert!(!html.contains("revoke-phones") && !html.contains("bulk__box"));
        // An account with its own phones only may revoke these and no other.
        let mut view = full_view(&rights, &query, &groups);
        view.phones = Some(PhonesData {
            revoke_all: false,
            revoke_own: true,
            ..phones()
        });
        let html = page(&view);
        assert!(html.contains(r#"name="id.d1""#) && !html.contains(r#"name="id.d2""#));
    }

    #[test]
    fn a_row_opens_its_detail_in_place_with_a_close_link() {
        let (rights, groups) = (all_rights(), groups());
        let query = Query::default();
        let html = page(&full_view(&rights, &query, &groups));
        // The link of a row opens it, and keeps its place in the list with an anchor.
        assert!(
            html.contains(r##"href="/auth/settings/inbox?open=a1#a-a1""##),
            "{html}"
        );
        assert!(!html.contains("data-detail"));
        let detail = AlertDetail {
            summary: summary("a1", "infra", "Disk", 1, 0),
            deliveries: vec![
                DeliveryView {
                    device_id: "d1".into(),
                    device_name: "Pixel".into(),
                    user_name: "alice".into(),
                    attempts: 1,
                    first_sent_at: Some(NOW),
                    last_sent_at: None,
                    received_at: Some(NOW + 1_000),
                    expired: false,
                    device_revoked: false,
                },
                DeliveryView {
                    device_id: "d2".into(),
                    device_name: "Galaxy".into(),
                    user_name: "bob".into(),
                    attempts: 3,
                    first_sent_at: Some(NOW),
                    last_sent_at: Some(NOW),
                    received_at: None,
                    expired: true,
                    device_revoked: false,
                },
            ],
        };
        let mut view = full_view(&rights, &query, &groups);
        view.sent = Some(sent(Some(detail)));
        let open = Query {
            open: Some("a1".into()),
            ..Query::default()
        };
        view.query = &open;
        let html = page(&view);
        assert!(
            html.contains(r#"<tr class="adetail" data-detail="a1">"#),
            "{html}"
        );
        assert!(
            html.contains(r#"data-device="d2""#)
                && html.contains("Expired: the phone never got it")
        );
        assert!(html.contains(r##"aria-label="Close the detail""##));
        assert!(
            html.contains(r##"href="/auth/settings/inbox#a-a1""##),
            "close keeps the place"
        );
        assert_eq!(html.matches("data-detail=").count(), 1);
        assert!(
            html.contains("Sent by</b> key ci-deploy")
                || html.contains("Sent by</b> an account")
                || html.contains("Sent by</b>")
        );
    }

    #[test]
    fn the_period_picker_is_a_popover_of_links() {
        let (rights, query, groups) = (all_rights(), Query::default(), groups());
        let html = page(&full_view(&rights, &query, &groups));
        assert!(
            html.contains(r#"<summary class="pick" aria-label="Period" title="Period">"#),
            "{html}"
        );
        for period in ["p=1h", "p=6h", "p=7d", "p=30d"] {
            assert!(
                html.contains(&format!("href=\"/auth/settings/inbox?{period}\"")),
                "{period}"
            );
        }
        assert!(html.contains("Last 24 h") && html.contains(r#"aria-current="true""#));
    }

    #[test]
    fn the_connections_have_a_search_form_that_keeps_the_filters() {
        let (rights, groups) = (all_rights(), groups());
        let query = Query {
            login: "al\"ice".into(),
            conn: Some(ConnState::Open),
            period: Period::Hour,
            ..Query::default()
        };
        let html = page(&full_view(&rights, &query, &groups));
        assert!(html.contains(
            r#"<form class="searchBox" method="get" action="/auth/settings/inbox" role="search">"#
        ));
        assert!(
            html.contains(r#"name="cs" value="open""#) && html.contains(r#"name="p" value="1h""#)
        );
        assert!(html.contains(r#"value="al&quot;ice""#));
        assert!(html.contains(r#"data-reason="""#));
        // The filter "open" keeps the open connection only.
        assert!(
            html.contains(r#"<tr data-login="alice""#) && !html.contains(r#"<tr data-login="bob""#)
        );
    }

    #[test]
    fn the_plus_button_is_a_form_and_the_code_shows_in_a_popover_with_a_done_link() {
        let (rights, query, groups) = (all_rights(), Query::default(), groups());
        let html = page(&full_view(&rights, &query, &groups));
        // The plus button makes the code. There is no second button that makes one.
        assert!(html
            .contains(r#"<form class="inline" method="post" action="/auth/settings/inbox/code">"#));
        assert_eq!(html.matches("Make a code").count(), 0);
        assert!(!html.contains("data-code-pop"));
        let mut view = full_view(&rights, &query, &groups);
        view.code = Some(CodeData {
            code: "portal-inbox://enroll?s=h%3A1&k=a&c=b",
            fingerprint: "0a1b2c3d",
            expires_at_ms: NOW,
        });
        let html = page(&view);
        assert!(html.contains("data-code-pop"));
        assert!(
            html.contains(r#"id="enroll-code""#)
                && html.contains(">portal-inbox://enroll?s=h%3A1&amp;k=a&amp;c=b</textarea>")
        );
        assert!(html.contains(r#"href="portal-inbox://enroll?s=h%3A1&amp;k=a&amp;c=b""#));
        assert!(html.contains(r#"data-copy="enroll-code""#));
        assert!(html.contains("<code>0a1b2c3d</code>"));
        assert!(html
            .contains(r#"<a class="button" href="/auth/settings/inbox" data-code-done>Done</a>"#));
        assert!(html.contains("2023-11-14"));
        // The popover is not inside a scroller: it sits in the head of the zone.
        let head = html.find(r#"id="phones-title""#).unwrap();
        let pop = html.find("data-code-pop").unwrap();
        let scroll = html[head..].find(r#"<div class="scroll">"#).unwrap() + head;
        assert!(pop < scroll);
    }

    #[test]
    fn the_graph_counts_each_bar_and_puts_the_critical_groups_on_top() {
        let (rights, query, groups) = (all_rights(), Query::default(), groups());
        let html = page(&full_view(&rights, &query, &groups));
        assert!(
            html.contains(r#"<svg class="chart" viewBox="0 0 240 52""#),
            "{html}"
        );
        assert!(html.contains("4 alerts</title>") && html.contains("1 critical</title>"));
        assert!(html.contains("5 in total"));
        // The group filter keeps one group in the graph.
        let query = Query {
            group: Some("crit".into()),
            ..Query::default()
        };
        let html = page(&full_view(&rights, &query, &groups));
        assert!(html.contains("1 in total") && !html.contains("4 alerts</title>"));
        // Each period has 12 to 30 bars.
        for period in Period::ALL {
            let bars = period.millis() / period.bucket_millis();
            assert!((12..=30).contains(&bars), "{period:?}: {bars}");
        }
    }

    #[test]
    fn the_origin_comes_from_the_source_of_the_alert() {
        let resolve = |id: &str| (id == "k1").then(|| "ci-deploy".to_string());
        assert_eq!(Origin::of("portal", &resolve), Origin::Watch);
        assert_eq!(Origin::of("admin", &resolve), Origin::Account);
        assert_eq!(
            Origin::of("producer:local", &resolve),
            Origin::Key("local".into())
        );
        assert_eq!(
            Origin::of("webhook:key:k1", &resolve),
            Origin::Key("ci-deploy".into())
        );
        assert_eq!(
            Origin::of("notify:key:gone", &resolve),
            Origin::Key("API key".into())
        );
    }

    #[test]
    fn the_query_is_strict_and_the_address_leaves_out_the_defaults() {
        let pairs = |text: &str| -> Vec<(String, String)> {
            text.split('&')
                .filter(|p| !p.is_empty())
                .map(|p| {
                    let (a, b) = p.split_once('=').unwrap();
                    (a.to_string(), b.to_string())
                })
                .collect()
        };
        let query = Query::parse(&pairs(
            "p=7d&g=infra&a=wait&ps=online&pp=1&mine=1&cs=closed&login=bob&s=sent",
        ))
        .unwrap();
        assert_eq!(query.period, Period::Week);
        assert_eq!(query.alert, Some(AlertState::Pending));
        assert_eq!(
            query.href(|_| {}),
            "/auth/settings/inbox?p=7d&g=infra&a=wait&ps=online&pp=1&mine=1&cs=closed&login=bob"
        );
        assert_eq!(Query::default().href(|_| {}), PAGE);
        for bad in [
            "p=2d",
            "x=1",
            "a=what",
            "ps=gone",
            "pp=0",
            "cs=maybe",
            "open=zz",
            "g=",
            "p=1h&p=6h",
        ] {
            assert!(Query::parse(&pairs(bad)).is_none(), "{bad}");
        }
    }

    #[test]
    fn the_page_uses_no_inline_style_and_no_inline_script() {
        let (rights, query, groups) = (all_rights(), Query::default(), groups());
        let mut view = full_view(&rights, &query, &groups);
        view.notice = notice_for("sent");
        view.code = Some(CodeData {
            code: "portal-inbox://x",
            fingerprint: "ab",
            expires_at_ms: NOW,
        });
        let html = page(&view);
        assert!(!html.contains(" style="), "{html}");
        assert!(!html.contains("<style") && !html.contains("onclick"));
        assert!(!html.contains("<script>"), "the scripts have a src");
    }

    #[test]
    fn each_notice_code_has_a_text_and_an_unknown_code_has_none() {
        for code in [
            "sent",
            "duplicate",
            "revoked",
            "no_title",
            "bad_title",
            "bad_group",
            "unknown_device",
            "too_many_codes",
            "limited",
            "failed",
        ] {
            assert!(notice_for(code).is_some(), "{code}");
        }
        assert!(notice_for("<script>").is_none());
        assert!(notice_for("sent").unwrap().ok && !notice_for("failed").unwrap().ok);
    }
}
