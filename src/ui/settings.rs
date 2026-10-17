//! The settings of the superadmin. There is one HTML page for each tab (Users, Services, Logins).
//! The pages follow the manner of clickhouse-dash. Each page has a header, a row of underlined tab
//! links, and then the page content.
//!
//! A page carries only what its tab shows. So the browser never builds the DOM of the other tabs.
//! The launcher carries none of it. Everything works without script. The Users page loads one small
//! script for its password generator. The graphs are inline SVG. This code builds them from
//! attributes only, because the CSP forbids `style`. Everything that comes from outside is escaped.
//! This includes logins (typed or stored), addresses, user agents and service names.

use std::fmt::Write as _;

use super::{document, sign_out_form, theme_switch};
use crate::catalog::{Catalog, Service};
use crate::grants::{holds, Grant, Mode, ACCESS};
use crate::store::{
    AccountActivity, AuditRow, DayStat, LoginPage, LoginRow, ServiceUse, SessionRow,
};
use crate::util::{
    format_duration, format_utc, format_utc_secs, html_escape, short_agent, TYPED_LOGIN_MAX,
};

pub const USERS_PATH: &str = "/auth/settings";
pub const SERVICES_PATH: &str = "/auth/settings/services";
pub const LOGINS_PATH: &str = "/auth/settings/logins";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tab {
    Users,
    Services,
    Logins,
}

impl Tab {
    const ALL: [Tab; 3] = [Tab::Users, Tab::Services, Tab::Logins];

    fn path(self) -> &'static str {
        match self {
            Self::Users => USERS_PATH,
            Self::Services => SERVICES_PATH,
            Self::Logins => LOGINS_PATH,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Users => "Users",
            Self::Services => "Services",
            Self::Logins => "Logins",
        }
    }
}

/// The data that every settings page needs around its content.
pub struct Frame<'a> {
    pub logout_token: &'a str,
}

/// The outcome of the last action. The outcome is one code from a fixed list. The URL never
/// carries text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Notice {
    pub text: &'static str,
    pub ok: bool,
    /// The outcome of the add-user form. The form stays open, so the next try needs one click.
    pub add_form: bool,
}

pub fn notice_for(code: &str) -> Option<Notice> {
    let (text, ok, add_form) = match code {
        "user-added" => (
            "User added. A new user has no grant: give them with Edit.",
            true,
            false,
        ),
        "user-updated" => ("Account updated.", true, false),
        "password-reset" => ("Password changed. Their sessions are closed.", true, false),
        "unchanged" => ("Nothing to change.", true, false),
        "user-deleted" => ("User deleted. Their sessions are closed.", true, false),
        "exists" => ("That login already exists.", false, true),
        "reserved" => ("That login is reserved for the superadmin.", false, true),
        "invalid-login" => (
            "Invalid login: 1 to 32 characters among a-z, 0-9, dot, dash and underscore.",
            false,
            true,
        ),
        "weak-password" => (
            "The password must have at least 10 characters.",
            false,
            true,
        ),
        "limit" => ("The user limit is reached.", false, true),
        "edit-weak-password" => (
            "The password must have at least 10 characters.",
            false,
            false,
        ),
        "edit-unknown-grant" => (
            "A grant of the form is not offered any more. Open the page again.",
            false,
            false,
        ),
        "grants-static" => (
            "The grants come from the configuration only (access.grants_mode is static).",
            false,
            false,
        ),
        "grant-added" => ("Grant added.", true, false),
        "grant-deleted" => ("Grant removed.", true, false),
        "grant-bad-id" => (
            "Invalid resource id: 1 to 128 characters, without a space.",
            false,
            false,
        ),
        "grant-undeclared" => (
            "The service does not declare this resource type for this action. Open the page again.",
            false,
            false,
        ),
        "grant-not-known" => (
            "The service has not given its actions yet. Try again in a moment.",
            false,
            false,
        ),
        "grant-limit" => ("The user has the largest number of grants.", false, false),
        "no-grant" => ("That grant does not exist.", false, false),
        "no-user" => ("That user does not exist.", false, false),
        "error" => ("The action failed. See the portal logs.", false, false),
        _ => return None,
    };
    Some(Notice { text, ok, add_form })
}

