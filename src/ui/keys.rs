//! The page "Keys" (portal 5.8): the keys of the user and of all the users on the left, and the
//! form of a key in a pane on the right.
//!
//! A key has one kind, API or MCP, and the list shows it with an icon. The pane has four states:
//! closed, a new key, the change of a key, and the text of a key that was just made. The text of a
//! key shows one time, in the pane of the creation, with `Cache-Control: no-store`. The page works
//! without script: the kind selector is a small form that asks for the page again, and a bulk
//! revoke is a form with checkboxes.
//!
//! Each action has one entry point. With the pane closed, the toolbar has "New key". With the pane
//! open, the toolbar button is absent, and the head of the pane has the title, Cancel and the
//! action.

use std::fmt::Write as _;

use super::components::{
    bulk_all_box, bulk_box, bulk_button, bulk_form, chips, icon, icon_link, toolbar, zone, Bulk,
    Chip, Zone,
};
use super::grant_editor::{self, EditorData, Place};
use super::settings::{fill_page, notice_html, Frame, Notice, Tab};
use crate::grants::{Grant, ACCESS};
use crate::keys::KeyKind;
use crate::util::{format_utc_instant, html_escape, time_tag};

/// The address of the page.
pub const PATH: &str = "/auth/keys";
/// `GET`: the page with the pane of a new key open.
pub const NEW_PATH: &str = "/auth/keys/new";
pub const CREATE_PATH: &str = "/auth/keys/create";
/// `GET`: the page with the pane of the change of a key open. `POST`: the change.
pub const EDIT_PATH: &str = "/auth/keys/edit";
pub const REVOKE_PATH: &str = "/auth/keys/revoke";

/// The sections of the page (portal 5.8).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Section {
    Mine,
    All,
}

impl Section {
    pub fn name(self) -> &'static str {
        match self {
            Self::Mine => "mine",
            Self::All => "all",
        }
    }
}

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
    /// No pane: the list has the full width, and the toolbar has "New key".
    Closed,
    /// The form of a new key. The view holds the kind and the editor.
    New,
    /// The form of the change of a key.
    Edit(EditTarget),
    /// The text of the key that was just made.
    Made(NewKey<'a>),
}

/// The data of the page.
pub struct KeysView<'a> {
    pub section: Section,
    pub can_read_own: bool,
    pub can_read_all: bool,
    pub can_create: bool,
    /// The kinds of key that the viewer may make: the kinds that the file turns on and that the
    /// viewer holds `keys.own.create` on. The new-key page has a selector only with two or more.
    pub create_kinds: Vec<KeyKind>,
    /// The kind of the key that the form makes or changes. `None` with the pane closed.
    pub kind: Option<KeyKind>,
    /// The filter of the list: only the keys of this kind.
    pub kind_filter: Option<KeyKind>,
    /// The names of the groups of the inbox, for the column "Can use".
    pub groups: Vec<(String, String)>,
    pub can_revoke: bool,
    pub can_revoke_all: bool,
    /// The owner can make keys and cannot use them (portal 5.3b).
    pub cannot_use: bool,
    pub notice: Option<Notice>,
    pub lines: Vec<KeyLine>,
    /// The cursor of the next page (portal 5.8b), when there is one.
    pub next: Option<String>,
    pub csrf: &'a str,
    /// The editor of the grants of the form. `None`: the pane has no form.
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

/// The address of the page with a section and a kind filter.
fn list_href(section: Section, kind: Option<KeyKind>) -> String {
    let mut query = Vec::new();
    if section == Section::All {
        query.push("section=all".to_string());
    }
    if let Some(kind) = kind {
        query.push(format!("k={}", kind.name()));
    }
    if query.is_empty() {
        PATH.to_string()
    } else {
        format!("{PATH}?{}", query.join("&"))
    }
}

