//! The sign-in page and the card of the second step. They share one look: a flat card, a band with the
//! destination, and fields with a floating label.
//!
//! The band shows the page that the visitor asked for. It exists only when the sign-in has a
//! destination: the signed `rd` of the page is not the default `/`. Without a destination, the card has no
//! band and starts with the fields. The band depends on `rd` only. It never
//! depends on what a visitor typed, so a refused sign-in keeps the same answer for each cause.
//!
//! A floating label is a real `<label for>` and the field has the placeholder `" "`. The stylesheet reads
//! `:placeholder-shown`, `:focus` and the autofill states. No script is needed. The button that shows the
//! password is in the page with the attribute `hidden`: the shared script shows it.
//!
//! A message (a refusal, a limit, a wrong code) is a strip at the bottom edge of the card, under the button. It has the
//! height and the look of the band, in the colour of danger. The strip exists in the page only when there is a
//! message: a card without a message has no empty place for it.
//!
//! The pages go through the pass of [`super::mask`]. The classes below are in the shared stylesheet.

use super::sign_in_document;
use crate::util::html_escape;

/// The sentence of a refused sign-in. Each cause of a refusal gives this sentence.
pub const INVALID_CREDENTIALS: &str = "Invalid login or password";

/// The id of the hidden heading. The form takes its name from it.
const TITLE_ID: &str = "signin-title";
/// The id of the strip of the message. A field that the message concerns points to it.
const MESSAGE_ID: &str = "signin-msg";

/// The longest path (in characters) that the band shows. The stylesheet cuts what does not fit.
const MAX_PATH_CHARS: usize = 96;

/// The sprite symbols that the sign-in cards draw.
pub const ICONS: [&str; 4] = ["i-lock", "i-alert-triangle", "i-eye", "i-eye-off"];

/// Tells whether a character can change the order or the look of the text that is around it. The band does
/// not show such a character: an address cannot reorder or hide the text that stands before it.
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