/// The page around a tab. It has a header, the tab links, then `content`. `script` loads the
/// generator script.
fn page(tab: Tab, frame: &Frame, content: &str, script: bool) -> String {
    let mut body = String::new();
    let _ = write!(
        body,
        r##"<header class="appHeader"><div class="appHeader__left"><a class="backLink" href="/"><svg class="icon" aria-hidden="true"><use href="#i-chevron-left"/></svg>Back to panel</a></div><div class="appHeader__right">{}{}</div></header><nav class="navRow" aria-label="Settings"><div class="contentTabs">"##,
        theme_switch(),
        sign_out_form(frame.logout_token),
    );
    for each in Tab::ALL {
        if each == tab {
            let _ = write!(
                body,
                r#"<a class="contentTabs__tab is-active" href="{}" aria-current="page">{}</a>"#,
                each.path(),
                each.label()
            );
        } else {
            let _ = write!(
                body,
                r#"<a class="contentTabs__tab" href="{}">{}</a>"#,
                each.path(),
                each.label()
            );
        }
    }
    let _ = write!(
        body,
        r#"</div></nav><main class="settings"><h1 class="visually-hidden">Settings: {}</h1>{content}</main>"#,
        tab.label()
    );
    if script {
        body.push_str(r#"<script src="/auth/assets/settings.js" defer></script>"#);
    }
    document(
        &format!("Settings: {}", tab.label()),
        r#"<link rel="stylesheet" href="/auth/settings.css">"#,
        &[
            "i-device-desktop",
            "i-moon",
            "i-sun",
            "i-logout",
            "i-chevron-left",
            "i-alert-triangle",
        ],
        &body,
    )
}

fn plural(n: u32, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

fn opt(value: &Option<String>) -> String {
    match value {
        Some(text) if !text.is_empty() => html_escape(text),
        _ => "-".to_string(),
    }
}

// Users ---------------------------------------------------------------------------------------

/// One account and the data that is known about it.
pub struct UserCard {
    pub login: String,
    /// All the grants that apply. The list is empty for the superadmin, who has all grants.
    pub grants: Vec<Grant>,
    /// The grants of the configuration (a `user` block). These grants are read-only on the page.
    pub configured: Vec<Grant>,
    pub created_at: Option<i64>,
    pub superadmin: bool,
    pub activity: AccountActivity,
}

/// A service and the actions that the page offers for it. The fields are `access`, then `actions`.
#[derive(Clone, Debug)]
pub struct ServiceActions {
    pub id: String,
    pub name: String,
    /// The name and the description of each action that the service publishes.
    pub actions: Vec<(String, String)>,
    /// The actions that a grant on a sub-resource can name. Each action has its resource types,
    /// without `service`.
    pub sub_resources: Vec<(String, Vec<String>)>,
    /// The service answered: its actions and its resource types are known.
    pub known: bool,
}

impl ServiceActions {
    /// The service is known, and it does not declare the resource type of this grant on a
    /// sub-resource for its action.
    fn undeclared(&self, grant: &Grant) -> bool {
        let Some(resource) = &grant.resource else {
            return false;
        };
        self.known
            && !self
                .sub_resources
                .iter()
                .any(|(action, kinds)| *action == grant.action && kinds.contains(&resource.kind))
    }

    /// Each resource type of the service, one time, sorted.
    fn resource_types(&self) -> Vec<&str> {
        let mut kinds: Vec<&str> = self
            .sub_resources
            .iter()
            .flat_map(|(_, kinds)| kinds.iter().map(String::as_str))
            .collect();
        kinds.sort_unstable();
        kinds.dedup();
        kinds
    }
}

pub struct UsersView<'a> {
    pub notice: Option<Notice>,
    /// Where the grants come from.
    pub mode: Mode,
    /// The whole catalogue, not the filtered catalogue for display.
    pub catalog: &'a Catalog,
    /// The services of the catalogue with their actions. The order is the order of the catalogue.
    pub services: Vec<ServiceActions>,
    pub users: Vec<UserCard>,
    pub superadmin: UserCard,
    /// Every real connection, per day.
    pub days: Vec<DayStat>,
    pub first_day: i64,
    pub recent_events: Vec<AuditRow>,
    pub unknown_failures_30d: u32,
    pub max_users: usize,
    /// The anti-CSRF token of the administration forms.
    pub token: &'a str,
}

pub fn users_page(frame: &Frame, view: &UsersView) -> String {
    let mut out = String::new();
    if let Some(notice) = view.notice {
        // A failure interrupts the page. The page announces the failure at once.
        let _ = write!(
            out,
            r#"<div class="notice {}" role="{}"><p>{}</p></div>"#,
            if notice.ok {
                "notice--ok"
            } else {
                "notice--error"
            },
            if notice.ok { "status" } else { "alert" },
            html_escape(notice.text)
        );
    }

    // The first items are the ones the administrator needs most: who the accounts are, and what
    // went wrong.
    let failed = view.unknown_failures_30d
        + view.superadmin.activity.failed_30d
        + view
            .users
            .iter()
            .map(|u| u.activity.failed_30d)
            .sum::<u32>();
    let _ = write!(
        out,
        r#"<section class="panel" aria-labelledby="accounts-title"><div class="panel__head"><h2 class="panel__title" id="accounts-title">Accounts</h2><span class="hint">{} of {} users</span>"#,
        view.users.len(),
        view.max_users
    );
    if failed > 0 {
        let _ = write!(
            out,
            r#"<span class="pill pill--danger">{} in 30 days</span>"#,
            plural(failed, "failed sign-in", "failed sign-ins")
        );
    }
    out.push_str("</div>");
    if view.unknown_failures_30d > 0 {
        let _ = write!(
            out,
            r#"<p class="hint">{} tried a login that does not exist: see <a href="{LOGINS_PATH}">Logins</a>.</p>"#,
            view.unknown_failures_30d
        );
    }
    add_form(&mut out, view);
    out.push_str(r#"<div class="userList">"#);
    user_card(&mut out, view, &view.superadmin);
    for user in &view.users {
        user_card(&mut out, view, user);
    }
    out.push_str("</div></section>");

    let total: u32 = view.days.iter().map(|d| d.sessions).sum();
    let _ = write!(
        out,
        r#"<section class="panel" aria-labelledby="connections-title"><div class="panel__head"><h2 class="panel__title" id="connections-title">Connections</h2></div><div class="overview"><div class="overview__text"><p class="hint">{total} in the last {} days, all accounts.{} Times are UTC.</p></div>{}</div></section>"#,
        view.days.len(),
        peak_text(&view.days, view.first_day),
        spark(&view.days, true),
    );

    out.push_str(
        r#"<section class="panel" aria-labelledby="events-title"><div class="panel__head"><h2 class="panel__title" id="events-title">Recent events</h2></div>"#,
    );
    if view.recent_events.is_empty() {
        out.push_str(r#"<p class="hint">No event yet.</p>"#);
    } else {
        out.push_str(
            r#"<div class="tableScroll"><table class="table"><thead><tr><th>When (UTC)</th><th>Event</th><th>Account</th><th>By</th><th>Address</th></tr></thead><tbody>"#,
        );
        for event in &view.recent_events {
            let _ = write!(
                out,
                "<tr><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td></tr>",
                format_utc(event.ts),
                audit_label(&event.kind),
                opt(&event.login),
                opt(&event.actor),
                opt(&event.ip),
            );
        }
        out.push_str("</tbody></table></div>");
    }
    out.push_str("</section>");
    incident_panel(&mut out, view);
    page(Tab::Users, frame, &out, true)
}

/// The incident tool. One click opens the confirmation. A second click ends every session, the
/// session of the superadmin included.
fn incident_panel(out: &mut String, view: &UsersView) {
    let _ = write!(
        out,
        r#"<section class="panel" aria-labelledby="incident-title"><div class="panel__head"><h2 class="panel__title" id="incident-title">Incident</h2></div><p class="hint">Ends every session of every account, yours included. Each user must sign in again. Accounts, grants and history stay.</p><details class="confirm"><summary class="button button--danger">End all sessions</summary><form method="post" action="/auth/users/end-sessions"><input type="hidden" name="csrf" value="{}"><input type="hidden" name="confirm" value="end-all-sessions"><p class="hint">You will be signed out too.</p><button class="button button--danger" type="submit">Confirm: end all sessions</button></form></details></section>"#,
        html_escape(view.token)
    );
}

/// Example text for a graph: "Peak: 7 on 2026-10-04." The text puts the values in the page.
/// The values do not live in tooltips only.
fn peak_text(days: &[DayStat], first_day: i64) -> String {
    let best = days
        .iter()
        .enumerate()
        .max_by_key(|(index, day)| (day.sessions, std::cmp::Reverse(*index)));
    match best {
        Some((index, day)) if day.sessions > 0 => {
            let date = format_utc(first_day + index as i64 * 86_400);
            format!(" Peak: {} on {}.", day.sessions, &date[..10])
        }
        _ => String::new(),
    }
}

/// The grant boxes of the edit form. There is one row for each service, and `access` comes first.
/// A grant of the configuration is ticked and cannot change. In the mode `static`, no box can change
/// and the form sends no grants.
fn grant_boxes(out: &mut String, view: &UsersView, user: &UserCard) {
    if view.services.is_empty() {
        out.push_str(r#"<p class="hint">No service in the catalogue.</p>"#);
        return;
    }
    out.push_str(r#"<div class="grantList">"#);
    for service in &view.services {
        let _ = write!(
            out,
            r#"<fieldset class="grantBoxes"><legend>{}</legend>"#,
            html_escape(&service.name)
        );
        let actions = std::iter::once((ACCESS, "Open the service.")).chain(
            service
                .actions
                .iter()
                .map(|(n, d)| (n.as_str(), d.as_str())),
        );
        for (action, description) in actions {
            let held = holds(&user.grants, action, &service.id);
            let fixed = !view.mode.editable() || holds(&user.configured, action, &service.id);
            let title = if fixed && view.mode.editable() {
                "From the configuration: it cannot change here.".to_string()
            } else {
                description.to_string()
            };
            let _ = write!(
                out,
                r#"<label class="check"{}><input type="checkbox"{}{}{}><span>{}</span></label>"#,
                if title.is_empty() {
                    String::new()
                } else {
                    format!(r#" title="{}""#, html_escape(&title))
                },
                if fixed {
                    String::new()
                } else {
                    format!(
                        r#" name="g.{}.{}" value="1""#,
                        html_escape(&service.id),
                        html_escape(action)
                    )
                },
                if held { " checked" } else { "" },
                if fixed { " disabled" } else { "" },
                html_escape(action),
            );
        }
        out.push_str("</fieldset>");
    }
    out.push_str("</div>");
    match view.mode {
        Mode::Static => out.push_str(
            r#"<p class="hint">The grants come from the configuration (<code>user</code> blocks). This page does not change them.</p>"#,
        ),
        Mode::Hybrid => out.push_str(
            r#"<p class="hint">A box that cannot change is a grant of the configuration.</p>"#,
        ),
        Mode::Dynamic => {}
    }
}

/// A password field with its "Generate" button (script). The same block has the line that says what
/// was generated.
fn password_field(out: &mut String, id: &str, label: &str, required: bool, hint: &str) {
    // The id can contain a login. Escape the id like all data that comes from outside.
    let id = html_escape(id);
    let _ = write!(
        out,
        r#"<div class="field"><label for="{id}">{label}</label><div class="passwordRow"><input id="{id}" name="password" type="password" minlength="10"{} autocomplete="new-password" autocapitalize="off" spellcheck="false" aria-describedby="{id}-hint {id}-status"><button class="button generate" type="button" data-generate-password="{id}">Generate</button></div><p class="hint" id="{id}-hint">{hint}</p><p class="hint" id="{id}-status" aria-live="polite"></p></div>"#,
        if required { " required" } else { "" },
    );
}

fn add_form(out: &mut String, view: &UsersView) {
    // The add action is a rare task. It has one button. The form opens after a refused attempt.
    let _ = write!(
        out,
        r#"<details class="addUser"{}><summary class="button button--primary">Add a user</summary><form method="post" action="/auth/users/add"><input type="hidden" name="csrf" value="{}"><div class="addUser__fields"><div class="field"><label for="new-login">Login</label><input id="new-login" name="login" type="text" maxlength="32" pattern="[A-Za-z0-9._\-]{{1,32}}" required autocomplete="off" autocapitalize="off" spellcheck="false" aria-describedby="new-login-hint"><p class="hint" id="new-login-hint">1 to 32 characters: letters, digits, dot, dash, underscore.</p></div>"#,
        if view.notice.is_some_and(|n| n.add_form) {
            " open"
        } else {
            ""
        },
        html_escape(view.token)
    );
    password_field(
        out,
        "new-password",
        "Password",
        true,
        "At least 10 characters.",
    );
    out.push_str("</div>");
    out.push_str(
        r#"<p class="hint">A new user has no grant and sees no service. Give the grants with Edit.</p><button class="button button--primary" type="submit">Add user</button></form></details>"#,
    );
}

fn user_card(out: &mut String, view: &UsersView, user: &UserCard) {
    let activity = &user.activity;
    let last = activity
        .last_login
        .map(format_utc)
        .unwrap_or_else(|| "never".to_string());
    let _ = write!(
        out,
        r#"<details class="user"><summary class="user__summary"><span class="user__name">{}</span>"#,
        html_escape(&user.login)
    );
    if user.superadmin {
        out.push_str(r#"<span class="pill pill--accent">superadmin</span>"#);
    } else {
        let open = allowed_services(view, user).len();
        let _ = write!(
            out,
            r#"<span class="pill{}">{}</span>"#,
            if open == 0 { " pill--muted" } else { "" },
            match open {
                0 => "no service".to_string(),
                1 => "1 service".to_string(),
                n => format!("{n} services"),
            }
        );
    }
    let _ = write!(
        out,
        r#"<span class="user__meta">last sign-in {last} &middot; {} in 30 days</span></summary><div class="user__body">"#,
        activity.sessions_30d
    );
    let _ = write!(
        out,
        r#"<div class="user__graph">{}<p class="hint">Connections per day, last {} days.{}{}</p></div>"#,
        spark(&activity.days, false),
        activity.days.len(),
        peak_text(&activity.days, view.first_day),
        match (user.created_at, activity.failed_30d) {
            (Some(at), 0) => format!(" Created {}.", format_utc(at)),
            (Some(at), n) => format!(
                " Created {}. {}.",
                format_utc(at),
                plural(n, "failed sign-in", "failed sign-ins")
            ),
            (None, 0) => String::new(),
            (None, n) => format!(" {}.", plural(n, "failed sign-in", "failed sign-ins")),
        },
    );

    out.push_str(r#"<h3>Latest sessions</h3>"#);
    if activity.recent.is_empty() {
        out.push_str(r#"<p class="hint">No session recorded.</p>"#);
    } else {
        out.push_str(
            r#"<div class="tableScroll"><table class="table"><thead><tr><th>Started (UTC)</th><th>Address</th><th>Browser</th><th>Duration</th><th>Ended</th></tr></thead><tbody>"#,
        );
        for session in &activity.recent {
            session_row(out, session);
        }
        out.push_str("</tbody></table></div>");
    }

    out.push_str(r#"<h3>Services used</h3>"#);
    if activity.services.is_empty() {
        out.push_str(r#"<p class="hint">No service used yet.</p>"#);
    } else {
        out.push_str(
            r#"<div class="tableScroll"><table class="table"><thead><tr><th>Service</th><th>Requests</th><th>Last use (UTC)</th></tr></thead><tbody>"#,
        );
        for used in &activity.services {
            service_use_row(out, view.catalog, used);
        }
        out.push_str("</tbody></table></div>");
    }

    out.push_str(r#"<h3>Allowed services</h3>"#);
    let allowed = allowed_services(view, user);
    if allowed.is_empty() {
        out.push_str(r#"<p class="hint">No service.</p>"#);
    } else {
        out.push_str(r#"<p class="chips">"#);
        for service in allowed {
            let _ = write!(
                out,
                r#"<span class="pill pill--plain">{}</span>"#,
                html_escape(&service.name)
            );
        }
        out.push_str("</p>");
    }

    if !user.superadmin {
        sub_resource_grants(out, view, user);
    }

    if !user.superadmin {
        let login = html_escape(&user.login);
        let token = html_escape(view.token);
        let _ = write!(
            out,
            r#"<div class="actions"><form method="post" action="/auth/users/impersonate"><input type="hidden" name="csrf" value="{token}"><input type="hidden" name="login" value="{login}"><button class="button" type="submit">Log in as {login}</button></form>"#
        );
        let _ = write!(
            out,
            r#"<details class="confirm confirm--edit"><summary class="button">Edit</summary><form method="post" action="/auth/users/update"><input type="hidden" name="csrf" value="{token}"><input type="hidden" name="login" value="{login}">"#
        );
        grant_boxes(out, view, user);
        password_field(
            out,
            &format!("pw-{}", user.login),
            "New password",
            false,
            "Leave empty to keep the current one. Changing it ends their sessions.",
        );
        out.push_str(r#"<button class="button button--primary" type="submit">Save</button></form></details>"#);
        let _ = write!(
            out,
            r#"<details class="confirm"><summary class="button button--danger">Delete</summary><form method="post" action="/auth/users/delete"><input type="hidden" name="csrf" value="{token}"><input type="hidden" name="login" value="{login}"><p class="hint">Closes their sessions. Their history is kept until it expires.</p><button class="button button--danger" type="submit">Confirm delete</button></form></details></div>"#
        );
    }
    out.push_str("</div></details>");
}

/// The grants of a user on sub-resources, by service, with the forms that add and remove them.
/// A service shows when the user has a grant on one of its sub-resources, or when the service
/// declares resource types and the grants can change. The page does not list the resources: the
/// superadmin types the id.
fn sub_resource_grants(out: &mut String, view: &UsersView, user: &UserCard) {
    let editable = view.mode.editable();
    let held: Vec<&Grant> = user
        .grants
        .iter()
        .filter(|grant| grant.resource.is_some())
        .collect();
    // The services of the catalogue first, then the services that left it.
    let mut groups: Vec<(&str, &str, Option<&ServiceActions>)> = view
        .services
        .iter()
        .map(|s| (s.id.as_str(), s.name.as_str(), Some(s)))
        .collect();
    for grant in &held {
        if !groups.iter().any(|(id, _, _)| *id == grant.service) {
            groups.push((&grant.service, &grant.service, None));
        }
    }
    let login = html_escape(&user.login);
    let token = html_escape(view.token);
    let mut body = String::new();
    for (id, name, service) in groups {
        let mine: Vec<&Grant> = held
            .iter()
            .filter(|grant| grant.service == id)
            .copied()
            .collect();
        let offer = editable && service.is_some_and(|s| !s.sub_resources.is_empty());
        if mine.is_empty() && !offer {
            continue;
        }
        let _ = write!(
            body,
            r#"<div class="subGrants__service"><p class="subGrants__name">{}</p>"#,
            html_escape(name)
        );
        if !mine.is_empty() {
            body.push_str(r#"<ul class="subGrants__list">"#);
            for grant in mine {
                let Some(resource) = &grant.resource else {
                    continue;
                };
                let _ = write!(
                    body,
                    r#"<li><code>{}</code> on <code>{}:{}</code>"#,
                    html_escape(&grant.action),
                    html_escape(&resource.kind),
                    html_escape(&resource.id)
                );
                if service.is_some_and(|s| s.undeclared(grant)) {
                    body.push_str(r##"<span class="warn" role="img" aria-label="The service does not declare this resource type for this action. The PDP does not apply this grant." title="The service does not declare this resource type for this action. The PDP does not apply this grant."><svg class="icon" aria-hidden="true"><use href="#i-alert-triangle"/></svg></span>"##);
                }
                if user.configured.contains(grant) {
                    body.push_str(r#" <span class="pill pill--muted" title="From the configuration: it cannot change here.">configuration</span>"#);
                } else if editable {
                    let _ = write!(
                        body,
                        r#"<form class="subGrants__remove" method="post" action="/auth/users/grant-delete"><input type="hidden" name="csrf" value="{token}"><input type="hidden" name="login" value="{login}"><input type="hidden" name="grant" value="{}"><button class="button" type="submit">Remove</button></form>"#,
                        html_escape(&grant.to_string())
                    );
                }
                body.push_str("</li>");
            }
            body.push_str("</ul>");
        }
        if let Some(service) = service.filter(|_| offer) {
            let _ = write!(
                body,
                r#"<form class="subGrant" method="post" action="/auth/users/grant-add"><input type="hidden" name="csrf" value="{token}"><input type="hidden" name="login" value="{login}"><input type="hidden" name="service" value="{}"><label class="field">Action<select name="action" required>"#,
                html_escape(&service.id)
            );
            for (action, _) in &service.sub_resources {
                let _ = write!(body, "<option>{}</option>", html_escape(action));
            }
            body.push_str(r#"</select></label><label class="field">Resource type<select name="type" required>"#);
            for kind in service.resource_types() {
                let _ = write!(body, "<option>{}</option>", html_escape(kind));
            }
            body.push_str(r#"</select></label><label class="field">Resource id<input name="id" type="text" required maxlength="128" pattern="\S+" autocomplete="off" autocapitalize="off" spellcheck="false"></label><button class="button" type="submit">Add grant</button></form>"#);
        }
        body.push_str("</div>");
    }
    out.push_str("<h3>Grants on sub-resources</h3>");
    if body.is_empty() {
        out.push_str(if editable {
            r#"<p class="hint">No grant on a sub-resource. No service declares a resource type.</p>"#
        } else {
            r#"<p class="hint">No grant on a sub-resource.</p>"#
        });
    } else {
        let _ = write!(out, r#"<div class="subGrants">{body}</div>"#);
    }
}

/// The services that the user can open. These are the services with the grant `access`. The
/// superadmin can open all services.
fn allowed_services<'a>(view: &'a UsersView, user: &UserCard) -> Vec<&'a Service> {
    view.catalog
        .groups()
        .iter()
        .flat_map(|group| group.services.iter())
        .filter(|service| user.superadmin || holds(&user.grants, ACCESS, &service.id))
        .collect()
}

fn session_row(out: &mut String, session: &SessionRow) {
    let (duration, ended) = match session.ended_at {
        Some(end) => (
            format_duration(end - session.started_at),
            end_label(session.end_reason.as_deref()),
        ),
        None => (
            format!(
                "{} so far",
                format_duration(session.last_seen_at - session.started_at)
            ),
            "Active",
        ),
    };
    let _ = write!(
        out,
        r#"<tr><td>{}{}</td><td>{}</td><td title="{}">{}</td><td>{}</td><td>{}</td></tr>"#,
        format_utc(session.started_at),
        if session.impersonated {
            r#" <span class="pill pill--muted">via superadmin</span>"#
        } else {
            ""
        },
        opt(&session.ip),
        html_escape(&session.user_agent),
        html_escape(&short_agent(&session.user_agent)),
        duration,
        ended,
    );
}

fn service_use_row(out: &mut String, catalog: &Catalog, used: &ServiceUse) {
    let name = catalog
        .groups()
        .iter()
        .flat_map(|g| g.services.iter())
        .find(|s| s.id == used.service_id)
        .map(|s| s.name.as_str())
        .unwrap_or(used.service_id.as_str());
    let _ = write!(
        out,
        "<tr><td>{}</td><td>{}</td><td>{}</td></tr>",
        html_escape(name),
        used.hits,
        format_utc(used.last_at)
    );
}

fn end_label(reason: Option<&str>) -> &'static str {
    match reason {
        Some("logout") => "Signed out",
        Some("expired") => "Expired",
        Some("idle") => "Idle timeout",
        Some("revoked") => "Revoked",
        Some("restart") => "Portal restarted",
        Some("user_deleted") => "User deleted",
        Some("impersonation") => "Switched account",
        _ => "Ended",
    }
}

fn audit_label(kind: &str) -> &'static str {
    match kind {
        "login_failed" => "Failed sign-in",
        "user_added" => "User added",
        "user_deleted" => "User deleted",
        "user_updated" => "Grants changed",
        "password_reset" => "Password reset",
        "impersonation_started" => "Signed in as a user",
        "impersonation_ended" => "Back to superadmin",
        "sessions_ended" => "All sessions ended",
        _ => "Event",
    }
}

/// Bars, one for each day, newest on the right. This code draws three paths, one path for each
/// kind of bar. It does not draw one element for each day: a graph costs the browser four nodes, not
/// sixty. It uses attributes only, and no inline style. The values are in the text next to it, not
/// in tooltips.
fn spark(days: &[DayStat], wide: bool) -> String {
    const STEP: usize = 4;
    const HEIGHT: i64 = 32;
    let max = i64::from(days.iter().map(|d| d.sessions).max().unwrap_or(0)).max(1);
    let total: u32 = days.iter().map(|d| d.sessions).sum();
    let width = days.len().max(1) * STEP - 1;
    let (mut bars, mut today, mut zeros) = (String::new(), String::new(), String::new());
    for (index, day) in days.iter().enumerate() {
        let height = if day.sessions == 0 {
            1
        } else {
            (i64::from(day.sessions) * (HEIGHT - 2) / max).max(2)
        };
        let target = if day.sessions == 0 {
            &mut zeros
        } else if index + 1 == days.len() {
            &mut today
        } else {
            &mut bars
        };
        let _ = write!(
            target,
            "M{} {}h3v{height}h-3z",
            index * STEP,
            HEIGHT - height
        );
    }
    let mut out = format!(
        r#"<svg class="spark{}" viewBox="0 0 {width} {HEIGHT}" preserveAspectRatio="none" role="img" aria-label="Connections per day over the last {} days: {total} in total">"#,
        if wide { " spark--wide" } else { "" },
        days.len()
    );
    for (class, path) in [
        ("spark__zero", zeros),
        ("spark__bar", bars),
        ("spark__bar spark__bar--today", today),
    ] {
        if !path.is_empty() {
            let _ = write!(out, r#"<path class="{class}" d="{path}"/>"#);
        }
    }
    out.push_str("</svg>");
    out
}

// Services ------------------------------------------------------------------------------------

pub struct ServicesView<'a> {
    pub catalog: &'a Catalog,
    /// The actions of each service.
    pub services: &'a [ServiceActions],
    /// Every account with the grants that apply to it.
    pub holders: &'a [(String, Vec<Grant>)],
    /// The services whose actions document the code cannot read: `(service id, problem)`.
    pub problems: Vec<(String, String)>,
}

pub fn services_page(frame: &Frame, view: &ServicesView) -> String {
    let mut out = String::from(
        r#"<section class="panel" aria-labelledby="services-title"><div class="panel__head"><h2 class="panel__title" id="services-title">Services</h2></div><p class="hint">Who can open each service, and the actions that each service publishes at <code>/.well-known/access-actions</code>. A user opens a service with the grant <code>access</code>. The superadmin opens all of them. A service without actions has only <code>access</code>: for it, <code>access</code> is the full permission. A grant on a sub-resource names a resource type that the action declares, for example <code>database:analytics</code>. Give it on the Users page. <code>portal.exclude</code> opens paths of a service to anyone, signed in or not: it is listed here so that it can be audited.</p><div class="tableScroll"><table class="table"><thead><tr><th>Service</th><th>Group</th><th>Path</th><th>Actions</th><th>Users with access</th><th>Open without sign-in</th></tr></thead><tbody>"#,
    );
    for group in view.catalog.groups() {
        for service in &group.services {
            let problem = view.problems.iter().find(|(id, _)| *id == service.id);
            let _ = write!(
                out,
                r#"<tr><td><span class="svcName">{}</span>{}<br><code>{}</code></td><td>{}</td><td><code>{}</code></td><td>"#,
                html_escape(&service.name),
                match problem {
                    Some((_, text)) => format!(
                        r##"<span class="warn" role="img" aria-label="{0}" title="{0}"><svg class="icon" aria-hidden="true"><use href="#i-alert-triangle"/></svg></span>"##,
                        html_escape(text)
                    ),
                    None => String::new(),
                },
                html_escape(&service.id),
                html_escape(&group.name),
                html_escape(&service.href),
            );
            let actions = view
                .services
                .iter()
                .find(|s| s.id == service.id)
                .map(|s| s.actions.as_slice())
                .unwrap_or(&[]);
            if actions.is_empty() {
                out.push_str(r#"<span class="pill pill--muted">access only</span>"#);
            } else {
                out.push_str(r#"<span class="pill">access</span> "#);
                for (name, description) in actions {
                    let _ = write!(
                        out,
                        r#"<span class="pill pill--plain" title="{}">{}</span> "#,
                        html_escape(description),
                        html_escape(name)
                    );
                }
            }
            out.push_str("</td><td>");
            let users: Vec<&String> = view
                .holders
                .iter()
                .filter(|(_, grants)| holds(grants, ACCESS, &service.id))
                .map(|(login, _)| login)
                .collect();
            if users.is_empty() {
                out.push_str(r#"<span class="pill pill--muted">Superadmin only</span>"#);
            } else {
                for login in users {
                    let _ = write!(out, r#"<span class="pill">{}</span> "#, html_escape(login));
                }
            }
            out.push_str("</td><td>");
            if service.exclude.is_empty() {
                out.push_str(r#"<span class="pill pill--muted">None</span>"#);
            } else {
                for rule in &service.exclude {
                    let _ = write!(out, r#"<code>{}</code> "#, html_escape(rule.text()));
                }
            }
            out.push_str("</td></tr>");
        }
    }
    out.push_str("</tbody></table></div></section>");
    undeclared_grants(&mut out, view);
    page(Tab::Services, frame, &out, false)
}

/// The grants on sub-resources whose resource type the service does not declare for the action.
/// At start, the server does not know the services: the catalogue and the actions documents come
/// at run time. Thus a grant of the configuration can name a type that the service does not
/// declare. This panel shows each such grant. The panel is absent when there is none.
fn undeclared_grants(out: &mut String, view: &ServicesView) {
    let mut rows = String::new();
    for (login, grants) in view.holders {
        for grant in grants {
            let undeclared = view
                .services
                .iter()
                .any(|s| s.id == grant.service && s.undeclared(grant));
            if undeclared {
                let _ = write!(
                    rows,
                    "<tr><td>{}</td><td><code>{}</code></td></tr>",
                    html_escape(login),
                    html_escape(&grant.to_string())
                );
            }
        }
    }
    if rows.is_empty() {
        return;
    }
    let _ = write!(
        out,
        r#"<section class="panel" aria-labelledby="checks-title"><div class="panel__head"><h2 class="panel__title" id="checks-title">Grants to check</h2></div><p class="hint">The service does not declare the resource type of these grants for the action. The PDP does not apply these grants. Correct the grant, or the document of the service.</p><div class="tableScroll"><table class="table"><thead><tr><th>User</th><th>Grant</th></tr></thead><tbody>{rows}</tbody></table></div></section>"#
    );
}

// Logins --------------------------------------------------------------------------------------

pub struct LoginsView<'a> {
    pub page: &'a LoginPage,
    /// Only the refused attempts.
    pub only_failed: bool,
    pub retention_days: u32,
}

fn logins_href(page: u32, only_failed: bool) -> String {
    let mut query = Vec::new();
    if page > 0 {
        query.push(format!("p={}", page + 1));
    }
    if only_failed {
        query.push("r=failed".to_string());
    }
    if query.is_empty() {
        LOGINS_PATH.to_string()
    } else {
        format!("{LOGINS_PATH}?{}", query.join("&amp;"))
    }
}

pub fn logins_page(frame: &Frame, view: &LoginsView) -> String {
    let data = view.page;
    let mut out = String::new();
    let _ = write!(
        out,
        r#"<div class="stats"><div class="stat"><span class="stat__label">Attempts, 24 h</span><span class="stat__value">{}</span></div><div class="stat"><span class="stat__label">Refused, 24 h</span><span class="stat__value{}">{}</span></div><div class="stat"><span class="stat__label">Addresses refused, 24 h</span><span class="stat__value">{}</span></div></div>"#,
        data.attempts_24h,
        if data.failed_24h > 0 {
            " stat__value--danger"
        } else {
            ""
        },
        data.failed_24h,
        data.addresses_24h,
    );
    let _ = write!(
        out,
        r#"<section class="panel" aria-labelledby="attempts-title"><div class="panel__head"><h2 class="panel__title" id="attempts-title">Sign-in attempts</h2><span class="hint">{}</span><div class="segmented" role="group" aria-label="Show">"#,
        if view.only_failed {
            format!("{} refused", data.matching)
        } else {
            plural(data.matching, "attempt", "attempts")
        },
    );
    for (label, only_failed) in [("All", false), ("Refused", true)] {
        let current = only_failed == view.only_failed;
        let _ = write!(
            out,
            r#"<a href="{}"{}>{label}</a>"#,
            logins_href(0, only_failed),
            if current {
                r#" class="is-active" aria-current="true""#
            } else {
                ""
            },
        );
    }
    let _ = write!(
        out,
        r#"</div></div><p class="hint">The login is recorded as typed, cut to {TYPED_LOGIN_MAX} characters, whether or not the account exists. The password is never recorded. Kept {} days, at most 20,000 attempts. Attempts blocked by the rate limiter are not listed one by one. Times are UTC.</p>"#,
        view.retention_days,
    );
    if data.rows.is_empty() {
        out.push_str(r#"<p class="hint">No attempt recorded.</p>"#);
    } else {
        out.push_str(
            r#"<div class="tableScroll"><table class="table"><thead><tr><th>When (UTC)</th><th>Result</th><th>Login typed</th><th>Address</th><th>Browser</th></tr></thead><tbody>"#,
        );
        for row in &data.rows {
            login_row(&mut out, row);
        }
        out.push_str("</tbody></table></div>");
    }
    let (current, pages) = (data.page, data.pages());
    if pages > 1 {
        out.push_str(r#"<nav class="inbox" aria-label="Pages">"#);
        if current > 0 {
            let _ = write!(
                out,
                r#"<a class="button" rel="prev" href="{}">Newer</a>"#,
                logins_href(current - 1, view.only_failed)
            );
        }
        let _ = write!(
            out,
            r#"<span class="hint">Page {} of {pages}</span>"#,
            current + 1
        );
        if current + 1 < pages {
            let _ = write!(
                out,
                r#"<a class="button" rel="next" href="{}">Older</a>"#,
                logins_href(current + 1, view.only_failed)
            );
        }
        out.push_str("</nav>");
    }
    out.push_str("</section>");
    page(Tab::Logins, frame, &out, false)
}

fn login_row(out: &mut String, row: &LoginRow) {
    let result = if row.success {
        r#"<span class="pill pill--ok">Signed in</span>"#
    } else {
        r#"<span class="pill pill--danger">Refused</span>"#
    };
    let typed = if row.login.is_empty() {
        r#"<span class="hint">(empty)</span>"#.to_string()
    } else {
        format!(r#"<code class="typed">{}</code>"#, html_escape(&row.login))
    };
    let _ = write!(
        out,
        r#"<tr><td>{}</td><td>{result}</td><td>{typed}{}</td><td>{}</td><td title="{}">{}</td></tr>"#,
        format_utc_secs(row.ts),
        if row.known {
            ""
        } else {
            r#" <span class="pill pill--muted">no such account</span>"#
        },
        opt(&row.ip),
        html_escape(&row.user_agent),
        html_escape(&short_agent(&row.user_agent)),
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::parse_labels;
    use crate::store::{AccountActivity, LoginRow, SessionRow};
    use std::collections::BTreeMap;

    fn service(name: &str, href: &str) -> Service {
        let labels: BTreeMap<String, String> = [
            ("portal.name", name),
            ("portal.href", href),
            ("portal.group", "Infra"),
        ]
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
        parse_labels("x", &labels).unwrap().unwrap()
    }

    fn grant(action: &str, service: &str) -> Grant {
        Grant::new(action, service).unwrap()
    }

    /// `access` on each named service (ids `infra-<name>`).
    fn access(services: &[&str]) -> Vec<Grant> {
        services
            .iter()
            .map(|s| grant(ACCESS, &format!("infra-{s}")))
            .collect()
    }

    fn card(login: &str, services: &[&str]) -> UserCard {
        UserCard {
            login: login.into(),
            grants: access(services),
            configured: Vec::new(),
            created_at: Some(1_791_000_000),
            superadmin: false,
            activity: AccountActivity {
                login: login.into(),
                days: vec![DayStat::default(); 30],
                ..AccountActivity::default()
            },
        }
    }

    fn frame() -> Frame<'static> {
        Frame {
            logout_token: "out<",
        }
    }

    fn services() -> Vec<ServiceActions> {
        let plain = |id: &str, name: &str| ServiceActions {
            id: id.into(),
            name: name.into(),
            actions: Vec::new(),
            sub_resources: Vec::new(),
            known: true,
        };
        vec![
            ServiceActions {
                id: "infra-grafana".into(),
                name: "Grafana".into(),
                actions: vec![
                    ("dashboards.read".into(), "See the dashboards.".into()),
                    ("dashboards.write".into(), "Change the <dashboards>.".into()),
                ],
                sub_resources: vec![
                    ("dashboards.read".into(), vec!["folder".into()]),
                    (
                        "dashboards.write".into(),
                        vec!["folder".into(), "dashboard".into()],
                    ),
                ],
                known: true,
            },
            plain("infra-notes", "Notes"),
            plain("infra-typo", "Typo"),
            plain("infra-mixed", "Mixed"),
        ]
    }

    fn view(catalog: &Catalog) -> UsersView<'_> {
        let mut root = card("root", &[]);
        root.superadmin = true;
        root.created_at = None;
        UsersView {
            notice: None,
            mode: Mode::Dynamic,
            catalog,
            services: services(),
            users: vec![
                card("alice", &["grafana", "mixed"]),
                card("bob", &["mixed"]),
            ],
            superadmin: root,
            days: vec![DayStat::default(); 30],
            first_day: 1_788_000_000,
            recent_events: Vec::new(),
            unknown_failures_30d: 0,
            max_users: 64,
            token: "tok<en",
        }
    }

    fn catalog() -> Catalog {
        Catalog::build(
            vec![
                service("Grafana", "/grafana/"),
                service("Notes", "/notes/"),
                service("Typo", "/typo/"),
                service("Mixed", "/mixed/"),
            ],
            Vec::new(),
        )
    }

    fn services_view<'a>(
        catalog: &'a Catalog,
        actions: &'a [ServiceActions],
        holders: &'a [(String, Vec<Grant>)],
    ) -> ServicesView<'a> {
        ServicesView {
            catalog,
            services: actions,
            holders,
            problems: Vec::new(),
        }
    }

    fn login_row_of(login: &str, success: bool, known: bool) -> LoginRow {
        LoginRow {
            ts: 1_791_623_502,
            login: login.into(),
            known,
            success,
            ip: Some("203.0.113.7".into()),
            user_agent: "Mozilla/5.0 (X11; Linux x86_64; rv:131.0) Gecko/20100101 Firefox/131.0"
                .into(),
        }
    }

    fn logins<'a>(page: &'a LoginPage, only_failed: bool) -> LoginsView<'a> {
        LoginsView {
            page,
            only_failed,
            retention_days: 90,
        }
    }

    #[test]
    fn every_page_is_a_whole_document_with_the_same_header_and_three_tab_links() {
        let cat = catalog();
        let actions = services();
        let holders = vec![("alice".to_string(), access(&["grafana"]))];
        let blank = LoginPage::default();
        let pages = [
            (Tab::Users, users_page(&frame(), &view(&cat))),
            (
                Tab::Services,
                services_page(&frame(), &services_view(&cat, &actions, &holders)),
            ),
            (Tab::Logins, logins_page(&frame(), &logins(&blank, false))),
        ];
        for (tab, html) in &pages {
            assert!(
                html.starts_with("<!doctype html>"),
                "{tab:?} is a page of its own"
            );
            assert!(html.contains(r#"<link rel="stylesheet" href="/auth/settings.css">"#));
            assert!(html.contains(r##"<a class="backLink" href="/">"##));
            assert!(html.contains(r#"action="/auth/logout""#) && html.contains("out&lt;"));
            assert_eq!(html.matches(r#"class="contentTabs__tab"#).count(), 3);
            assert!(
                !html.contains("/auth/settings/roles"),
                "no page of the old model"
            );
            assert_eq!(html.matches(r#"aria-current="page""#).count(), 1);
            assert!(html.contains(&format!(
                r#"<a class="contentTabs__tab is-active" href="{}" aria-current="page">{}</a>"#,
                tab.path(),
                tab.label()
            )));
            assert!(!html.contains(r#"class="modal"#), "no popup any more");
            assert!(
                !html.contains("tabInput"),
                "tabs are links, not radio buttons"
            );
        }
        assert!(pages[0].1.contains("/auth/assets/settings.js"));
        for (_, html) in &pages[1..] {
            assert!(
                !html.contains("settings.js"),
                "only the Users page needs the generator"
            );
        }
    }

    #[test]
    fn a_page_carries_only_its_own_tab() {
        let cat = catalog();
        let users = users_page(&frame(), &view(&cat));
        assert!(users.contains("Accounts") && users.contains("Recent events"));
        assert!(!users.contains("Users with access") && !users.contains("Sign-in attempts"));
        let actions = services();
        let services = services_page(&frame(), &services_view(&cat, &actions, &[]));
        assert!(services.contains("Superadmin only"));
        assert!(!services.contains("Accounts") && !services.contains(r#"class="user""#));
    }

    #[test]
    fn accounts_come_before_the_add_form_which_opens_only_after_a_refused_attempt() {
        let cat = catalog();
        let html = users_page(&frame(), &view(&cat));
        assert!(html.find(">Accounts<").unwrap() < html.find("Add a user</summary>").unwrap());
        assert!(html.contains(r#"<details class="addUser"><summary"#));
        for refused in [
            "exists",
            "weak-password",
            "invalid-login",
            "limit",
            "reserved",
        ] {
            let mut v = view(&cat);
            v.notice = notice_for(refused);
            assert!(
                users_page(&frame(), &v).contains(r#"<details class="addUser" open>"#),
                "{refused}"
            );
        }
        let mut v = view(&cat);
        v.notice = notice_for("edit-weak-password");
        assert!(!users_page(&frame(), &v).contains(r#"<details class="addUser" open>"#));
    }

    #[test]
    fn an_account_can_be_edited_with_its_current_grants_ticked_and_a_password_that_may_stay_empty()
    {
        let cat = catalog();
        let html = users_page(&frame(), &view(&cat));
        assert_eq!(html.matches(r#"action="/auth/users/update""#).count(), 2);
        let end = html.find(r#"id="pw-alice""#).unwrap();
        let start = html[..end].rfind(r#"action="/auth/users/update""#).unwrap();
        let alice = &html[start..end];
        assert!(alice.contains(r#"name="g.infra-grafana.access" value="1" checked"#));
        assert!(alice.contains(r#"name="g.infra-mixed.access" value="1" checked"#));
        assert!(
            alice.contains(r#"name="g.infra-notes.access" value="1">"#),
            "not held: not ticked"
        );
        assert!(alice.contains(r#"name="g.infra-grafana.dashboards.read" value="1">"#));
        assert!(
            alice.contains(r#"title="Change the &lt;dashboards&gt;.""#),
            "the description of an action is escaped"
        );
        assert!(html.contains(r#"id="pw-alice""#) && html.contains(r#"id="pw-bob""#));
        assert!(html.contains(r#"data-generate-password="pw-alice""#));
        assert!(html.contains("Leave empty to keep the current one."));
        let edit_password = &html[html.find(r#"id="pw-alice""#).unwrap()..];
        let edit_password = &edit_password[..edit_password.find("</div>").unwrap()];
        assert!(
            !edit_password.contains(" required"),
            "empty keeps the password"
        );
        assert!(html.contains(
            r#"id="new-password" name="password" type="password" minlength="10" required"#
        ));
        assert!(
            !html.contains(r#"pw-<"#),
            "the field id is escaped like everything from outside"
        );
    }

    #[test]
    fn failures_are_announced_and_the_failed_sign_ins_are_counted_in_the_accounts_header() {
        let cat = catalog();
        let mut v = view(&cat);
        v.notice = notice_for("exists");
        v.unknown_failures_30d = 2;
        v.users[0].activity.failed_30d = 1;
        let html = users_page(&frame(), &v);
        assert!(html.contains(r#"class="notice notice--error" role="alert""#));
        assert!(
            html.contains(r#"<span class="pill pill--danger">3 failed sign-ins in 30 days</span>"#)
        );
        assert!(html.contains(
            r#"2 tried a login that does not exist: see <a href="/auth/settings/logins">Logins</a>"#
        ));
        let mut one = view(&cat);
        one.users[0].activity.failed_30d = 1;
        assert!(
            users_page(&frame(), &one).contains("1 failed sign-in in 30 days"),
            "singular"
        );
        let mut ok = view(&cat);
        ok.notice = notice_for("user-added");
        assert!(users_page(&frame(), &ok).contains(r#"class="notice notice--ok" role="status""#));
    }

    #[test]
    fn tables_scroll_inside_a_box_and_an_empty_value_is_a_plain_hyphen() {
        let cat = catalog();
        let mut v = view(&cat);
        v.recent_events = vec![AuditRow {
            ts: 1_791_000_000,
            kind: "password_reset".into(),
            login: Some("alice".into()),
            actor: None,
            ip: None,
        }];
        let html = users_page(&frame(), &v);
        assert!(html.contains("Password reset"));
        assert_eq!(
            html.matches(r#"<div class="tableScroll"><table"#).count(),
            html.matches("</table>").count()
        );
        assert!(!html.contains("&ndash;"));
        assert!(!html.contains('\u{25B8}'));
    }

    #[test]
    fn a_graph_is_four_nodes_say_its_peak_and_only_the_newest_bar_wears_the_accent() {
        let mut days = vec![DayStat::default(); 30];
        days[3] = DayStat {
            sessions: 5,
            seconds: 60,
        };
        days[29] = DayStat {
            sessions: 2,
            seconds: 60,
        };
        let first = 1_788_000_000;
        assert_eq!(peak_text(&days, first), " Peak: 5 on 2026-09-01.");
        assert_eq!(peak_text(&vec![DayStat::default(); 30], first), "");
        let svg = spark(&days, true);
        assert_eq!(
            svg.matches("<path").count(),
            3,
            "empty days, the others, today"
        );
        assert_eq!(svg.matches("spark__bar--today").count(), 1);
        assert!(
            svg.contains("M12 2h3v30h-3z"),
            "the busiest day fills the graph: {svg}"
        );
        assert!(svg.contains("M116 20h3v12h-3z"), "today is 2 of 5: {svg}");
        assert!(!svg.contains("style=") && !svg.contains("<rect") && !svg.contains("<title>"));
        assert!(svg.contains(r#"class="spark spark--wide""#));
        assert!(svg.contains("7 in total"));
        let quiet = spark(&vec![DayStat::default(); 30], false);
        assert_eq!(
            quiet.matches("<path").count(),
            1,
            "a path only when there is something to draw"
        );
    }

    #[test]
    fn the_services_page_shows_the_actions_the_users_with_access_and_the_problems() {
        let cat = catalog();
        let actions = services();
        let holders = vec![
            ("alice".to_string(), access(&["grafana", "mixed"])),
            (
                "bob".to_string(),
                vec![grant("dashboards.read", "infra-grafana")],
            ),
        ];
        let mut v = services_view(&cat, &actions, &holders);
        v.problems = vec![(
            "infra-typo".into(),
            "http://typo/x gave the status <500>".into(),
        )];
        let html = services_page(&frame(), &v);
        assert!(html.contains(
            r#"<span class="pill pill--plain" title="See the dashboards.">dashboards.read</span>"#
        ));
        assert_eq!(
            html.matches("access only").count(),
            3,
            "Notes, Typo and Mixed publish nothing"
        );
        assert_eq!(
            html.matches(r#"<span class="pill">alice</span>"#).count(),
            2
        );
        assert!(
            !html.contains(">bob<"),
            "an action without access does not open the service"
        );
        assert_eq!(html.matches("Superadmin only").count(), 2, "Notes and Typo");
        assert_eq!(html.matches(r#"class="warn""#).count(), 1);
        assert!(html.contains("gave the status &lt;500&gt;"));
    }

    #[test]
    fn users_show_grants_actions_and_a_two_step_delete() {
        let cat = catalog();
        let html = users_page(&frame(), &view(&cat));
        assert_eq!(
            html.matches(r#"action="/auth/users/impersonate""#).count(),
            2
        );
        assert_eq!(html.matches(r#"action="/auth/users/delete""#).count(), 2);
        assert!(html.contains("Confirm delete"));
        assert!(
            html.contains(r#"<span class="pill">2 services</span>"#),
            "alice"
        );
        assert!(
            html.contains(r#"<span class="pill">1 service</span>"#),
            "bob"
        );
        assert!(html.contains("superadmin</span>"));
        assert!(html.contains("value=\"tok&lt;en\""), "the token is escaped");
    }

    #[test]
    fn the_superadmin_card_has_no_delete_or_login_as() {
        let cat = catalog();
        let mut v = view(&cat);
        v.users.clear();
        let html = users_page(&frame(), &v);
        assert!(!html.contains("/auth/users/delete\""));
        assert!(!html.contains("/auth/users/impersonate\""));
    }

    #[test]
    fn what_comes_from_outside_is_escaped() {
        let cat = catalog();
        let mut v = view(&cat);
        v.users[0].login = "<b>x</b>".into();
        v.users[0].activity.recent.push(SessionRow {
            started_at: 1_791_000_000,
            last_seen_at: 1_791_000_100,
            ended_at: None,
            end_reason: None,
            ip: Some("<script>1</script>".into()),
            user_agent: "\"><img src=x onerror=1>".into(),
            impersonated: true,
        });
        let html = users_page(&frame(), &v);
        assert!(!html.contains("<b>x</b>"));
        assert!(!html.contains("<script>"));
        assert!(!html.contains("<img src=x"));
        assert!(html.contains("via superadmin"));
        assert!(html.contains("Active"));
    }

    #[test]
    fn a_new_user_is_added_without_grants() {
        let cat = catalog();
        let html = users_page(&frame(), &view(&cat));
        let add = &html[html.find(r#"action="/auth/users/add""#).unwrap()..];
        let add = &add[..add.find("</form>").unwrap()];
        assert!(
            !add.contains(r#"name="g."#),
            "the add form has no grant box"
        );
        assert!(add.contains("A new user has no grant"));
    }

    #[test]
    fn in_the_static_mode_no_box_can_change_and_the_form_sends_none() {
        let cat = catalog();
        let mut v = view(&cat);
        v.mode = Mode::Static;
        let html = users_page(&frame(), &v);
        assert!(!html.contains(r#"name="g."#));
        assert!(html.contains(r#"<input type="checkbox" checked disabled><span>access</span>"#));
        assert!(html.contains("This page does not change them."));
    }

    #[test]
    fn in_the_hybrid_mode_a_grant_of_the_configuration_is_read_only() {
        let cat = catalog();
        let mut v = view(&cat);
        v.mode = Mode::Hybrid;
        v.users[1].configured = access(&["notes"]);
        v.users[1].grants.extend(access(&["notes"]));
        let html = users_page(&frame(), &v);
        let end = html.find(r#"id="pw-bob""#).unwrap();
        let bob = &html[html[..end].rfind(r#"action="/auth/users/update""#).unwrap()..end];
        assert!(bob.contains(r#"title="From the configuration: it cannot change here."><input type="checkbox" checked disabled><span>access</span>"#));
        assert!(
            !bob.contains("g.infra-notes.access"),
            "the form does not send it"
        );
        assert!(bob.contains(r#"name="g.infra-mixed.access" value="1" checked"#));
    }

    #[test]
    fn allowed_services_follow_the_access_grants() {
        let cat = catalog();
        let mut v = view(&cat);
        v.users[1]
            .grants
            .push(grant("dashboards.read", "infra-grafana"));
        let names = |u: &UserCard| -> Vec<String> {
            allowed_services(&v, u)
                .iter()
                .map(|s| s.name.clone())
                .collect()
        };
        assert_eq!(names(&v.users[0]), ["Grafana", "Mixed"]);
        assert_eq!(
            names(&v.users[1]),
            ["Mixed"],
            "an action alone does not open a service"
        );
        assert_eq!(
            names(&v.superadmin).len(),
            4,
            "the superadmin opens every service"
        );
    }

    #[test]
    fn the_add_form_offers_a_generator() {
        let cat = catalog();
        let html = users_page(&frame(), &view(&cat));
        assert!(html.contains(r#"data-generate-password="new-password""#));
        assert!(html.contains(r#"id="new-password-status""#));
        assert!(html.contains(r#"<label for="new-password">Password</label>"#));
    }

    #[test]
    fn notices_come_from_a_fixed_list() {
        assert!(notice_for("user-added").unwrap().ok);
        assert!(!notice_for("exists").unwrap().ok);
        assert!(notice_for("<script>").is_none());
    }

    #[test]
    fn the_logins_page_lists_every_attempt_with_what_was_typed_and_from_where() {
        let page = LoginPage {
            rows: vec![
                login_row_of("alice", true, true),
                login_row_of("admin", false, false),
                login_row_of("alice", false, true),
                login_row_of("", false, false),
            ],
            matching: 4,
            attempts_24h: 4,
            failed_24h: 3,
            addresses_24h: 1,
            ..LoginPage::default()
        };
        let html = logins_page(&frame(), &logins(&page, false));
        assert!(
            html.contains("2026-10-10 09:11:42"),
            "seconds tell attempts apart"
        );
        assert!(html.contains(r#"<code class="typed">admin</code> <span class="pill pill--muted">no such account</span>"#));
        assert!(html.contains(r#"<span class="pill pill--ok">Signed in</span>"#));
        assert_eq!(
            html.matches(r#"<span class="pill pill--danger">Refused</span>"#)
                .count(),
            3
        );
        assert!(html.contains("203.0.113.7") && html.contains("Firefox 131"));
        assert!(html.contains(r#"<span class="hint">(empty)</span>"#));
        assert!(html.contains(r#"<span class="stat__value stat__value--danger">3</span>"#));
        assert!(html.contains("The password is never recorded"));
        assert!(html.contains("4 attempts"));
        assert!(
            !html.contains(r#"class="inbox""#),
            "one page needs no inbox"
        );
    }

    #[test]
    fn what_a_visitor_typed_cannot_break_out_of_the_logins_page() {
        let page = LoginPage {
            rows: vec![
                login_row_of(r#"</code><script>alert(1)</script>"#, false, false),
                login_row_of(r#""><img src=x onerror=1>"#, false, false),
                login_row_of("a\\u{a}b", false, false),
            ],
            matching: 3,
            ..LoginPage::default()
        };
        let html = logins_page(&frame(), &logins(&page, false));
        assert!(!html.contains("<script>"));
        assert!(!html.contains("<img src=x"));
        assert!(html.contains("&lt;/code&gt;&lt;script&gt;alert(1)&lt;/script&gt;"));
        assert!(html.contains("&quot;&gt;&lt;img src=x onerror=1&gt;"));
    }

    #[test]
    fn the_logins_page_filters_and_pages_with_links_that_keep_the_filter() {
        let page = LoginPage {
            rows: vec![login_row_of("alice", false, true)],
            matching: 120,
            page: 1,
            ..LoginPage::default()
        };
        let html = logins_page(&frame(), &logins(&page, true));
        assert!(html.contains("Page 2 of 3"));
        assert!(
            html.contains(r#"rel="prev" href="/auth/settings/logins?r=failed""#),
            "page 1 has no p"
        );
        assert!(html.contains(r#"rel="next" href="/auth/settings/logins?p=3&amp;r=failed""#));
        assert!(html.contains(r#"<a href="/auth/settings/logins?r=failed" class="is-active" aria-current="true">Refused</a>"#));
        assert!(html.contains(r#"<a href="/auth/settings/logins">All</a>"#));
        assert!(html.contains("120 refused"));
        let last = LoginPage { page: 2, ..page };
        let html = logins_page(&frame(), &logins(&last, false));
        assert!(!html.contains(r#"rel="next""#));
        assert!(html.contains(r#"rel="prev" href="/auth/settings/logins?p=2""#));
    }

    fn sub(text: &str) -> Grant {
        Grant::parse(text).unwrap()
    }

    /// The part of the Users page for one user: from the name of the user to the next card.
    fn card_of<'a>(html: &'a str, login: &str) -> &'a str {
        let start = html
            .find(&format!(r#"<span class="user__name">{login}</span>"#))
            .unwrap();
        let rest = &html[start..];
        let end = rest[1..]
            .find(r#"<span class="user__name">"#)
            .map_or(rest.len(), |i| i + 1);
        &rest[..end]
    }

    #[test]
    fn a_user_card_lists_the_grants_on_sub_resources_and_offers_the_declared_types() {
        let cat = catalog();
        let mut v = view(&cat);
        v.users[0]
            .grants
            .push(sub("dashboards.write on service:infra-grafana/folder:ops"));
        v.users[0]
            .grants
            .push(sub("dashboards.read on service:infra-grafana/team:<x>"));
        v.users[0]
            .grants
            .push(sub("x.read on service:gone/thing:a"));
        let html = users_page(&frame(), &v);
        let alice = card_of(&html, "alice");
        assert!(alice.contains("Grants on sub-resources"));
        assert!(alice.contains(
            r#"<li><code>dashboards.write</code> on <code>folder:ops</code><form class="subGrants__remove" method="post" action="/auth/users/grant-delete">"#
        ));
        assert!(alice.contains(
            r#"name="grant" value="dashboards.write on service:infra-grafana/folder:ops""#
        ));
        assert!(
            alice.contains("<code>team:&lt;x&gt;</code>"),
            "the id is escaped"
        );
        assert_eq!(
            alice.matches(r#"class="warn""#).count(),
            1,
            "Grafana does not declare team; the service that left is not known"
        );
        assert!(alice.contains(r#"<p class="subGrants__name">gone</p>"#));
        assert_eq!(
            alice.matches(r#"action="/auth/users/grant-add""#).count(),
            1,
            "only Grafana declares types"
        );
        assert!(alice.contains(r#"<input type="hidden" name="service" value="infra-grafana">"#));
        assert!(alice.contains(
            r#"<select name="action" required><option>dashboards.read</option><option>dashboards.write</option></select>"#
        ));
        assert!(alice.contains(
            r#"<select name="type" required><option>dashboard</option><option>folder</option></select>"#
        ));
        assert!(alice.contains(r#"name="id" type="text" required maxlength="128""#));
        assert!(
            !alice.contains("<option>access</option>"),
            "access has no sub-resource"
        );
        let bob = card_of(&html, "bob");
        assert!(
            !bob.contains("<li><code>"),
            "bob has no grant on a sub-resource"
        );
        assert!(bob.contains(r#"action="/auth/users/grant-add""#));
        assert!(
            !card_of(&html, "root").contains("Grants on sub-resources"),
            "the superadmin has all grants"
        );
    }

    #[test]
    fn a_grant_on_a_sub_resource_from_the_configuration_cannot_be_removed() {
        let cat = catalog();
        let configured = sub("dashboards.read on service:infra-grafana/folder:ops");
        let mut v = view(&cat);
        v.mode = Mode::Hybrid;
        v.users[0].grants.push(configured.clone());
        v.users[0].configured.push(configured);
        let html = users_page(&frame(), &v);
        let alice = card_of(&html, "alice");
        assert!(alice.contains(r#"<span class="pill pill--muted" title="From the configuration: it cannot change here.">configuration</span>"#));
        assert!(!alice.contains("grant-delete"));
        let mut fixed = view(&cat);
        fixed.mode = Mode::Static;
        fixed.users[0]
            .grants
            .push(sub("dashboards.read on service:infra-grafana/folder:ops"));
        let html = users_page(&frame(), &fixed);
        assert!(!html.contains("grant-add") && !html.contains("grant-delete"));
        assert!(card_of(&html, "alice").contains("<code>folder:ops</code>"));
        assert!(card_of(&html, "bob").contains("No grant on a sub-resource.</p>"));
    }

    #[test]
    fn the_services_page_lists_the_grants_whose_type_the_service_does_not_declare() {
        let cat = catalog();
        let actions = services();
        let holders = vec![
            (
                "alice".to_string(),
                vec![
                    sub("dashboards.read on service:infra-grafana/folder:ops"),
                    sub("dashboards.read on service:infra-grafana/dashboard:1"),
                ],
            ),
            (
                "bob".to_string(),
                vec![sub("x.read on service:infra-notes/a:b")],
            ),
        ];
        let html = services_page(&frame(), &services_view(&cat, &actions, &holders));
        assert!(html.contains("Grants to check"));
        assert!(html.contains(
            "<tr><td>alice</td><td><code>dashboards.read on service:infra-grafana/dashboard:1</code></td></tr>"
        ));
        assert!(html.contains("<td>bob</td>"), "notes declares nothing");
        assert!(!html.contains("folder:ops</code></td>"));
        let mut unknown = services();
        unknown[1].known = false;
        let html = services_page(&frame(), &services_view(&cat, &unknown, &holders[1..]));
        assert!(
            !html.contains("Grants to check"),
            "a service that has not answered is not judged"
        );
    }

    #[test]
    fn the_new_notices_are_in_the_list() {
        for code in [
            "grant-added",
            "grant-deleted",
            "grant-bad-id",
            "grant-undeclared",
            "grant-not-known",
            "grant-limit",
            "no-grant",
        ] {
            assert!(notice_for(code).is_some(), "{code}");
        }
    }
}
