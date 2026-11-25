//! The page "Links" of the settings: the personal links of the signed-in account.
//!
//! A personal link is a tile that an account adds to its own launcher. Only the owner sees it. The page lists the
//! links, and an account that holds the action `links.own.edit` adds, changes and deletes them. An account that lost
//! the action keeps its links and can still delete them. Every form works without script. With script, a file input
//! fills the field of the icon (the script of the settings pages, `data-icon-into`).

use std::fmt::Write as _;

use super::settings::{fill_page, notice_html, Frame, Notice, Tab};
use crate::catalog::external::{
    personal_icon_path, ExternalEntry, MAX_DESCRIPTION_CHARS, MAX_HREF_BYTES, MAX_NAME_CHARS,
};
use crate::util::html_escape;

/// The page "Links" of the settings.
pub const PATH: &str = "/auth/settings/links";
/// The form that adds a personal link.
pub const ADD_PATH: &str = "/auth/links/add";
/// The form that changes a personal link.
pub const EDIT_PATH: &str = "/auth/links/edit";
/// The form that deletes a personal link.
pub const DELETE_PATH: &str = "/auth/links/delete";

/// The largest data URI of an icon, in bytes of the file: 32 KiB.
const ICON_MAX_BYTES: usize = 32 * 1024;

/// The words of the outcome of an action. The URL carries a code from this list, never text.
pub fn notice_for(code: &str) -> Option<Notice> {
    let (text, ok) = match code {
        "link-added" => ("Link added.", true),
        "link-updated" => ("Link saved.", true),
        "link-deleted" => ("Link deleted.", true),
        "link-bad-name" => (
            "Invalid name: 1 to 32 characters, with no control character and at least one letter or digit.",
            false,
        ),
        "link-bad-link" => (
            "Invalid link: an absolute http or https address with a valid host, 2048 bytes at most.",
            false,
        ),
        "link-bad-description" => (
            "Invalid description: 200 characters at most, with no control character.",
            false,
        ),
        "link-bad-icon" => (
            "Invalid icon: a data URI of a PNG, JPEG, WebP, GIF or SVG image, 32 KiB at most.",
            false,
        ),
        "link-exists" => ("A link of yours already has that name.", false),
        "link-limit" => (
            "You reached the largest number of links (ui.max_personal_links).",
            false,
        ),
        "link-gone" => ("That link does not exist.", false),
        "link-error" => ("The link could not be saved. Try again.", false),
        _ => return None,
    };
    Some(Notice { text, ok })
}

/// What the page shows.
pub struct LinksView<'a> {
    pub notice: Option<Notice>,
    /// The anti-forgery token of the forms.
    pub token: &'a str,
    /// The links of the account, sorted by name.
    pub links: &'a [ExternalEntry],
    /// The account holds the action `links.own.edit` now.
    pub can_edit: bool,
    /// The largest number of links of one account (`ui.max_personal_links`).
    pub max: usize,
}

fn csrf(token: &str) -> String {
    format!(
        r#"<input type="hidden" name="csrf" value="{}">"#,
        html_escape(token)
    )
}

/// The fields of the form of a link. `prefix` makes the ids unique on the page. An empty entry gives an empty form.
fn fields(prefix: &str, entry: Option<&ExternalEntry>, keep_icon: bool) -> String {
    let name = entry.map(|e| e.name.as_str()).unwrap_or("");
    let href = entry.map(|e| e.href.as_str()).unwrap_or("");
    let description = entry.map(|e| e.description.as_str()).unwrap_or("");
    let icon_hint = if keep_icon {
        "Leave the field empty to keep the icon."
    } else {
        "A data URI of a PNG, JPEG, WebP, GIF or SVG image, 32 KiB at most."
    };
    let remove = if keep_icon {
        r#"<label class="myLinks__check"><input type="checkbox" name="remove_icon" value="1"> Remove the icon</label>"#
            .to_string()
    } else {
        String::new()
    };
    format!(
        r#"<div class="field"><label class="field__label" for="{prefix}-name">Name</label><input class="uiInput" id="{prefix}-name" name="name" type="text" required maxlength="{MAX_NAME_CHARS}" autocomplete="off" spellcheck="false" value="{}" placeholder="Team wiki"></div><div class="field"><label class="field__label" for="{prefix}-href">Link</label><input class="uiInput" id="{prefix}-href" name="href" type="url" required maxlength="{MAX_HREF_BYTES}" autocomplete="off" autocapitalize="off" spellcheck="false" value="{}" placeholder="https://wiki.example.org/"></div><div class="field"><label class="field__label" for="{prefix}-desc">Description (optional)</label><input class="uiInput" id="{prefix}-desc" name="description" type="text" maxlength="{MAX_DESCRIPTION_CHARS}" autocomplete="off" value="{}" placeholder="The wiki of the team"></div><div class="field"><label class="field__label" for="{prefix}-icon">Icon (optional)</label><input class="uiInput" id="{prefix}-icon" name="icon" type="text" maxlength="{}" autocomplete="off" autocapitalize="off" spellcheck="false" placeholder="data:image/png;base64,..."><div class="iconPick"><input type="file" id="{prefix}-icon-file" accept="image/png,image/jpeg,image/webp,image/gif,image/svg+xml" data-icon-into="{prefix}-icon" data-icon-max="{ICON_MAX_BYTES}" aria-label="Choose an icon file"></div><p class="hint" data-icon-msg>{icon_hint}</p>{remove}</div>"#,
        html_escape(name),
        html_escape(href),
        html_escape(description),
        // A data URI of the largest icon: the base64 text of 32 KiB, with its start.
        ICON_MAX_BYTES * 4 / 3 + 64,
    )
}

