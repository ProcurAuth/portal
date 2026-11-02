//! The shared parts of the settings pages: the pieces that a page builds its screen from.
//!
//! Each part is a function that returns HTML. Each part works without script. A script
//! (`settings.js`) can improve it in place, but no part needs it.
//!
//! | Part | Function | Without script |
//! |---|---|---|
//! | Icon, icon link, icon button | `icon`, `icon_link`, `icon_post` | A link or a form. |
//! | Count chip that filters | `chips` | A link with a query parameter. |
//! | Panel with a head and a bounded list | `zone` | A `section` with its own scroller. |
//! | Popover | `popover` | A `details` element. |
//! | Confirm in place | `ask` | A `details` element that holds a form. |
//! | Bulk action | `bulk_*` | A form. The count needs script. |
//! | Two columns | `split` | A grid that stacks under 1000 px. |
//!
//! Everything that comes from outside goes through `html_escape` here. The caller gives text, and
//! the function escapes it. The parameters that end with `_html` are the exception: they hold
//! markup that the caller built.
//!
//! The pieces that only the pages of batch D2 use (bulk action, icon with a count as a button) have
//! no caller in the first pages. Their tests are in this file.
#![allow(dead_code)]

use std::fmt::Write as _;

use crate::util::html_escape;

/// An icon of the sprite. The page inlines the symbol: the page builder scans the markup for
/// `href="#i-name"`.
pub fn icon(name: &str) -> String {
    format!(r##"<svg class="icon" aria-hidden="true"><use href="#i-{name}"/></svg>"##)
}

/// A flat icon link. The icon is the whole content, so `label` is its name for a screen reader and
/// its tooltip.
pub fn icon_link(href: &str, label: &str, name: &str) -> String {
    format!(
        r#"<a class="ib" href="{}" aria-label="{}" title="{}">{}</a>"#,
        html_escape(href),
        html_escape(label),
        html_escape(label),
        icon(name)
    )
}

/// A form with one flat icon button: an action that needs no confirmation. `fields_html` holds the
/// hidden fields, with the anti-CSRF token.
pub fn icon_post(action: &str, label: &str, name: &str, fields_html: &str) -> String {
    format!(
        r#"<form class="inline" method="post" action="{}">{fields_html}<button class="ib" type="submit" aria-label="{}" title="{}">{}</button></form>"#,
        html_escape(action),
        html_escape(label),
        html_escape(label),
        icon(name)
    )
}

/// An icon with a count, as a link: a button of a toolbar. `label` names it ("Phones: 3").
pub fn icon_count_link(href: &str, label: &str, name: &str, count: usize) -> String {
    format!(
        r#"<a class="ib ib--count" href="{}" aria-label="{}" title="{}">{}<span class="num">{count}</span></a>"#,
        html_escape(href),
        html_escape(label),
        html_escape(label),
        icon(name)
    )
}

/// An icon with a count that is not a control, for a table cell. `label` names it for a screen
/// reader, and shows in the tooltip.
pub fn icon_count(label: &str, name: &str, count: usize) -> String {
    format!(
        r#"<span class="feat" role="img" aria-label="{}" title="{}">{}<span class="num">{count}</span></span>"#,
        html_escape(label),
        html_escape(label),
        icon(name)
    )
}

/// One count chip. A click filters the table of its panel.
pub struct Chip<'a> {
    /// The text of the chip, or its icon (markup). A chip with an icon needs a `name`.
    pub label_html: &'a str,
    /// The name of the chip for a screen reader and for the tooltip. Empty: the label is enough.
    pub name: &'a str,
    pub count: usize,
    /// Where the chip goes: the same page with a query parameter. The chip is a link, so it works
    /// without script.
    pub href: &'a str,
    /// The filter is on.
    pub on: bool,
}

