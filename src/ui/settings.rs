//! The settings of the superadmin. Each tab is one HTML page, and each page is built from the parts
//! of [`super::components`]. The tabs are Users, Services, Keys and Inbox. A tab shows only when its
//! feature is on. The page of one account is a child page of Users, not a tab.
//!
//! The pages explain the access model with their structure:
//!
//! - The Users page has the accounts on the left. On the right it has the sign-ins and the recent
//!   changes. A row of the accounts opens the page of the account.
//! - The page of an account has one row of tools (log in as, the password, the activity, delete,
//!   Save). Under it, the groups "Services" and "Portal features" stand side by side. The editor of
//!   grants draws them.
//! - "New user" opens the same page in create mode: the login and the password stand in the row of
//!   tools, the editor is under it, and one button makes the account.
//! - The Services page has the services on the left. A click on a row selects it, with a link.
//!   The panel "Opened by" shows which accounts open the selected service and which do not.
//!
//! A page carries only what it shows, so the browser never builds the DOM of the other pages. The
//! launcher carries none of it. Everything works without script, but the editor of grants, which
//! the script draws. The graphs are inline SVG. This code builds them from attributes only,
//! because the CSP forbids `style`. Everything that comes from outside is escaped. This includes
//! logins (typed or stored), addresses, user agents, service names and resource ids.

use std::fmt::Write as _;

use super::components::{
    ask, chips, icon, icon_count, icon_link, icon_post, popover, split, toolbar, zone, zone_bare,
    Ask, Chip, Side, Strength, Zone,
};
use super::grant_editor::{self, Place};
use super::{document_class, sign_out_form, status_icon, theme_switch};
use crate::actions::Document;
use crate::catalog::{Catalog, Service};
use crate::grants::{holds, Grant, Mode, ACCESS};
use crate::store::{AccountActivity, AuditRow, DayStat, LoginPage, ServiceUse, SessionRow};
use crate::util::{
    format_duration, html_escape, percent_encode_query_component, short_agent, time_tag,
};

pub const USERS_PATH: &str = "/auth/settings";
/// The page of one account is this prefix, then the login.
pub const USER_PATH_PREFIX: &str = "/auth/settings/users/";
/// "New user": the page of an account in create mode.
pub const NEW_USER_PATH: &str = "/auth/settings/new-user";
pub const SERVICES_PATH: &str = "/auth/settings/services";
/// The address of "Give access" in the panel "Opened by". The query names `user` and `service`.
/// The answer is a redirect to the page of the account, with the service chosen in the editor.
pub const GIVE_PATH: &str = "/auth/settings/give";
/// The page of the keys (portal 5.8a).
pub const KEYS_PATH: &str = "/auth/keys";
/// The page of the inbox: the first page of the tab "Inbox".
pub const INBOX_PATH: &str = crate::ui::inbox::PAGE;

/// The path of the page of one account. A login has only `a-z`, `0-9`, `.`, `-` and `_`, so the
/// path needs no encoding. The caller escapes the path for HTML all the same.
pub fn user_path(login: &str) -> String {
    format!("{USER_PATH_PREFIX}{login}")
}

/// The page of the account `login`, with the editor of grants on the service `service`. The
/// caller escapes it for HTML.
pub fn user_service_path(login: &str, service: &str) -> String {
    format!(
        "{}?service={}",
        user_path(login),
        percent_encode_query_component(service)
    )
}

/// Where "Give access" goes: the page of the account, with the grant `access` on the service
/// ticked in the editor. The editor saves nothing until the person clicks **Save**.
pub fn give_target(login: &str, service: &str) -> String {
    format!("{}&give=1", user_service_path(login, service))
}

/// The Services page with a selected service and a filter.
fn services_href(selected: Option<&str>, only_check: bool) -> String {
    let mut query = Vec::new();
    if let Some(id) = selected {
        query.push(format!("svc={}", percent_encode_query_component(id)));
    }
    if only_check {
        query.push("f=check".to_string());
    }
    if query.is_empty() {
        SERVICES_PATH.to_string()
    } else {
        format!("{SERVICES_PATH}?{}", query.join("&"))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tab {
    Users,
    Services,
    /// The page of the keys (portal 5.8). Each user who can read keys sees it.
    Keys,
    /// The pages of the inbox. Each user with a right of the inbox features sees it.
    Inbox,
}

impl Tab {
    fn path(self) -> &'static str {
        match self {
            Self::Users => USERS_PATH,
            Self::Services => SERVICES_PATH,
            Self::Keys => KEYS_PATH,
            Self::Inbox => INBOX_PATH,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Users => "Users",
            Self::Services => "Services",
            Self::Keys => "Keys",
            Self::Inbox => "Inbox",
        }
    }
}

/// The data that every settings page needs around its content.
pub struct Frame<'a> {
    pub logout_token: &'a str,
    /// The viewer sees the tab "Keys": the portal accepts keys.
    pub keys: bool,
    /// The viewer sees the tab "Inbox": the file has the block `inbox`, and the viewer has a right
    /// of the service.
    pub inbox: bool,
    /// The viewer is the superadmin, who sees the tabs Users and Services too.
    pub superadmin: bool,
}

impl Frame<'_> {
    /// The tabs of the viewer, in the order of the page. There are four at most. A tab that the
    /// configuration turns off is not in the list: the page draws no grey tab.
    pub fn tabs(&self) -> Vec<Tab> {
        let mut tabs = Vec::new();
        if self.superadmin {
            tabs.extend([Tab::Users, Tab::Services]);
        }
        if self.keys {
            tabs.push(Tab::Keys);
        }
        if self.inbox {
            tabs.push(Tab::Inbox);
        }
        tabs
    }
}

/// The outcome of the last action. The outcome is one code from a fixed list. The URL never
/// carries text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Notice {
    pub text: &'static str,
    pub ok: bool,
}

pub fn notice_for(code: &str) -> Option<Notice> {
    let (text, ok) = match code {
        "user-added" => ("Account added.", true),
        "user-updated" => ("Access saved. It applies from the next request.", true),
        "password-reset" => (
            "Password changed. The sessions of this account are closed.",
            true,
        ),
        "unchanged" => ("Nothing to change.", true),
        "user-deleted" => ("Account deleted. Its sessions are closed.", true),
        "exists" => ("That login already exists.", false),
        "reserved" => ("That login is reserved for the superadmin.", false),
        "invalid-login" => (
            "Invalid login: 1 to 32 characters among a-z, 0-9, dot, dash and underscore.",
            false,
        ),
        "weak-password" => ("The password must have at least 10 characters.", false),
        "limit" => ("The account limit is reached (users.max_users).", false),
        "edit-weak-password" => ("The password must have at least 10 characters.", false),
        "edit-unknown-grant" | "unknown-grant" => (
            "A grant of the form is not valid now. Open the page again.",
            false,
        ),
        "grants-static" => (
            "The grants come from the configuration only (access.grants_mode is static).",
            false,
        ),
        "bad-limits" => (
            "A limit value is not valid for this grant: a whole number in the range of the limit, from the document of the service.",
            false,
        ),
        "keys-needs" => (
            "An action of the keys needs another action. Give the other action first, and save.",
            false,
        ),
        "keys-lifetime" => (
            "The longest key lifetime is a whole number of seconds from 0 to 31536000.",
            false,
        ),
        "grant-limit" => ("This account has the largest number of grants.", false),
        "no-user" => ("That account does not exist.", false),
        "error" => ("The action failed. See the portal logs.", false),
        _ => return None,
    };
    Some(Notice { text, ok })
}

/// The ids of the sprite symbols that a page draws: each `href="#i-name"` of its markup. The page
/// inlines these symbols only.
fn icon_ids(body: &str) -> Vec<&str> {
    let mut ids: Vec<&str> = Vec::new();
    let mut rest = body;
    while let Some(at) = rest.find(r##"href="#i-"##) {
        let tail = &rest[at + r#"href=""#.len() + 1..];
        let end = tail.find('"').unwrap_or(tail.len());
        let id = &tail[..end];
        if !ids.contains(&id) {
            ids.push(id);
        }
        rest = &tail[end..];
    }
    ids
}

/// The page around a tab, for a page that fills the window: its parts are bounded and scroll
/// inside. `title` names the page for the browser and for the screen reader.
pub(super) fn fill_page(tab: Tab, frame: &Frame, title: &str, content: &str) -> String {
    shell_class(&frame.tabs(), tab, frame, title, content, " settings--fill")
}

/// The shell of every settings page. The header and the tabs stay at the top. The `main` element
/// is the one scroller of the page, and it reserves the place of its scrollbar.
fn shell_class(
    tabs: &[Tab],
    tab: Tab,
    frame: &Frame,
    title: &str,
    content: &str,
    class: &str,
) -> String {
    let mut body = String::new();
    let _ = write!(
        body,
        r##"<a class="skipLink" href="#main">Skip to content</a><header class="appHeader"><div class="appHeader__left"><a class="backLink" href="/"><svg class="icon icon--lg" aria-hidden="true"><use href="#i-arrow-left"/></svg>Back to panel</a></div><div class="appHeader__right">{}{}</div></header><nav class="navRow" aria-label="Settings"><div class="contentTabs contentTabs--nav">"##,
        theme_switch(),
        sign_out_form(frame.logout_token),
    );
    for each in tabs.iter().copied() {
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
        r#"</div></nav><main class="settings{class}" id="main" tabindex="-1"><h1 class="visually-hidden">{}</h1>{content}</main>"#,
        html_escape(title)
    );
    body.push_str(&script_tag());
    let icons = icon_ids(&body);
    document_class(
        &format!("Settings: {title}"),
        &format!(
            r#"<link rel="stylesheet" href="/auth/settings.css?v={}">"#,
            asset_version()
        ),
        &icons,
        "shell",
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

pub(super) fn plural(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

fn opt(value: &Option<String>) -> String {
    match value {
        Some(text) if !text.is_empty() => html_escape(text),
        _ => r#"<span class="muted">none</span>"#.to_string(),
    }
}

pub(super) fn notice_html(out: &mut String, notice: Option<Notice>) {
    if let Some(notice) = notice {
        // A failure interrupts the page. The page announces the failure at once.
        let _ = write!(
            out,
            r#"<div class="uiBanner uiBanner--inset{}" role="{}"><span class="uiBanner__text">{}</span></div>"#,
            if notice.ok { " uiBanner--ok" } else { "" },
            if notice.ok { "status" } else { "alert" },
            html_escape(notice.text)
        );
    }
}

// The data of the accounts ----------------------------------------------------------------------

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
    /// The account comes from the configuration file. The pages show it and change none of it
    /// (portal 3.4a).
    pub file: bool,
    /// The keys that the account owns.
    pub keys: usize,
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
    /// The portal can ask the service for the children of a node (portal 3.1a): the service has
    /// an address for its documents, and the portal makes identity tokens.
    pub fetch: bool,
    /// The ids of the groups of the inbox that are critical. Only the service `portal` has any.
    pub critical: Vec<String>,
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
}

/// The id of the service `portal`.
const PORTAL: &str = crate::portal_actions::SERVICE;

/// The feature of an action of the service `portal`: the group of the icon in the table of
/// accounts.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Feature {
    Keys,
    Inbox,
}

impl Feature {
    fn of(action: &str) -> Option<Self> {
        if action.starts_with("keys.") || action == crate::portal_actions::USERS_READ {
            Some(Self::Keys)
        } else if ["inbox.", "alert.", "phone."]
            .iter()
            .any(|prefix| action.starts_with(prefix))
        {
            Some(Self::Inbox)
        } else {
            None
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::Keys => "Keys",
            Self::Inbox => "Inbox",
        }
    }

    fn icon(self) -> &'static str {
        match self {
            Self::Keys => "key",
            Self::Inbox => "inbox",
        }
    }
}

/// How many actions of a feature the account holds, and how many the document of the portal has.
fn feature_rights(
    user: &UserCard,
    features: Option<&ServiceActions>,
    feature: Feature,
) -> (usize, usize) {
    let total = features.map_or(0, |f| {
        f.actions
            .iter()
            .filter(|(name, _)| Feature::of(name) == Some(feature))
            .count()
    });
    let mut held: Vec<&str> = user
        .grants
        .iter()
        .filter(|g| g.service == PORTAL && Feature::of(&g.action) == Some(feature))
        .map(|g| g.action.as_str())
        .collect();
    held.sort_unstable();
    held.dedup();
    (held.len(), total)
}

/// The access of one account in short: the services that it opens, and its grants to check.
struct Summary {
    services: usize,
    to_check: usize,
}

fn summary(catalog: &Catalog, services: &[ServiceActions], user: &UserCard) -> Summary {
    let opened = catalog
        .groups()
        .iter()
        .flat_map(|group| group.services.iter())
        .filter(|service| holds(&user.grants, ACCESS, &service.id))
        .count();
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
        to_check,
    }
}

