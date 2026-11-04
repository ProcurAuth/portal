//! The sign-in page and the card of the second step. They share one look: a flat card, a band with the
//! destination, and fields with a floating label.
//!
//! The band shows the host and the path that the visitor asked for. It exists only when the sign-in has a
//! destination: the signed `rd` of the page is not the default `/`. Without a destination, the card has no
//! band and starts with the fields. The band depends on `rd` and on the host of the request only. It never
//! depends on what a visitor typed, so a refused sign-in keeps the same answer for each cause.
//!
//! A floating label is a real `<label for>` and the field has the placeholder `" "`. The stylesheet reads
//! `:placeholder-shown`, `:focus` and the autofill states. No script is needed. The button that shows the
//! password is in the page with the attribute `hidden`: the shared script shows it.
//!
//! The pages go through the pass of [`super::mask`]. The classes below are in the shared stylesheet.

use super::sign_in_document;
use crate::util::html_escape;

/// The sentence of a refused sign-in. Each cause of a refusal gives this sentence.
pub const INVALID_CREDENTIALS: &str = "Invalid login or password";

/// The id of the hidden heading. The form takes its name from it.
const TITLE_ID: &str = "signin-title";
/// The id of the line of the message. A field that the message concerns points to it.
const MESSAGE_ID: &str = "signin-msg";

/// The longest path (in characters) that the band shows. The stylesheet cuts what does not fit.
const MAX_PATH_CHARS: usize = 96;

/// The sprite symbols that the sign-in cards draw.
pub const ICONS: [&str; 4] = ["i-lock", "i-alert-triangle", "i-eye", "i-eye-off"];

/// Tells whether a character can change the order or the look of the text that is around it. The band does
/// not show such a character: an address cannot reorder or hide the host that stands before it.
fn is_unsafe_char(c: char) -> bool {
    c.is_control()
        || matches!(
            c,
            '\u{00AD}'
                | '\u{034F}'
                | '\u{061C}'
                | '\u{115F}'..='\u{1160}'
                | '\u{17B4}'..='\u{17B5}'
                | '\u{180B}'..='\u{180F}'
                | '\u{200B}'..='\u{200F}'
                | '\u{2028}'..='\u{202E}'
                | '\u{2060}'..='\u{206F}'
                | '\u{3164}'
                | '\u{FE00}'..='\u{FE0F}'
                | '\u{FEFF}'
                | '\u{FFA0}'
                | '\u{FFF0}'..='\u{FFFF}'
        )
}

/// The path of the destination, as text: the part of `rd` before the query, without the characters that
/// [`is_unsafe_char`] names, and cut at [`MAX_PATH_CHARS`] with an ellipsis. `None` when `rd` is the default.
fn destination_path(rd: &str) -> Option<String> {
    if rd == "/" {
        return None;
    }
    let path = rd.split('?').next().unwrap_or_default();
    let mut shown: String = path.chars().filter(|c| !is_unsafe_char(*c)).collect();
    if shown.chars().count() > MAX_PATH_CHARS {
        shown = shown.chars().take(MAX_PATH_CHARS).collect();
        shown.push('\u{2026}');
    }
    Some(shown)
}

/// The band at the top of the card. It is empty when `rd` is the default: the card then has no band.
/// The text is plain text: the band has no link, whatever `rd` holds.
fn band(host: Option<&str>, rd: &str) -> String {
    let Some(path) = destination_path(rd) else {
        return String::new();
    };
    let host = host.unwrap_or_default();
    let host_html = if host.is_empty() {
        String::new()
    } else {
        format!(r#"<span class="signin__host">{}</span>"#, html_escape(host))
    };
    format!(
        r##"<div class="signin__band"><svg class="signin__lock" aria-hidden="true" focusable="false"><use href="#i-lock"/></svg><span class="signin__to" title="{}"><span class="visually-hidden">To </span>{host_html}<b class="signin__path">{}</b></span></div>"##,
        html_escape(&format!("{host}{path}")),
        html_escape(&path),
    )
}

/// A field with a floating label. `attributes` holds the attributes of the input after `id`. `after` is the
/// markup that follows the input in the same box (the button that shows the password).
fn field(id: &str, label: &str, attributes: &str, extra_class: &str, after: &str) -> String {
    format!(
        r#"<div class="signin__field{extra_class}"><label class="signin__label" for="{id}">{label}</label><input class="signin__in" id="{id}" {attributes} placeholder=" ">{after}</div>"#
    )
}

/// The attributes of a field that a message concerns.
fn invalid_attributes(invalid: bool) -> &'static str {
    if invalid {
        r#" aria-invalid="true" aria-describedby="signin-msg""#
    } else {
        ""
    }
}