/// What a key can use, in one short line: the groups of the inbox that it sends to and reads, or
/// the services of an MCP key. A mark shows a right that the owner lost.
fn can_use(view: &KeysView<'_>, line: &KeyLine) -> (String, String) {
    if line.grants.is_empty() {
        return (r#"<span class="muted">nothing</span>"#.to_string(), String::new());
    }
    let portal = crate::portal_actions::SERVICE;
    let name_of = |path: &str| {
        let id = path.strip_prefix("inbox_group:").unwrap_or(path);
        view.groups
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
            r#"<span class="feat feat--text">{}{}</span>"#,
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
    let lost = line.grants.iter().filter(|g| g.cover == Cover::None).count();
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
    (out, plain.join("; "))
}

/// Whether the row has a box: the account may revoke the key.
fn may_revoke(view: &KeysView<'_>, line: &KeyLine) -> bool {
    line.revoked_at.is_none()
        && !line.file
        && match view.section {
            Section::Mine => view.can_revoke && !view.impersonated,
            Section::All => view.can_revoke_all && !view.impersonated,
        }
}

fn key_row(out: &mut String, view: &KeysView<'_>, bulk: &Bulk, boxes: bool, line: &KeyLine) {
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
    let editing = matches!(&view.pane, Pane::Edit(target) if target.id == line.id);
    let _ = write!(
        out,
        r#"<tr{} data-kind="{}">"#,
        if editing { r#" class="is-selected""# } else { "" },
        line.kind.map_or("", KeyKind::name)
    );
    if boxes {
        let _ = write!(
            out,
            "<td>{}</td>",
            if may_revoke(view, line) {
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
    if view.section == Section::All {
        let _ = write!(
            out,
            r#"<td class="muted">{}</td>"#,
            match &line.owner {
                Some(login) => html_escape(login),
                None => "superadmin".to_string(),
            }
        );
    }
    let (can_use_html, can_use_text) = can_use(view, line);
    let _ = write!(
        out,
        r#"<td title="{}">{}</td><td class="r">{}</td><td class="r" title="{} checks">{}</td><td class="r">"#,
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

fn list_zone(view: &KeysView<'_>) -> String {
    let lines: Vec<&KeyLine> = view
        .lines
        .iter()
        .filter(|l| view.kind_filter.is_none_or(|k| l.kind == Some(k)))
        .collect();
    let count_kind = |kind: KeyKind| view.lines.iter().filter(|l| l.kind == Some(kind)).count();
    let all_href = list_href(view.section, None);
    let api_href = list_href(
        view.section,
        (view.kind_filter != Some(KeyKind::Api)).then_some(KeyKind::Api),
    );
    let mcp_href = list_href(
        view.section,
        (view.kind_filter != Some(KeyKind::Mcp)).then_some(KeyKind::Mcp),
    );
    let api_label = format!(r#"<span class="st--off">{}</span>"#, icon("webhook"));
    let mcp_label = format!(r#"<span class="st--off">{}</span>"#, icon("robot"));
    let chips_html = chips(&[
        Chip {
            label_html: "All",
            name: "",
            count: view.lines.len(),
            href: &all_href,
            on: view.kind_filter.is_none(),
        },
        Chip {
            label_html: &api_label,
            name: KIND_TIP_API,
            count: count_kind(KeyKind::Api),
            href: &api_href,
            on: view.kind_filter == Some(KeyKind::Api),
        },
        Chip {
            label_html: &mcp_label,
            name: KIND_TIP_MCP,
            count: count_kind(KeyKind::Mcp),
            href: &mcp_href,
            on: view.kind_filter == Some(KeyKind::Mcp),
        },
    ]);
    let bulk = Bulk {
        id: "revoke-keys",
        action: REVOKE_PATH,
        field: "id",
        fields_html: &format!(
            r#"<input type="hidden" name="csrf" value="{}"><input type="hidden" name="section" value="{}">"#,
            html_escape(view.csrf),
            view.section.name()
        ),
        verb: "Revoke",
        one: "key",
        many: "keys",
        danger: true,
    };
    let boxes = view.lines.iter().any(|l| may_revoke(view, l));
    let tools = if boxes { bulk_button(&bulk) } else { String::new() };
    let mut body = String::new();
    if boxes {
        body.push_str(&bulk_form(&bulk));
    }
    if lines.is_empty() {
        body.push_str(r#"<p class="empty">No key yet.</p>"#);
    } else {
        body.push_str(r#"<table class="grid grid--keys"><colgroup>"#);
        if boxes {
            body.push_str(r#"<col class="w28">"#);
        }
        body.push_str(r#"<col class="w28"><col class="w152">"#);
        if view.section == Section::All {
            body.push_str(r#"<col class="w88">"#);
        }
        body.push_str(r#"<col><col class="w88"><col class="w152"><col class="w28"></colgroup><thead><tr>"#);
        if boxes {
            let _ = write!(body, r#"<th scope="col">{}</th>"#, bulk_all_box(&bulk));
        }
        body.push_str(r#"<th scope="col"><span class="visually-hidden">Kind</span></th><th scope="col">Key</th>"#);
        if view.section == Section::All {
            body.push_str(r#"<th scope="col">Owner</th>"#);
        }
        body.push_str(r#"<th scope="col">Can use</th><th scope="col" class="r">Expires</th><th scope="col" class="r">Last used</th><th scope="col"><span class="visually-hidden">Edit</span></th></tr></thead><tbody>"#);
        for line in &lines {
            key_row(&mut body, view, &bulk, boxes, line);
        }
        body.push_str("</tbody></table>");
    }
    if let Some(cursor) = &view.next {
        let _ = write!(
            body,
            r#"<p class="more"><a href="{PATH}?section={}&amp;after={}">Next keys</a></p>"#,
            view.section.name(),
            html_escape(cursor)
        );
    }
    zone(
        &Zone {
            id: "keys-list",
            title: "",
            count: "",
            chips_html: &chips_html,
            tools_html: &tools,
            class: "",
            above_html: "",
        },
        &body,
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
            if *each == kind { r#" aria-current="true""# } else { "" },
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
        r##"<section class="pane" aria-labelledby="key-pane-title">{}<div class="pane__body"><p class="uiBanner uiBanner--inset uiBanner--ok" role="status">The key <strong>{}</strong> is made. Copy its text now. The portal shows it one time and keeps only a digest.</p><div class="field"><label class="field__label" for="key-text">Key</label><div class="keyRow"><input class="uiInput uiInput--mono keyText" id="key-text" type="text" readonly value="{}" spellcheck="false" autocomplete="off"><button class="button copyButton" type="button" data-copy="key-text"><svg class="icon icon--sm copyButton__idle" aria-hidden="true"><use href="#i-copy"/></svg><svg class="icon icon--sm copyButton__done" aria-hidden="true"><use href="#i-check"/></svg>Copy</button></div></div><p class="uiBanner uiBanner--inset uiBanner--warn">Each request with this key acts as you. Do not put it in a connector that other persons use.{}</p></div></section>"##,
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

/// The page "Keys": the list, and the pane when it is open.
pub fn keys_page(frame: &Frame, view: &KeysView<'_>) -> String {
    let mut out = String::new();
    notice_html(&mut out, view.notice);
    if view.cannot_use {
        out.push_str(r#"<p class="uiBanner uiBanner--inset uiBanner--warn">Your keys cannot reach a service: you do not have the right keys.own.use.</p>"#);
    }
    let mut tools = String::new();
    if view.can_read_own && view.can_read_all {
        let item = |section: Section, label: &str| {
            let on = view.section == section;
            format!(
                r#"<a class="segmented__option{}" href="{}"{}>{label}</a>"#,
                if on { " is-active" } else { "" },
                list_href(section, None),
                if on { r#" aria-current="true""# } else { "" },
            )
        };
        let _ = write!(
            tools,
            r#"<div class="segmented" role="group" aria-label="Section">{}{}</div>"#,
            item(Section::Mine, "Mine"),
            item(Section::All, "All"),
        );
    }
    // One entry point: "New key" shows only while the pane is closed.
    let closed = matches!(view.pane, Pane::Closed);
    if closed
        && view.can_create
        && !view.create_kinds.is_empty()
        && view.section == Section::Mine
        && !view.impersonated
    {
        let _ = write!(
            tools,
            r#"<a class="button button--primary" href="{NEW_PATH}">{}New key</a>"#,
            icon("plus")
        );
    }
    out.push_str(&toolbar("Keys", &view.lines.len().to_string(), &tools));
    let list = list_zone(view);
    let pane = match &view.pane {
        Pane::Closed => None,
        Pane::New | Pane::Edit(_) => Some(form_pane(view)),
        Pane::Made(key) => Some(made_pane(key)),
    };
    match pane {
        Some(pane) => {
            let _ = write!(out, r#"<div class="split split--even">{list}{pane}</div>"#);
        }
        None => {
            let _ = write!(out, r#"<div class="split split--one">{list}</div>"#);
        }
    }
    fill_page(Tab::Keys, frame, "Keys", &out)
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
    use crate::grants::Grant;

    fn frame() -> Frame<'static> {
        Frame {
            logout_token: "out",
            keys: true,
            inbox: true,
            superadmin: false,
        }
    }

    fn line(id: &str, name: &str, kind: KeyKind, grants: &[(&str, Cover)]) -> KeyLine {
        KeyLine {
            id: id.into(),
            owner: Some("alice".into()),
            name: name.into(),
            hint: "AbCd".into(),
            kind: Some(kind),
            state: KeyState::Active,
            file: false,
            created_at: 1_700_000_000,
            expires_at: None,
            last_check_at: None,
            check_count: 0,
            revoked_at: None,
            grants: grants
                .iter()
                .map(|(text, cover)| GrantLine {
                    grant: Grant::parse(text).unwrap(),
                    cover: *cover,
                })
                .collect(),
            no_access: Vec::new(),
            can_change: true,
        }
    }

    fn view(pane: Pane<'static>, kinds: &[KeyKind]) -> KeysView<'static> {
        KeysView {
            section: Section::Mine,
            can_read_own: true,
            can_read_all: true,
            can_create: !kinds.is_empty(),
            create_kinds: kinds.to_vec(),
            kind: kinds.first().copied(),
            kind_filter: None,
            groups: vec![("infra".into(), "Infra <b>".into()), ("db".into(), "Database".into())],
            can_revoke: true,
            can_revoke_all: false,
            cannot_use: false,
            notice: None,
            lines: vec![
                line(
                    "k1",
                    "ci <deploy>",
                    KeyKind::Api,
                    &[
                        ("alert.send on service:portal/inbox_group:infra", Cover::Full),
                        ("alert.send on service:portal/inbox_group:db", Cover::None),
                        ("alert.read on service:portal/inbox_group:infra", Cover::Full),
                    ],
                ),
                line(
                    "k2",
                    "agent",
                    KeyKind::Mcp,
                    &[("access on service:notes", Cover::Full), ("notes.read on service:notes", Cover::Full)],
                ),
            ],
            next: None,
            csrf: "tok<",
            editor: None,
            pane,
            expires_at: "2026-12-01T00:00:00Z".into(),
            max_lifetime: None,
            copy_name: String::new(),
            impersonated: false,
        }
    }

    const BOTH: [KeyKind; 2] = [KeyKind::Mcp, KeyKind::Api];

    fn page(view: &KeysView) -> String {
        keys_page(&frame(), view)
    }

    #[test]
    fn the_entry_point_is_the_toolbar_button_or_the_pane_action_and_never_both() {
        let closed = page(&view(Pane::Closed, &BOTH));
        assert!(closed.contains(r#"<a class="button button--primary" href="/auth/keys/new">"#));
        assert!(!closed.contains(r#"class="pane""#) && !closed.contains(">Create</button>"));
        for pane in [
            Pane::New,
            Pane::Edit(EditTarget {
                id: "k1".into(),
                name: "ci".into(),
            }),
        ] {
            let mut open = view(pane, &BOTH);
            open.kind = Some(KeyKind::Api);
            let html = page(&open);
            assert!(!html.contains("New key</a>"), "{html}");
            assert!(!html.contains(r#"href="/auth/keys/new""#));
            assert!(html.contains(">Cancel</a>") && html.contains(r#"type="submit" form=""#));
            assert!(html.contains(">Create</button>") || html.contains(">Save</button>"));
        }
        // Made: the pane has "Done" and the toolbar has no button.
        let made = page(&view(
            Pane::Made(NewKey {
                text: "api_secret<",
                name: "ci",
                superadmin: false,
            }),
            &BOTH,
        ));
        assert!(!made.contains("New key</a>") && made.contains(r#"href="/auth/keys">Done</a>"#));
        assert!(!made.contains(">Create</button>") && !made.contains(">Cancel</a>"));
    }

    #[test]
    fn the_kind_selector_shows_with_two_kinds_and_only_then() {
        let mut two = view(Pane::New, &BOTH);
        two.kind = Some(KeyKind::Mcp);
        let html = page(&two);
        assert!(html.contains(r#"<form id="kind-form" method="get" action="/auth/keys/new"></form>"#));
        assert!(html.contains(r#"form="kind-form" name="kind" value="api""#));
        assert!(html.contains(r#"form="kind-form" name="kind" value="mcp""#));
        // The API kind comes first.
        assert!(
            html.find(r#"name="kind" value="api""#).unwrap()
                < html.find(r#"name="kind" value="mcp""#).unwrap()
        );
        assert!(html.contains(">New key<") || html.contains("New key</h2>"));
        for one in [[KeyKind::Mcp], [KeyKind::Api]] {
            let mut single = view(Pane::New, &one);
            single.kind = Some(one[0]);
            let html = page(&single);
            assert!(!html.contains("kind-form"), "{html}");
            assert!(html.contains(&format!("New {} key", one[0].label())));
            // The kind is a hidden field of the form.
            assert!(html.contains(&format!(r#"<input type="hidden" name="kind" value="{}">"#, one[0].name())));
        }
        // With no kind to make, there is no "New key" and no pane.
        let none = page(&view(Pane::Closed, &[]));
        assert!(!none.contains("New key") && !none.contains("/auth/keys/new"));
    }

    #[test]
    fn the_change_of_a_key_shows_its_kind_as_a_fact_and_not_as_a_choice() {
        let mut edit = view(
            Pane::Edit(EditTarget {
                id: "k1".into(),
                name: "ci <deploy>".into(),
            }),
            &BOTH,
        );
        edit.kind = Some(KeyKind::Api);
        let html = page(&edit);
        assert!(html.contains(r#"<span class="kindTag""#) && !html.contains("kind-form"));
        assert!(html.contains(r#"<input type="hidden" name="id" value="k1">"#));
        assert!(html.contains(r#"action="/auth/keys/edit""#) && html.contains(">Save</button>"));
        assert!(html.contains(r#"value="ci &lt;deploy&gt;""#));
        assert!(!html.contains(r#"name="expires_at""#), "a key keeps its expiry date");
        // The row of the key is selected.
        assert!(html.contains(r#"<tr class="is-selected" data-kind="api">"#));
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
        assert!(!html.contains("new-key-form"), "the pane has no form any more");
    }

    #[test]
    fn the_list_has_kind_icons_kind_chips_and_a_bulk_revoke_and_no_button_in_a_row() {
        let html = page(&view(Pane::Closed, &BOTH));
        assert!(html.contains("#i-webhook") && html.contains("#i-robot"));
        assert!(html.contains(r#"aria-label="API key: for services and webhooks""#));
        assert!(html.contains(r#"href="/auth/keys?k=api""#) && html.contains(r#"href="/auth/keys?k=mcp""#));
        assert!(html.contains(r#"<form id="revoke-keys" class="bulk" method="post" action="/auth/keys/revoke">"#));
        assert!(html.contains(r#"name="section" value="mine""#));
        assert!(html.contains(r#"name="id.k1" value="1""#) && html.contains(r#"name="id.k2" value="1""#));
        assert!(html.contains("Revoke the ticked keys?"));
        assert!(!html.contains("Make a key like this one") && !html.contains("keyAction"));
        // The columns, with the groups of the inbox named and escaped.
        assert!(html.contains("Send Infra &lt;b&gt;, Database &middot; Read Infra &lt;b&gt;") || html.contains("Send Infra &lt;b&gt;, Database · Read Infra &lt;b&gt;"), "{html}");
        assert!(html.contains("notes · 2 permissions"));
        assert!(html.contains(r#"href="/auth/keys/edit?id=k1""#) && html.contains(r#"aria-label="Edit ci &lt;deploy&gt;""#));
        // A right that the owner lost shows the mark.
        assert!(html.contains("1 right has no effect"));
        // The filter keeps one kind.
        let mut api = view(Pane::Closed, &BOTH);
        api.kind_filter = Some(KeyKind::Api);
        let html = page(&api);
        assert!(html.contains(">ci &lt;deploy&gt;<") && !html.contains(">agent<"));
        assert!(html.contains(r#"class="chip is-on" href="/auth/keys?k=api""#) || html.contains(r#"class="chip is-on" href="/auth/keys""#));
        // No box and no bulk button without the right to revoke.
        let mut quiet = view(Pane::Closed, &BOTH);
        quiet.can_revoke = false;
        let html = page(&quiet);
        assert!(!html.contains("revoke-keys") && !html.contains("bulk__box"));
    }

    #[test]
    fn the_pane_has_no_inline_style_and_no_inline_script() {
        for pane in [Pane::Closed, Pane::New] {
            let html = page(&view(pane, &BOTH));
            assert!(!html.contains(" style=") && !html.contains("<style"));
            assert!(!html.contains("<script>"));
        }
    }
}