// Users -----------------------------------------------------------------------------------------

/// The data of the Users page: the accounts, the sign-ins and the recent changes.
pub struct UsersView<'a> {
    pub notice: Option<Notice>,
    /// `users.allow_creation`. `false`: the page has no button, and no entry that makes an
    /// account.
    pub allow_creation: bool,
    /// Where the grants come from.
    pub mode: Mode,
    /// The whole catalogue, not the filtered catalogue for display.
    pub catalog: &'a Catalog,
    /// The services of the catalogue with their actions. The order is the order of the catalogue.
    pub services: Vec<ServiceActions>,
    /// The accounts of the database, then the accounts of the file.
    pub users: Vec<UserCard>,
    /// The portal features with their actions, for the totals of the icons. `None`: the file turns
    /// none on.
    pub features: Option<ServiceActions>,
    /// The file has the block `keys`: the table has the column of the keys.
    pub keys: bool,
    /// The file has the block `inbox`.
    pub inbox: bool,
    pub max_users: usize,
    /// The anti-CSRF token of the administration forms.
    pub token: &'a str,
    /// The newest sign-ins, and the figures of the chips.
    pub logins: &'a LoginPage,
    /// The chip "refused" is on.
    pub only_refused: bool,
    /// Every real connection, per day, all accounts.
    pub days: &'a [DayStat],
    /// The latest audit events.
    pub recent_events: &'a [AuditRow],
}

impl UsersView<'_> {
    /// The page offers "New user": the file allows it and the grants are not read-only.
    fn can_create(&self) -> bool {
        self.allow_creation && self.mode.editable()
    }
}

pub fn users_page(frame: &Frame, view: &UsersView) -> String {
    let mut out = String::new();
    notice_html(&mut out, view.notice);
    let mut tools = String::new();
    let _ = write!(
        tools,
        "{}",
        ask(&Ask {
            label: "End all sessions",
            icon: "logout",
            question: "End all sessions? You sign in again too.",
            verb: "End",
            action: "/auth/users/end-sessions",
            fields_html: &format!(
                r#"<input type="hidden" name="csrf" value="{}"><input type="hidden" name="confirm" value="end-all-sessions">"#,
                html_escape(view.token)
            ),
            strength: Strength::Light,
            name: "",
            danger: true,
            side: Side::End,
        })
    );
    if view.can_create() {
        let _ = write!(
            tools,
            r#"<a class="button button--primary" href="{NEW_USER_PATH}">{}New user</a>"#,
            icon("plus")
        );
    }
    let database = view.users.iter().filter(|u| !u.file).count();
    out.push_str(&toolbar(
        "Accounts",
        &format!("{database} of {}", view.max_users),
        &tools,
    ));
    let left = zone_bare("Accounts", &accounts_table(view));
    let signins = signins_zone(view);
    let changes = changes_zone(view);
    out.push_str(&split("users", &left, &format!("{signins}{changes}")));
    fill_page(Tab::Users, frame, "Users", &out)
}

