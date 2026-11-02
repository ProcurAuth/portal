//! The page "Keys" (portal 5.8): the keys of the user, the keys of all the users, the form of a new
//! key and the form of a change.
//!
//! A key is shown with its id, its name, its hint, its state, its dates, the time of its last check
//! and its grants with their limit values. The text of a key is shown one time, in the page that
//! the creation gives ([`new_key_page`]). The page draws no icon of its own, and it works without
//! script.

use std::fmt::Write as _;

use super::grant_editor::{self, EditorData, Place};
use super::settings::{notice_html, page_shell, part_head, plural, shell, Frame, Notice, Tab};
use crate::grants::{Grant, ACCESS};
use crate::keys::KeyKind;
use crate::limits::{Limits, Value, Window};
use crate::util::{format_duration, format_utc_instant, html_escape, time_tag};

/// The address of the page.
pub const PATH: &str = "/auth/keys";
pub const NEW_PATH: &str = "/auth/keys/new";
pub const CREATE_PATH: &str = "/auth/keys/create";
pub const EDIT_PATH: &str = "/auth/keys/edit";
/// `GET`: the page that changes a key. `POST`: the change.
pub const EDIT_PAGE: &str = "/auth/keys/edit";
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
    /// The values of the owner next to the values of the key (portal 4.5b).
    pub owner_values: Limits,
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

/// The key of the page of a change.
pub struct EditTarget {
    pub id: String,
    pub name: String,
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
    /// The kind of the key that the form makes or changes. `None` on the list.
    pub kind: Option<KeyKind>,
    pub can_revoke: bool,
    pub can_revoke_all: bool,
    /// The owner can make keys and cannot use them (portal 5.3b).
    pub cannot_use: bool,
    pub notice: Option<Notice>,
    pub lines: Vec<KeyLine>,
    /// The cursor of the next page (portal 5.8b), when there is one.
    pub next: Option<String>,
    pub csrf: &'a str,
    /// The editor of the grants of the form. `None`: the page has no form.
    pub editor: Option<EditorData>,
    /// The key that the form changes. `None`: the form makes a new key.
    pub edit: Option<EditTarget>,
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

fn window_label(window: Window) -> &'static str {
    match window {
        Window::Second => "each second",
        Window::Minute => "each minute",
        Window::Hour => "each hour",
        Window::Day => "each day",
    }
}

/// The values of a limit set, as text: `max_rows 100, requests 60 each minute`.
fn values_text(limits: &Limits) -> String {
    let mut parts = Vec::new();
    for (name, value) in limits.iter() {
        match value {
            Value::Ceiling(n) => parts.push(format!("{name} {n}")),
            Value::Budget(budget) => {
                let items: Vec<String> = budget
                    .items()
                    .iter()
                    .map(|i| format!("{} {}", i.amount, window_label(i.window)))
                    .collect();
                parts.push(format!("{name} {}", items.join(", ")));
            }
        }
    }
    parts.join("; ")
}

/// Whether the value of the key is above the value of the owner, so it has no effect
/// (portal 4.5b).
fn above_owner(name: &str, value: &Value, owner: &Limits) -> bool {
    match (value, owner.get(name)) {
        (Value::Ceiling(key), Some(Value::Ceiling(owner))) => key > owner,
        (Value::Budget(key), Some(Value::Budget(owner))) => key
            .items()
            .iter()
            .any(|i| owner.amount(i.window).is_some_and(|a| i.amount > a)),
        _ => false,
    }
}

