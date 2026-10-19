//! The settings of the superadmin. Each tab is one HTML page: Users, Services and Activity. Each
//! account also has a page of its own, below the Users tab. The pages follow the manner of
//! clickhouse-dash: a header, a row of underlined tab links, then parts with a title and a count.
//!
//! The pages explain the access model with their structure:
//!
//! - The Users page is a table of the accounts. Each row gives a summary of the access of the
//!   account and links to the page of the account.
//! - The page of an account shows its access by service. Each service has its actions on the whole
//!   service, then its grants on the resource tree, written as the path that the grant names.
//! - The Services page shows how a grant reads, the grants to check, and the actions and the tree
//!   of resource types of each service.
//! - The Activity page shows the connections, the recent changes and the sign-in attempts.
//!
//! A page carries only what it shows, so the browser never builds the DOM of the other pages. The
//! launcher carries none of it. Everything works without script. The page of an account loads one
//! small script for its password generator. The graphs are inline SVG. This code builds them from
//! attributes only, because the CSP forbids `style`. Everything that comes from outside is escaped.
//! This includes logins (typed or stored), addresses, user agents, service names and resource ids.

use std::fmt::Write as _;

use super::{document, sign_out_form, theme_switch};
use crate::actions::{Document, Level};
use crate::catalog::{Catalog, Service};
use crate::grants::{holds, Grant, Mode, ACCESS};
use crate::store::{
    AccountActivity, AuditRow, DayStat, LoginPage, LoginRow, ServiceUse, SessionRow,
};
use crate::util::{
    format_duration, format_utc, format_utc_secs, html_escape, short_agent, TYPED_LOGIN_MAX,
};

pub const USERS_PATH: &str = "/auth/settings";
/// The page of one account is this prefix, then the login.
pub const USER_PATH_PREFIX: &str = "/auth/settings/users/";
pub const SERVICES_PATH: &str = "/auth/settings/services";
pub const LOGINS_PATH: &str = "/auth/settings/logins";

/// The path of the page of one account. A login has only `a-z`, `0-9`, `.`, `-` and `_`, so the
/// path needs no encoding. The caller escapes the path for HTML all the same.
pub fn user_path(login: &str) -> String {
    format!("{USER_PATH_PREFIX}{login}")
}

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
            Self::Logins => "Activity",
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
            "Account added. It has no grant yet: tick what it can open below, then save.",
            true,
            false,
        ),
        "user-updated" => (
            "Access saved. It applies from the next request.",
            true,
            false,
        ),
        "password-reset" => (
            "Password changed. The sessions of this account are closed.",
            true,
            false,
        ),
        "unchanged" => ("Nothing to change.", true, false),
        "user-deleted" => ("Account deleted. Its sessions are closed.", true, false),
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
        "limit" => (
            "The account limit is reached (users.max_users).",
            false,
            true,
        ),
        "edit-weak-password" => (
            "The password must have at least 10 characters.",
            false,
            false,
        ),
        "edit-unknown-grant" => (
            "A box of the form is not offered any more. Open the page again.",
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
            "Invalid resource id: 1 to 256 bytes, without a control character.",
            false,
            false,
        ),
        "grant-undeclared" => (
            "The service does not declare this resource for this action. Open the page again.",
            false,
            false,
        ),
        "grant-not-known" => (
            "The service has not given its actions yet. Try again in a moment.",
            false,
            false,
        ),
        "grant-limit" => (
            "This account has the largest number of grants.",
            false,
            false,
        ),
        "no-grant" => ("That grant does not exist.", false, false),
        "no-user" => ("That account does not exist.", false, false),
        "error" => ("The action failed. See the portal logs.", false, false),
        _ => return None,
    };
    Some(Notice { text, ok, add_form })
}

/// The icons that the settings pages can draw. Each page inlines only the symbols that it lists.
const ICONS: [&str; 7] = [
    "i-device-desktop",
    "i-moon",
    "i-sun",
    "i-logout",
    "i-chevron-left",
    "i-chevron-right",
    "i-alert-triangle",
];

