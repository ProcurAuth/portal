//! The settings of the superadmin: one HTML page per tab (Users, Roles, Services, Logins), in the
//! manner of clickhouse-dash: a header, a row of underlined tab links, then the page.
//!
//! A page carries only what its tab shows, so the browser never builds the DOM of the others, and the
//! launcher carries none of it. Everything works without script; the Users page loads one small
//! script for its password generator. The graphs are inline SVG built here from attributes only (the
//! CSP forbids `style`). Everything that comes from outside (logins, typed or stored, addresses, user
//! agents, service names) is escaped.

use std::fmt::Write as _;

use super::{document, sign_out_form, theme_switch};
use crate::access::service_allows;
use crate::catalog::{Catalog, Service};
use crate::store::{
    AccountActivity, AuditRow, DayStat, LoginPage, LoginRow, ServiceUse, SessionRow,
};
use crate::util::{
    format_duration, format_utc, format_utc_secs, html_escape, short_agent, TYPED_LOGIN_MAX,
};

pub const USERS_PATH: &str = "/auth/settings";
pub const ROLES_PATH: &str = "/auth/settings/roles";
pub const SERVICES_PATH: &str = "/auth/settings/services";
pub const LOGINS_PATH: &str = "/auth/settings/logins";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tab {
    Users,
    Roles,
    Services,
    Logins,
}

impl Tab {
    const ALL: [Tab; 4] = [Tab::Users, Tab::Roles, Tab::Services, Tab::Logins];

    fn path(self) -> &'static str {
        match self {
            Self::Users => USERS_PATH,
            Self::Roles => ROLES_PATH,
            Self::Services => SERVICES_PATH,
            Self::Logins => LOGINS_PATH,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Users => "Users",
            Self::Roles => "Roles",
            Self::Services => "Services",
            Self::Logins => "Logins",
        }
    }
}

/// What every settings page needs around its content.
pub struct Frame<'a> {
    pub logout_token: &'a str,
}

/// The outcome of the last action, from a fixed list of codes (the URL never carries text).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Notice {
    pub text: &'static str,
    pub ok: bool,
    /// The outcome of the add-user form: it stays open so the next try is one click away.
    pub add_form: bool,
}

pub fn notice_for(code: &str) -> Option<Notice> {
    let (text, ok, add_form) = match code {
        "user-added" => ("User added.", true, false),
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
        "unknown-role" => (
            "That role is not defined in the configuration.",
            false,
            true,
        ),
        "limit" => ("The user limit is reached.", false, true),
        "edit-weak-password" => (
            "The password must have at least 10 characters.",
            false,
            false,
        ),
        "edit-unknown-role" => (
            "That role is not defined in the configuration.",
            false,
            false,
        ),
        "no-user" => ("That user does not exist.", false, false),
        "error" => ("The action failed. See the portal logs.", false, false),
        _ => return None,
    };
    Some(Notice { text, ok, add_form })
}

/// The page around a tab: header, tab links, then `content`. `script` loads the generator script.
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

/// One account and what is known about it.
pub struct UserCard {
    pub login: String,
    pub roles: Vec<String>,
    pub created_at: Option<i64>,
    pub superadmin: bool,
    pub activity: AccountActivity,
}

pub struct UsersView<'a> {
    pub notice: Option<Notice>,
    /// The roles that exist (the configuration).
    pub registry: &'a [String],
    /// The whole catalogue, not the one filtered for display.
    pub catalog: &'a Catalog,
    pub users: Vec<UserCard>,
    pub superadmin: UserCard,
    /// Every real connection, per day.
    pub days: Vec<DayStat>,
    pub first_day: i64,
    pub recent_events: Vec<AuditRow>,
    pub unknown_failures_30d: u32,
    pub max_users: usize,
    /// Anti-CSRF token of the administration forms.
    pub token: &'a str,
}