/// A group of chips. A chip with a count of zero is absent, unless it is the one that is on.
/// No chip at all gives an empty string.
pub fn chips(items: &[Chip]) -> String {
    let shown: Vec<&Chip> = items.iter().filter(|c| c.count > 0 || c.on).collect();
    if shown.is_empty() {
        return String::new();
    }
    let mut out = String::from(r#"<span class="chips" role="group" aria-label="Filter">"#);
    for chip in shown {
        let _ = write!(
            out,
            r#"<a class="chip{}" href="{}"{}{}>{}<span class="num">{}</span></a>"#,
            if chip.on { " is-on" } else { "" },
            html_escape(chip.href),
            if chip.on {
                r#" aria-current="true""#
            } else {
                ""
            },
            if chip.name.is_empty() {
                String::new()
            } else {
                format!(
                    r#" aria-label="{0} {1}" title="{0}""#,
                    html_escape(chip.name),
                    chip.count
                )
            },
            chip.label_html,
            chip.count
        );
    }
    out.push_str("</span>");
    out
}

/// The head of a panel: a small title, a count, the chips, then the tools at the right.
pub struct Zone<'a> {
    pub id: &'a str,
    pub title: &'a str,
    /// Muted text after the title: a count, or the name of the selected row. Empty: none.
    pub count: &'a str,
    pub chips_html: &'a str,
    pub tools_html: &'a str,
    /// More classes of the panel, with a space before the first: " zone--short". Empty: none.
    pub class: &'a str,
}

/// A panel: its head, then `body_html` in a list with its own scroller. The page does not scroll
/// for a long list: the list does. `body_html` is a table (`grid`) or any markup.
pub fn zone(z: &Zone, body_html: &str) -> String {
    let mut out = format!(
        r#"<section class="zone{}" aria-labelledby="{}"><div class="zone__head"><h2 class="title" id="{}">{}</h2>"#,
        html_escape(z.class),
        html_escape(z.id),
        html_escape(z.id),
        html_escape(z.title)
    );
    if !z.count.is_empty() {
        let _ = write!(
            out,
            r#"<span class="count">{}</span>"#,
            html_escape(z.count)
        );
    }
    out.push_str(z.chips_html);
    out.push_str(r#"<span class="grow"></span>"#);
    out.push_str(z.tools_html);
    let _ = write!(
        out,
        r#"</div><div class="scroll">{body_html}</div></section>"#
    );
    out
}

/// A panel with no head: the toolbar above it names it. `label` is its name for a screen reader.
pub fn zone_bare(label: &str, body_html: &str) -> String {
    format!(
        r#"<section class="zone zone--bare" aria-label="{}"><div class="scroll">{body_html}</div></section>"#,
        html_escape(label)
    )
}

/// The toolbar of a page: a title, a count, then the tools at the right.
pub fn toolbar(title: &str, count: &str, tools_html: &str) -> String {
    let mut out = format!(
        r#"<div class="tbar"><h2 class="title">{}</h2>"#,
        html_escape(title)
    );
    if !count.is_empty() {
        let _ = write!(out, r#"<span class="count">{}</span>"#, html_escape(count));
    }
    let _ = write!(out, r#"<span class="grow"></span>{tools_html}</div>"#);
    out
}

/// Two panels side by side. They stack under 1000 px. `class` names the proportion: `users`,
/// `services` or `even`.
pub fn split(class: &str, left_html: &str, right_html: &str) -> String {
    format!(
        r#"<div class="split split--{}">{left_html}<div class="stackcol">{right_html}</div></div>"#,
        html_escape(class)
    )
}

/// Where a popover opens: under its button, from the left edge or from the right edge.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Side {
    Start,
    End,
}

/// A popover: an icon button that opens a small panel. A click outside, the key Escape and
/// another popover close it. The panel does not move the page: it floats over it.
///
/// It is a `details` element, so it opens without script. The script adds the closing.
pub fn popover(label: &str, icon_name: &str, side: Side, wide: bool, body_html: &str) -> String {
    format!(
        r#"<details class="pop"><summary class="ib" aria-label="{0}" title="{0}">{1}</summary><div class="pop__menu{2}{3}">{body_html}</div></details>"#,
        html_escape(label),
        icon(icon_name),
        if side == Side::End {
            " pop__menu--end"
        } else {
            ""
        },
        if wide { " pop__menu--wide" } else { "" },
    )
}

/// How much a confirmation asks for. It follows the importance of the resource.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Strength {
    /// One click on the confirmation. For a thing that the person makes again in a minute.
    Light,
    /// The person types the name of the resource first. For a thing that cannot be made again.
    /// The field has a `pattern`, so the browser keeps the button from sending without script.
    Firm,
}

/// A confirmation in place: the button turns into a cross, and the question opens at its left, in
/// the same line. Nothing moves.
pub struct Ask<'a> {
    /// The name of the opener, for a screen reader and for the tooltip.
    pub label: &'a str,
    pub icon: &'a str,
    /// The question, as text: "Delete carol and 3 keys?".
    pub question: &'a str,
    /// The word of the confirm button: "Delete".
    pub verb: &'a str,
    pub action: &'a str,
    /// The hidden fields of the form, with the anti-CSRF token (markup).
    pub fields_html: &'a str,
    pub strength: Strength,
    /// The text that a `Firm` confirmation asks for.
    pub name: &'a str,
    /// A destructive action wears the colour of danger.
    pub danger: bool,
    /// Where the question opens: `Start` puts it at the right of the cross, `End` at the left. A
    /// button at the end of a toolbar uses `End`.
    pub side: Side,
}

