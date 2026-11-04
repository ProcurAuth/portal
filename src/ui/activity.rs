//! The tab Activity: the sign-in attempts and the changes of the accounts. Two chips switch the
//! list, and a third chip keeps the refused attempts. The chips are links, so the page works
//! without script. Above the list, a graph shows the connections of 30 days. The two lists and
//! their chips cover the same 30 days.

use std::fmt::Write as _;

use super::components::{chips, icon, zone, Chip, Zone};
use super::settings::{
    fill_page, notice_html, opt, spark, user_path, Frame, Notice, Tab, ACTIVITY_PATH,
};
use super::status_icon;
use crate::store::{AuditRow, DayStat, LoginPage, LoginRow};
use crate::util::{html_escape, short_agent, time_tag};

/// The data of the tab Activity.
pub struct ActivityView<'a> {
    pub notice: Option<Notice>,
    /// The newest sign-ins, and the figures of the chips.
    pub logins: &'a LoginPage,
    /// The chip "Changes" is on: the list shows the audit events, not the sign-ins.
    pub changes: bool,
    /// The chip "Refused" is on: the list shows the refused sign-ins.
    pub only_refused: bool,
    /// Every real connection, per day, all accounts.
    pub days: &'a [DayStat],
    /// The Unix time of the start of the first day of `days`, in UTC.
    pub first_day: i64,
    /// The latest audit events.
    pub recent_events: &'a [AuditRow],
}

fn href(changes: bool, only_refused: bool) -> String {
    if changes {
        format!("{ACTIVITY_PATH}?v=changes")
    } else if only_refused {
        format!("{ACTIVITY_PATH}?r=failed")
    } else {
        ACTIVITY_PATH.to_string()
    }
}

