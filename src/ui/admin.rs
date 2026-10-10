//! The settings popup of the superadmin: users with their history, roles, services.
//!
//! Works without script: the popup opens on the `#settings` anchor (`:target`), the tabs are radio
//! buttons, every action is a form. Script only adds focus handling and the password generator. The graphs are inline SVG built here, with attributes only (the CSP
//! forbids `style`). Everything that comes from outside (logins, addresses, user agents, service
//! names) is escaped.

use std::fmt::Write as _;

use crate::access::service_allows;
use crate::catalog::{Catalog, Service};
use crate::store::{AccountActivity, AuditRow, DayStat, ServiceUse, SessionRow};
use crate::util::{format_duration, format_utc, html_escape, short_agent};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tab {
    Users,
    Roles,
    Services,
}

impl Tab {
    pub fn parse(value: &str) -> Self {
        match value {
            "roles" => Self::Roles,
            "services" => Self::Services,
            _ => Self::Users,
        }
    }

    fn id(self) -> &'static str {
        match self {
            Self::Users => "users",
            Self::Roles => "roles",
            Self::Services => "services",
        }
    }
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

/// One account and what is known about it.
pub struct UserCard {
    pub login: String,
    pub roles: Vec<String>,
    pub created_at: Option<i64>,
    pub superadmin: bool,
    pub activity: AccountActivity,
}

pub struct AdminView<'a> {
    pub tab: Tab,
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