/// What the band says about the destination: its path. The path is the part of `rd` before the query, without the
/// characters that [`is_unsafe_char`] names, and cut at [`MAX_PATH_CHARS`] with an ellipsis. `None` when `rd` is the
/// default. A page of the portal has no name of its own in the band: a visitor can ask for any path, and the server
/// signs it, so a name would tell the pages of the product from the other paths.
///
/// The destination is always on the host of the page: `rd` is a path that the server signed, and the browser goes to
/// it with no host. So the band shows no host.
fn destination(rd: &str) -> Option<String> {
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
fn band(rd: &str) -> String {
    let Some(text) = destination(rd) else {
        return String::new();
    };
    format!(
        r##"<div class="signin__band"><svg class="signin__lock" aria-hidden="true" focusable="false"><use href="#i-lock"/></svg><span class="signin__to" title="{}"><span class="signin__tag">Redirect</span> <b class="signin__path">{}</b></span></div>"##,
        html_escape(&text),
        html_escape(&text),
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

/// The strip of the message. It is empty when there is no message: the card then has no strip and no empty
/// place. The page has the strip after the button, at the bottom edge of the card.
fn message(text: Option<&str>) -> String {
    text.map_or_else(String::new, |text| {
        format!(
            r##"<p class="signin__msg" id="{MESSAGE_ID}" role="alert"><svg class="signin__mi" aria-hidden="true" focusable="false"><use href="#i-alert-triangle"/></svg><span>{}</span></p>"##,
            html_escape(text),
        )
    })
}

/// The button that shows and hides the password. The page has it hidden: the script shows it.
fn reveal(target: &str) -> String {
    format!(
        r##"<button class="signin__reveal" type="button" data-reveal hidden aria-controls="{target}" aria-pressed="false" aria-label="Show password"><svg class="signin__show" aria-hidden="true" focusable="false"><use href="#i-eye"/></svg><svg class="signin__hide" aria-hidden="true" focusable="false"><use href="#i-eye-off"/></svg></button>"##
    )
}

/// The card, with its hidden heading and its band. `form` is the markup that follows. `strip` is the strip of the
/// message: it is the last part of the card.
fn card(title: &str, rd: &str, form: &str, strip: &str) -> String {
    format!(
        r#"<h1 class="visually-hidden" id="{TITLE_ID}">{title}</h1>{}{form}{strip}"#,
        band(rd)
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
    /// A sentence for the strip of the message, for example [`INVALID_CREDENTIALS`].
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
    // One text says what the box does: "Stay signed in for 30 days". It is the label, and so the name of the box.
    let keep = l.keep_for.map_or_else(String::new, |words| {
        format!(
            r#"<label class="signin__check"><input type="checkbox" name="{}" value="{}">Stay signed in for {}</label>"#,
            l.names.keep,
            l.names.keep_value,
            html_escape(words),
        )
    });
    let form = format!(
        r#"<form class="{}" method="post" action="/auth/login" autocomplete="on" aria-labelledby="{TITLE_ID}"><input type="hidden" name="csrf" value="{}"><input type="hidden" name="rd" value="{}"><input type="hidden" name="sig" value="{}"><input type="hidden" name="{}" value="{}">{}{}{keep}<div class="hp" aria-hidden="true" inert><label>Leave this field empty<input type="text" name="{}" tabindex="-1" autocomplete="off" data-lpignore="true" data-1p-ignore="true" data-bwignore="true" value=""></label></div><button class="signin__btn" type="submit">Continue</button></form>"#,
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
        l.names.trap,
    );
    let strip = message(l.error);
    sign_in_document(
        &card("Authentication", l.rd, &form, &strip),
        &ICONS,
        l.error.is_some(),
    )
}

/// What the card of the second step needs. The fields are those of [`super::mfa::Fields`].
pub struct Code<'a> {
    /// The hidden fields of both forms, already written.
    pub hidden: &'a str,
    pub rd: &'a str,
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
        r#"<form class="{} signin__form--head" method="post" action="{}" autocomplete="off" aria-labelledby="{TITLE_ID}">{}{}<button class="signin__btn" type="submit">Continue</button></form><details class="signin__alt"><summary class="signin__sum">Use a recovery code</summary><form class="signin__recovery" method="post" action="{}" autocomplete="off">{}{}<button class="signin__btn signin__btn--sec" type="submit">Continue</button></form></details>"#,
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
    let strip = message(c.error);
    sign_in_document(
        &card("Verification", c.rd, &form, &strip),
        &ICONS[..2],
        c.error.is_some(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn login(rd: &str, error: Option<&str>) -> String {
        login_page(&Login {
            csrf: "c",
            rd,
            sig: "s",
            form_token: "t",
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
        let page = login("/", None);
        assert!(!page.contains("signin__band"), "{page}");
        assert!(page.contains("signin__form--bare"));
        assert!(!page.contains("Launcher"));
    }

    #[test]
    fn a_destination_gives_a_band_with_the_path_and_no_host() {
        let page = login("/chdash/x?a=1&b=2", None);
        assert!(
            page.contains(r#"<b class="signin__path">/chdash/x</b>"#),
            "{page}"
        );
        assert!(!page.contains("example.com"));
        assert!(!page.contains("signin__form--bare"));
        // The query is not in the band. It stays in the hidden field.
        assert!(!page.contains("a=1&amp;b=2</b>"));
        assert!(page.contains(r#"name="rd" value="/chdash/x?a=1&amp;b=2""#));
        let page = login("/chdash/", None);
        assert!(page.contains(r#"<b class="signin__path">/chdash/</b>"#));
    }

    #[test]
    fn a_page_of_the_portal_shows_its_path_like_any_other_page() {
        for rd in [
            "/auth/settings/services",
            "/auth/settings",
            "/auth/settings/activity",
            "/auth/settings/inbox",
            "/auth/settings/security",
            "/auth/settings/new-user",
            "/auth/keys/new",
            "/auth/settings/users/alice",
            "/auth/settingsX",
            "/chdash/reports",
        ] {
            let page = login(rd, None);
            // The whole band: the tag, the path in the title and in the text, and nothing else.
            let start = page.find(r#"<div class="signin__band">"#).unwrap();
            let band = &page[start..start + page[start..].find("</div>").unwrap()];
            assert!(
                band.contains(&format!(
                    r#"title="{rd}"><span class="signin__tag">Redirect</span> <b class="signin__path">{rd}</b></span>"#
                )),
                "{rd}: {band}"
            );
            assert!(
                !band.contains("Settings") && !band.contains("Services"),
                "{rd}"
            );
        }
        // The query stays out of the band.
        let page = login("/auth/settings/services?svc=a&f=check", None);
        assert!(page.contains(r#"<b class="signin__path">/auth/settings/services</b>"#));
    }

    #[test]
    fn the_band_shows_the_address_as_text_and_never_as_markup_or_a_link() {
        for rd in [
            "/\"><script>alert(1)</script>",
            "/a<img src=x onerror=alert(1)>",
            "/javascript:alert(1)",
            "/x'onmouseover='alert(1)",
        ] {
            let page = login(rd, None);
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
        let page = login("/javascript:alert(1)", None);
        assert!(page.contains("/javascript:alert(1)</b>"));
        let page = login("/a<b>&\"", None);
        assert!(
            page.contains(r#"<b class="signin__path">/a&lt;b&gt;&amp;&quot;</b>"#),
            "{page}"
        );
    }

    #[test]
    fn the_band_cuts_a_long_path_and_drops_the_characters_that_reorder_text() {
        let long = format!("/{}", "a".repeat(500));
        let page = login(&long, None);
        let shown = page
            .split(r#"<b class="signin__path">"#)
            .nth(1)
            .unwrap()
            .split("</b>")
            .next()
            .unwrap();
        assert_eq!(shown.chars().count(), MAX_PATH_CHARS + 1);
        assert!(shown.ends_with('\u{2026}'));
        let page = login("/a\u{202E}b\u{200B}c\u{0007}d", None);
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
        let plain = band_of(&login("/chdash/", None));
        let refused = band_of(&login("/chdash/", Some(INVALID_CREDENTIALS)));
        let limited = band_of(&login("/chdash/", Some("Too many sign-in attempts.")));
        assert_eq!(plain, refused);
        assert_eq!(plain, limited);
    }

    #[test]
    fn the_password_button_is_hidden_and_the_heading_is_for_a_screen_reader() {
        let page = login("/", None);
        assert!(page.contains(r#"<button class="signin__reveal" type="button" data-reveal hidden aria-controls="password" aria-pressed="false" aria-label="Show password">"#));
        assert!(
            page.contains(r#"<h1 class="visually-hidden" id="signin-title">Authentication</h1>"#)
        );
        assert!(page.contains("<title>Authentication</title>"));
        assert_eq!(page.matches("<h1").count(), 1);
        assert!(page.contains(r#"autocomplete="current-password""#));
        assert!(page.contains(r#"autocomplete="username""#));
        assert!(page.contains("autofocus"));
        assert!(page.contains(r#"name="keep" value="1">Stay signed in for 30 days</label>"#));
        assert!(!page.contains("style=") && !page.contains("<script>"));
    }

    #[test]
    fn a_refusal_marks_both_fields_and_a_limit_does_not() {
        let refused = login("/", Some(INVALID_CREDENTIALS));
        assert_eq!(refused.matches(r#"aria-invalid="true""#).count(), 2);
        assert!(refused.contains(r#"id="signin-msg" role="alert">"#));
        assert!(refused.contains("Invalid login or password"));
        assert!(refused.contains(r#"class="signin signin--strip""#));
        let limited = login(
            "/",
            Some("Too many sign-in attempts from this address. Try again later."),
        );
        assert_eq!(limited.matches("aria-invalid").count(), 0);
        assert!(limited.contains(r#"role="alert""#));
        let quiet = login("/", None);
        assert!(!quiet.contains("role=\"alert\"") && !quiet.contains("aria-live"));
    }

    #[test]
    fn the_strip_exists_only_with_a_message_and_comes_after_the_button() {
        let quiet = login("/", None);
        // No message: no strip, no empty place, no reference to a strip, no room made for it.
        for word in ["signin__msg", "signin-msg", "signin__mi", "signin--strip"] {
            assert!(!quiet.contains(word), "{word} in {quiet}");
        }
        assert!(quiet.contains(r#"<div class="signin">"#));
        let page = login("/chdash/", Some(INVALID_CREDENTIALS));
        assert_eq!(page.matches("signin__msg").count(), 1, "{page}");
        assert_eq!(page.matches(r#"role="alert""#).count(), 1);
        let button = page.find(r#"<button class="signin__btn""#).unwrap();
        let form_end = page.find("</form>").unwrap();
        let strip = page.find(r#"<p class="signin__msg""#).unwrap();
        // The strip is after the button and after the form: the last part of the card.
        assert!(button < form_end && form_end < strip, "{page}");
        assert!(page[strip..].contains("</p></div></main>"), "{page}");
        assert!(page.contains(
            r##"<svg class="signin__mi" aria-hidden="true" focusable="false"><use href="#i-alert-triangle"/></svg><span>Invalid login or password</span></p>"##
        ));
        // Each notice of the page uses the same strip.
        for text in [
            "Too many sign-in attempts from this address. Try again later.",
            "The page was open too long. Enter your login and password again.",
        ] {
            let page = login("/", Some(text));
            let strip = page.find(r#"<p class="signin__msg""#).unwrap();
            assert!(page.find("</form>").unwrap() < strip);
            assert!(page.contains(&format!("<span>{text}</span></p>")), "{page}");
            assert_eq!(page.matches("aria-invalid").count(), 0);
        }
    }

    #[test]
    fn the_box_has_one_text_that_says_how_long_the_session_stays() {
        let page = login("/", None);
        assert!(page.contains(
            r#"<label class="signin__check"><input type="checkbox" name="keep" value="1">Stay signed in for 30 days</label>"#
        ));
        // One text only: no second wording, no title and no hidden description.
        assert!(!page.contains("Remember me") && !page.contains("Keep me"));
        assert!(!page.contains("signin-keep") && !page.contains(r#"title="Stay signed in"#));
        // The box comes after the eye button and before the button of the form.
        let eye = page.find("data-reveal").unwrap();
        let keep = page.find("name=\"keep\"").unwrap();
        let button = page.find("<button class=\"signin__btn\"").unwrap();
        assert!(eye < keep && keep < button);
        // Without a life, the form has no box and no description.
        let none = login_page(&Login {
            csrf: "c",
            rd: "/",
            sig: "s",
            form_token: "t",
            error: None,
            max_password_bytes: 1024,
            keep_for: None,
            names: LoginNames {
                form_token: "token",
                trap: "website",
                keep: "keep",
                keep_value: "1",
            },
        });
        assert!(!none.contains("Stay signed in") && !none.contains("signin__check"));
    }

    #[test]
    fn the_sign_in_pages_are_anonymous_with_no_mark_an_empty_icon_and_no_product_word() {
        let code = |rd: &str, error: Option<&str>| {
            code_page(&Code {
                hidden: "",
                rd,
                error,
                invalid: "Invalid code",
                action: "/auth/login/code",
            })
        };
        let pages = [
            login("/", None),
            login("/chdash/x", None),
            login("/", Some("Invalid login or password")),
            code("/", None),
            code("/chdash/", Some("Invalid code")),
        ];
        for page in &pages {
            for part in [
                "i-logo",
                "logoDot",
                "appMark",
                "appSep",
                "image/svg+xml",
                "apple-touch",
                "<svg class=\"appMark",
            ] {
                assert!(!page.contains(part), "{part}: {page}");
            }
            assert!(!page.to_ascii_lowercase().contains("portal"), "{page}");
            // The stylesheet, and the empty icon that no file backs.
            assert_eq!(page.matches("<link ").count(), 2, "{page}");
            assert_eq!(page.matches("rel=\"icon\"").count(), 1, "{page}");
            assert!(page.contains("href=\"data:,\""), "{page}");
        }
    }

    #[test]
    fn the_card_of_the_code_has_a_band_only_with_a_destination() {
        let code = |rd: &str, error: Option<&str>| {
            code_page(&Code {
                hidden: r#"<input type="hidden" name="pending" value="p">"#,
                rd,
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
        // The strip of the error is after both forms and after the link of the recovery code. A card without an
        // error has no strip.
        let strip = error.find(r#"<p class="signin__msg""#).unwrap();
        assert!(error.rfind("</details>").unwrap() < strip, "{error}");
        assert!(
            error.contains(r#"role="alert"><svg"#)
                && error.contains("<span>Invalid code</span></p>")
        );
        assert!(error.contains(r#"class="signin signin--strip""#));
        assert!(!without.contains("signin__msg") && !without.contains("aria-live"));
    }
}
