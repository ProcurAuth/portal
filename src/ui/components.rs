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
//! | Small chips with an overflow | `tags` | Links and a `details` element. |
//! | Popover | `popover` | A `details` element. |
//! | Confirm in place | `ask` | A `details` element that holds a form. |
//! | Bulk action | `bulk_*` | A form. The count needs script. |
//! | Two columns | `split` | A grid that stacks under 1000 px. |
//!
//! Everything that comes from outside goes through `html_escape` here. The caller gives text, and
//! the function escapes it. The parameters that end with `_html` are the exception: they hold
//! markup that the caller built.
//!
//! The tests of the parts are in this file.

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
    chip_group(&shown)
}

/// A group of chips that keeps its place when a filter changes. The caller picks the chips from
/// the totals of the list. A chip with a count of zero stays, dimmed, and is not a link.
pub fn chips_stable(items: &[Chip]) -> String {
    let shown: Vec<&Chip> = items.iter().collect();
    chip_group(&shown)
}

fn chip_group(shown: &[&Chip]) -> String {
    if shown.is_empty() {
        return String::new();
    }
    let mut out = String::from(r#"<span class="chips" role="group" aria-label="Filter">"#);
    for chip in shown {
        let name = if chip.name.is_empty() {
            String::new()
        } else {
            format!(
                r#" aria-label="{0} {1}" title="{0}""#,
                html_escape(chip.name),
                chip.count
            )
        };
        if chip.href.is_empty() || (chip.count == 0 && !chip.on) {
            // A fact, not a control: an empty `href`, or no row to filter.
            let _ = write!(
                out,
                r#"<span class="chip chip--static{}{}"{}{}>{}<span class="num">{}</span></span>"#,
                if chip.on { " is-on" } else { "" },
                if chip.count == 0 { " is-zero" } else { "" },
                if chip.count == 0 && !chip.on {
                    r#" aria-disabled="true""#
                } else {
                    ""
                },
                name,
                chip.label_html,
                chip.count
            );
            continue;
        }
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
            name,
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
    /// The tooltip of the count. Empty: none.
    pub count_title: &'a str,
    pub chips_html: &'a str,
    pub tools_html: &'a str,
    /// More classes of the panel, with a space before the first: " zone--sent". Empty: none.
    pub class: &'a str,
    /// Markup between the head and the list that does not scroll: a small graph. Empty: none.
    pub above_html: &'a str,
}

/// A panel: its head, then `body_html` in a list with its own scroller. The page does not scroll
/// for a long list: the list does. `body_html` is a table (`grid`) or any markup.
pub fn zone(z: &Zone, body_html: &str) -> String {
    let above = if z.above_html.is_empty() {
        ""
    } else {
        " zone--above"
    };
    // A panel with no title is named by its id for a screen reader: the toolbar above it shows the
    // title.
    let mut out = if z.title.is_empty() {
        format!(
            r#"<section class="zone{above}{}" aria-label="{}"><div class="zone__head">"#,
            html_escape(z.class),
            html_escape(z.id),
        )
    } else {
        format!(
            r#"<section class="zone{above}{}" aria-labelledby="{}"><div class="zone__head"><h2 class="title" id="{}">{}</h2>"#,
            html_escape(z.class),
            html_escape(z.id),
            html_escape(z.id),
            html_escape(z.title)
        )
    };
    if !z.count.is_empty() {
        let title = if z.count_title.is_empty() {
            String::new()
        } else {
            format!(r#" title="{}""#, html_escape(z.count_title))
        };
        let _ = write!(
            out,
            r#"<span class="count"{title}>{}</span>"#,
            html_escape(z.count)
        );
    }
    out.push_str(z.chips_html);
    out.push_str(r#"<span class="grow"></span>"#);
    out.push_str(z.tools_html);
    let _ = write!(
        out,
        r#"</div>{}<div class="scroll">{body_html}</div></section>"#,
        z.above_html
    );
    out
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

/// Two panels side by side. They stack under 1000 px. `class` names the proportion: `services`.
pub fn split(class: &str, left_html: &str, right_html: &str) -> String {
    format!(
        r#"<div class="split split--{}">{left_html}<div class="stackcol">{right_html}</div></div>"#,
        html_escape(class)
    )
}

/// The zones of a page that has a main zone on the left and a stack on the right. `left` is
/// `None` when the account has no main zone: the stack then has the full width. `class` names the
/// proportion (`inbox`) or the narrow page of one zone (`solo`).
pub fn split_zones(class: &str, left: Option<&str>, right: &str) -> String {
    match (left, right.is_empty()) {
        (Some(left), false) => split(class, left, right),
        (Some(left), true) => {
            format!(r#"<div class="split split--one">{left}</div>"#)
        }
        (None, _) => format!(
            r#"<div class="split split--one split--{}"><div class="stackcol">{right}</div></div>"#,
            html_escape(class)
        ),
    }
}

/// The head of a page that fills the window and belongs to the tab Access: the account, "New user",
/// "New key" and "Edit key". The crumb goes back to the tab, the title follows, then the tools of
/// the page. The actions stand at the right: Cancel and the primary button.
pub struct PageHead<'a> {
    pub title: &'a str,
    /// The tools after the title (markup). Empty: none.
    pub tools_html: &'a str,
    /// The actions at the right (markup). Empty: none.
    pub actions_html: &'a str,
}

pub fn page_head(h: &PageHead) -> String {
    format!(
        r##"<div class="accountBar"><nav class="crumb" aria-label="Breadcrumb"><a href="{}">Access</a>{}</nav><h2 class="name" aria-current="page">{}</h2>{}<span class="grow"></span>{}</div>"##,
        super::settings::ACCESS_PATH,
        icon("chevron-right"),
        html_escape(h.title),
        h.tools_html,
        h.actions_html,
    )
}

/// A link that looks like a button: "Cancel" in the head of a full page.
pub fn cancel_link() -> String {
    format!(
        r#"<a class="button" href="{}">Cancel</a>"#,
        super::settings::ACCESS_PATH
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
    popover_with(
        label,
        "ib",
        &icon(icon_name),
        side,
        if wide { " pop__menu--wide" } else { "" },
        body_html,
    )
}

/// A popover whose opener has its own look: `summary_class` is the class of the opener (`ib`,
/// `pick`, `button button--primary`) and `summary_html` its content. `menu_class` adds classes to
/// the panel, with a space before the first. `label` names the opener for a screen reader and for
/// the tooltip. Pass an empty `label` when the content of the opener is a visible word.
pub fn popover_with(
    label: &str,
    summary_class: &str,
    summary_html: &str,
    side: Side,
    menu_class: &str,
    body_html: &str,
) -> String {
    let name = if label.is_empty() {
        String::new()
    } else {
        format!(r#" aria-label="{0}" title="{0}""#, html_escape(label))
    };
    format!(
        r#"<details class="pop"><summary class="{summary_class}"{name}>{summary_html}</summary><div class="pop__menu{}{menu_class}">{body_html}</div></details>"#,
        if side == Side::End {
            " pop__menu--end"
        } else {
            ""
        },
    )
}

/// One small chip of a list of tags: a name that links to a filtered page.
pub struct Tag<'a> {
    /// Markup before the text: the icon of a kind. Empty: none.
    pub lead_html: &'a str,
    pub text: &'a str,
    /// Markup after the text: the mark of a critical group. Empty: none.
    pub mark_html: &'a str,
    pub href: &'a str,
    pub title: &'a str,
    /// Muted text in the popover, after the chip: "11 accounts". Empty: none.
    pub note: &'a str,
}

fn tag_link(tag: &Tag) -> String {
    format!(
        r#"<a class="tag" href="{}" title="{}">{}{}{}</a>"#,
        html_escape(tag.href),
        html_escape(tag.title),
        tag.lead_html,
        html_escape(tag.text),
        tag.mark_html
    )
}

/// Small chips in one line. After `limit` chips, a chip "+N" opens a popover with all of them and
/// their notes. `all_label` names the popover opener for a screen reader. No tag gives a dash.
pub fn tags(items: &[Tag], limit: usize, all_label: &str) -> String {
    if items.is_empty() {
        return r#"<span class="muted">-</span>"#.to_string();
    }
    let mut out = String::from(r#"<span class="tags">"#);
    for tag in items.iter().take(limit) {
        out.push_str(&tag_link(tag));
    }
    if items.len() > limit {
        let mut menu = format!(
            r#"<div class="sectionLabel">{}</div>"#,
            html_escape(all_label)
        );
        for tag in items {
            let note = if tag.note.is_empty() {
                String::new()
            } else {
                format!(r#"<span class="muted">{}</span>"#, html_escape(tag.note))
            };
            let _ = write!(menu, r#"<div class="mrow">{}{note}</div>"#, tag_link(tag));
        }
        out.push_str(&popover_with(
            all_label,
            "tag tag--more",
            &format!("+{}", items.len() - limit),
            Side::Start,
            " pop__menu--list",
            &menu,
        ));
    }
    out.push_str("</span>");
    out
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
    /// The prefix of the name of each ticked row in the form. The box of the row `42` is named
    /// `<field>.42` and has the value `1`: a form never repeats a name.
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
        r#"<input class="bulk__box" type="checkbox" form="{}" name="{}.{}" value="1" aria-label="{}">"#,
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

/// The button of the bulk action. Put it in the head of the panel. With script the button shows
/// the count of the ticked rows, "(0)" and dimmed while none is ticked. Without script it is
/// always there, with no count.
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
        let count = icon_count("Keys: 5 of 8 permissions", "key", 5);
        assert!(
            count.contains(r#"role="img" aria-label="Keys: 5 of 8 permissions""#)
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
            box_.contains(r#"form="revoke-phones" name="id.42" value="1""#),
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
    fn tags_are_links_and_the_overflow_is_a_popover_with_all_of_them() {
        let items: Vec<Tag> = ["a", "b", "c <d>"]
            .iter()
            .map(|name| Tag {
                lead_html: "",
                text: name,
                mark_html: "",
                href: "/p?group=x&y=1",
                title: "Group",
                note: "3 accounts",
            })
            .collect();
        let html = tags(&items, 2, "All 3 groups of carol");
        assert_eq!(
            html.matches(r#"<a class="tag" href="/p?group=x&amp;y=1""#)
                .count(),
            2 + 3
        );
        assert!(html.contains(r#"<summary class="tag tag--more" aria-label="All 3 groups of carol" title="All 3 groups of carol">+1</summary>"#), "{html}");
        assert!(
            html.contains("c &lt;d&gt;")
                && html.contains(r#"<span class="muted">3 accounts</span>"#)
        );
        let two = tags(&items[..2], 2, "x");
        assert!(!two.contains("<details"), "no overflow, no popover: {two}");
        assert_eq!(tags(&[], 2, "x"), r#"<span class="muted">-</span>"#);
    }

    #[test]
    fn a_panel_has_a_head_and_a_list_with_its_own_scroller() {
        let html = zone(
            &Zone {
                id: "signins-title",
                title: "Sign-ins",
                count: "12",
                count_title: "",
                chips_html: "",
                tools_html: "",
                class: "",
                above_html: "",
            },
            "<table></table>",
        );
        assert!(html.contains(r#"aria-labelledby="signins-title""#));
        assert!(html.contains(r#"<h2 class="title" id="signins-title">Sign-ins</h2>"#));
        assert!(html.contains(r#"<div class="scroll"><table></table></div>"#));
        let two = split("services", "<section></section>", "<section></section>");
        assert!(two.contains("split split--services") && two.contains("stackcol"));
    }
}