/// The line of the message. It always takes its place in the card, so the card does not move when a message
/// appears.
fn message(text: Option<&str>) -> String {
    match text {
        Some(text) => format!(
            r##"<p class="signin__msg" id="{MESSAGE_ID}" role="alert"><svg class="signin__mi" aria-hidden="true" focusable="false"><use href="#i-alert-triangle"/></svg><span>{}</span></p>"##,
            html_escape(text),
        ),
        None => format!(r#"<p class="signin__msg" id="{MESSAGE_ID}" aria-live="polite"></p>"#),
    }
}

/// The button that shows and hides the password. The page has it hidden: the script shows it.
fn reveal(target: &str) -> String {
    format!(
        r##"<button class="signin__reveal" type="button" data-reveal hidden aria-controls="{target}" aria-pressed="false" aria-label="Show password"><svg class="signin__show" aria-hidden="true" focusable="false"><use href="#i-eye"/></svg><svg class="signin__hide" aria-hidden="true" focusable="false"><use href="#i-eye-off"/></svg></button>"##
    )
}

/// The card, with its hidden heading and its band. `form` is the markup that follows.
fn card(title: &str, host: Option<&str>, rd: &str, form: &str) -> String {
    format!(
        r#"<h1 class="visually-hidden" id="{TITLE_ID}">{title}</h1>{}{form}"#,
        band(host, rd)
    )
}

/// The class of a form. A card with no band has more space above its first field.
fn form_class(rd: &str) -> &'static str {
    if rd == "/" {
        "signin__form signin__form--bare"
    } else {
        "signin__form"
    }
}

/// What the sign-in page needs.
pub struct Login<'a> {
    pub csrf: &'a str,
    /// The return target. It was checked and signed. `/` is the default.
    pub rd: &'a str,
    pub sig: &'a str,
    pub form_token: &'a str,
    /// The host that the visitor asked for. `None` when the request gave no usable host.
    pub host: Option<&'a str>,
    /// A sentence for the message line, for example [`INVALID_CREDENTIALS`].
    pub error: Option<&'a str>,
    pub max_password_bytes: usize,
    /// The words of the life of a remembered session. `None`: the form has no box.
    pub keep_for: Option<&'a str>,
    /// The names of the fields that the server reads.
    pub names: LoginNames,
}

/// The names of the fields of the sign-in form that are not `login` and `password`.
pub struct LoginNames {
    pub form_token: &'static str,
    pub trap: &'static str,
    pub keep: &'static str,
    pub keep_value: &'static str,
}

/// The sign-in page.
pub fn login_page(l: &Login) -> String {
    let invalid = l.error == Some(INVALID_CREDENTIALS);
    let marks = invalid_attributes(invalid);
    let keep = l.keep_for.map_or_else(String::new, |words| {
        format!(
            r#"<label class="signin__check"><input type="checkbox" name="{}" value="{}">Keep me signed in for {}</label>"#,
            l.names.keep,
            l.names.keep_value,
            html_escape(words)
        )
    });
    let form = format!(
        r#"<form class="{}" method="post" action="/auth/login" autocomplete="on" aria-labelledby="{TITLE_ID}"><input type="hidden" name="csrf" value="{}"><input type="hidden" name="rd" value="{}"><input type="hidden" name="sig" value="{}"><input type="hidden" name="{}" value="{}">{}{}{}{keep}<div class="hp" aria-hidden="true" inert><label>Leave this field empty<input type="text" name="{}" tabindex="-1" autocomplete="off" data-lpignore="true" data-1p-ignore="true" data-bwignore="true" value=""></label></div><button class="signin__btn" type="submit">Continue</button></form>"#,
        form_class(l.rd),
        html_escape(l.csrf),
        html_escape(l.rd),
        html_escape(l.sig),
        l.names.form_token,
        html_escape(l.form_token),
        field(
            "login",
            "Login",
            &format!(
                r#"name="login" type="text" autocomplete="username" autocapitalize="off" spellcheck="false" maxlength="128" required autofocus{marks}"#
            ),
            "",
            "",
        ),
        field(
            "password",
            "Password",
            &format!(
                r#"name="password" type="password" autocomplete="current-password" maxlength="{}" required{marks}"#,
                l.max_password_bytes
            ),
            " signin__field--pw",
            &reveal("password"),
        ),
        message(l.error),
        l.names.trap,
    );
    sign_in_document(&card("Authentication", l.host, l.rd, &form), &ICONS)
}

