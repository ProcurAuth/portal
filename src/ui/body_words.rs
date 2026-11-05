//! Test helper: the words that a page shows in its body, and the check that the product name is not one of them.
//!
//! The name of the product is for the title of the tab and for the file names of the repository. No page body
//! shows it. The words of a body are its text and the attributes that a reader sees: `aria-label`, `title`, `alt`,
//! `placeholder` and the `value` of a visible field. The JSON data of the editor of grants counts too, because the
//! script draws it in the body.
//!
//! A technical identifier is data and not prose: it is part of the contract of the product. The list [`ALLOWED`]
//! names each one, so each exception is visible. Every other hit makes the test fail.

/// The technical identifiers that a body can hold. The check removes each one before it looks for the product name.
pub(crate) const ALLOWED: &[(&str, &str)] = &[
    (
        "service:portal",
        "the id of the internal service in the text of a grant",
    ),
    (
        "service portal",
        "the id of the internal service in an error of a key form",
    ),
    (
        r#""portal""#,
        "the id of the internal service, a quoted string in the data of the editor of grants",
    ),
];

/// The attributes that a reader sees or hears.
const SEEN_ATTRIBUTES: [&str; 5] = [
    "aria-label",
    "aria-description",
    "title",
    "alt",
    "placeholder",
];

/// The words of a body: its text, then the seen attributes. A line is one text or one value.
pub(crate) fn words(html: &str) -> Vec<String> {
    let body = html.find("<body").map_or(html, |at| &html[at..]);
    let mut out = Vec::new();
    let mut rest = body;
    let mut skip_until: Option<&str> = None;
    while let Some(open) = rest.find('<') {
        let (text, tail) = rest.split_at(open);
        if skip_until.is_none() && !text.trim().is_empty() {
            out.push(text.trim().to_string());
        }
        let Some(close) = tail.find('>') else { break };
        let tag = &tail[1..close];
        rest = &tail[close + 1..];
        if let Some(end) = skip_until {
            if tag == end {
                skip_until = None;
            }
            continue;
        }
        let name = tag
            .split(|c: char| c.is_whitespace() || c == '/')
            .next()
            .unwrap_or("");
        match name {
            "style" => skip_until = Some("/style"),
            "script" if !tag.contains("application/json") => skip_until = Some("/script"),
            _ => {}
        }
        let hidden_field = name == "input" && tag.contains(r#"type="hidden""#);
        for attribute in SEEN_ATTRIBUTES {
            if let Some(value) = attribute_of(tag, attribute) {
                out.push(value);
            }
        }
        if !hidden_field && matches!(name, "input" | "textarea" | "option" | "button") {
            if let Some(value) = attribute_of(tag, "value") {
                out.push(value);
            }
        }
    }
    out
}

/// The value of `name="..."` in the text of a tag.
fn attribute_of(tag: &str, name: &str) -> Option<String> {
    let needle = format!(" {name}=\"");
    let start = tag.find(&needle)? + needle.len();
    Some(tag[start..start + tag[start..].find('"')?].to_string())
}

/// The words that name the product and that [`ALLOWED`] does not cover.
pub(crate) fn product_words(html: &str) -> Vec<String> {
    words(html)
        .into_iter()
        .filter(|word| {
            let mut left = word.clone();
            for (allowed, _) in ALLOWED {
                left = left.replace(allowed, "");
            }
            left.to_ascii_lowercase().contains("portal")
        })
        .collect()
}

/// The pieces of the logo that no body holds: the symbols of the sprite, the class of the dot and the classes of
/// the old header.
pub(crate) const LOGO_PARTS: [&str; 4] = ["i-logo", "logoDot", "appMark", "appSep"];

/// Fails when the body of `html` names the product outside [`ALLOWED`], or holds a piece of the logo.
pub(crate) fn assert_clean(page: &str, html: &str) {
    let hits = product_words(html);
    assert!(
        hits.is_empty(),
        "{page}: the body names the product: {hits:#?}"
    );
    for part in LOGO_PARTS {
        // The symbol of the sign-out icon is `i-logout`: it is not the logo.
        assert!(
            !html.replace("i-logout", "").contains(part),
            "{page}: the body holds {part}"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_words_are_the_text_and_the_seen_attributes_of_the_body_only() {
        let html = r##"<html><head><title>Portal</title><style>.portal{}</style></head><body class="x"><script src="/portal.js"></script><script>var portal;</script><a href="/portal" aria-label="Go to Portal" class="portal">Home</a><input type="hidden" name="portal" value="portal"><input value="a portal"><script type="application/json">{"id":"portal"}</script><p>One <b>two</b></p></body></html>"##;
        let found = words(html);
        assert_eq!(
            found,
            [
                "Go to Portal",
                "Home",
                "a portal",
                r#"{"id":"portal"}"#,
                "One",
                "two"
            ]
        );
    }

    #[test]
    fn a_hit_that_the_list_does_not_cover_is_found_in_any_case() {
        let html = r#"<body><p>The Portal shows it. Add <code>portal.name</code> and a grant on service:portal.</p><p title="PORTAL">x</p></body>"#;
        let hits = product_words(html);
        assert_eq!(hits.len(), 3, "{hits:?}");
        assert!(hits[0].contains("The Portal shows it"));
        // A label key is a hit too: no hint of a page names it.
        assert_eq!(hits[1], "portal.name");
        assert_eq!(hits[2], "PORTAL");
        assert!(product_words(r#"<body><p>On service:portal</p></body>"#).is_empty());
    }

    #[test]
    fn every_allowed_identifier_is_one_that_the_product_uses_as_data() {
        for (text, why) in ALLOWED {
            assert!(text.to_ascii_lowercase().contains("portal"), "{text}");
            assert!(!why.is_empty(), "{text}");
        }
    }
}
