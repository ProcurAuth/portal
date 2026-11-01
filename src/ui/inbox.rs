//! The pages of the tab "Inbox": the overview with the manual send of an alert, the phones, the
//! connection log, the phones of one account with the enrollment code, and the documentation.
//!
//! Each page shows only what the rights of the account allow. The page asks nothing of the browser
//! but HTML and the small script of the settings pages. Everything works without script. The
//! script only gives the Copy button. A form is a form: a POST with an anti-CSRF token, and then a
//! redirect with a code of the outcome. Everything that comes from outside is escaped. This
//! includes names, titles and logins.

use std::fmt::Write as _;

use super::settings::{notice_html, page_shell, part_head, plural, Frame, Notice, Tab};
use crate::inbox::admin::{AlertDetail, AlertSummary, ConnectionView, DeviceRow, DeviceState};
use crate::inbox_actions::PAGE;
use crate::util::{html_escape, time_tag};

pub const OVERVIEW_PATH: &str = PAGE;
pub const DEVICES_PATH: &str = "/auth/settings/inbox/devices";
pub const CONNECTIONS_PATH: &str = "/auth/settings/inbox/connections";
pub const PHONES_PATH: &str = "/auth/settings/inbox/phones";
pub const DOC_PATH: &str = "/auth/settings/inbox/doc";
/// The page of one alert is this prefix, then the id of the alert.
pub const ALERT_PATH_PREFIX: &str = "/auth/settings/inbox/alerts/";
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

/// What the account may do in the tab. The page of the account has only the sections that it may
/// open.
#[derive(Clone, Debug, Default)]
pub struct Rights {
    /// The overview: `alert.read`, `device.read`, `connection.read` or `alert.send`.
    pub overview: bool,
    /// `device.read`.
    pub devices: bool,
    /// `device.revoke` together with `device.read`.
    pub revoke: bool,
    /// `connection.read`.
    pub connections: bool,
    /// `group.subscribe` on one group at least: the account has its own phones.
    pub phones: bool,
    /// The groups with `alert.send`.
    pub send: Vec<GroupChoice>,
}

impl Rights {
    /// Whether the account has any right in the tab.
    pub fn any(&self) -> bool {
        self.overview || self.devices || self.connections || self.phones
    }

    fn sections(&self) -> Vec<Section> {
        let mut out = Vec::new();
        if self.overview {
            out.push(Section::Overview);
        }
        if self.devices {
            out.push(Section::Devices);
        }
        if self.connections {
            out.push(Section::Connections);
        }
        if self.phones {
            out.push(Section::Phones);
        }
        out.push(Section::Doc);
        out
    }
}

/// The sections of the tab.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Section {
    Overview,
    Devices,
    Connections,
    Phones,
    Doc,
}

impl Section {
    fn path(self) -> &'static str {
        match self {
            Self::Overview => OVERVIEW_PATH,
            Self::Devices => DEVICES_PATH,
            Self::Connections => CONNECTIONS_PATH,
            Self::Phones => PHONES_PATH,
            Self::Doc => DOC_PATH,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Overview => "Overview",
            Self::Devices => "Phones",
            Self::Connections => "Connections",
            Self::Phones => "My phones",
            Self::Doc => "Documentation",
        }
    }
}

/// The text of the outcome of a form, from a fixed list.
pub fn notice_for(code: &str) -> Option<Notice> {
    let (text, ok) = match code {
        "sent" => ("The alert is sent.", true),
        "duplicate" => ("This alert was sent before. Nothing is sent again.", true),
        "revoked" => ("The phone is revoked. It gets no alert after this.", true),
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
    Some(Notice {
        text,
        ok,
        add_form: false,
    })
}

/// The head of a page of the tab: the title, a count, and the switch between the sections.
fn head(out: &mut String, rights: &Rights, section: Section, id: &str, title: &str, count: &str) {
    let mut switch = String::from(
        r#"<div class="segmented segmented--compact" role="group" aria-label="Section">"#,
    );
    for each in rights.sections() {
        let current = each == section;
        let _ = write!(
            switch,
            r#"<a class="segmented__option{}" href="{}"{}>{}</a>"#,
            if current { " is-active" } else { "" },
            each.path(),
            if current {
                r#" aria-current="true""#
            } else {
                ""
            },
            each.label()
        );
    }
    switch.push_str("</div>");
    part_head(out, id, title, count, &switch);
}

fn when(ms: i64) -> String {
    time_tag(ms / 1000, true)
}

fn when_opt(ms: Option<i64>) -> String {
    match ms {
        Some(ms) => when(ms),
        None => r#"<span class="muted">never</span>"#.to_string(),
    }
}

fn muted_none() -> &'static str {
    r#"<span class="muted">none</span>"#
}

fn state_badge(state: DeviceState) -> String {
    let class = match state {
        DeviceState::Online => " badge--ok",
        DeviceState::Offline => "",
        DeviceState::Unreachable => " badge--warn",
        DeviceState::Revoked => " badge--danger",
    };
    format!(r#"<span class="badge{class}">{}</span>"#, state.word())
}

fn name_of<'a>(groups: &'a [GroupLine], id: &'a str) -> &'a str {
    groups
        .iter()
        .find(|g| g.id == id)
        .map_or(id, |g| g.name.as_str())
}

// Overview ------------------------------------------------------------------------------------

/// The figures of the delivery.
pub struct Stats {
    pub enrolled: usize,
    pub online: usize,
    pub unreachable: usize,
    pub pending: i64,
    pub oldest_pending_ms: i64,
}

/// One group of the file.
#[derive(Clone, Debug)]
pub struct GroupLine {
    pub id: String,
    pub name: String,
    pub description: String,
    pub critical: bool,
    pub subscribed: usize,
}

pub struct OverviewView<'a> {
    pub notice: Option<Notice>,
    pub csrf: &'a str,
    /// The figures. `None`: the account has no `device.read`.
    pub stats: Option<Stats>,
    pub groups: Vec<GroupLine>,
    /// The last alerts of the groups that the account may read. `None`: no such group.
    pub alerts: Option<Vec<AlertSummary>>,
    /// The key of the form: a double click sends one alert.
    pub idempotency_key: &'a str,
}

