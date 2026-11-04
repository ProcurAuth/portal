//! The parts of the page for the notifications: the badge of the tab Inbox, the toasts, and the
//! inbox icon of the launcher with its popover.
//!
//! Each part works without script. The popover is a `details` element. The forms are plain POST
//! forms with the field `back`. The script (`web/notify.js`) adds the polling, the toasts and the
//! update of the badges. It finds its elements by data attributes, and the markup of these parts
//! names its classes in the shared stylesheet only: the launcher goes through the pass of
//! `ui::mask`, so no rule here depends on the structure.

use std::fmt::Write as _;

use super::components::icon;
use crate::inbox::notify::{Importance, Level};
use crate::util::{format_utc_instant, html_escape};

/// The address that the script asks.
pub const POLL_PATH: &str = "/auth/settings/inbox/notifications";
/// The form that moves the marker "read up to".
pub const READ_PATH: &str = "/auth/settings/inbox/read";
/// The form that sets the level of the toasts.
pub const LEVEL_PATH: &str = "/auth/settings/inbox/notify";
/// The values of the field `back` of the two forms: the page that the answer returns to.
pub const BACK_HOME: &str = "home";
pub const BACK_INBOX: &str = "inbox";

/// The rows that the popover lists.
pub const ROWS_SHOWN: usize = 8;

/// What a page needs to draw the badge and to start the script.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Header {
    pub level: Level,
    /// The alerts newer than the marker, up to the cap.
    pub unread: u32,
    /// The time of the page, in milliseconds: the script asks for the alerts after it.
    pub cursor_ms: i64,
}

/// One alert in the list of the popover.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Row {
    pub id: String,
    pub title: String,
    /// The name of the group, or "Direct".
    pub group: String,
    pub importance: Importance,
    pub created_at_ms: i64,
    /// The row of the alert in the page Inbox. `None`: the account cannot read the alert.
    pub href: Option<String>,
    pub unread: bool,
}

/// The popover of the launcher.
pub struct Bell<'a> {
    pub header: Header,
    pub rows: &'a [Row],
    pub now_ms: i64,
    /// The token of the forms (the purpose `inbox`).
    pub csrf: &'a str,
}

/// The word of a level of an alert.
pub fn importance_word(importance: Importance) -> &'static str {
    match importance {
        Importance::Critical => "Critical",
        Importance::Alert => "Alert",
        Importance::Ring => "Ring",
    }
}

fn importance_icon(importance: Importance) -> &'static str {
    match importance {
        Importance::Critical => "alert-triangle",
        Importance::Alert => "bell",
        Importance::Ring => "bell-ringing",
    }
}

const LEVELS: [Importance; 3] = [Importance::Critical, Importance::Alert, Importance::Ring];

/// The unread count. The script sets the number and the attribute `hidden`. A count at the cap
/// shows "99+".
fn count_text(unread: u32) -> String {
    if unread >= crate::inbox::notify::UNREAD_CAP {
        format!("{}+", crate::inbox::notify::UNREAD_CAP - 1)
    } else {
        unread.to_string()
    }
}

/// The badge. It is absent (`hidden`) with no unread alert, and with the level `Off`.
pub fn badge(header: &Header) -> String {
    let hidden = if header.unread == 0 || header.level == Level::Off {
        " hidden"
    } else {
        ""
    };
    format!(
        r#"<span class="nbadge" data-notify-badge{hidden}><span data-notify-n>{}</span><span class="visually-hidden"> unread</span></span>"#,
        count_text(header.unread)
    )
}

/// The icons of the three levels, one of them for a row, all of them for a template: the script
/// keeps the one that fits.
fn level_icons(only: Option<Importance>) -> String {
    let mut out = String::new();
    for level in LEVELS.into_iter().filter(|l| only.is_none_or(|o| o == *l)) {
        let _ = write!(
            out,
            r##"<svg class="icon" aria-hidden="true" data-for="{}"><use href="#i-{}"/></svg>"##,
            level.as_str(),
            importance_icon(level)
        );
    }
    out
}