pub fn render(view: &AdminView) -> String {
    let mut out = String::new();
    out.push_str(
        r##"<section class="modal" id="settings"><a class="modal__backdrop" href="#" tabindex="-1" aria-hidden="true"></a><div class="modal__panel" role="dialog" aria-modal="true" aria-labelledby="settings-title"><div class="modal__head"><h2 id="settings-title">Settings</h2><a class="modal__close" href="#" aria-label="Close settings" title="Close"><svg class="icon" aria-hidden="true"><use href="#i-x"/></svg></a></div>"##,
    );
    for tab in [Tab::Users, Tab::Roles, Tab::Services] {
        let _ = write!(
            out,
            r#"<input class="tabInput" type="radio" name="settings-tab" id="tab-{id}"{checked}>"#,
            id = tab.id(),
            checked = if tab == view.tab { " checked" } else { "" },
        );
    }
    out.push_str(
        r#"<div class="tabs"><label for="tab-users">Users</label><label for="tab-roles">Roles</label><label for="tab-services">Services</label></div><div class="panels">"#,
    );
    out.push_str(r#"<section class="panel panel--users">"#);
    users_panel(&mut out, view);
    out.push_str(r#"</section><section class="panel panel--roles">"#);
    roles_panel(&mut out, view);
    out.push_str(r#"</section><section class="panel panel--services">"#);
    services_panel(&mut out, view);
    out.push_str("</section></div></div></section>");
    out
}

// Users ---------------------------------------------------------------------------------------

fn users_panel(out: &mut String, view: &AdminView) {
    if let Some(notice) = view.notice {
        // A failure interrupts: it is announced at once, and takes focus when the popup opens.
        let _ = write!(
            out,
            r#"<div class="notice {}" role="{}" tabindex="-1"><p>{}</p></div>"#,
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
        r#"<div class="accountsHead"><h3 class="sectionTitle">Accounts <span class="hint">{} of {} users</span></h3>"#,
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
            r#"<p class="hint">{} tried a login that does not exist (what was typed is never stored).</p>"#,
            view.unknown_failures_30d
        );
    }

    add_form(out, view);

    out.push_str(r#"<div class="userList">"#);
    user_card(out, view, &view.superadmin);
    for user in &view.users {
        user_card(out, view, user);
    }
    out.push_str("</div>");

    let total: u32 = view.days.iter().map(|d| d.sessions).sum();
    let _ = write!(
        out,
        r#"<h3 class="sectionTitle">Connections</h3><div class="overview"><div class="overview__text"><p class="hint">{total} in the last {} days, all accounts.{} Times are UTC.</p></div>{}</div>"#,
        view.days.len(),
        peak_text(&view.days, view.first_day),
        spark(&view.days, view.first_day, true),
    );

    out.push_str(r#"<h3 class="sectionTitle">Recent events</h3>"#);
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
}

fn plural(n: u32, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
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
fn role_boxes(out: &mut String, view: &AdminView, held: &[String]) {
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

fn add_form(out: &mut String, view: &AdminView) {
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

fn user_card(out: &mut String, view: &AdminView, user: &UserCard) {
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
        spark(&activity.days, view.first_day, false),
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

    out.push_str(r#"<h4>Latest sessions</h4>"#);
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

    out.push_str(r#"<h4>Services used</h4>"#);
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

    out.push_str(r#"<h4>Allowed services</h4>"#);
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

fn allowed_services<'a>(view: &'a AdminView, user: &UserCard) -> Vec<&'a Service> {
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

fn opt(value: &Option<String>) -> String {
    match value {
        Some(text) if !text.is_empty() => html_escape(text),
        _ => "-".to_string(),
    }
}

/// Bars, one per day, newest on the right. Attributes only: no inline style.
fn spark(days: &[DayStat], first_day: i64, wide: bool) -> String {
    const STEP: usize = 4;
    const HEIGHT: i64 = 32;
    let max = i64::from(days.iter().map(|d| d.sessions).max().unwrap_or(0)).max(1);
    let total: u32 = days.iter().map(|d| d.sessions).sum();
    let width = days.len() * STEP - 1;
    let mut out = format!(
        r#"<svg class="spark{}" viewBox="0 0 {width} {HEIGHT}" preserveAspectRatio="none" role="img" aria-label="Connections per day over the last {} days: {total} in total">"#,
        if wide { " spark--wide" } else { "" },
        days.len()
    );
    for (index, day) in days.iter().enumerate() {
        let height = if day.sessions == 0 {
            1
        } else {
            (i64::from(day.sessions) * (HEIGHT - 2) / max).max(2)
        };
        let date = format_utc(first_day + index as i64 * 86_400);
        let _ = write!(
            out,
            r#"<rect class="{}" x="{}" y="{}" width="3" height="{height}"><title>{}: {} {}, {} connected</title></rect>"#,
            if day.sessions == 0 {
                "spark__zero"
            } else if index + 1 == days.len() {
                "spark__bar spark__bar--today"
            } else {
                "spark__bar"
            },
            index * STEP,
            HEIGHT - height,
            &date[..10],
            day.sessions,
            if day.sessions == 1 {
                "connection"
            } else {
                "connections"
            },
            format_duration(day.seconds),
        );
    }
    out.push_str("</svg>");
    out
}

// Roles ---------------------------------------------------------------------------------------

fn roles_panel(out: &mut String, view: &AdminView) {
    if view.registry.is_empty() {
        out.push_str(
            r#"<p class="hint">No role is defined. Roles come from the portal configuration: <code>roles { names = "ops, dev" }</code>. Services bind to them with the <code>portal.roles</code> label or annotation.</p>"#,
        );
        return;
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
        let holders: Vec<&UserCard> = view
            .users
            .iter()
            .filter(|u| u.roles.contains(role))
            .collect();
        if holders.is_empty() {
            out.push_str(r#"<p class="hint">No user holds it.</p>"#);
        } else {
            out.push_str(r#"<p class="chips">"#);
            for user in holders {
                let _ = write!(
                    out,
                    r#"<span class="pill pill--plain">{}</span>"#,
                    html_escape(&user.login)
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
    out.push_str("</div>");
}

// Services ------------------------------------------------------------------------------------

fn services_panel(out: &mut String, view: &AdminView) {
    out.push_str(
        r#"<p class="hint">Who may open each service. A service is opened to the roles it names with <code>portal.roles</code>; a service with no role is open to the superadmin only, who may open all of them.</p><div class="tableScroll"><table class="table"><thead><tr><th>Service</th><th>Group</th><th>Path</th><th>Roles allowed</th></tr></thead><tbody>"#,
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
            out.push_str("</td></tr>");
        }
    }
    out.push_str("</tbody></table></div>");
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::parse_labels;
    use crate::store::{AccountActivity, SessionRow};
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

    fn view<'a>(catalog: &'a Catalog, registry: &'a [String]) -> AdminView<'a> {
        let mut root = card("root", &[]);
        root.superadmin = true;
        root.created_at = None;
        AdminView {
            tab: Tab::Users,
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
                service("Windmill", "/windmill/", ""),
                service("Typo", "/typo/", "opz"),
                service("Mixed", "/mixed/", "dev, opz"),
            ],
            Vec::new(),
        )
    }

    #[test]
    fn accounts_come_before_the_add_form_which_opens_only_after_a_refused_attempt() {
        let (cat, reg) = (catalog(), registry());
        let html = render(&view(&cat, &reg));
        assert!(html.find("Accounts <span").unwrap() < html.find("Add a user</summary>").unwrap());
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
                render(&v).contains(r#"<details class="addUser" open>"#),
                "{refused}"
            );
        }
        let mut v = view(&cat, &reg);
        v.notice = notice_for("edit-weak-password");
        assert!(!render(&v).contains(r#"<details class="addUser" open>"#));
    }

    #[test]
    fn an_account_can_be_edited_with_its_current_roles_ticked_and_a_password_that_may_stay_empty() {
        let (cat, reg) = (catalog(), registry());
        let html = render(&view(&cat, &reg));
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
        let html = render(&v);
        assert!(html.contains(r#"role="alert" tabindex="-1""#));
        assert!(
            html.contains(r#"<span class="pill pill--danger">3 failed sign-ins in 30 days</span>"#)
        );
        assert!(html.contains("2 tried a login that does not exist"));
        let mut one = view(&cat, &reg);
        one.users[0].activity.failed_30d = 1;
        assert!(
            render(&one).contains("1 failed sign-in in 30 days"),
            "singular"
        );
        let mut ok = view(&cat, &reg);
        ok.notice = notice_for("user-added");
        assert!(render(&ok).contains(r#"role="status" tabindex="-1""#));
    }

    #[test]
    fn the_dialog_carries_its_own_name_and_the_tables_scroll_inside_a_box() {
        let (cat, reg) = (catalog(), registry());
        let mut v = view(&cat, &reg);
        v.recent_events = vec![AuditRow {
            ts: 1_791_000_000,
            kind: "password_reset".into(),
            login: Some("alice".into()),
            actor: None,
            ip: None,
        }];
        let html = render(&v);
        assert!(
            html.contains(r#"role="dialog" aria-modal="true" aria-labelledby="settings-title""#)
        );
        assert!(!html.contains(r#"<section class="modal" id="settings" aria-labelledby"#));
        assert_eq!(
            html.matches("Close settings").count(),
            1,
            "the backdrop is not a second link"
        );
        assert!(html.contains("Password reset"));
        assert_eq!(
            html.matches(r#"<div class="tableScroll"><table"#).count(),
            html.matches("</table>").count()
        );
        assert!(
            !html.contains("&ndash;"),
            "an empty value is a plain hyphen"
        );
        assert!(!html.contains('\u{25B8}'));
    }

    #[test]
    fn graphs_say_their_peak_and_only_the_newest_bar_wears_the_accent() {
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
        let svg = spark(&days, first, true);
        assert_eq!(svg.matches("spark__bar--today").count(), 1);
        assert!(svg.contains(r#"class="spark__bar" x="12""#));
    }

    #[test]
    fn the_popup_has_three_tabs_and_the_active_one_is_checked() {
        let (cat, reg) = (catalog(), registry());
        let mut v = view(&cat, &reg);
        v.tab = Tab::Roles;
        let html = render(&v);
        for id in ["users", "roles", "services"] {
            assert!(html.contains(&format!(r#"id="tab-{id}""#)));
        }
        assert!(html.contains(r#"id="tab-roles" checked"#));
        assert!(!html.contains(r#"id="tab-users" checked"#));
    }

    #[test]
    fn a_service_bound_to_a_missing_role_gets_a_warning_without_naming_the_role() {
        let (cat, reg) = (catalog(), registry());
        let html = render(&view(&cat, &reg));
        let services = &html[html.find("panel--services").unwrap()..];
        assert_eq!(
            services.matches(r#"class="warn""#).count(),
            2,
            "Typo and Mixed"
        );
        assert!(!html.contains("opz"), "the missing role is never shown");
        assert_eq!(
            services.matches("Superadmin only").count(),
            2,
            "no role and a missing role both leave the service to the superadmin"
        );
        assert!(!services.contains("All users"));
    }

    #[test]
    fn users_show_roles_actions_and_a_two_step_delete() {
        let (cat, reg) = (catalog(), registry());
        let html = render(&view(&cat, &reg));
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
        let html = render(&v);
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
        let html = render(&v);
        assert!(!html.contains("<b>x</b>"));
        assert!(!html.contains("<script>"));
        assert!(!html.contains("<img src=x"));
        assert!(html.contains("via superadmin"));
        assert!(html.contains("Active"));
    }

    #[test]
    fn graphs_use_attributes_only_and_scale_to_the_busiest_day() {
        let mut days = vec![DayStat::default(); 30];
        days[29] = DayStat {
            sessions: 4,
            seconds: 3_600,
        };
        days[28] = DayStat {
            sessions: 1,
            seconds: 60,
        };
        let svg = spark(&days, 1_788_000_000, true);
        assert!(!svg.contains("style="));
        assert!(svg.contains(r#"class="spark spark--wide""#));
        assert!(svg.contains("5 in total"));
        assert_eq!(svg.matches("<rect").count(), 30);
        assert!(
            svg.contains(r#"height="30""#),
            "the busiest day fills the graph"
        );
        assert!(svg.contains("1 connection,"));
    }

    #[test]
    fn the_roles_tab_lists_holders_and_the_services_each_role_opens() {
        let (cat, reg) = (catalog(), registry());
        let html = render(&view(&cat, &reg));
        let roles =
            &html[html.find("panel--roles").unwrap()..html.find("panel--services").unwrap()];
        assert!(roles.contains("<h3>ops</h3>") && roles.contains("<h3>dev</h3>"));
        assert!(roles.contains("alice") && roles.contains("bob"));
        assert!(roles.contains("Grafana"));
    }

    #[test]
    fn with_no_role_defined_the_form_and_the_tab_say_so() {
        let (cat, reg) = (catalog(), Vec::<String>::new());
        let html = render(&view(&cat, &reg));
        assert!(html.contains("No role is defined"));
        assert!(!html.contains(r#"class="roleBoxes""#));
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
    fn the_add_form_offers_a_generator_and_the_close_cross_is_not_a_button() {
        let (cat, reg) = (catalog(), registry());
        let html = render(&view(&cat, &reg));
        assert!(html.contains(r#"data-generate-password="new-password""#));
        assert!(html.contains(r#"id="new-password-status""#));
        assert!(html.contains(r#"<label for="new-password">Password</label>"#));
        assert!(html.contains(r##"<a class="modal__close" href="#""##));
        assert!(!html.contains("button--icon"));
    }

    #[test]
    fn notices_come_from_a_fixed_list() {
        assert!(notice_for("user-added").unwrap().ok);
        assert!(!notice_for("exists").unwrap().ok);
        assert!(notice_for("<script>").is_none());
    }
}
