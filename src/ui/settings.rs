//! The settings of the superadmin. Each tab is one HTML page: Users, Services and Activity. Each
//! account also has a page of its own, below the Users tab. The pages follow the manner of
//! clickhouse-dash: a header, a row of underlined tab links, then parts with a title and a count.
//!
//! The pages explain the access model with their structure:
//!
//! - The Users page is a table of the accounts. Each row gives a summary of the access of the
//!   account and links to the page of the account.
//! - The page of an account lists the services that it has a grant for, one line each, and one
//!   control that gives access to another service. A new account sees no service.
//! - The editor of one (account, service) pair has its own Save. It has the box `access`, the
//!   actions on the whole service and the grants on resources, written as sentences. A grant on a
//!   resource has two steps: the level, then the ids of that chain and the action.
//! - The add-user form gives access to services at once, with the same cards as the page of an account.
//! - The Services page shows how a grant reads, the grants to check, and the actions and the tree
//!   of resource types of each service. The page of one service shows who has access to it.
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
use crate::grants::{covers, holds, Grant, Mode, ACCESS};
use crate::store::{
    AccountActivity, AuditRow, DayStat, LoginPage, LoginRow, ServiceUse, SessionRow,
};
use crate::util::{
    format_duration, format_utc, format_utc_secs, html_escape, percent_encode_query_component,
    short_agent, TYPED_LOGIN_MAX,
};

pub const USERS_PATH: &str = "/auth/settings";
/// The page of one account is this prefix, then the login.
pub const USER_PATH_PREFIX: &str = "/auth/settings/users/";
pub const SERVICES_PATH: &str = "/auth/settings/services";
/// The page of one service is this prefix, then the id of the service.
pub const SERVICE_PATH_PREFIX: &str = "/auth/settings/services/";
/// The editor of the access of one account to one service. The query names `user` and `service`.
pub const ACCESS_PATH: &str = "/auth/settings/access";
pub const LOGINS_PATH: &str = "/auth/settings/logins";

/// The largest number of ids that a field of a new grant suggests.
const MAX_SUGGESTED_IDS: usize = 50;

/// The path of the page of one account. A login has only `a-z`, `0-9`, `.`, `-` and `_`, so the
/// path needs no encoding. The caller escapes the path for HTML all the same.
pub fn user_path(login: &str) -> String {
    format!("{USER_PATH_PREFIX}{login}")
}

/// The path of the page of one service. The id is encoded: the catalogue does not limit it to
/// ASCII.
pub fn service_path(service: &str) -> String {
    format!(
        "{SERVICE_PATH_PREFIX}{}",
        percent_encode_query_component(service)
    )
}

/// The address of the editor of the access of `login` to `service`. The caller escapes it for HTML.
pub fn access_path(login: &str, service: &str) -> String {
    format!(
        "{ACCESS_PATH}?user={}&service={}",
        percent_encode_query_component(login),
        percent_encode_query_component(service)
    )
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
            "Account added. It has no grant yet: it sees no service. Give it access below.",
            true,
            false,
        ),
        "user-added-access" => (
            "Account added with its access. Open a line below to set its actions and its resources.",
            true,
            false,
        ),
        "user-updated" => (
            "Access saved. It applies from the next request.",
            true,
            false,
        ),
        "add-unknown-service" => (
            "A box of the form is not offered any more. Open the form again.",
            false,
            true,
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
        body.push_str(&script_tag());
    }
    let icons: Vec<&str> = ICONS
        .iter()
        .copied()
        .filter(|id| body.contains(&format!("#{id}\"")))
        .collect();
    document(
        &format!("Settings: {title}"),
        &format!(
            r#"<link rel="stylesheet" href="/auth/settings.css?v={}">"#,
            asset_version()
        ),
        &icons,
        &body,
    )
}

/// The version of the settings stylesheet and script: a hash of their content. The server lets a
/// browser keep these files for an hour, so each address carries the version. A new release then
/// loads at once, and a browser never runs an old script on a new page.
fn asset_version() -> &'static str {
    static VERSION: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    VERSION.get_or_init(|| {
        // FNV-1a over the two files.
        let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
        for byte in super::SETTINGS_CSS
            .bytes()
            .chain(super::SETTINGS_JS.bytes())
        {
            hash ^= u64::from(byte);
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
        format!("{hash:016x}")[..10].to_string()
    })
}

