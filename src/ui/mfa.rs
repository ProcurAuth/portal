//! The pages of the second factor: the cards of the sign-in (the code, the forced enrolment, the recovery codes) and
//! the section "Two-factor" of the settings.
//!
//! The cards go through the pass of [`super::mask`], like the sign-in card. Their classes are in the shared stylesheet.
//! A card has no explanatory sentence: a label names each field. Every page works without script. The script only adds
//! the Copy button of the recovery codes.

use std::fmt::Write as _;

use super::components::{ask, Ask, Side, Strength};
use super::settings::{fill_page, notice_html, Frame, Notice, Tab};
use super::{auth_document, status_icon};
use crate::mfa::Enrolment;
use crate::util::{html_escape, time_tag};

/// The path of the second step of the sign-in.
pub const CODE_PATH: &str = "/auth/login/code";
/// The path of the forced enrolment of the sign-in.
pub const ENROL_PATH: &str = "/auth/login/enrol";
/// The page "Two-factor" of the settings. The query `enable=1` opens the enrolment.
pub const SECURITY_PATH: &str = "/auth/settings/security";
pub const SECURITY_ENABLE_PATH: &str = "/auth/security/enable";
pub const SECURITY_DISABLE_PATH: &str = "/auth/security/disable";
pub const SECURITY_RECOVERY_PATH: &str = "/auth/security/recovery";
/// The superadmin removes the second factor of another account.
pub const RESET_PATH: &str = "/auth/users/reset-two-factor";

/// The hidden fields that the two steps of the sign-in carry.
pub struct Fields<'a> {
    pub csrf: &'a str,
    pub rd: &'a str,
    pub sig: &'a str,
    /// The host that the visitor asked for. The band of the card shows it.
    pub host: Option<&'a str>,
    /// The pending token (see [`crate::mfa::sign_pending`]).
    pub pending: &'a str,
}

fn hidden(f: &Fields) -> String {
    format!(
        r#"<input type="hidden" name="csrf" value="{}"><input type="hidden" name="rd" value="{}"><input type="hidden" name="sig" value="{}"><input type="hidden" name="pending" value="{}">"#,
        html_escape(f.csrf),
        html_escape(f.rd),
        html_escape(f.sig),
        html_escape(f.pending),
    )
}

fn error_line(error: Option<&str>) -> String {
    error
        .map(|text| {
            format!(
                r#"<div class="error" role="alert" aria-live="assertive">{}</div>"#,
                html_escape(text)
            )
        })
        .unwrap_or_default()
}

/// The sentence of a wrong code. The code field takes the mark "invalid" for it.
pub const INVALID_CODE: &str = "Invalid code";

/// The card of the second step: one field for the code of the app, and under it a place for a recovery code. Both
/// forms post to the same address, and the server accepts either kind of code in either field. The card has the
/// look of the sign-in page (see [`super::signin`]).
pub fn code_card(f: &Fields, error: Option<&str>) -> String {
    super::signin::code_page(&super::signin::Code {
        hidden: &hidden(f),
        rd: f.rd,
        host: f.host,
        error,
        invalid: INVALID_CODE,
        action: CODE_PATH,
    })
}

/// The secret of an enrolment: the QR code, the secret in groups of four characters and the link, both as text.
/// The block is the same in the card and in the settings.
pub fn secret_block(e: &Enrolment) -> String {
    let qr = e.qr.as_deref().unwrap_or_default();
    format!(
        r#"<div class="mfaScan">{qr}<p class="mfaLabel">Secret</p><code class="mfaSecret">{}</code><p class="mfaLabel">Link</p><code class="mfaUri">{}</code></div>"#,
        html_escape(&e.grouped),
        html_escape(&e.uri),
    )
}

/// The card of the forced enrolment: the secret, one field for the first code, one button.
pub fn enrol_card(f: &Fields, enrolment: &Enrolment, error: Option<&str>) -> String {
    auth_document(
        "Authentication",
        &format!(
            r#"<h1>Two-factor</h1>{}<form method="post" action="{ENROL_PATH}" autocomplete="off">{}<input type="hidden" name="enrol" value="{}">{}<label for="code">Code from the app</label><input class="otp" id="code" name="code" type="text" inputmode="numeric" autocomplete="one-time-code" autocapitalize="off" spellcheck="false" maxlength="7" pattern="[0-9 ]{{6,7}}" required autofocus><button class="button button--primary" type="submit">Turn on and continue</button></form>"#,
            error_line(error),
            hidden(f),
            html_escape(&enrolment.token),
            secret_block(enrolment),
        ),
    )
}

/// The recovery codes in a field that the person can select, with a Copy button that needs script. `title` names the
/// field for a screen reader.
pub fn codes_block(codes: &[String]) -> String {
    format!(
        r#"<div class="mfaBox"><textarea class="mfaCopy" readonly rows="{}" spellcheck="false" aria-label="Recovery codes">{}</textarea><button class="button copyBtn" type="button" data-copy>Copy</button></div>"#,
        codes.len(),
        html_escape(&codes.join("\n")),
    )
}