pub fn activity_page(frame: &Frame, view: &ActivityView) -> String {
    let data = view.logins;
    let mut out = String::new();
    notice_html(&mut out, view.notice);
    let signed = format!(r#"<span class="st--ok">{}</span>"#, icon("check"));
    let pencil = icon("pencil");
    let refused = format!(r#"<span class="st--bad">{}</span>"#, icon("ban"));
    // A chip that is alone is a fact, not a control: it has no link.
    let on_sign_ins = !view.changes && !view.only_refused;
    let on_refused = view.only_refused && !view.changes;
    let shown = [
        data.total > 0 || on_sign_ins,
        !view.recent_events.is_empty() || view.changes,
        data.refused > 0 || on_refused,
    ]
    .iter()
    .filter(|shown| **shown)
    .count();
    let (sign_ins_href, changes_href) = if shown == 1 {
        (String::new(), String::new())
    } else {
        (href(false, false), href(true, false))
    };
    let views = chips(&[
        Chip {
            label_html: &format!("{signed}Sign-ins"),
            name: "Sign-in attempts, 30 days",
            count: data.total as usize,
            href: &sign_ins_href,
            on: on_sign_ins,
        },
        Chip {
            label_html: &format!("{pencil}Changes"),
            name: "Changes, 30 days",
            count: view.recent_events.len(),
            href: &changes_href,
            on: view.changes,
        },
    ]);
    let refused_href = href(false, true);
    let refusals = chips(&[Chip {
        label_html: &refused,
        name: "Refused attempts, 30 days",
        count: data.refused as usize,
        href: if shown == 1 { "" } else { &refused_href },
        on: on_refused,
    }]);
    let connections: u32 = view.days.iter().map(|d| d.sessions).sum();
    let chips_html = if refusals.is_empty() {
        views
    } else {
        format!(r#"{views}<span class="chips__sep"></span>{refusals}"#)
    };
    let tools = format!(
        r#"<span class="count" title="Connections for each day, all accounts">{connections} connections in {} days</span>"#,
        view.days.len()
    );
    let body = if view.changes {
        changes_table(view)
    } else {
        signins_table(view)
    };
    out.push_str(&zone(
        &Zone {
            id: "activity-title",
            title: "Activity",
            count: "",
            count_title: "",
            chips_html: &chips_html,
            tools_html: &tools,
            class: " zone--page",
            above_html: &format!(
                r#"<div class="spark__box">{}{}</div>"#,
                spark(view.days, "spark--strip"),
                spark_axis(view.first_day, view.days.len())
            ),
        },
        &body,
    ));
    fill_page(Tab::Activity, frame, "Activity", &out)
}

/// The marks of the time under the graph: the first day, the middle day and today, in UTC as the
/// bars are. Plain text, so it needs no script.
fn spark_axis(first_day: i64, days: usize) -> String {
    if days < 3 {
        return String::new();
    }
    let mark = |index: usize| {
        let at = first_day + i64::try_from(index).unwrap_or(0) * 86_400;
        crate::util::format_utc(at)[5..10].to_string()
    };
    format!(
        r#"<div class="spark__axis"><span>{}</span><span>{}</span><span>Today</span></div>"#,
        mark(0),
        mark(days / 2)
    )
}

/// The newest attempts, one line each. The head of the table is for a screen reader only.
fn signins_table(view: &ActivityView) -> String {
    if view.logins.rows.is_empty() {
        return format!(
            r#"<p class="empty">{}</p>"#,
            if view.only_refused {
                "No refused sign-in."
            } else {
                "No sign-in recorded yet."
            }
        );
    }
    let mut out = String::from(
        r#"<table class="grid grid--signins"><colgroup><col class="w28"><col class="w136"><col class="w120"><col class="w120"><col></colgroup><thead><tr><th scope="col"><span class="visually-hidden">Result</span></th><th scope="col">When</th><th scope="col">Login typed</th><th scope="col">Address</th><th scope="col">Browser</th></tr></thead><tbody>"#,
    );
    for row in &view.logins.rows {
        login_row(&mut out, row);
    }
    out.push_str("</tbody></table>");
    if view.logins.rows.len() < view.logins.matching as usize {
        let _ = write!(
            out,
            r#"<p class="empty">The list shows the newest {} of {} attempts.</p>"#,
            view.logins.rows.len(),
            view.logins.matching
        );
    }
    out
}

fn login_row(out: &mut String, row: &LoginRow) {
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

/// The audit events: the time, the change, the account, who did it and from where.
fn changes_table(view: &ActivityView) -> String {
    if view.recent_events.is_empty() {
        return r#"<p class="empty">No change recorded yet.</p>"#.to_string();
    }
    // The column of the account fits the longest login.
    let widest = view
        .recent_events
        .iter()
        .filter_map(|e| e.login.as_deref())
        .map(|login| login.chars().count())
        .max()
        .unwrap_or(0)
        .min(20);
    let mut out = format!(
        r#"<table class="grid grid--changes"><colgroup><col class="w136"><col class="w180"><col class="{}"><col class="w88"><col></colgroup><thead><tr><th scope="col">When</th><th scope="col">Change</th><th scope="col">Account</th><th scope="col">By</th><th scope="col">Address</th></tr></thead><tbody>"#,
        super::access::col_class(widest, 88)
    );
    for event in view.recent_events {
        let account = match &event.login {
            Some(login) if !login.is_empty() => format!(
                r#"<a href="{}">{}</a>"#,
                html_escape(&user_path(login)),
                html_escape(login)
            ),
            _ => String::new(),
        };
        let by = match &event.actor {
            Some(actor) if !actor.is_empty() => html_escape(actor),
            _ => String::new(),
        };
        let _ = write!(
            out,
            r#"<tr><td>{}</td><td>{}</td><td>{account}</td><td class="muted">{by}</td><td class="muted">{}</td></tr>"#,
            time_tag(event.ts, false),
            audit_label(&event.kind),
            opt(&event.ip),
        );
    }
    out.push_str("</tbody></table>");
    out
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
        "totp_enabled" => "Two-factor turned on",
        "totp_disabled" => "Two-factor turned off",
        "totp_reset" => "Two-factor reset",
        "recovery_regenerated" => "Recovery codes renewed",
        "recovery_used" => "Recovery code used",
        "mfa_failed" => "Refused second factor",
        _ => "Event",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::fixtures::*;

    fn view(data: &Data) -> ActivityView<'_> {
        ActivityView {
            notice: None,
            logins: &data.logins,
            changes: false,
            only_refused: false,
            days: &data.days,
            first_day: FIRST_DAY,
            recent_events: &data.events,
        }
    }

    fn page(view: &ActivityView) -> String {
        activity_page(&full_frame(), view)
    }

    #[test]
    fn the_sign_ins_list_has_chips_that_are_links_and_one_line_for_each_attempt() {
        let d = data();
        let html = page(&view(&d));
        let panel = &html[html.find(r#"aria-labelledby="activity-title""#).unwrap()..];
        let panel = &panel[..panel.find("</section>").unwrap()];
        // The chips switch the list and filter it with a query parameter, so they work without script.
        assert!(
            panel.contains(r#"href="/auth/settings/activity" aria-current="true""#),
            "{panel}"
        );
        assert!(panel.contains(r#"href="/auth/settings/activity?v=changes""#));
        assert!(panel.contains(r#"href="/auth/settings/activity?r=failed""#));
        assert!(panel.contains(
            r#"aria-label="Refused attempts, 30 days 3" title="Refused attempts, 30 days""#
        ));
        assert!(!panel.contains("<button"), "no script needed to filter");
        // The graph has a name and its figure is in the text.
        assert!(panel.contains(r#"<svg class="spark spark--strip""#));
        // The marks: the first day, the middle day (the day 15 of 30) and today.
        assert!(
            panel.contains(
                r#"<div class="spark__axis"><span>10-06</span><span>10-21</span><span>Today</span></div>"#
            ),
            "{panel}"
        );
        assert!(panel.contains("4 connections in 30 days"));
        // One line for each attempt: the state is an icon with a name, the time is a local time.
        assert_eq!(
            panel.matches("<tr>").count(),
            2 + 1,
            "two attempts and the head"
        );
        assert!(panel.contains(r#"<span class="visually-hidden">Signed in</span>"#));
        assert!(panel.contains(r#"title="Refused""#));
        assert!(panel.contains(r#"<time class="when" datetime="#));
        assert!(panel.contains("203.0.113.7") && panel.contains("Firefox 131"));
        // What was typed is text.
        assert!(panel.contains("&lt;b&gt;x&lt;/b&gt;") && !panel.contains("<b>x</b>"));
        assert!(panel.contains("No such account"));
        // The filter is on: the chip of the refused attempts is the current one.
        let mut v = view(&d);
        v.only_refused = true;
        let html = page(&v);
        assert!(html.contains(
            r#"<a class="chip is-on" href="/auth/settings/activity?r=failed" aria-current="true""#
        ));
        // A chip with a count of zero is absent (CTRL-15).
        let mut none = data();
        none.logins.refused = 0;
        let html = page(&view(&none));
        assert!(!html.contains("r=failed"));
        // No pages: no Newer, no Older.
        assert!(!html.contains("Newer") && !html.contains("Older"));
        // The graph is always there: no column or block hides at a small width.
        assert!(!html.contains("wide"), "nothing hides below 1400 px");
        // The chip "Sign-ins" counts the rows of the period, and the chip "Changes" too.
        let mut v = view(&d);
        v.changes = true;
        let html = page(&v);
        assert!(
            html.contains(r#"aria-label="Changes, 30 days 1""#),
            "{html}"
        );
        assert!(html.contains(r#"aria-label="Sign-in attempts, 30 days 12""#));
        // A list cut at the limit says so, and the chip keeps the whole figure.
        let mut cut = data();
        cut.logins.matching = 1_500;
        cut.logins.total = 1_500;
        let html = page(&view(&cut));
        assert!(html.contains("The list shows the newest 2 of 1500 attempts."));
    }

    #[test]
    fn a_chip_that_is_alone_is_a_fact_with_no_link() {
        let mut d = data();
        d.logins.refused = 0;
        d.events.clear();
        let html = page(&view(&d));
        assert!(
            html.contains(
                r#"<span class="chip chip--static is-on" aria-label="Sign-in attempts, 30 days 12""#
            ),
            "{html}"
        );
        assert!(!html.contains("?v=changes") && !html.contains("r=failed"));
        // With a second chip both are links.
        let d = data();
        assert!(page(&view(&d)).contains(r#"<a class="chip is-on" href="/auth/settings/activity""#));
    }

    #[test]
    fn the_changes_chip_shows_the_audit_events_and_the_other_list_is_not_drawn() {
        let d = data();
        let mut v = view(&d);
        v.changes = true;
        let html = page(&v);
        assert!(html.contains(
            r#"<a class="chip is-on" href="/auth/settings/activity?v=changes" aria-current="true""#
        ));
        assert!(html.contains("Permissions changed"));
        assert!(html.contains(r#"<a href="/auth/settings/users/alice">alice</a>"#));
        assert!(html.contains(r#"<td class="muted">root</td><td class="muted">192.0.2.10</td>"#));
        assert!(!html.contains("grid--signins"));
        let mut empty = data();
        empty.events.clear();
        empty.logins = LoginPage::default();
        let mut v = view(&empty);
        assert!(page(&v).contains("No sign-in recorded yet."));
        v.changes = true;
        assert!(page(&v).contains("No change recorded yet."));
        // The chip with no count is absent: an empty log has no chip to click.
        assert!(!page(&v).contains("?r=failed"));
        let html = page(&v);
        for gone in ["settings/logins", "Connections</h2>", "Recent changes</h2>"] {
            assert!(!main_of(&html).contains(gone), "{gone}");
        }
    }
}
