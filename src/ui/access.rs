//! The tab Access: the accounts and the keys side by side, under one row of filters.
//!
//! The superadmin sees both lists. An account with a right of the keys sees the list of keys alone.
//! The superadmin is not a row of the table of accounts: the configuration file sets it, and it
//! holds every right.
//!
//! The URL is the truth of the filters: `?user=&group=&service=`. Each chip and each cross is a
//! link, and the filter form uses the method GET, so the page works without script. The server
//! filters both lists. A value that names nothing gives an empty list, never an error. All the
//! filters apply together. A row of the accounts opens the page of its account.
//!
//! | Filter | Accounts | Keys |
//! |---|---|---|
//! | `user` | The account. | The keys that it owns. |
//! | `group` | The members: the accounts that hold `inbox.receive` on the group. | The keys with a grant on the group. |
//! | `service` | The accounts that open the service. The column shows their rights on it. | The keys with a grant on the service. |
//!
//! The columns of the keys, of the groups and of the features are absent when the file turns the
//! feature off. With no key and no group, the column "Open" shows the services of each account.

use std::fmt::Write as _;

use super::components::{ask, icon, icon_count, tags, zone, Ask, Side, Strength, Tag, Zone};
use super::keys::{key_groups, keys_zone, KeyLine, KeysList};
use super::settings::{
    feature_rights, fill_page, notice_html, plural, summary, user_path, Feature, Frame, Notice,
    ServiceActions, Tab, UserCard, ACCESS_PATH, NEW_USER_PATH,
};
use super::status_icon;
use crate::catalog::Catalog;
use crate::grants::{holds, Mode, ACCESS};
use crate::keys::KeyKind;
use crate::util::{html_escape, percent_encode_query_component, time_tag};

/// The query of the tab: the three filters, the chip of a kind of key and the chip "mine". An empty
/// text is a filter that is off.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Query {
    pub user: String,
    pub group: String,
    /// The id of a service, or its name as the person typed it.
    pub service: String,
    pub kind: Option<KeyKind>,
    pub mine: bool,
}

impl Query {
    /// The address of the page with this query.
    pub fn href(&self) -> String {
        let mut pairs = Vec::new();
        for (name, value) in [
            ("user", &self.user),
            ("group", &self.group),
            ("service", &self.service),
        ] {
            if !value.is_empty() {
                pairs.push(format!("{name}={}", percent_encode_query_component(value)));
            }
        }
        if let Some(kind) = self.kind {
            pairs.push(format!("k={}", kind.name()));
        }
        if self.mine {
            pairs.push("mine=1".to_string());
        }
        if pairs.is_empty() {
            ACCESS_PATH.to_string()
        } else {
            format!("{ACCESS_PATH}?{}", pairs.join("&"))
        }
    }

    /// One of the three filters is on.
    pub fn filtered(&self) -> bool {
        !(self.user.is_empty() && self.group.is_empty() && self.service.is_empty())
    }

    /// The same query with the three filters off. The chips of the keys stay.
    pub fn cleared(&self) -> Self {
        Self {
            user: String::new(),
            group: String::new(),
            service: String::new(),
            ..self.clone()
        }
    }

    pub fn with_kind(&self, kind: Option<KeyKind>) -> Self {
        Self {
            kind,
            ..self.clone()
        }
    }

    pub fn with_mine(&self, mine: bool) -> Self {
        Self {
            mine,
            ..self.clone()
        }
    }

    /// The query of a click on a group chip: the group, or no group when it is on already.
    pub fn toggle_group(&self, group: &str) -> Self {
        Self {
            group: if self.group == group {
                String::new()
            } else {
                group.to_string()
            },
            ..self.clone()
        }
    }

    pub fn with_group(&self, group: &str) -> Self {
        Self {
            group: group.to_string(),
            ..self.clone()
        }
    }

    pub fn with_service(&self, service: &str) -> Self {
        Self {
            service: service.to_string(),
            ..self.clone()
        }
    }

    pub fn with_user(&self, user: &str) -> Self {
        Self {
            user: user.to_string(),
            ..self.clone()
        }
    }
}

/// A group of the inbox, as the page names it.
#[derive(Clone, Debug)]
pub struct GroupLine {
    pub id: String,
    pub name: String,
    pub critical: bool,
}

/// The accounts that the page lists. Only the superadmin has them.
pub struct Accounts<'a> {
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
    pub max_users: usize,
    /// The anti-CSRF token of the administration forms.
    pub token: &'a str,
    /// The logins that have a second factor. `None`: the second factor is off, or the database cannot be read.
    /// The table has the column "2FA" only when this is `Some`.
    pub two_factor: Option<std::collections::HashSet<String>>,
}

/// The data of the tab Access.
pub struct AccessView<'a> {
    pub notice: Option<Notice>,
    pub query: Query,
    /// The accounts. `None`: the viewer is not the superadmin.
    pub accounts: Option<Accounts<'a>>,
    /// The keys. `None`: the file has no block `keys`, or the viewer holds no right of the keys.
    pub keys: Option<KeysList>,
    /// The groups of the inbox. Empty: the file has no block `inbox`.
    pub groups: Vec<GroupLine>,
}

impl Accounts<'_> {
    /// The page offers "New user": the file allows it and the grants are not read-only.
    fn can_create(&self) -> bool {
        self.allow_creation && self.mode.editable()
    }

    /// The id of the service that the filter names: the id, or the name that the person typed.
    /// A text that names no service of the catalogue stays as it is, and keeps no account.
    fn service_id(&self, text: &str) -> String {
        service_id_of(self.catalog, text)
    }
}