/// The page around a tab. It has a header, the tab links, then `content`. `title` names the page
/// for the browser and for the screen reader. `script` loads the password generator.
fn page(tab: Tab, frame: &Frame, title: &str, content: &str, script: bool) -> String {
    let mut body = String::new();
    let _ = write!(
        body,
        r##"<a class="skipLink" href="#main">Skip to content</a><header class="appHeader"><div class="appHeader__left"><a class="backLink" href="/"><svg class="icon" aria-hidden="true"><use href="#i-chevron-left"/></svg>Back to panel</a></div><div class="appHeader__right">{}{}</div></header><nav class="navRow" aria-label="Settings"><div class="contentTabs">"##,
        theme_switch(),
        sign_out_form(frame.logout_token),
    );
    for each in Tab::ALL {
        let current = if each == tab {
            r#" is-active" aria-current="page"#
        } else {
            ""
        };
        let _ = write!(
            body,
            r#"<a class="contentTabs__tab{current}" href="{}">{}</a>"#,
            each.path(),
            each.label()
        );
    }
    let _ = write!(
        body,
        r#"</div></nav><main class="settings" id="main" tabindex="-1"><h1 class="visually-hidden">{}</h1>{content}</main>"#,
        html_escape(title)
    );
    if script {
        body.push_str(r#"<script src="/auth/assets/settings.js" defer></script>"#);
    }
    let icons: Vec<&str> = ICONS
        .iter()
        .copied()
        .filter(|id| body.contains(&format!("#{id}\"")))
        .collect();
    document(
        &format!("Settings: {title}"),
        r#"<link rel="stylesheet" href="/auth/settings.css">"#,
        &icons,
        &body,
    )
}

fn plural(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

fn opt(value: &Option<String>) -> String {
    match value {
        Some(text) if !text.is_empty() => html_escape(text),
        _ => r#"<span class="muted">none</span>"#.to_string(),
    }
}

fn notice_html(out: &mut String, notice: Option<Notice>) {
    if let Some(notice) = notice {
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
}

/// The head of a part: the title, a count in muted text, then optional controls at the right.
fn part_head(out: &mut String, id: &str, title: &str, count: &str, controls: &str) {
    let _ = write!(
        out,
        r#"<div class="part__head"><h2 class="part__title" id="{id}">{title}</h2>"#
    );
    if !count.is_empty() {
        let _ = write!(out, r#"<span class="part__count">{count}</span>"#);
    }
    if !controls.is_empty() {
        let _ = write!(out, r#"<div class="part__controls">{controls}</div>"#);
    }
    out.push_str("</div>");
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
    /// The last known document of the service: its tree of types and its actions. It is empty
    /// while the service has not answered.
    pub document: std::sync::Arc<Document>,
    /// The service answered: its actions and its resource types are known.
    pub known: bool,
}

impl ServiceActions {
    /// The problem of a grant below the service that does not follow its document: the action
    /// does not list the type, or the path does not follow the parents. `None`: the grant is on
    /// the whole service, the grant follows the document, or the service has not answered yet.
    fn problem(&self, grant: &Grant) -> Option<String> {
        if grant.path.is_empty() || !self.known {
            return None;
        }
        self.document.check_grant(grant).err()
    }

    fn undeclared(&self, grant: &Grant) -> bool {
        self.problem(grant).is_some()
    }

    /// The levels that a new grant below the service can name.
    fn levels(&self) -> Vec<Level> {
        self.document.levels()
    }
}

/// The data of the Users page: the table of the accounts and the add form.
pub struct UsersView<'a> {
    pub notice: Option<Notice>,
    /// The add form is open: the address asks for it, or the last add was refused.
    pub add_open: bool,
    /// Where the grants come from.
    pub mode: Mode,
    /// The whole catalogue, not the filtered catalogue for display.
    pub catalog: &'a Catalog,
    /// The services of the catalogue with their actions. The order is the order of the catalogue.
    pub services: Vec<ServiceActions>,
    pub users: Vec<UserCard>,
    pub superadmin: UserCard,
    pub unknown_failures_30d: u32,
    pub max_users: usize,
    /// The anti-CSRF token of the administration forms.
    pub token: &'a str,
}

/// The access of one account in short: the services that it opens, its grants below a service,
/// and its grants to check.
struct Summary {
    services: usize,
    below: usize,
    to_check: usize,
}

fn summary(catalog: &Catalog, services: &[ServiceActions], user: &UserCard) -> Summary {
    let opened = catalog
        .groups()
        .iter()
        .flat_map(|group| group.services.iter())
        .filter(|service| holds(&user.grants, ACCESS, &service.id))
        .count();
    let below = user.grants.iter().filter(|g| !g.path.is_empty()).count();
    let to_check = user
        .grants
        .iter()
        .filter(|grant| {
            services
                .iter()
                .any(|s| s.id == grant.service && s.undeclared(grant))
        })
        .count();
    Summary {
        services: opened,
        below,
        to_check,
    }
}

pub fn users_page(frame: &Frame, view: &UsersView) -> String {
    let mut out = String::new();
    notice_html(&mut out, view.notice);
    let add_open = view.add_open || view.notice.is_some_and(|n| n.add_form);
    out.push_str(r#"<section class="part" aria-labelledby="accounts-title">"#);
    let controls = if add_open {
        String::new()
    } else {
        format!(
            r##"<a class="button button--primary" href="{USERS_PATH}?add=1#add-user">Add a user</a>"##
        )
    };
    part_head(
        &mut out,
        "accounts-title",
        "Accounts",
        &format!("{} of {}", view.users.len(), view.max_users),
        &controls,
    );
    out.push_str(r#"<p class="part__lede">Each account has its own grants. A new account has no grant and sees no service. Open an account to see and change what it can use.</p>"#);
    if add_open {
        add_form(&mut out, view);
    }
    let failed: u32 = view.unknown_failures_30d
        + view.superadmin.activity.failed_30d
        + view
            .users
            .iter()
            .map(|u| u.activity.failed_30d)
            .sum::<u32>();
    if failed > 0 {
        let unknown = if view.unknown_failures_30d > 0 {
            format!(
                ", {} with a login that does not exist",
                view.unknown_failures_30d
            )
        } else {
            String::new()
        };
        let _ = write!(
            out,
            r#"<p class="callout callout--danger"><strong>{} in the last 30 days</strong>{unknown}. <a href="{LOGINS_PATH}?r=failed">See the refused attempts</a></p>"#,
            plural(failed as usize, "refused sign-in", "refused sign-ins"),
        );
    }
    out.push_str(r#"<div class="tableBox"><table class="table accounts"><thead><tr><th scope="col">Account</th><th scope="col">Access</th><th scope="col">Last sign-in (UTC)</th><th scope="col" class="num">Sessions, 30 days</th><th scope="col" class="num">Refused, 30 days</th></tr></thead><tbody>"#);
    account_row(&mut out, view, &view.superadmin);
    for user in &view.users {
        account_row(&mut out, view, user);
    }
    out.push_str("</tbody></table></div></section>");
    incident_part(&mut out, view.token);
    page(Tab::Users, frame, "Users", &out, false)
}

fn account_row(out: &mut String, view: &UsersView, user: &UserCard) {
    let login = html_escape(&user.login);
    let _ = write!(
        out,
        r#"<tr><th scope="row" data-label="Account"><a class="accountLink" href="{}">{login}</a>"#,
        html_escape(&user_path(&user.login))
    );
    if user.superadmin {
        out.push_str(r#" <span class="badge">Superadmin</span>"#);
    }
    out.push_str(r#"</th><td data-label="Access">"#);
    if user.superadmin {
        out.push_str(r#"<span class="muted">Every service and every action</span>"#);
    } else {
        let s = summary(view.catalog, &view.services, user);
        if s.services == 0 && s.below == 0 && user.grants.is_empty() {
            out.push_str(r#"<span class="muted">No access</span>"#);
        } else {
            out.push_str(r#"<span class="badges">"#);
            let _ = write!(
                out,
                r#"<span class="badge badge--accent">Opens {}</span>"#,
                plural(s.services, "service", "services")
            );
            let actions = user
                .grants
                .iter()
                .filter(|g| g.action != ACCESS && g.path.is_empty())
                .count();
            if actions > 0 {
                let _ = write!(
                    out,
                    r#"<span class="badge">{}</span>"#,
                    plural(actions, "action", "actions")
                );
            }
            if s.below > 0 {
                let _ = write!(
                    out,
                    r#"<span class="badge">{} on resources</span>"#,
                    plural(s.below, "grant", "grants")
                );
            }
            if s.to_check > 0 {
                let _ = write!(
                    out,
                    r#"<span class="badge badge--warn">{} to check</span>"#,
                    s.to_check
                );
            }
            out.push_str("</span>");
        }
    }
    let last = match user.activity.last_login {
        Some(at) => format_utc(at),
        None => r#"<span class="muted">Never</span>"#.to_string(),
    };
    let failed = user.activity.failed_30d;
    let _ = write!(
        out,
        r#"</td><td data-label="Last sign-in (UTC)">{last}</td><td class="num" data-label="Sessions, 30 days">{}</td><td class="num{}" data-label="Refused, 30 days">{failed}</td></tr>"#,
        user.activity.sessions_30d,
        if failed > 0 { " num--danger" } else { "" },
    );
}

/// The incident tool. One click opens the confirmation. A second click ends every session, the
/// session of the superadmin included.
fn incident_part(out: &mut String, token: &str) {
    let _ = write!(
        out,
        r#"<section class="part part--danger" aria-labelledby="incident-title"><div class="dangerRow"><div><h2 class="part__title" id="incident-title">End all sessions</h2><p class="hint">For an incident. Every account must sign in again, yours included. Accounts, grants and history stay.</p></div><details class="confirm"><summary class="button button--danger">End all sessions</summary><form class="confirm__panel" method="post" action="/auth/users/end-sessions"><input type="hidden" name="csrf" value="{}"><input type="hidden" name="confirm" value="end-all-sessions"><p>You will be signed out too.</p><button class="button button--dangerFill" type="submit">Confirm: end all sessions</button></form></details></div></section>"#,
        html_escape(token)
    );
}

/// A password field with its "Generate" button (script). The same block has the line that says what
/// was generated.
fn password_field(out: &mut String, id: &str, label: &str, required: bool, hint: &str) {
    // The id can contain a login. Escape the id like all data that comes from outside.
    let id = html_escape(id);
    let _ = write!(
        out,
        r#"<div class="field"><label class="field__label" for="{id}">{label}</label><div class="passwordRow"><input id="{id}" name="password" type="password" minlength="10"{} autocomplete="new-password" autocapitalize="off" spellcheck="false" aria-describedby="{id}-hint {id}-status"><button class="button generate" type="button" data-generate-password="{id}">Generate</button></div><p class="hint" id="{id}-hint">{hint}</p><p class="hint" id="{id}-status" aria-live="polite"></p></div>"#,
        if required { " required" } else { "" },
    );
}

fn add_form(out: &mut String, view: &UsersView) {
    // The add action is a rare task. The form shows when the address asks for it, or after a
    // refused attempt. The script of the generator loads with the form only.
    let _ = write!(
        out,
        r#"<form class="box addUser" id="add-user" method="post" action="/auth/users/add"><h3 class="box__title">Add a user</h3><input type="hidden" name="csrf" value="{}"><div class="addUser__fields"><div class="field"><label class="field__label" for="new-login">Login</label><input id="new-login" name="login" type="text" maxlength="32" pattern="[A-Za-z0-9._\-]{{1,32}}" required autocomplete="off" autocapitalize="off" spellcheck="false" aria-describedby="new-login-hint"><p class="hint" id="new-login-hint">1 to 32 characters: letters, digits, dot, dash, underscore.</p></div>"#,
        html_escape(view.token)
    );
    password_field(
        out,
        "new-password",
        "Password",
        true,
        "At least 10 characters.",
    );
    let _ = write!(
        out,
        r#"</div><p class="hint">The account starts with no grant. The next page gives it access.</p><div class="formActions"><button class="button button--primary" type="submit">Add user</button><a class="button button--ghost" href="{USERS_PATH}">Cancel</a></div></form><script src="/auth/assets/settings.js" defer></script>"#
    );
}

// One account ---------------------------------------------------------------------------------

/// The data of the page of one account.
pub struct UserView<'a> {
    pub notice: Option<Notice>,
    pub mode: Mode,
    /// The whole catalogue, not the filtered catalogue for display.
    pub catalog: &'a Catalog,
    /// The services of the catalogue with their actions. The order is the order of the catalogue.
    pub services: Vec<ServiceActions>,
    pub user: UserCard,
    /// Unix time of the first day of the graph.
    pub first_day: i64,
    /// The anti-CSRF token of the administration forms.
    pub token: &'a str,
}

pub fn user_page(frame: &Frame, view: &UserView) -> String {
    let user = &view.user;
    let login = html_escape(&user.login);
    let mut out = String::new();
    let _ = write!(
        out,
        r##"<nav class="crumbs" aria-label="Breadcrumb"><a href="{USERS_PATH}">Users</a><svg class="icon" aria-hidden="true"><use href="#i-chevron-right"/></svg><span aria-current="page">{login}</span></nav>"##
    );
    notice_html(&mut out, view.notice);
    user_head(&mut out, view);
    if user.superadmin {
        out.push_str(r#"<section class="part" aria-labelledby="access-title">"#);
        part_head(&mut out, "access-title", "Access", "", "");
        out.push_str(r#"<p class="part__lede">The superadmin has every grant on every service. The configuration file gives its login and password. This page cannot change them.</p></section>"#);
    } else {
        access_part(&mut out, view);
    }
    activity_part(&mut out, view);
    if !user.superadmin {
        account_part(&mut out, view);
    }
    page(Tab::Users, frame, &user.login, &out, !user.superadmin)
}

fn user_head(out: &mut String, view: &UserView) {
    let user = &view.user;
    let activity = &user.activity;
    let _ = write!(
        out,
        r#"<header class="userHead"><div class="userHead__id"><h2 class="userHead__name">{}</h2>"#,
        html_escape(&user.login)
    );
    if user.superadmin {
        out.push_str(r#"<span class="badge">Superadmin</span>"#);
    } else {
        let s = summary(view.catalog, &view.services, user);
        if s.services == 0 {
            out.push_str(r#"<span class="badge">No service</span>"#);
        } else {
            let _ = write!(
                out,
                r#"<span class="badge badge--accent">Opens {}</span>"#,
                plural(s.services, "service", "services")
            );
        }
        if s.to_check > 0 {
            let _ = write!(
                out,
                r##"<a class="badge badge--warn" href="#access-title">{} to check</a>"##,
                plural(s.to_check, "grant", "grants")
            );
        }
    }
    out.push_str("</div>");
    if !user.superadmin {
        let _ = write!(
            out,
            r#"<form class="userHead__action" method="post" action="/auth/users/impersonate"><input type="hidden" name="csrf" value="{}"><input type="hidden" name="login" value="{}"><button class="button" type="submit">Log in as {}</button></form>"#,
            html_escape(view.token),
            html_escape(&user.login),
            html_escape(&user.login)
        );
    }
    let last = activity
        .last_login
        .map(format_utc)
        .unwrap_or_else(|| "Never".to_string());
    let created = user
        .created_at
        .map(format_utc)
        .unwrap_or_else(|| "In the configuration".to_string());
    let _ = write!(
        out,
        r#"<dl class="facts"><div><dt>Created (UTC)</dt><dd>{created}</dd></div><div><dt>Last sign-in (UTC)</dt><dd>{last}</dd></div><div><dt>Sessions, 30 days</dt><dd>{}</dd></div><div><dt>Refused, 30 days</dt><dd{}>{}</dd></div></dl></header>"#,
        activity.sessions_30d,
        if activity.failed_30d > 0 {
            r#" class="danger""#
        } else {
            ""
        },
        activity.failed_30d,
    );
}

/// The group name of a service of the catalogue.
fn group_of<'a>(catalog: &'a Catalog, id: &str) -> Option<&'a str> {
    catalog
        .groups()
        .iter()
        .find(|g| g.services.iter().any(|s| s.id == id))
        .map(|g| g.name.as_str())
}

/// The access of one account, by service. The boxes of all the services belong to one form, the
/// form `grants`: a box names it with its `form` attribute. Thus each service can also hold its
/// own small forms for the grants below the service, and the page needs no script.
fn access_part(out: &mut String, view: &UserView) {
    let user = &view.user;
    let editable = view.mode.editable();
    let token = html_escape(view.token);
    let login = html_escape(&user.login);
    let opened = summary(view.catalog, &view.services, user).services;
    out.push_str(r#"<section class="part" aria-labelledby="access-title">"#);
    part_head(
        out,
        "access-title",
        "Access",
        &format!("Opens {opened} of {}", view.services.len()),
        "",
    );
    out.push_str(r#"<p class="part__lede"><code>access</code> opens a service. Each other box gives one action on the whole service. A grant on a resource gives the action on that resource and on everything below it.</p>"#);
    match view.mode {
        Mode::Static => out.push_str(r#"<p class="callout">The grants come from the configuration (<code>user</code> blocks, <code>access.grants_mode = "static"</code>). This page shows them and cannot change them.</p>"#),
        Mode::Hybrid if !user.configured.is_empty() => out.push_str(r#"<p class="callout">Some grants come from the configuration (<code>user</code> block). They show the mark <em>configuration</em> and cannot change here.</p>"#),
        _ => {}
    }
    if editable {
        let _ = write!(
            out,
            r#"<form id="grants" method="post" action="/auth/users/update"><input type="hidden" name="csrf" value="{token}"><input type="hidden" name="login" value="{login}"><input type="hidden" name="grants" value="1"></form>"#
        );
    }
    if view.services.is_empty() {
        out.push_str(r#"<p class="empty">No service in the catalogue yet. A service appears when a container or an IngressRoute has <code>portal.*</code> labels.</p>"#);
    }
    // The services of the catalogue by group, then the services that left the catalogue but
    // still have grants of this account.
    let mut groups: Vec<(&str, Vec<&ServiceActions>)> = Vec::new();
    for service in &view.services {
        let group = group_of(view.catalog, &service.id).unwrap_or("Other");
        match groups.iter_mut().find(|(name, _)| *name == group) {
            Some((_, list)) => list.push(service),
            None => groups.push((group, vec![service])),
        }
    }
    for (group, services) in &groups {
        let _ = write!(
            out,
            r#"<h3 class="groupLabel">{}</h3><div class="svcList">"#,
            html_escape(group)
        );
        for service in services {
            service_access(out, view, service);
        }
        out.push_str("</div>");
    }
    let gone: Vec<&Grant> = user
        .grants
        .iter()
        .filter(|g| !view.services.iter().any(|s| s.id == g.service))
        .collect();
    if !gone.is_empty() {
        out.push_str(r#"<h3 class="groupLabel">Not in the catalogue</h3><div class="box"><p class="hint">These services are not in the catalogue now. Their grants stay and apply when the service comes back.</p><ul class="tree">"#);
        for grant in gone {
            let _ = write!(
                out,
                r#"<li class="tree__row"><code>{}</code></li>"#,
                html_escape(&grant.to_string())
            );
        }
        out.push_str("</ul></div>");
    }
    if editable && !view.services.is_empty() {
        out.push_str(r#"<div class="saveBar"><button class="button button--primary" type="submit" form="grants">Save access</button><p class="hint">Saves the boxes of every service. A grant on a resource saves at once with its own button.</p></div>"#);
    }
    out.push_str("</section>");
}

/// One service on the page of an account: the boxes for the whole service, then the grants on
/// its resource tree.
fn service_access(out: &mut String, view: &UserView, service: &ServiceActions) {
    let user = &view.user;
    let editable = view.mode.editable();
    let id = html_escape(&service.id);
    let opens = holds(&user.grants, ACCESS, &service.id);
    let whole: Vec<&(String, String)> = service
        .actions
        .iter()
        .filter(|(name, _)| holds(&user.grants, name, &service.id))
        .collect();
    let below: Vec<&Grant> = user
        .grants
        .iter()
        .filter(|g| g.service == service.id && !g.path.is_empty())
        .collect();
    let held = opens || !whole.is_empty() || !below.is_empty();
    let to_check = below.iter().filter(|g| service.undeclared(g)).count();
    let _ = write!(
        out,
        r##"<details class="svc"{}><summary class="svc__head"><svg class="icon svc__chevron" aria-hidden="true"><use href="#i-chevron-right"/></svg><span class="svc__name">{}</span><span class="svc__state">"##,
        if held { " open" } else { "" },
        html_escape(&service.name),
    );
    if opens {
        out.push_str(r#"<span class="badge badge--accent">Opens</span>"#);
    } else if held {
        out.push_str(r#"<span class="badge badge--warn">No access box</span>"#);
    } else {
        out.push_str(r#"<span class="muted">No access</span>"#);
    }
    if !service.actions.is_empty() {
        let _ = write!(
            out,
            r#"<span class="svc__count">{} of {} actions</span>"#,
            whole.len(),
            service.actions.len()
        );
    }
    if !below.is_empty() {
        let _ = write!(
            out,
            r#"<span class="svc__count">{}</span>"#,
            plural(below.len(), "resource grant", "resource grants")
        );
    }
    if to_check > 0 {
        let _ = write!(
            out,
            r#"<span class="badge badge--warn">{to_check} to check</span>"#
        );
    }
    out.push_str(r#"</span></summary><div class="svc__body">"#);

    // The whole service.
    let _ = write!(
        out,
        r#"<fieldset class="node"><legend class="node__title">Whole service <code>service:{id}</code></legend><div class="checks">"#
    );
    let actions = std::iter::once((
        ACCESS,
        "Open the service. Without it, the account does not see the service.",
    ))
    .chain(
        service
            .actions
            .iter()
            .map(|(n, d)| (n.as_str(), d.as_str())),
    );
    for (action, description) in actions {
        let checked = holds(&user.grants, action, &service.id);
        let configured = holds(&user.configured, action, &service.id);
        let fixed = !editable || configured;
        let _ = write!(
            out,
            r#"<label class="check{}"><input type="checkbox"{}{}{}><span class="check__text"><code class="check__name">{}</code>"#,
            if action == ACCESS {
                " check--access"
            } else {
                ""
            },
            if fixed {
                String::new()
            } else {
                format!(
                    r#" form="grants" name="g.{id}.{}" value="1""#,
                    html_escape(action)
                )
            },
            if checked { " checked" } else { "" },
            if fixed { " disabled" } else { "" },
            html_escape(action),
        );
        if configured && editable {
            out.push_str(r#" <span class="badge">configuration</span>"#);
        }
        if !description.is_empty() {
            let _ = write!(
                out,
                r#"<span class="check__desc">{}</span>"#,
                html_escape(description)
            );
        }
        out.push_str("</span></label>");
    }
    out.push_str("</div></fieldset>");

    // The resource tree.
    let levels = if editable {
        service.levels()
    } else {
        Vec::new()
    };
    if below.is_empty() && levels.is_empty() {
        out.push_str("</div></details>");
        return;
    }
    out.push_str(r#"<div class="node"><p class="node__title">On resources</p>"#);
    if below.is_empty() {
        out.push_str(r#"<p class="hint">No grant on a resource of this service.</p>"#);
    } else {
        out.push_str(r#"<ul class="tree">"#);
        for grant in below {
            resource_grant(out, view, service, grant);
        }
        out.push_str("</ul>");
    }
    if !levels.is_empty() {
        add_grant(out, view, service, &levels);
    }
    out.push_str("</div></div></details>");
}

/// The path of a grant for a human: each segment as its type and its decoded id, with `/`
/// between the segments, as the text form of a grant writes it.
fn path_html(grant: &Grant) -> String {
    let mut out = String::from(r#"<span class="path">"#);
    for (index, segment) in grant.path.iter().enumerate() {
        if index > 0 {
            out.push_str(r#"<span class="path__sep" aria-hidden="true">/</span>"#);
        }
        let _ = write!(
            out,
            r#"<span class="path__seg"><span class="path__type">{}</span> <code class="path__id">{}</code></span>"#,
            html_escape(&segment.kind),
            html_escape(&segment.id)
        );
    }
    out.push_str("</span>");
    out
}

/// One grant below a service: the action, the path, then the mark of the configuration or the
/// Remove button, and the problem when the service does not declare it.
fn resource_grant(out: &mut String, view: &UserView, service: &ServiceActions, grant: &Grant) {
    let user = &view.user;
    let problem = service.problem(grant);
    let _ = write!(
        out,
        r#"<li class="tree__row{}"><code class="tree__action">{}</code><span class="tree__on">on</span>{}<span class="tree__below">and below</span>"#,
        if problem.is_some() {
            " tree__row--warn"
        } else {
            ""
        },
        html_escape(&grant.action),
        path_html(grant),
    );
    if user.configured.contains(grant) {
        out.push_str(r#"<span class="badge tree__end">configuration</span>"#);
    } else if view.mode.editable() {
        let _ = write!(
            out,
            r#"<form class="tree__end" method="post" action="/auth/users/grant-delete"><input type="hidden" name="csrf" value="{}"><input type="hidden" name="login" value="{}"><input type="hidden" name="grant" value="{}"><button class="button button--small" type="submit" aria-label="Remove {} on {}">Remove</button></form>"#,
            html_escape(view.token),
            html_escape(&user.login),
            html_escape(&grant.to_string()),
            html_escape(&grant.action),
            html_escape(&grant.path_text()),
        );
    }
    if let Some(problem) = problem {
        let _ = write!(
            out,
            r##"<p class="tree__problem"><svg class="icon" aria-hidden="true"><use href="#i-alert-triangle"/></svg><span><strong>To check:</strong> {}. The PDP does not apply this grant.</span></p>"##,
            html_escape(&problem)
        );
    }
    out.push_str("</li>");
}

/// The forms that add a grant on a resource: one form for each declared level, behind one
/// disclosure. Each form shows the path that it gives, with one id field for each type of the
/// chain. The server does not list the resources: the superadmin types the ids.
fn add_grant(out: &mut String, view: &UserView, service: &ServiceActions, levels: &[Level]) {
    let token = html_escape(view.token);
    let login = html_escape(&view.user.login);
    let id = html_escape(&service.id);
    let _ = write!(
        out,
        r#"<details class="addGrant"><summary class="button button--small">Add a grant on a resource of {}</summary><div class="addGrant__forms">"#,
        html_escape(&service.name)
    );
    for level in levels {
        let Some(kind) = level.chain.last() else {
            continue;
        };
        let kind = html_escape(kind);
        let pattern = level
            .chain
            .iter()
            .map(|k| format!("{}:&lt;id&gt;", html_escape(k)))
            .collect::<Vec<_>>()
            .join("/");
        let _ = write!(
            out,
            r#"<form class="addGrant__form" method="post" action="/auth/users/grant-add"><input type="hidden" name="csrf" value="{token}"><input type="hidden" name="login" value="{login}"><input type="hidden" name="service" value="{id}"><input type="hidden" name="type" value="{kind}"><p class="addGrant__level">On one <strong>{kind}</strong> <code>service:{id}/{pattern}</code></p><div class="addGrant__fields"><label class="field"><span class="field__label">Action</span><select name="action" required>"#
        );
        for action in &level.actions {
            let _ = write!(out, "<option>{}</option>", html_escape(action));
        }
        out.push_str("</select></label>");
        for (index, step) in level.chain.iter().enumerate() {
            let _ = write!(
                out,
                r#"<label class="field"><span class="field__label">Id of the {}</span><input name="id{index}" type="text" required maxlength="256" autocomplete="off" autocapitalize="off" spellcheck="false"></label>"#,
                html_escape(step)
            );
        }
        let _ = write!(
            out,
            r#"<button class="button" type="submit" aria-label="Add the grant on one {kind}">Add</button></div></form>"#
        );
    }
    out.push_str("</div></details>");
}

/// What the account did: the connections per day, the latest sessions and the services used.
fn activity_part(out: &mut String, view: &UserView) {
    let activity = &view.user.activity;
    out.push_str(r#"<section class="part" aria-labelledby="activity-title">"#);
    part_head(
        out,
        "activity-title",
        "Activity",
        &format!("Last {} days", activity.days.len()),
        "",
    );
    let total: u32 = activity.days.iter().map(|d| d.sessions).sum();
    let _ = write!(
        out,
        r#"<div class="chart"><p class="chart__text"><strong>{}</strong>{}</p>{}</div>"#,
        plural(total as usize, "connection", "connections"),
        peak_text(&activity.days, view.first_day),
        spark(&activity.days, true),
    );
    out.push_str(
        r#"<div class="split"><div class="split__col"><h3 class="subTitle">Latest sessions</h3>"#,
    );
    if activity.recent.is_empty() {
        out.push_str(r#"<p class="empty">No session recorded.</p>"#);
    } else {
        out.push_str(
            r#"<div class="tableBox"><table class="table stack"><thead><tr><th scope="col">Started (UTC)</th><th scope="col">Address</th><th scope="col">Browser</th><th scope="col">Duration</th><th scope="col">End</th></tr></thead><tbody>"#,
        );
        for session in &activity.recent {
            session_row(out, session);
        }
        out.push_str("</tbody></table></div>");
    }
    out.push_str(r#"</div><div class="split__col split__col--narrow"><h3 class="subTitle">Services used</h3>"#);
    if activity.services.is_empty() {
        out.push_str(r#"<p class="empty">No service used yet.</p>"#);
    } else {
        out.push_str(
            r#"<div class="tableBox"><table class="table"><thead><tr><th scope="col">Service</th><th scope="col" class="num">Requests</th><th scope="col">Last use (UTC)</th></tr></thead><tbody>"#,
        );
        for used in &activity.services {
            service_use_row(out, view.catalog, used);
        }
        out.push_str("</tbody></table></div>");
    }
    out.push_str("</div></div></section>");
}

/// The password and the deletion of the account.
fn account_part(out: &mut String, view: &UserView) {
    let token = html_escape(view.token);
    let login = html_escape(&view.user.login);
    out.push_str(r#"<section class="part" aria-labelledby="account-title">"#);
    part_head(out, "account-title", "Account", "", "");
    let _ = write!(
        out,
        r#"<div class="split"><form class="box split__col" method="post" action="/auth/users/update"><h3 class="box__title">Password</h3><input type="hidden" name="csrf" value="{token}"><input type="hidden" name="login" value="{login}">"#
    );
    password_field(
        out,
        &format!("pw-{}", view.user.login),
        "New password",
        true,
        "At least 10 characters. The sessions of this account end at once.",
    );
    let _ = write!(
        out,
        r#"<div class="formActions"><button class="button" type="submit">Change password</button></div></form><div class="box box--danger split__col"><h3 class="box__title">Delete the account</h3><p class="hint">Its sessions end at once. Its history stays until it expires.</p><details class="confirm"><summary class="button button--danger">Delete {login}</summary><form class="confirm__panel" method="post" action="/auth/users/delete"><input type="hidden" name="csrf" value="{token}"><input type="hidden" name="login" value="{login}"><p>This cannot be undone.</p><button class="button button--dangerFill" type="submit">Confirm: delete {login}</button></form></details></div></div></section>"#
    );
}

/// Example text for a graph: " Peak: 7 on 2026-10-04." The text puts the values in the page.
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
            r#"<span class="badge badge--ok">Active</span>"#,
        ),
    };
    let _ = write!(
        out,
        r#"<tr><td data-label="Started (UTC)">{}{}</td><td data-label="Address">{}</td><td data-label="Browser" title="{}">{}</td><td data-label="Duration">{}</td><td data-label="End">{}</td></tr>"#,
        format_utc_secs(session.started_at),
        if session.impersonated {
            r#" <span class="badge">by the superadmin</span>"#
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
        r#"<tr><td>{}</td><td class="num">{}</td><td>{}</td></tr>"#,
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
        Some("user_deleted") => "Account deleted",
        Some("impersonation") => "Switched account",
        _ => "Ended",
    }
}

fn audit_label(kind: &str) -> &'static str {
    match kind {
        "login_failed" => "Refused sign-in",
        "user_added" => "Account added",
        "user_deleted" => "Account deleted",
        "user_updated" => "Grants changed",
        "password_reset" => "Password changed",
        "impersonation_started" => "Signed in as the account",
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
    let mut out = String::new();
    let count = view.catalog.service_count();
    let publishing = view
        .services
        .iter()
        .filter(|s| !s.actions.is_empty())
        .count();
    out.push_str(r#"<section class="part" aria-labelledby="model-title">"#);
    part_head(&mut out, "model-title", "How a grant reads", "", "");
    out.push_str(r#"<p class="part__lede">A grant gives one account one action on one node of a service. A grant on a node covers every node below it. Accounts get grants on their own page.</p><dl class="model"><div class="model__row"><dt><code>access on service:<var>id</var></code></dt><dd>Opens the service. Without it, the account does not see the service and the gate refuses it.</dd></div><div class="model__row"><dt><code><var>action</var> on service:<var>id</var></code></dt><dd>The action on the whole service and on every resource in it.</dd></div><div class="model__row"><dt><code><var>action</var> on service:<var>id</var>/<var>type</var>:<var>id</var></code></dt><dd>The action on that resource and on everything below it, for example a database and its tables. A path has 1 to 4 levels and follows the resource types that the service declares.</dd></div><div class="model__row"><dt>Subtree match</dt><dd>When a service asks with <code>context.match = "subtree"</code>, the answer is yes also for a node above a grant. The service can then list that node, so the account can reach what is below it.</dd></div></dl></section>"#);
    undeclared_grants(&mut out, view);
    out.push_str(r#"<section class="part" aria-labelledby="services-title">"#);
    part_head(
        &mut out,
        "services-title",
        "Services",
        &format!(
            "{}, {publishing} with actions",
            plural(count, "service", "services")
        ),
        "",
    );
    out.push_str(r#"<p class="part__lede">Each service publishes its actions and its resource types at <code>/.well-known/access-actions</code>. A service without them has only <code>access</code>, and <code>access</code> is then the full permission.</p>"#);
    if count == 0 {
        out.push_str(r#"<p class="empty">No service in the catalogue yet. A service appears when a container or an IngressRoute has <code>portal.*</code> labels.</p>"#);
    }
    for group in view.catalog.groups() {
        let _ = write!(
            out,
            r#"<h3 class="groupLabel">{}</h3><div class="svcGrid">"#,
            html_escape(&group.name)
        );
        for service in &group.services {
            service_card(&mut out, view, service);
        }
        out.push_str("</div>");
    }
    out.push_str("</section>");
    page(Tab::Services, frame, "Services", &out, false)
}

fn service_card(out: &mut String, view: &ServicesView, service: &Service) {
    let problem = view.problems.iter().find(|(id, _)| *id == service.id);
    let actions = view.services.iter().find(|s| s.id == service.id);
    let _ = write!(
        out,
        r#"<article class="box svcCard" aria-labelledby="svc-{0}"><div class="svcCard__head"><h4 class="svcCard__name" id="svc-{0}">{1}</h4><code class="svcCard__id">{0}</code></div><p class="svcCard__path"><code>{2}</code></p>"#,
        html_escape(&service.id),
        html_escape(&service.name),
        html_escape(&service.href),
    );
    if let Some((_, text)) = problem {
        let _ = write!(
            out,
            r##"<p class="callout callout--warn"><svg class="icon" aria-hidden="true"><use href="#i-alert-triangle"/></svg><span>The actions cannot be read: {}. The page keeps the last good list.</span></p>"##,
            html_escape(text)
        );
    }
    out.push_str(r#"<dl class="kv">"#);
    // Who opens the service.
    let users: Vec<&String> = view
        .holders
        .iter()
        .filter(|(_, grants)| holds(grants, ACCESS, &service.id))
        .map(|(login, _)| login)
        .collect();
    out.push_str("<div><dt>Opened by</dt><dd>");
    if users.is_empty() {
        out.push_str(r#"<span class="muted">The superadmin only</span>"#);
    } else {
        out.push_str(r#"<span class="badges">"#);
        for login in users {
            let _ = write!(
                out,
                r#"<a class="badge badge--link" href="{}">{}</a>"#,
                html_escape(&user_path(login)),
                html_escape(login)
            );
        }
        out.push_str("</span>");
    }
    out.push_str("</dd></div>");
    // The actions.
    out.push_str("<div><dt>Actions</dt><dd>");
    match actions {
        Some(found) if !found.actions.is_empty() => {
            out.push_str(r#"<ul class="actionList"><li><code>access</code><span>Open the service.</span></li>"#);
            for (name, description) in &found.actions {
                let levels = found
                    .document
                    .actions
                    .iter()
                    .find(|a| &a.name == name)
                    .map(|a| {
                        a.resource_types
                            .iter()
                            .map(|t| html_escape(t))
                            .collect::<Vec<_>>()
                            .join(", ")
                    })
                    .unwrap_or_default();
                let _ = write!(
                    out,
                    r#"<li><code>{}</code><span>{}{}</span></li>"#,
                    html_escape(name),
                    html_escape(description),
                    if levels.is_empty() {
                        String::new()
                    } else {
                        format!(r#" <span class="muted">On: {levels}.</span>"#)
                    }
                );
            }
            out.push_str("</ul>");
        }
        _ => out.push_str(r#"<span class="muted"><code>access</code> only: opening the service is the full permission.</span>"#),
    }
    out.push_str("</dd></div>");
    // The tree of resource types.
    if let Some(found) = actions.filter(|f| !f.document.types.is_empty()) {
        out.push_str(r#"<div><dt>Resources</dt><dd><ul class="typeTree"><li><code>service</code>"#);
        type_children(out, &found.document, "service", 0);
        out.push_str("</li></ul></dd></div>");
    }
    if !service.exclude.is_empty() {
        out.push_str(r#"<div><dt>Open without sign-in</dt><dd>"#);
        for rule in &service.exclude {
            let _ = write!(
                out,
                r#"<code class="rule">{}</code>"#,
                html_escape(rule.text())
            );
        }
        out.push_str("</dd></div>");
    }
    out.push_str("</dl></article>");
}

/// The children of `parent` in the tree of types, as a nested list with their descriptions.
fn type_children(out: &mut String, document: &Document, parent: &str, depth: usize) {
    let children: Vec<_> = document
        .types
        .iter()
        .filter(|t| t.parent == parent)
        .collect();
    if children.is_empty() || depth > crate::grants::MAX_DEPTH {
        return;
    }
    out.push_str("<ul>");
    for child in children {
        let _ = write!(out, "<li><code>{}</code>", html_escape(&child.name));
        if !child.description.is_empty() {
            let _ = write!(
                out,
                r#" <span class="muted">{}</span>"#,
                html_escape(&child.description)
            );
        }
        type_children(out, document, &child.name, depth + 1);
        out.push_str("</li>");
    }
    out.push_str("</ul>");
}

/// The grants below a service that do not follow its document: the action does not list the
/// type, or the path does not follow the parents.
/// At start, the server does not know the services: the catalogue and the actions documents come
/// at run time. Thus a grant of the configuration can name a type that the service does not
/// declare. This part shows each such grant with its problem and a link to its account. The part
/// is absent when there is none.
fn undeclared_grants(out: &mut String, view: &ServicesView) {
    let mut rows = String::new();
    let mut count = 0;
    for (login, grants) in view.holders {
        for grant in grants {
            let Some(problem) = view
                .services
                .iter()
                .filter(|s| s.id == grant.service)
                .find_map(|s| s.problem(grant))
            else {
                continue;
            };
            count += 1;
            let _ = write!(
                rows,
                r#"<tr><th scope="row" data-label="Account"><a href="{}">{}</a></th><td data-label="Grant"><code class="grantText">{}</code></td><td data-label="Problem">{}</td></tr>"#,
                html_escape(&user_path(login)),
                html_escape(login),
                html_escape(&grant.to_string()),
                html_escape(&problem),
            );
        }
    }
    if rows.is_empty() {
        return;
    }
    out.push_str(r#"<section class="part part--warn" aria-labelledby="checks-title">"#);
    part_head(
        out,
        "checks-title",
        "Grants to check",
        &count.to_string(),
        "",
    );
    let _ = write!(
        out,
        r#"<p class="part__lede">The service does not declare the resource of these grants for the action. The PDP does not apply them. Correct the grant on the page of the account, or the document of the service.</p><div class="tableBox"><table class="table stack"><thead><tr><th scope="col">Account</th><th scope="col">Grant</th><th scope="col">Problem</th></tr></thead><tbody>{rows}</tbody></table></div></section>"#
    );
}

// Activity ------------------------------------------------------------------------------------

pub struct LoginsView<'a> {
    pub page: &'a LoginPage,
    /// Only the refused attempts.
    pub only_failed: bool,
    pub retention_days: u32,
    /// Every real connection, per day, all accounts.
    pub days: &'a [DayStat],
    pub first_day: i64,
    /// The latest audit events.
    pub recent_events: &'a [AuditRow],
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
    let connections: u32 = view.days.iter().map(|d| d.sessions).sum();
    let _ = write!(
        out,
        r#"<div class="stats"><div class="stat"><span class="stat__label">Sign-in attempts, 24 hours</span><span class="stat__value">{}</span></div><div class="stat"><span class="stat__label">Refused, 24 hours</span><span class="stat__value{}">{}</span></div><div class="stat"><span class="stat__label">Addresses refused, 24 hours</span><span class="stat__value">{}</span></div><div class="stat"><span class="stat__label">Connections, {} days</span><span class="stat__value">{connections}</span></div></div>"#,
        data.attempts_24h,
        if data.failed_24h > 0 {
            " stat__value--danger"
        } else {
            ""
        },
        data.failed_24h,
        data.addresses_24h,
        view.days.len(),
    );
    out.push_str(r#"<div class="layout"><div class="layout__main">"#);
    out.push_str(r#"<section class="part" aria-labelledby="attempts-title">"#);
    let mut filter = String::from(r#"<div class="segmented" role="group" aria-label="Show">"#);
    for (label, only_failed) in [("All", false), ("Refused", true)] {
        let current = only_failed == view.only_failed;
        let _ = write!(
            filter,
            r#"<a class="segmented__option{}" href="{}"{}>{label}</a>"#,
            if current { " is-active" } else { "" },
            logins_href(0, only_failed),
            if current {
                r#" aria-current="true""#
            } else {
                ""
            },
        );
    }
    filter.push_str("</div>");
    part_head(
        &mut out,
        "attempts-title",
        "Sign-in attempts",
        &if view.only_failed {
            format!("{} refused", data.matching)
        } else {
            plural(data.matching as usize, "attempt", "attempts")
        },
        &filter,
    );
    let _ = write!(
        out,
        r#"<p class="part__lede">The login is recorded as typed, cut to {TYPED_LOGIN_MAX} characters, whether or not the account exists. The password is never recorded. Kept {} days, at most 20,000 attempts. The rate limiter blocks some attempts before this log. Times are UTC.</p>"#,
        view.retention_days,
    );
    if data.rows.is_empty() {
        out.push_str(if view.only_failed {
            r#"<p class="empty">No refused attempt in the log.</p>"#
        } else {
            r#"<p class="empty">No attempt recorded yet.</p>"#
        });
    } else {
        out.push_str(
            r#"<div class="tableBox"><table class="table stack"><thead><tr><th scope="col">When (UTC)</th><th scope="col">Result</th><th scope="col">Login typed</th><th scope="col">Address</th><th scope="col">Browser</th></tr></thead><tbody>"#,
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
    out.push_str(r#"</section></div><div class="layout__side">"#);
    out.push_str(r#"<section class="part" aria-labelledby="connections-title">"#);
    part_head(
        &mut out,
        "connections-title",
        "Connections",
        "All accounts",
        "",
    );
    let _ = write!(
        out,
        r#"<div class="chart"><p class="chart__text"><strong>{}</strong> in the last {} days.{}</p>{}</div></section>"#,
        plural(connections as usize, "connection", "connections"),
        view.days.len(),
        peak_text(view.days, view.first_day),
        spark(view.days, true),
    );
    out.push_str(r#"<section class="part" aria-labelledby="events-title">"#);
    part_head(&mut out, "events-title", "Recent changes", "", "");
    if view.recent_events.is_empty() {
        out.push_str(r#"<p class="empty">No change recorded yet.</p>"#);
    } else {
        out.push_str(r#"<ol class="events">"#);
        for event in view.recent_events {
            let account = match &event.login {
                Some(login) if !login.is_empty() => format!(
                    r#" <a href="{}">{}</a>"#,
                    html_escape(&user_path(login)),
                    html_escape(login)
                ),
                _ => String::new(),
            };
            let by = match &event.actor {
                Some(actor) if !actor.is_empty() => format!(" by {}", html_escape(actor)),
                _ => String::new(),
            };
            let _ = write!(
                out,
                r#"<li class="events__row{}"><span class="events__what">{}{account}</span><span class="events__meta">{}{by}, from {}</span></li>"#,
                if event.kind == "login_failed" {
                    " events__row--danger"
                } else {
                    ""
                },
                audit_label(&event.kind),
                format_utc_secs(event.ts),
                opt(&event.ip),
            );
        }
        out.push_str("</ol>");
    }
    out.push_str("</section></div></div>");
    page(Tab::Logins, frame, "Activity", &out, false)
}

fn login_row(out: &mut String, row: &LoginRow) {
    let result = if row.success {
        r#"<span class="badge badge--ok">Signed in</span>"#
    } else {
        r#"<span class="badge badge--danger">Refused</span>"#
    };
    let typed = if row.login.is_empty() {
        r#"<span class="muted">(empty)</span>"#.to_string()
    } else {
        format!(r#"<code class="typed">{}</code>"#, html_escape(&row.login))
    };
    let _ = write!(
        out,
        r#"<tr><td data-label="When (UTC)">{}</td><td data-label="Result">{result}</td><td data-label="Login typed">{typed}{}</td><td data-label="Address">{}</td><td data-label="Browser" title="{}">{}</td></tr>"#,
        format_utc_secs(row.ts),
        if row.known {
            ""
        } else {
            r#" <span class="badge">no such account</span>"#
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

    fn sub(text: &str) -> Grant {
        Grant::parse(text).unwrap()
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

    /// The document of Grafana in the tests: a folder, and the dashboards of a folder.
    const GRAFANA: &str = r#"{"server":"infra-grafana","resource_types":[{"name":"folder","parent":"service","description":"A folder."},{"name":"dashboard","parent":"folder"}],"actions":[{"name":"dashboards.read","resource_types":["service","folder"],"description":"See the dashboards."},{"name":"dashboards.write","resource_types":["service","folder","dashboard"],"description":"Change the <dashboards>."}]}"#;

    fn services() -> Vec<ServiceActions> {
        let plain = |id: &str, name: &str| ServiceActions {
            id: id.into(),
            name: name.into(),
            actions: Vec::new(),
            document: Default::default(),
            known: true,
        };
        let grafana = crate::actions::parse("infra-grafana", GRAFANA.as_bytes()).unwrap();
        vec![
            ServiceActions {
                id: "infra-grafana".into(),
                name: "Grafana".into(),
                actions: grafana
                    .actions
                    .iter()
                    .map(|a| (a.name.clone(), a.description.clone()))
                    .collect(),
                document: std::sync::Arc::new(grafana),
                known: true,
            },
            plain("infra-notes", "Notes"),
            plain("infra-typo", "Typo"),
            plain("infra-mixed", "Mixed"),
        ]
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

    fn view(catalog: &Catalog) -> UsersView<'_> {
        let mut root = card("root", &[]);
        root.superadmin = true;
        root.created_at = None;
        UsersView {
            notice: None,
            add_open: false,
            mode: Mode::Dynamic,
            catalog,
            services: services(),
            users: vec![
                card("alice", &["grafana", "mixed"]),
                card("bob", &["mixed"]),
                card("carol", &[]),
            ],
            superadmin: root,
            unknown_failures_30d: 0,
            max_users: 64,
            token: "tok<en",
        }
    }

    fn user_view(catalog: &Catalog, user: UserCard) -> UserView<'_> {
        UserView {
            notice: None,
            mode: Mode::Dynamic,
            catalog,
            services: services(),
            user,
            first_day: 1_788_000_000,
            token: "tok<en",
        }
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

    fn logins<'a>(
        page: &'a LoginPage,
        only_failed: bool,
        events: &'a [AuditRow],
    ) -> LoginsView<'a> {
        LoginsView {
            page,
            only_failed,
            retention_days: 90,
            days: &[],
            first_day: 1_788_000_000,
            recent_events: events,
        }
    }

    #[test]
    fn every_page_is_a_whole_document_with_the_same_header_skip_link_and_three_tab_links() {
        let cat = catalog();
        let actions = services();
        let holders = vec![("alice".to_string(), access(&["grafana"]))];
        let blank = LoginPage::default();
        let pages = [
            (Tab::Users, users_page(&frame(), &view(&cat))),
            (
                Tab::Users,
                user_page(&frame(), &user_view(&cat, card("alice", &["grafana"]))),
            ),
            (
                Tab::Services,
                services_page(&frame(), &services_view(&cat, &actions, &holders)),
            ),
            (
                Tab::Logins,
                logins_page(&frame(), &logins(&blank, false, &[])),
            ),
        ];
        for (tab, html) in &pages {
            assert!(html.starts_with("<!doctype html>"), "{tab:?}");
            assert!(html.contains(r#"<link rel="stylesheet" href="/auth/settings.css">"#));
            assert!(html.contains(r##"<a class="skipLink" href="#main">Skip to content</a>"##));
            assert!(html.contains(r#"<main class="settings" id="main""#));
            assert!(html.contains(r##"<a class="backLink" href="/">"##));
            assert!(html.contains("Back to panel"));
            assert!(html.contains(r#"action="/auth/logout""#) && html.contains("out&lt;"));
            assert_eq!(html.matches(r#"class="contentTabs__tab"#).count(), 3);
            assert_eq!(html.matches(r#"is-active" aria-current="page""#).count(), 1);
            assert!(html.contains(&format!(
                r#"<a class="contentTabs__tab is-active" aria-current="page" href="{}">{}</a>"#,
                tab.path(),
                tab.label()
            )));
            assert!(!html.contains(r#"class="modal"#), "no popup");
            assert!(!html.contains(" style="), "no inline style (CSP)");
            assert!(!html.contains('\u{2014}') && !html.contains('\u{2013}'));
        }
        assert!(
            !pages[0].1.contains("settings.js"),
            "the add form is closed"
        );
        assert!(pages[1].1.contains("/auth/assets/settings.js"));
        for (_, html) in &pages[2..] {
            assert!(!html.contains("settings.js"));
        }
    }

    #[test]
    fn a_page_inlines_only_the_icons_it_draws() {
        let cat = catalog();
        let users = users_page(&frame(), &view(&cat));
        assert!(!users.contains(r#"<symbol id="i-chevron-right""#));
        assert!(!users.contains(r#"<symbol id="i-alert-triangle""#));
        let alice = user_page(&frame(), &user_view(&cat, card("alice", &["grafana"])));
        assert!(alice.contains(r#"<symbol id="i-chevron-right""#));
    }

    #[test]
    fn the_users_page_is_a_table_of_accounts_that_link_to_their_own_page() {
        let cat = catalog();
        let mut v = view(&cat);
        v.users[1]
            .grants
            .push(sub("dashboards.read on service:infra-grafana/team:x"));
        v.users[0].activity.failed_30d = 2;
        let html = users_page(&frame(), &v);
        for login in ["root", "alice", "bob", "carol"] {
            assert!(html.contains(&format!(
                r#"<a class="accountLink" href="/auth/settings/users/{login}">{login}</a>"#
            )));
        }
        assert!(html.contains("Superadmin</span>"));
        assert!(html.contains("Every service and every action"));
        assert!(html.contains(r#"<span class="badge badge--accent">Opens 2 services</span>"#));
        assert!(html.contains(r#"<span class="badge badge--accent">Opens 1 service</span>"#));
        assert!(
            html.contains(r#"<span class="muted">No access</span>"#),
            "carol"
        );
        assert!(
            html.contains(r#"<span class="badge badge--warn">1 to check</span>"#),
            "bob holds a grant that Grafana does not declare"
        );
        assert!(
            html.contains(r#"<td class="num num--danger" data-label="Refused, 30 days">2</td>"#)
        );
        assert!(html.contains("2 refused sign-ins in the last 30 days"));
        assert!(html.contains(r#"href="/auth/settings/logins?r=failed""#));
        assert!(html.contains("3 of 64"));
        assert!(
            !html.contains(r#"action="/auth/users/update""#),
            "no editor on the list"
        );
        assert!(html.contains(r#"action="/auth/users/end-sessions""#));
    }

    #[test]
    fn the_add_form_opens_on_request_or_after_a_refused_attempt_only() {
        let cat = catalog();
        let html = users_page(&frame(), &view(&cat));
        assert!(!html.contains(r#"action="/auth/users/add""#));
        assert!(html.contains(r##"href="/auth/settings?add=1#add-user">Add a user</a>"##));
        let mut asked = view(&cat);
        asked.add_open = true;
        let html = users_page(&frame(), &asked);
        assert!(html.contains(r#"id="add-user" method="post" action="/auth/users/add""#));
        assert!(html.contains(r#"data-generate-password="new-password""#));
        assert!(html.contains("/auth/assets/settings.js"));
        let add = &html[html.find(r#"action="/auth/users/add""#).unwrap()..];
        let add = &add[..add.find("</form>").unwrap()];
        assert!(!add.contains(r#"name="g."#), "a new account has no grant");
        for refused in [
            "exists",
            "weak-password",
            "invalid-login",
            "limit",
            "reserved",
        ] {
            let mut v = view(&cat);
            v.notice = notice_for(refused);
            let html = users_page(&frame(), &v);
            assert!(html.contains(r#"action="/auth/users/add""#), "{refused}");
            assert!(html.contains(r#"class="notice notice--error" role="alert""#));
        }
        let mut other = view(&cat);
        other.notice = notice_for("edit-weak-password");
        assert!(!users_page(&frame(), &other).contains(r#"action="/auth/users/add""#));
    }

    #[test]
    fn the_account_page_ticks_the_held_boxes_in_one_form_and_keeps_the_password_apart() {
        let cat = catalog();
        let html = user_page(
            &frame(),
            &user_view(&cat, card("alice", &["grafana", "mixed"])),
        );
        assert!(html.contains(r#"<form id="grants" method="post" action="/auth/users/update"><input type="hidden" name="csrf" value="tok&lt;en"><input type="hidden" name="login" value="alice"><input type="hidden" name="grants" value="1"></form>"#));
        assert!(html.contains(r#"form="grants" name="g.infra-grafana.access" value="1" checked>"#));
        assert!(html.contains(r#"form="grants" name="g.infra-mixed.access" value="1" checked>"#));
        assert!(html.contains(r#"form="grants" name="g.infra-notes.access" value="1">"#));
        assert!(html.contains(r#"form="grants" name="g.infra-grafana.dashboards.read" value="1">"#));
        assert!(html.contains("Change the &lt;dashboards&gt;."), "escaped");
        assert!(html.contains(r#"<button class="button button--primary" type="submit" form="grants">Save access</button>"#));
        assert!(html.contains("Opens 2 of 4"));
        // The password form sends no box and no `grants` field.
        let start = html.rfind(r#"action="/auth/users/update""#).unwrap();
        let password = &html[start..start + html[start..].find("</form>").unwrap()];
        assert!(password.contains(r#"id="pw-alice""#) && password.contains(" required"));
        assert!(!password.contains(r#"name="grants""#) && !password.contains(r#"name="g."#));
        assert!(html.contains(r#"action="/auth/users/impersonate""#));
        assert!(html.contains("Confirm: delete alice"));
        // A service with a grant opens; a service without one stays closed.
        assert!(html.contains(r#"<details class="svc" open><summary class="svc__head">"#));
        assert!(html.contains(r#"<span class="muted">No access</span>"#));
    }

    #[test]
    fn the_account_page_writes_each_resource_grant_as_its_path_with_its_problem() {
        let cat = catalog();
        let mut alice = card("alice", &["grafana"]);
        alice.grants.push(sub(
            "dashboards.write on service:infra-grafana/folder:ops/dashboard:a%2Fb",
        ));
        alice
            .grants
            .push(sub("dashboards.read on service:infra-grafana/team:%3Cx%3E"));
        alice.grants.push(sub("x.read on service:gone/thing:a"));
        let html = user_page(&frame(), &user_view(&cat, alice));
        assert!(html.contains(r#"<code class="tree__action">dashboards.write</code><span class="tree__on">on</span><span class="path"><span class="path__seg"><span class="path__type">folder</span> <code class="path__id">ops</code></span><span class="path__sep" aria-hidden="true">/</span><span class="path__seg"><span class="path__type">dashboard</span> <code class="path__id">a/b</code></span></span><span class="tree__below">and below</span>"#), "decoded ids");
        assert!(html.contains(r#"name="grant" value="dashboards.write on service:infra-grafana/folder:ops/dashboard:a%2Fb""#));
        assert!(
            html.contains(r#"aria-label="Remove dashboards.write on folder:ops/dashboard:a%2Fb""#)
        );
        assert!(
            html.contains(r#"<code class="path__id">&lt;x&gt;</code>"#),
            "escaped"
        );
        assert_eq!(
            html.matches("tree__row--warn").count(),
            1,
            "team is not declared"
        );
        assert!(html.contains("<strong>To check:</strong>"));
        assert!(html.contains(r#"<span class="badge badge--warn">1 to check</span>"#));
        assert!(
            html.contains("Not in the catalogue")
                && html.contains("x.read on service:gone/thing:a")
        );
        // One add form for each declared level, behind one disclosure.
        assert_eq!(html.matches(r#"action="/auth/users/grant-add""#).count(), 2);
        assert!(html.contains(
            r#"<code>service:infra-grafana/folder:&lt;id&gt;/dashboard:&lt;id&gt;</code>"#
        ));
        assert!(
            html.contains(r#"<span class="field__label">Id of the folder</span><input name="id0""#)
        );
        assert!(
            !html.contains("<option>access</option>"),
            "access has no resource"
        );
    }

    #[test]
    fn configured_grants_are_read_only_and_the_static_mode_has_no_form() {
        let cat = catalog();
        let mut bob = card("bob", &["mixed"]);
        bob.configured = access(&["notes"]);
        bob.grants.extend(access(&["notes"]));
        let configured = sub("dashboards.read on service:infra-grafana/folder:ops");
        bob.grants.push(configured.clone());
        bob.configured.push(configured);
        let mut v = user_view(&cat, bob);
        v.mode = Mode::Hybrid;
        let html = user_page(&frame(), &v);
        assert!(html.contains(r#"<input type="checkbox" checked disabled><span class="check__text"><code class="check__name">access</code> <span class="badge">configuration</span>"#));
        assert!(!html.contains("g.infra-notes.access"));
        assert!(html.contains(r#"<span class="badge tree__end">configuration</span>"#));
        assert!(!html.contains("grant-delete"));
        v.mode = Mode::Static;
        let html = user_page(&frame(), &v);
        assert!(!html.contains(r#"name="g."#) && !html.contains(r#"id="grants""#));
        assert!(!html.contains("grant-add") && !html.contains("Save access"));
        assert!(html.contains("cannot change them"));
    }

    #[test]
    fn the_superadmin_page_shows_no_grant_form_no_delete_and_no_log_in_as() {
        let cat = catalog();
        let mut root = card("root", &[]);
        root.superadmin = true;
        let html = user_page(&frame(), &user_view(&cat, root));
        assert!(html.contains("The superadmin has every grant"));
        for gone in [
            "/auth/users/update",
            "/auth/users/delete",
            "/auth/users/impersonate",
            "settings.js",
        ] {
            assert!(!html.contains(gone), "{gone}");
        }
    }

    #[test]
    fn what_comes_from_outside_is_escaped() {
        let cat = catalog();
        let mut user = card("<b>x</b>", &[]);
        user.activity.recent.push(SessionRow {
            started_at: 1_791_000_000,
            last_seen_at: 1_791_000_100,
            ended_at: None,
            end_reason: None,
            ip: Some("<script>1</script>".into()),
            user_agent: "\"><img src=x onerror=1>".into(),
            impersonated: true,
        });
        let html = user_page(&frame(), &user_view(&cat, user));
        assert!(!html.contains("<b>x</b>"));
        assert!(!html.contains("<script>1"));
        assert!(!html.contains("<img src=x"));
        assert!(html.contains("by the superadmin") && html.contains("Active"));
        assert!(html.contains(r#"id="pw-&lt;b&gt;x&lt;/b&gt;""#));
    }

    #[test]
    fn the_services_page_explains_a_grant_and_shows_actions_tree_and_holders() {
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
        assert!(html.contains("How a grant reads"));
        assert!(html.contains("Subtree match"));
        assert!(html.contains(r#"<li><code>dashboards.read</code><span>See the dashboards. <span class="muted">On: service, folder.</span></span></li>"#));
        assert!(html.contains(r#"<ul class="typeTree"><li><code>service</code><ul><li><code>folder</code> <span class="muted">A folder.</span><ul><li><code>dashboard</code></li></ul></li></ul></li></ul>"#));
        assert_eq!(
            html.matches("only: opening the service is the full permission")
                .count(),
            3
        );
        assert_eq!(
            html.matches(
                r#"<a class="badge badge--link" href="/auth/settings/users/alice">alice</a>"#
            )
            .count(),
            2
        );
        assert!(
            !html.contains(">bob</a>"),
            "an action without access does not open the service"
        );
        assert_eq!(
            html.matches("The superadmin only").count(),
            2,
            "Notes and Typo"
        );
        assert!(html.contains("gave the status &lt;500&gt;"));
        assert!(!html.contains("accounts-title"));
    }

    #[test]
    fn the_services_page_lists_the_grants_to_check_with_their_problem_and_account() {
        let cat = catalog();
        let actions = services();
        let holders = vec![
            (
                "alice".to_string(),
                vec![
                    sub("dashboards.read on service:infra-grafana/folder:ops"),
                    sub("dashboards.read on service:infra-grafana/folder:ops/dashboard:1"),
                    sub("dashboards.write on service:infra-grafana/dashboard:1"),
                ],
            ),
            (
                "bob".to_string(),
                vec![sub("x.read on service:infra-notes/a:b")],
            ),
        ];
        let html = services_page(&frame(), &services_view(&cat, &actions, &holders));
        assert!(html.contains("Grants to check"));
        assert!(html.contains(r#"<a href="/auth/settings/users/alice">alice</a>"#));
        assert!(
            html.contains("dashboards.read on service:infra-grafana/folder:ops/dashboard:1</code>")
        );
        assert!(html.contains("dashboards.write on service:infra-grafana/dashboard:1</code>"));
        assert!(html.contains("does not list the resource type"));
        assert!(
            !html.contains("folder:ops</code>"),
            "a declared grant is fine"
        );
        assert!(html.contains(r#"<a href="/auth/settings/users/bob">bob</a>"#));
        let mut unknown = services();
        unknown[1].known = false;
        let html = services_page(&frame(), &services_view(&cat, &unknown, &holders[1..]));
        assert!(
            !html.contains("Grants to check"),
            "a service that has not answered is not judged"
        );
    }

    #[test]
    fn the_activity_page_lists_attempts_changes_and_connections() {
        let page = LoginPage {
            rows: vec![
                login_row_of("alice", true, true),
                login_row_of("admin", false, false),
                login_row_of("", false, false),
            ],
            matching: 3,
            attempts_24h: 3,
            failed_24h: 2,
            addresses_24h: 1,
            ..LoginPage::default()
        };
        let events = vec![AuditRow {
            ts: 1_791_623_502,
            kind: "login_failed".into(),
            login: Some("alice".into()),
            actor: None,
            ip: None,
        }];
        let html = logins_page(&frame(), &logins(&page, false, &events));
        assert!(
            html.contains("2026-10-10 09:11:42"),
            "seconds tell attempts apart"
        );
        assert!(html.contains(
            r#"<code class="typed">admin</code> <span class="badge">no such account</span>"#
        ));
        assert!(html.contains(r#"<span class="badge badge--ok">Signed in</span>"#));
        assert_eq!(
            html.matches(r#"<span class="badge badge--danger">Refused</span>"#)
                .count(),
            2
        );
        assert!(html.contains(r#"<span class="muted">(empty)</span>"#));
        assert!(html.contains(r#"<span class="stat__value stat__value--danger">2</span>"#));
        assert!(html.contains("The password is never recorded"));
        assert!(html.contains("3 attempts"));
        assert!(!html.contains(r#"class="inbox""#));
        assert!(html.contains(r#"<li class="events__row events__row--danger"><span class="events__what">Refused sign-in <a href="/auth/settings/users/alice">alice</a></span>"#));
        assert!(html.contains("Recent changes") && html.contains("Connections"));
    }

    #[test]
    fn what_a_visitor_typed_cannot_break_out_of_the_activity_page() {
        let page = LoginPage {
            rows: vec![
                login_row_of(r#"</code><script>alert(1)</script>"#, false, false),
                login_row_of(r#""><img src=x onerror=1>"#, false, false),
            ],
            matching: 2,
            ..LoginPage::default()
        };
        let html = logins_page(&frame(), &logins(&page, false, &[]));
        assert!(!html.contains("<script>alert"));
        assert!(!html.contains("<img src=x"));
        assert!(html.contains("&lt;/code&gt;&lt;script&gt;alert(1)&lt;/script&gt;"));
    }

    #[test]
    fn the_activity_page_filters_and_pages_with_links_that_keep_the_filter() {
        let page = LoginPage {
            rows: vec![login_row_of("alice", false, true)],
            matching: 120,
            page: 1,
            ..LoginPage::default()
        };
        let html = logins_page(&frame(), &logins(&page, true, &[]));
        assert!(html.contains("Page 2 of 3"));
        assert!(html.contains(r#"rel="prev" href="/auth/settings/logins?r=failed""#));
        assert!(html.contains(r#"rel="next" href="/auth/settings/logins?p=3&amp;r=failed""#));
        assert!(html.contains(r#"<a class="segmented__option is-active" href="/auth/settings/logins?r=failed" aria-current="true">Refused</a>"#));
        assert!(
            html.contains(r#"<a class="segmented__option" href="/auth/settings/logins">All</a>"#)
        );
        assert!(html.contains("120 refused"));
        let last = LoginPage { page: 2, ..page };
        let html = logins_page(&frame(), &logins(&last, false, &[]));
        assert!(!html.contains(r#"rel="next""#));
        assert!(html.contains(r#"rel="prev" href="/auth/settings/logins?p=2""#));
    }

    #[test]
    fn a_graph_is_four_nodes_says_its_peak_and_only_the_newest_bar_wears_the_accent() {
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
        assert_eq!(svg.matches("<path").count(), 3);
        assert_eq!(svg.matches("spark__bar--today").count(), 1);
        assert!(svg.contains("M12 2h3v30h-3z"), "{svg}");
        assert!(svg.contains("M116 20h3v12h-3z"), "{svg}");
        assert!(!svg.contains("style=") && !svg.contains("<rect") && !svg.contains("<title>"));
        assert!(svg.contains("7 in total"));
        assert_eq!(
            spark(&vec![DayStat::default(); 30], false)
                .matches("<path")
                .count(),
            1
        );
    }

    #[test]
    fn notices_come_from_a_fixed_list() {
        assert!(notice_for("user-added").unwrap().ok);
        assert!(!notice_for("exists").unwrap().ok);
        assert!(notice_for("<script>").is_none());
        for code in [
            "grant-added",
            "grant-deleted",
            "grant-bad-id",
            "grant-undeclared",
            "grant-not-known",
            "grant-limit",
            "no-grant",
            "password-reset",
            "user-updated",
        ] {
            assert!(notice_for(code).is_some(), "{code}");
        }
    }
}