/// `ask` as markup.
pub fn ask(a: &Ask) -> String {
    let danger = format!(
        "{}{}",
        if a.danger { " ask--danger" } else { "" },
        if a.side == Side::Start {
            " ask--start"
        } else {
            ""
        }
    );
    let mut out = format!(
        r#"<details class="ask{danger}"><summary class="ib{}" aria-label="{}" title="{}"><span class="ask__open">{}</span><span class="ask__close">{}</span></summary><form class="ask__panel" method="post" action="{}">{}<span class="ask__q">{}</span>"#,
        if a.danger { " ib--danger" } else { "" },
        html_escape(a.label),
        html_escape(a.label),
        icon(a.icon),
        icon("x"),
        html_escape(a.action),
        a.fields_html,
        html_escape(a.question),
    );
    if a.strength == Strength::Firm {
        let _ = write!(
            out,
            r#"<input class="uiInput ask__name" type="text" required pattern="{}" autocomplete="off" autocapitalize="off" spellcheck="false" aria-label="Type {} to confirm" placeholder="{}">"#,
            html_escape(&regex_escape(a.name)),
            html_escape(a.name),
            html_escape(a.name),
        );
    }
    let _ = write!(
        out,
        r#"<button class="button{}" type="submit">{}</button></form></details>"#,
        if a.danger {
            " button--danger"
        } else {
            " button--primary"
        },
        html_escape(a.verb),
    );
    out
}