/// The icon of a row: the image, or the first letter of the name.
fn badge(entry: &ExternalEntry) -> String {
    if entry.icon.is_some() {
        format!(
            r#"<img class="myLinks__icon" src="{}" alt="" width="28" height="28" loading="lazy">"#,
            html_escape(&personal_icon_path(&entry.id))
        )
    } else {
        let initial: String = entry
            .name
            .chars()
            .find(|c| c.is_alphanumeric())
            .map(|c| c.to_uppercase().collect())
            .unwrap_or_else(|| "?".to_string());
        format!(
            r#"<span class="myLinks__icon myLinks__icon--letter" aria-hidden="true">{}</span>"#,
            html_escape(&initial)
        )
    }
}

fn row(entry: &ExternalEntry, view: &LinksView, index: usize) -> String {
    let token = csrf(view.token);
    let id = html_escape(&entry.id);
    let mut out = format!(
        r#"<li class="myLinks__row"><div class="myLinks__head">{}<div class="myLinks__text"><span class="myLinks__name">{}</span><span class="myLinks__href">{}</span>"#,
        badge(entry),
        html_escape(&entry.name),
        html_escape(&entry.href),
    );
    if !entry.description.is_empty() {
        let _ = write!(
            out,
            r#"<span class="myLinks__desc">{}</span>"#,
            html_escape(&entry.description)
        );
    }
    out.push_str("</div>");
    out.push_str(r#"<div class="myLinks__acts">"#);
    let _ = write!(
        out,
        r#"<form method="post" action="{DELETE_PATH}">{token}<input type="hidden" name="id" value="{id}"><button class="button button--danger" type="submit">Delete</button></form>"#
    );
    out.push_str("</div></div>");
    if view.can_edit {
        let _ = write!(
            out,
            r#"<details class="myLinks__edit"><summary>Change</summary><form class="extForm" method="post" action="{EDIT_PATH}">{token}<input type="hidden" name="id" value="{id}">{}<button class="button button--primary" type="submit">Save</button></form></details>"#,
            fields(&format!("lk{index}"), Some(entry), entry.icon.is_some()),
        );
    }
    out.push_str("</li>");
    out
}

/// The page "Links".
pub fn links_page(frame: &Frame, view: &LinksView) -> String {
    let mut out = String::new();
    notice_html(&mut out, view.notice);
    out.push_str(
        r#"<section class="myLinks" aria-labelledby="my-links-title"><h2 class="myLinks__title" id="my-links-title">My links</h2>"#,
    );
    out.push_str(
        r#"<p class="hint">These links are tiles on your launcher. Only you see them. This server does not protect a link: anyone who knows the address can open it.</p>"#,
    );
    if view.links.is_empty() {
        out.push_str(r#"<p class="myLinks__empty">You have no link yet.</p>"#);
    } else {
        out.push_str(r#"<ul class="myLinks__list">"#);
        for (index, entry) in view.links.iter().enumerate() {
            out.push_str(&row(entry, view, index));
        }
        out.push_str("</ul>");
    }
    if !view.can_edit {
        out.push_str(
            r#"<p class="hint">Your account cannot add or change links. You can still delete them.</p>"#,
        );
    } else if view.links.len() >= view.max {
        let _ = write!(
            out,
            r#"<p class="hint">You have {} links, the largest number. Delete one to add another.</p>"#,
            view.links.len()
        );
    } else {
        let _ = write!(
            out,
            r#"<details class="myLinks__add"><summary class="button button--primary">Add a link</summary><form class="extForm" method="post" action="{ADD_PATH}">{}{}<button class="button button--primary" type="submit">Add</button></form></details>"#,
            csrf(view.token),
            fields("lk-new", None, false),
        );
    }
    out.push_str("</section>");
    fill_page(Tab::Links, frame, "Links", &out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::fixtures::frame;
    use std::sync::Arc;

    fn link(name: &str, href: &str) -> ExternalEntry {
        let (name, id) = crate::catalog::external::check_name(name).unwrap();
        ExternalEntry {
            id,
            name,
            href: href.into(),
            ..Default::default()
        }
    }

    fn view<'a>(links: &'a [ExternalEntry], can_edit: bool, max: usize) -> LinksView<'a> {
        LinksView {
            notice: None,
            token: "tok<en",
            links,
            can_edit,
            max,
        }
    }

    fn page(links: &[ExternalEntry], can_edit: bool, max: usize) -> String {
        let frame = Frame {
            links: true,
            superadmin: false,
            ..frame()
        };
        links_page(&frame, &view(links, can_edit, max))
    }

    #[test]
    fn each_code_of_the_outcome_has_words_and_an_unknown_code_has_none() {
        for code in [
            "link-added",
            "link-updated",
            "link-deleted",
            "link-bad-name",
            "link-bad-link",
            "link-bad-description",
            "link-bad-icon",
            "link-exists",
            "link-limit",
            "link-gone",
            "link-error",
        ] {
            let notice = notice_for(code).unwrap_or_else(|| panic!("{code}"));
            assert!(!notice.text.is_empty());
            assert_eq!(
                notice.ok,
                code.ends_with("added") || code.ends_with("updated") || code.ends_with("deleted")
            );
        }
        assert!(notice_for("<script>").is_none() && notice_for("user-added").is_none());
    }

    #[test]
    fn an_account_with_the_action_has_the_forms_that_add_and_change() {
        let links = [link("Team wiki", "https://wiki.example.org/")];
        let html = page(&links, true, 32);
        assert!(html.contains(&format!(r#"action="{ADD_PATH}""#)));
        assert!(html.contains(&format!(r#"action="{EDIT_PATH}""#)));
        assert!(html.contains(&format!(r#"action="{DELETE_PATH}""#)));
        assert!(
            html.contains(r#"name="csrf" value="tok&lt;en""#),
            "the token is escaped"
        );
        assert!(html.contains(r#"name="id" value="ext-team-wiki""#));
        // The form of a change has the box that removes the icon only when there is an icon to keep.
        assert!(html.contains(r#"name="icon""#));
        assert!(!html.contains("remove_icon"), "no icon, nothing to remove");
        // Each field of the page has a label, and the ids are unique.
        assert!(html.contains(r#"for="lk-new-name""#) && html.contains(r#"for="lk0-name""#));
        // The page sits in the tab Links of the settings.
        assert!(
            html.contains("My links") && html.contains(r#"aria-current="page""#)
                || !html.contains("contentTabs")
        );
    }

    #[test]
    fn an_account_without_the_action_can_only_delete() {
        let links = [link("Team wiki", "https://wiki.example.org/")];
        let html = page(&links, false, 32);
        assert!(html.contains(&format!(r#"action="{DELETE_PATH}""#)));
        assert!(!html.contains(ADD_PATH) && !html.contains(EDIT_PATH));
        assert!(html.contains("cannot add or change links"));
    }

    #[test]
    fn the_name_the_link_and_the_description_are_escaped() {
        let mut bad = link("Wiki", "https://wiki.example.org/?a=1&b=<2>");
        bad.description = "say \"hi\" <b>".into();
        let html = page(&[bad], true, 32);
        assert!(html.contains("https://wiki.example.org/?a=1&amp;b=&lt;2&gt;"));
        assert!(html.contains("say &quot;hi&quot; &lt;b&gt;"));
        assert!(!html.contains("<b>"));
    }

    #[test]
    fn at_the_limit_the_page_has_no_form_that_adds_and_says_why() {
        let links = [
            link("One", "https://1.example.org/"),
            link("Two", "https://2.example.org/"),
        ];
        let html = page(&links, true, 2);
        assert!(!html.contains(ADD_PATH));
        assert!(html.contains("the largest number"));
        // Below the limit the form is there, and an empty list says so.
        assert!(page(&links, true, 3).contains(ADD_PATH));
        assert!(page(&[], true, 3).contains("You have no link yet."));
    }

    #[test]
    fn a_link_with_an_icon_shows_the_route_of_the_owner_and_one_without_shows_its_letter() {
        let mut with = link("Café", "https://c.example.org/");
        with.icon = Some(Arc::new(
            crate::catalog::image::Image::from_bytes(
                b"<svg xmlns=\"http://www.w3.org/2000/svg\"/>".to_vec(),
                None,
            )
            .unwrap(),
        ));
        let without = link("Docs", "https://d.example.org/");
        let html = page(&[with, without], true, 32);
        assert!(
            html.contains(r#"src="/auth/my-icon/ext-caf%C3%A9""#),
            "{html}"
        );
        assert!(html.contains(r#"myLinks__icon--letter" aria-hidden="true">D<"#));
        assert!(
            html.contains("remove_icon"),
            "the link with an icon can lose it"
        );
    }
}