pub fn users_page(frame: &Frame, view: &UsersView) -> String {
    let mut out = String::new();
    if let Some(notice) = view.notice {
        // A failure interrupts: it is announced at once.
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

    // What the administrator opens this for comes first: who the accounts are and what went wrong.
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
    page(Tab::Users, frame, &out, true)
}

/// "Peak: 7 on 2026-10-04." for a graph, so its values do not live in tooltips only.
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

/// The role boxes of a form; `held` are the roles ticked from the start.
fn role_boxes(out: &mut String, view: &UsersView, held: &[String]) {
    out.push_str(r#"<fieldset class="roleBoxes"><legend>Roles</legend>"#);
    for role in view.registry {
        let _ = write!(
            out,
            r#"<label class="check"><input type="checkbox" name="r_{0}" value="1"{1}><span>{0}</span></label>"#,
            html_escape(role),
            if held.contains(role) { " checked" } else { "" },
        );
    }
    out.push_str("</fieldset>");
}

/// A password field with its "Generate" button (script) and the line that says what was generated.
fn password_field(out: &mut String, id: &str, label: &str, required: bool, hint: &str) {
    // The id may carry a login: escaped like everything that comes from outside.
    let id = html_escape(id);
    let _ = write!(
        out,
        r#"<div class="field"><label for="{id}">{label}</label><div class="passwordRow"><input id="{id}" name="password" type="password" minlength="10"{} autocomplete="new-password" autocapitalize="off" spellcheck="false" aria-describedby="{id}-hint {id}-status"><button class="button generate" type="button" data-generate-password="{id}">Generate</button></div><p class="hint" id="{id}-hint">{hint}</p><p class="hint" id="{id}-status" aria-live="polite"></p></div>"#,
        if required { " required" } else { "" },
    );
}

fn add_form(out: &mut String, view: &UsersView) {
    // Adding is the rare task: one button, and the form opens after a refused attempt.
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
    if view.registry.is_empty() {
        out.push_str(
            r#"<p class="hint">No role is defined in the configuration (<code>roles { names = "..." }</code>): a new user cannot open any service until one is bound to a role.</p>"#,
        );
    } else {
        role_boxes(out, view, &[]);
    }
    out.push_str(
        r#"<button class="button button--primary" type="submit">Add user</button></form></details>"#,
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
    }
    for role in &user.roles {
        let _ = write!(
            out,
            r#"<span class="pill{}">{}</span>"#,
            if view.registry.contains(role) {
                ""
            } else {
                " pill--muted"
            },
            html_escape(role)
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
        if view.registry.is_empty() {
            out.push_str(r#"<p class="hint">No role is defined in the configuration.</p>"#);
        } else {
            role_boxes(out, view, &user.roles);
        }
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

fn allowed_services<'a>(view: &'a UsersView, user: &UserCard) -> Vec<&'a Service> {
    view.catalog
        .groups()
        .iter()
        .flat_map(|group| group.services.iter())
        .filter(|service| {
            let roles = (!user.superadmin).then_some(user.roles.as_slice());
            service_allows(&service.roles, view.registry, roles)
        })
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
        "user_updated" => "Roles changed",
        "password_reset" => "Password reset",
        "impersonation_started" => "Signed in as a user",
        "impersonation_ended" => "Back to superadmin",
        _ => "Event",
    }
}

/// Bars, one per day, newest on the right, drawn as three paths (a path per kind of bar, not an
/// element per day: a graph costs the browser four nodes, not sixty). Attributes only: no inline
/// style. The values are in the text next to it, not in tooltips.
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
        let _ = write!(target, "M{} {}h3v{height}h-3z", index * STEP, HEIGHT - height);
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

// Roles ---------------------------------------------------------------------------------------

pub struct RolesView<'a> {
    /// The roles that exist (the configuration).
    pub registry: &'a [String],
    pub catalog: &'a Catalog,
    /// Every account with the roles it holds.
    pub holders: &'a [(String, Vec<String>)],
}

pub fn roles_page(frame: &Frame, view: &RolesView) -> String {
    let mut out = String::from(
        r#"<section class="panel" aria-labelledby="roles-title"><div class="panel__head"><h2 class="panel__title" id="roles-title">Roles</h2></div>"#,
    );
    if view.registry.is_empty() {
        out.push_str(
            r#"<p class="hint">No role is defined. Roles come from the portal configuration: <code>roles { names = "ops, dev" }</code>. Services bind to them with the <code>portal.roles</code> label or annotation.</p></section>"#,
        );
        return page(Tab::Roles, frame, &out, false);
    }
    out.push_str(
        r#"<p class="hint">Defined in the portal configuration. A service lists the roles it accepts with <code>portal.roles</code>; a user holds any number of them.</p><div class="roleGrid">"#,
    );
    for role in view.registry {
        let _ = write!(
            out,
            r#"<article class="roleCard"><h3>{}</h3><h4>Users</h4>"#,
            html_escape(role)
        );
        let holders: Vec<&String> = view
            .holders
            .iter()
            .filter(|(_, roles)| roles.contains(role))
            .map(|(login, _)| login)
            .collect();
        if holders.is_empty() {
            out.push_str(r#"<p class="hint">No user holds it.</p>"#);
        } else {
            out.push_str(r#"<p class="chips">"#);
            for login in holders {
                let _ = write!(
                    out,
                    r#"<span class="pill pill--plain">{}</span>"#,
                    html_escape(login)
                );
            }
            out.push_str("</p>");
        }
        out.push_str(r#"<h4>Services it opens</h4>"#);
        let opened: Vec<&Service> = view
            .catalog
            .groups()
            .iter()
            .flat_map(|g| g.services.iter())
            .filter(|s| s.roles.contains(role))
            .collect();
        if opened.is_empty() {
            out.push_str(r#"<p class="hint">No service asks for it.</p>"#);
        } else {
            out.push_str(r#"<p class="chips">"#);
            for service in opened {
                let _ = write!(
                    out,
                    r#"<span class="pill pill--plain">{}</span>"#,
                    html_escape(&service.name)
                );
            }
            out.push_str("</p>");
        }
        out.push_str("</article>");
    }
    out.push_str("</div></section>");
    page(Tab::Roles, frame, &out, false)
}

// Services ------------------------------------------------------------------------------------

pub struct ServicesView<'a> {
    pub registry: &'a [String],
    pub catalog: &'a Catalog,
}

pub fn services_page(frame: &Frame, view: &ServicesView) -> String {
    let mut out = String::from(
        r#"<section class="panel" aria-labelledby="services-title"><div class="panel__head"><h2 class="panel__title" id="services-title">Services</h2></div><p class="hint">Who may open each service. A service is opened to the roles it names with <code>portal.roles</code>; a service with no role is open to the superadmin only, who may open all of them. <code>portal.exclude</code> opens paths of a service to anyone, signed in or not: it is listed here so that it can be audited.</p><div class="tableScroll"><table class="table"><thead><tr><th>Service</th><th>Group</th><th>Path</th><th>Roles allowed</th><th>Open without sign-in</th></tr></thead><tbody>"#,
    );
    for group in view.catalog.groups() {
        for service in &group.services {
            let known: Vec<&String> = service
                .roles
                .iter()
                .filter(|r| view.registry.contains(r))
                .collect();
            let has_unknown = known.len() < service.roles.len();
            let _ = write!(
                out,
                r#"<tr><td><span class="svcName">{}</span>{}</td><td>{}</td><td><code>{}</code></td><td>"#,
                html_escape(&service.name),
                if has_unknown {
                    r##"<span class="warn" role="img" aria-label="A role bound to this service does not exist" title="A role bound to this service does not exist"><svg class="icon" aria-hidden="true"><use href="#i-alert-triangle"/></svg></span>"##
                } else {
                    ""
                },
                html_escape(&group.name),
                html_escape(&service.href),
            );
            if known.is_empty() {
                out.push_str(r#"<span class="pill pill--muted">Superadmin only</span>"#);
            } else {
                for role in known {
                    let _ = write!(out, r#"<span class="pill">{}</span>"#, html_escape(role));
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
    page(Tab::Services, frame, &out, false)
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
        let _ = write!(out, r#"<span class="hint">Page {} of {pages}</span>"#, current + 1);
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

    fn service(name: &str, href: &str, roles: &str) -> Service {
        let labels: BTreeMap<String, String> = [
            ("portal.name", name),
            ("portal.href", href),
            ("portal.roles", roles),
            ("portal.group", "Infra"),
        ]
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
        parse_labels("x", &labels).unwrap().unwrap()
    }

    fn registry() -> Vec<String> {
        vec!["ops".into(), "dev".into()]
    }

    fn card(login: &str, roles: &[&str]) -> UserCard {
        UserCard {
            login: login.into(),
            roles: roles.iter().map(|r| r.to_string()).collect(),
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
        Frame { logout_token: "out<" }
    }

    fn view<'a>(catalog: &'a Catalog, registry: &'a [String]) -> UsersView<'a> {
        let mut root = card("root", &[]);
        root.superadmin = true;
        root.created_at = None;
        UsersView {
            notice: None,
            registry,
            catalog,
            users: vec![card("alice", &["ops", "dev"]), card("bob", &["dev"])],
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
                service("Grafana", "/grafana/", "ops"),
                service("Notes", "/notes/", ""),
                service("Typo", "/typo/", "opz"),
                service("Mixed", "/mixed/", "dev, opz"),
            ],
            Vec::new(),
        )
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
    fn every_page_is_a_whole_document_with_the_same_header_and_four_tab_links() {
        let (cat, reg) = (catalog(), registry());
        let holders = vec![("alice".to_string(), vec!["ops".to_string()])];
        let blank = LoginPage::default();
        let pages = [
            (Tab::Users, users_page(&frame(), &view(&cat, &reg))),
            (
                Tab::Roles,
                roles_page(
                    &frame(),
                    &RolesView {
                        registry: &reg,
                        catalog: &cat,
                        holders: &holders,
                    },
                ),
            ),
            (
                Tab::Services,
                services_page(
                    &frame(),
                    &ServicesView {
                        registry: &reg,
                        catalog: &cat,
                    },
                ),
            ),
            (Tab::Logins, logins_page(&frame(), &logins(&blank, false))),
        ];
        for (tab, html) in &pages {
            assert!(html.starts_with("<!doctype html>"), "{tab:?} is a page of its own");
            assert!(html.contains(r#"<link rel="stylesheet" href="/auth/settings.css">"#));
            assert!(html.contains(r##"<a class="backLink" href="/">"##));
            assert!(html.contains(r#"action="/auth/logout""#) && html.contains("out&lt;"));
            assert_eq!(html.matches(r#"class="contentTabs__tab"#).count(), 4);
            assert_eq!(html.matches(r#"aria-current="page""#).count(), 1);
            assert!(html.contains(&format!(
                r#"<a class="contentTabs__tab is-active" href="{}" aria-current="page">{}</a>"#,
                tab.path(),
                tab.label()
            )));
            assert!(!html.contains(r#"class="modal"#), "no popup any more");
            assert!(!html.contains("tabInput"), "tabs are links, not radio buttons");
        }
        assert!(pages[0].1.contains("/auth/assets/settings.js"));
        for (_, html) in &pages[1..] {
            assert!(!html.contains("settings.js"), "only the Users page needs the generator");
        }
    }

    #[test]
    fn a_page_carries_only_its_own_tab() {
        let (cat, reg) = (catalog(), registry());
        let users = users_page(&frame(), &view(&cat, &reg));
        assert!(users.contains("Accounts") && users.contains("Recent events"));
        assert!(!users.contains("Roles it") && !users.contains("Sign-in attempts"));
        let services = services_page(
            &frame(),
            &ServicesView {
                registry: &reg,
                catalog: &cat,
            },
        );
        assert!(services.contains("Superadmin only"));
        assert!(!services.contains("Accounts") && !services.contains(r#"class="user""#));
    }

    #[test]
    fn accounts_come_before_the_add_form_which_opens_only_after_a_refused_attempt() {
        let (cat, reg) = (catalog(), registry());
        let html = users_page(&frame(), &view(&cat, &reg));
        assert!(html.find(">Accounts<").unwrap() < html.find("Add a user</summary>").unwrap());
        assert!(html.contains(r#"<details class="addUser"><summary"#));
        for refused in [
            "exists",
            "weak-password",
            "invalid-login",
            "unknown-role",
            "limit",
            "reserved",
        ] {
            let mut v = view(&cat, &reg);
            v.notice = notice_for(refused);
            assert!(
                users_page(&frame(), &v).contains(r#"<details class="addUser" open>"#),
                "{refused}"
            );
        }
        let mut v = view(&cat, &reg);
        v.notice = notice_for("edit-weak-password");
        assert!(!users_page(&frame(), &v).contains(r#"<details class="addUser" open>"#));
    }

    #[test]
    fn an_account_can_be_edited_with_its_current_roles_ticked_and_a_password_that_may_stay_empty() {
        let (cat, reg) = (catalog(), registry());
        let html = users_page(&frame(), &view(&cat, &reg));
        assert_eq!(html.matches(r#"action="/auth/users/update""#).count(), 2);
        let alice = &html[html.find("pw-alice").unwrap() - 600..html.find("pw-alice").unwrap()];
        assert!(alice.contains(r#"name="r_ops" value="1" checked"#));
        assert!(alice.contains(r#"name="r_dev" value="1" checked"#));
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
        let (cat, reg) = (catalog(), registry());
        let mut v = view(&cat, &reg);
        v.notice = notice_for("exists");
        v.unknown_failures_30d = 2;
        v.users[0].activity.failed_30d = 1;
        let html = users_page(&frame(), &v);
        assert!(html.contains(r#"class="notice notice--error" role="alert""#));
        assert!(
            html.contains(r#"<span class="pill pill--danger">3 failed sign-ins in 30 days</span>"#)
        );
        assert!(html.contains(r#"2 tried a login that does not exist: see <a href="/auth/settings/logins">Logins</a>"#));
        let mut one = view(&cat, &reg);
        one.users[0].activity.failed_30d = 1;
        assert!(
            users_page(&frame(), &one).contains("1 failed sign-in in 30 days"),
            "singular"
        );
        let mut ok = view(&cat, &reg);
        ok.notice = notice_for("user-added");
        assert!(users_page(&frame(), &ok).contains(r#"class="notice notice--ok" role="status""#));
    }

    #[test]
    fn tables_scroll_inside_a_box_and_an_empty_value_is_a_plain_hyphen() {
        let (cat, reg) = (catalog(), registry());
        let mut v = view(&cat, &reg);
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
        assert_eq!(svg.matches("<path").count(), 3, "empty days, the others, today");
        assert_eq!(svg.matches("spark__bar--today").count(), 1);
        assert!(svg.contains("M12 2h3v30h-3z"), "the busiest day fills the graph: {svg}");
        assert!(svg.contains("M116 20h3v12h-3z"), "today is 2 of 5: {svg}");
        assert!(!svg.contains("style=") && !svg.contains("<rect") && !svg.contains("<title>"));
        assert!(svg.contains(r#"class="spark spark--wide""#));
        assert!(svg.contains("7 in total"));
        let quiet = spark(&vec![DayStat::default(); 30], false);
        assert_eq!(quiet.matches("<path").count(), 1, "a path only when there is something to draw");
    }

    #[test]
    fn a_service_bound_to_a_missing_role_gets_a_warning_without_naming_the_role() {
        let (cat, reg) = (catalog(), registry());
        let html = services_page(
            &frame(),
            &ServicesView {
                registry: &reg,
                catalog: &cat,
            },
        );
        assert_eq!(html.matches(r#"class="warn""#).count(), 2, "Typo and Mixed");
        assert!(!html.contains("opz"), "the missing role is never shown");
        assert_eq!(
            html.matches("Superadmin only").count(),
            2,
            "no role and a missing role both leave the service to the superadmin"
        );
        assert!(!html.contains("All users"));
    }

    #[test]
    fn users_show_roles_actions_and_a_two_step_delete() {
        let (cat, reg) = (catalog(), registry());
        let html = users_page(&frame(), &view(&cat, &reg));
        assert_eq!(
            html.matches(r#"action="/auth/users/impersonate""#).count(),
            2
        );
        assert_eq!(html.matches(r#"action="/auth/users/delete""#).count(), 2);
        assert!(html.contains("Confirm delete"));
        assert!(html.contains(r#"name="r_ops""#) && html.contains(r#"name="r_dev""#));
        assert!(html.contains("superadmin</span>"));
        assert!(html.contains("value=\"tok&lt;en\""), "the token is escaped");
    }

    #[test]
    fn the_superadmin_card_has_no_delete_or_login_as() {
        let (cat, reg) = (catalog(), registry());
        let mut v = view(&cat, &reg);
        v.users.clear();
        let html = users_page(&frame(), &v);
        assert!(!html.contains("/auth/users/delete\""));
        assert!(!html.contains("/auth/users/impersonate\""));
    }

    #[test]
    fn what_comes_from_outside_is_escaped() {
        let (cat, reg) = (catalog(), registry());
        let mut v = view(&cat, &reg);
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
    fn the_roles_page_lists_holders_and_the_services_each_role_opens() {
        let (cat, reg) = (catalog(), registry());
        let holders = vec![
            ("alice".to_string(), vec!["ops".to_string(), "dev".to_string()]),
            ("bob".to_string(), vec!["dev".to_string()]),
        ];
        let html = roles_page(
            &frame(),
            &RolesView {
                registry: &reg,
                catalog: &cat,
                holders: &holders,
            },
        );
        assert!(html.contains("<h3>ops</h3>") && html.contains("<h3>dev</h3>"));
        assert!(html.contains("alice") && html.contains("bob"));
        assert!(html.contains("Grafana"));
    }

    #[test]
    fn with_no_role_defined_the_form_and_the_roles_page_say_so() {
        let (cat, reg) = (catalog(), Vec::<String>::new());
        let html = users_page(&frame(), &view(&cat, &reg));
        assert!(html.contains("No role is defined"));
        assert!(!html.contains(r#"class="roleBoxes""#));
        let roles = roles_page(
            &frame(),
            &RolesView {
                registry: &reg,
                catalog: &cat,
                holders: &[],
            },
        );
        assert!(roles.contains("No role is defined"));
        assert!(!roles.contains("roleGrid"));
    }

    #[test]
    fn allowed_services_follow_the_roles_and_the_registry() {
        let (cat, reg) = (catalog(), registry());
        let v = view(&cat, &reg);
        let names = |u: &UserCard| -> Vec<String> {
            allowed_services(&v, u)
                .iter()
                .map(|s| s.name.clone())
                .collect()
        };
        assert_eq!(names(&v.users[0]), ["Grafana", "Mixed"]);
        assert_eq!(names(&v.users[1]), ["Mixed"]);
        assert_eq!(
            names(&v.superadmin).len(),
            4,
            "the superadmin opens every service, with or without a role"
        );
    }

    #[test]
    fn the_add_form_offers_a_generator() {
        let (cat, reg) = (catalog(), registry());
        let html = users_page(&frame(), &view(&cat, &reg));
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
        assert!(html.contains("2026-10-10 09:11:42"), "seconds tell attempts apart");
        assert!(html.contains(r#"<code class="typed">admin</code> <span class="pill pill--muted">no such account</span>"#));
        assert!(html.contains(r#"<span class="pill pill--ok">Signed in</span>"#));
        assert_eq!(html.matches(r#"<span class="pill pill--danger">Refused</span>"#).count(), 3);
        assert!(html.contains("203.0.113.7") && html.contains("Firefox 131"));
        assert!(html.contains(r#"<span class="hint">(empty)</span>"#));
        assert!(html.contains(r#"<span class="stat__value stat__value--danger">3</span>"#));
        assert!(html.contains("The password is never recorded"));
        assert!(html.contains("4 attempts"));
        assert!(!html.contains(r#"class="inbox""#), "one page needs no inbox");
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
        assert!(html.contains(r#"rel="prev" href="/auth/settings/logins?r=failed""#), "page 1 has no p");
        assert!(html.contains(r#"rel="next" href="/auth/settings/logins?p=3&amp;r=failed""#));
        assert!(html.contains(r#"<a href="/auth/settings/logins?r=failed" class="is-active" aria-current="true">Refused</a>"#));
        assert!(html.contains(r#"<a href="/auth/settings/logins">All</a>"#));
        assert!(html.contains("120 refused"));
        let last = LoginPage {
            page: 2,
            ..page
        };
        let html = logins_page(&frame(), &logins(&last, false));
        assert!(!html.contains(r#"rel="next""#));
        assert!(html.contains(r#"rel="prev" href="/auth/settings/logins?p=2""#));
    }
}
