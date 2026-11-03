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
    bulk_all_box, bulk_box, bulk_button, bulk_form, chips, icon, icon_link, zone, Bulk, Chip, Zone,
};
use super::grant_editor::{self, EditorData, Place};
use super::settings::{fill_page, notice_html, Frame, Notice, Tab};
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

/// The state of the pane.
pub enum Pane<'a> {
    /// The form of a new key. The view holds the kind and the editor.
    New,
    /// The form of the change of a key.
    Edit(EditTarget),
    /// The text of the key that was just made.
    Made(NewKey<'a>),
}

/// The data of the pages of a key.
pub struct KeysView<'a> {
    pub can_create: bool,
    /// The kinds of key that the viewer may make: the kinds that the file turns on and that the
    /// viewer holds `keys.own.create` on. The new-key page has a selector only with two or more.
    pub create_kinds: Vec<KeyKind>,
    /// The kind of the key that the form makes or changes.
    pub kind: Option<KeyKind>,
    pub notice: Option<Notice>,
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
    /// The user browses as another user: no change is possible (portal 3.7).
    pub impersonated: bool,
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

// The pane -------------------------------------------------------------------------------------

/// The head of the pane: the title, Cancel and the action. The action is a button of the form of
/// the pane, or a link when the pane has no form.
fn pane_head(title: &str, cancel: Option<&str>, action: &str) -> String {
    let mut out = format!(
        r#"<div class="formHead"><h2 class="title" id="key-pane-title">{}</h2><span class="grow"></span>"#,
        html_escape(title)
    );
    if let Some(cancel) = cancel {
        let _ = write!(out, r#"<a class="button" href="{cancel}">Cancel</a>"#);
    }
    out.push_str(action);
    out.push_str("</div>");
    out
}

/// The kind of a new key: a selector when the viewer may make two kinds or more, and nothing when
/// the viewer may make one. The selector is a form with the method GET, so it works without script:
/// each button is a submit button that names its kind.
fn kind_field(view: &KeysView<'_>, editing: bool) -> String {
    let Some(kind) = view.kind else {
        return String::new();
    };
    if editing {
        // The kind of a key does not change: the pane shows it as a fact.
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
            r#"<button class="segmented__option{}" type="submit" form="kind-form" name="kind" value="{}" title="{}"{}>{}{}</button>"#,
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

fn form_pane(view: &KeysView<'_>) -> String {
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
    let (action, form_id, submit) = match edit {
        Some(_) => (EDIT_PATH, "key-form", "Save"),
        None => (CREATE_PATH, "new-key-form", "Create"),
    };
    let mut out = String::from(r#"<section class="pane" aria-labelledby="key-pane-title">"#);
    out.push_str(&pane_head(
        &title,
        Some(PATH),
        &format!(
            r#"<button class="button button--primary" type="submit" form="{form_id}">{submit}</button>"#
        ),
    ));
    out.push_str(r#"<div class="pane__body">"#);
    if edit.is_none() && view.create_kinds.len() > 1 {
        // The selector is a form of its own: the buttons name it with the attribute `form`.
        let _ = write!(
            out,
            r#"<form id="kind-form" method="get" action="{NEW_PATH}"></form>"#
        );
    }
    let _ = write!(
        out,
        r#"<form method="post" action="{action}" id="{form_id}" class="keyForm" data-gate><input type="hidden" name="csrf" value="{}">"#,
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
    let _ = write!(
        out,
        r#"<div class="keyFields"><div class="field keyFields__name"><label class="field__label" for="key-name">Name</label><input class="uiInput" type="text" id="key-name" name="name" value="{}" maxlength="256" required></div>"#,
        html_escape(name),
    );
    out.push_str(&kind_field(view, edit.is_some()));
    if edit.is_none() {
        let hint = match view.max_lifetime {
            Some(max) => format!(
                "The longest lifetime is {}.",
                crate::util::format_duration(i64::try_from(max).unwrap_or(i64::MAX))
            ),
            None => "UTC, for example 2026-11-23T00:00:00Z.".to_string(),
        };
        let _ = write!(
            out,
            r#"<div class="field expiry" data-expiry{}><label class="field__label" for="key-expires">Expires</label><input class="uiInput" type="text" id="key-expires" name="expires_at" value="{}" size="22"><p class="uiField__hint expiry__hint">{}</p></div>"#,
            view.max_lifetime
                .map(|max| format!(r#" data-max="{max}""#))
                .unwrap_or_default(),
            html_escape(&view.expires_at),
            html_escape(&hint)
        );
    }
    out.push_str("</div>");
    if let Some(editor) = &view.editor {
        grant_editor::render(
            &mut out,
            editor,
            Place {
                form: None,
                title: "Permissions of the key",
                blank_rows: 3,
                groups: true,
            },
        );
    }
    out.push_str("</form></div></section>");
    out
}

/// The pane after a creation: the text of the key, one time.
fn made_pane(key: &NewKey<'_>) -> String {
    format!(
        r##"<section class="pane" aria-labelledby="key-pane-title">{}<div class="pane__body"><p class="uiBanner uiBanner--inset uiBanner--ok" role="status"><span class="uiBanner__text">The key <strong>{}</strong> is made. Copy its text now. The portal shows it one time and keeps only a digest.</span></p><div class="field"><label class="field__label" for="key-text">Key</label><div class="keyRow"><input class="uiInput uiInput--mono keyText" id="key-text" type="text" readonly value="{}" spellcheck="false" autocomplete="off"><button class="button copyButton" type="button" data-copy="key-text"><svg class="icon icon--sm copyButton__idle" aria-hidden="true"><use href="#i-copy"/></svg><svg class="icon icon--sm copyButton__done" aria-hidden="true"><use href="#i-check"/></svg>Copy</button></div></div><p class="uiBanner uiBanner--inset uiBanner--warn"><span class="uiBanner__text">Each request with this key acts as you. Do not put it in a connector that other persons use.{}</span></p></div></section>"##,
        pane_head(
            "Key made",
            None,
            &format!(r#"<a class="button button--primary" href="{PATH}">Done</a>"#)
        ),
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
    notice_html(&mut out, view.notice);
    let pane = match &view.pane {
        Pane::New | Pane::Edit(_) => form_pane(view),
        Pane::Made(key) => made_pane(key),
    };
    let _ = write!(out, r#"<div class="split split--one">{pane}</div>"#);
    fill_page(Tab::Access, frame, "Keys", &out)
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