pub fn overview_page(frame: &Frame, rights: &Rights, view: &OverviewView<'_>) -> String {
    let mut out = String::new();
    notice_html(&mut out, view.notice);
    out.push_str(r#"<section class="pagePart" aria-labelledby="inbox-title">"#);
    head(
        &mut out,
        rights,
        Section::Overview,
        "inbox-title",
        "Overview",
        &plural(view.groups.len(), "group", "groups"),
    );
    if let Some(stats) = &view.stats {
        let oldest = if stats.pending > 0 {
            format!(
                "{}, the oldest waits {}",
                stats.pending,
                crate::util::format_duration(stats.oldest_pending_ms / 1000)
            )
        } else {
            "0".to_string()
        };
        let _ = write!(
            out,
            r#"<dl class="stats"><div class="stat"><dt class="stat__label">Phones</dt><dd class="stat__value">{}</dd></div><div class="stat"><dt class="stat__label">Online</dt><dd class="stat__value">{}</dd></div><div class="stat"><dt class="stat__label">Unreachable</dt><dd class="stat__value{}">{}</dd></div><div class="stat"><dt class="stat__label">Alerts waiting</dt><dd class="stat__value">{}</dd></div></dl>"#,
            stats.enrolled,
            stats.online,
            if stats.unreachable > 0 {
                " stat__value--danger"
            } else {
                ""
            },
            stats.unreachable,
            oldest,
        );
    }
    out.push_str(
        r#"<div class="dataTableWrap"><table class="dataTable stack groupsTable"><thead><tr><th scope="col">Group</th><th scope="col">Id</th><th scope="col" class="num">Phones</th></tr></thead><tbody>"#,
    );
    for group in &view.groups {
        let _ = write!(
            out,
            r#"<tr data-group="{id}"><th scope="row" data-label="Group" title="{description}">{name}{critical}</th><td data-label="Id"><code>{id}</code></td><td class="num" data-label="Phones">{n}</td></tr>"#,
            id = html_escape(&group.id),
            name = html_escape(&group.name),
            description = html_escape(&group.description),
            critical = if group.critical {
                r#" <span class="badge badge--warn" title="A limit never applies to a critical group.">critical</span>"#
            } else {
                ""
            },
            n = group.subscribed,
        );
    }
    out.push_str("</tbody></table></div></section>");

    if !rights.send.is_empty() {
        send_form(&mut out, rights, view);
    }
    if let Some(alerts) = &view.alerts {
        out.push_str(r#"<section class="pagePart" aria-labelledby="alerts-title">"#);
        part_head(
            &mut out,
            "alerts-title",
            "Last alerts",
            &plural(alerts.len(), "alert", "alerts"),
            "",
        );
        if alerts.is_empty() {
            out.push_str(
                r#"<div class="uiState uiState--compact"><p class="uiState__body">No alert yet.</p></div>"#,
            );
        } else {
            out.push_str(
                r#"<div class="dataTableWrap"><table class="dataTable stack alertsTable"><thead><tr><th scope="col" class="num">When</th><th scope="col">Group</th><th scope="col">Title</th><th scope="col" class="num">Received</th><th scope="col" class="num">Expired</th></tr></thead><tbody>"#,
            );
            for alert in alerts {
                alert_row(&mut out, &view.groups, alert);
            }
            out.push_str("</tbody></table></div>");
        }
        out.push_str("</section>");
    }
    page_shell(Tab::Inbox, frame, "Inbox", &out)
}

fn alert_row(out: &mut String, groups: &[GroupLine], alert: &AlertSummary) {
    let _ = write!(
        out,
        r#"<tr data-alert="{id}" data-group="{group}" data-received="{received}" data-targets="{targets}" data-expired="{expired}" data-created="{created}"><td class="num" data-label="When">{when}</td><td data-label="Group">{name}</td><td data-label="Title"><a href="{ALERT_PATH_PREFIX}{id}">{title}</a></td><td class="num" data-label="Received">{received} of {targets}</td><td class="num{danger}" data-label="Expired">{expired}</td></tr>"#,
        id = html_escape(&alert.id),
        group = html_escape(&alert.group),
        when = when(alert.created_at),
        name = html_escape(name_of(groups, &alert.group)),
        title = html_escape(&alert.title),
        received = alert.received,
        targets = alert.targets,
        expired = alert.expired,
        danger = if alert.expired > 0 {
            " num--danger"
        } else {
            ""
        },
        created = alert.created_at,
    );
}

fn send_form(out: &mut String, rights: &Rights, view: &OverviewView<'_>) {
    out.push_str(r#"<section class="pagePart" aria-labelledby="send-title">"#);
    part_head(out, "send-title", "Send an alert", "", "");
    let _ = write!(
        out,
        r#"<form method="post" action="{SEND_PATH}" class="uiForm sendForm" data-gate><input type="hidden" name="csrf" value="{}"><input type="hidden" name="key" value="{}"><div class="sendForm__row"><div class="uiField uiField--narrow"><label class="uiField__label" for="send-group">Group</label><select class="uiInput" id="send-group" name="group" required>"#,
        html_escape(view.csrf),
        html_escape(view.idempotency_key),
    );
    for choice in &rights.send {
        let _ = write!(
            out,
            r#"<option value="{}">{}</option>"#,
            html_escape(&choice.id),
            html_escape(&choice.name)
        );
    }
    let _ = write!(
        out,
        r#"</select></div><div class="uiField"><label class="uiField__label" for="send-title-input">Title</label><input class="uiInput" type="text" id="send-title-input" name="title" maxlength="{MAX_TITLE}" autocomplete="off" required></div><button class="button button--primary" type="submit">Send</button></div><p class="uiField__hint">The title has at most {MAX_TITLE} characters, on one line. Each phone of the group gets it.</p></form></section>"#
    );
}

// One alert -----------------------------------------------------------------------------------

pub struct AlertView<'a> {
    pub notice: Option<Notice>,
    pub detail: &'a AlertDetail,
    pub group_name: &'a str,
}

pub fn alert_page(frame: &Frame, view: &AlertView<'_>) -> String {
    let summary = &view.detail.summary;
    let mut out = String::new();
    notice_html(&mut out, view.notice);
    out.push_str(r#"<section class="pagePart" aria-labelledby="inbox-title">"#);
    let _ = write!(
        out,
        r##"<div class="pagePart__head"><a class="pageBack" href="{OVERVIEW_PATH}" aria-label="Back to the overview" title="Back to the overview"><svg class="icon icon--lg" aria-hidden="true"><use href="#i-arrow-left"/></svg></a><h2 class="pagePart__title" id="inbox-title">{}</h2><span class="pagePart__count">{}</span></div>"##,
        html_escape(&summary.title),
        plural(summary.targets as usize, "phone", "phones"),
    );
    let _ = write!(
        out,
        r#"<dl class="stats"><div class="stat"><dt class="stat__label">Group</dt><dd class="stat__value">{}</dd></div><div class="stat"><dt class="stat__label">Made</dt><dd class="stat__value">{}</dd></div><div class="stat"><dt class="stat__label">Received</dt><dd class="stat__value">{} of {}</dd></div><div class="stat"><dt class="stat__label">Expired</dt><dd class="stat__value{}">{}</dd></div><div class="stat"><dt class="stat__label">Source</dt><dd class="stat__value">{}</dd></div></dl>"#,
        html_escape(view.group_name),
        when(summary.created_at),
        summary.received,
        summary.targets,
        if summary.expired > 0 {
            " stat__value--danger"
        } else {
            ""
        },
        summary.expired,
        html_escape(&summary.source),
    );
    if view.detail.deliveries.is_empty() {
        out.push_str(
            r#"<div class="uiState uiState--compact"><p class="uiState__body">No phone was subscribed when the alert was made.</p></div>"#,
        );
    } else {
        out.push_str(
            r#"<div class="dataTableWrap"><table class="dataTable stack deliveriesTable"><thead><tr><th scope="col">Phone</th><th scope="col">Owner</th><th scope="col" class="num">Sent</th><th scope="col" class="num">Received</th><th scope="col">State</th></tr></thead><tbody>"#,
        );
        for d in &view.detail.deliveries {
            let state = if d.received_at.is_some() {
                r#"<span class="badge">received</span>"#
            } else if d.device_revoked {
                r#"<span class="badge badge--danger">phone revoked</span>"#
            } else if d.expired {
                r#"<span class="badge badge--danger">expired</span>"#
            } else if d.attempts > 0 {
                r#"<span class="badge badge--warn">sent, not confirmed</span>"#
            } else {
                r#"<span class="badge badge--warn">waiting</span>"#
            };
            let _ = write!(
                out,
                r#"<tr data-device="{id}"><th scope="row" data-label="Phone">{name}</th><td data-label="Owner">{owner}</td><td class="num" data-label="Sent">{sent}</td><td class="num" data-label="Received">{received}</td><td data-label="State">{state}</td></tr>"#,
                id = html_escape(&d.device_id),
                name = html_escape(&d.device_name),
                owner = html_escape(&d.user_name),
                sent = if d.attempts > 0 {
                    format!("{} x, {}", d.attempts, when_opt(d.first_sent_at))
                } else {
                    r#"<span class="muted">not yet</span>"#.to_string()
                },
                received = when_opt(d.received_at),
            );
        }
        out.push_str("</tbody></table></div>");
    }
    out.push_str("</section>");
    page_shell(Tab::Inbox, frame, "Alert", &out)
}

// Phones --------------------------------------------------------------------------------------

fn revoke_form(out: &mut String, csrf: &str, device: &DeviceRow, back: &str, who: &str) {
    let _ = write!(
        out,
        r#"<details class="confirm"><summary class="button button--small button--quiet-danger">Revoke</summary><form class="confirm__panel" method="post" action="{REVOKE_PATH}"><input type="hidden" name="csrf" value="{}"><input type="hidden" name="id" value="{}"><input type="hidden" name="back" value="{}"><p>{} gets no alert after this. The phone must enroll again with a new code.</p><button class="button button--danger" type="submit">Confirm: revoke {}</button></form></details>"#,
        html_escape(csrf),
        html_escape(&device.device.id),
        html_escape(back),
        html_escape(who),
        html_escape(&device.device.name),
    );
}

fn device_row(out: &mut String, view: &DevicesView<'_>, row: &DeviceRow, show_owner: bool) {
    let d = &row.device;
    let _ = write!(
        out,
        r#"<tr data-device="{id}" data-name="{name}" data-login="{login}" data-state="{state}" data-pending="{pending}" data-groups="{groups}"><th scope="row" data-label="Phone">{name}</th>"#,
        groups = html_escape(&d.subscriptions.join(",")),
        id = html_escape(&d.id),
        name = html_escape(&d.name),
        login = html_escape(&d.user_name),
        state = row.state.word(),
        pending = d.pending,
    );
    if show_owner {
        let _ = write!(
            out,
            r#"<td data-label="Owner">{}</td>"#,
            html_escape(&d.user_name)
        );
    }
    let subs = if d.subscriptions.is_empty() {
        muted_none().to_string()
    } else {
        d.subscriptions
            .iter()
            .map(|g| html_escape(g))
            .collect::<Vec<_>>()
            .join(", ")
    };
    let _ = write!(
        out,
        r#"<td data-label="State">{}</td><td data-label="App">{}</td><td class="num" data-label="Last seen">{}</td><td data-label="Groups">{}</td><td class="num{}" data-label="Waiting">{}</td><td data-label="Actions" class="keyActions">"#,
        state_badge(row.state),
        match &d.app_version {
            Some(v) if !v.is_empty() => html_escape(v),
            _ => muted_none().to_string(),
        },
        when_opt(row.live.as_ref().map(|l| l.last_seen_at).or(d.last_seen_at)),
        subs,
        if d.pending > 0 && row.state != DeviceState::Online {
            " num--danger"
        } else {
            ""
        },
        d.pending,
    );
    if view.can_revoke && d.revoked_at.is_none() {
        revoke_form(out, view.csrf, row, view.back, &d.user_name);
    }
    out.push_str("</td></tr>");
}

pub struct DevicesView<'a> {
    pub notice: Option<Notice>,
    pub csrf: &'a str,
    pub rows: Vec<DeviceRow>,
    pub can_revoke: bool,
    /// The address that the revoke form returns to.
    pub back: &'a str,
}

fn devices_table(out: &mut String, view: &DevicesView<'_>, show_owner: bool) {
    let _ = write!(
        out,
        r#"<div class="dataTableWrap"><table class="dataTable stack devicesTable"><thead><tr><th scope="col">Phone</th>{}<th scope="col">State</th><th scope="col">App</th><th scope="col" class="num">Last seen</th><th scope="col">Groups</th><th scope="col" class="num">Waiting</th><th scope="col"><span class="visually-hidden">Actions</span></th></tr></thead><tbody>"#,
        if show_owner {
            r#"<th scope="col">Owner</th>"#
        } else {
            ""
        }
    );
    for row in &view.rows {
        device_row(out, view, row, show_owner);
    }
    out.push_str("</tbody></table></div>");
}

pub fn devices_page(frame: &Frame, rights: &Rights, view: &DevicesView<'_>) -> String {
    let mut out = String::new();
    notice_html(&mut out, view.notice);
    out.push_str(r#"<section class="pagePart" aria-labelledby="inbox-title">"#);
    head(
        &mut out,
        rights,
        Section::Devices,
        "inbox-title",
        "Phones",
        &plural(view.rows.len(), "phone", "phones"),
    );
    if view.rows.is_empty() {
        out.push_str(
            r#"<div class="uiState uiState--compact"><p class="uiState__body">No phone is enrolled yet.</p></div>"#,
        );
    } else {
        devices_table(&mut out, view, true);
    }
    out.push_str("</section>");
    page_shell(Tab::Inbox, frame, "Phones", &out)
}

pub struct PhonesView<'a> {
    pub devices: DevicesView<'a>,
    /// The login of the account.
    /// The account may make a code: it browses as itself.
    pub can_add: bool,
}

pub fn phones_page(frame: &Frame, rights: &Rights, view: &PhonesView<'_>) -> String {
    let mut out = String::new();
    notice_html(&mut out, view.devices.notice);
    out.push_str(r#"<section class="pagePart" aria-labelledby="inbox-title">"#);
    head(
        &mut out,
        rights,
        Section::Phones,
        "inbox-title",
        "My phones",
        &plural(view.devices.rows.len(), "phone", "phones"),
    );
    if view.devices.rows.is_empty() {
        out.push_str(
            r#"<div class="uiState uiState--compact"><p class="uiState__body">No phone yet. Add one below.</p></div>"#,
        );
    } else {
        devices_table(&mut out, &view.devices, false);
    }
    out.push_str("</section>");
    if view.can_add {
        out.push_str(r#"<section class="pagePart" aria-labelledby="add-title">"#);
        part_head(&mut out, "add-title", "Add a phone", "", "");
        let _ = write!(
            out,
            r#"<p class="part__lede">The code works one time. Type it in the app Inbox on the phone, or open the link on the phone. The portal shows the code one time.</p><form method="post" action="{CODE_PATH}" class="uiForm"><input type="hidden" name="csrf" value="{}"><div class="formActions"><button class="button button--primary" type="submit">Make a code</button></div></form></section>"#,
            html_escape(view.devices.csrf),
        );
    }
    page_shell(Tab::Inbox, frame, "My phones", &out)
}

/// The page that shows a new code one time.
pub struct CodeView<'a> {
    pub code: &'a str,
    pub fingerprint: &'a str,
    pub expires_at_ms: i64,
}

pub fn code_page(frame: &Frame, rights: &Rights, view: &CodeView<'_>) -> String {
    let mut out = String::new();
    out.push_str(r#"<section class="pagePart" aria-labelledby="inbox-title">"#);
    head(
        &mut out,
        rights,
        Section::Phones,
        "inbox-title",
        "The code is made",
        "",
    );
    let code = html_escape(view.code);
    let _ = write!(
        out,
        r##"<p class="uiBanner uiBanner--inset uiBanner--ok" role="status">Copy the code now. The portal shows it one time, and it keeps only a digest. The code works until {expires}</p><div class="uiField"><label class="uiField__label" for="enroll-code">Code</label><div class="keyRow"><input class="uiInput uiInput--mono keyText" id="enroll-code" type="text" readonly value="{code}" data-expires-ms="{expires_ms}" spellcheck="false" autocomplete="off"><button class="button copyButton" type="button" data-copy="enroll-code"><svg class="icon icon--sm copyButton__idle" aria-hidden="true"><use href="#i-copy"/></svg><svg class="icon icon--sm copyButton__done" aria-hidden="true"><use href="#i-check"/></svg>Copy</button></div></div><p><a class="button button--primary" href="{code}">Open in the app</a></p><p class="uiField__hint">The key of the server has the fingerprint <code>{fingerprint}</code>. The app shows the same fingerprint. Compare the two before you go on.</p><p class="uiBanner uiBanner--inset uiBanner--warn">Anyone with this code can add a phone to your account until the code ends or is used.</p><p><a class="button" href="{PHONES_PATH}">Back to my phones</a></p></section>"##,
        expires = time_tag(view.expires_at_ms / 1000, true),
        expires_ms = view.expires_at_ms,
        fingerprint = html_escape(view.fingerprint),
    );
    page_shell(Tab::Inbox, frame, "Add a phone", &out)
}

// Connections ---------------------------------------------------------------------------------

pub struct ConnectionsView<'a> {
    /// The login of the filter. Empty: all the logins.
    pub login: &'a str,
    pub rows: Vec<ConnectionView>,
    /// The log shows at most this number of lines.
    pub limit: u32,
}

pub fn connections_page(frame: &Frame, rights: &Rights, view: &ConnectionsView<'_>) -> String {
    let mut out = String::new();
    out.push_str(r#"<section class="pagePart" aria-labelledby="inbox-title">"#);
    head(
        &mut out,
        rights,
        Section::Connections,
        "inbox-title",
        "Connections",
        &plural(view.rows.len(), "line", "lines"),
    );
    let _ = write!(
        out,
        r#"<form method="get" action="{CONNECTIONS_PATH}" class="uiForm filterForm"><div class="uiField uiField--narrow"><label class="uiField__label" for="filter-login">Login</label><input class="uiInput" type="text" id="filter-login" name="login" value="{}" maxlength="64" autocomplete="off"></div><button class="button" type="submit">Filter</button></form><p class="part__lede">The newest {} connections of the phones. The login is the login of the account of the phone.</p>"#,
        html_escape(view.login),
        view.limit,
    );
    if view.rows.is_empty() {
        out.push_str(
            r#"<div class="uiState uiState--compact"><p class="uiState__body">No connection in the log.</p></div>"#,
        );
    } else {
        out.push_str(
            r#"<div class="dataTableWrap"><table class="dataTable stack connectionsTable"><thead><tr><th scope="col" class="num">Start</th><th scope="col">Login</th><th scope="col">Phone</th><th scope="col">Address</th><th scope="col">App</th><th scope="col" class="num">End</th><th scope="col">Reason</th></tr></thead><tbody>"#,
        );
        for c in &view.rows {
            let _ = write!(
                out,
                r#"<tr data-login="{login}" data-device="{id}" data-reason="{reason_data}"><td class="num" data-label="Start">{start}</td><td data-label="Login">{login}</td><td data-label="Phone">{name}</td><td data-label="Address">{ip}</td><td data-label="App">{app}</td><td class="num" data-label="End">{end}</td><td data-label="Reason">{reason}</td></tr>"#,
                login = html_escape(&c.user_name),
                id = html_escape(&c.device_id),
                start = when(c.connected_at),
                name = html_escape(&c.device_name),
                ip = html_escape(&c.ip),
                app = html_escape(&c.app_version),
                end = match c.disconnected_at {
                    Some(ms) => when(ms),
                    None => r#"<span class="muted">open</span>"#.to_string(),
                },
                reason_data = html_escape(c.close_reason.as_deref().unwrap_or("")),
                reason = match &c.close_reason {
                    Some(r) => html_escape(r),
                    None => muted_none().to_string(),
                },
            );
        }
        out.push_str("</tbody></table></div>");
    }
    out.push_str("</section>");
    page_shell(Tab::Inbox, frame, "Connections", &out)
}

// Documentation -------------------------------------------------------------------------------

pub struct DocView {
    /// The address of the listener of the producers, with the scheme.
    pub base: String,
    pub groups: Vec<GroupLine>,
    /// The portal accepts keys: a key can be a producer.
    pub keys: bool,
    /// The account may make a key.
    pub keys_page: bool,
}

pub fn doc_page(frame: &Frame, rights: &Rights, view: &DocView) -> String {
    let base = html_escape(&view.base);
    let mut out = String::new();
    out.push_str(r#"<section class="pagePart" aria-labelledby="inbox-title">"#);
    head(
        &mut out,
        rights,
        Section::Doc,
        "inbox-title",
        "Documentation",
        "",
    );
    let _ = write!(
        out,
        r#"<div class="docs"><p class="part__lede">A producer is a program that sends alerts: a script, Grafana, Alertmanager or any tool that sends an HTTP request. The inbox takes each alert, keeps it, and sends it to each phone that subscribes to the group. A phone shows the alert one time.</p>
<h3 class="subTitle">Address</h3>
<p>The producers send their requests to this address. Use <code>http</code> in place of <code>https</code> when the listener has no TLS.</p>
<p><code>{base}</code></p>
<h3 class="subTitle">Who can send</h3>
<p>A request has the header <code>Authorization: Bearer &lt;token&gt;</code>. A tool that cannot set a header can use the query <code>?token=&lt;token&gt;</code>. A token in an address can reach a log of a proxy: use the header when you can. Two kinds of token work at the same time.</p>
<ul class="docList"><li>A <strong>token of the file</strong> is the value of a block <code>producer</code> in the configuration. The block lists the groups of the token.</li>"#
    );
    if view.keys {
        out.push_str(
            r#"<li>A <strong>key of the portal</strong> starts with <code>mcp_</code>. The key needs the permission <code>alert.send</code> on the group, and the permission of its owner too. The permission can have a limit of alerts in a time window. A critical group has no limit. A key sends no notification of the type resolved.</li>"#,
        );
    }
    out.push_str("</ul>");
    if view.keys && view.keys_page {
        out.push_str(
            r#"<p>Make a key on the page <a href="/auth/keys">Keys</a>. Add the permission <code>alert.send</code> of the service inbox for each group.</p>"#,
        );
    }
    out.push_str(r#"<h3 class="subTitle">Groups</h3>"#);
    if view.groups.is_empty() {
        out.push_str("<p>No group.</p>");
    } else {
        out.push_str(
            r#"<div class="dataTableWrap"><table class="dataTable"><thead><tr><th scope="col">Id</th><th scope="col">Name</th><th scope="col">Limit</th></tr></thead><tbody>"#,
        );
        for g in &view.groups {
            let _ = write!(
                out,
                r#"<tr><td><code>{}</code></td><td>{}</td><td>{}</td></tr>"#,
                html_escape(&g.id),
                html_escape(&g.name),
                if g.critical {
                    "never (critical)"
                } else {
                    "by the key"
                }
            );
        }
        out.push_str("</tbody></table></div>");
    }
    let _ = write!(
        out,
        r#"<h3 class="subTitle">Send an alert</h3>
<p>The body is JSON. The group is the id of a group. The title has at most {MAX_TITLE} characters, on one line. The header <code>Idempotency-Key</code> is optional: two requests with the same key make one alert.</p>
<pre class="docCode"><code>curl -X POST {base}/api/v1/alerts \
  -H "Authorization: Bearer $TOKEN" \
  -H "Idempotency-Key: deploy-4711" \
  -d '{{"group":"infra","title":"Disk full on db-1"}}'</code></pre>
<p>The answer is <code>201</code> with <code>alert_id</code>, <code>duplicate</code> and <code>targets</code> (the number of phones). A repeated request gives <code>200</code>.</p>
<h3 class="subTitle">Webhook for any tool</h3>
<p>The path has the group. The title comes from the query <code>?title=</code>, from the header <code>X-Title</code>, from the field <code>title</code> of a JSON body, or from the body itself. The inbox cuts a long title.</p>
<pre class="docCode"><code>curl -X POST "{base}/api/v1/notify/infra?title=Backup%20done" \
  -H "Authorization: Bearer $TOKEN"</code></pre>
<h3 class="subTitle">Alertmanager</h3>
<p>Add a receiver. Each firing alert of the payload makes one alert. A repeat of the same alert makes no second alert. Alerts of the type resolved are not sent unless the producer has <code>webhook_send_resolved = true</code>.</p>
<pre class="docCode"><code>receivers:
  - name: inbox
    webhook_configs:
      - url: {base}/api/v1/webhooks/alertmanager/infra
        send_resolved: false
        http_config:
          authorization:
            type: Bearer
            credentials_file: /etc/alertmanager/inbox-token</code></pre>
<h3 class="subTitle">Grafana</h3>
<p>Add a contact point of the type Webhook. Set the URL to the Alertmanager path of the group, the method to POST, and the authorization header to <code>Bearer</code> with the token. Grafana sends the same payload as Alertmanager.</p>
<h3 class="subTitle">Status of an alert</h3>
<p><code>GET {base}/api/v1/alerts/&lt;alert_id&gt;</code> gives the delivery status of an alert, for a producer that escalates in another tool. The token must have the permission <code>alert.read</code> on the group of the alert, or the group in its block.</p>
<h3 class="subTitle">Errors</h3>
<div class="dataTableWrap"><table class="dataTable"><thead><tr><th scope="col">Status</th><th scope="col">Meaning</th></tr></thead><tbody>
<tr><td><code>401</code></td><td>The token is missing or not valid.</td></tr>
<tr><td><code>403</code></td><td>The token has no permission to send to this group. The body names the group.</td></tr>
<tr><td><code>422</code></td><td>The group does not exist, or the title is empty.</td></tr>
<tr><td><code>429</code></td><td>The limit of alerts of the key is reached. The header <code>Retry-After</code> gives the seconds to wait.</td></tr>
<tr><td><code>503</code></td><td>The inbox cannot take the alert now (the storage is away), or the portal cannot check a key yet (<code>directory_not_ready</code>). Send the request again. A token of the file works when the portal is not ready.</td></tr>
</tbody></table></div>
</div></section>"#
    );
    page_shell(Tab::Inbox, frame, "Documentation", &out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::inbox::admin::{DeliveryView, DeviceView};

    fn frame() -> Frame<'static> {
        Frame {
            logout_token: "out",
            keys: true,
            inbox: true,
            superadmin: false,
        }
    }

    fn rights(all: bool) -> Rights {
        Rights {
            overview: all,
            devices: all,
            revoke: all,
            connections: all,
            phones: true,
            send: if all {
                vec![GroupChoice {
                    id: "infra".into(),
                    name: "Infra <b>".into(),
                }]
            } else {
                Vec::new()
            },
        }
    }

    fn device(id: &str, name: &str, login: &str, state: DeviceState) -> DeviceRow {
        DeviceRow {
            device: DeviceView {
                id: id.into(),
                name: name.into(),
                user_name: login.into(),
                enrolled_at: 1_000,
                enrolled_ip: "127.0.0.1".into(),
                app_version: Some("1.2.3".into()),
                last_connected_at: Some(2_000),
                last_seen_at: Some(3_000),
                last_ip: None,
                revoked_at: (state == DeviceState::Revoked).then_some(4_000),
                subscriptions: vec!["infra".into()],
                pending: 2,
            },
            live: None,
            state,
        }
    }

    fn groups() -> Vec<GroupLine> {
        vec![
            GroupLine {
                id: "infra".into(),
                name: "Infra <b>".into(),
                description: "prod".into(),
                critical: true,
                subscribed: 3,
            },
            GroupLine {
                id: "other".into(),
                name: "Other".into(),
                description: String::new(),
                critical: false,
                subscribed: 0,
            },
        ]
    }

    fn alert(id: &str, group: &str, title: &str) -> AlertSummary {
        AlertSummary {
            id: id.into(),
            group: group.into(),
            title: title.into(),
            source: "admin".into(),
            created_at: 1_700_000_000_000,
            targets: 3,
            received: 2,
            expired: 1,
            missing: Vec::new(),
        }
    }

    #[test]
    fn the_overview_has_the_groups_the_critical_mark_and_the_send_form_for_the_allowed_groups() {
        let view = OverviewView {
            notice: None,
            csrf: "tok",
            stats: Some(Stats {
                enrolled: 3,
                online: 2,
                unreachable: 1,
                pending: 4,
                oldest_pending_ms: 90_000,
            }),
            groups: groups(),
            alerts: Some(vec![alert("a1", "infra", "Disk <script>")]),
            idempotency_key: "k1",
        };
        let html = overview_page(&frame(), &rights(true), &view);
        assert!(html.contains("Infra &lt;b&gt;"), "{html}");
        assert!(!html.contains("Infra <b>"));
        assert!(html.contains(r#"<span class="badge badge--warn""#));
        assert!(html.contains("critical"));
        assert!(html.contains(r#"action="/auth/settings/inbox/send""#));
        assert!(html.contains(r#"<option value="infra">Infra &lt;b&gt;</option>"#));
        assert!(
            !html.contains(r#"<option value="other">"#),
            "the form lists only the groups that the account may send to"
        );
        assert!(html.contains(r#"name="key" value="k1""#));
        assert!(html.contains("Disk &lt;script&gt;"));
        assert!(html.contains(r#"href="/auth/settings/inbox/alerts/a1""#));
        assert!(html.contains("2 of 3"));
        assert!(html.contains("Alerts waiting"));
        // The tab and the sections.
        assert!(html.contains(r#"class="contentTabs__tab is-active" aria-current="page" href="/auth/settings/inbox">Inbox"#));
        assert!(html.contains(">Overview</a>") && html.contains(">Phones</a>"));
        assert!(html.contains(">Connections</a>") && html.contains(">My phones</a>"));
        assert!(html.contains(">Documentation</a>"));
    }

    #[test]
    fn an_account_without_the_send_right_has_no_form_and_one_without_the_read_right_has_no_alerts()
    {
        let view = OverviewView {
            notice: None,
            csrf: "tok",
            stats: None,
            groups: groups(),
            alerts: None,
            idempotency_key: "k1",
        };
        let html = overview_page(&frame(), &rights(false), &view);
        assert!(!html.contains(r#"action="/auth/settings/inbox/send""#));
        assert!(!html.contains("Last alerts"));
        assert!(!html.contains("Alerts waiting"));
        // The sections that the account may not open are not in the switch.
        assert!(!html.contains(">Connections</a>"));
        assert!(!html.contains(r#"href="/auth/settings/inbox/devices""#));
        assert!(html.contains(">My phones</a>"));
    }

    #[test]
    fn the_revoke_button_asks_for_a_confirmation_and_exists_only_with_the_right() {
        let rows = || {
            vec![
                device("d1", "phone <1>", "alice", DeviceState::Online),
                device("d2", "old", "bob", DeviceState::Revoked),
            ]
        };
        let with = DevicesView {
            notice: None,
            csrf: "tok",
            rows: rows(),
            can_revoke: true,
            back: DEVICES_PATH,
        };
        let html = devices_page(&frame(), &rights(true), &with);
        assert!(html.contains(r#"<details class="confirm">"#));
        assert!(html.contains("Confirm: revoke phone &lt;1&gt;"));
        assert_eq!(
            html.matches("Confirm: revoke").count(),
            1,
            "a revoked phone has no button"
        );
        assert!(
            html.contains(r#"data-state="online""#) && html.contains(r#"data-state="revoked""#)
        );
        assert!(html.contains(r#"data-login="alice""#));
        let without = DevicesView {
            can_revoke: false,
            rows: rows(),
            ..with
        };
        let html = devices_page(&frame(), &rights(true), &without);
        assert!(!html.contains("Confirm: revoke"));
    }

    #[test]
    fn the_phones_of_an_account_show_the_add_form_with_its_token() {
        let view = PhonesView {
            devices: DevicesView {
                notice: None,
                csrf: "tok<",
                rows: Vec::new(),
                can_revoke: true,
                back: PHONES_PATH,
            },
            can_add: true,
        };
        let html = phones_page(&frame(), &rights(false), &view);
        assert!(html.contains(r#"action="/auth/settings/inbox/code""#));
        assert!(html.contains(r#"name="csrf" value="tok&lt;""#));
        assert!(html.contains("Make a code"));
        assert!(html.contains("No phone yet"));
        let view = PhonesView {
            can_add: false,
            ..view
        };
        assert!(!phones_page(&frame(), &rights(false), &view).contains("Make a code"));
    }

    #[test]
    fn the_code_page_has_the_text_the_link_the_fingerprint_and_the_copy_button() {
        let html = code_page(
            &frame(),
            &rights(false),
            &CodeView {
                code: "inbox://enroll?s=h%3A1&k=a&c=b",
                fingerprint: "0a1b2c3d",
                expires_at_ms: 1_700_000_000_000,
            },
        );
        assert!(html.contains(r#"value="inbox://enroll?s=h%3A1&amp;k=a&amp;c=b""#));
        assert!(html.contains(r#"href="inbox://enroll?s=h%3A1&amp;k=a&amp;c=b""#));
        assert!(html.contains(r#"data-copy="enroll-code""#));
        assert!(html.contains("0a1b2c3d"));
        assert!(html.contains("one time"));
        assert!(html.contains("2023-11-14"));
    }

    #[test]
    fn the_connection_log_has_a_filter_that_keeps_its_value() {
        let html = connections_page(
            &frame(),
            &rights(true),
            &ConnectionsView {
                login: "al\"ice",
                rows: vec![ConnectionView {
                    device_id: "d1".into(),
                    device_name: "p".into(),
                    user_name: "alice".into(),
                    ip: "10.0.0.1".into(),
                    app_version: "1".into(),
                    connected_at: 1_700_000_000_000,
                    disconnected_at: None,
                    close_reason: None,
                }],
                limit: 100,
            },
        );
        assert!(html.contains(r#"value="al&quot;ice""#));
        assert!(html.contains(r#"method="get""#));
        assert!(html.contains(r#"data-login="alice""#));
        assert!(html.contains("open"));
    }

    #[test]
    fn the_alert_page_shows_each_delivery() {
        let detail = AlertDetail {
            summary: alert("a1", "infra", "T"),
            deliveries: vec![
                DeliveryView {
                    device_id: "d1".into(),
                    device_name: "p1".into(),
                    user_name: "alice".into(),
                    attempts: 2,
                    first_sent_at: Some(1_700_000_000_000),
                    last_sent_at: None,
                    received_at: Some(1_700_000_001_000),
                    expired: false,
                    device_revoked: false,
                },
                DeliveryView {
                    device_id: "d2".into(),
                    device_name: "p2".into(),
                    user_name: "bob".into(),
                    attempts: 0,
                    first_sent_at: None,
                    last_sent_at: None,
                    received_at: None,
                    expired: true,
                    device_revoked: false,
                },
            ],
        };
        let html = alert_page(
            &frame(),
            &AlertView {
                notice: None,
                detail: &detail,
                group_name: "Infra",
            },
        );
        assert!(html.contains(">received<") && html.contains(">expired<"));
        assert!(html.contains(r#"data-device="d2""#));
    }

    #[test]
    fn the_documentation_shows_the_real_address_and_the_key_text_only_with_keys() {
        let view = |keys| DocView {
            base: "https://inbox.example.org:8080".into(),
            groups: groups(),
            keys,
            keys_page: keys,
        };
        let html = doc_page(&frame(), &rights(true), &view(true));
        assert!(html.contains("https://inbox.example.org:8080/api/v1/alerts"));
        assert!(html.contains("/api/v1/notify/infra"));
        assert!(html.contains("/api/v1/webhooks/alertmanager/infra"));
        assert!(html.contains("<code>mcp_</code>"));
        assert!(html.contains("never (critical)"));
        assert!(html.contains(r#"href="/auth/keys""#));
        let no_keys = Frame {
            keys: false,
            ..frame()
        };
        let html = doc_page(&no_keys, &rights(true), &view(false));
        assert!(!html.contains("mcp_"));
        assert!(!html.contains(r#"href="/auth/keys""#));
    }

    #[test]
    fn the_pages_use_no_inline_style_and_no_inline_script() {
        let view = OverviewView {
            notice: notice_for("sent"),
            csrf: "tok",
            stats: None,
            groups: groups(),
            alerts: Some(vec![alert("a1", "infra", "T")]),
            idempotency_key: "k1",
        };
        for html in [
            overview_page(&frame(), &rights(true), &view),
            doc_page(
                &frame(),
                &rights(true),
                &DocView {
                    base: "http://x".into(),
                    groups: groups(),
                    keys: true,
                    keys_page: true,
                },
            ),
        ] {
            assert!(!html.contains(" style="), "{html}");
            assert!(!html.contains("<style"));
            assert!(!html.contains("onclick"));
            assert!(
                !html.contains("<script>"),
                "only the script with a src is allowed"
            );
        }
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
        assert!(notice_for("sent").unwrap().ok);
        assert!(!notice_for("failed").unwrap().ok);
    }
}