/// The table of the accounts. The columns of the features and of the keys are absent when the
/// file turns the feature off.
fn accounts_table(view: &UsersView) -> String {
    let features = view.keys || view.inbox;
    let mut out =
        String::from(r#"<table class="grid grid--users"><colgroup><col><col class="w64">"#);
    if features {
        out.push_str(r#"<col class="w88">"#);
    }
    if view.keys {
        out.push_str(r#"<col class="w48">"#);
    }
    out.push_str(r#"<col class="w152"><col class="w48"><col class="w56"></colgroup><thead><tr><th scope="col">Account</th><th scope="col">Services</th>"#);
    if features {
        out.push_str(r#"<th scope="col">Features</th>"#);
    }
    if view.keys {
        out.push_str(r#"<th scope="col" class="r">Keys</th>"#);
    }
    out.push_str(r#"<th scope="col" class="r">Last sign-in</th><th scope="col" class="r" title="Sessions, 30 days">Sess.</th><th scope="col" class="r" title="Refused sign-ins, 30 days">Refused</th></tr></thead><tbody>"#);
    for user in &view.users {
        account_row(&mut out, view, user);
    }
    out.push_str("</tbody></table>");
    out
}

fn account_row(out: &mut String, view: &UsersView, user: &UserCard) {
    let login = html_escape(&user.login);
    let _ = write!(
        out,
        r#"<tr><th scope="row"><a class="rowLink" href="{}">{login}</a>"#,
        html_escape(&user_path(&user.login))
    );
    if user.superadmin {
        out.push_str(r#" <span class="badge">Superadmin</span>"#);
    }
    if user.file {
        out.push_str(r#" <span class="badge">file</span>"#);
    }
    out.push_str("</th><td>");
    if user.superadmin {
        out.push_str(r#"<span class="muted">every</span>"#);
    } else {
        let s = summary(view.catalog, &view.services, user);
        if s.services == 0 {
            out.push_str(r#"<span class="muted">none</span>"#);
        } else {
            let _ = write!(out, "{} of {}", s.services, view.services.len());
        }
        if s.to_check > 0 {
            out.push(' ');
            out.push_str(&status_icon(
                "warn",
                "alert-triangle",
                &plural(s.to_check, "permission to check", "permissions to check"),
            ));
        }
    }
    out.push_str("</td>");
    if view.keys || view.inbox {
        out.push_str("<td>");
        if user.superadmin {
            out.push_str(r#"<span class="muted">every</span>"#);
        } else {
            for (shown, feature) in [(view.keys, Feature::Keys), (view.inbox, Feature::Inbox)] {
                let (held, total) = feature_rights(user, view.features.as_ref(), feature);
                if shown && held > 0 {
                    out.push_str(&icon_count(
                        &format!("{}: {held} of {total} rights", feature.name()),
                        feature.icon(),
                        held,
                    ));
                }
            }
        }
        out.push_str("</td>");
    }
    if view.keys {
        let _ = write!(
            out,
            r#"<td class="r num{}">{}</td>"#,
            if user.keys == 0 { " muted" } else { "" },
            if user.superadmin {
                "-".to_string()
            } else {
                user.keys.to_string()
            }
        );
    }
    let last = match user.activity.last_login {
        Some(at) => time_tag(at, false),
        None => r#"<span class="muted">never</span>"#.to_string(),
    };
    let failed = user.activity.failed_30d;
    let _ = write!(
        out,
        r#"<td class="r num">{last}</td><td class="r num">{}</td><td class="r num{}">{failed}</td></tr>"#,
        user.activity.sessions_30d,
        if failed > 0 { " danger" } else { "" },
    );
}

/// The panel "Sign-ins": the chips All and Refused, the graph of the connections of 30 days, then
/// the newest attempts, one line each.
fn signins_zone(view: &UsersView) -> String {
    let data = view.logins;
    let refused_icon = format!(r#"<span class="st--bad">{}</span>"#, icon("ban"));
    let chip_list = chips(&[
        Chip {
            label_html: "All",
            name: "",
            count: data.total as usize,
            href: USERS_PATH,
            on: !view.only_refused,
        },
        Chip {
            label_html: &refused_icon,
            name: "Refused",
            count: data.refused as usize,
            href: &format!("{USERS_PATH}?r=failed"),
            on: view.only_refused,
        },
    ]);
    let connections: u32 = view.days.iter().map(|d| d.sessions).sum();
    let tools = format!(
        r#"<span class="count" title="Connections for each day, all accounts">{connections} in {} d</span>{}"#,
        view.days.len(),
        spark(view.days, "spark--mini"),
    );
    let mut body = String::new();
    if data.rows.is_empty() {
        let _ = write!(
            body,
            r#"<p class="empty">{}</p>"#,
            if view.only_refused {
                "No refused sign-in."
            } else {
                "No sign-in recorded yet."
            }
        );
    } else {
        body.push_str(r#"<table class="grid grid--signins"><colgroup><col class="w28"><col class="w152"><col class="w120"><col class="w120"><col></colgroup><thead class="visually-hidden"><tr><th scope="col">Result</th><th scope="col">When</th><th scope="col">Login typed</th><th scope="col">Address</th><th scope="col">Browser</th></tr></thead><tbody>"#);
        for row in &data.rows {
            login_row(&mut body, row);
        }
        body.push_str("</tbody></table>");
    }
    zone(
        &Zone {
            id: "signins-title",
            title: "Sign-ins",
            count: &data.matching.to_string(),
            chips_html: &chip_list,
            tools_html: &tools,
            class: "",
            above_html: "",
        },
        &body,
    )
}

fn login_row(out: &mut String, row: &crate::store::LoginRow) {
    let result = if row.success {
        status_icon("ok", "check", "Signed in")
    } else {
        status_icon("error", "ban", "Refused")
    };
    let typed = if row.login.is_empty() {
        r#"<span class="muted">(empty)</span>"#.to_string()
    } else {
        format!(
            r#"<code class="typed{}"{}>{}</code>"#,
            if row.known { "" } else { " muted" },
            if row.known {
                ""
            } else {
                r#" title="No such account""#
            },
            html_escape(&row.login)
        )
    };
    let _ = write!(
        out,
        r#"<tr><td>{result}</td><td>{}</td><td>{typed}</td><td class="muted">{}</td><td class="muted" title="{}">{}</td></tr>"#,
        time_tag(row.ts, false),
        opt(&row.ip),
        html_escape(&row.user_agent),
        html_escape(&short_agent(&row.user_agent)),
    );
}

/// The panel "Recent changes": the audit events.
fn changes_zone(view: &UsersView) -> String {
    let mut body = String::new();
    if view.recent_events.is_empty() {
        body.push_str(r#"<p class="empty">No change recorded yet.</p>"#);
    } else {
        body.push_str(r#"<table class="grid grid--changes"><colgroup><col class="w152"><col><col class="w180"></colgroup><thead class="visually-hidden"><tr><th scope="col">When</th><th scope="col">Change</th><th scope="col">By, address</th></tr></thead><tbody>"#);
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
                body,
                r#"<tr><td>{}</td><td>{}{account}</td><td class="muted">{by}{}</td></tr>"#,
                time_tag(event.ts, false),
                audit_label(&event.kind),
                opt(&event.ip),
            );
        }
        body.push_str("</tbody></table>");
    }
    zone(
        &Zone {
            id: "changes-title",
            title: "Recent changes",
            count: &view.recent_events.len().to_string(),
            chips_html: "",
            tools_html: "",
            class: "",
            above_html: "",
        },
        &body,
    )
}

fn audit_label(kind: &str) -> &'static str {
    match kind {
        "login_failed" => "Refused sign-in",
        "user_added" => "Account added",
        "user_deleted" => "Account deleted",
        "user_updated" => "Permissions changed",
        "password_reset" => "Password changed",
        "impersonation_started" => "Signed in as the account",
        "impersonation_ended" => "Back to superadmin",
        "sessions_ended" => "All sessions ended",
        _ => "Event",
    }
}

// One account -----------------------------------------------------------------------------------

/// The data of the page of one account.
pub struct UserView<'a> {
    pub notice: Option<Notice>,
    pub mode: Mode,
    /// The whole catalogue, not the filtered catalogue for display.
    pub catalog: &'a Catalog,
    /// The services of the catalogue with their actions. The order is the order of the catalogue.
    pub services: Vec<ServiceActions>,
    pub user: UserCard,
    /// The anti-CSRF token of the administration forms.
    pub token: &'a str,
    /// The portal features that the editor offers: the keys and the inbox. `None`: the file turns
    /// none on.
    pub features: Option<ServiceActions>,
    /// The number of keys that the account owns. The confirmation of the delete names them: they go
    /// with the account.
    pub owned_keys: usize,
}

/// The keys that go with an account: " and 3 keys". Empty without a key.
fn keys_clause(count: usize) -> String {
    match count {
        0 => String::new(),
        1 => " and 1 key".to_string(),
        n => format!(" and {n} keys"),
    }
}

pub fn user_page(frame: &Frame, view: &UserView) -> String {
    let user = &view.user;
    let editable = view.mode.editable() && !user.superadmin && !user.file;
    let mut out = String::new();
    notice_html(&mut out, view.notice);
    out.push_str(&account_bar(view, editable));
    if user.superadmin {
        out.push_str(r#"<p class="lede">Every right on every service. The configuration file sets the login and the password.</p>"#);
        return fill_page(Tab::Users, frame, &user.login, &out);
    }
    if editable {
        let _ = write!(
            out,
            r#"<form id="grants" method="post" action="/auth/users/update"><input type="hidden" name="csrf" value="{}"><input type="hidden" name="login" value="{}"><input type="hidden" name="grants" value="1"></form>"#,
            html_escape(view.token),
            html_escape(&user.login),
        );
    }
    if view.services.is_empty() && view.features.is_none() {
        out.push_str(r#"<p class="empty">No service in the catalogue yet. A service appears when a container or an IngressRoute has <code>portal.*</code> labels.</p>"#);
    } else {
        // The editor shows every grant that the account holds, also a grant that the document of
        // the service does not name now. The grants of the configuration file are locked.
        let fixed = |g: &Grant| !editable || user.configured.iter().any(|c| c.same_node(g));
        let stored: Vec<Grant> = user.grants.iter().filter(|g| !fixed(g)).cloned().collect();
        let mut data = grant_editor::build(
            &view.services,
            view.features.as_ref(),
            None,
            |_| true,
            stored,
            true,
        );
        data.locked = user.grants.iter().filter(|g| fixed(g)).cloned().collect();
        for grant in data.locked.clone() {
            data.keep(&grant);
        }
        grant_editor::render(
            &mut out,
            &data,
            Place {
                form: editable.then_some("grants"),
                title: "Permissions",
                blank_rows: 0,
                groups: true,
            },
        );
    }
    fill_page(Tab::Users, frame, &user.login, &out)
}

/// The one row of tools of an account: the way back, the login, log in as, the password, the
/// activity, delete, two facts, then Save. A control that does not apply is absent.
fn account_bar(view: &UserView, editable: bool) -> String {
    let user = &view.user;
    let login = html_escape(&user.login);
    let token = html_escape(view.token);
    let mut out = format!(
        r##"<div class="accountBar"><nav class="crumb" aria-label="Breadcrumb"><a href="{USERS_PATH}">Users</a>{}</nav><h2 class="name" aria-current="page">{login}</h2>"##,
        icon("chevron-right")
    );
    if user.superadmin {
        out.push_str(r#"<span class="badge">Superadmin</span>"#);
    }
    if user.file {
        out.push_str(r#"<span class="badge">file</span>"#);
    }
    if !user.superadmin {
        let s = summary(view.catalog, &view.services, user);
        if s.to_check > 0 {
            out.push_str(&status_icon(
                "warn",
                "alert-triangle",
                &plural(s.to_check, "permission to check", "permissions to check"),
            ));
        }
    }
    let can_manage = !user.superadmin && !user.file;
    if can_manage {
        out.push_str(&icon_post(
            "/auth/users/impersonate",
            &format!("Log in as {}", user.login),
            "user-share",
            &format!(
                r#"<input type="hidden" name="csrf" value="{token}"><input type="hidden" name="login" value="{login}">"#
            ),
        ));
        out.push_str(&popover(
            "Change the password",
            "key",
            Side::Start,
            false,
            &password_form(&user.login, view.token),
        ));
    }
    out.push_str(&popover(
        "Activity of the account",
        "activity",
        Side::Start,
        true,
        &activity_body(view),
    ));
    if can_manage {
        let keys = keys_clause(view.owned_keys);
        out.push_str(&ask(&Ask {
            label: &format!("Delete {}", user.login),
            icon: "trash",
            question: &format!("Delete {}{keys}?", user.login),
            verb: "Delete",
            action: "/auth/users/delete",
            fields_html: &format!(
                r#"<input type="hidden" name="csrf" value="{token}"><input type="hidden" name="login" value="{login}">"#
            ),
            // An account with keys takes them with it: the person types the login first.
            strength: if view.owned_keys > 0 {
                Strength::Firm
            } else {
                Strength::Light
            },
            name: &user.login,
            danger: true,
            side: Side::Start,
        }));
    }
    let created = user
        .created_at
        .map(|at| time_tag(at, false))
        .unwrap_or_else(|| "in the file".to_string());
    let last = user
        .activity
        .last_login
        .map(|at| time_tag(at, false))
        .unwrap_or_else(|| "never".to_string());
    let _ = write!(
        out,
        r#"<dl class="factLine"><div><dt>Created</dt><dd>{created}</dd></div><div><dt>Last sign-in</dt><dd>{last}</dd></div></dl><span class="grow"></span>"#
    );
    if editable && (!view.services.is_empty() || view.features.is_some()) {
        out.push_str(
            r#"<button class="button button--primary" type="submit" form="grants" data-save>Save</button>"#,
        );
    }
    out.push_str("</div>");
    out
}

/// The password form of the popover. It holds the field and "Generate": the page has none.
fn password_form(login: &str, token: &str) -> String {
    let id = html_escape(&format!("pw-{login}"));
    format!(
        r#"<form class="pwForm" method="post" action="/auth/users/update"><input type="hidden" name="csrf" value="{}"><input type="hidden" name="login" value="{}">{}<button class="button" type="submit" title="The sessions of this account end at once">Change</button></form>"#,
        html_escape(token),
        html_escape(login),
        password_input(&id, "New password", "At least 10 characters"),
    )
}

/// A password field with its "Generate" button (script). The button shows the result of the
/// action in its own slot: a second icon that takes the place of the first at the same size, so
/// nothing moves (the live region for a screen reader takes no room).
fn password_input(id: &str, label: &str, hint: &str) -> String {
    format!(
        r##"<input class="uiInput" id="{id}" name="password" type="password" minlength="10" required autocomplete="new-password" autocapitalize="off" spellcheck="false" placeholder="{hint}" aria-label="{label}"><button class="ib generate" type="button" data-generate-password="{id}" aria-label="Generate a password" title="Generate a password"><span class="generate__idle">{}</span><span class="generate__done" aria-hidden="true">{}</span></button><span class="visually-hidden" id="{id}-status" role="status"></span>"##,
        icon("refresh"),
        icon("check"),
    )
}

/// What the account did: the figures, the graph, the latest sessions and the services used. It
/// opens in the popover of the page of the account.
fn activity_body(view: &UserView) -> String {
    let activity = &view.user.activity;
    let connections: u32 = activity.days.iter().map(|d| d.sessions).sum();
    let mut out = format!(
        r#"<div class="kvs"><span>Sessions, 30 d</span><b class="num">{}</b><span>Refused, 30 d</span><b class="num{}">{}</b><span>Connections</span><span class="num">{connections} in {} d</span>{}</div>"#,
        activity.sessions_30d,
        if activity.failed_30d > 0 {
            " danger"
        } else {
            ""
        },
        activity.failed_30d,
        activity.days.len(),
        spark(&activity.days, "spark--mini"),
    );
    if activity.recent.is_empty() {
        out.push_str(r#"<p class="empty">No session recorded.</p>"#);
    } else {
        out.push_str(r#"<table class="grid grid--popup"><colgroup><col class="w152"><col class="w120"><col><col class="w96"><col class="w108"></colgroup><thead><tr><th scope="col">Started</th><th scope="col">Address</th><th scope="col">Browser</th><th scope="col">Duration</th><th scope="col">End</th></tr></thead><tbody>"#);
        for session in &activity.recent {
            session_row(&mut out, session);
        }
        out.push_str("</tbody></table>");
    }
    if !activity.services.is_empty() {
        out.push_str(r#"<table class="grid grid--popup"><colgroup><col><col class="w96"><col class="w152"></colgroup><thead><tr><th scope="col">Service used</th><th scope="col" class="r">Requests</th><th scope="col" class="r">Last use</th></tr></thead><tbody>"#);
        for used in &activity.services {
            service_use_row(&mut out, view.catalog, used);
        }
        out.push_str("</tbody></table>");
    }
    out
}

fn session_row(out: &mut String, session: &SessionRow) {
    let (duration, ended) = match session.ended_at {
        Some(end) => (
            format_duration(end - session.started_at),
            end_label(session.end_reason.as_deref()).to_string(),
        ),
        None => (
            format_duration(session.last_seen_at - session.started_at),
            status_icon("ok", "check", "Active"),
        ),
    };
    let _ = write!(
        out,
        r#"<tr><td>{}{}</td><td>{}</td><td title="{}">{}</td><td>{}</td><td>{}</td></tr>"#,
        time_tag(session.started_at, false),
        if session.impersonated {
            r#" <span class="badge" title="Opened by the superadmin">su</span>"#
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
        r#"<tr><td>{}</td><td class="r num">{}</td><td class="r">{}</td></tr>"#,
        html_escape(name),
        used.hits,
        time_tag(used.last_at, false)
    );
}

fn end_label(reason: Option<&str>) -> &'static str {
    match reason {
        Some("logout") => "Signed out",
        Some("expired") => "Expired",
        Some("idle") => "Idle timeout",
        Some("revoked") => "Revoked",
        Some("restart") => "Restarted",
        Some("user_deleted") => "Deleted",
        Some("impersonation") => "Switched",
        _ => "Ended",
    }
}

// A new account ---------------------------------------------------------------------------------

/// The data of the page "New user".
pub struct NewUserView<'a> {
    pub notice: Option<Notice>,
    pub services: Vec<ServiceActions>,
    pub features: Option<ServiceActions>,
    /// The anti-CSRF token of the administration forms.
    pub token: &'a str,
}

/// The page of an account in create mode. The login and the password stand in the row of tools.
/// The editor is under it. One primary action makes the account with its grants.
pub fn new_user_page(frame: &Frame, view: &NewUserView) -> String {
    let mut out = String::new();
    notice_html(&mut out, view.notice);
    let _ = write!(
        out,
        r##"<form class="accountBar" id="add-user" data-gate method="post" action="/auth/users/add"><input type="hidden" name="csrf" value="{}"><input type="hidden" name="grants" value="1"><nav class="crumb" aria-label="Breadcrumb"><a href="{USERS_PATH}">Users</a>{}</nav><h2 class="name" aria-current="page">New user</h2><input class="uiInput accountBar__login" id="new-login" name="login" type="text" maxlength="32" pattern="[A-Za-z0-9._\-]{{1,32}}" required autocomplete="off" autocapitalize="off" spellcheck="false" placeholder="Login" aria-label="Login" title="1 to 32 characters: letters, digits, dot, dash, underscore"><span class="pwRow">{}</span><span class="grow"></span><button class="button button--primary" type="submit">Create</button></form>"##,
        html_escape(view.token),
        icon("chevron-right"),
        password_input(
            "new-password",
            "Password",
            "Password, at least 10 characters"
        ),
    );
    if view.services.is_empty() && view.features.is_none() {
        out.push_str(r#"<p class="empty">No service in the catalogue yet. A service appears when a container or an IngressRoute has <code>portal.*</code> labels.</p>"#);
    } else {
        let data = grant_editor::build(
            &view.services,
            view.features.as_ref(),
            None,
            |_| true,
            Vec::new(),
            true,
        );
        grant_editor::render(
            &mut out,
            &data,
            Place {
                form: Some("add-user"),
                title: "Permissions",
                blank_rows: 0,
                groups: true,
            },
        );
    }
    fill_page(Tab::Users, frame, "New user", &out)
}

// Services --------------------------------------------------------------------------------------

/// An account as the Services page sees it: its login, its grants, and its source.
#[derive(Clone, Debug)]
pub struct Holder {
    pub login: String,
    pub grants: Vec<Grant>,
    /// The account comes from the configuration file: the pages do not change its grants.
    pub file: bool,
}

pub struct ServicesView<'a> {
    pub catalog: &'a Catalog,
    /// The actions of each service.
    pub services: &'a [ServiceActions],
    /// Every account with the grants that apply to it.
    pub holders: &'a [Holder],
    /// The services whose actions document the code cannot read: `(service id, problem)`.
    pub problems: Vec<(String, String)>,
    /// Where the grants come from. Only a page that can change grants offers "Give access".
    pub mode: Mode,
    /// The service that the query selects. `None`, or an id that the catalogue does not hold: the
    /// first service.
    pub selected: Option<String>,
    /// The chip "to check" is on.
    pub only_check: bool,
}

impl ServicesView<'_> {
    fn actions_of(&self, id: &str) -> Option<&ServiceActions> {
        self.services.iter().find(|s| s.id == id)
    }

    /// The service has a grant to check, or its document cannot be read.
    fn needs_check(&self, service: &Service) -> bool {
        self.problems.iter().any(|(id, _)| *id == service.id)
            || self.holders.iter().any(|h| {
                self.actions_of(&service.id).is_some_and(|s| {
                    h.grants
                        .iter()
                        .any(|g| g.service == service.id && s.undeclared(g))
                })
            })
    }

    /// The id of the selected service: the one that the query names, or the first.
    fn selected_id(&self) -> Option<&str> {
        let all = || self.catalog.groups().iter().flat_map(|g| g.services.iter());
        self.selected
            .as_deref()
            .and_then(|asked| all().find(|s| s.id == asked))
            .or_else(|| all().next())
            .map(|s| s.id.as_str())
    }
}

pub fn services_page(frame: &Frame, view: &ServicesView) -> String {
    let mut out = String::new();
    let count = view.catalog.service_count();
    if count == 0 {
        out.push_str(&toolbar("Services", "", ""));
        out.push_str(r#"<p class="empty">No service in the catalogue yet. A service appears when a container or an IngressRoute has <code>portal.*</code> labels.</p>"#);
        return fill_page(Tab::Services, frame, "Services", &out);
    }
    let selected = view.selected_id();
    let to_check = view
        .catalog
        .groups()
        .iter()
        .flat_map(|g| g.services.iter())
        .filter(|s| view.needs_check(s))
        .count();
    let check_icon = format!(
        r#"<span class="st--warn">{}</span>"#,
        icon("alert-triangle")
    );
    let chip_list = chips(&[
        Chip {
            label_html: "All",
            name: "",
            count,
            href: &services_href(selected, false),
            on: !view.only_check,
        },
        Chip {
            label_html: &check_icon,
            name: "Services to check",
            count: to_check,
            href: &services_href(selected, true),
            on: view.only_check,
        },
    ]);
    let mut table = String::from(
        r#"<table class="grid grid--services"><colgroup><col><col class="w160"><col class="w64"><col class="w64"><col class="w56"><col class="w80"></colgroup><thead><tr><th scope="col">Service</th><th scope="col">Path</th><th scope="col" class="r" title="Accounts that open the service">Opened</th><th scope="col" class="r">Actions</th><th scope="col" class="r">Types</th><th scope="col" class="r">Resources</th></tr></thead>"#,
    );
    let mut index = 0;
    for group in view.catalog.groups() {
        let rows: Vec<&Service> = group
            .services
            .iter()
            .filter(|s| !view.only_check || view.needs_check(s))
            .collect();
        if rows.is_empty() {
            continue;
        }
        let _ = write!(
            table,
            r#"<tbody><tr class="grid__group"><th scope="rowgroup" colspan="6">{}</th></tr>"#,
            html_escape(&group.name)
        );
        for service in rows {
            index += 1;
            service_row(
                &mut table,
                view,
                service,
                index,
                selected == Some(&service.id),
            );
        }
        table.push_str("</tbody>");
    }
    table.push_str("</table>");
    let left = zone(
        &Zone {
            id: "services-title",
            title: "Services",
            count: "",
            chips_html: &chip_list,
            tools_html: "",
            class: "",
            above_html: "",
        },
        &table,
    );
    let right = format!("{}{}", opened_by_zone(view, selected), checks_zone(view));
    out.push_str(&split("services", &left, &right));
    fill_page(Tab::Services, frame, "Services", &out)
}

/// One service of the Services page, on one table row of one line. The name is a link: it selects
/// the row, and the page shows who opens it.
fn service_row(
    out: &mut String,
    view: &ServicesView,
    service: &Service,
    index: usize,
    selected: bool,
) {
    let problem = view.problems.iter().find(|(id, _)| *id == service.id);
    let found = view.actions_of(&service.id);
    let _ = write!(
        out,
        r#"<tr id="row-{index}"{}><th scope="row"><a class="rowLink" href="{}#row-{index}"{}>{}</a> <code class="muted">{}</code>"#,
        if selected {
            r#" class="is-selected""#
        } else {
            ""
        },
        html_escape(&services_href(Some(&service.id), view.only_check)),
        if selected {
            r#" aria-current="true""#
        } else {
            ""
        },
        html_escape(&service.name),
        html_escape(&service.id),
    );
    if let Some((_, text)) = problem {
        let message = format!(
            "The actions of {} cannot be read: {text}. The page keeps the last good list.",
            service.name
        );
        out.push_str(&status_icon(
            "warn",
            "alert-triangle",
            &html_escape(&message),
        ));
    }
    let _ = write!(
        out,
        r#"</th><td class="muted">{}"#,
        html_escape(&service.href)
    );
    if !service.exclude.is_empty() {
        let rules: Vec<String> = service
            .exclude
            .iter()
            .map(|rule| rule.text().to_string())
            .collect();
        let _ = write!(
            out,
            r#" <span class="badge" title="Open without sign-in: {}">{} open</span>"#,
            html_escape(&rules.join(", ")),
            rules.len()
        );
    }
    let opened = view
        .holders
        .iter()
        .filter(|h| holds(&h.grants, ACCESS, &service.id))
        .count();
    let figure = |n: usize| {
        if n == 0 {
            r#"<span class="muted">-</span>"#.to_string()
        } else {
            n.to_string()
        }
    };
    let _ = write!(
        out,
        r#"</td><td class="r num">{opened}</td><td class="r num">{}</td><td class="r num">{}</td><td class="r num">{}</td></tr>"#,
        figure(found.map_or(0, |f| f.actions.len())),
        figure(found.map_or(0, |f| f.document.types.len())),
        figure(found.map_or(0, |f| f.document.resources.len())),
    );
}

/// The panel "Opened by": the accounts that open the selected service, then the accounts that do
/// not, each with a link that gives access.
fn opened_by_zone(view: &ServicesView, selected: Option<&str>) -> String {
    let Some(id) = selected else {
        return String::new();
    };
    let found = view.actions_of(id);
    let name = view
        .catalog
        .groups()
        .iter()
        .flat_map(|g| g.services.iter())
        .find(|s| s.id == id)
        .map_or(id, |s| s.name.as_str());
    let (with, without): (Vec<&Holder>, Vec<&Holder>) = view
        .holders
        .iter()
        .partition(|h| holds(&h.grants, ACCESS, id));
    let mut body = String::from(
        r#"<table class="grid grid--holders"><colgroup><col><col class="w88"><col class="w88"></colgroup><tbody>"#,
    );
    if with.is_empty() {
        body.push_str(r#"<tr><td colspan="3" class="muted">Superadmin only</td></tr>"#);
    }
    for holder in &with {
        let mine: Vec<&Grant> = holder.grants.iter().filter(|g| g.service == id).collect();
        let check = found.map_or(0, |s| mine.iter().filter(|g| s.undeclared(g)).count());
        let _ = write!(
            body,
            r#"<tr><td><a href="{}">{}</a>{}</td><td class="r muted">{}</td><td class="r">{}</td></tr>"#,
            html_escape(&user_path(&holder.login)),
            html_escape(&holder.login),
            if holder.file {
                r#" <span class="badge">file</span>"#
            } else {
                ""
            },
            plural(mine.len(), "grant", "grants"),
            if check > 0 {
                format!(r#"<span class="warnText">{check} to check</span>"#)
            } else {
                String::new()
            },
        );
    }
    let can_give = view.mode.editable();
    let addable: Vec<&&Holder> = without.iter().filter(|h| !h.file).collect();
    if can_give && !addable.is_empty() {
        let _ = write!(
            body,
            r#"<tr class="grid__group"><th scope="rowgroup" colspan="3">Not opened</th></tr>"#
        );
        for holder in addable {
            let _ = write!(
                body,
                r#"<tr><td><a href="{}">{}</a></td><td></td><td class="r">{}</td></tr>"#,
                html_escape(&user_path(&holder.login)),
                html_escape(&holder.login),
                icon_link(
                    &format!(
                        "{GIVE_PATH}?user={}&service={}&give=1",
                        percent_encode_query_component(&holder.login),
                        percent_encode_query_component(id)
                    ),
                    &format!("Give {} access to {name}", holder.login),
                    "plus"
                ),
            );
        }
    }
    body.push_str("</tbody></table>");
    zone(
        &Zone {
            id: "opened-title",
            title: "Opened by",
            count: name,
            chips_html: "",
            tools_html: "",
            class: " zone--short",
            above_html: "",
        },
        &body,
    )
}

/// The grants below a service that do not follow its document: the action does not list the
/// type, or the path does not follow the parents.
/// At start, the server does not know the services: the catalogue and the actions documents come
/// at run time. Thus a grant of the configuration can name a type that the service does not
/// declare. This panel shows each such grant with its problem and a link to its account. The
/// panel is absent when there is none.
fn checks_zone(view: &ServicesView) -> String {
    let mut rows = String::new();
    let mut count = 0;
    for holder in view.holders {
        for grant in &holder.grants {
            let Some(problem) = view
                .services
                .iter()
                .filter(|s| s.id == grant.service)
                .find_map(|s| s.problem(grant))
            else {
                continue;
            };
            count += 1;
            let text = html_escape(&grant.to_string());
            let _ = write!(
                rows,
                r#"<tr><td><a href="{}">{}</a></td><td title="{text}"><code>{text}</code></td><td class="muted" title="{}">{}</td></tr>"#,
                html_escape(&user_path(&holder.login)),
                html_escape(&holder.login),
                html_escape(&problem),
                html_escape(&problem),
            );
        }
    }
    if rows.is_empty() {
        return String::new();
    }
    zone(
        &Zone {
            id: "checks-title",
            title: "To check",
            count: &count.to_string(),
            chips_html: "",
            tools_html: "",
            class: "",
            above_html: "",
        },
        &format!(
            r#"<table class="grid grid--checks"><colgroup><col class="w88"><col><col class="w180"></colgroup><thead><tr><th scope="col">Account</th><th scope="col">Grant</th><th scope="col">Problem</th></tr></thead><tbody>{rows}</tbody></table>"#
        ),
    )
}

// The graph -------------------------------------------------------------------------------------

/// Bars, one for each day, newest on the right. This code draws three paths, one path for each
/// kind of bar. It does not draw one element for each day: a graph costs the browser four nodes, not
/// sixty. It uses attributes only, and no inline style. The values are in the text next to it, not
/// in tooltips.
fn spark(days: &[DayStat], class: &str) -> String {
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
        r#"<svg class="spark {class}" viewBox="0 0 {width} {HEIGHT}" preserveAspectRatio="none" role="img" aria-label="Connections per day over the last {} days: {total} in total">"#,
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

    /// The data that the page gives to the editor of grants.
    fn editor_of(html: &str) -> serde_json::Value {
        let start = html.find(r#"class="grantEditor__data">"#).unwrap()
            + r#"class="grantEditor__data">"#.len();
        let end = start + html[start..].find("</script>").unwrap();
        serde_json::from_str(&html[start..end]).unwrap()
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
            file: false,
            keys: 0,
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
            keys: false,
            inbox: false,
            superadmin: true,
        }
    }

    /// The four tabs: the file has the blocks `keys` and `inbox`.
    fn full_frame() -> Frame<'static> {
        Frame {
            keys: true,
            inbox: true,
            ..frame()
        }
    }

    /// The document of Grafana in the tests: a folder, and the dashboards of a folder.
    const GRAFANA: &str = r#"{"server":"infra-grafana","resource_types":[{"name":"folder","parent":"service","description":"A folder."},{"name":"dashboard","parent":"folder","description":"The type dashboard."}],"actions":[{"name":"dashboards.read","resource_types":["service","folder"],"description":"See the dashboards."},{"name":"dashboards.write","resource_types":["service","folder","dashboard"],"description":"Change the <dashboards>."}]}"#;

    fn services() -> Vec<ServiceActions> {
        let plain = |id: &str, name: &str| ServiceActions {
            id: id.into(),
            name: name.into(),
            actions: Vec::new(),
            document: Default::default(),
            known: true,
            fetch: false,
            critical: Vec::new(),
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
                fetch: false,
                critical: Vec::new(),
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

    /// The portal features with the keys and the inbox: the real document of the code.
    fn features() -> ServiceActions {
        use crate::keys::KeyKind;
        use crate::portal_actions::{document, Features};
        let document = document(&Features {
            inbox_groups: Some(vec![("infra".into(), "Infra".into())]),
            kinds: vec![KeyKind::Api, KeyKind::Mcp],
        });
        ServiceActions {
            id: PORTAL.into(),
            name: "Portal features".into(),
            actions: document
                .actions
                .iter()
                .map(|a| (a.name.clone(), a.description.clone()))
                .collect(),
            document,
            known: true,
            fetch: false,
            critical: Vec::new(),
        }
    }

    struct Data {
        logins: LoginPage,
        days: Vec<DayStat>,
        events: Vec<AuditRow>,
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

    fn data() -> Data {
        let mut days = vec![DayStat::default(); 30];
        days[29] = DayStat {
            sessions: 4,
            seconds: 10,
        };
        Data {
            logins: LoginPage {
                rows: vec![
                    login_row_of("alice", true, true),
                    login_row_of("<b>x</b>", false, false),
                ],
                matching: 2,
                total: 12,
                refused: 3,
                ..LoginPage::default()
            },
            days,
            events: vec![AuditRow {
                ts: 1_791_623_000,
                kind: "user_updated".into(),
                login: Some("alice".into()),
                actor: Some("root".into()),
                ip: Some("192.0.2.10".into()),
            }],
        }
    }

    fn view<'a>(catalog: &'a Catalog, data: &'a Data) -> UsersView<'a> {
        UsersView {
            notice: None,
            allow_creation: true,
            mode: Mode::Dynamic,
            catalog,
            services: services(),
            users: vec![
                card("alice", &["grafana", "mixed"]),
                card("bob", &["mixed"]),
                card("carol", &[]),
            ],
            features: Some(features()),
            keys: true,
            inbox: true,
            max_users: 64,
            token: "tok<en",
            logins: &data.logins,
            only_refused: false,
            days: &data.days,
            recent_events: &data.events,
        }
    }

    fn user_view(catalog: &Catalog, user: UserCard) -> UserView<'_> {
        UserView {
            notice: None,
            mode: Mode::Dynamic,
            catalog,
            services: services(),
            user,
            token: "tok<en",
            features: Some(features()),
            owned_keys: 0,
        }
    }

    fn holder(login: &str, grants: Vec<Grant>) -> Holder {
        Holder {
            login: login.into(),
            grants,
            file: false,
        }
    }

    fn services_view<'a>(
        catalog: &'a Catalog,
        actions: &'a [ServiceActions],
        holders: &'a [Holder],
    ) -> ServicesView<'a> {
        ServicesView {
            catalog,
            services: actions,
            holders,
            problems: Vec::new(),
            mode: Mode::Dynamic,
            selected: None,
            only_check: false,
        }
    }

    fn main_of(html: &str) -> &str {
        &html[html.find("<main").unwrap()..html.find("</main>").unwrap()]
    }

    #[test]
    fn every_page_is_a_whole_document_with_the_same_header_skip_link_and_tabs() {
        let cat = catalog();
        let actions = services();
        let d = data();
        let holders = vec![holder("alice", access(&["grafana"]))];
        let pages = [
            (Tab::Users, users_page(&full_frame(), &view(&cat, &d))),
            (
                Tab::Users,
                user_page(&full_frame(), &user_view(&cat, card("alice", &["grafana"]))),
            ),
            (
                Tab::Users,
                new_user_page(
                    &full_frame(),
                    &NewUserView {
                        notice: None,
                        services: services(),
                        features: Some(features()),
                        token: "tok<en",
                    },
                ),
            ),
            (
                Tab::Services,
                services_page(&full_frame(), &services_view(&cat, &actions, &holders)),
            ),
        ];
        for (tab, html) in &pages {
            assert!(html.starts_with("<!doctype html>"), "{tab:?}");
            assert!(html.contains(&format!(
                r#"<link rel="stylesheet" href="/auth/settings.css?v={}">"#,
                asset_version()
            )));
            assert!(html.contains(r##"<a class="skipLink" href="#main">Skip to content</a>"##));
            assert!(html.contains(r#"<body class="shell">"#));
            assert!(html.contains(r#"<main class="settings settings--fill""#));
            assert!(html.contains(r##"<a class="backLink" href="/">"##));
            assert!(html.contains("Back to panel"));
            assert!(html.contains(r#"action="/auth/logout""#) && html.contains("out&lt;"));
            let nav = html.split("<main").next().unwrap();
            assert_eq!(
                nav.matches(r#"class="contentTabs__tab"#).count(),
                4,
                "four tabs"
            );
            assert_eq!(html.matches(r#"is-active" aria-current="page""#).count(), 1);
            assert!(html.contains(&format!(
                r#"<a class="contentTabs__tab is-active" aria-current="page" href="{}">{}</a>"#,
                tab.path(),
                tab.label()
            )));
            assert!(!html.contains("Activity</a>") && !html.contains("/auth/settings/logins"));
            assert!(!html.contains(r#"class="modal"#), "no popup");
            assert!(!html.contains(" style="), "no inline style (CSP)");
            assert!(!html.contains('\u{2014}') && !html.contains('\u{2013}'));
            assert_eq!(html.matches("/auth/assets/settings.js").count(), 1);
        }
    }

    #[test]
    fn the_tabs_follow_the_configuration_and_there_are_four_at_most() {
        let tabs = |superadmin, keys, inbox| {
            Frame {
                logout_token: "t",
                keys,
                inbox,
                superadmin,
            }
            .tabs()
        };
        // All on: four tabs, no Activity.
        assert_eq!(
            tabs(true, true, true),
            [Tab::Users, Tab::Services, Tab::Keys, Tab::Inbox]
        );
        // Panel only: no keys and no inbox, two tabs.
        assert_eq!(tabs(true, false, false), [Tab::Users, Tab::Services]);
        assert_eq!(
            tabs(true, true, false),
            [Tab::Users, Tab::Services, Tab::Keys]
        );
        assert_eq!(
            tabs(true, false, true),
            [Tab::Users, Tab::Services, Tab::Inbox]
        );
        assert_eq!(tabs(false, true, true), [Tab::Keys, Tab::Inbox]);
        assert_eq!(tabs(false, false, true), [Tab::Inbox]);
        assert_eq!(tabs(false, true, false), [Tab::Keys]);
        assert!(tabs(false, false, false).is_empty());
        for (keys, inbox) in [(false, false), (true, false), (false, true), (true, true)] {
            assert!(tabs(true, keys, inbox).len() <= 4);
        }
        // A page draws the links of the tabs of its frame, and no grey tab.
        let cat = catalog();
        let d = data();
        let html = users_page(&frame(), &view(&cat, &d));
        let nav = html.split("<main").next().unwrap();
        assert_eq!(nav.matches(r#"class="contentTabs__tab"#).count(), 2);
        assert!(!nav.contains("/auth/keys") && !nav.contains("/auth/settings/inbox"));
        assert!(!nav.contains("disabled") && !nav.contains("aria-disabled"));
        let keys_only = Frame {
            keys: true,
            ..frame()
        };
        let html = users_page(&keys_only, &view(&cat, &d));
        let nav = html.split("<main").next().unwrap();
        assert!(nav.contains(r#"href="/auth/keys">Keys</a>"#));
        assert!(!nav.contains("/auth/settings/inbox"));
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
        let d = data();
        let users = users_page(&frame(), &view(&cat, &d));
        assert!(!users.contains(r#"<symbol id="i-user-share""#));
        assert!(users.contains(r#"<symbol id="i-plus""#));
        let alice = user_page(&frame(), &user_view(&cat, card("alice", &["grafana"])));
        assert!(alice.contains(r#"<symbol id="i-user-share""#));
        assert!(alice.contains(r#"<symbol id="i-key""#));
        assert!(!alice.contains(r#"<symbol id="i-plus""#));
        assert_eq!(
            icon_ids(r##"<use href="#i-a"/><use href="#i-b"/><use href="#i-a"/>"##),
            ["i-a", "i-b"]
        );
    }

    #[test]
    fn the_users_page_is_a_table_of_accounts_that_link_to_their_own_page() {
        let cat = catalog();
        let d = data();
        let mut v = view(&cat, &d);
        v.users[1]
            .grants
            .push(sub("dashboards.read on service:infra-grafana/team:x"));
        v.users[0].activity.failed_30d = 2;
        v.users[0].keys = 3;
        v.users[0].grants.extend([
            grant("keys.own.read", PORTAL),
            grant("keys.own.edit", PORTAL),
            grant("inbox.receive", PORTAL),
        ]);
        let html = users_page(&full_frame(), &v);
        for login in ["alice", "bob", "carol"] {
            assert!(
                html.contains(&format!(
                    r#"<a class="rowLink" href="/auth/settings/users/{login}">{login}</a>"#
                )),
                "{login}"
            );
        }
        assert!(
            !html.contains("Superadmin</span>"),
            "the superadmin is not a row of the accounts table"
        );
        assert!(
            html.contains("2 of 4"),
            "alice opens two of the four services"
        );
        assert!(html.contains(r#"<span class="muted">none</span>"#), "carol");
        assert!(
            html.contains("permission to check"),
            "bob holds a grant that Grafana does not declare"
        );
        // The features are icons with counts and a name.
        assert!(
            html.contains(r#"role="img" aria-label="Keys: 2 of 8 rights""#),
            "{html}"
        );
        assert!(html.contains(r#"aria-label="Inbox: 1 of 6 rights""#));
        assert!(
            html.contains(r#"<td class="r num">3</td>"#),
            "the keys of alice"
        );
        assert!(html.contains(r#"<td class="r num danger">2</td>"#));
        assert!(html.contains(">3 of 64<"));
        assert!(
            !html.contains(r#"action="/auth/users/update""#),
            "no editor on the list"
        );
        assert!(!html.contains("Access</th>") && !html.contains("accountLink"));
        // A file account has its badge and its page.
        v.users.push(UserCard {
            file: true,
            ..card("dave", &["notes"])
        });
        let html = users_page(&full_frame(), &v);
        assert!(html.contains(r#"<a class="rowLink" href="/auth/settings/users/dave">dave</a> <span class="badge">file</span>"#));
        assert!(
            html.contains(">3 of 64<"),
            "the limit counts the database only"
        );
    }

    #[test]
    fn the_columns_of_a_feature_that_is_off_are_absent_from_the_dom() {
        let cat = catalog();
        let d = data();
        // Panel only: the file has neither the block keys nor the block inbox.
        let mut v = view(&cat, &d);
        v.keys = false;
        v.inbox = false;
        v.features = None;
        v.allow_creation = false;
        let html = users_page(&frame(), &v);
        for gone in [
            "Features",
            "Keys</th>",
            r#"class="feat""#,
            "New user",
            NEW_USER_PATH,
        ] {
            assert!(!html.contains(gone), "{gone}");
        }
        assert!(html.contains("Last sign-in") && html.contains("Services</th>"));
        // Keys on, inbox off: the column of the keys, one icon.
        let mut v = view(&cat, &d);
        v.inbox = false;
        v.users[0].grants.push(grant("keys.own.read", PORTAL));
        v.users[0].grants.push(grant("inbox.receive", PORTAL));
        let html = users_page(&frame(), &v);
        assert!(html.contains("Keys</th>") && html.contains("aria-label=\"Keys: 1 of 8 rights\""));
        assert!(!html.contains("Inbox: "), "the inbox is off");
    }

    #[test]
    fn new_user_shows_only_when_the_file_allows_it_and_the_grants_are_not_read_only() {
        let cat = catalog();
        let d = data();
        let on = users_page(&frame(), &view(&cat, &d));
        assert!(on.contains(&format!(r#"href="{NEW_USER_PATH}">"#)));
        assert!(on.contains("New user</a>"));
        // The form that made accounts is not in the page any more.
        for gone in [
            r#"id="add-user""#,
            r#"action="/auth/users/add""#,
            "Create user",
            "add=1",
        ] {
            assert!(!on.contains(gone), "{gone}");
        }
        let mut off = view(&cat, &d);
        off.allow_creation = false;
        let html = users_page(&frame(), &off);
        assert!(!html.contains("New user") && !html.contains(NEW_USER_PATH));
        let mut fixed = view(&cat, &d);
        fixed.mode = Mode::Static;
        let html = users_page(&frame(), &fixed);
        assert!(
            !html.contains("New user") && !html.contains(NEW_USER_PATH),
            "static mode"
        );
        let mut hybrid = view(&cat, &d);
        hybrid.mode = Mode::Hybrid;
        assert!(users_page(&frame(), &hybrid).contains("New user</a>"));
    }

    #[test]
    fn the_sign_ins_panel_has_chips_that_are_links_a_graph_and_one_line_for_each_attempt() {
        let cat = catalog();
        let d = data();
        let html = users_page(&frame(), &view(&cat, &d));
        let panel = &html[html.find(r#"aria-labelledby="signins-title""#).unwrap()..];
        let panel = &panel[..panel.find("</section>").unwrap()];
        // The chips filter with a query parameter, so they work without script.
        assert!(panel.contains(r#"<a class="chip is-on" href="/auth/settings" aria-current="true">All<span class="num">12</span></a>"#), "{panel}");
        assert!(panel.contains(r#"href="/auth/settings?r=failed""#));
        assert!(panel.contains(r#"aria-label="Refused 3" title="Refused""#));
        assert!(!panel.contains("<button"), "no script needed to filter");
        // The graph has a name and its figure is in the text.
        assert!(panel.contains(r#"<svg class="spark spark--mini""#));
        assert!(panel.contains("4 in 30 d"));
        // One line for each attempt: the state is an icon with a name, the time is a local time.
        assert_eq!(
            panel.matches("<tr>").count(),
            2 + 1,
            "two attempts and the hidden head"
        );
        assert!(panel.contains(r#"<span class="visually-hidden">Signed in</span>"#));
        assert!(panel.contains(r#"title="Refused""#));
        assert!(panel.contains(r#"<time class="when" datetime="#));
        assert!(panel.contains("203.0.113.7") && panel.contains("Firefox 131"));
        // What was typed is text.
        assert!(panel.contains("&lt;b&gt;x&lt;/b&gt;") && !panel.contains("<b>x</b>"));
        assert!(panel.contains("No such account"));
        // The filter is on: the chip of the refused attempts is the current one.
        let mut v = view(&cat, &d);
        v.only_refused = true;
        let html = users_page(&frame(), &v);
        assert!(html.contains(
            r#"<a class="chip is-on" href="/auth/settings?r=failed" aria-current="true""#
        ));
        assert!(html.contains(r#"<a class="chip" href="/auth/settings">All"#));
        // A chip with a count of zero is absent (CTRL-15).
        let mut none = data();
        none.logins.refused = 0;
        let html = users_page(&frame(), &view(&cat, &none));
        assert!(!html.contains("r=failed"));
        // No pages: no Newer, no Older.
        assert!(!html.contains("Newer") && !html.contains("Older"));
    }

    #[test]
    fn the_recent_changes_panel_lists_the_audit_events_and_the_old_activity_page_is_gone() {
        let cat = catalog();
        let d = data();
        let html = users_page(&frame(), &view(&cat, &d));
        let panel = &html[html.find(r#"aria-labelledby="changes-title""#).unwrap()..];
        assert!(panel.contains("Permissions changed"));
        assert!(panel.contains(r#"<a href="/auth/settings/users/alice">alice</a>"#));
        assert!(panel.contains("root, 192.0.2.10"));
        let mut empty = data();
        empty.events.clear();
        empty.logins = LoginPage::default();
        let html = users_page(&frame(), &view(&cat, &empty));
        assert!(
            html.contains("No change recorded yet.") && html.contains("No sign-in recorded yet.")
        );
        for gone in [
            "Activity",
            "settings/logins",
            "Sign-in attempts",
            "Connections</h2>",
        ] {
            assert!(!main_of(&html).contains(gone), "{gone}");
        }
    }

    #[test]
    fn end_all_sessions_is_a_confirmation_in_place_in_the_toolbar() {
        let cat = catalog();
        let d = data();
        let html = users_page(&frame(), &view(&cat, &d));
        assert!(html.contains(r#"<details class="ask ask--danger"><summary class="ib ib--danger" aria-label="End all sessions""#));
        assert!(html.contains(r#"action="/auth/users/end-sessions""#));
        assert!(html.contains(r#"name="confirm" value="end-all-sessions""#));
        assert!(html.contains(r#"name="csrf" value="tok&lt;en""#));
        assert!(!html.contains("dangerRow") && !html.contains("incident"));
    }

    #[test]
    fn the_account_page_is_one_row_of_tools_over_two_groups_in_one_form() {
        let cat = catalog();
        let mut alice = card("alice", &["grafana", "mixed"]);
        alice.grants.push(grant("dashboards.read", "infra-grafana"));
        alice
            .grants
            .push(sub("dashboards.write on service:infra-grafana/folder:ops"));
        alice.grants.push(grant("keys.own.read", PORTAL));
        let mut v = user_view(&cat, alice);
        v.owned_keys = 3;
        let html = user_page(&full_frame(), &v);
        let bar =
            &html[html.find(r#"<div class="accountBar">"#).unwrap()..html.find("</main>").unwrap()];
        let bar = &bar[..bar.find("<form id=\"grants\"").unwrap()];
        // The row: the way back, the login, log in as, the password, the activity, delete, the
        // facts, Save. One row, and an icon for each tool, with its name.
        assert!(bar.contains(r#"<a href="/auth/settings">Users</a>"#));
        assert!(bar.contains(r#"<h2 class="name" aria-current="page">alice</h2>"#));
        assert!(
            bar.contains(r#"action="/auth/users/impersonate""#)
                && bar.contains(r#"aria-label="Log in as alice""#)
        );
        assert!(bar.contains(r#"aria-label="Change the password""#));
        assert!(bar.contains(r#"aria-label="Activity of the account""#));
        assert!(bar.contains(r#"aria-label="Delete alice""#));
        assert!(bar.contains("Created") && bar.contains("Last sign-in"));
        assert!(bar.contains(r#"type="submit" form="grants" data-save>Save</button>"#));
        let order = [
            "Users</a>",
            "Log in as alice",
            "Change the password",
            "Activity of the account",
            "Delete alice",
            "Created",
            ">Save<",
        ];
        let at: Vec<usize> = order.iter().map(|t| bar.find(t).unwrap()).collect();
        assert!(at.windows(2).all(|w| w[0] < w[1]), "{at:?}");
        // The delete shows the count of the keys that go with the account, in place.
        assert!(bar.contains("Delete alice and 3 keys?"), "{bar}");
        assert!(
            bar.contains(r#"required pattern="alice""#),
            "an account with keys asks for its name"
        );
        // One form, one editor with two parts.
        assert!(html.contains(r#"<form id="grants" method="post" action="/auth/users/update"><input type="hidden" name="csrf" value="tok&lt;en"><input type="hidden" name="login" value="alice"><input type="hidden" name="grants" value="1"></form>"#));
        assert_eq!(html.matches(r#"<div class="grantEditor""#).count(), 1);
        assert!(
            html.contains(r#"data-title="Permissions" data-layout="groups" data-form="grants">"#)
        );
        let data = editor_of(&html);
        assert_eq!(data["owner"], serde_json::Value::Null, "no bound of rights");
        assert_eq!(data["features"]["id"], PORTAL);
        assert_eq!(data["services"].as_array().unwrap().len(), 4);
        for grant in [
            "access on service:infra-grafana",
            "dashboards.write on service:infra-grafana/folder:ops",
            "keys.own.read on service:portal",
        ] {
            assert!(
                html.contains(&format!(r#"" value="{grant}" form="grants">"#)),
                "{grant}"
            );
        }
        assert!(
            html.contains("Change the \\u003cdashboards\\u003e."),
            "escaped"
        );
        assert!(!html.contains("Save access") && !html.contains("Opens 2 of 4"));
        // Without script the grants are a read-only list.
        assert!(html.contains(r#"<noscript><ul class="grantList"><li><code>"#));
        // Without keys, the delete needs one click.
        let html = user_page(&full_frame(), &user_view(&cat, card("bob", &["mixed"])));
        assert!(html.contains("Delete bob?") && !html.contains("pattern=\"bob\""));
    }

    #[test]
    fn the_password_field_is_in_its_popover_and_not_in_the_page() {
        let cat = catalog();
        let html = user_page(&frame(), &user_view(&cat, card("alice", &["grafana"])));
        let pop = &html[html
            .find(r#"<summary class="ib" aria-label="Change the password""#)
            .unwrap()..];
        let pop = &pop[..pop.find("</details>").unwrap()];
        assert!(pop.contains(r#"id="pw-alice""#) && pop.contains(" required"));
        assert!(pop.contains(r#"data-generate-password="pw-alice""#));
        assert!(pop.contains(r#"action="/auth/users/update""#));
        assert!(!pop.contains(r#"name="grants""#));
        // No other password field, no Generate widget outside the popover.
        assert_eq!(html.matches(r#"type="password""#).count(), 1);
        assert_eq!(html.matches("data-generate-password").count(), 1);
        assert_eq!(html.matches(r#"action="/auth/users/update""#).count(), 2);
        // The popover is a details element: it opens without script.
        assert!(html.contains(
            r#"<details class="pop"><summary class="ib" aria-label="Change the password""#
        ));
        assert!(html.contains(
            r#"<details class="pop"><summary class="ib" aria-label="Activity of the account""#
        ));
    }

    #[test]
    fn the_activity_of_an_account_is_in_a_popover() {
        let cat = catalog();
        let mut alice = card("alice", &["grafana"]);
        alice.activity.sessions_30d = 7;
        alice.activity.failed_30d = 2;
        alice.activity.recent.push(SessionRow {
            started_at: 1_791_000_000,
            last_seen_at: 1_791_000_100,
            ended_at: Some(1_791_000_200),
            end_reason: Some("logout".into()),
            ip: Some("192.0.2.9".into()),
            user_agent: "Mozilla/5.0 (X11; Linux x86_64; rv:131.0) Gecko/20100101 Firefox/131.0"
                .into(),
            impersonated: false,
        });
        alice.activity.services.push(crate::store::ServiceUse {
            service_id: "infra-grafana".into(),
            hits: 41,
            last_at: 1_791_000_150,
        });
        let html = user_page(&frame(), &user_view(&cat, alice));
        let pop = &html[html
            .find(r#"aria-label="Activity of the account""#)
            .unwrap()..];
        let pop = &pop[..pop.find("</details>").unwrap()];
        assert!(pop.contains("Sessions, 30 d") && pop.contains("Refused, 30 d"));
        assert!(
            pop.contains("192.0.2.9") && pop.contains("Signed out") && pop.contains("Firefox 131")
        );
        assert!(pop.contains("Grafana") && pop.contains(">41<"));
        assert!(pop.contains(r#"<svg class="spark spark--mini""#));
        assert!(!html.contains("partFold") && !html.contains("Latest sessions"));
    }

    #[test]
    fn the_account_page_closes_its_elements() {
        let cat = catalog();
        let html = user_page(
            &frame(),
            &user_view(&cat, card("alice", &["notes", "grafana"])),
        );
        for tag in [
            "div", "section", "header", "ul", "li", "label", "a", "h2", "form", "details",
            "summary", "table", "tr",
        ] {
            assert_eq!(
                main_of(&html).matches(&format!("<{tag}")).count(),
                main_of(&html).matches(&format!("</{tag}>")).count(),
                "<{tag}> is closed"
            );
        }
    }

    #[test]
    fn a_service_that_names_its_hosts_gives_the_editor_each_host_with_its_own_actions() {
        let cat = catalog();
        let document = crate::actions::parse(
            "infra-grafana",
            br#"{"server":"infra-grafana","resource_types":[{"name":"host","parent":"service","description":"The type host."}],"actions":[{"name":"dashboards.read","resource_types":["service","host"],"description":"See the dashboards."},{"name":"explorer.read","resource_types":["service","host"],"description":"Browse."}],"resources":[{"path":"host:prod","label":"Production"},{"path":"host:replica","label":"Replica","actions":["dashboards.read"]}]}"#,
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
        let data = editor_of(&html);
        let grafana = data["services"]
            .as_array()
            .unwrap()
            .iter()
            .find(|s| s["id"] == "infra-grafana")
            .unwrap();
        let nodes = grafana["nodes"].as_array().unwrap();
        let paths: Vec<&str> = nodes.iter().map(|n| n["path"].as_str().unwrap()).collect();
        assert_eq!(paths, ["", "host:prod", "host:replica"]);
        assert_eq!(nodes[1]["label"], "Production");
        assert_eq!(nodes[1]["only"], serde_json::json!([]));
        assert_eq!(
            nodes[2]["only"],
            serde_json::json!(["dashboards.read"]),
            "the second host names its actions"
        );
        assert!(html.contains(
            r#"" value="explorer.read on service:infra-grafana/host:prod" form="grants">"#
        ));
    }

    #[test]
    fn a_grant_on_the_whole_service_is_one_field_and_a_grant_on_a_host_is_another() {
        let cat = catalog();
        let document = crate::actions::parse(
            "infra-grafana",
            br#"{"server":"infra-grafana","resource_types":[{"name":"host","parent":"service","description":"The type host."}],"actions":[{"name":"dashboards.read","resource_types":["service","host"],"description":"The action dashboards.read."}],"resources":[{"path":"host:a"},{"path":"host:b"}]}"#,
        )
        .unwrap();
        let mut view = user_view(&cat, card("alice", &["grafana"]));
        view.services[0].actions = vec![("dashboards.read".into(), String::new())];
        view.services[0].document = std::sync::Arc::new(document);
        view.user
            .grants
            .push(grant("dashboards.read", "infra-grafana"));
        let html = user_page(&frame(), &view);
        assert!(
            html.contains(r#"" value="dashboards.read on service:infra-grafana" form="grants">"#)
        );
        assert!(!html.contains(r#"/host:a" form="#) && !html.contains(r#"/host:b" form="#));
        view.user.grants = access(&["grafana"]);
        view.user
            .grants
            .push(sub("dashboards.read on service:infra-grafana/host:b"));
        let html = user_page(&frame(), &view);
        assert!(html.contains(
            r#"" value="dashboards.read on service:infra-grafana/host:b" form="grants">"#
        ));
        assert!(!html.contains(r#"/host:a" form="#));
    }

    #[test]
    fn an_action_without_the_grant_access_stays_ticked_in_the_form() {
        let cat = catalog();
        let mut alice = card("alice", &[]);
        alice.grants.push(grant("dashboards.read", "infra-grafana"));
        let html = user_page(&frame(), &user_view(&cat, alice));
        assert!(
            html.contains(r#"" value="dashboards.read on service:infra-grafana" form="grants">"#)
        );
        assert!(!html.contains(r#"" value="access on service:infra-grafana" form="grants">"#));
    }

    #[test]
    fn new_user_is_the_account_page_in_create_mode_with_one_primary_action() {
        let cat = catalog();
        let html = new_user_page(
            &frame(),
            &NewUserView {
                notice: None,
                services: services(),
                features: Some(features()),
                token: "tok<en",
            },
        );
        let _ = &cat;
        // The login and the password are in the row of tools. One form holds them and the editor.
        let form = &html[html
            .find(r#"<form class="accountBar" id="add-user""#)
            .unwrap()..];
        let form = &form[..form.find("</form>").unwrap()];
        assert!(form.contains(r#"data-gate method="post" action="/auth/users/add""#));
        assert!(form.contains(r#"name="csrf" value="tok&lt;en""#));
        assert!(form.contains(r#"name="grants" value="1""#));
        assert!(form.contains(r#"id="new-login" name="login""#));
        assert!(form.contains(r#"id="new-password" name="password" type="password""#));
        assert!(form.contains(r#"data-generate-password="new-password""#));
        assert!(form.contains(r#"<h2 class="name" aria-current="page">New user</h2>"#));
        assert_eq!(form.matches(r#"type="submit""#).count(), 1);
        assert!(form
            .contains(r#"<button class="button button--primary" type="submit">Create</button>"#));
        // The editor sits under it and belongs to the form.
        assert!(
            html.contains(r#"data-title="Permissions" data-layout="groups" data-form="add-user">"#)
        );
        let data = editor_of(&html);
        assert_eq!(data["ticked"], serde_json::json!([]));
        assert!(data["features"]["id"] == PORTAL);
        // What the account page has for an account that exists is absent.
        for gone in [
            r#"id="grants""#,
            "data-save",
            "/auth/users/delete",
            "impersonate",
            "Create user",
            "Cancel",
        ] {
            assert!(!html.contains(gone), "{gone}");
        }
        // A refused try says why on the same page.
        let refused = new_user_page(
            &frame(),
            &NewUserView {
                notice: notice_for("exists"),
                services: services(),
                features: None,
                token: "t",
            },
        );
        assert!(
            refused.contains(r#"role="alert""#) && refused.contains("That login already exists.")
        );
        assert!(
            !refused.contains(r#"data-layout="groups" data-form="add-user">"#)
                || refused.contains("grantEditor")
        );
    }

    #[test]
    fn the_superadmin_page_and_a_file_account_page_change_nothing() {
        let cat = catalog();
        let mut root = card("root", &[]);
        root.superadmin = true;
        let html = user_page(&frame(), &user_view(&cat, root));
        assert!(html.contains("Every right on every service."));
        for gone in [
            "/auth/users/update",
            "/auth/users/delete",
            "/auth/users/impersonate",
            "grantEditor",
            "data-save",
            "Change the password",
        ] {
            assert!(!html.contains(gone), "{gone}");
        }
        assert!(html.contains("Activity of the account"));
        // A file account: its grants are locked, no form, no password, no delete, no Save.
        let mut dave = card("dave", &["grafana"]);
        dave.file = true;
        dave.created_at = None;
        dave.configured = dave.grants.clone();
        let html = user_page(&frame(), &user_view(&cat, dave));
        for gone in [
            r#"id="grants""#,
            "/auth/users/update",
            "/auth/users/delete",
            "/auth/users/impersonate",
            "data-save",
            "Change the password",
        ] {
            assert!(!html.contains(gone), "{gone}");
        }
        assert!(html.contains(r#"<span class="badge">file</span>"#));
        assert!(html.contains("in the file"));
        let data = editor_of(&html);
        assert_eq!(data["locked"].as_array().unwrap().len(), 1);
        assert_eq!(data["ticked"], serde_json::json!([]));
        assert!(!html.contains(r#"name="grant"#));
    }

    #[test]
    fn configured_grants_are_locked_and_the_static_mode_has_no_form() {
        let cat = catalog();
        let mut bob = card("bob", &["mixed"]);
        bob.configured = access(&["grafana"]);
        bob.grants.extend(access(&["grafana"]));
        let configured = sub("dashboards.read on service:infra-grafana/folder:ops");
        bob.grants.push(configured.clone());
        bob.configured.push(configured);
        let mut view = user_view(&cat, bob.clone());
        view.mode = Mode::Hybrid;
        let html = user_page(&frame(), &view);
        assert!(
            html.contains(r#"name="grant0" value="access on service:infra-mixed" form="grants""#)
        );
        assert!(!html.contains("infra-grafana\" form="));
        let data = editor_of(&html);
        let locked: Vec<String> = data["locked"]
            .as_array()
            .unwrap()
            .iter()
            .map(|g| format!("{} {}", g[0].as_str().unwrap(), g[2].as_str().unwrap()))
            .collect();
        assert_eq!(locked, ["access ", "dashboards.read folder:ops"]);
        let mut page = user_view(&cat, bob);
        page.mode = Mode::Static;
        let html = user_page(&frame(), &page);
        assert!(!html.contains(r#"id="grants""#) && !html.contains("data-save"));
        assert!(!html.contains(r#"name="grant"#));
        assert!(!html.contains("/auth/users/delete") || html.contains("Delete bob"));
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
        assert!(html.contains("Opened by the superadmin") && html.contains("Active"));
        assert!(html.contains(r#"id="pw-&lt;b&gt;x&lt;/b&gt;""#));
        assert!(html.contains("Delete &lt;b&gt;x&lt;/b&gt;?"));
    }

    #[test]
    fn the_services_page_selects_a_row_with_a_link_and_shows_who_opens_it() {
        let cat = catalog();
        let actions = services();
        let holders = vec![
            holder("alice", access(&["grafana", "mixed"])),
            holder("bob", vec![grant("dashboards.read", "infra-grafana")]),
            holder("carol", Vec::new()),
        ];
        let html = services_page(&frame(), &services_view(&cat, &actions, &holders));
        // Each row is a link with a query parameter, so the selection works without script.
        assert!(html.contains(r#"<a class="rowLink" href="/auth/settings/services?svc=infra-grafana#row-1" aria-current="true">Grafana</a>"#), "{html}");
        assert!(html.contains(r#"href="/auth/settings/services?svc=infra-notes#row-3""#));
        assert!(html.contains(r#"<tr id="row-1" class="is-selected">"#));
        assert_eq!(html.matches("is-selected").count(), 1);
        assert!(
            !html.contains("/auth/settings/services/infra-"),
            "no page for one service"
        );
        // The panel "Opened by" belongs to the selected row.
        let panel = &html[html.find(r#"aria-labelledby="opened-title""#).unwrap()..];
        let panel = &panel[..panel.find("</section>").unwrap()];
        assert!(panel.contains(r#"<span class="count">Grafana</span>"#));
        assert!(panel.contains(r#"<a href="/auth/settings/users/alice">alice</a>"#));
        assert!(
            !panel.contains(">bob</a></td><td class=\"r muted\">"),
            "an action without access does not open the service"
        );
        // The accounts that do not open the service follow, with a link that gives access.
        assert!(panel.contains("Not opened"));
        assert!(
            panel.contains(
                r#"href="/auth/settings/give?user=bob&amp;service=infra-grafana&amp;give=1""#
            ),
            "{panel}"
        );
        assert!(panel.contains(r#"aria-label="Give carol access to Grafana""#));
        assert!(!panel.contains("user=alice"), "alice opens it already");
        // Another row selected by the query parameter.
        let mut v = services_view(&cat, &actions, &holders);
        v.selected = Some("infra-mixed".into());
        let html = services_page(&frame(), &v);
        assert!(html.contains(r#"<tr id="row-2" class="is-selected">"#));
        assert!(html.contains(r#"<span class="count">Mixed</span>"#));
        // A value that names no service selects the first one.
        v.selected = Some("<nope>".into());
        let html = services_page(&frame(), &v);
        assert!(html.contains(r#"<tr id="row-1" class="is-selected">"#));
        assert!(!html.contains("<nope>"));
        // Nobody opens it: one line, no empty panel.
        v.selected = Some("infra-notes".into());
        let html = services_page(&frame(), &v);
        assert!(html.contains("Superadmin only"));
        // A page that cannot change grants offers no link that gives access.
        v.mode = Mode::Static;
        assert!(!services_page(&frame(), &v).contains("settings/give"));
    }

    #[test]
    fn the_services_chips_are_links_and_the_check_filter_keeps_only_the_services_to_check() {
        let cat = catalog();
        let actions = services();
        let holders = vec![holder(
            "alice",
            vec![
                grant(ACCESS, "infra-grafana"),
                sub("dashboards.read on service:infra-grafana/team:x"),
            ],
        )];
        let mut v = services_view(&cat, &actions, &holders);
        v.problems = vec![(
            "infra-typo".into(),
            "http://typo/x gave the status <500>".into(),
        )];
        let html = services_page(&frame(), &v);
        assert!(html.contains(r#"<a class="chip is-on" href="/auth/settings/services?svc=infra-grafana" aria-current="true">All<span class="num">4</span></a>"#), "{html}");
        assert!(
            html.contains(
                r#"href="/auth/settings/services?svc=infra-grafana&f=check""#
                    .replace('&', "&amp;")
                    .as_str()
            ),
            "{html}"
        );
        assert!(html.contains(r#"aria-label="Services to check 2" title="Services to check""#));
        assert!(html.contains("gave the status &lt;500&gt;"));
        // The filter is on: only Grafana (a grant to check) and Typo (an unreadable document).
        v.only_check = true;
        let filtered = services_page(&frame(), &v);
        let table =
            &filtered[filtered.find("<tbody>").unwrap()..filtered.find("</table>").unwrap()];
        assert!(table.contains(">Grafana</a>") && table.contains(">Typo</a>"));
        assert!(!table.contains(">Notes</a>") && !table.contains(">Mixed</a>"));
        assert!(filtered.contains(
            r#"<a class="chip is-on" href="/auth/settings/services?svc=infra-grafana&amp;f=check""#
        ));
        // No service to check: no chip.
        let clean = services_page(&frame(), &services_view(&cat, &actions, &[]));
        assert!(!clean.contains("f=check") && !clean.contains("Services to check"));
    }

    #[test]
    fn the_services_page_lists_the_grants_to_check_with_their_problem_and_account() {
        let cat = catalog();
        let actions = services();
        let holders = vec![
            holder(
                "alice",
                vec![
                    sub("dashboards.read on service:infra-grafana/folder:ops"),
                    sub("dashboards.read on service:infra-grafana/folder:ops/dashboard:1"),
                    sub("dashboards.write on service:infra-grafana/dashboard:1"),
                ],
            ),
            holder("bob", vec![sub("x.read on service:infra-notes/a:b")]),
        ];
        let html = services_page(&frame(), &services_view(&cat, &actions, &holders));
        assert!(html.contains(r#"<h2 class="title" id="checks-title">To check</h2>"#));
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
        // The panel is absent when there is nothing to check.
        let none = services_page(
            &frame(),
            &services_view(&cat, &actions, &[holder("alice", access(&["grafana"]))]),
        );
        assert!(!none.contains("checks-title"));
    }

    #[test]
    fn the_services_page_has_the_counts_the_state_icon_of_a_problem_and_an_empty_state() {
        let cat = catalog();
        let actions = services();
        let holders = vec![holder("alice", access(&["grafana"]))];
        let mut v = services_view(&cat, &actions, &holders);
        v.problems = vec![("infra-typo".into(), "x".into())];
        let html = services_page(&frame(), &v);
        assert!(html.contains(
            r#"<tr class="grid__group"><th scope="rowgroup" colspan="6">Infra</th></tr>"#
        ));
        assert!(html.contains(r#"<span class="statusIcon statusIcon--warn""#));
        // Grafana: one account opens it, two actions, two types.
        let row = &html[html.find(r#"<tr id="row-1""#).unwrap()..];
        let row = &row[..row.find("</tr>").unwrap()];
        assert!(
            row.contains(
                r#"<td class="r num">1</td><td class="r num">2</td><td class="r num">2</td>"#
            ),
            "{row}"
        );
        let empty = Catalog::build(Vec::new(), Vec::new());
        let html = services_page(&frame(), &services_view(&empty, &[], &[]));
        assert!(html.contains("No service in the catalogue yet."));
        assert!(!html.contains("opened-title"));
    }

    #[test]
    fn a_graph_is_four_nodes_and_only_the_newest_bar_wears_the_accent() {
        let mut days = vec![DayStat::default(); 30];
        days[3] = DayStat {
            sessions: 5,
            seconds: 60,
        };
        days[29] = DayStat {
            sessions: 2,
            seconds: 60,
        };
        let svg = spark(&days, "spark--mini");
        assert_eq!(svg.matches("<path").count(), 3);
        assert_eq!(svg.matches("spark__bar--today").count(), 1);
        assert!(svg.contains("M12 2h3v30h-3z"), "{svg}");
        assert!(svg.contains("M116 20h3v12h-3z"), "{svg}");
        assert!(!svg.contains("style=") && !svg.contains("<rect") && !svg.contains("<title>"));
        assert!(svg.contains("7 in total") && svg.contains(r#"role="img""#));
        assert_eq!(
            spark(&vec![DayStat::default(); 30], "")
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
            "grant-limit",
            "bad-limits",
            "password-reset",
            "user-updated",
            "unknown-grant",
        ] {
            assert!(notice_for(code).is_some(), "{code}");
        }
    }
}
