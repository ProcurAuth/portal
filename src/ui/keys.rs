//! The keys in the tab Access (portal 5.8): the list of the keys, and the pages "New key" and
//! "Edit key".
//!
//! A key has one kind, API or MCP, and the list shows it with an icon. The list is a panel of the
//! tab Access, next to the accounts. The pages of a key are child pages of Access, as the page of an
//! account is. The text of a key shows one time, in the page that follows the creation, with
//! `Cache-Control: no-store`. Each page works without script: the kind selector is a small form
//! that asks for the page again, and a bulk revoke is a form with checkboxes.
//!
//! Each action has one entry point. The head of the list has "New key". A page of a key has
//! Cancel and the action.

use std::fmt::Write as _;

use super::access::Query;
use super::components::{
    ask, bulk_all_box, bulk_box, bulk_button, bulk_form, cancel_link, chips, icon, icon_link,
    page_head, zone, Ask, Bulk, Chip, PageHead, Side, Strength, Zone,
};
use super::grant_editor::{self, EditorData, Place};
use super::settings::{banner_error, fill_page, notice_html, Frame, Notice, Tab};
use crate::grants::{Grant, ACCESS};
use crate::keys::KeyKind;
use crate::util::{format_utc_instant, html_escape, time_tag};

/// The list of the keys is in the tab Access.
pub const PATH: &str = super::settings::ACCESS_PATH;
/// `GET`: the page with the pane of a new key open.
pub const NEW_PATH: &str = "/auth/keys/new";
pub const CREATE_PATH: &str = "/auth/keys/create";
/// `GET`: the page with the pane of the change of a key open. `POST`: the change.
pub const EDIT_PATH: &str = "/auth/keys/edit";
pub const REVOKE_PATH: &str = "/auth/keys/revoke";

/// The state of a key (portal 5.5): the first that applies, in this order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeyState {
    Expired,
    TooOld,
    Active,
    /// The summary row of a revoked key.
    Revoked,
}

impl KeyState {
    fn icon(self) -> String {
        let (tone, icon) = match self {
            Self::Active => ("ok", "check"),
            Self::Revoked => ("error", "ban"),
            Self::Expired | Self::TooOld => ("warn", "alert-triangle"),
        };
        super::status_icon(tone, icon, self.label())
    }

    fn label(self) -> &'static str {
        match self {
            Self::Expired => "expired",
            Self::TooOld => "too old",
            Self::Active => "active",
            Self::Revoked => "revoked",
        }
    }
}

/// How the rights of the owner cover a grant of a key (portal 4.3).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cover {
    /// The owner holds the grant, or a grant above it.
    Full,
    /// The owner holds only nodes below the node of the grant.
    Limited,
    /// The owner holds nothing of it: the grant gives nothing.
    None,
}

/// A grant of a key, as the list shows it.
#[derive(Clone, Debug)]
pub struct GrantLine {
    pub grant: Grant,
    pub cover: Cover,
}

/// One line of the page: a key, or the summary row of a revoked key.
#[derive(Clone, Debug)]
pub struct KeyLine {
    pub id: String,
    /// The login of the owner. `None`: the key of the superadmin.
    pub owner: Option<String>,
    /// The viewer owns the key.
    pub mine: bool,
    pub name: String,
    pub hint: String,
    /// The kind of the key. `None`: the summary row of a revoked key has no kind.
    pub kind: Option<KeyKind>,
    pub state: KeyState,
    /// The key comes from the configuration file: it is read-only.
    pub file: bool,
    pub created_at: i64,
    pub expires_at: Option<i64>,
    pub last_check_at: Option<i64>,
    pub check_count: i64,
    pub revoked_at: Option<i64>,
    pub grants: Vec<GrantLine>,
    /// Services where the key has a node grant and no grant `access` (portal 4.2b).
    pub no_access: Vec<String>,
    /// The user can change the key: the row has a link to the page of the change.
    pub can_change: bool,
}

/// The new key: its text, shown one time.
pub struct NewKey<'a> {
    pub text: &'a str,
    pub name: &'a str,
    pub superadmin: bool,
}

/// The key of the pane of a change.
pub struct EditTarget {
    pub id: String,
    pub name: String,
}

/// The page of a key.
pub enum Pane<'a> {
    /// The form of a new key. The view holds the kind and the editor.
    New,
    /// The form of the change of a key.
    Edit(EditTarget),
    /// The text of the key that was just made.
    Made(NewKey<'a>),
}

/// The field of the form of a key that a refusal names.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Field {
    Name,
    Expiry,
}

/// The field that the outcome of a refused form names. `None`: the outcome is for the whole form.
pub fn field_of(code: &str) -> Option<Field> {
    match code {
        "bad_name" => Some(Field::Name),
        "bad_expiry" => Some(Field::Expiry),
        _ => None,
    }
}