/// What the card of the second step needs. The fields are those of [`super::mfa::Fields`].
pub struct Code<'a> {
    /// The hidden fields of both forms, already written.
    pub hidden: &'a str,
    pub rd: &'a str,
    pub host: Option<&'a str>,
    pub error: Option<&'a str>,
    /// The message that marks the code field as invalid.
    pub invalid: &'a str,
    /// The path of the forms.
    pub action: &'a str,
}

/// The card of the second step: one field for the code of the app, and under it a place for a recovery code. Both
/// forms post to the same address.
pub fn code_page(c: &Code) -> String {
    let marks = invalid_attributes(c.error == Some(c.invalid));
    let form = format!(
        r#"<form class="{} signin__form--head" method="post" action="{}" autocomplete="off" aria-labelledby="{TITLE_ID}">{}{}{}<button class="signin__btn" type="submit">Continue</button></form><details class="signin__alt"><summary class="signin__sum">Use a recovery code</summary><form class="signin__recovery" method="post" action="{}" autocomplete="off">{}{}<button class="signin__btn signin__btn--sec" type="submit">Continue</button></form></details>"#,
        form_class(c.rd),
        c.action,
        c.hidden,
        field(
            "code",
            "Authentication code",
            &format!(
                r#"name="code" type="text" inputmode="numeric" autocomplete="one-time-code" autocapitalize="off" spellcheck="false" maxlength="7" pattern="[0-9 ]{{6,7}}" required autofocus{marks}"#
            ),
            " signin__field--code",
            "",
        ),
        message(c.error),
        c.action,
        c.hidden,
        field(
            "recovery",
            "Recovery code",
            r#"name="code" type="text" autocomplete="off" autocapitalize="off" spellcheck="false" maxlength="11" pattern="[A-Za-z2-7 \-]{10,11}" required"#,
            " signin__field--code",
            "",
        ),
    );
    sign_in_document(&card("Verification", c.host, c.rd, &form), &ICONS[..2])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn login(rd: &str, host: Option<&str>, error: Option<&str>) -> String {
        login_page(&Login {
            csrf: "c",
            rd,
            sig: "s",
            form_token: "t",
            host,
            error,
            max_password_bytes: 1024,
            keep_for: Some("30 days"),
            names: LoginNames {
                form_token: "token",
                trap: "website",
                keep: "keep",
                keep_value: "1",
            },
        })
    }

    #[test]
    fn the_default_return_target_gives_no_band() {
        let page = login("/", Some("app.example.com"), None);
        assert!(!page.contains("signin__band"), "{page}");
        assert!(page.contains("signin__form--bare"));
        assert!(!page.contains("Launcher"));
    }

    #[test]
    fn a_destination_gives_a_band_with_the_host_and_the_path() {
        let page = login("/chdash/x?a=1&b=2", Some("app.example.com"), None);
        assert!(page.contains(r#"<span class="signin__host">app.example.com</span><b class="signin__path">/chdash/x</b>"#), "{page}");
        assert!(!page.contains("signin__form--bare"));
        // The query is not in the band. It stays in the hidden field.
        assert!(!page.contains("a=1&amp;b=2</b>"));
        assert!(page.contains(r#"name="rd" value="/chdash/x?a=1&amp;b=2""#));
        let page = login("/chdash/", None, None);
        assert!(page.contains(r#"<b class="signin__path">/chdash/</b>"#));
        assert!(!page.contains("signin__host"));
    }

    #[test]
    fn the_band_shows_the_address_as_text_and_never_as_markup_or_a_link() {
        for rd in [
            "/\"><script>alert(1)</script>",
            "/a<img src=x onerror=alert(1)>",
            "/javascript:alert(1)",
            "/x'onmouseover='alert(1)",
        ] {
            let page = login(rd, Some("app.example.com"), None);
            for markup in [
                "<script>alert",
                "<img",
                "<a ",
                "onerror=alert(1)>",
                "'onmouseover='",
            ] {
                assert!(!page.contains(markup), "{markup} in {page}");
            }
            assert!(
                !page.contains("href=\"javascript"),
                "no link to the address"
            );
        }
        let page = login("/javascript:alert(1)", Some("app.example.com"), None);
        assert!(page.contains("/javascript:alert(1)</b>"));
        let page = login("/a<b>&\"", Some("app.example.com"), None);
        assert!(
            page.contains(r#"<b class="signin__path">/a&lt;b&gt;&amp;&quot;</b>"#),
            "{page}"
        );
    }

    #[test]
    fn the_band_cuts_a_long_path_and_drops_the_characters_that_reorder_text() {
        let long = format!("/{}", "a".repeat(500));
        let page = login(&long, Some("h.example"), None);
        let shown = page
            .split(r#"<b class="signin__path">"#)
            .nth(1)
            .unwrap()
            .split("</b>")
            .next()
            .unwrap();
        assert_eq!(shown.chars().count(), MAX_PATH_CHARS + 1);
        assert!(shown.ends_with('\u{2026}'));
        let page = login("/a\u{202E}b\u{200B}c\u{0007}d", Some("h.example"), None);
        assert!(
            page.contains(r#"<b class="signin__path">/abcd</b>"#),
            "{page}"
        );
    }

    #[test]
    fn the_band_does_not_depend_on_the_message_or_on_the_form() {
        let band_of = |page: &str| {
            let start = page.find("<div class=\"signin__band\">").unwrap();
            page[start..start + page[start..].find("</div>").unwrap()].to_string()
        };
        let plain = band_of(&login("/chdash/", Some("a.example"), None));
        let refused = band_of(&login(
            "/chdash/",
            Some("a.example"),
            Some(INVALID_CREDENTIALS),
        ));
        let limited = band_of(&login(
            "/chdash/",
            Some("a.example"),
            Some("Too many sign-in attempts."),
        ));
        assert_eq!(plain, refused);
        assert_eq!(plain, limited);
    }

    #[test]
    fn the_password_button_is_hidden_and_the_heading_is_for_a_screen_reader() {
        let page = login("/", Some("a.example"), None);
        assert!(page.contains(r#"<button class="signin__reveal" type="button" data-reveal hidden aria-controls="password" aria-pressed="false" aria-label="Show password">"#));
        assert!(
            page.contains(r#"<h1 class="visually-hidden" id="signin-title">Authentication</h1>"#)
        );
        assert!(page.contains("<title>Authentication</title>"));
        assert_eq!(page.matches("<h1").count(), 1);
        assert!(page.contains(r#"autocomplete="current-password""#));
        assert!(page.contains(r#"autocomplete="username""#));
        assert!(page.contains("autofocus"));
        assert!(page.contains(r#"name="keep" value="1">Keep me signed in for 30 days</label>"#));
        assert!(!page.contains("style=") && !page.contains("<script>"));
    }

    #[test]
    fn a_refusal_marks_both_fields_and_a_limit_does_not() {
        let refused = login("/", None, Some(INVALID_CREDENTIALS));
        assert_eq!(refused.matches(r#"aria-invalid="true""#).count(), 2);
        assert!(refused.contains(r#"id="signin-msg" role="alert">"#));
        assert!(refused.contains("Invalid login or password"));
        let limited = login(
            "/",
            None,
            Some("Too many sign-in attempts from this address. Try again later."),
        );
        assert_eq!(limited.matches("aria-invalid").count(), 0);
        assert!(limited.contains(r#"role="alert""#));
        let quiet = login("/", None, None);
        assert!(!quiet.contains("role=\"alert\"") && quiet.contains("aria-live=\"polite\""));
    }

    #[test]
    fn the_card_of_the_code_has_a_band_only_with_a_destination() {
        let code = |rd: &str, error: Option<&str>| {
            code_page(&Code {
                hidden: r#"<input type="hidden" name="pending" value="p">"#,
                rd,
                host: Some("app.example.com"),
                error,
                invalid: "Invalid code",
                action: "/auth/login/code",
            })
        };
        let with = code("/chdash/", None);
        assert!(with.contains("signin__band") && with.contains("/chdash/</b>"));
        assert!(with.contains(r#"<h1 class="visually-hidden" id="signin-title">Verification</h1>"#));
        let without = code("/", None);
        assert!(!without.contains("signin__band"));
        assert_eq!(without.matches("<form").count(), 2);
        assert!(without.contains(r#"autocomplete="one-time-code""#));
        let error = code("/", Some("Invalid code"));
        assert!(error.contains(r#"aria-invalid="true""#));
    }
}