/// The card after the forced enrolment: the codes, shown once, and the way on.
pub fn codes_card(codes: &[String], next: &str) -> String {
    auth_document(
        "Authentication",
        &format!(
            r#"<h1>Recovery codes</h1>{}<a class="button button--primary" href="{}">Continue</a>"#,
            codes_block(codes),
            html_escape(next),
        ),
    )
}

// The section "Two-factor" of the settings ------------------------------------------------------

/// What the section shows.
pub enum State<'a> {
    /// The account has no second factor. The person can turn it on.
    Off,
    /// The enrolment: the secret, and one field for the first code.
    Enrol { enrolment: &'a Enrolment },
    /// The account has a second factor.
    On {
        since: i64,
        recovery_left: u32,
        /// The policy is `required`: the person cannot turn it off.
        locked_on: bool,
    },
    /// The codes that a turn-on or a renewal made, shown once.
    Codes { codes: &'a [String] },
}

pub struct SecurityView<'a> {
    pub notice: Option<Notice>,
    /// The anti-CSRF token of the forms of the section.
    pub token: &'a str,
    pub state: State<'a>,
}

fn csrf(token: &str) -> String {
    format!(
        r#"<input type="hidden" name="csrf" value="{}">"#,
        html_escape(token)
    )
}

/// The fields of the forms that ask the password and a code.
fn proof_fields(token: &str, id: &str) -> String {
    format!(
        r#"{}<label class="visually-hidden" for="{id}-password">Password</label><input class="uiInput" id="{id}-password" name="password" type="password" autocomplete="current-password" maxlength="1024" required placeholder="Password"><label class="visually-hidden" for="{id}-code">Code</label><input class="uiInput otp" id="{id}-code" name="code" type="text" inputmode="numeric" autocomplete="one-time-code" autocapitalize="off" spellcheck="false" maxlength="11" required placeholder="Code or recovery code">"#,
        csrf(token),
    )
}