/// The id of the service that `text` names: an id of the catalogue, or a name of it without
/// regard to case. A text that names no service is an id that no grant holds.
fn service_id_of(catalog: &Catalog, text: &str) -> String {
    let all = || catalog.groups().iter().flat_map(|g| g.services.iter());
    all()
        .find(|s| s.id == text)
        .or_else(|| all().find(|s| s.name.eq_ignore_ascii_case(text)))
        .map_or_else(|| text.to_string(), |s| s.id.clone())
}

/// The name of the service that the filter shows: its name in the catalogue, or the text.
fn service_label(catalog: &Catalog, text: &str) -> String {
    let id = service_id_of(catalog, text);
    catalog
        .groups()
        .iter()
        .flat_map(|g| g.services.iter())
        .find(|s| s.id == id)
        .map_or_else(|| text.to_string(), |s| s.name.clone())
}

/// The members of a group: each account with `inbox.receive` on it, and the superadmin.
fn member_count(users: &[UserCard], group: &str) -> usize {
    users
        .iter()
        .filter(|u| u.groups.iter().any(|g| g == group))
        .count()
        + 1
}

/// Whether the key holds a grant on the service.
fn key_on_service(line: &KeyLine, service: &str) -> bool {
    line.grants.iter().any(|g| g.grant.service == service)
}

/// The keys that the three filters keep.
fn keys_matching<'a>(
    lines: &'a [KeyLine],
    query: &Query,
    catalog: Option<&Catalog>,
) -> Vec<&'a KeyLine> {
    let service = catalog.map_or_else(
        || query.service.clone(),
        |c| service_id_of(c, &query.service),
    );
    lines
        .iter()
        .filter(|l| query.user.is_empty() || l.owner.as_deref() == Some(query.user.as_str()))
        .filter(|l| query.group.is_empty() || key_groups(l).contains(&query.group.as_str()))
        .filter(|l| query.service.is_empty() || key_on_service(l, &service))
        .collect()
}