/// The data of the pages of a key.
pub struct KeysView<'a> {
    /// The kinds of key that the viewer may make: the kinds that the file turns on and that the
    /// viewer holds `keys.own.create` on. The new-key page has a selector only with two or more.
    pub create_kinds: Vec<KeyKind>,
    /// The kind of the key that the form makes or changes.
    pub kind: Option<KeyKind>,
    pub notice: Option<Notice>,
    /// The field that the outcome names. The message shows under it.
    pub field: Option<Field>,
    pub csrf: &'a str,
    /// The editor of the grants of the form. `None`: the page has no form.
    pub editor: Option<EditorData>,
    pub pane: Pane<'a>,
    /// The first value of the field `expires_at`.
    pub expires_at: String,
    /// The longest lifetime of a key, in seconds (portal 5.3).
    pub max_lifetime: Option<u64>,
    /// The name that the form of a new key has when it copies a key (portal 4.1a).
    pub copy_name: String,
    /// The id of the service that has a right and no `access`, when the server refused the form
    /// for this reason. The form shows again with the values that the person typed.
    pub needs_access: Option<String>,
}

/// The text of the outcome of a form, from a fixed list (portal 5.8f).
pub fn notice_for(code: &str) -> Option<Notice> {
    let (text, ok) = match code {
        "edited" => ("The key is changed. The change applies within a few seconds.", true),
        "revoked" => ("The key is revoked. It stops within a few seconds.", true),
        "too_many_keys" => (
            "You have the maximum number of active keys. Revoke a key first.",
            false,
        ),
        "bad_name" => (
            "The name of a key has 1 to 64 characters, with no control character.",
            false,
        ),
        "bad_expiry" => (
            "The expiry date is not valid. It is a date and a time in UTC in the future, and it is not later than the longest lifetime of a key.",
            false,
        ),
        "bad_grant" => (
            "A permission is not valid: its text, its action, its place in the tree of the service, or the number of permissions.",
            false,
        ),
        "not_covered" => (
            "Your rights do not cover a permission of this key.",
            false,
        ),
        "bad_limits" => (
            "A limit value is not valid for its permission.",
            false,
        ),
        "needs_access" => (
            "A permission on a service needs the switch Can open on that service. Tick the switch, then save.",
            false,
        ),
        "capacity" => (
            "The portal holds the largest number of counter rows or of permissions. Revoke a key first.",
            false,
        ),
        "unknown_key" => ("That key does not exist, or you cannot change it.", false),
        "file_key" => (
            "This key comes from the configuration file. Remove it from the file, and restart the portal.",
            false,
        ),
        _ => return None,
    };
    Some(Notice { text, ok })
}

const KIND_TIP_API: &str = "API key: for services and webhooks";
const KIND_TIP_MCP: &str = "MCP key: for agents";

fn kind_tip(kind: KeyKind) -> &'static str {
    match kind {
        KeyKind::Api => KIND_TIP_API,
        KeyKind::Mcp => KIND_TIP_MCP,
    }
}

/// The icon of a kind: a webhook for an API key, a robot for an MCP key.
fn kind_icon(kind: KeyKind) -> &'static str {
    match kind {
        KeyKind::Api => "webhook",
        KeyKind::Mcp => "robot",
    }
}