/// The page "Two-factor": one panel, dense, with the state, the actions of the state and nothing else.
pub fn security_page(frame: &Frame, view: &SecurityView) -> String {
    let mut out = String::new();
    notice_html(&mut out, view.notice);
    let mut body = String::new();
    let mut status = String::new();
    match &view.state {
        State::Off => {
            status.push_str(&status_icon("warn", "alert-triangle", "Off"));
            status.push_str(r#"<span class="count">Off</span>"#);
            let _ = write!(
                body,
                r#"<a class="button button--primary" href="{SECURITY_PATH}?enable=1">Turn on</a>"#
            );
        }
        State::Enrol { enrolment } => {
            status.push_str(r#"<span class="count">Turn on</span>"#);
            let _ = write!(
                body,
                r#"<form class="mfaForm" method="post" action="{SECURITY_ENABLE_PATH}" autocomplete="off">{}<input type="hidden" name="enrol" value="{}">{}<label class="field__label" for="enrol-code">Code from the app</label><input class="uiInput otp" id="enrol-code" name="code" type="text" inputmode="numeric" autocomplete="one-time-code" autocapitalize="off" spellcheck="false" maxlength="7" pattern="[0-9 ]{{6,7}}" required autofocus><div class="inline"><button class="button button--primary" type="submit">Turn on</button><a class="button" href="{SECURITY_PATH}">Cancel</a></div></form>"#,
                csrf(view.token),
                html_escape(&enrolment.token),
                secret_block(enrolment),
            );
        }
        State::On {
            since,
            recovery_left,
            locked_on,
        } => {
            status.push_str(&status_icon("ok", "circle-check", "On"));
            status.push_str(r#"<span class="count">On</span>"#);
            let _ = write!(
                body,
                r#"<dl class="factLine"><div><dt>Since</dt><dd>{}</dd></div><div><dt>Recovery codes left</dt><dd>{recovery_left}</dd></div></dl>"#,
                time_tag(*since, false)
            );
            let mut actions = String::new();
            let _ = write!(
                actions,
                r#"<details class="mfaDo"><summary class="button">New recovery codes</summary><form class="mfaPanel" method="post" action="{SECURITY_RECOVERY_PATH}">{}<button class="button button--primary" type="submit">Make new codes</button></form></details>"#,
                proof_fields(view.token, "recovery"),
            );
            if !locked_on {
                let _ = write!(
                    actions,
                    r#"<details class="mfaDo"><summary class="button button--danger">Turn off</summary><form class="mfaPanel" method="post" action="{SECURITY_DISABLE_PATH}">{}<button class="button button--danger" type="submit">Turn off</button></form></details>"#,
                    proof_fields(view.token, "disable"),
                );
            }
            let _ = write!(body, r#"<div class="mfaActions">{actions}</div>"#);
        }
        State::Codes { codes } => {
            status.push_str(r#"<span class="count">Recovery codes</span>"#);
            let _ = write!(
                body,
                r#"{}<a class="button button--primary" href="{SECURITY_PATH}">Done</a>"#,
                codes_block(codes),
            );
        }
    }
    let _ = write!(
        out,
        r#"<section class="zone zone--security" aria-labelledby="mfa-title"><div class="zone__head"><h2 class="title" id="mfa-title">Two-factor</h2>{status}</div><div class="scroll"><div class="mfaBody">{body}</div></div></section>"#
    );
    fill_page(Tab::Security, frame, "Two-factor", &out)
}

/// The reset of the second factor of another account, in the page of the account: the state and, for the superadmin,
/// a confirmation in place. Nothing for an account that has none.
pub fn account_tool(login: &str, enabled: bool, token: &str) -> String {
    if !enabled {
        return String::new();
    }
    let mut out = status_icon("ok", "circle-check", "Two-factor is on");
    out.push_str(&ask(&Ask {
        label: &format!("Reset the two-factor of {login}"),
        icon: "refresh",
        question: &format!("Reset the two-factor of {login}?"),
        verb: "Reset",
        action: RESET_PATH,
        fields_html: &format!(
            r#"{}<input type="hidden" name="login" value="{}">"#,
            csrf(token),
            html_escape(login)
        ),
        strength: Strength::Light,
        name: login,
        danger: true,
        side: Side::Start,
    }));
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mfa::Mfa;
    use crate::secretbox::SecretBox;

    fn enrolment() -> Enrolment {
        let mfa = Mfa::new(SecretBox::new(&[3; 32]).unwrap());
        mfa.begin("alice", "Portal", 1_700_000_000).unwrap()
    }

    fn fields() -> Fields<'static> {
        Fields {
            csrf: "csrf-value",
            rd: "/x?a=1&b=2",
            sig: "sig",
            host: Some("app.example.com"),
            pending: "pending.token",
        }
    }

    #[test]
    fn the_card_of_the_code_has_the_two_forms_the_hidden_fields_and_an_error_line() {
        let page = code_card(&fields(), Some("Invalid code"));
        assert_eq!(page.matches(r#"action="/auth/login/code""#).count(), 2);
        assert_eq!(
            page.matches(r#"name="pending" value="pending.token""#)
                .count(),
            2
        );
        assert!(page.contains(r#"autocomplete="one-time-code""#));
        assert!(page.contains(r#"inputmode="numeric""#));
        assert!(page.contains(r#"maxlength="11""#));
        assert!(
            page.contains(r#"role="alert"><svg"#) && page.contains("<span>Invalid code</span>")
        );
        assert!(
            page.contains("value=\"/x?a=1&amp;b=2\""),
            "the target is escaped"
        );
        assert!(!page.contains("<script>"), "no inline script");
        assert!(!code_card(&fields(), None).contains("role=\"alert\""));
    }

    #[test]
    fn the_card_of_the_enrolment_shows_the_qr_the_secret_and_the_link_as_text() {
        let e = enrolment();
        let page = enrol_card(&fields(), &e, None);
        assert!(page.contains("<svg class=\"qr\""));
        assert!(page.contains(&html_escape(&e.grouped)));
        assert!(page.contains(&html_escape(&e.uri)));
        assert!(page.contains(&format!(r#"name="enrol" value="{}""#, e.token)));
        assert!(
            !page.contains("<img") && !page.contains("data:"),
            "no image request"
        );
        assert!(!page.contains("style="), "the CSP forbids inline style");
    }

    #[test]
    fn no_page_of_the_second_factor_names_the_product_except_in_the_link_of_the_app() {
        use crate::ui::body_words::{assert_clean, product_words};
        let e = enrolment();
        let codes = ["abcde-fghij".to_string()];
        assert_clean("code card", &code_card(&fields(), Some("Invalid code")));
        assert_clean("codes card", &codes_card(&codes, "/"));
        // The link of the app holds the name of the instance as its issuer: this is the only place.
        let card = enrol_card(&fields(), &e, None);
        assert_clean("enrol card", &card);
        assert!(card.contains("issuer=Portal"));
        let frame = crate::ui::fixtures::full_frame();
        let states = [
            State::Off,
            State::Enrol { enrolment: &e },
            State::On {
                since: 1_700_000_000,
                recovery_left: 3,
                locked_on: false,
            },
            State::Codes { codes: &codes },
        ];
        for state in states {
            let page = security_page(
                &frame,
                &SecurityView {
                    notice: None,
                    token: "t",
                    state,
                },
            );
            assert_clean("security", &page);
        }
        // Another name of the instance: the body has nothing of it, with or without the link.
        let mfa = Mfa::new(SecretBox::new(&[3; 32]).unwrap());
        let other = mfa.begin("alice", "Prod", 1_700_000_000).unwrap();
        let page = enrol_card(&fields(), &other, None);
        assert!(product_words(&page).is_empty());
        assert!(page.contains("issuer=Prod"));
    }

    #[test]
    fn the_recovery_codes_are_in_a_field_that_a_person_can_select() {
        let codes = ["abcde-fghij".to_string(), "klmno-pqrst".to_string()];
        let page = codes_card(&codes, "/next?x=1&y=2");
        assert!(page.contains("abcde-fghij\nklmno-pqrst</textarea>"));
        assert!(page.contains("data-copy"));
        assert!(page.contains("href=\"/next?x=1&amp;y=2\""));
    }
}