/// One row of the list. `None` makes the template that the script copies.
fn row(row: Option<&Row>, now_ms: i64) -> String {
    let (level, title, group, time, instant, unread, href) = match row {
        Some(r) => (
            r.importance.as_str(),
            html_escape(&r.title),
            html_escape(&r.group),
            age(now_ms, r.created_at_ms),
            format_utc_instant(r.created_at_ms / 1000),
            if r.unread { " data-unread" } else { "" },
            r.href.as_deref(),
        ),
        None => (
            "",
            String::new(),
            String::new(),
            String::new(),
            String::new(),
            "",
            None,
        ),
    };
    let inner = format!(
        r#"<span class="nrow__ico">{}</span><span class="nrow__body"><span class="nrow__title" data-row-title>{title}</span><span class="nrow__meta" data-row-group>{group}</span></span><time class="nrow__time" data-row-time datetime="{instant}" title="{instant}">{time}</time>"#,
        level_icons(row.map(|r| r.importance)),
    );
    // A template always has a link: the script removes it for an alert that has no page.
    let link = match (row, href) {
        (Some(_), Some(href)) => format!(
            r#"<a class="nrow__in" data-row-link href="{}">{inner}</a>"#,
            html_escape(href)
        ),
        (Some(_), None) => format!(r#"<span class="nrow__in">{inner}</span>"#),
        (None, _) => format!(
            r#"<a class="nrow__in" data-row-link href="{}">{inner}</a>"#,
            super::settings::INBOX_PATH
        ),
    };
    format!(r#"<li class="nrow" data-level="{level}"{unread}>{link}</li>"#)
}

/// The age of a time: `now`, `18m`, `2h`, `3d`.
fn age(now_ms: i64, then_ms: i64) -> String {
    let secs = (now_ms - then_ms).max(0) / 1000;
    match secs {
        0..=59 => "now".to_string(),
        60..=3599 => format!("{}m", secs / 60),
        3600..=86_399 => format!("{}h", secs / 3600),
        _ => format!("{}d", secs / 86_400),
    }
}

fn hidden_if(on: bool) -> &'static str {
    if on {
        " hidden"
    } else {
        ""
    }
}

/// The control of the level: three buttons in one form. `back` is the page that the answer
/// returns to. The button of the current level is pressed.
pub fn level_form(level: Level, csrf: &str, back: &str) -> String {
    let mut out = format!(
        r#"<form class="nlevel" method="post" action="{LEVEL_PATH}" role="group" aria-label="Toasts" data-notify-form><input type="hidden" name="csrf" value="{}"><input type="hidden" name="back" value="{back}">"#,
        html_escape(csrf)
    );
    for (each, word, label) in [
        (Level::All, "All", "All alerts"),
        (Level::Important, "Alerts", "Alert and Critical only"),
        (Level::Off, "Off", "No toast, no badge"),
    ] {
        let _ = write!(
            out,
            r#"<button class="nlevel__b" type="submit" name="level" value="{}" aria-pressed="{}" title="{label}" data-notify-level="{}">{word}</button>"#,
            each.as_str(),
            each == level,
            each.as_str()
        );
    }
    out.push_str("</form>");
    out
}

/// The container of the toasts, with the data for the script. Every page that has the badge or
/// the icon has it, once, at the end of the body. The two live regions are in the page from the
/// start: a screen reader announces what the script adds to them. A critical toast goes to the
/// assertive one.
pub fn toasts(header: &Header) -> String {
    format!(
        r##"<div class="toasts" data-notify="{POLL_PATH}" data-cursor="{}" data-level="{}"><div class="toasts__live" role="status" aria-live="polite" data-toast-polite></div><div class="toasts__live" role="alert" aria-live="assertive" data-toast-assertive></div><a class="toasts__more" href="{}" data-notify-more hidden></a><template data-notify-toast><div class="toast" data-level=""><span class="toast__ico">{}</span><span class="toast__body"><span class="toast__title" data-toast-title></span><span class="toast__meta"><span data-toast-group></span> <span data-toast-level></span></span><a class="toast__link" href="{}" data-toast-link>Open</a></span><button class="toast__close" type="button" data-toast-close aria-label="Dismiss"><svg class="icon" aria-hidden="true"><use href="#i-x"/></svg></button></div></template></div>"##,
        header.cursor_ms,
        header.level.as_str(),
        super::settings::INBOX_PATH,
        level_icons(None),
        super::settings::INBOX_PATH,
    )
}

/// The icon of the launcher with its popover. It sits next to the gear.
pub fn bell(bell: &Bell) -> String {
    let h = &bell.header;
    let mut list = String::new();
    for r in bell.rows {
        list.push_str(&row(Some(r), bell.now_ms));
    }
    let empty = bell.rows.is_empty();
    format!(
        r##"<details class="nbell" data-notify-bell><summary class="button button--icon" title="Notifications">{}<span class="visually-hidden">Notifications</span>{}</summary><div class="nbell__panel"><div class="nbell__head"><span class="nbell__title">Notifications</span><form method="post" action="{READ_PATH}" data-notify-form><input type="hidden" name="csrf" value="{csrf}"><input type="hidden" name="back" value="{BACK_HOME}"><button class="nbell__act" type="submit">Mark all read</button></form></div><ul class="nlist" data-notify-list{}>{list}</ul><p class="nbell__empty" data-notify-empty{}>No notification</p><div class="nbell__foot"><a class="nbell__open" href="{}">Open the inbox</a>{}</div><template data-notify-row>{}</template></div></details>"##,
        icon("inbox"),
        badge(h),
        hidden_if(empty),
        hidden_if(!empty),
        super::settings::INBOX_PATH,
        level_form(h.level, bell.csrf, BACK_HOME),
        row(None, bell.now_ms),
        csrf = html_escape(bell.csrf),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn header(level: Level, unread: u32) -> Header {
        Header {
            level,
            unread,
            cursor_ms: 1_700_000_000_000,
        }
    }

    fn rows() -> Vec<Row> {
        vec![
            Row {
                id: "aa".into(),
                title: "Disk <full>".into(),
                group: "Infra & co".into(),
                importance: Importance::Critical,
                created_at_ms: 1_700_000_000_000 - 180_000,
                href: Some("/auth/settings/inbox?open=aa#a-aa".into()),
                unread: true,
            },
            Row {
                id: "bb".into(),
                title: "Hello".into(),
                group: "Direct".into(),
                importance: Importance::Ring,
                created_at_ms: 1_700_000_000_000 - 7_200_000,
                href: None,
                unread: false,
            },
        ]
    }

    #[test]
    fn the_badge_shows_the_count_and_hides_at_zero_and_when_off() {
        assert!(badge(&header(Level::All, 3)).contains(">3<"));
        assert!(!badge(&header(Level::All, 3)).contains("hidden>"));
        assert!(badge(&header(Level::All, 0)).contains(" hidden"));
        assert!(badge(&header(Level::Off, 7)).contains(" hidden"));
        assert!(badge(&header(Level::All, 100)).contains(">99+<"));
        // The badge has a word for a screen reader.
        assert!(badge(&header(Level::All, 3)).contains("visually-hidden"));
    }

    #[test]
    fn the_popover_is_a_details_element_with_the_rows_the_forms_and_the_link() {
        let rows = rows();
        let html = bell(&Bell {
            header: header(Level::All, 1),
            rows: &rows,
            now_ms: 1_700_000_000_000,
            csrf: "tok\"en",
        });
        assert!(html.starts_with(r#"<details class="nbell""#), "{html}");
        assert!(html.contains("<summary"), "it opens without script");
        // The text of an alert is escaped.
        assert!(html.contains("Disk &lt;full&gt;") && !html.contains("Disk <full>"));
        assert!(html.contains("Infra &amp; co"));
        assert!(html.contains(r#"data-level="critical" data-unread"#));
        // A row that the account cannot read has no link.
        assert!(html.contains(r#"href="/auth/settings/inbox?open=aa#a-aa""#));
        assert_eq!(
            html.matches("data-row-link").count(),
            2,
            "one row and the template"
        );
        assert!(html.contains(">3m<") && html.contains(">2h<"));
        assert!(html.contains("Mark all read") && html.contains("Open the inbox"));
        assert!(html.contains(r#"name="csrf" value="tok&quot;en""#));
        assert!(html.contains(r#"action="/auth/settings/inbox/read""#));
        assert!(html.contains(r#"value="home""#));
        // The empty state is there and hidden while the list has rows.
        assert!(html.contains("data-notify-empty hidden"));
        let empty = bell(&Bell {
            header: header(Level::All, 0),
            rows: &[],
            now_ms: 0,
            csrf: "t",
        });
        assert!(
            empty.contains("data-notify-list hidden")
                && !empty.contains("data-notify-empty hidden")
        );
    }

    #[test]
    fn the_level_control_presses_the_current_level() {
        for (level, pressed) in [
            (Level::All, "all"),
            (Level::Important, "important"),
            (Level::Off, "off"),
        ] {
            let html = level_form(level, "t", BACK_INBOX);
            for each in ["all", "important", "off"] {
                let mark = format!(r#"value="{each}" aria-pressed="{}""#, each == pressed);
                assert!(html.contains(&mark), "{level:?} {each}: {html}");
            }
            assert!(html.contains(r#"name="back" value="inbox""#));
        }
    }

    #[test]
    fn the_toasts_have_two_live_regions_and_a_template_with_a_named_close_button() {
        let html = toasts(&header(Level::Important, 2));
        assert!(html.contains(r#"role="status" aria-live="polite""#));
        assert!(html.contains(r#"role="alert" aria-live="assertive""#));
        assert!(html.contains(r#"aria-label="Dismiss""#));
        assert!(html.contains(r#"data-cursor="1700000000000""#));
        assert!(html.contains(r#"data-level="important""#));
        assert!(html.contains("<template data-notify-toast>"));
        // Nothing in the markup is an inline style or an inline script.
        assert!(!html.contains("style=") && !html.contains("<script") && !html.contains(" on"));
    }
}