fn grant_line(out: &mut String, line: &GrantLine) {
    let _ = write!(
        out,
        "<li><code>{}</code>",
        html_escape(&line.grant.to_string())
    );
    match line.cover {
        Cover::Full => {}
        Cover::Limited => out.push_str(
            r#" <span class="badge badge--warn" title="Your rights cover only a part of this node.">limited</span>"#,
        ),
        Cover::None => out.push_str(
            r#" <span class="badge badge--warn" title="Your rights do not cover this permission any more.">without effect</span>"#,
        ),
    }
    if !line.grant.limits.is_empty() {
        out.push_str(r#"<span class="cellNote">"#);
        for (name, value) in line.grant.limits.iter() {
            let mut text = values_text(&single(name, value));
            if above_owner(name, value, &line.owner_values) {
                let _ = write!(
                    text,
                    " (without effect: your value is {})",
                    values_text(&single_of(name, &line.owner_values))
                );
            }
            let _ = write!(out, "{} ", html_escape(&text));
        }
        out.push_str("</span>");
    }
    out.push_str("</li>");
}

fn single(name: &str, value: &Value) -> Limits {
    let mut limits = Limits::new();
    let _ = limits.set(name, value.clone());
    limits
}

fn single_of(name: &str, from: &Limits) -> Limits {
    from.get(name)
        .map(|value| single(name, value))
        .unwrap_or_default()
}

/// The page of a new key, and the page of the change of a key: a page of its own, with the name,
/// the expiry date of a new key and the grants.
pub fn key_form_page(frame: &Frame, view: &KeysView<'_>) -> String {
    let edit = view.edit.as_ref();
    let title = match edit {
        Some(target) => format!("Change {}", html_escape(&target.name)),
        None => "New key".to_string(),
    };
    let (action, form_id, submit) = match edit {
        Some(_) => (EDIT_PATH, "key-form", "Save"),
        None => (CREATE_PATH, "new-key-form", "Create key"),
    };
    let mut out = String::new();
    notice_html(&mut out, view.notice);
    out.push_str(r#"<section class="pagePart" aria-labelledby="key-form-title">"#);
    // The selector of the kind shows only for a new key, and only when the viewer may make more
    // than one kind. It is a form that asks for the page again: the form of the key depends on the
    // kind. Without a second kind, there is no selector.
    let selector = if edit.is_none() && view.create_kinds.len() > 1 {
        let options: String = view
            .create_kinds
            .iter()
            .map(|kind| {
                format!(
                    r#"<option value="{}"{}>{}</option>"#,
                    kind.name(),
                    if Some(*kind) == view.kind {
                        " selected"
                    } else {
                        ""
                    },
                    kind.label()
                )
            })
            .collect();
        format!(
            r#"<form class="kindChoice" method="get" action="{NEW_PATH}"><label class="uiField__label" for="key-kind">Kind of key</label> <select class="uiInput" id="key-kind" name="kind">{options}</select> <button class="button" type="submit">Show the form</button></form>"#
        )
    } else {
        String::new()
    };
    let _ = write!(
        out,
        r##"<div class="pagePart__head"><a class="pageBack" href="{PATH}" aria-label="Back to the keys" title="Back to the keys"><svg class="icon icon--lg" aria-hidden="true"><use href="#i-arrow-left"/></svg></a><h2 class="pagePart__title" id="key-form-title">{title}</h2><div class="part__controls"><a class="button" href="{PATH}">Cancel</a><button class="button button--primary" type="submit" form="{form_id}">{submit}</button></div></div>"##
    );
    out.push_str(&selector);
    let _ = write!(
        out,
        r#"<form method="post" action="{action}" id="{form_id}" class="uiForm keyForm" data-gate><input type="hidden" name="csrf" value="{}">"#,
        html_escape(view.csrf)
    );
    if let (None, Some(kind)) = (edit, view.kind) {
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
        r#"<div class="keyForm__top"><div class="uiField"><label class="uiField__label" for="key-name">Name</label><input class="uiInput" type="text" id="key-name" name="name" value="{}" maxlength="256" required></div>"#,
        html_escape(name),
    );
    if edit.is_none() {
        let hint = match view.max_lifetime {
            Some(max) => format!(
                "The longest lifetime is {}. The date is in UTC, for example 2026-11-23T00:00:00Z.",
                format_duration(i64::try_from(max).unwrap_or(i64::MAX))
            ),
            None => "Empty: the key has no expiry date. The date is in UTC, for example 2026-11-23T00:00:00Z."
                .to_string(),
        };
        let _ = write!(
            out,
            r#"<div class="uiField expiry" data-expiry{}><label class="uiField__label" for="key-expires">Expires</label><input class="uiInput" type="text" id="key-expires" name="expires_at" value="{}" size="22"><p class="uiField__hint expiry__hint">{}</p></div>"#,
            view.max_lifetime
                .map(|max| format!(r#" data-max="{max}""#))
                .unwrap_or_default(),
            html_escape(&view.expires_at),
            html_escape(&hint)
        );
    }
    out.push_str("</div>");
    out.push_str(r#"<h3 class="subTitle">Permissions</h3>"#);
    if let Some(editor) = &view.editor {
        grant_editor::render(
            &mut out,
            editor,
            Place {
                form: None,
                title: "Permissions of the key",
                blank_rows: 3,
                groups: false,
            },
        );
    }
    out.push_str("</form></section>");
    page_shell(Tab::Keys, frame, &title, &out)
}

fn key_row(out: &mut String, view: &KeysView<'_>, line: &KeyLine) {
    // One line: the state and the name, what the key can use, when it ends, when it was used.
    // The id, the hint and the creation date are in the tooltip of the name.
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
    let kind_badge = match line.kind {
        Some(kind) => format!(
            r#" <span class="badge" data-kind="{}" title="{}">{}</span>"#,
            kind.name(),
            match kind {
                KeyKind::Mcp => "An MCP key: for an agent. The key edge verifies it.",
                KeyKind::Api => "An API key: for a service. The API of the inbox verifies it.",
            },
            kind.label()
        ),
        None => String::new(),
    };
    let _ = write!(
        out,
        r#"<tr><th scope="row" data-label="Key">{} <span class="svcRow__name" title="Id {}.{hint}{created}">{}</span>{kind_badge}{}"#,
        line.state.icon(),
        html_escape(&line.id),
        html_escape(&line.name),
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
    out.push_str("</th>");
    if view.section == Section::All {
        let _ = write!(
            out,
            r#"<td data-label="Owner">{}</td>"#,
            match &line.owner {
                Some(login) => html_escape(login),
                None => "superadmin".to_string(),
            }
        );
    }
    out.push_str(r#"<td class="foldCell" data-label="Can use">"#);
    if line.grants.is_empty() {
        out.push_str(r#"<span class="muted">nothing</span>"#);
    } else {
        let mut services: Vec<&str> = line
            .grants
            .iter()
            .map(|g| g.grant.service.as_str())
            .collect();
        services.sort_unstable();
        services.dedup();
        let shown: Vec<&str> = services.iter().take(3).copied().collect();
        let _ = write!(
            out,
            r#"<details class="fold"><summary>{}{} · {}"#,
            html_escape(&shown.join(", ")),
            if services.len() > 3 {
                format!(" +{}", services.len() - 3)
            } else {
                String::new()
            },
            plural(line.grants.len(), "permission", "permissions")
        );
        if !line.no_access.is_empty() {
            out.push_str(&super::status_icon(
                "warn",
                "alert-triangle",
                "A service of this key has no access permission",
            ));
        }
        out.push_str(r#"</summary><ul class="grantList">"#);
        for grant in &line.grants {
            grant_line(out, grant);
        }
        out.push_str("</ul>");
        for service in &line.no_access {
            let _ = write!(
                out,
                r#"<p class="uiBanner uiBanner--inset uiBanner--info">This key cannot reach this service ({}): it has no access permission.</p>"#,
                html_escape(service)
            );
        }
        out.push_str("</details>");
    }
    out.push_str("</td>");
    let _ = write!(
        out,
        r#"<td class="num" data-label="Expires">{}</td><td class="num" data-label="Last used" title="{} checks">{}</td>"#,
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
    out.push_str(r#"<td data-label="Actions" class="keyActions">"#);
    let may_revoke = line.revoked_at.is_none()
        && !line.file
        && match view.section {
            Section::Mine => view.can_revoke && !view.impersonated,
            Section::All => view.can_revoke_all && !view.impersonated,
        };
    if line.can_change {
        let _ = write!(
            out,
            r##"<a class="button button--small keyAction" href="{EDIT_PAGE}?id={}" title="Change this key"><svg class="icon icon--sm" aria-hidden="true"><use href="#i-pencil"/></svg><span class="keyAction__label">Change</span></a>"##,
            html_escape(&line.id)
        );
    }
    if view.can_create && !line.file && line.revoked_at.is_none() && view.section == Section::Mine {
        let _ = write!(
            out,
            r##"<a class="button button--small keyAction" href="{NEW_PATH}?copy={}" title="Make a key like this one"><svg class="icon icon--sm" aria-hidden="true"><use href="#i-copy"/></svg><span class="keyAction__label">Make a key like this one</span></a>"##,
            html_escape(&line.id)
        );
    }
    if may_revoke {
        let _ = write!(
            out,
            r##"<form method="post" action="{REVOKE_PATH}"><input type="hidden" name="csrf" value="{}"><input type="hidden" name="id" value="{}"><input type="hidden" name="section" value="{}"><button class="button button--small keyAction keyAction--danger" type="submit" title="Revoke this key"><svg class="icon icon--sm" aria-hidden="true"><use href="#i-trash"/></svg><span class="keyAction__label">Revoke</span></button></form>"##,
            html_escape(view.csrf),
            html_escape(&line.id),
            view.section.name()
        );
    }
    out.push_str("</td></tr>");
}

/// The page "Keys".
pub fn keys_page(frame: &Frame, view: &KeysView<'_>) -> String {
    let mut out = String::new();
    notice_html(&mut out, view.notice);
    out.push_str(r#"<section class="pagePart" aria-labelledby="keys-title">"#);
    let mut sections = String::new();
    sections.push_str(
        r#"<div class="segmented segmented--compact" role="group" aria-label="Section">"#,
    );
    if view.can_read_own {
        let _ = write!(
            sections,
            r#"<a class="segmented__option{}" href="{PATH}?section=mine"{}>Mine</a>"#,
            if view.section == Section::Mine {
                " is-active"
            } else {
                ""
            },
            if view.section == Section::Mine {
                r#" aria-current="true""#
            } else {
                ""
            }
        );
    }
    if view.can_read_all {
        let _ = write!(
            sections,
            r#"<a class="segmented__option{}" href="{PATH}?section=all"{}>All</a>"#,
            if view.section == Section::All {
                " is-active"
            } else {
                ""
            },
            if view.section == Section::All {
                r#" aria-current="true""#
            } else {
                ""
            }
        );
    }
    sections.push_str("</div>");
    if view.can_create && view.section == Section::Mine && !view.impersonated {
        let _ = write!(
            sections,
            r##" <a class="button button--primary" href="{NEW_PATH}"><svg class="icon icon--sm" aria-hidden="true"><use href="#i-plus"/></svg>New key</a>"##
        );
    }
    part_head(
        &mut out,
        "keys-title",
        "Keys",
        &plural(view.lines.len(), "line", "lines"),
        &sections,
    );
    out.push_str(r#"<p class="part__lede">A key lets a program act as its owner, with at most the rights of its owner. The portal shows the text of a key one time. Send it in the header <code>Authorization: Bearer</code>.</p>"#);
    if view.cannot_use {
        out.push_str(r#"<p class="uiBanner uiBanner--inset uiBanner--warn">Your keys cannot reach a service: you do not have the right keys.own.use.</p>"#);
    }
    if view.lines.is_empty() {
        out.push_str(r#"<div class="uiState uiState--compact"><p class="uiState__body">No key yet.</p></div>"#);
    } else {
        let _ = write!(
            out,
            r#"<div class="dataTableWrap"><table class="dataTable stack keysTable"><thead><tr><th scope="col">Key</th>{owner}<th scope="col">Can use</th><th scope="col" class="num">Expires</th><th scope="col" class="num">Last used</th><th scope="col"><span class="visually-hidden">Actions</span></th></tr></thead><tbody>"#,
            owner = if view.section == Section::All {
                r#"<th scope="col">Owner</th>"#
            } else {
                ""
            }
        );
        for line in &view.lines {
            key_row(&mut out, view, line);
        }
        out.push_str("</tbody></table></div>");
    }
    if let Some(cursor) = &view.next {
        let _ = write!(
            out,
            r#"<p><a class="button" href="{PATH}?section={}&amp;after={}">Next keys</a></p>"#,
            view.section.name(),
            html_escape(cursor)
        );
    }
    out.push_str("</section>");
    page_shell(Tab::Keys, frame, "Keys", &out)
}

/// The page that shows the text of a new key one time (portal 5.2, 5.8c).
pub fn new_key_page(frame: &Frame, key: &NewKey<'_>) -> String {
    let mut out = String::new();
    out.push_str(r#"<div class="pageCols"><div class="pageCols__main"><section class="pagePart" aria-labelledby="new-key-title">"#);
    part_head(&mut out, "new-key-title", "The key is made", "", "");
    let _ = write!(
        out,
        r##"<p class="uiBanner uiBanner--inset uiBanner--ok" role="status">The key <strong>{name}</strong> is made. Copy its text now. The portal shows it one time, and it keeps only a digest.</p><div class="uiField"><label class="uiField__label" for="key-text">Key</label><div class="keyRow"><input class="uiInput uiInput--mono keyText" id="key-text" type="text" readonly value="{text}" spellcheck="false" autocomplete="off"><button class="button copyButton" type="button" data-copy="key-text"><svg class="icon icon--sm copyButton__idle" aria-hidden="true"><use href="#i-copy"/></svg><svg class="icon icon--sm copyButton__done" aria-hidden="true"><use href="#i-check"/></svg>Copy</button></div></div><p class="uiBanner uiBanner--inset uiBanner--warn">Each request with this key acts as you. Do not put it in a connector that other persons use.{every}</p>"##,
        name = html_escape(key.name),
        text = html_escape(key.text),
        every = if key.superadmin {
            " This key can have every right. Give it limits."
        } else {
            ""
        }
    );
    let _ = write!(
        out,
        r#"<p><a class="button button--primary" href="{PATH}">Back to the keys</a></p></section></div></div>"#
    );
    shell(&frame.tabs(), Tab::Keys, frame, "New key", &out, true)
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