/// What a key can use, in one short line: the groups of the inbox that it sends to and reads, or
/// the services of an MCP key. A mark shows a right that the owner lost.
fn can_use(groups: &[(String, String)], line: &KeyLine) -> (String, String) {
    if line.grants.is_empty() {
        return (
            r#"<span class="muted">nothing</span>"#.to_string(),
            String::new(),
        );
    }
    let portal = crate::portal_actions::SERVICE;
    let name_of = |path: &str| {
        let id = path.strip_prefix("inbox_group:").unwrap_or(path);
        groups
            .iter()
            .find(|(g, _)| g == id)
            .map_or(id.to_string(), |(_, name)| name.clone())
    };
    let names = |action: &str| -> Vec<String> {
        line.grants
            .iter()
            .filter(|g| g.grant.service == portal && g.grant.action == action)
            .map(|g| name_of(&crate::grants::path_text(&g.grant.path)))
            .collect()
    };
    let mut parts: Vec<String> = Vec::new();
    let send = names(crate::portal_actions::ALERT_SEND);
    if !send.is_empty() {
        parts.push(format!("Send {}", send.join(", ")));
    }
    let read = names(crate::portal_actions::ALERT_READ);
    if !read.is_empty() {
        parts.push(format!("Read {}", read.join(", ")));
    }
    let mut out = String::new();
    let mut plain: Vec<String> = Vec::new();
    if !parts.is_empty() {
        plain.push(parts.join(" - "));
        let _ = write!(
            out,
            r#"<span class="feat feat--text">{}<span class="feat__text">{}</span></span>"#,
            icon("inbox"),
            html_escape(&parts.join(" · "))
        );
    }
    let mut services: Vec<&str> = line
        .grants
        .iter()
        .map(|g| g.grant.service.as_str())
        .filter(|s| *s != portal)
        .collect();
    services.sort_unstable();
    services.dedup();
    if !services.is_empty() {
        let count = line
            .grants
            .iter()
            .filter(|g| g.grant.service != portal)
            .count();
        let shown: Vec<&str> = services.iter().take(3).copied().collect();
        let text = format!(
            "{}{} · {}",
            shown.join(", "),
            if services.len() > 3 {
                format!(" +{}", services.len() - 3)
            } else {
                String::new()
            },
            super::settings::plural(count, "permission", "permissions")
        );
        plain.push(text.clone());
        let _ = write!(out, r#"<span class="muted">{}</span>"#, html_escape(&text));
    }
    let lost = line
        .grants
        .iter()
        .filter(|g| g.cover == Cover::None)
        .count();
    if lost > 0 {
        out.push_str(&super::status_icon(
            "warn",
            "alert-triangle",
            &format!(
                "{} no effect: the owner does not hold it any more",
                super::settings::plural(lost, "right has", "rights have")
            ),
        ));
    }
    if !line.no_access.is_empty() {
        out.push_str(&super::status_icon(
            "warn",
            "alert-triangle",
            "A service of this key has no access permission",
        ));
    }
    (
        format!(r#"<span class="canuse">{out}</span>"#),
        plain.join("; "),
    )
}

/// The keys that the viewer sees in the tab Access, with what the list needs to draw its controls.
pub struct KeysList {
    /// Every key of the viewer, or of all the accounts. The file keys come first, then the keys of
    /// the database, newest first.
    pub lines: Vec<KeyLine>,
    /// The list holds more keys than the page draws.
    pub truncated: bool,
    /// The viewer sees the keys of all the accounts: the list has the column Owner and the chip
    /// "mine".
    pub all: bool,
    /// The kinds of key that the viewer may make. None: the list has no "New key".
    pub create_kinds: Vec<KeyKind>,
    pub can_revoke: bool,
    pub can_revoke_all: bool,
    /// The user browses as another user: no change is possible (portal 3.7).
    pub impersonated: bool,
    /// The owner can make keys and cannot use them (portal 5.3b).
    pub cannot_use: bool,
    /// The names of the groups of the inbox, for the column "Can use".
    pub groups: Vec<(String, String)>,
    /// The anti-CSRF token of the forms of the keys.
    pub csrf: String,
}

impl KeysList {
    /// Whether the row has a box: the viewer may revoke the key.
    fn may_revoke(&self, line: &KeyLine) -> bool {
        line.revoked_at.is_none()
            && !line.file
            && !self.impersonated
            && if line.mine {
                self.can_revoke || self.can_revoke_all
            } else {
                self.can_revoke_all
            }
    }

    /// "New key" is a link in the head of the list. It is absent when the viewer may make no kind.
    fn can_create(&self) -> bool {
        !self.create_kinds.is_empty() && !self.impersonated
    }
}

/// The groups of the inbox that a key names in its grants.
pub(super) fn key_groups(line: &KeyLine) -> Vec<&str> {
    line.grants
        .iter()
        .filter(|g| g.grant.service == crate::portal_actions::SERVICE)
        .filter_map(|g| {
            g.grant
                .path
                .first()
                .filter(|s| s.kind == crate::portal_actions::INBOX_GROUP)
        })
        .map(|s| s.id.as_str())
        .collect()
}

fn key_row(out: &mut String, list: &KeysList, bulk: &Bulk, boxes: bool, line: &KeyLine) {
    // One line: the kind, the name, what the key can use, when it ends, when it was used. The id,
    // the hint and the creation date are in the tooltip of the name.
    let hint = match line.kind {
        Some(kind) if !line.hint.is_empty() => {
            format!(" Starts with {}{}.", kind.prefix(), line.hint)
        }
        _ => String::new(),
    };
    let created = if line.file {
        String::new()
    } else {
        format!(" Made {}.", crate::util::format_utc(line.created_at))
    };
    let _ = write!(
        out,
        r#"<tr data-kind="{}">"#,
        line.kind.map_or("", KeyKind::name)
    );
    if boxes {
        let _ = write!(
            out,
            "<td>{}</td>",
            if list.may_revoke(line) {
                bulk_box(bulk, &line.id, &format!("Select {}", line.name))
            } else {
                String::new()
            }
        );
    }
    let kind_cell = match line.kind {
        Some(kind) => format!(
            r#"<span class="kindIcon" role="img" aria-label="{0}" title="{0}">{1}</span>"#,
            kind_tip(kind),
            icon(kind_icon(kind))
        ),
        None => String::new(),
    };
    let state = if line.state == KeyState::Active {
        String::new()
    } else {
        format!(" {}", line.state.icon())
    };
    let _ = write!(
        out,
        r#"<td>{kind_cell}</td><td title="Id {}.{hint}{created}">{}{}{}"#,
        html_escape(&line.id),
        html_escape(&line.name),
        state,
        if line.file {
            r#" <span class="badge" title="Defined in the configuration file">file</span>"#
        } else {
            ""
        }
    );
    if let Some(at) = line.revoked_at {
        let _ = write!(
            out,
            r#" <span class="muted">revoked {}</span>"#,
            time_tag(at, false)
        );
    }
    out.push_str("</td>");
    if list.all {
        let _ = write!(
            out,
            r#"<td class="muted wide">{}</td>"#,
            match &line.owner {
                Some(login) => html_escape(login),
                None => "superadmin".to_string(),
            }
        );
    }
    let (can_use_html, can_use_text) = can_use(&list.groups, line);
    let _ = write!(
        out,
        r#"<td title="{}">{}</td><td class="r wide">{}</td><td class="r" title="{} checks">{}</td><td class="r">"#,
        html_escape(&can_use_text),
        can_use_html,
        match line.expires_at {
            Some(at) => time_tag(at, false),
            None => r#"<span class="muted">never</span>"#.to_string(),
        },
        line.check_count,
        match line.last_check_at {
            Some(at) => time_tag(at, false),
            None => r#"<span class="muted">never</span>"#.to_string(),
        }
    );
    if line.can_change {
        out.push_str(&icon_link(
            &format!("{EDIT_PATH}?id={}", line.id),
            &format!("Edit {}", line.name),
            "pencil",
        ));
    }
    out.push_str("</td></tr>");
}

/// The list of the keys in the tab Access. `matching` are the lines that the filters of the page
/// keep. The chips of the kinds count them, so the figures follow the filters. The chip "mine" and
/// the chip of a kind then keep the rows of the list.
pub(super) fn keys_zone(list: &KeysList, query: &Query, matching: &[&KeyLine]) -> String {
    let shown: Vec<&KeyLine> = matching
        .iter()
        .copied()
        .filter(|l| query.kind.is_none_or(|k| l.kind == Some(k)))
        .filter(|l| !query.mine || l.mine)
        .collect();
    let count_kind = |kind: KeyKind| matching.iter().filter(|l| l.kind == Some(kind)).count();
    let api_label = format!(r#"<span class="st--off">{}</span>"#, icon("webhook"));
    let mcp_label = format!(r#"<span class="st--off">{}</span>"#, icon("robot"));
    let mine_label = format!(r#"<span class="st--off">{}</span>"#, icon("user"));
    let all_href = query.with_kind(None).with_mine(false).href();
    let api_href = query.with_kind(toggled(query.kind, KeyKind::Api)).href();
    let mcp_href = query.with_kind(toggled(query.kind, KeyKind::Mcp)).href();
    let mine_href = query.with_mine(!query.mine).href();
    let chips_html = chips(&[
        Chip {
            label_html: "All",
            name: "",
            count: matching.len(),
            href: &all_href,
            on: query.kind.is_none() && !query.mine,
        },
        Chip {
            label_html: &api_label,
            name: KIND_TIP_API,
            count: count_kind(KeyKind::Api),
            href: &api_href,
            on: query.kind == Some(KeyKind::Api),
        },
        Chip {
            label_html: &mcp_label,
            name: KIND_TIP_MCP,
            count: count_kind(KeyKind::Mcp),
            href: &mcp_href,
            on: query.kind == Some(KeyKind::Mcp),
        },
        Chip {
            label_html: &mine_label,
            name: "My keys",
            count: if list.all {
                matching.iter().filter(|l| l.mine).count()
            } else {
                0
            },
            href: &mine_href,
            on: query.mine,
        },
    ]);
    let bulk = Bulk {
        id: "revoke-keys",
        action: REVOKE_PATH,
        field: "id",
        // A viewer who may revoke the keys of all the accounts asks for the section "all": the
        // handler then keeps no owner filter.
        fields_html: &format!(
            r#"<input type="hidden" name="csrf" value="{}"><input type="hidden" name="section" value="{}">"#,
            html_escape(&list.csrf),
            if list.can_revoke_all { "all" } else { "mine" }
        ),
        verb: "Revoke",
        one: "key",
        many: "keys",
        danger: true,
    };
    let boxes = list.lines.iter().any(|l| list.may_revoke(l));
    let mut tools = String::new();
    if boxes {
        tools.push_str(&bulk_button(&bulk));
    }
    if list.can_create() {
        let _ = write!(
            tools,
            r#"<a class="button button--primary" href="{NEW_PATH}">{}New key</a>"#,
            icon("plus")
        );
    }
    let mut body = String::new();
    if list.cannot_use {
        body.push_str(r#"<p class="uiBanner uiBanner--inset uiBanner--warn"><span class="uiBanner__text">Your keys cannot reach a service: you do not have the right keys.own.use.</span></p>"#);
    }
    if boxes {
        body.push_str(&bulk_form(&bulk));
    }
    if shown.is_empty() {
        body.push_str(&empty_keys(list, query));
    } else {
        body.push_str(r#"<table class="grid grid--keys"><colgroup>"#);
        if boxes {
            body.push_str(r#"<col class="w28">"#);
        }
        body.push_str(r#"<col class="w28"><col class="w136">"#);
        if list.all {
            body.push_str(r#"<col class="w88 wide">"#);
        }
        body.push_str(
            r#"<col><col class="w120 wide"><col class="w120"><col class="w28"></colgroup><thead><tr>"#,
        );
        if boxes {
            let _ = write!(body, r#"<th scope="col">{}</th>"#, bulk_all_box(&bulk));
        }
        body.push_str(r#"<th scope="col"><span class="visually-hidden">Kind</span></th><th scope="col">Key</th>"#);
        if list.all {
            body.push_str(r#"<th scope="col" class="wide">Owner</th>"#);
        }
        body.push_str(r#"<th scope="col">Can use</th><th scope="col" class="r wide">Expires</th><th scope="col" class="r">Last used</th><th scope="col"><span class="visually-hidden">Edit</span></th></tr></thead><tbody>"#);
        for line in &shown {
            key_row(&mut body, list, &bulk, boxes, line);
        }
        body.push_str("</tbody></table>");
    }
    if list.truncated {
        let _ = write!(
            body,
            r#"<p class="empty">The list shows the first {} keys.</p>"#,
            list.lines.len()
        );
    }
    let count = if shown.len() == list.lines.len() {
        list.lines.len().to_string()
    } else {
        format!("{} of {}", shown.len(), list.lines.len())
    };
    zone(
        &Zone {
            id: "keys-title",
            title: "Keys",
            count: &count,
            count_title: "",
            chips_html: &chips_html,
            tools_html: &tools,
            class: "",
            above_html: "",
        },
        &body,
    )
}

/// The chip of a kind that is on goes off at a second click.
fn toggled(current: Option<KeyKind>, kind: KeyKind) -> Option<KeyKind> {
    (current != Some(kind)).then_some(kind)
}

/// What an empty list says: no key at all, or no key that the filters keep. A key holds the rights
/// of services or the rights of the inbox, so a filter on both gives none.
fn empty_keys(list: &KeysList, query: &Query) -> String {
    if list.lines.is_empty() {
        return r#"<p class="empty">No key yet.</p>"#.to_string();
    }
    let both = if !query.group.is_empty() && !query.service.is_empty() {
        " A key holds the rights of a service or the rights of a group, not both."
    } else {
        ""
    };
    format!(
        r#"<p class="empty">No key matches these filters.{both} <a href="{}">Clear</a></p>"#,
        query.cleared().href()
    )
}

// The pages -------------------------------------------------------------------------------------

/// The kind of a new key: a selector when the viewer may make two kinds or more, and nothing when
/// the viewer may make one. The chips are links, so the selector works without script. The kind
/// of a key that exists does not change: the page shows it as a fact.
fn kind_field(view: &KeysView<'_>, editing: bool) -> String {
    let Some(kind) = view.kind else {
        return String::new();
    };
    if editing {
        return format!(
            r#"<div class="field"><span class="field__label">Kind</span><span class="kindTag" title="{}">{}{}</span></div>"#,
            kind_tip(kind),
            icon(kind_icon(kind)),
            kind.label()
        );
    }
    if view.create_kinds.len() < 2 {
        return String::new();
    }
    let mut buttons = String::new();
    let mut kinds = view.create_kinds.clone();
    kinds.sort_by_key(|k| *k != KeyKind::Api);
    for each in &kinds {
        let _ = write!(
            buttons,
            r#"<a class="segmented__option{}" href="{NEW_PATH}?kind={}" title="{}"{}>{}{}</a>"#,
            if *each == kind { " is-active" } else { "" },
            each.name(),
            kind_tip(*each),
            if *each == kind {
                r#" aria-current="true""#
            } else {
                ""
            },
            icon(kind_icon(*each)),
            each.label()
        );
    }
    format!(
        r#"<div class="field"><span class="field__label">Kind</span><div class="segmented" role="group" aria-label="Kind of key">{buttons}</div></div>"#
    )
}

/// The message under a field that a refused form names, and the attributes that tie it to it.
fn field_error(id: &str, view: &KeysView<'_>, field: Field) -> (&'static str, String) {
    match view.notice.filter(|n| !n.ok && view.field == Some(field)) {
        Some(notice) => (
            "uiInput--error",
            format!(
                r#"<span class="field__error" id="{id}-error">{}</span>"#,
                html_escape(notice.text)
            ),
        ),
        None => ("", String::new()),
    }
}

/// The aside of the form of an API key: the calls that the ticked rights allow. The script fills it
/// from the editor, so without script it is absent. The boxes of the editor already show the rights:
/// the aside does not list them again.
const API_ASIDE: &str = r#"<aside class="keyAside" aria-label="What the key can do"><h2 class="title">Calls that these rights allow</h2><div class="mono-line calls" data-calls></div><p class="hint">The portal knows these calls from its own documentation. A service does not publish its calls in the action document.</p><div class="mono-line">Authorization: Bearer &lt;key&gt;</div><p class="hint">The key text shows one time, after Create. Copy it then.</p><p class="hint">The key holds at most the rights of its owner. A right that the owner loses has no effect.</p></aside>"#;

/// The page "New key" or "Edit key": the fields across the width, then the editor of the
/// permissions. An MCP key shows the services on the whole width. An API key shows the inbox on the
/// left and what it allows on the right. Cancel and the action are at the top right.
fn form_page(view: &KeysView<'_>) -> String {
    let edit = match &view.pane {
        Pane::Edit(target) => Some(target),
        _ => None,
    };
    let kind = view.kind;
    let title = match (edit, kind) {
        (Some(_), _) => "Edit key".to_string(),
        (None, Some(kind)) if view.create_kinds.len() < 2 => format!("New {} key", kind.label()),
        _ => "New key".to_string(),
    };
    let (action, submit) = match edit {
        Some(_) => (EDIT_PATH, "Save"),
        None => (CREATE_PATH, "Create"),
    };
    let tools = match edit {
        Some(target) => ask(&Ask {
            label: &format!("Revoke {}", target.name),
            icon: "trash",
            question: &format!("Revoke {}?", target.name),
            verb: "Revoke",
            action: REVOKE_PATH,
            fields_html: &format!(
                r#"<input type="hidden" name="csrf" value="{}"><input type="hidden" name="id" value="{}"><input type="hidden" name="section" value="mine">"#,
                html_escape(view.csrf),
                html_escape(&target.id)
            ),
            strength: Strength::Light,
            name: "",
            danger: true,
            side: Side::Start,
        }),
        None => String::new(),
    };
    let mut out = page_head(&PageHead {
        title: &title,
        tools_html: &tools,
        actions_html: &format!(
            r#"{}<button class="button button--primary" type="submit" form="key-form">{submit}</button>"#,
            cancel_link()
        ),
    });
    if view.field.is_some() && view.notice.is_some_and(|n| !n.ok) {
        out.push_str(&banner_error(
            "The key is not saved. Fix the field that shows an error.",
        ));
    }
    let _ = write!(
        out,
        r#"<form method="post" action="{action}" id="key-form" class="keyForm" data-gate><input type="hidden" name="csrf" value="{}">"#,
        html_escape(view.csrf)
    );
    if let (None, Some(kind)) = (edit, kind) {
        let _ = write!(
            out,
            r#"<input type="hidden" name="kind" value="{}">"#,
            kind.name()
        );
    }
    if let Some(target) = edit {
        let _ = write!(
            out,
            r#"<input type="hidden" name="id" value="{}">"#,
            html_escape(&target.id)
        );
    }
    let name = match edit {
        Some(target) => target.name.as_str(),
        None => view.copy_name.as_str(),
    };
    let (name_class, name_error) = field_error("key-name", view, Field::Name);
    // The kind comes first: a click on a kind loads the page again, and the fields after it stay
    // as they are before the choice.
    let _ = write!(
        out,
        r#"<div class="fgrid fgrid--key">{}<div class="field"><label class="field__label" for="key-name">Name</label><input class="uiInput {name_class}" type="text" id="key-name" name="name" value="{}" maxlength="64" required placeholder="1 to 64 characters" autocomplete="off"{}>{name_error}</div>"#,
        kind_field(view, edit.is_some()),
        html_escape(name),
        if name_error.is_empty() {
            ""
        } else {
            r#" aria-invalid="true" aria-describedby="key-name-error""#
        }
    );
    if edit.is_none() {
        let hint = match view.max_lifetime {
            Some(max) => format!(
                "The longest lifetime is {}.",
                crate::util::format_duration(i64::try_from(max).unwrap_or(i64::MAX))
            ),
            None => "UTC, for example 2026-11-23T00:00:00Z.".to_string(),
        };
        let (expiry_class, expiry_error) = field_error("key-expires", view, Field::Expiry);
        let _ = write!(
            out,
            r#"<div class="field expiry" data-expiry{}><label class="field__label" for="key-expires">Expires</label><input class="uiInput {expiry_class}" type="text" id="key-expires" name="expires_at" value="{}" size="22"{}><p class="uiField__hint expiry__hint">{}</p>{expiry_error}</div>"#,
            view.max_lifetime
                .map(|max| format!(r#" data-max="{max}""#))
                .unwrap_or_default(),
            html_escape(&view.expires_at),
            if expiry_error.is_empty() {
                ""
            } else {
                r#" aria-invalid="true" aria-describedby="key-expires-error""#
            },
            html_escape(&hint)
        );
    }
    out.push_str("</div></form>");
    if let Some(editor) = &view.editor {
        let mut grants = String::new();
        grant_editor::render(
            &mut grants,
            editor,
            Place {
                form: Some("key-form"),
                title: "Permissions of the key",
                blank_rows: 3,
                groups: true,
            },
        );
        if kind == Some(KeyKind::Api) {
            let _ = write!(out, r#"<div class="keyLayout">{grants}{API_ASIDE}</div>"#);
        } else {
            out.push_str(&grants);
        }
    }
    out
}

/// The page after a creation: the text of the key, one time.
fn made_page(key: &NewKey<'_>) -> String {
    let head = page_head(&PageHead {
        title: "Key made",
        tools_html: "",
        actions_html: &format!(
            r#"<a class="button button--primary" href="{}">Done</a>"#,
            super::settings::ACCESS_PATH
        ),
    });
    format!(
        r##"{head}<p class="uiBanner uiBanner--inset uiBanner--ok" role="status"><span class="uiBanner__text">The key <strong>{}</strong> is made. Copy its text now. The portal shows it one time and keeps only a digest.</span></p><div class="field"><label class="field__label" for="key-text">Key</label><div class="keyRow"><input class="uiInput uiInput--mono keyText" id="key-text" type="text" readonly value="{}" spellcheck="false" autocomplete="off"><button class="button copyButton" type="button" data-copy="key-text"><svg class="icon icon--sm copyButton__idle" aria-hidden="true"><use href="#i-copy"/></svg><svg class="icon icon--sm copyButton__done" aria-hidden="true"><use href="#i-check"/></svg>Copy</button></div></div><p class="uiBanner uiBanner--inset uiBanner--warn"><span class="uiBanner__text">Each request with this key acts as you. Do not put it in a connector that other persons use.{}</span></p>"##,
        html_escape(key.name),
        html_escape(key.text),
        if key.superadmin {
            " This key can have every right. Give it limits."
        } else {
            ""
        }
    )
}

// The page -------------------------------------------------------------------------------------

/// The page of a key: the form of a new key, the form of the change of a key, or the text of the
/// key that was just made.
pub fn keys_page(frame: &Frame, view: &KeysView<'_>) -> String {
    let mut out = String::new();
    // A refusal that names a field shows under that field. The other outcomes show as a banner.
    if let Some(service) = &view.needs_access {
        let name = view
            .editor
            .iter()
            .flat_map(|e| e.services.iter().chain(e.features.iter()))
            .find(|s| s.id == *service)
            .map_or(service.as_str(), |s| s.name.as_str());
        out.push_str(&banner_error(&super::settings::needs_access_text(name)));
    } else if view.field.is_none() {
        notice_html(&mut out, view.notice);
    }
    out.push_str(&match &view.pane {
        Pane::New | Pane::Edit(_) => form_page(view),
        Pane::Made(key) => made_page(key),
    });
    let title = match &view.pane {
        Pane::New => "New key",
        Pane::Edit(_) => "Edit key",
        Pane::Made(_) => "Key made",
    };
    fill_page(Tab::Access, frame, title, &out)
}

/// The grant of a new key of this kind that the form offers as a box, from a grant of the owner.
/// A key holds `access` on the service `portal` and the actions that its kind lists. An API key
/// holds no grant on another service.
pub fn is_offered(kind: KeyKind, grant: &Grant) -> bool {
    if grant.service == crate::portal_actions::SERVICE {
        grant.action == ACCESS
            || crate::portal_actions::key_actions(kind).contains(&grant.action.as_str())
    } else {
        kind == KeyKind::Mcp
    }
}

/// The text of a time in the form of the field `expires_at`.
pub fn instant_text(unix: i64) -> String {
    format_utc_instant(unix)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::fixtures::*;

    fn view(pane: Pane<'static>, kinds: &[KeyKind]) -> KeysView<'static> {
        KeysView {
            create_kinds: kinds.to_vec(),
            kind: kinds.first().copied(),
            notice: None,
            field: None,
            csrf: "tok<",
            editor: None,
            pane,
            expires_at: "2026-12-01T00:00:00Z".into(),
            max_lifetime: None,
            copy_name: String::new(),
            needs_access: None,
        }
    }

    const BOTH: [KeyKind; 2] = [KeyKind::Mcp, KeyKind::Api];

    fn page(view: &KeysView) -> String {
        keys_page(
            &Frame {
                superadmin: false,
                keys: true,
                ..frame()
            },
            view,
        )
    }

    #[test]
    fn the_form_is_a_full_page_with_cancel_and_the_action_at_the_top_right_and_no_list() {
        for pane in [
            Pane::New,
            Pane::Edit(EditTarget {
                id: "k1".into(),
                name: "ci <deploy>".into(),
            }),
        ] {
            let edit = matches!(pane, Pane::Edit(_));
            let mut v = view(pane, &BOTH);
            v.kind = Some(KeyKind::Api);
            let html = page(&v);
            let head = &html[html.find(r#"<div class="accountBar">"#).unwrap()..];
            let head = &head[..head
                .find(r#"<form method="post" action="/auth/keys/"#)
                .unwrap()];
            assert!(head.contains(r#"<a href="/auth/settings">Access</a>"#));
            assert!(head.contains(if edit {
                ">Edit key</h2>"
            } else {
                ">New key</h2>"
            }));
            assert!(head.contains(r#"<a class="button" href="/auth/settings">Cancel</a>"#));
            assert!(head.contains(&format!(
                r#"type="submit" form="key-form">{}</button>"#,
                if edit { "Save" } else { "Create" }
            )));
            assert!(
                !html.contains("New key</a>") && !html.contains("keys-title"),
                "no list, no second entry"
            );
            assert!(!html.contains("contentTabs"), "one tab: no strip");
        }
    }

    #[test]
    fn the_kind_selector_shows_with_two_kinds_and_only_then_and_its_chips_are_links() {
        let mut two = view(Pane::New, &BOTH);
        two.kind = Some(KeyKind::Mcp);
        let html = page(&two);
        assert!(html.contains(r#"<a class="segmented__option" href="/auth/keys/new?kind=api""#));
        assert!(html
            .contains(r#"<a class="segmented__option is-active" href="/auth/keys/new?kind=mcp""#));
        // The API kind comes first.
        assert!(html.find("kind=api").unwrap() < html.find("kind=mcp").unwrap());
        assert!(html.contains(">New key<") || html.contains("New key</h2>"));
        for one in [[KeyKind::Mcp], [KeyKind::Api]] {
            let mut single = view(Pane::New, &one);
            single.kind = Some(one[0]);
            let html = page(&single);
            assert!(!html.contains("segmented"), "{html}");
            assert!(html.contains(&format!("New {} key", one[0].label())));
            // The kind is a hidden field of the form.
            assert!(html.contains(&format!(
                r#"<input type="hidden" name="kind" value="{}">"#,
                one[0].name()
            )));
        }
    }

    #[test]
    fn the_change_of_a_key_shows_its_kind_as_a_fact_and_can_revoke_it_in_place() {
        let mut edit = view(
            Pane::Edit(EditTarget {
                id: "k1".into(),
                name: "ci <deploy>".into(),
            }),
            &BOTH,
        );
        edit.kind = Some(KeyKind::Api);
        let html = page(&edit);
        assert!(html.contains(r#"<span class="kindTag""#) && !html.contains("segmented"));
        assert!(html.contains(r#"<input type="hidden" name="id" value="k1">"#));
        assert!(html.contains(r#"action="/auth/keys/edit""#));
        assert!(html.contains(r#"value="ci &lt;deploy&gt;""#));
        assert!(
            !html.contains(r#"name="expires_at""#),
            "a key keeps its expiry date"
        );
        // A trash icon revokes the key with a confirmation in place. The form of the revoke is not
        // inside the form of the key.
        assert!(html.contains(r#"aria-label="Revoke ci &lt;deploy&gt;""#));
        assert!(html.contains(r#"action="/auth/keys/revoke""#));
        assert!(html.contains(r#"name="section" value="mine""#));
        let key_form = &html[html
            .find(r#"<form method="post" action="/auth/keys/edit""#)
            .unwrap()..];
        let key_form = &key_form[..key_form.find("</form>").unwrap()];
        assert!(!key_form[1..].contains("<form"), "no nested form");
    }

    #[test]
    fn a_refused_form_says_why_under_the_field_and_in_a_banner() {
        let mut v = view(Pane::New, &BOTH);
        v.notice = notice_for("bad_name");
        v.field = field_of("bad_name");
        let html = page(&v);
        assert!(html.contains("The key is not saved. Fix the field that shows an error."));
        assert!(html.contains(r#"<span class="field__error" id="key-name-error">The name of a key has 1 to 64 characters"#));
        assert!(html.contains(r#"aria-invalid="true" aria-describedby="key-name-error""#));
        v.notice = notice_for("bad_expiry");
        v.field = field_of("bad_expiry");
        let html = page(&v);
        assert!(html.contains(r#"id="key-expires-error">The expiry date is not valid."#));
        // The outcome of the whole form shows in a banner only.
        v.notice = notice_for("too_many_keys");
        v.field = field_of("too_many_keys");
        let html = page(&v);
        assert!(
            html.contains("You have the maximum number of active keys.")
                && !html.contains("field__error")
        );
    }

    #[test]
    fn an_api_key_shows_the_summary_and_the_calls_and_an_mcp_key_shows_only_the_editor() {
        use crate::ui::grant_editor::{self, EditorData};
        let editor = |data: &EditorData| {
            let _ = grant_editor::render;
            data.clone()
        };
        let data = EditorData {
            services: Vec::new(),
            features: None,
            ticked: Vec::new(),
            owner: None,
            locked: Vec::new(),
        };
        let mut api = view(Pane::New, &BOTH);
        api.kind = Some(KeyKind::Api);
        api.editor = Some(editor(&data));
        let html = page(&api);
        assert!(html.contains(r#"<div class="keyLayout">"#));
        assert!(html.contains("data-calls") && !html.contains("This key can"));
        assert!(html.contains("Authorization: Bearer"));
        assert!(html.contains(r#"data-form="key-form""#));
        let mut mcp = view(Pane::New, &BOTH);
        mcp.kind = Some(KeyKind::Mcp);
        mcp.editor = Some(editor(&data));
        let html = page(&mcp);
        assert!(!html.contains("keyLayout") && !html.contains("This key can"));
        assert!(!html.contains("The agent uses the services that you tick."));
    }

    #[test]
    fn the_kind_comes_first_and_the_form_has_no_label_without_a_control() {
        let mut two = view(Pane::New, &BOTH);
        two.kind = Some(KeyKind::Mcp);
        let html = page(&two);
        assert!(html.find("Kind of key").unwrap() < html.find(r#"id="key-name""#).unwrap());
        assert!(!html.contains(">Rights<") && !html.contains("field--note"));
    }

    #[test]
    fn a_form_refused_for_access_names_the_service_and_keeps_the_name() {
        let mut v = view(Pane::New, &BOTH);
        v.kind = Some(KeyKind::Mcp);
        v.copy_name = "my <agent>".into();
        v.needs_access = Some("grafana".into());
        let html = page(&v);
        assert!(
            html.contains("grafana: a permission needs the switch Can open grafana."),
            "{html}"
        );
        assert!(html.contains(r#"value="my &lt;agent&gt;""#));
        assert!(html.contains(r#"role="alert""#));
    }

    #[test]
    fn a_key_made_shows_its_text_one_time_with_a_copy_button() {
        let html = page(&view(
            Pane::Made(NewKey {
                text: "api_secret<",
                name: "ci <1>",
                superadmin: true,
            }),
            &BOTH,
        ));
        assert!(html.contains(r#"id="key-text" type="text" readonly value="api_secret&lt;""#));
        assert!(html.contains(r#"data-copy="key-text""#));
        assert!(html.contains("<strong>ci &lt;1&gt;</strong>") && html.contains("acts as you"));
        assert!(html.contains("This key can have every right"));
        assert!(
            html.contains(r#"<a class="button button--primary" href="/auth/settings">Done</a>"#)
        );
        assert!(!html.contains("key-form") && !html.contains(">Cancel</a>"));
    }

    #[test]
    fn a_page_of_a_key_has_no_inline_style_and_no_inline_script() {
        let html = page(&view(Pane::New, &BOTH));
        assert!(!html.contains(" style=") && !html.contains("<style"));
        assert!(!html.contains("<script>"));
    }
}
