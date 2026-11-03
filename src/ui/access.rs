//! The tab Access: the accounts of the portal. The superadmin is not a row of the table: the
//! configuration file sets it, and it holds every right.
//!
//! A row opens the page of its account. The columns of the keys and of the features are absent when
//! the file turns the feature off.

use std::fmt::Write as _;

use super::components::{ask, icon, icon_count, toolbar, zone_bare, Ask, Side, Strength};
use super::settings::{
    feature_rights, fill_page, notice_html, plural, summary, user_path, Feature, Frame, Notice,
    ServiceActions, Tab, UserCard, NEW_USER_PATH,
};
use super::status_icon;
use crate::catalog::Catalog;
use crate::grants::Mode;
use crate::util::{html_escape, time_tag};

/// The data of the tab Access.
pub struct AccessView<'a> {
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
}

impl AccessView<'_> {
    /// The page offers "New user": the file allows it and the grants are not read-only.
    fn can_create(&self) -> bool {
        self.allow_creation && self.mode.editable()
    }
}

pub fn access_page(frame: &Frame, view: &AccessView) -> String {
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
    out.push_str(&zone_bare("Accounts", &accounts_table(view)));
    fill_page(Tab::Access, frame, "Access", &out)
}

/// The table of the accounts. The columns of the features and of the keys are absent when the
/// file turns the feature off.
fn accounts_table(view: &AccessView) -> String {
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

fn account_row(out: &mut String, view: &AccessView, user: &UserCard) {
    let login = html_escape(&user.login);
    let _ = write!(
        out,
        r#"<tr><th scope="row"><a class="rowLink" href="{}">{login}</a>"#,
        html_escape(&user_path(&user.login))
    );
    if user.file {
        out.push_str(r#" <span class="badge">file</span>"#);
    }
    out.push_str("</th><td>");
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
    out.push_str("</td>");
    if view.keys || view.inbox {
        out.push_str("<td>");
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
        out.push_str("</td>");
    }
    if view.keys {
        let _ = write!(
            out,
            r#"<td class="r num{}">{}</td>"#,
            if user.keys == 0 { " muted" } else { "" },
            user.keys
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::fixtures::*;
    use crate::ui::settings::PORTAL;

    #[test]
    fn the_access_page_is_a_table_of_accounts_that_link_to_their_own_page() {
        let cat = catalog();
        let mut v = view(&cat);
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
        let html = access_page(&full_frame(), &v);
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
        let html = access_page(&full_frame(), &v);
        assert!(html.contains(r#"<a class="rowLink" href="/auth/settings/users/dave">dave</a> <span class="badge">file</span>"#));
        assert!(
            html.contains(">3 of 64<"),
            "the limit counts the database only"
        );
    }

    #[test]
    fn the_columns_of_a_feature_that_is_off_are_absent_from_the_dom() {
        let cat = catalog();
        // Panel only: the file has neither the block keys nor the block inbox.
        let mut v = view(&cat);
        v.keys = false;
        v.inbox = false;
        v.features = None;
        v.allow_creation = false;
        let html = access_page(&frame(), &v);
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
        let mut v = view(&cat);
        v.inbox = false;
        v.users[0].grants.push(grant("keys.own.read", PORTAL));
        v.users[0].grants.push(grant("inbox.receive", PORTAL));
        let html = access_page(&frame(), &v);
        assert!(html.contains("Keys</th>") && html.contains("aria-label=\"Keys: 1 of 8 rights\""));
        assert!(!html.contains("Inbox: "), "the inbox is off");
    }

    #[test]
    fn new_user_shows_only_when_the_file_allows_it_and_the_grants_are_not_read_only() {
        let cat = catalog();
        let on = access_page(&frame(), &view(&cat));
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
        let mut off = view(&cat);
        off.allow_creation = false;
        let html = access_page(&frame(), &off);
        assert!(!html.contains("New user") && !html.contains(NEW_USER_PATH));
        let mut fixed = view(&cat);
        fixed.mode = Mode::Static;
        let html = access_page(&frame(), &fixed);
        assert!(
            !html.contains("New user") && !html.contains(NEW_USER_PATH),
            "static mode"
        );
        let mut hybrid = view(&cat);
        hybrid.mode = Mode::Hybrid;
        assert!(access_page(&frame(), &hybrid).contains("New user</a>"));
    }

    #[test]
    fn end_all_sessions_is_a_confirmation_in_place_in_the_toolbar() {
        let cat = catalog();
        let html = access_page(&frame(), &view(&cat));
        assert!(html.contains(r#"<details class="ask ask--danger"><summary class="ib ib--danger" aria-label="End all sessions""#));
        assert!(html.contains(r#"action="/auth/users/end-sessions""#));
        assert!(html.contains(r#"name="confirm" value="end-all-sessions""#));
        assert!(html.contains(r#"name="csrf" value="tok&lt;en""#));
        assert!(!html.contains("dangerRow") && !html.contains("incident"));
    }

}