pub fn access_page(frame: &Frame, view: &AccessView) -> String {
    let mut out = String::new();
    notice_html(&mut out, view.notice);
    let query = &view.query;
    let mut columns = 0;
    let mut body = String::new();
    if let Some(accounts) = &view.accounts {
        out.push_str(&filter_bar(view, accounts));
        body.push_str(&accounts_zone(view, accounts));
        columns += 1;
    }
    if let Some(list) = &view.keys {
        let catalog = view.accounts.as_ref().map(|a| a.catalog);
        let matching = keys_matching(&list.lines, query, catalog);
        body.push_str(&keys_zone(list, query, &matching));
        columns += 1;
    }
    let _ = write!(out, r#"<div class="access access--{columns}">{body}</div>"#);
    fill_page(Tab::Access, frame, "Access", &out)
}

// The filters -----------------------------------------------------------------------------------

/// The row of filters: the user, the groups as chips that are links, the service, the active
/// filters with a cross, and "Clear". It is a form with the method GET.
fn filter_bar(view: &AccessView, accounts: &Accounts) -> String {
    let query = &view.query;
    let mut out = String::from(
        r#"<form class="fbar" method="get" action="/auth/settings" role="search" aria-label="Filters of the page">"#,
    );
    let users: String = accounts
        .users
        .iter()
        .map(|u| format!(r#"<option value="{}"></option>"#, html_escape(&u.login)))
        .collect();
    let services: String = accounts
        .catalog
        .groups()
        .iter()
        .flat_map(|g| g.services.iter())
        .map(|s| {
            format!(
                r#"<option value="{}" label="{}"></option>"#,
                html_escape(&s.name),
                html_escape(&s.id)
            )
        })
        .collect();
    let shown_user = html_escape(&clip(&query.user));
    let _ = write!(
        out,
        r#"<span class="fbar__field">{}<input class="uiInput" name="user" list="filter-users" value="{shown_user}" placeholder="User" aria-label="Filter by user" autocomplete="off" maxlength="32"></span>"#,
        icon("user")
    );
    if !view.groups.is_empty() {
        out.push_str(r#"<span class="fbar__sep"></span><span class="fbar__label">Group</span><span class="chips" role="group" aria-label="Filter by group">"#);
        for group in &view.groups {
            let on = query.group == group.id;
            let mark = if group.critical {
                format!(
                    r#"<span class="crit" title="Critical group: its alerts always ring.">{}</span>"#,
                    icon("bell-ringing")
                )
            } else {
                String::new()
            };
            let _ = write!(
                out,
                r#"<a class="chip{}" href="{}"{} title="Members of {}">{}{mark}<span class="num">{}</span></a>"#,
                if on { " is-on" } else { "" },
                html_escape(&query.toggle_group(&group.id).href()),
                if on { r#" aria-current="true""# } else { "" },
                html_escape(&group.name),
                html_escape(&group.name),
                member_count(&accounts.users, &group.id),
            );
        }
        out.push_str("</span>");
    }
    let shown_service = if query.service.is_empty() {
        String::new()
    } else {
        html_escape(&clip(&service_label(accounts.catalog, &query.service)))
    };
    let _ = write!(
        out,
        r#"<span class="fbar__sep"></span><span class="fbar__field">{}<input class="uiInput" name="service" list="filter-services" value="{shown_service}" placeholder="Service" aria-label="Filter by service" autocomplete="off" maxlength="160"></span>"#,
        icon("search")
    );
    // The chips of the groups and of the keys stay when the form is sent.
    if !query.group.is_empty() {
        let _ = write!(
            out,
            r#"<input type="hidden" name="group" value="{}">"#,
            html_escape(&clip(&query.group))
        );
    }
    if let Some(kind) = query.kind {
        let _ = write!(
            out,
            r#"<input type="hidden" name="k" value="{}">"#,
            kind.name()
        );
    }
    if query.mine {
        out.push_str(r#"<input type="hidden" name="mine" value="1">"#);
    }
    out.push_str(
        r#"<button class="visually-hidden" type="submit" tabindex="-1">Apply the filters</button>"#,
    );
    out.push_str(&active_chips(view, accounts));
    let _ = write!(
        out,
        r#"<datalist id="filter-users">{users}</datalist><datalist id="filter-services">{services}</datalist></form>"#
    );
    out
}

/// A text that comes from the address, cut to a length that fits the row.
fn clip(text: &str) -> String {
    text.chars().take(64).collect()
}

/// One chip for each filter of a field that is on, with a cross that removes it, then "Clear". A
/// group that is on shows in the chip of the group.
fn active_chips(view: &AccessView, accounts: &Accounts) -> String {
    let query = &view.query;
    let mut out = String::new();
    let mut chip = |label: &str, value: &str, remove: &Query, extra: &str| {
        let _ = write!(
            out,
            r#"<span class="fchip">{label}: {}<a class="fchip__x" href="{}" aria-label="Remove the filter {label} {}" title="Remove the filter">{}</a>{extra}</span>"#,
            html_escape(value),
            html_escape(&remove.href()),
            html_escape(value),
            icon("x"),
        );
    };
    if !query.user.is_empty() {
        let known = accounts.users.iter().any(|u| u.login == query.user);
        let go = if known {
            format!(
                r#"<a class="fchip__go" href="{}">Show account</a>"#,
                html_escape(&user_path(&query.user))
            )
        } else {
            String::new()
        };
        chip("user", &clip(&query.user), &query.with_user(""), &go);
    }
    if !query.service.is_empty() {
        let name = clip(&service_label(accounts.catalog, &query.service));
        chip("service", &name, &query.with_service(""), "");
    }
    if query.filtered() {
        let _ = write!(
            out,
            r#"<a class="fclear" href="{}">Clear</a>"#,
            html_escape(&query.cleared().href())
        );
    }
    out
}

// The accounts ----------------------------------------------------------------------------------

/// The class of a fixed column that holds `chars` characters of 13 px text and a cell padding.
/// `floor` is the smallest class. The caller limits `chars`: a longer text is cut with an
/// ellipsis, and the tooltip of the cell gives it in full.
pub(super) fn col_class(chars: usize, floor: usize) -> &'static str {
    const CLASSES: [(usize, &str); 11] = [
        (88, "w88"),
        (108, "w108"),
        (120, "w120"),
        (136, "w136"),
        (152, "w152"),
        (160, "w160"),
        (176, "w176"),
        (208, "w208"),
        (240, "w240"),
        (272, "w272"),
        (320, "w320"),
    ];
    let need = chars * 8 + 12;
    CLASSES
        .iter()
        .find(|(px, _)| *px >= floor && *px >= need)
        .map_or("w320", |(_, class)| class)
}

/// The accounts that the filters keep.
fn accounts_matching<'a>(
    accounts: &'a Accounts,
    query: &Query,
) -> (Vec<&'a UserCard>, Option<String>) {
    let service = (!query.service.is_empty()).then(|| accounts.service_id(&query.service));
    let shown = accounts
        .users
        .iter()
        .filter(|u| query.user.is_empty() || u.login == query.user)
        .filter(|u| query.group.is_empty() || u.groups.contains(&query.group))
        .filter(|u| {
            service.as_deref().is_none_or(|id| {
                u.grants.iter().any(|g| g.service == id) && holds(&u.grants, ACCESS, id)
            })
        })
        .collect();
    (shown, service)
}

fn accounts_zone(view: &AccessView, accounts: &Accounts) -> String {
    let query = &view.query;
    let (shown, service) = accounts_matching(accounts, query);
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
                html_escape(accounts.token)
            ),
            strength: Strength::Light,
            name: "",
            danger: true,
            side: Side::End,
        })
    );
    if accounts.can_create() {
        let _ = write!(
            tools,
            r#"<a class="button button--primary" href="{NEW_USER_PATH}">{}New user</a>"#,
            icon("plus")
        );
    }
    let total = accounts.users.len();
    let count = if shown.len() == total {
        total.to_string()
    } else {
        format!("{}/{total}", shown.len())
    };
    let count_title = format!(
        "Accounts that match the filters. The limit is {}.",
        accounts.max_users
    );
    // The number of accounts that have a second factor, among all the accounts of the list.
    let two_factor_count = accounts.two_factor.as_ref().map(|set| {
        let on = accounts
            .users
            .iter()
            .filter(|u| set.contains(&u.login))
            .count();
        format!(
            r#"<span class="count" title="Accounts with two-factor sign-in">{}2FA {on}/{total}</span>"#,
            icon("lock")
        )
    });
    let body = if shown.is_empty() {
        if total == 0 {
            r#"<p class="empty">No account yet.</p>"#.to_string()
        } else {
            format!(
                r#"<p class="empty">No account matches these filters. <a href="{}">Clear</a></p>"#,
                html_escape(&query.cleared().href())
            )
        }
    } else {
        accounts_table(view, accounts, &shown, service.as_deref())
    };
    zone(
        &Zone {
            id: "accounts-title",
            title: "Accounts",
            count: &count,
            count_title: &count_title,
            chips_html: two_factor_count.as_deref().unwrap_or_default(),
            tools_html: &tools,
            class: "",
            above_html: "",
        },
        &body,
    )
}