/// The text as a pattern of an input: the browser reads it as a regular expression in unicode mode,
/// where only the syntax characters take a backslash. The pattern must match the whole value.
fn regex_escape(text: &str) -> String {
    let mut out = String::new();
    for c in text.chars() {
        if "\\^$.|?*+()[]{}/".contains(c) {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

// The bulk action ------------------------------------------------------------------------------

/// The bulk action of a table (variant A of the plan): a checkbox in each row, and one button in
/// the head of the panel that shows the count of the ticked rows: "Revoke (3)". A click on it asks
/// "Revoke 3 phones?" in the same place.
///
/// The form is empty. The checkboxes and the button name it with the attribute `form`, so the
/// table and the head of the panel can sit anywhere in the page.
pub struct Bulk<'a> {
    /// The id of the form. The page needs one id for each bulk action.
    pub id: &'a str,
    pub action: &'a str,
    /// The name of each ticked row in the form.
    pub field: &'a str,
    /// The hidden fields of the form, with the anti-CSRF token (markup).
    pub fields_html: &'a str,
    /// The word of the button and of the confirm button: "Revoke".
    pub verb: &'a str,
    /// The thing that a row is, in the singular and in the plural: "phone", "phones".
    pub one: &'a str,
    pub many: &'a str,
    pub danger: bool,
}

/// The form of the bulk action. Put it once in the page, outside the table.
pub fn bulk_form(b: &Bulk) -> String {
    format!(
        r#"<form id="{}" class="bulk" method="post" action="{}">{}</form>"#,
        html_escape(b.id),
        html_escape(b.action),
        b.fields_html
    )
}

/// The checkbox of one row. `label` names the row for a screen reader ("Select Pixel 8").
pub fn bulk_box(b: &Bulk, value: &str, label: &str) -> String {
    format!(
        r#"<input class="bulk__box" type="checkbox" form="{}" name="{}" value="{}" aria-label="{}">"#,
        html_escape(b.id),
        html_escape(b.field),
        html_escape(value),
        html_escape(label)
    )
}

/// The checkbox of the head of the column. It ticks every row of the table (script).
pub fn bulk_all_box(b: &Bulk) -> String {
    format!(
        r#"<input class="bulk__all" type="checkbox" data-bulk-all="{}" aria-label="Select all">"#,
        html_escape(b.id)
    )
}

/// The button of the bulk action. Put it in the head of the panel. With script the button is
/// absent until a row is ticked, and shows the count. Without script it is always there, with no
/// count.
pub fn bulk_button(b: &Bulk) -> String {
    format!(
        r#"<details class="ask bulk__ask{danger}" data-bulk="{id}" data-verb="{verb}" data-one="{one}" data-many="{many}"><summary class="button{btn}"><span>{verb}</span><span class="bulk__n num" data-bulk-n></span></summary><div class="ask__panel"><span class="ask__q" data-bulk-q>{verb} the ticked {many}?</span><button class="button{confirm}" type="submit" form="{id}">{verb}</button></div></details>"#,
        danger = if b.danger { " ask--danger" } else { "" },
        id = html_escape(b.id),
        verb = html_escape(b.verb),
        one = html_escape(b.one),
        many = html_escape(b.many),
        btn = if b.danger {
            " button--quiet-danger"
        } else {
            ""
        },
        confirm = if b.danger {
            " button--danger"
        } else {
            " button--primary"
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_chip_is_a_link_and_a_chip_of_zero_is_absent() {
        let html = chips(&[
            Chip {
                label_html: "All",
                name: "",
                count: 12,
                href: "/p",
                on: true,
            },
            Chip {
                label_html: &icon("ban"),
                name: "Refused",
                count: 2,
                href: "/p?r=refused&x=<",
                on: false,
            },
            Chip {
                label_html: "Never",
                name: "Never",
                count: 0,
                href: "/p?r=never",
                on: false,
            },
        ]);
        assert!(html.contains(r#"<a class="chip is-on" href="/p" aria-current="true">All<span class="num">12</span></a>"#));
        assert!(html.contains(r#"href="/p?r=refused&amp;x=&lt;""#), "{html}");
        assert!(html.contains(r#"aria-label="Refused 2" title="Refused""#));
        assert!(!html.contains("Never"), "a count of zero is absent: {html}");
        assert!(!html.contains("<button"), "no script needed: {html}");
        assert_eq!(chips(&[]), "");
    }

    #[test]
    fn an_icon_control_has_a_name_for_a_screen_reader_and_a_tooltip() {
        let link = icon_link("/x?a=1&b=2", "Open <it>", "key");
        assert!(
            link.contains(r#"aria-label="Open &lt;it&gt;" title="Open &lt;it&gt;""#),
            "{link}"
        );
        assert!(link.contains("&amp;b=2"));
        let post = icon_post(
            "/do",
            "Log in as carol",
            "user-share",
            r#"<input type="hidden" name="csrf" value="t">"#,
        );
        assert!(
            post.contains(r#"method="post" action="/do""#)
                && post.contains(r#"aria-label="Log in as carol""#)
        );
        let count = icon_count("Keys: 5 of 8 rights", "key", 5);
        assert!(
            count.contains(r#"role="img" aria-label="Keys: 5 of 8 rights""#)
                && count.contains(">5<")
        );
    }

    #[test]
    fn a_popover_is_a_details_element_that_opens_without_script() {
        let html = popover("Change the password", "key", Side::Start, false, "<p>x</p>");
        assert!(
            html.starts_with(r#"<details class="pop"><summary class="ib""#),
            "{html}"
        );
        assert!(html.contains(r#"aria-label="Change the password""#));
        assert!(html.contains(r#"<div class="pop__menu"><p>x</p></div>"#));
        let end = popover("A", "key", Side::End, true, "");
        assert!(
            end.contains("pop__menu pop__menu--end pop__menu--wide"),
            "{end}"
        );
    }

    #[test]
    fn a_confirmation_turns_the_button_into_a_cross_and_a_firm_one_asks_for_the_name() {
        let light = ask(&Ask {
            label: "Delete carol",
            icon: "trash",
            question: "Delete carol and 3 keys?",
            verb: "Delete",
            action: "/auth/users/delete",
            fields_html: r#"<input type="hidden" name="login" value="carol">"#,
            strength: Strength::Light,
            name: "carol",
            danger: true,
            side: Side::End,
        });
        assert!(
            light.starts_with(r#"<details class="ask ask--danger"><summary class="ib ib--danger""#),
            "{light}"
        );
        assert!(light.contains("ask__open") && light.contains("ask__close"));
        assert!(light.contains("Delete carol and 3 keys?"));
        assert!(light
            .contains(r#"<button class="button button--danger" type="submit">Delete</button>"#));
        assert!(
            !light.contains("pattern="),
            "a light confirmation has no field: {light}"
        );
        let firm = ask(&Ask {
            strength: Strength::Firm,
            name: "a.b-c",
            ..Ask {
                label: "Delete",
                icon: "trash",
                question: "Delete a.b-c?",
                verb: "Delete",
                action: "/x",
                fields_html: "",
                strength: Strength::Light,
                name: "",
                danger: true,
                side: Side::End,
            }
        });
        assert!(firm.contains(r#"required pattern="a\.b-c""#), "{firm}");
    }

    #[test]
    fn the_bulk_action_is_a_form_with_boxes_and_one_button_that_name_it() {
        let b = Bulk {
            id: "revoke-phones",
            action: "/auth/settings/inbox/revoke",
            field: "id",
            fields_html: r#"<input type="hidden" name="csrf" value="t">"#,
            verb: "Revoke",
            one: "phone",
            many: "phones",
            danger: true,
        };
        let form = bulk_form(&b);
        assert!(form.contains(r#"id="revoke-phones""#) && form.contains(r#"method="post""#));
        let box_ = bulk_box(&b, "42", "Select Pixel 8");
        assert!(
            box_.contains(r#"form="revoke-phones" name="id" value="42""#),
            "{box_}"
        );
        assert!(box_.contains(r#"aria-label="Select Pixel 8""#));
        let button = bulk_button(&b);
        assert!(button.contains(r#"data-bulk="revoke-phones""#));
        assert!(button.contains(r#"data-many="phones""#));
        assert!(
            button.contains("Revoke the ticked phones?"),
            "the text without script: {button}"
        );
        assert!(button.contains(r#"type="submit" form="revoke-phones""#));
        assert!(!button.contains(" disabled"), "nothing grey: {button}");
    }

    #[test]
    fn a_panel_has_a_head_and_a_list_with_its_own_scroller() {
        let html = zone(
            &Zone {
                id: "signins-title",
                title: "Sign-ins",
                count: "12",
                chips_html: "",
                tools_html: "",
                class: "",
            },
            "<table></table>",
        );
        assert!(html.contains(r#"aria-labelledby="signins-title""#));
        assert!(html.contains(r#"<h2 class="title" id="signins-title">Sign-ins</h2>"#));
        assert!(html.contains(r#"<div class="scroll"><table></table></div>"#));
        let two = split("users", "<section></section>", "<section></section>");
        assert!(two.contains("split split--users") && two.contains("stackcol"));
    }
}