/// The script of the settings pages, with its version.
fn script_tag() -> String {
    format!(
        r#"<script src="/auth/assets/settings.js?v={}" defer></script>"#,
        asset_version()
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
#[derive(Clone)]
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
    if add_open {
        // The add form is the whole page: the other accounts are not shown while it is open.
        out.push_str(r#"<section class="part" aria-labelledby="accounts-title">"#);
        part_head(&mut out, "accounts-title", "New user", "", "");
        add_form(&mut out, view);
        out.push_str("</section>");
        return page(Tab::Users, frame, "New user", &out, false);
    }
    out.push_str(r#"<section class="part" aria-labelledby="accounts-title">"#);
    let controls = format!(
        r##"<a class="button button--primary" href="{USERS_PATH}?add=1#add-user">Add a user</a>"##
    );
    part_head(
        &mut out,
        "accounts-title",
        "Accounts",
        &format!("{} of {}", view.users.len(), view.max_users),
        &controls,
    );
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
            r#"<p class="alertLine"><strong>{} in the last 30 days</strong>{unknown}. <a href="{LOGINS_PATH}?r=failed">See the refused attempts</a></p>"#,
            plural(failed as usize, "refused sign-in", "refused sign-ins"),
        );
    }
    out.push_str(r#"<div class="tableBox"><table class="table accounts"><thead><tr><th scope="col">Account</th><th scope="col">Access</th><th scope="col">Last sign-in (UTC)</th><th scope="col" class="num">Sessions, 30 d</th><th scope="col" class="num">Refused, 30 d</th></tr></thead><tbody>"#);
    account_row(&mut out, view, &view.superadmin);
    for user in &view.users {
        account_row(&mut out, view, user);
    }
    out.push_str("</tbody></table></div></section>");
    incident_part(&mut out, view.token);
    page(Tab::Users, frame, "Users", &out, false)
}

/// The access of an account in one line of muted text: what it opens, its actions on whole
/// services and its grants on resources.
fn summary_text(user: &UserCard, s: &Summary) -> String {
    let actions = user
        .grants
        .iter()
        .filter(|g| g.action != ACCESS && g.path.is_empty())
        .count();
    let mut parts = vec![format!(
        "<strong>{}</strong>",
        plural(s.services, "service", "services")
    )];
    if actions > 0 {
        parts.push(plural(actions, "action", "actions"));
    }
    if s.below > 0 {
        parts.push(format!(
            "{} on resources",
            plural(s.below, "grant", "grants")
        ));
    }
    parts.join(", ")
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
        if user.grants.is_empty() {
            out.push_str(r#"<span class="muted">No access</span>"#);
        } else {
            let _ = write!(
                out,
                r#"<span class="access">{}</span>"#,
                summary_text(user, &s)
            );
            if s.to_check > 0 {
                let _ = write!(
                    out,
                    r#" <span class="badge badge--warn">{} to check</span>"#,
                    s.to_check
                );
            }
        }
    }
    let last = match user.activity.last_login {
        Some(at) => format_utc(at),
        None => r#"<span class="muted">Never</span>"#.to_string(),
    };
    let failed = user.activity.failed_30d;
    let _ = write!(
        out,
        r#"</td><td data-label="Last sign-in (UTC)">{last}</td><td class="num" data-label="Sessions, 30 d">{}</td><td class="num{}" data-label="Refused, 30 d">{failed}</td></tr>"#,
        user.activity.sessions_30d,
        if failed > 0 { " num--danger" } else { "" },
    );
}

/// The incident tool. One click opens the confirmation. A second click ends every session, the
/// session of the superadmin included.
fn incident_part(out: &mut String, token: &str) {
    let _ = write!(
        out,
        r#"<section class="dangerRow" aria-labelledby="incident-title"><div><h2 class="dangerRow__title" id="incident-title">End all sessions</h2><p class="hint">For an incident. Every account must sign in again, yours included. Accounts, grants and history stay.</p></div><details class="confirm"><summary class="button button--danger">End all sessions</summary><form class="confirm__panel" method="post" action="/auth/users/end-sessions"><input type="hidden" name="csrf" value="{}"><input type="hidden" name="confirm" value="end-all-sessions"><p>You will be signed out too.</p><button class="button button--dangerFill" type="submit">Confirm: end all sessions</button></form></details></section>"#,
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
    // refused attempt. The script of the generator loads with the form only. The login and the
    // password are on the top row. Under them, the permissions are cards in columns, by group.
    // One request makes the account and its grants.
    let access = view.mode.editable() && !view.services.is_empty();
    let _ = write!(
        out,
        r#"<form class="addUser" id="add-user" method="post" action="/auth/users/add"><input type="hidden" name="csrf" value="{}"><div class="addUser__fields"><div class="field"><label class="field__label" for="new-login">Login</label><input id="new-login" name="login" type="text" maxlength="32" pattern="[A-Za-z0-9._\-]{{1,32}}" required autocomplete="off" autocapitalize="off" spellcheck="false" title="1 to 32 characters: letters, digits, dot, dash, underscore"></div>"#,
        html_escape(view.token),
    );
    password_field(out, "new-password", "Password", true, "");
    out.push_str("</div>");
    if access {
        out.push_str(r#"<fieldset class="permForm__perms"><legend class="field__label">Permissions</legend>"#);
        permission_cards(
            out,
            view.catalog,
            &view.services,
            &[],
            &[],
            true,
            None,
            None,
        );
        out.push_str("</fieldset>");
    }
    let _ = write!(
        out,
        r#"<div class="formActions"><button class="button button--primary" type="submit">Add user</button><a class="button button--ghost" href="{USERS_PATH}">Cancel</a></div></form>{}"#,
        script_tag()
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
    let mut out = String::new();
    user_head(&mut out, view);
    notice_html(&mut out, view.notice);
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

/// The head of the page of an account: the way back to the list, the login, its badges, one line
/// of facts and "Log in as".
fn user_head(out: &mut String, view: &UserView) {
    let user = &view.user;
    let activity = &user.activity;
    let login = html_escape(&user.login);
    let _ = write!(
        out,
        r##"<header class="userHead"><div class="userHead__main"><nav class="crumbs" aria-label="Breadcrumb"><a href="{USERS_PATH}">Users</a><svg class="icon" aria-hidden="true"><use href="#i-chevron-right"/></svg></nav><h2 class="userHead__name" aria-current="page">{login}</h2>"##
    );
    if user.superadmin {
        out.push_str(r#"<span class="badge">Superadmin</span>"#);
    } else {
        let s = summary(view.catalog, &view.services, user);
        if s.to_check > 0 {
            let _ = write!(
                out,
                r##"<a class="badge badge--warn" href="#access-title">{} to check</a>"##,
                plural(s.to_check, "grant", "grants")
            );
        }
    }
    if !user.superadmin {
        let _ = write!(
            out,
            r#"<form class="userHead__action" method="post" action="/auth/users/impersonate"><input type="hidden" name="csrf" value="{}"><input type="hidden" name="login" value="{login}"><button class="button" type="submit">Log in as {login}</button></form>"#,
            html_escape(view.token),
        );
    }
    let last = activity
        .last_login
        .map(format_utc)
        .unwrap_or_else(|| "never".to_string());
    let created = user
        .created_at
        .map(format_utc)
        .unwrap_or_else(|| "in the configuration".to_string());
    let _ = write!(
        out,
        r#"</div><dl class="facts"><div><dt>Created</dt><dd>{created}</dd></div><div><dt>Last sign-in</dt><dd>{last}</dd></div><div><dt>Sessions, 30 d</dt><dd>{}</dd></div><div><dt>Refused, 30 d</dt><dd{}>{}</dd></div><div><dt>Times</dt><dd>UTC</dd></div></dl></header>"#,
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

/// The address of the editor of the access of `login` to `service`, ready for an HTML attribute.
fn access_href(login: &str, service: &str) -> String {
    html_escape(&access_path(login, service))
}

/// The services of the catalogue by group, in the order of the catalogue.
fn by_group<'a>(
    catalog: &Catalog,
    services: impl IntoIterator<Item = &'a ServiceActions>,
) -> Vec<(String, Vec<&'a ServiceActions>)> {
    let mut groups: Vec<(String, Vec<&ServiceActions>)> = Vec::new();
    for service in services {
        let group = group_of(catalog, &service.id).unwrap_or("Other");
        match groups.iter_mut().find(|(name, _)| name == group) {
            Some((_, list)) => list.push(service),
            None => groups.push((group.to_string(), vec![service])),
        }
    }
    groups
}

/// The access of one account, as the permissions of a key in clickhouse-dash: one card for each
/// service, by group. The head of a card has the box `access`, the name and a count. The card
/// opens on the actions of the service and the grants on resources. A card with a grant starts
/// open. One form and one Save send every box of the page.
fn access_part(out: &mut String, view: &UserView) {
    let user = &view.user;
    let editable = view.mode.editable();
    let opened = summary(view.catalog, &view.services, user).services;
    out.push_str(r#"<section class="part" aria-labelledby="access-title">"#);
    let controls = if editable && !view.services.is_empty() {
        r#"<button class="button button--primary" type="submit" form="grants">Save access</button>"#
    } else {
        ""
    };
    part_head(
        out,
        "access-title",
        "Access",
        &format!("Opens {opened} of {}", view.services.len()),
        controls,
    );
    match view.mode {
        Mode::Static => out.push_str(r#"<p class="alertLine alertLine--info">The grants come from the configuration (<code>user</code> blocks, <code>access.grants_mode = "static"</code>). This page shows them and cannot change them.</p>"#),
        Mode::Hybrid if !user.configured.is_empty() => out.push_str(r#"<p class="alertLine alertLine--info">Some grants come from the configuration (<code>user</code> block). They show the mark <em>configuration</em> and cannot change here.</p>"#),
        _ => {}
    }
    if editable {
        let _ = write!(
            out,
            r#"<form id="grants" method="post" action="/auth/users/update"><input type="hidden" name="csrf" value="{}"><input type="hidden" name="login" value="{}"><input type="hidden" name="grants" value="1"></form>"#,
            html_escape(view.token),
            html_escape(&user.login),
        );
    }
    if view.services.is_empty() {
        out.push_str(r#"<p class="empty">No service in the catalogue yet. A service appears when a container or an IngressRoute has <code>portal.*</code> labels.</p>"#);
    } else {
        let holder = Holder {
            login: &user.login,
            token: view.token,
            grants: &user.grants,
            configured: &user.configured,
            editable,
        };
        permission_cards(
            out,
            view.catalog,
            &view.services,
            &user.grants,
            &user.configured,
            editable,
            Some("grants"),
            Some(&holder),
        );
    }
    // The services that left the catalogue but still have grants of this account.
    let gone: Vec<&Grant> = user
        .grants
        .iter()
        .filter(|g| !view.services.iter().any(|s| s.id == g.service))
        .collect();
    if !gone.is_empty() {
        out.push_str(r#"<h3 class="subTitle">Not in the catalogue</h3><p class="hint">These services are not in the catalogue now. Their grants stay and apply when the service comes back.</p><ul class="grantRows">"#);
        for grant in gone {
            let _ = write!(
                out,
                r#"<li class="grantRow"><code>{}</code></li>"#,
                html_escape(&grant.to_string())
            );
        }
        out.push_str("</ul>");
    }
    if editable && !view.services.is_empty() {
        out.push_str(r#"<div class="formActions"><button class="button button--primary" type="submit" form="grants">Save access</button></div>"#);
    }
    out.push_str("</section>");
}

/// The account that a list of cards shows: who it is and what it holds.
struct Holder<'a> {
    login: &'a str,
    token: &'a str,
    grants: &'a [Grant],
    configured: &'a [Grant],
    editable: bool,
}

/// The first sentence of a description, for the line under an action.
fn first_sentence(text: &str) -> &str {
    match text.find(". ") {
        Some(end) => &text[..=end],
        None => text,
    }
}

/// What one service shows in the permissions: the line of the list and the panel.
struct Card {
    /// The line in the list: the box `access`, the name and the count.
    row: String,
    /// The panel at the right: the actions of the service and of each named resource, then the
    /// grants on resources. `None`: the service has nothing to open, so its line is all it has.
    panel: Option<String>,
    /// The account holds something on this service: its line shows it, and it opens first.
    has_grants: bool,
}

/// The permissions as a list of services and a panel, by group of the catalogue. The list is at
/// the left: one line for each service, with its box `access`. The panel at the right shows the
/// service that the list selects: its actions on the whole service and, side by side, the actions
/// of each resource that it names. A click on a line selects its panel without a new page: all the
/// panels are in the page, so no box is lost. `grants` ticks the boxes. A box that belongs to the
/// form of another element names it in `form`. `holder` adds the grants on resources of the
/// account to each panel: only a page that edits one account passes it.
#[allow(clippy::too_many_arguments)]
fn permission_cards(
    out: &mut String,
    catalog: &Catalog,
    services: &[ServiceActions],
    grants: &[Grant],
    configured: &[Grant],
    editable: bool,
    form: Option<&str>,
    holder: Option<&Holder>,
) {
    let groups = by_group(catalog, services);
    let cards: Vec<(String, Vec<(&ServiceActions, Card)>)> = groups
        .into_iter()
        .map(|(group, members)| {
            let cards = members
                .into_iter()
                .map(|service| {
                    (
                        service,
                        service_card(service, grants, configured, editable, form, holder),
                    )
                })
                .collect();
            (group, cards)
        })
        .collect();
    // The panel that shows first: the first service with a grant, else the first with a panel.
    let shown = cards
        .iter()
        .flat_map(|(_, members)| members.iter())
        .filter(|(_, card)| card.panel.is_some())
        .find(|(_, card)| card.has_grants)
        .or_else(|| {
            cards
                .iter()
                .flat_map(|(_, members)| members.iter())
                .find(|(_, card)| card.panel.is_some())
        })
        .map(|(service, _)| service.id.clone());
    out.push_str(
        r#"<div class="perms"><div class="perms__list" role="group" aria-label="Services">"#,
    );
    for (group, members) in &cards {
        let _ = write!(
            out,
            r#"<h3 class="perms__group">{}</h3>"#,
            html_escape(group)
        );
        for (service, card) in members {
            let selected = shown.as_deref() == Some(service.id.as_str());
            let _ = write!(
                out,
                r#"<div class="perm__row{}{}" data-svc="{}">{}</div>"#,
                if card.has_grants { " has-grants" } else { "" },
                if selected { " is-selected" } else { "" },
                html_escape(&service.id),
                card.row,
            );
        }
    }
    out.push_str(r#"</div><div class="perms__panels">"#);
    for (_, members) in &cards {
        for (service, card) in members {
            let Some(panel) = &card.panel else {
                continue;
            };
            let selected = shown.as_deref() == Some(service.id.as_str());
            let _ = write!(
                out,
                r#"<section class="perm__panel{}" id="panel-{}" data-svc="{}" aria-label="{}">{}</section>"#,
                if selected { " is-selected" } else { "" },
                html_escape(&service.id),
                html_escape(&service.id),
                html_escape(&service.name),
                panel,
            );
        }
    }
    out.push_str("</div></div>");
}

/// The line and the panel of one service. The boxes of the panel are locked and unticked while
/// the box `access` is not ticked: an action needs the open service. The script unlocks them.
fn service_card(
    service: &ServiceActions,
    grants: &[Grant],
    configured: &[Grant],
    editable: bool,
    form: Option<&str>,
    holder: Option<&Holder>,
) -> Card {
    let opens = holds(grants, ACCESS, &service.id);
    let below: Vec<&Grant> = grants
        .iter()
        .filter(|g| g.service == service.id && !g.path.is_empty())
        .collect();
    let access_fixed = !editable || holds(configured, ACCESS, &service.id);
    // A service with no action and no level has nothing to open: its line is all it has.
    let flat = service.actions.is_empty()
        && service.document.resources.is_empty()
        && below.is_empty()
        && service.levels().is_empty();
    let locked = !opens;
    // The boxes of the service: how many there are, and how many are ticked.
    let mut total = 1;
    let mut ticked = usize::from(opens);
    let mut cols = String::new();
    let nodes = &service.document.resources;
    if !service.actions.is_empty() {
        cols.push_str(r#"<div class="perm__col">"#);
        if !nodes.is_empty() {
            cols.push_str(r#"<h4 class="perm__colhead">Whole service</h4>"#);
        }
        cols.push_str(r#"<ul class="perm__actions">"#);
        for (action, description) in &service.actions {
            let held = opens && holds(grants, action, &service.id);
            let fixed = !editable || holds(configured, action, &service.id);
            total += 1;
            ticked += usize::from(held);
            let _ = write!(
                cols,
                r#"<li><label class="perm__action" title="{}">{}<code>{}</code><span class="perm__tip">{}</span></label></li>"#,
                html_escape(description),
                grant_box(&service.id, action, held, fixed, locked, form),
                html_escape(action),
                html_escape(first_sentence(description)),
            );
        }
        cols.push_str("</ul></div>");
    }
    // The resources that the service names, each one in a column of its own, with the actions
    // that apply on it only. A grant on such a resource is a box. Any other grant below the
    // service is a sentence.
    let mut boxed: Vec<Grant> = Vec::new();
    for node in nodes {
        let kind = node
            .path
            .last()
            .map(|s| s.kind.as_str())
            .unwrap_or_default();
        let id = node.path.last().map(|s| s.id.as_str()).unwrap_or_default();
        let actions = service.document.actions_on(node);
        let grants_here: Vec<Grant> = actions
            .iter()
            .filter_map(|a| Grant::on_path(&a.name, &service.id, node.path.clone()).ok())
            .collect();
        // A box shows ticked when the grant is held, or when a grant above it covers it: the
        // action on the whole service, or on a resource above.
        let ticked_here = if opens {
            grants_here.iter().filter(|g| covers_any(grants, g)).count()
        } else {
            0
        };
        total += grants_here.len();
        ticked += ticked_here;
        let _ = write!(
            cols,
            r#"<div class="perm__col perm__node perm__node--d{}"><h4 class="perm__colhead"><span class="path__type">{}</span> <strong>{}</strong>{}<span class="perm__count"><span class="perm__n">{}/{}</span></span></h4><ul class="perm__actions">"#,
            node.path.len().min(4),
            html_escape(kind),
            html_escape(&node.label),
            if node.label == id {
                String::new()
            } else {
                format!(r#" <code class="svcRow__id">{}</code>"#, html_escape(id))
            },
            ticked_here,
            grants_here.len(),
        );
        for (action, grant) in actions.iter().zip(&grants_here) {
            let fixed = !editable || covers_any(configured, grant);
            let _ = write!(
                cols,
                r#"<li><label class="perm__action" title="{}">{}<code>{}</code><span class="perm__tip">{}</span></label></li>"#,
                html_escape(&action.description),
                resource_box(
                    grant,
                    opens && covers_any(grants, grant),
                    fixed,
                    locked,
                    form
                ),
                html_escape(&action.name),
                html_escape(first_sentence(&action.description)),
            );
        }
        boxed.extend(grants_here);
        cols.push_str("</ul></div>");
    }
    let below_rest: Vec<&Grant> = below
        .iter()
        .copied()
        .filter(|g| !boxed.contains(g))
        .collect();
    let mut res = String::new();
    if let Some(holder) = holder {
        if !below_rest.is_empty() {
            let _ = write!(
                res,
                r#"<ul class="grantRows" aria-label="Grants on resources of {}">"#,
                html_escape(&service.name)
            );
            for grant in &below_rest {
                resource_grant(&mut res, holder, service, grant);
            }
            res.push_str("</ul>");
        }
        if holder.editable && !service.levels().is_empty() {
            let _ = write!(
                res,
                r#"<a class="linkButton" href="{}">Add a grant on a resource</a>"#,
                access_href(holder.login, &service.id)
            );
        }
    }
    let count = format!(
        r#"<span class="perm__count"><span class="perm__n">{ticked}/{total}</span>{}</span>"#,
        if below_rest.is_empty() {
            String::new()
        } else {
            format!(", {} on resources", below_rest.len())
        },
    );
    let to_check = below.iter().filter(|g| service.undeclared(g)).count();
    let marks = format!(
        "{}{}",
        if access_fixed && editable && opens {
            r#"<span class="badge">configuration</span>"#.to_string()
        } else {
            String::new()
        },
        if to_check > 0 {
            format!(r#"<span class="badge badge--warn">{to_check} to check</span>"#)
        } else {
            String::new()
        },
    );
    let name = html_escape(&service.name);
    let id = html_escape(&service.id);
    let bx = grant_box(&service.id, ACCESS, opens, access_fixed, false, form);
    let bx_named = bx.replacen("<input", &format!(r#"<input aria-label="Open {name}""#), 1);
    let row = if flat {
        format!(
            r#"<label class="perm__link perm__link--flat">{bx}<span class="perm__name">{name}</span><code class="svcRow__id">{id}</code>{marks}</label>"#
        )
    } else {
        format!(
            r##"{bx_named}<a class="perm__link" href="#panel-{id}"><span class="perm__name">{name}</span><code class="svcRow__id">{id}</code>{marks}{count}<svg class="icon perm__chevron" aria-hidden="true"><use href="#i-chevron-right"/></svg></a>"##
        )
    };
    let panel = (!flat).then(|| {
        format!(
            r#"<header class="perm__panelhead"><strong>{name}</strong><code class="svcRow__id">{id}</code>{marks}{count}</header><div class="perm__cols">{cols}</div>{}"#,
            if res.is_empty() {
                String::new()
            } else {
                format!(r#"<div class="perm__res">{res}</div>"#)
            },
        )
    });
    Card {
        row,
        panel,
        has_grants: ticked > 0 || !below.is_empty(),
    }
}

/// What a set of grants gives on one service, in one line: open or not, the actions on the whole
/// service, then the number of grants on resources.
fn access_line(service: &ServiceActions, grants: &[Grant]) -> String {
    let mine: Vec<&Grant> = grants.iter().filter(|g| g.service == service.id).collect();
    let mut parts: Vec<String> = vec![if mine
        .iter()
        .any(|g| g.action == ACCESS && g.path.is_empty())
    {
        r#"<strong>open</strong>"#.to_string()
    } else {
        r#"<span class="muted">not open</span>"#.to_string()
    }];
    let actions: Vec<String> = mine
        .iter()
        .filter(|g| g.action != ACCESS && g.path.is_empty())
        .map(|g| format!("<code>{}</code>", html_escape(&g.action)))
        .collect();
    if !actions.is_empty() {
        parts.push(actions.join(" "));
    }
    let below: Vec<&&Grant> = mine.iter().filter(|g| !g.path.is_empty()).collect();
    if !below.is_empty() {
        let mut text = plural(below.len(), "resource grant", "resource grants");
        let to_check = below.iter().filter(|g| service.undeclared(g)).count();
        if to_check > 0 {
            let _ = write!(
                text,
                r#" <span class="badge badge--warn">{to_check} to check</span>"#
            );
        }
        parts.push(text);
    }
    parts.join(r#" <span class="muted" aria-hidden="true">·</span> "#)
}

/// The path of a grant for a human: each segment as its type and its decoded id, with `›` between
/// the segments.
fn path_html(grant: &Grant) -> String {
    let mut out = String::from(r#"<span class="path">"#);
    for (index, segment) in grant.path.iter().enumerate() {
        if index > 0 {
            out.push_str(r#"<span class="path__sep" aria-hidden="true">›</span>"#);
        }
        let _ = write!(
            out,
            r#"<span class="path__type">{}</span> <code class="path__id">{}</code>"#,
            html_escape(&segment.kind),
            html_escape(&segment.id)
        );
    }
    out.push_str("</span>");
    out
}

/// A grant that a broader grant of the same account already covers: the same action and service,
/// on the whole service or on a node above. The grant adds nothing.
fn covered_by<'a>(grants: &'a [Grant], grant: &Grant) -> Option<&'a Grant> {
    grants.iter().find(|other| {
        *other != grant
            && other.action == grant.action
            && other.service == grant.service
            && grant.path.starts_with(&other.path)
    })
}

/// The words of a broader grant, for the mark "covered".
fn reach_text(grant: &Grant) -> String {
    if grant.path.is_empty() {
        "the action on the whole service".to_string()
    } else {
        format!(
            "{} on {}",
            html_escape(&grant.action),
            html_escape(&grant.path_text())
        )
    }
}

// The editor of the access of one account to one service --------------------------------------

/// The data of the editor of one (account, service) pair.
pub struct AccessView<'a> {
    pub notice: Option<Notice>,
    pub mode: Mode,
    pub service: ServiceActions,
    pub user: UserCard,
    /// The admin came from "Give access": the box `access` starts ticked.
    pub give: bool,
    /// The level of step 1 of a new grant on a resource: the name of the type.
    pub level: Option<String>,
    /// The grants of every account on this service. They suggest the ids of step 2.
    pub known: Vec<Grant>,
    /// The anti-CSRF token of the administration forms.
    pub token: &'a str,
}

pub fn access_page(frame: &Frame, view: &AccessView) -> String {
    let user = &view.user;
    let service = &view.service;
    let login = html_escape(&user.login);
    let mut out = String::new();
    let _ = write!(
        out,
        r##"<header class="userHead"><div class="userHead__main"><nav class="crumbs" aria-label="Breadcrumb"><a href="{USERS_PATH}">Users</a><svg class="icon" aria-hidden="true"><use href="#i-chevron-right"/></svg><a href="{}">{login}</a><svg class="icon" aria-hidden="true"><use href="#i-chevron-right"/></svg></nav><h2 class="userHead__name" aria-current="page">{}</h2><code class="svcRow__id">{}</code></div></header>"##,
        html_escape(&user_path(&user.login)),
        html_escape(&service.name),
        html_escape(&service.id),
    );
    notice_html(&mut out, view.notice);
    access_form(&mut out, view);
    resource_part(&mut out, view);
    let title = format!("{} · {}", user.login, service.name);
    page(Tab::Users, frame, &title, &out, false)
}

/// The box `access` and the actions on the whole service. One form, one Save button. The form
/// names its service, so the server changes no other service of the account.
fn access_form(out: &mut String, view: &AccessView) {
    let user = &view.user;
    let service = &view.service;
    let editable = view.mode.editable();
    let opens = holds(&user.grants, ACCESS, &service.id);
    let ticked = opens || (view.give && editable);
    out.push_str(r#"<section class="part" aria-labelledby="whole-title">"#);
    part_head(out, "whole-title", "On the whole service", "", "");
    if editable {
        let _ = write!(
            out,
            r#"<form class="editor" method="post" action="/auth/users/update"><input type="hidden" name="csrf" value="{}"><input type="hidden" name="login" value="{}"><input type="hidden" name="grants" value="1"><input type="hidden" name="only" value="{}">"#,
            html_escape(view.token),
            html_escape(&user.login),
            html_escape(&service.id),
        );
    }
    out.push_str(r#"<ul class="options">"#);
    let mut rows: Vec<(&str, &str)> = vec![(
        ACCESS,
        "Open the service. The account sees it on the panel and the gate lets it in.",
    )];
    rows.extend(
        service
            .actions
            .iter()
            .map(|(name, text)| (name.as_str(), text.as_str())),
    );
    for (action, description) in rows {
        let held = holds(&user.grants, action, &service.id);
        let configured = holds(&user.configured, action, &service.id);
        let fixed = !editable || configured;
        let checked = held || (action == ACCESS && ticked);
        let _ = write!(
            out,
            r#"<li><label class="option{}">{}<span class="option__name"><code>{}</code></span><span class="option__text">{}</span>{}</label></li>"#,
            if checked { " is-on" } else { "" },
            grant_box(&service.id, action, checked, fixed, false, None),
            html_escape(action),
            html_escape(description),
            if configured && editable {
                r#"<span class="badge">configuration</span>"#
            } else {
                ""
            },
        );
    }
    out.push_str("</ul>");
    if service.actions.is_empty() && service.known {
        out.push_str(r#"<p class="hint">This service publishes no other action: <code>access</code> is the full permission.</p>"#);
    } else if !service.known {
        out.push_str(r#"<p class="hint">The service has not given its actions yet. Open the page again in a moment.</p>"#);
    }
    if view.give && editable && !opens {
        let _ = write!(
            out,
            r#"<p class="hint">{} does not have this access yet. Save to give it.</p>"#,
            html_escape(&user.login)
        );
    }
    if editable {
        let _ = write!(
            out,
            r#"<div class="formActions"><button class="button button--primary" type="submit">Save</button><a class="button button--ghost" href="{}">Back to {}</a></div></form>"#,
            html_escape(&user_path(&user.login)),
            html_escape(&user.login)
        );
    } else {
        let _ = write!(
            out,
            r#"<p class="alertLine alertLine--info">The grants come from the configuration (<code>access.grants_mode = "static"</code>). This page shows them and cannot change them.</p><p><a class="button button--ghost" href="{}">Back to {}</a></p>"#,
            html_escape(&user_path(&user.login)),
            html_escape(&user.login)
        );
    }
    out.push_str("</section>");
}

/// One check box. A fixed box (configuration, or the mode `static`) has no name, so the form does
/// not send it. `form` names the form that the box belongs to, when the box is not inside it.
fn grant_box(
    service: &str,
    action: &str,
    checked: bool,
    fixed: bool,
    locked: bool,
    form: Option<&str>,
) -> String {
    format!(
        r#"<input type="checkbox"{}{}{}>"#,
        if fixed {
            String::new()
        } else {
            format!(
                r#"{} name="g.{}.{}" value="1" data-action="{}" data-path="""#,
                form.map(|id| format!(r#" form="{id}""#))
                    .unwrap_or_default(),
                html_escape(service),
                html_escape(action),
                html_escape(action),
            )
        },
        if checked { " checked" } else { "" },
        if fixed || locked { " disabled" } else { "" },
    )
}

/// Whether one of `held` covers `grant`: the same action and service, on the node or above it.
fn covers_any(held: &[Grant], grant: &Grant) -> bool {
    covers(held, &grant.action, &grant.service, &grant.path)
}

/// One check box of a grant on a resource that the service names. The name is `r.` and the
/// grant in its text form.
fn resource_box(
    grant: &Grant,
    checked: bool,
    fixed: bool,
    locked: bool,
    form: Option<&str>,
) -> String {
    format!(
        r#"<input type="checkbox"{}{}{}>"#,
        if fixed {
            String::new()
        } else {
            format!(
                r#"{} name="r.{}" value="1" data-action="{}" data-path="{}""#,
                form.map(|id| format!(r#" form="{id}""#))
                    .unwrap_or_default(),
                html_escape(&grant.to_string()),
                html_escape(&grant.action),
                html_escape(&grant.path_text()),
            )
        },
        if checked { " checked" } else { "" },
        if fixed || locked { " disabled" } else { "" },
    )
}

/// The grants on resources of the service: each one as a sentence, then the two steps that add one.
fn resource_part(out: &mut String, view: &AccessView) {
    let user = &view.user;
    let service = &view.service;
    let editable = view.mode.editable();
    let below: Vec<&Grant> = user
        .grants
        .iter()
        .filter(|g| g.service == service.id && !g.path.is_empty())
        .collect();
    let levels = if editable {
        service.levels()
    } else {
        Vec::new()
    };
    if below.is_empty() && levels.is_empty() {
        return;
    }
    out.push_str(r#"<section class="part" aria-labelledby="resources-title">"#);
    part_head(
        out,
        "resources-title",
        "On one resource",
        &below.len().to_string(),
        "",
    );
    out.push_str(r#"<p class="part__lede">A grant on a resource applies to that resource and to everything below it.</p>"#);
    if below.is_empty() {
        out.push_str(r#"<p class="empty">No grant on a resource.</p>"#);
    } else {
        let _ = write!(
            out,
            r#"<ul class="grantRows" aria-label="Grants on resources of {}">"#,
            html_escape(&service.name)
        );
        let holder = Holder {
            login: &user.login,
            token: view.token,
            grants: &user.grants,
            configured: &user.configured,
            editable,
        };
        for grant in below {
            resource_grant(out, &holder, service, grant);
        }
        out.push_str("</ul>");
    }
    if !levels.is_empty() {
        add_resource(out, view, &levels);
    }
    out.push_str("</section>");
}

/// One grant below a service, written as a sentence: the action, the path and "and everything
/// below". The row then has the mark of a broader grant, the problem when the service does not
/// declare it, and the mark of the configuration or the Remove button.
fn resource_grant(out: &mut String, holder: &Holder, service: &ServiceActions, grant: &Grant) {
    let problem = service.problem(grant);
    let covering = covered_by(holder.grants, grant);
    let _ = write!(
        out,
        r#"<li class="grantRow{}"><span class="sentence"><code class="grantRow__action"{}>{}</code> <span class="muted">on</span> {} <span class="muted">and everything below</span></span>"#,
        if problem.is_some() {
            " grantRow--warn"
        } else {
            ""
        },
        service
            .actions
            .iter()
            .find(|(name, _)| *name == grant.action)
            .map(|(_, text)| format!(r#" title="{}""#, html_escape(text)))
            .unwrap_or_default(),
        html_escape(&grant.action),
        path_html(grant),
    );
    if let Some(problem) = &problem {
        let _ = write!(
            out,
            r##"<span class="grantRow__problem"><svg class="icon" aria-hidden="true"><use href="#i-alert-triangle"/></svg><span><strong>To check:</strong> {}. The PDP does not apply this grant.</span></span>"##,
            html_escape(problem)
        );
    } else if let Some(other) = covering {
        let _ = write!(
            out,
            r#"<span class="badge">Covered by {}: adds nothing</span>"#,
            reach_text(other)
        );
    }
    if holder.configured.contains(grant) {
        out.push_str(r#"<span class="badge grantRow__end">configuration</span>"#);
    } else if holder.editable {
        let _ = write!(
            out,
            r#"<form class="grantRow__end" method="post" action="/auth/users/grant-delete"><input type="hidden" name="csrf" value="{}"><input type="hidden" name="login" value="{}"><input type="hidden" name="grant" value="{}"><button class="button button--small" type="submit" aria-label="Remove {} on {}">Remove</button></form>"#,
            html_escape(holder.token),
            html_escape(holder.login),
            html_escape(&grant.to_string()),
            html_escape(&grant.action),
            html_escape(&grant.path_text()),
        );
    }
    out.push_str("</li>");
}

/// The words for a level: "One <type>", with the chain above it when it has one.
fn level_label(level: &Level) -> String {
    let kind = level.chain.last().map(String::as_str).unwrap_or_default();
    let mut text = format!("One <strong>{}</strong>", html_escape(kind));
    if level.chain.len() > 1 {
        let _ = write!(
            text,
            r#" <span class="muted">in {}</span>"#,
            level.chain[..level.chain.len() - 1]
                .iter()
                .map(|k| html_escape(k))
                .collect::<Vec<_>>()
                .join(" › ")
        );
    }
    text
}

/// A new grant on a resource, in two steps. Step 1 asks for the level, from the tree of types of
/// the service. Step 2 asks for the ids of that chain and the action, among the actions that list
/// the type. A service with one level has no step 1. Both steps work without script: step 1 is a
/// GET form, step 2 is the POST form.
fn add_resource(out: &mut String, view: &AccessView, levels: &[Level]) {
    let token = html_escape(view.token);
    let login = html_escape(&view.user.login);
    let id = html_escape(&view.service.id);
    let picked = if levels.len() == 1 {
        levels.first()
    } else {
        view.level.as_deref().and_then(|kind| {
            levels
                .iter()
                .find(|l| l.chain.last().map(String::as_str) == Some(kind))
        })
    };
    let Some(level) = picked else {
        let _ = write!(
            out,
            r#"<form class="step" method="get" action="{ACCESS_PATH}"><input type="hidden" name="user" value="{login}"><input type="hidden" name="service" value="{id}"><fieldset class="step__box"><legend class="step__title">Add a grant on a resource. Step 1 of 2: what does it name?</legend><ul class="options">"#
        );
        for (index, level) in levels.iter().enumerate() {
            let kind = level.chain.last().map(String::as_str).unwrap_or_default();
            let description = view
                .service
                .document
                .types
                .iter()
                .find(|t| t.name == kind)
                .map(|t| t.description.as_str())
                .unwrap_or_default();
            let _ = write!(
                out,
                r#"<li><label class="option"><input type="radio" name="level" value="{}"{}><span class="option__name">{}</span><span class="option__text">{}</span></label></li>"#,
                html_escape(kind),
                if index == 0 { " required" } else { "" },
                level_label(level),
                html_escape(description),
            );
        }
        out.push_str(r#"</ul><div class="formActions"><button class="button" type="submit">Next</button></div></fieldset></form>"#);
        return;
    };
    let kind = level.chain.last().map(String::as_str).unwrap_or_default();
    let _ = write!(
        out,
        r#"<form class="step" method="post" action="/auth/users/grant-add"><input type="hidden" name="csrf" value="{token}"><input type="hidden" name="login" value="{login}"><input type="hidden" name="service" value="{id}"><input type="hidden" name="type" value="{}"><fieldset class="step__box"><legend class="step__title">Add a grant on a resource{}: {}</legend><div class="step__ids">"#,
        html_escape(kind),
        if levels.len() == 1 {
            ""
        } else {
            ". Step 2 of 2"
        },
        level_label(level),
    );
    for (index, step) in level.chain.iter().enumerate() {
        let mut seen: Vec<&str> = view
            .known
            .iter()
            .filter_map(|g| g.path.get(index))
            .filter(|s| s.kind == *step)
            .map(|s| s.id.as_str())
            .collect();
        seen.sort_unstable();
        seen.dedup();
        let _ = write!(
            out,
            r#"<label class="field"><span class="field__label">Id of the {step_name}</span><input name="id{index}" type="text" list="ids-{index}" required maxlength="256" autocomplete="off" autocapitalize="off" spellcheck="false"><datalist id="ids-{index}">"#,
            step_name = html_escape(step),
        );
        for known in seen.iter().take(MAX_SUGGESTED_IDS) {
            let _ = write!(out, r#"<option value="{}">"#, html_escape(known));
        }
        out.push_str("</datalist></label>");
    }
    out.push_str(r#"</div><ul class="options">"#);
    for (index, action) in level.actions.iter().enumerate() {
        let description = view
            .service
            .actions
            .iter()
            .find(|(name, _)| name == action)
            .map(|(_, text)| text.as_str())
            .unwrap_or_default();
        let _ = write!(
            out,
            r#"<li><label class="option"><input type="radio" name="action" value="{0}" required{1}><span class="option__name"><code>{0}</code></span><span class="option__text">{2}</span></label></li>"#,
            html_escape(action),
            if level.actions.len() == 1 && index == 0 {
                " checked"
            } else {
                ""
            },
            html_escape(description),
        );
    }
    let _ = write!(
        out,
        r#"</ul><p class="hint">The grant covers that {} and everything below it. The server does not list the resources: type the id as the service writes it.</p><div class="formActions"><button class="button button--primary" type="submit">Add the grant</button>"#,
        html_escape(kind)
    );
    if levels.len() > 1 {
        let _ = write!(
            out,
            r#"<a class="button button--ghost" href="{}">Choose another level</a>"#,
            access_href(&view.user.login, &view.service.id)
        );
    }
    out.push_str("</div></fieldset></form>");
}

// Who has access to one service ---------------------------------------------------------------

/// The data of the page of one service: who has access.
pub struct ServiceView {
    pub mode: Mode,
    pub service: ServiceActions,
    /// Every account with the grants that apply to it.
    pub holders: Vec<(String, Vec<Grant>)>,
}

pub fn service_page(frame: &Frame, view: &ServiceView) -> String {
    let service = &view.service;
    let editable = view.mode.editable();
    let mut out = String::new();
    let _ = write!(
        out,
        r##"<header class="userHead"><div class="userHead__main"><nav class="crumbs" aria-label="Breadcrumb"><a href="{SERVICES_PATH}">Services</a><svg class="icon" aria-hidden="true"><use href="#i-chevron-right"/></svg></nav><h2 class="userHead__name" aria-current="page">{}</h2><code class="svcRow__id">{}</code></div></header>"##,
        html_escape(&service.name),
        html_escape(&service.id),
    );
    let with_grants: Vec<&(String, Vec<Grant>)> = view
        .holders
        .iter()
        .filter(|(_, grants)| grants.iter().any(|g| g.service == service.id))
        .collect();
    out.push_str(r#"<section class="part" aria-labelledby="holders-title">"#);
    part_head(
        &mut out,
        "holders-title",
        "Who has access",
        &format!("{} of {}", with_grants.len(), view.holders.len()),
        "",
    );
    out.push_str(r#"<p class="part__lede">The superadmin always has access. Each other account needs its own grants.</p>"#);
    if with_grants.is_empty() {
        out.push_str(r#"<p class="empty">No account has a grant on this service.</p>"#);
    } else {
        out.push_str(r#"<div class="tableBox"><table class="table accessTable"><thead><tr><th scope="col">Account</th><th scope="col">Access</th><th scope="col"><span class="visually-hidden">Edit</span></th></tr></thead><tbody>"#);
        for (login, grants) in with_grants {
            let _ = write!(
                out,
                r#"<tr><th scope="row" data-label="Account"><a class="accountLink" href="{}">{}</a></th><td data-label="Access">{}"#,
                html_escape(&user_path(login)),
                html_escape(login),
                access_line(service, grants),
            );
            out.push_str(r#"</td><td class="accessTable__end" data-label="Edit">"#);
            let _ = write!(
                out,
                r#"<a class="button button--small" href="{}" aria-label="{} access of {}">{}</a></td></tr>"#,
                access_href(login, &service.id),
                if editable { "Edit" } else { "Show" },
                html_escape(login),
                if editable { "Edit" } else { "Show" },
            );
        }
        out.push_str("</tbody></table></div>");
    }
    let others: Vec<&str> = view
        .holders
        .iter()
        .filter(|(_, grants)| !holds(grants, ACCESS, &service.id))
        .map(|(login, _)| login.as_str())
        .collect();
    if editable && !others.is_empty() {
        let _ = write!(
            out,
            r#"<form class="pick" method="get" action="{ACCESS_PATH}"><input type="hidden" name="service" value="{}"><input type="hidden" name="give" value="1"><label class="field"><span class="field__label">Give access to an account</span><select name="user" required><option value="" selected disabled>Choose an account</option>"#,
            html_escape(&service.id)
        );
        for login in others {
            let _ = write!(
                out,
                r#"<option value="{0}">{0}</option>"#,
                html_escape(login)
            );
        }
        out.push_str(
            r#"</select></label><button class="button" type="submit">Add</button></form>"#,
        );
    }
    out.push_str("</section>");
    page(Tab::Services, frame, &service.name, &out, false)
}

// One account, its activity ---------------------------------------------------------------------

/// What the account did: the connections per day, the latest sessions and the services used.
fn activity_part(out: &mut String, view: &UserView) {
    let activity = &view.user.activity;
    out.push_str(r#"<section class="part" aria-labelledby="activity-title">"#);
    let total: u32 = activity.days.iter().map(|d| d.sessions).sum();
    part_head(
        out,
        "activity-title",
        "Activity",
        &format!(
            "{} in {} days.{}",
            plural(total as usize, "connection", "connections"),
            activity.days.len(),
            peak_text(&activity.days, view.first_day)
        ),
        "",
    );
    out.push_str(&spark(&activity.days, true));
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

/// The password and the deletion of the account, on one row.
fn account_part(out: &mut String, view: &UserView) {
    let token = html_escape(view.token);
    let login = html_escape(&view.user.login);
    out.push_str(r#"<section class="part" aria-labelledby="account-title">"#);
    part_head(out, "account-title", "Account", "", "");
    let _ = write!(
        out,
        r#"<div class="accountRow"><form class="accountRow__password" method="post" action="/auth/users/update"><input type="hidden" name="csrf" value="{token}"><input type="hidden" name="login" value="{login}">"#
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
        r#"<button class="button" type="submit">Change password</button></form><div class="accountRow__delete"><p class="field__label">Delete the account</p><details class="confirm"><summary class="button button--danger">Delete {login}</summary><form class="confirm__panel" method="post" action="/auth/users/delete"><input type="hidden" name="csrf" value="{token}"><input type="hidden" name="login" value="{login}"><p>Its sessions end at once. Its history stays until it expires. This cannot be undone.</p><button class="button button--dangerFill" type="submit">Confirm: delete {login}</button></form></details></div></div></section>"#
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
    undeclared_grants(&mut out, view);
    out.push_str(r#"<div class="layout"><div class="layout__main">"#);
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
    out.push_str(r#"<p class="part__lede">Each service publishes its actions and its resource types at <code>/.well-known/access-actions</code>. A service without them has only <code>access</code>, and <code>access</code> is then the full permission. Point at an action to read what it does.</p>"#);
    if count == 0 {
        out.push_str(r#"<p class="empty">No service in the catalogue yet. A service appears when a container or an IngressRoute has <code>portal.*</code> labels.</p>"#);
    } else {
        out.push_str(r#"<div class="tableBox"><table class="table stack servicesTable"><thead><tr><th scope="col">Service</th><th scope="col">Path</th><th scope="col">Opened by</th><th scope="col">Actions</th><th scope="col">Resource types</th></tr></thead>"#);
        for group in view.catalog.groups() {
            let _ = write!(
                out,
                r#"<tbody><tr class="groupRow"><th scope="rowgroup" colspan="5">{}</th></tr>"#,
                html_escape(&group.name)
            );
            for service in &group.services {
                service_row(&mut out, view, service);
            }
            out.push_str("</tbody>");
        }
        out.push_str("</table></div>");
    }
    out.push_str(r#"</section></div><div class="layout__side">"#);
    out.push_str(r#"<aside class="side" aria-labelledby="model-title">"#);
    part_head(&mut out, "model-title", "How a grant reads", "", "");
    out.push_str(r#"<p class="part__lede">A grant gives one account one action on one node of a service, and on every node below it. Accounts get grants on their own page.</p><dl class="model"><dt><code>access on service:<var>id</var></code></dt><dd>Opens the service. Without it, the account does not see the service and the gate refuses it.</dd><dt><code><var>action</var> on service:<var>id</var></code></dt><dd>The action on the whole service and on every resource in it.</dd><dt><code><var>action</var> on service:<var>id</var>/<var>type</var>:<var>id</var></code></dt><dd>The action on that resource and everything below it, for example a database and its tables. 1 to 4 levels, along the types that the service declares.</dd><dt>Subtree match</dt><dd>A service that asks with <code>context.match = "subtree"</code> gets yes also for a node above a grant, so it can list that node and the account reaches what is below.</dd></dl></aside>"#);
    out.push_str("</div></div>");
    page(Tab::Services, frame, "Services", &out, false)
}

/// One service of the Services page, on one table row.
fn service_row(out: &mut String, view: &ServicesView, service: &Service) {
    let problem = view.problems.iter().find(|(id, _)| *id == service.id);
    let actions = view.services.iter().find(|s| s.id == service.id);
    let _ = write!(
        out,
        r#"<tr><th scope="row" data-label="Service"><a class="accountLink" href="{}"><span class="svcRow__name">{}</span></a> <code class="svcRow__id">{}</code>"#,
        html_escape(&service_path(&service.id)),
        html_escape(&service.name),
        html_escape(&service.id),
    );
    if problem.is_some() {
        out.push_str(r#" <span class="badge badge--warn">actions unreadable</span>"#);
    }
    let _ = write!(
        out,
        r#"</th><td data-label="Path"><code>{}</code>"#,
        html_escape(&service.href)
    );
    if !service.exclude.is_empty() {
        out.push_str(r#"<span class="cellNote">Open without sign-in:"#);
        for rule in &service.exclude {
            let _ = write!(out, r#" <code>{}</code>"#, html_escape(rule.text()));
        }
        out.push_str("</span>");
    }
    out.push_str(r#"</td><td data-label="Opened by">"#);
    let users: Vec<&String> = view
        .holders
        .iter()
        .filter(|(_, grants)| holds(grants, ACCESS, &service.id))
        .map(|(login, _)| login)
        .collect();
    if users.is_empty() {
        out.push_str(r#"<span class="muted">Superadmin only</span>"#);
    } else {
        let links: Vec<String> = users
            .iter()
            .map(|login| {
                format!(
                    r#"<a href="{}">{}</a>"#,
                    html_escape(&user_path(login)),
                    html_escape(login)
                )
            })
            .collect();
        out.push_str(&links.join(", "));
    }
    out.push_str(r#"</td><td data-label="Actions">"#);
    match actions {
        Some(found) if !found.actions.is_empty() => {
            out.push_str(
                r#"<span class="chips"><code class="tag" title="Open the service.">access</code>"#,
            );
            for (name, description) in &found.actions {
                let levels = found
                    .document
                    .actions
                    .iter()
                    .find(|a| &a.name == name)
                    .map(|a| a.resource_types.join(", "))
                    .unwrap_or_default();
                let title = if levels.is_empty() {
                    description.clone()
                } else {
                    format!("{description} On: {levels}.")
                };
                let _ = write!(
                    out,
                    r#"<code class="tag" title="{}">{}</code>"#,
                    html_escape(&title),
                    html_escape(name)
                );
            }
            out.push_str("</span>");
        }
        _ => out.push_str(r#"<span class="muted"><code>access</code> only</span>"#),
    }
    out.push_str(r#"</td><td data-label="Resource types">"#);
    match actions.filter(|f| !f.document.types.is_empty()) {
        Some(found) => {
            out.push_str(r#"<ul class="typeTree">"#);
            type_children(out, &found.document, "service", 0);
            out.push_str("</ul>");
        }
        None => out.push_str(r#"<span class="muted">none</span>"#),
    }
    out.push_str("</td></tr>");
    if let Some((_, text)) = problem {
        let _ = write!(
            out,
            r##"<tr class="subRow"><td colspan="5"><span class="cellWarn"><svg class="icon" aria-hidden="true"><use href="#i-alert-triangle"/></svg>The actions of {} cannot be read: {}. The page keeps the last good list.</span></td></tr>"##,
            html_escape(&service.name),
            html_escape(text)
        );
    }
}

/// The children of `parent` in the tree of types, as a nested list. The description of a type is
/// its title.
fn type_children(out: &mut String, document: &Document, parent: &str, depth: usize) {
    if depth > crate::grants::MAX_DEPTH {
        return;
    }
    for child in document.types.iter().filter(|t| t.parent == parent) {
        let _ = write!(
            out,
            r#"<li><code{}>{}</code>"#,
            if child.description.is_empty() {
                String::new()
            } else {
                format!(r#" title="{}""#, html_escape(&child.description))
            },
            html_escape(&child.name)
        );
        if document.types.iter().any(|t| t.parent == child.name) {
            out.push_str("<ul>");
            type_children(out, document, &child.name, depth + 1);
            out.push_str("</ul>");
        }
        out.push_str("</li>");
    }
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
        r#"<p class="part__lede">The service does not declare the resource of these grants for the action. The PDP does not apply them. Correct the grant on the page of the account, or the document of the service.</p><div class="tableBox"><table class="table stack wrap"><thead><tr><th scope="col">Account</th><th scope="col">Grant</th><th scope="col">Problem</th></tr></thead><tbody>{rows}</tbody></table></div></section>"#
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
        r#"<dl class="stats"><div class="stat"><dt class="stat__label">Sign-in attempts, 24 h</dt><dd class="stat__value">{}</dd></div><div class="stat"><dt class="stat__label">Refused, 24 h</dt><dd class="stat__value{}">{}</dd></div><div class="stat"><dt class="stat__label">Addresses refused, 24 h</dt><dd class="stat__value">{}</dd></div><div class="stat"><dt class="stat__label">Connections, {} d</dt><dd class="stat__value">{connections}</dd></div></dl>"#,
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
        &format!(
            "{}, all accounts, {} days.{}",
            plural(connections as usize, "connection", "connections"),
            view.days.len(),
            peak_text(view.days, view.first_day)
        ),
        "",
    );
    out.push_str(&spark(view.days, true));
    out.push_str("</section>");
    out.push_str(r#"<section class="part" aria-labelledby="events-title">"#);
    part_head(&mut out, "events-title", "Recent changes", "", "");
    if view.recent_events.is_empty() {
        out.push_str(r#"<p class="empty">No change recorded yet.</p>"#);
    } else {
        out.push_str(r#"<div class="tableBox"><table class="table events"><thead><tr><th scope="col">When (UTC)</th><th scope="col">Change</th><th scope="col">By, address</th></tr></thead><tbody>"#);
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
                Some(actor) if !actor.is_empty() => format!("{}, ", html_escape(actor)),
                _ => String::new(),
            };
            let _ = write!(
                out,
                r#"<tr{}><td>{}</td><td class="events__what">{}{account}</td><td class="muted">{by}{}</td></tr>"#,
                if event.kind == "login_failed" {
                    r#" class="events__row--danger""#
                } else {
                    ""
                },
                format_utc_secs(event.ts),
                audit_label(&event.kind),
                opt(&event.ip),
            );
        }
        out.push_str("</tbody></table></div>");
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

    fn access_view(user: UserCard, level: Option<&str>) -> AccessView<'static> {
        AccessView {
            notice: None,
            mode: Mode::Dynamic,
            service: services().remove(0),
            known: user.grants.clone(),
            user,
            give: false,
            level: level.map(str::to_string),
            token: "tok<en",
        }
    }

    fn service_view(holders: &[(String, Vec<Grant>)]) -> ServiceView {
        ServiceView {
            mode: Mode::Dynamic,
            service: services().remove(0),
            holders: holders.to_vec(),
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
                Tab::Users,
                access_page(&frame(), &access_view(card("alice", &["grafana"]), None)),
            ),
            (
                Tab::Services,
                services_page(&frame(), &services_view(&cat, &actions, &holders)),
            ),
            (
                Tab::Services,
                service_page(&frame(), &service_view(&holders)),
            ),
            (
                Tab::Logins,
                logins_page(&frame(), &logins(&blank, false, &[])),
            ),
        ];
        for (tab, html) in &pages {
            assert!(html.starts_with("<!doctype html>"), "{tab:?}");
            assert!(html.contains(&format!(
                r#"<link rel="stylesheet" href="/auth/settings.css?v={}">"#,
                asset_version()
            )));
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
    fn the_addresses_of_the_stylesheet_and_the_script_carry_their_version() {
        let version = asset_version();
        assert_eq!(version.len(), 10);
        assert!(version.bytes().all(|b| b.is_ascii_hexdigit()));
        assert!(script_tag().contains(&format!("/auth/assets/settings.js?v={version}")));
        let cat = catalog();
        let html = user_page(&frame(), &user_view(&cat, card("alice", &["grafana"])));
        assert!(html.contains(&format!("/auth/assets/settings.js?v={version}\" defer")));
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
        assert!(html.contains(r#"<span class="access"><strong>2 services</strong></span>"#));
        assert!(html.contains(
            r#"<span class="access"><strong>1 service</strong>, 1 grant on resources</span>"#
        ));
        assert!(
            html.contains(r#"<span class="muted">No access</span>"#),
            "carol"
        );
        assert!(
            html.contains(r#"<span class="badge badge--warn">1 to check</span>"#),
            "bob holds a grant that Grafana does not declare"
        );
        assert!(html.contains(r#"<td class="num num--danger" data-label="Refused, 30 d">2</td>"#));
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
        // The login and the password are on the top row. The permissions are cards by group.
        assert!(add.contains(r#"<div class="addUser__fields">"#));
        assert!(
            add.find("new-password").unwrap() < add.find(r#"<h3 class="perms__group">"#).unwrap()
        );
        assert!(
            !add.contains("copy"),
            "no copy of the access of another account"
        );
        assert!(add.contains(r#"<h3 class="perms__group">Infra</h3>"#));
        assert!(add.contains(r##"<input aria-label="Open Grafana" type="checkbox" name="g.infra-grafana.access" value="1" data-action="access" data-path=""><a class="perm__link" href="#panel-infra-grafana"><span class="perm__name">Grafana</span>"##));
        assert!(add.contains(r#"<input type="checkbox" name="g.infra-grafana.dashboards.read" value="1" data-action="dashboards.read" data-path="" disabled><code>dashboards.read</code><span class="perm__tip">See the dashboards.</span>"#), "locked until the box access is ticked");
        assert!(
            !add.contains(r#"form="grants""#),
            "the boxes are in the form"
        );
        let mut fixed = view(&cat);
        fixed.add_open = true;
        fixed.mode = Mode::Static;
        let html = users_page(&frame(), &fixed);
        assert!(
            !html.contains(r#"name="g."#),
            "no grant by the form in the static mode"
        );
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
    fn the_account_page_has_one_card_for_each_service_in_one_form_and_keeps_the_password_apart() {
        let cat = catalog();
        let mut alice = card("alice", &["grafana", "mixed"]);
        alice.grants.push(grant("dashboards.read", "infra-grafana"));
        alice
            .grants
            .push(sub("dashboards.write on service:infra-grafana/folder:ops"));
        let html = user_page(&frame(), &user_view(&cat, alice));
        assert!(html.contains("Opens 2 of 4"));
        assert!(html.contains(r#"<form id="grants" method="post" action="/auth/users/update"><input type="hidden" name="csrf" value="tok&lt;en"><input type="hidden" name="login" value="alice"><input type="hidden" name="grants" value="1"></form>"#));
        // The list has one line for each service. The line of a service with a grant is marked,
        // and the first of them shows its panel.
        assert!(html.contains(r##"<div class="perm__row has-grants is-selected" data-svc="infra-grafana"><input aria-label="Open Grafana" type="checkbox" form="grants" name="g.infra-grafana.access" value="1" data-action="access" data-path="" checked><a class="perm__link" href="#panel-infra-grafana"><span class="perm__name">Grafana</span><code class="svcRow__id">infra-grafana</code>"##));
        assert!(html.contains(r#"<section class="perm__panel is-selected" id="panel-infra-grafana" data-svc="infra-grafana" aria-label="Grafana">"#));
        assert_eq!(
            html.matches(r#"<span class="perm__count"><span class="perm__n">2/3</span>, 1 on resources</span>"#).count(),
            2,
            "in the line and in the panel"
        );
        // A service with no action has one line and no panel.
        assert!(html.contains(r#"<div class="perm__row" data-svc="infra-notes"><label class="perm__link perm__link--flat"><input type="checkbox" form="grants" name="g.infra-notes.access" value="1" data-action="access" data-path="">"#));
        assert!(!html.contains(r#"id="panel-infra-notes""#));
        // Each action has its box and its first sentence in text.
        assert!(html.contains(r#"<input type="checkbox" form="grants" name="g.infra-grafana.dashboards.read" value="1" data-action="dashboards.read" data-path="" checked><code>dashboards.read</code><span class="perm__tip">See the dashboards.</span>"#));
        assert!(html.contains("Change the &lt;dashboards&gt;."), "escaped");
        // The grants on resources are in the card, with a way to add one.
        assert!(
            html.contains(r#"<code class="grantRow__action""#) && html.contains("Remove</button>")
        );
        assert!(html.contains(r#"<a class="linkButton" href="/auth/settings/access?user=alice&amp;service=infra-grafana">Add a grant on a resource</a>"#));
        assert!(html.contains(">Save access</button>"));
        // The password form sends no box and no `grants` field.
        let start = html.rfind(r#"action="/auth/users/update""#).unwrap();
        let password = &html[start..start + html[start..].find("</form>").unwrap()];
        assert!(password.contains(r#"id="pw-alice""#) && password.contains(" required"));
        assert!(!password.contains(r#"name="grants""#) && !password.contains(r#"name="g."#));
        assert_eq!(html.matches(r#"action="/auth/users/update""#).count(), 2);
        assert!(html.contains(r#"action="/auth/users/impersonate""#));
        assert!(html.contains("Confirm: delete alice"));
    }

    #[test]
    fn the_list_and_the_panels_close_their_elements() {
        let cat = catalog();
        let html = user_page(
            &frame(),
            &user_view(&cat, card("alice", &["notes", "grafana"])),
        );
        let start = html.find(r#"<div class="perms">"#).unwrap();
        let end = html.find(r#"aria-labelledby="activity-title""#).unwrap();
        let cards = &html[start..end];
        for tag in [
            "div", "section", "header", "ul", "li", "label", "a", "h3", "h4",
        ] {
            assert_eq!(
                cards.matches(&format!("<{tag}")).count(),
                cards.matches(&format!("</{tag}>")).count(),
                "<{tag}> is closed"
            );
        }
    }

    #[test]
    fn a_service_that_names_its_hosts_shows_each_host_apart_with_its_own_actions() {
        let cat = catalog();
        let document = crate::actions::parse(
            "infra-grafana",
            br#"{"server":"infra-grafana","resource_types":[{"name":"host","parent":"service"}],"actions":[{"name":"dashboards.read","resource_types":["service","host"],"description":"See the dashboards."},{"name":"explorer.read","resource_types":["service","host"],"description":"Browse."}],"resources":[{"path":"host:prod","label":"Production"},{"path":"host:replica","label":"Replica","actions":["dashboards.read"]}]}"#,
        )
        .unwrap();
        let mut view = user_view(&cat, card("alice", &["grafana"]));
        view.services[0].actions = document
            .actions
            .iter()
            .map(|a| (a.name.clone(), a.description.clone()))
            .collect();
        view.services[0].document = std::sync::Arc::new(document);
        view.user
            .grants
            .push(sub("explorer.read on service:infra-grafana/host:prod"));
        let html = user_page(&frame(), &view);
        let prod = html
            .find(r#"<span class="path__type">host</span> <strong>Production</strong>"#)
            .unwrap();
        let replica = html.find(r#"<strong>Replica</strong>"#).unwrap();
        assert!(prod < replica);
        let prod_block = &html[prod..replica];
        let replica_block = &html[replica..replica + html[replica..].find("</section>").unwrap()];
        // The box of a grant on a host: `r.` and the grant in its text form. A held grant is ticked.
        assert!(prod_block.contains(r#"form="grants" name="r.explorer.read on service:infra-grafana/host:prod" value="1" data-action="explorer.read" data-path="host:prod" checked>"#));
        assert!(
            prod_block.contains(r#"name="r.dashboards.read on service:infra-grafana/host:prod""#)
        );
        // `explorer.read` is offered on the first host only: the second host names its actions.
        assert!(replica_block
            .contains(r#"name="r.dashboards.read on service:infra-grafana/host:replica""#));
        assert!(!replica_block.contains("explorer.read"));
        // A grant that is a box is not listed again as a sentence.
        assert!(!html.contains(r#"<ul class="grantRows""#));
        assert!(html.contains(">Production</strong>"));
    }

    #[test]
    fn a_grant_on_the_whole_service_shows_ticked_on_every_host_and_each_host_opens() {
        let cat = catalog();
        let document = crate::actions::parse(
            "infra-grafana",
            br#"{"server":"infra-grafana","resource_types":[{"name":"host","parent":"service"}],"actions":[{"name":"dashboards.read","resource_types":["service","host"]}],"resources":[{"path":"host:a"},{"path":"host:b"}]}"#,
        )
        .unwrap();
        let mut view = user_view(&cat, card("alice", &["grafana"]));
        view.services[0].actions = vec![("dashboards.read".into(), String::new())];
        view.services[0].document = std::sync::Arc::new(document);
        view.user
            .grants
            .push(grant("dashboards.read", "infra-grafana"));
        let html = user_page(&frame(), &view);
        // Both hosts show the action ticked, because the whole service covers them. Each host is
        // a closed block with its count, and each box tells the script its place in the tree.
        for host in ["a", "b"] {
            assert!(
                html.contains(&format!(r#"name="r.dashboards.read on service:infra-grafana/host:{host}" value="1" data-action="dashboards.read" data-path="host:{host}" checked>"#)),
                "{host}"
            );
        }
        assert_eq!(
            html.matches(r#"<span class="perm__n">1/1</span>"#).count(),
            2,
            "one count for each host"
        );
        assert!(
            html.contains(r#"<span class="perm__n">4/4</span>"#),
            "the service, with both hosts"
        );
        // A grant of its own on one host: only that host is ticked.
        view.user.grants = access(&["grafana"]);
        view.user
            .grants
            .push(sub("dashboards.read on service:infra-grafana/host:b"));
        let html = user_page(&frame(), &view);
        assert_eq!(
            html.matches(r#"data-path="host:a">"#).count(),
            1,
            "host a is not ticked"
        );
        assert!(html.contains(r#"data-path="host:b" checked>"#));
    }

    #[test]
    fn without_the_box_access_the_other_boxes_of_a_card_are_locked_and_unticked() {
        let cat = catalog();
        // alice holds an action of Grafana but not `access`: the page shows the action locked.
        let mut alice = card("alice", &[]);
        alice.grants.push(grant("dashboards.read", "infra-grafana"));
        let html = user_page(&frame(), &user_view(&cat, alice));
        assert!(html.contains(r#"name="g.infra-grafana.dashboards.read" value="1" data-action="dashboards.read" data-path="" disabled>"#));
        assert!(!html.contains(r#"data-path="" checked disabled"#));
        assert!(html.contains(r#"<span class="perm__n">0/3</span>"#));
        // With `access`, the boxes are free and the count follows what is ticked.
        let mut open = card("alice", &["grafana"]);
        open.grants.push(grant("dashboards.read", "infra-grafana"));
        let html = user_page(&frame(), &user_view(&cat, open));
        assert!(html.contains(r#"name="g.infra-grafana.dashboards.read" value="1" data-action="dashboards.read" data-path="" checked>"#));
        assert!(html.contains(r#"<span class="perm__n">2/3</span>"#));
    }

    #[test]
    fn the_add_form_is_the_whole_page_and_says_little() {
        let cat = catalog();
        let mut v = view(&cat);
        v.add_open = true;
        let html = users_page(&frame(), &v);
        assert!(html.contains(r#"id="accounts-title">New user</h2>"#));
        assert!(
            !html.contains(r#"class="table accounts""#),
            "no other account"
        );
        assert!(!html.contains("End all sessions") && !html.contains("refused sign-in"));
        assert!(!html.contains("part__lede") && !html.contains("Permissions (optional)"));
        assert!(!html.contains("At least 10 characters") && !html.contains("new-login-hint"));
        assert!(html.contains(">Permissions</legend>"));
        // The list of accounts has no lede either.
        let list = users_page(&frame(), &view(&cat));
        assert!(!list.contains("part__lede"));
    }

    #[test]
    fn a_new_account_has_no_marked_line_and_the_first_panel_shows() {
        let cat = catalog();
        let html = user_page(&frame(), &user_view(&cat, card("carol", &[])));
        assert!(html.contains("Opens 0 of 4"));
        assert!(!html.contains(" has-grants"));
        assert_eq!(
            html.matches(r#"<section class="perm__panel"#).count(),
            1,
            "Grafana only"
        );
        assert!(
            html.contains(r#"<section class="perm__panel is-selected" id="panel-infra-grafana""#)
        );
        assert_eq!(
            html.matches(r#"perm__link--flat"#).count(),
            3,
            "one line each"
        );
    }

    #[test]
    fn the_editor_has_its_own_save_that_names_the_service_and_shows_each_action_in_words() {
        let mut alice = card("alice", &["grafana"]);
        alice.grants.push(grant("dashboards.read", "infra-grafana"));
        let html = access_page(&frame(), &access_view(alice, None));
        assert!(html.contains(r#"<form class="editor" method="post" action="/auth/users/update"><input type="hidden" name="csrf" value="tok&lt;en"><input type="hidden" name="login" value="alice"><input type="hidden" name="grants" value="1"><input type="hidden" name="only" value="infra-grafana">"#));
        assert!(html.contains(
            r#"<input type="checkbox" name="g.infra-grafana.access" value="1" data-action="access" data-path="" checked>"#
        ));
        assert!(html.contains(
            r#"<input type="checkbox" name="g.infra-grafana.dashboards.read" value="1" data-action="dashboards.read" data-path="" checked>"#
        ));
        assert!(html.contains(
            r#"<input type="checkbox" name="g.infra-grafana.dashboards.write" value="1" data-action="dashboards.write" data-path="">"#
        ));
        assert!(
            html.contains("See the dashboards."),
            "the description is text, not a hover"
        );
        assert!(html.contains("Change the &lt;dashboards&gt;."), "escaped");
        assert_eq!(html.matches(">Save</button>").count(), 1);
        assert!(html.contains(
            r#"<a class="button button--ghost" href="/auth/settings/users/alice">Back to alice</a>"#
        ));
        assert!(!html.contains("Save access"), "no global bar");
        // "Give access": the box starts ticked, and the page says that nothing is saved yet.
        let mut give = access_view(card("bob", &[]), None);
        give.give = true;
        let html = access_page(&frame(), &give);
        assert!(html.contains(
            r#"<input type="checkbox" name="g.infra-grafana.access" value="1" data-action="access" data-path="" checked>"#
        ));
        assert!(html.contains("bob does not have this access yet. Save to give it."));
        let plain = access_page(&frame(), &access_view(card("bob", &[]), None));
        assert!(
            plain.contains(r#"<input type="checkbox" name="g.infra-grafana.access" value="1" data-action="access" data-path="">"#)
        );
    }

    #[test]
    fn the_editor_writes_each_resource_grant_as_a_sentence_with_its_problem_and_its_cover() {
        let mut alice = card("alice", &["grafana"]);
        alice.grants.push(grant("dashboards.read", "infra-grafana"));
        alice.grants.push(sub(
            "dashboards.write on service:infra-grafana/folder:ops/dashboard:a%2Fb",
        ));
        alice.grants.push(sub(
            "dashboards.read on service:infra-grafana/folder:%3Cx%3E",
        ));
        alice
            .grants
            .push(sub("dashboards.read on service:infra-grafana/team:t"));
        let html = access_page(&frame(), &access_view(alice, None));
        assert!(html.contains(r#"<span class="sentence"><code class="grantRow__action" title="Change the &lt;dashboards&gt;.">dashboards.write</code> <span class="muted">on</span> <span class="path"><span class="path__type">folder</span> <code class="path__id">ops</code><span class="path__sep" aria-hidden="true">›</span><span class="path__type">dashboard</span> <code class="path__id">a/b</code></span> <span class="muted">and everything below</span></span>"#), "decoded ids");
        assert!(html.contains(r#"name="grant" value="dashboards.write on service:infra-grafana/folder:ops/dashboard:a%2Fb""#));
        assert!(
            html.contains(r#"aria-label="Remove dashboards.write on folder:ops/dashboard:a%2Fb""#)
        );
        assert!(
            html.contains(r#"<code class="path__id">&lt;x&gt;</code>"#),
            "escaped"
        );
        // `dashboards.read` on the whole service covers the folder grant: it adds nothing.
        assert!(html.contains(
            r#"<span class="badge">Covered by the action on the whole service: adds nothing</span>"#
        ));
        assert!(
            !html.contains("Covered by dashboards.write"),
            "a write grant does not cover a read grant"
        );
        assert_eq!(
            html.matches("grantRow--warn").count(),
            1,
            "team is not declared"
        );
        assert!(html.contains("<strong>To check:</strong>"));
    }

    #[test]
    fn a_grant_on_a_resource_has_two_steps_and_the_actions_of_the_chosen_level_only() {
        let mut alice = card("alice", &["grafana"]);
        alice
            .grants
            .push(sub("dashboards.read on service:infra-grafana/folder:ops"));
        alice
            .grants
            .push(sub("dashboards.read on service:infra-grafana/folder:logs"));
        // Step 1: the levels of the tree. No raw pattern, no id field yet.
        let html = access_page(&frame(), &access_view(alice.clone(), None));
        assert!(html.contains(r#"<form class="step" method="get" action="/auth/settings/access">"#));
        assert!(html.contains(r#"<input type="radio" name="level" value="folder" required><span class="option__name">One <strong>folder</strong></span><span class="option__text">A folder.</span>"#));
        assert!(html.contains(r#"<input type="radio" name="level" value="dashboard">"#));
        assert!(
            html.contains("One <strong>dashboard</strong> <span class=\"muted\">in folder</span>")
        );
        assert!(!html.contains(r#"action="/auth/users/grant-add""#));
        assert!(!html.contains("&lt;id&gt;"), "no raw pattern");
        // Step 2: one id for each type of the chain, then the actions that list the type.
        let html = access_page(&frame(), &access_view(alice, Some("dashboard")));
        assert!(
            html.contains(r#"<form class="step" method="post" action="/auth/users/grant-add">"#)
        );
        assert!(html.contains(r#"<input type="hidden" name="type" value="dashboard">"#));
        assert!(html.contains(r#"<span class="field__label">Id of the folder</span><input name="id0" type="text" list="ids-0""#));
        assert!(html
            .contains(r#"<span class="field__label">Id of the dashboard</span><input name="id1""#));
        // The ids that the grants already use are suggested for their level.
        assert!(html.contains(
            r#"<datalist id="ids-0"><option value="logs"><option value="ops"></datalist>"#
        ));
        assert!(html.contains(r#"<datalist id="ids-1"></datalist>"#));
        assert!(html.contains(
            r#"<input type="radio" name="action" value="dashboards.write" required checked>"#
        ));
        assert!(
            !html.contains(r#"value="dashboards.read" required"#),
            "dashboards.read does not list the type dashboard"
        );
        assert!(html.contains("Change the &lt;dashboards&gt;."));
        assert!(html.contains(">Choose another level</a>"));
        // A level that no action lists, or an unknown level, gives step 1 again.
        let html = access_page(
            &frame(),
            &access_view(card("bob", &["grafana"]), Some("team")),
        );
        assert!(html.contains(r#"name="level""#) && !html.contains(r#"name="id0""#));
    }

    #[test]
    fn a_service_with_one_level_has_no_first_step() {
        let mut view = access_view(card("alice", &["grafana"]), None);
        let mut document = (*view.service.document).clone();
        document.types.retain(|t| t.name == "folder");
        document.actions.retain(|a| a.name == "dashboards.read");
        view.service.document = std::sync::Arc::new(document);
        let html = access_page(&frame(), &view);
        assert!(!html.contains(r#"name="level""#));
        assert!(html.contains(r#"<input name="id0""#));
        assert!(
            html.contains(r#"value="dashboards.read" required checked"#),
            "one action"
        );
    }

    #[test]
    fn configured_grants_are_read_only_and_the_static_mode_has_no_form() {
        let cat = catalog();
        let mut bob = card("bob", &["mixed"]);
        bob.configured = access(&["grafana"]);
        bob.grants.extend(access(&["grafana"]));
        let configured = sub("dashboards.read on service:infra-grafana/folder:ops");
        bob.grants.push(configured.clone());
        bob.configured.push(configured);
        let mut v = access_view(bob.clone(), None);
        v.mode = Mode::Hybrid;
        let html = access_page(&frame(), &v);
        assert!(html.contains(r#"<input type="checkbox" checked disabled><span class="option__name"><code>access</code></span>"#));
        assert!(html.contains(r#"<span class="badge">configuration</span></label>"#));
        assert!(!html.contains("g.infra-grafana.access"));
        assert!(html.contains(r#"<span class="badge grantRow__end">configuration</span>"#));
        assert!(!html.contains("grant-delete"));
        v.mode = Mode::Static;
        let html = access_page(&frame(), &v);
        assert!(!html.contains(r#"name="g."#) && !html.contains(r#"action="/auth/users/update""#));
        assert!(!html.contains("grant-add") && !html.contains(">Save</button>"));
        assert!(html.contains("cannot change them"));
        let mut page = user_view(&cat, bob);
        page.mode = Mode::Static;
        let html = user_page(&frame(), &page);
        assert!(!html.contains(r#"id="grants""#) && !html.contains(">Save access<"));
        assert!(!html.contains(r#"name="g."#) && !html.contains("Add a grant on a resource"));
        assert!(html.contains("cannot change them"));
    }

    #[test]
    fn the_service_page_shows_who_has_access_and_offers_the_accounts_without_it() {
        let holders = vec![
            (
                "alice".to_string(),
                vec![
                    grant(ACCESS, "infra-grafana"),
                    grant("dashboards.read", "infra-grafana"),
                    sub("dashboards.write on service:infra-grafana/folder:ops"),
                ],
            ),
            ("bob".to_string(), access(&["notes"])),
            ("carol".to_string(), Vec::new()),
        ];
        let html = service_page(&frame(), &service_view(&holders));
        assert!(html.contains("1 of 3"));
        assert!(
            html.contains(r#"<a class="accountLink" href="/auth/settings/users/alice">alice</a>"#)
        );
        assert!(!html.contains(r#">bob</a>"#), "bob has no grant on Grafana");
        assert!(html.contains("<strong>open</strong>") && html.contains("1 resource grant"));
        assert!(
            html.contains(r#"href="/auth/settings/access?user=alice&amp;service=infra-grafana""#)
        );
        assert!(html.contains(r#"<input type="hidden" name="service" value="infra-grafana"><input type="hidden" name="give" value="1">"#));
        assert!(html
            .contains(r#"<option value="bob">bob</option><option value="carol">carol</option>"#));
        assert!(!html.contains(r#"<option value="alice">"#));
        let none = service_page(&frame(), &service_view(&[]));
        assert!(none.contains("No account has a grant on this service."));
    }

    #[test]
    fn the_services_page_links_each_service_to_its_own_page() {
        let cat = catalog();
        let actions = services();
        let html = services_page(&frame(), &services_view(&cat, &actions, &[]));
        assert!(html.contains(r#"<a class="accountLink" href="/auth/settings/services/infra-grafana"><span class="svcRow__name">Grafana</span></a>"#));
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
        assert!(html.contains(r#"<code class="tag" title="See the dashboards. On: service, folder.">dashboards.read</code>"#));
        assert!(html.contains(r#"<ul class="typeTree"><li><code title="A folder.">folder</code><ul><li><code>dashboard</code></li></ul></li></ul>"#));
        assert_eq!(
            html.matches(r#"<span class="muted"><code>access</code> only</span>"#)
                .count(),
            3
        );
        assert_eq!(
            html.matches(r#"<a href="/auth/settings/users/alice">alice</a>"#)
                .count(),
            2
        );
        assert!(
            !html.contains(">bob</a>"),
            "an action without access does not open the service"
        );
        assert_eq!(html.matches("Superadmin only").count(), 2, "Notes and Typo");
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
        assert!(html.contains(r#"<dd class="stat__value stat__value--danger">2</dd>"#));
        assert!(html.contains("The password is never recorded"));
        assert!(html.contains("3 attempts"));
        assert!(!html.contains(r#"class="inbox""#));
        assert!(html.contains(r#"<tr class="events__row--danger"><td>2026-10-10 09:11:42</td><td class="events__what">Refused sign-in <a href="/auth/settings/users/alice">alice</a></td>"#));
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
            "user-added-access",
            "add-unknown-service",
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