/// The table of the accounts. The columns of the features, of the groups and of the keys are
/// absent when the file turns the feature off. The column of the services becomes the column of
/// the rights while the filter names a service.
fn accounts_table(
    view: &AccessView,
    accounts: &Accounts,
    shown: &[&UserCard],
    service: Option<&str>,
) -> String {
    let keys = view.keys.is_some();
    let inbox = !view.groups.is_empty();
    let features = keys || inbox;
    let opens = !keys && !inbox;
    // The column of the account fits the longest login and the badge. The next column takes the
    // free width.
    let widest = shown
        .iter()
        .map(|u| u.login.chars().count() + if u.file { 5 } else { 0 })
        .max()
        .unwrap_or(0)
        .min(13);
    let mut out = String::from(if inbox {
        r#"<table class="grid grid--users grid--groups"><colgroup>"#
    } else {
        r#"<table class="grid grid--users"><colgroup>"#
    });
    if inbox || opens {
        let _ = write!(out, r#"<col class="{}">"#, col_class(widest, 88));
    } else {
        out.push_str("<col>");
    }
    if inbox {
        out.push_str("<col>");
    }
    if opens {
        out.push_str("<col>");
    }
    out.push_str(r#"<col class="w64">"#);
    if features {
        out.push_str(r#"<col class="w80">"#);
    }
    if keys {
        out.push_str(r#"<col class="w40">"#);
    }
    if accounts.two_factor.is_some() {
        out.push_str(r#"<col class="w40">"#);
    }
    out.push_str(r#"<col class="w120"><col class="w64"><col class="w56"></colgroup><thead><tr><th scope="col">Account</th>"#);
    if inbox {
        out.push_str(r#"<th scope="col">Groups</th>"#);
    }
    if opens {
        out.push_str(r#"<th scope="col">Open</th>"#);
    }
    let _ = write!(
        out,
        r#"<th scope="col">{}</th>"#,
        if service.is_some() {
            "Permissions"
        } else {
            "Services"
        }
    );
    if features {
        out.push_str(r#"<th scope="col">Features</th>"#);
    }
    if keys {
        out.push_str(r#"<th scope="col" class="r">Keys</th>"#);
    }
    if accounts.two_factor.is_some() {
        out.push_str(r#"<th scope="col" class="c" title="Two-factor sign-in">2FA</th>"#);
    }
    out.push_str(r#"<th scope="col" class="r">Last sign-in</th><th scope="col" class="r" title="Sessions, 30 days">Sessions</th><th scope="col" class="r" title="Refused sign-ins, 30 days">Refused</th></tr></thead><tbody>"#);
    for user in shown {
        account_row(&mut out, view, accounts, user, service);
    }
    out.push_str("</tbody></table>");
    out
}

fn account_row(
    out: &mut String,
    view: &AccessView,
    accounts: &Accounts,
    user: &UserCard,
    service: Option<&str>,
) {
    let query = &view.query;
    let keys = view.keys.is_some();
    let inbox = !view.groups.is_empty();
    let login = html_escape(&user.login);
    let _ = write!(
        out,
        r#"<tr><th scope="row" title="{login}"><a class="rowLink" href="{}">{login}</a>"#,
        html_escape(&user_path(&user.login))
    );
    if user.file {
        out.push_str(r#" <span class="badge">file</span>"#);
    }
    out.push_str("</th>");
    if inbox {
        let items: Vec<(String, String, &GroupLine)> = view
            .groups
            .iter()
            .filter(|g| user.groups.contains(&g.id))
            .map(|g| {
                (
                    query.with_group(&g.id).href(),
                    format!("{} accounts", member_count(&accounts.users, &g.id)),
                    g,
                )
            })
            .collect();
        let marks: Vec<String> = items
            .iter()
            .map(|(_, _, g)| {
                if g.critical {
                    format!(
                        r#"<span class="crit" title="Critical group: its alerts always ring.">{}</span>"#,
                        icon("bell-ringing")
                    )
                } else {
                    String::new()
                }
            })
            .collect();
        let titles: Vec<String> = items
            .iter()
            .map(|(_, _, g)| format!("Group {}: {} holds inbox.receive on it", g.name, user.login))
            .collect();
        let tag_list: Vec<Tag> = items
            .iter()
            .zip(marks.iter().zip(titles.iter()))
            .map(|((href, note, g), (mark, title))| Tag {
                lead_html: "",
                text: &g.name,
                mark_html: mark,
                href,
                title,
                note,
            })
            .collect();
        let _ = write!(
            out,
            r#"<td class="tagsCell">{}</td>"#,
            tags(
                &tag_list,
                1,
                &format!("All {} groups of {}", tag_list.len(), user.login)
            )
        );
    }
    if !keys && !inbox {
        out.push_str(r#"<td class="tagsCell">"#);
        out.push_str(&opened_tags(view, accounts, user));
        out.push_str("</td>");
    }
    out.push_str(r#"<td class="num">"#);
    match service {
        Some(id) => {
            let rights = user.grants.iter().filter(|g| g.service == id).count();
            let publishes = accounts
                .services
                .iter()
                .any(|s| s.id == id && !s.actions.is_empty());
            if rights == 0 {
                out.push_str(r#"<span class="muted">-</span>"#);
            } else if publishes {
                out.push_str(&plural(rights, "permission", "permissions"));
            } else {
                out.push_str("access");
            }
        }
        None => {
            let s = summary(accounts.catalog, &accounts.services, user);
            if s.services == 0 {
                out.push_str(r#"<span class="muted">-</span>"#);
            } else {
                let _ = write!(out, "{}/{}", s.services, accounts.services.len());
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
    }
    out.push_str("</td>");
    if keys || inbox {
        out.push_str("<td>");
        let mut any = false;
        for (shown, feature) in [(keys, Feature::Keys), (inbox, Feature::Inbox)] {
            let (held, total) = feature_rights(user, accounts.features.as_ref(), feature);
            if shown && held > 0 {
                any = true;
                out.push_str(&icon_count(
                    &format!("{}: {held} of {total} permissions", feature.name()),
                    feature.icon(),
                    held,
                ));
            }
        }
        if !any {
            out.push_str(r#"<span class="muted">-</span>"#);
        }
        out.push_str("</td>");
    }
    if keys {
        let _ = write!(
            out,
            r#"<td class="r num{}">{}</td>"#,
            if user.keys == 0 { " muted" } else { "" },
            if user.keys == 0 {
                "-".to_string()
            } else {
                user.keys.to_string()
            }
        );
    }
    if let Some(set) = &accounts.two_factor {
        if set.contains(&user.login) {
            let _ = write!(
                out,
                r#"<td class="c">{}</td>"#,
                status_icon("ok", "lock", "Two-factor is on")
            );
        } else {
            out.push_str(
                r#"<td class="c"><span class="muted" title="Two-factor is off">-</span></td>"#,
            );
        }
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

/// The services that an account opens, as small chips that filter by service. The file has neither
/// keys nor inbox, so the table has the room.
fn opened_tags(view: &AccessView, accounts: &Accounts, user: &UserCard) -> String {
    let opened: Vec<(&str, &str)> = accounts
        .catalog
        .groups()
        .iter()
        .flat_map(|g| g.services.iter())
        .filter(|s| holds(&user.grants, ACCESS, &s.id))
        .map(|s| (s.name.as_str(), s.id.as_str()))
        .collect();
    let hrefs: Vec<String> = opened
        .iter()
        .map(|(_, id)| view.query.with_service(id).href())
        .collect();
    let list: Vec<Tag> = opened
        .iter()
        .zip(hrefs.iter())
        .map(|((name, _), href)| Tag {
            lead_html: "",
            text: name,
            mark_html: "",
            href,
            title: name,
            note: "",
        })
        .collect();
    tags(
        &list,
        3,
        &format!("All {} services of {}", list.len(), user.login),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::fixtures::*;
    use crate::ui::settings::PORTAL;

    fn page(view: &AccessView) -> String {
        access_page(&full_frame(), view)
    }

    /// The part of the page between the head of a zone and its end.
    fn zone_of<'a>(html: &'a str, id: &str) -> &'a str {
        let at = html.find(&format!(r#"aria-labelledby="{id}""#)).unwrap();
        let rest = &html[at..];
        &rest[..rest.find("</section>").unwrap()]
    }

    #[test]
    fn the_page_has_the_accounts_and_the_keys_side_by_side_and_the_accounts_link_to_their_page() {
        let cat = catalog();
        let mut v = view(&cat);
        {
            let accounts = v.accounts.as_mut().unwrap();
            accounts.users[1]
                .grants
                .push(sub("dashboards.read on service:infra-grafana/team:x"));
            accounts.users[0].activity.failed_30d = 2;
            accounts.users[0].keys = 3;
            accounts.users[0].grants.extend([
                grant("keys.own.read", PORTAL),
                grant("keys.own.edit", PORTAL),
                grant("inbox.receive", PORTAL),
            ]);
        }
        let html = page(&v);
        assert!(html.contains(r#"<div class="access access--2">"#));
        let accounts = zone_of(&html, "accounts-title");
        for login in ["alice", "bob", "carol"] {
            assert!(
                accounts.contains(&format!(
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
            accounts.contains("2/4"),
            "alice opens two of the four services"
        );
        assert!(
            accounts.contains(r#"<span class="muted">-</span>"#),
            "carol"
        );
        assert!(
            accounts.contains("permission to check"),
            "bob holds a grant that Grafana does not declare"
        );
        // The features are icons with counts and a name.
        assert!(
            accounts.contains(r#"role="img" aria-label="Keys: 2 of 8 permissions""#),
            "{accounts}"
        );
        assert!(accounts.contains(r#"aria-label="Inbox: 1 of 7 permissions""#));
        assert!(
            accounts.contains(r#"<td class="r num">3</td>"#),
            "the keys of alice"
        );
        assert!(accounts.contains(r#"<td class="r num danger">2</td>"#));
        // The count shows the total, and the limit is in its tooltip.
        assert!(accounts.contains(r#"<span class="count" title="Accounts that match the filters. The limit is 64.">3</span>"#), "{accounts}");
        assert!(
            !html.contains(r#"action="/auth/users/update""#),
            "no editor on the list"
        );
        // A file account has its badge.
        v.accounts.as_mut().unwrap().users.push(UserCard {
            file: true,
            ..card("dave", &["notes"])
        });
        let html = page(&v);
        assert!(html.contains(r#"<a class="rowLink" href="/auth/settings/users/dave">dave</a> <span class="badge">file</span>"#));
    }

    #[test]
    fn each_account_shows_its_first_group_as_a_chip_and_a_plus_chip_with_all_of_them() {
        let cat = catalog();
        let mut v = view(&cat);
        v.groups.push(GroupLine {
            id: "db".into(),
            name: "Database".into(),
            critical: false,
        });
        v.accounts.as_mut().unwrap().users[0].groups =
            vec!["infra".into(), "db".into(), "oncall".into()];
        let html = page(&v);
        let accounts = zone_of(&html, "accounts-title");
        // Alice: one chip, then "+2" with a popover that lists the three groups and their members. The
        // cell is narrow: one chip and "+N" fit, and nothing is cut in the middle of a word.
        assert!(accounts.contains(r#"<summary class="tag tag--more" aria-label="All 3 groups of alice" title="All 3 groups of alice">+2</summary>"#), "{accounts}");
        assert!(accounts.contains(r#"href="/auth/settings?group=infra""#));
        assert!(
            accounts.contains("3 accounts"),
            "the members of infra: alice, bob and the superadmin"
        );
        // A critical group carries the mark in every chip.
        assert!(accounts.contains("#i-bell-ringing"));
        // Bob: one chip, no popover. Carol: no group, a dash.
        let bob =
            &accounts[accounts.find("users/bob").unwrap()..accounts.find("users/carol").unwrap()];
        assert!(
            bob.contains(">Infra</a>") && !bob.contains("tag--more"),
            "{bob}"
        );
        let carol = &accounts[accounts.find("users/carol").unwrap()..];
        assert!(carol.contains(r#"<span class="muted">-</span>"#));
    }

    #[test]
    fn the_buttons_and_the_columns_of_a_feature_that_is_off_are_absent_from_the_dom() {
        let cat = catalog();
        // Panel only: no keys, no inbox, no creation.
        let mut v = view(&cat);
        v.keys = None;
        v.groups.clear();
        {
            let accounts = v.accounts.as_mut().unwrap();
            accounts.features = None;
            accounts.allow_creation = false;
        }
        let html = access_page(&frame(), &v);
        for gone in [
            "Features",
            "Keys</th>",
            "Groups</th>",
            r#"class="feat""#,
            "New user",
            "New key",
            NEW_USER_PATH,
            "keys-title",
            "Group</span>",
        ] {
            assert!(!html.contains(gone), "{gone}");
        }
        assert!(
            html.contains(r#"<div class="access access--1">"#),
            "the accounts alone"
        );
        assert!(html.contains("Last sign-in") && html.contains("Services</th>"));
        assert!(
            html.contains("Open</th>"),
            "the column of the services that an account opens"
        );
        // Keys on, inbox off: the column of the keys, one icon, no group.
        let mut v = view(&cat);
        v.groups.clear();
        {
            let users = &mut v.accounts.as_mut().unwrap().users;
            users[0].grants.push(grant("keys.own.read", PORTAL));
            users[0].grants.push(grant("inbox.receive", PORTAL));
        }
        let html = page(&v);
        assert!(
            html.contains("Keys</th>") && html.contains("aria-label=\"Keys: 1 of 8 permissions\"")
        );
        assert!(!html.contains("Inbox: ") && !html.contains("Groups</th>"));
        assert!(!html.contains("Open</th>"));
    }

    #[test]
    fn new_user_shows_only_when_the_file_allows_it_and_the_grants_are_not_read_only() {
        let cat = catalog();
        let on = page(&view(&cat));
        assert!(on.contains(&format!(r#"href="{NEW_USER_PATH}">"#)));
        assert!(on.contains("New user</a>"));
        // The form that made accounts is not in the page.
        for gone in [
            r#"id="add-user""#,
            r#"action="/auth/users/add""#,
            "Create user",
        ] {
            assert!(!on.contains(gone), "{gone}");
        }
        let mut off = view(&cat);
        off.accounts.as_mut().unwrap().allow_creation = false;
        let html = page(&off);
        assert!(!html.contains("New user") && !html.contains(NEW_USER_PATH));
        let mut fixed = view(&cat);
        fixed.accounts.as_mut().unwrap().mode = Mode::Static;
        let html = page(&fixed);
        assert!(
            !html.contains("New user") && !html.contains(NEW_USER_PATH),
            "static mode"
        );
        let mut hybrid = view(&cat);
        hybrid.accounts.as_mut().unwrap().mode = Mode::Hybrid;
        assert!(page(&hybrid).contains("New user</a>"));
    }

    #[test]
    fn end_all_sessions_is_a_confirmation_in_place_in_the_head_of_the_accounts() {
        let cat = catalog();
        let html = page(&view(&cat));
        assert!(html.contains(r#"<details class="ask ask--danger"><summary class="ib ib--danger" aria-label="End all sessions""#));
        assert!(html.contains(r#"action="/auth/users/end-sessions""#));
        assert!(html.contains(r#"name="confirm" value="end-all-sessions""#));
        assert!(html.contains(r#"name="csrf" value="tok&lt;en""#));
    }

    #[test]
    fn the_filter_row_has_a_user_field_the_groups_as_links_and_a_service_field() {
        let cat = catalog();
        let html = page(&view(&cat));
        let bar = &html[html.find(r#"<form class="fbar""#).unwrap()..];
        let bar = &bar[..bar.find("</form>").unwrap()];
        assert!(bar.contains(r#"method="get" action="/auth/settings""#));
        assert!(bar.contains(r#"name="user" list="filter-users""#));
        assert!(bar.contains(r#"name="service" list="filter-services""#));
        // Each group is a link with its member count: the accounts of the group and the superadmin.
        assert!(bar.contains(r#"<a class="chip" href="/auth/settings?group=infra" title="Members of Infra">Infra<span class="num">3</span></a>"#), "{bar}");
        assert!(bar.contains("On call"), "the critical group has its mark");
        assert!(bar.contains("#i-bell-ringing"));
        // The datalist of the services shows the name, and the id is the label.
        assert!(bar.contains(r#"<option value="Grafana" label="infra-grafana"></option>"#));
        assert!(bar.contains(r#"<option value="alice"></option>"#));
        assert!(!bar.contains("Clear"), "no filter on, no Clear");
        // No filter on a group when the inbox is off.
        let mut off = view(&cat);
        off.groups.clear();
        let html = page(&off);
        assert!(!html.contains("Filter by group"));
        // The bar is for the superadmin: an account with a right of the keys sees the list alone.
        let mut keys_only = view(&cat);
        keys_only.accounts = None;
        let html = access_page(
            &Frame {
                superadmin: false,
                keys: true,
                ..frame()
            },
            &keys_only,
        );
        assert!(!html.contains("fbar") && html.contains(r#"<div class="access access--1">"#));
        assert!(html.contains("keys-title") && !html.contains("accounts-title"));
        assert!(!html.contains("contentTabs"), "one tab, no strip");
    }

    #[test]
    fn a_filter_keeps_the_matching_accounts_and_keys_and_the_counts_follow() {
        let cat = catalog();
        // user=alice
        let mut v = view(&cat);
        v.query.user = "alice".into();
        let html = page(&v);
        let accounts = zone_of(&html, "accounts-title");
        assert!(accounts.contains("users/alice") && !accounts.contains("users/bob"));
        assert!(accounts.contains(r#"<span class="count" title="Accounts that match the filters. The limit is 64.">1/3</span>"#));
        let keys = zone_of(&html, "keys-title");
        assert!(keys.contains("k1-key") && keys.contains("k2-key") && !keys.contains("k3-key"));
        assert!(keys.contains(r#"<span class="count">2/3</span>"#), "{keys}");
        // The chip of the filter has a cross and the link "Show account"; "Clear" shows.
        assert!(html.contains(r#"<span class="fchip">user: alice<a class="fchip__x" href="/auth/settings" aria-label="Remove the filter user alice""#), "{html}");
        assert!(html.contains(
            r#"<a class="fchip__go" href="/auth/settings/users/alice">Show account</a>"#
        ));
        assert!(html.contains(r#"<a class="fclear" href="/auth/settings">Clear</a>"#));
        // group=infra: the members, and the keys with a grant on the group.
        let mut v = view(&cat);
        v.query.group = "infra".into();
        let html = page(&v);
        let accounts = zone_of(&html, "accounts-title");
        assert!(
            accounts.contains("users/alice")
                && accounts.contains("users/bob")
                && !accounts.contains("users/carol")
        );
        let keys = zone_of(&html, "keys-title");
        assert!(keys.contains("k1-key") && !keys.contains("k2-key") && !keys.contains("k3-key"));
        assert!(html.contains(r#"<a class="chip is-on" href="/auth/settings" aria-current="true" title="Members of Infra">"#), "the chip is on and a click removes it");
        assert!(html.contains(r#"<input type="hidden" name="group" value="infra">"#));
        // service=Notes (the name, not the id): the accounts that open it, the keys with a grant on it.
        let mut v = view(&cat);
        v.query.service = "notes".into();
        let html = page(&v);
        let keys = zone_of(&html, "keys-title");
        assert!(keys.contains("k2-key") && !keys.contains("k1-key"));
        assert!(
            html.contains(r#"<span class="fchip">service: Notes<a"#),
            "the name, not the id: {html}"
        );
        assert!(html.contains(r#"name="service" list="filter-services" value="Notes""#));
        // The id works too, and the column of the services becomes the column of the rights.
        let mut v = view(&cat);
        v.query.service = "infra-grafana".into();
        let html = page(&v);
        let accounts = zone_of(&html, "accounts-title");
        assert!(accounts.contains("Permissions</th>") && !accounts.contains("Services</th>"));
        assert!(accounts.contains("users/alice") && !accounts.contains("users/bob"));
        assert!(
            accounts.contains(r#"<td class="num">1 permission</td>"#),
            "{accounts}"
        );
        // A service that publishes no action: its right is "access".
        let mut v = view(&cat);
        v.query.service = "Mixed".into();
        let html = page(&v);
        let accounts = zone_of(&html, "accounts-title");
        assert!(accounts.contains("users/alice") && accounts.contains("users/bob"));
        assert!(
            accounts.contains(r#"<td class="num">access</td>"#),
            "{accounts}"
        );
    }

    #[test]
    fn two_filters_are_a_plain_and_and_the_empty_lists_say_so_with_a_clear_link() {
        let cat = catalog();
        let mut v = view(&cat);
        v.query.group = "infra".into();
        v.query.service = "infra-notes".into();
        let html = page(&v);
        let keys = zone_of(&html, "keys-title");
        assert!(keys.contains("No key matches these filters."));
        assert!(!keys.contains("not both"), "no help sentence");
        assert!(keys.contains(r#"<a href="/auth/settings">Clear</a>"#));
        // A user that does not exist, a group that does not exist, a service that does not exist:
        // empty lists, never an error.
        for (user, group, service) in [("nobody", "", ""), ("", "<x>", ""), ("", "", "<nope>")] {
            let mut v = view(&cat);
            v.query = Query {
                user: user.into(),
                group: group.into(),
                service: service.into(),
                ..Query::default()
            };
            let html = page(&v);
            assert!(
                zone_of(&html, "accounts-title").contains("No account matches these filters."),
                "{user}{group}{service}"
            );
            assert!(zone_of(&html, "keys-title").contains("No key matches these filters."));
            assert!(!html.contains("<x>") && !html.contains("<nope>"), "escaped");
            assert!(html.contains(r#"<a class="fclear""#));
        }
        // No account and no key at all: no filter is on, so the lists say so with no Clear link.
        let mut none = view(&cat);
        none.accounts.as_mut().unwrap().users.clear();
        none.keys.as_mut().unwrap().lines.clear();
        let html = page(&none);
        assert!(html.contains("No account yet.") && html.contains("No key yet."));
    }

    #[test]
    fn the_chips_of_the_kinds_count_what_the_filters_keep_and_the_chip_mine_keeps_the_keys_of_the_viewer(
    ) {
        let cat = catalog();
        let mut v = view(&cat);
        v.query.user = "alice".into();
        let html = page(&v);
        let keys = zone_of(&html, "keys-title");
        // Alice owns one key of each kind: the chips follow the filter.
        assert!(
            keys.contains(r#"aria-label="API key: for services and webhooks 1""#),
            "{keys}"
        );
        assert!(keys.contains(r#"aria-label="MCP key: for agents 1""#));
        // No chip "All": it would repeat the count of the list and filter nothing.
        assert!(!keys.contains(">All<"), "{keys}");
        // The chip of a kind is a link that keeps the filters, and a second click removes it.
        assert!(keys.contains(r#"href="/auth/settings?user=alice&amp;k=api""#));
        let mut v = view(&cat);
        v.query.kind = Some(KeyKind::Api);
        let html = page(&v);
        let keys = zone_of(&html, "keys-title");
        assert!(keys.contains("k1-key") && keys.contains("k3-key") && !keys.contains("k2-key"));
        assert!(
            keys.contains(r#"class="chip is-on" href="/auth/settings" aria-current="true""#),
            "{keys}"
        );
        // "mine": the keys of the viewer. It shows only when the viewer sees the keys of others.
        let mut v = view(&cat);
        v.query.mine = true;
        let html = page(&v);
        let keys = zone_of(&html, "keys-title");
        assert!(keys.contains("k1-key") && keys.contains("k2-key") && !keys.contains("k3-key"));
        assert!(keys.contains(r#"aria-label="My keys 2""#));
        // The chips keep their place when the filters keep no key of a kind: the chip stays, dimmed
        // and with no link, and the chip "mine" stays too.
        let mut v = view(&cat);
        v.query.user = "nobody".into();
        let html = page(&v);
        let keys = zone_of(&html, "keys-title");
        assert!(
            keys.contains(r#"<span class="chip chip--static is-zero" aria-disabled="true" aria-label="API key: for services and webhooks 0""#),
            "{keys}"
        );
        assert!(keys.contains(r#"aria-label="My keys 0""#), "{keys}");
        // One kind in the list: a chip of a kind would filter nothing, so it is absent.
        let mut one = view(&cat);
        one.keys
            .as_mut()
            .unwrap()
            .lines
            .retain(|l| l.kind == Some(KeyKind::Api));
        let html = page(&one);
        let keys = zone_of(&html, "keys-title");
        assert!(
            !keys.contains(r##"<span class="st--off"><svg class="icon" aria-hidden="true"><use href="#i-webhook"/>"##),
            "{keys}"
        );
        let mut own = view(&cat);
        own.keys.as_mut().unwrap().all = false;
        own.keys.as_mut().unwrap().lines.retain(|l| l.mine);
        let html = page(&own);
        assert!(
            !html.contains("My keys") && !html.contains(">Owner</th>"),
            "one owner: no chip, no column"
        );
    }

    #[test]
    fn the_chip_confirm_shows_for_a_key_whose_grant_asks_or_whose_owner_side_asks() {
        let cat = catalog();
        let mut v = view(&cat);
        // No grant asks: no chip.
        let html = page(&v);
        assert!(!zone_of(&html, "keys-title").contains("badge--confirm"));
        // A grant of the key asks.
        let keys = v.keys.as_mut().unwrap();
        keys.lines[1].grants[1].grant = keys.lines[1].grants[1].grant.clone().with_confirm(true);
        let html = page(&v);
        let zone = zone_of(&html, "keys-title");
        assert_eq!(zone.matches("badge--confirm").count(), 1, "{zone}");
        assert!(
            zone.contains(r#"title="1 permission asks for an approval""#),
            "{zone}"
        );
        assert!(zone.contains("#i-lock"), "{zone}");
        // The owner side asks for the other grant: the chip counts both and says why.
        let keys = v.keys.as_mut().unwrap();
        keys.lines[1].grants[0].imposed = true;
        let html = page(&v);
        assert!(
            zone_of(&html, "keys-title").contains(
                r#"title="2 permissions ask for an approval, 1 because the owner asks for it""#
            ),
            "{html}"
        );
    }

    #[test]
    fn the_keys_list_has_kind_icons_a_bulk_revoke_and_a_new_key_link_and_no_button_in_a_row() {
        let cat = catalog();
        let html = page(&view(&cat));
        let keys = zone_of(&html, "keys-title");
        assert!(keys.contains("#i-webhook") && keys.contains("#i-robot"));
        assert!(keys.contains(r#"<a class="button button--primary" href="/auth/keys/new">"#));
        assert!(keys.contains(
            r#"<form id="revoke-keys" class="bulk" method="post" action="/auth/keys/revoke">"#
        ));
        assert!(
            keys.contains(r#"name="section" value="all""#),
            "the viewer may revoke every key"
        );
        assert!(
            keys.contains(r#"name="id.k1" value="1""#)
                && keys.contains(r#"name="id.k3" value="1""#)
        );
        assert!(keys.contains("Revoke the ticked keys?"));
        // The column "Can use" names the groups, escaped.
        assert!(keys.contains("Send Infra &lt;b&gt;"), "{keys}");
        assert!(
            keys.contains("notes · 2 permissions") || keys.contains("infra-notes · 2 permissions"),
            "{keys}"
        );
        assert!(
            keys.contains(r#"href="/auth/keys/edit?id=k1""#)
                && keys.contains(r#"aria-label="Edit k1-key""#)
        );
        assert!(
            !keys.contains(r#"href="/auth/keys/edit?id=k3""#),
            "a key of another account has no edit link"
        );
        // No box and no bulk button without the right to revoke; no "New key" without a kind.
        let mut quiet = view(&cat);
        {
            let list = quiet.keys.as_mut().unwrap();
            list.can_revoke = false;
            list.can_revoke_all = false;
            list.create_kinds.clear();
        }
        let html = page(&quiet);
        assert!(!html.contains("revoke-keys") && !html.contains("bulk__box"));
        assert!(!html.contains("New key") && !html.contains("/auth/keys/new"));
        // A browse as another account makes no change.
        let mut browsing = view(&cat);
        browsing.keys.as_mut().unwrap().impersonated = true;
        let html = page(&browsing);
        assert!(!html.contains("revoke-keys") && !html.contains("New key"));
    }

    #[test]
    fn a_page_has_no_inline_style_and_no_inline_script() {
        let cat = catalog();
        let mut v = view(&cat);
        v.query.user = "<b>\"x\"</b>".into();
        for html in [page(&view(&cat)), page(&v)] {
            assert!(!html.contains(" style=") && !html.contains("<style"));
            assert!(!html.contains("<script>"));
        }
    }
}
