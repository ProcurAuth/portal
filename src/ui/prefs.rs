//! The page "Inbox > Preferences": the digest and the quiet hours of the account.
//!
//! The page is HTML from the server and needs no script. It is a form that posts to [`PREFS`] with the token `csrf`. The
//! account has one time zone on the page: the digest at a time of the day and the quiet hours both use it.
//!
//! The values of the account replace the defaults of the file. The server refuses a value and changes nothing: it
//! answers with status 422, and the page shows the refused field with the values that the person typed.

use std::collections::HashMap;
use std::fmt::Write as _;

use inbox_core::proto::{DigestMode, DigestPrefs, Prefs, QuietPrefs};

use super::components::icon;
use super::settings::{fill_page, notice_html, Frame, Notice, Tab};
use crate::config::DigestConfig;
use crate::inbox::prefs::{self, PrefsError};
use crate::util::html_escape;

pub const PREFS: &str = "/auth/settings/inbox/prefs";

/// The text of the outcome of the form, from a fixed list.
pub fn notice_for(code: &str) -> Option<Notice> {
    let (text, ok) = match code {
        "saved" => (
            "Your preferences are saved. Your phones read them at their next connection.",
            true,
        ),
        "reset" => (
            "Your values are removed. The defaults of the file apply.",
            true,
        ),
        "failed" => ("The inbox could not save this. Try again.", false),
        _ => return None,
    };
    Some(Notice { text, ok })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DigestChoice {
    Daily,
    Every,
    Off,
}

/// The values of the form: what the page shows, and what a POST sends. A field is the text of the control.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Form {
    pub digest: DigestChoice,
    pub at: String,
    pub tz: String,
    pub hours: String,
    pub from: String,
    pub to: String,
    pub high_rings: bool,
}

impl Form {
    /// The values that the page shows: the preferences of the account, and for a block that the account never set, the
    /// default of the file.
    pub fn from_prefs(p: &Prefs, file: &DigestConfig) -> Form {
        let (digest, mut at, mut tz, mut hours) = match (&p.digest, file) {
            (Some(d), _) => (
                match d.mode {
                    DigestMode::Daily => DigestChoice::Daily,
                    DigestMode::Every => DigestChoice::Every,
                    DigestMode::Off => DigestChoice::Off,
                },
                d.at.clone().unwrap_or_default(),
                d.tz.clone().unwrap_or_default(),
                d.every_h.map(|h| h.to_string()).unwrap_or_default(),
            ),
            (
                None,
                DigestConfig::Daily {
                    hour,
                    minute,
                    timezone,
                },
            ) => (
                DigestChoice::Daily,
                format!("{hour:02}:{minute:02}"),
                timezone.clone(),
                String::new(),
            ),
            (None, DigestConfig::Every(period)) => (
                DigestChoice::Every,
                String::new(),
                String::new(),
                (period.as_secs() / 3600).max(1).to_string(),
            ),
            (None, DigestConfig::Off) => (
                DigestChoice::Off,
                String::new(),
                String::new(),
                String::new(),
            ),
        };
        if at.is_empty() {
            at = "09:00".into();
        }
        if hours.is_empty() {
            hours = "4".into();
        }
        let (from, to, high_rings) = match &p.quiet {
            Some(q) => {
                if tz.is_empty() {
                    tz = q.tz.clone();
                }
                (q.from.clone(), q.to.clone(), q.high_rings)
            }
            None => (String::new(), String::new(), false),
        };
        if tz.is_empty() {
            tz = match file {
                DigestConfig::Daily { timezone, .. } => timezone.clone(),
                _ => "UTC".into(),
            };
        }
        Form {
            digest,
            at,
            tz,
            hours,
            from,
            to,
            high_rings,
        }
    }

    /// The values of a POST, as typed. A missing field is empty text. The checkbox is true when it is present.
    pub fn from_fields(f: &HashMap<String, String>) -> Form {
        let get = |name: &str| {
            f.get(name)
                .map(|v| v.trim().to_string())
                .unwrap_or_default()
        };
        Form {
            digest: match get("digest").as_str() {
                "every" => DigestChoice::Every,
                "off" => DigestChoice::Off,
                _ => DigestChoice::Daily,
            },
            at: get("at"),
            tz: get("tz"),
            hours: get("hours"),
            from: get("from"),
            to: get("to"),
            high_rings: f.contains_key("high"),
        }
    }

    /// The preferences of the form. Nothing is changed to make a value fit: a bad value is the error, with its field.
    /// `fields` tells whether the radio `digest` is a known word.
    pub fn to_prefs(&self, fields: &HashMap<String, String>) -> Result<Prefs, PrefsError> {
        if !matches!(
            fields.get("digest").map(String::as_str),
            Some("daily" | "every" | "off")
        ) {
            return Err(PrefsError {
                field: "digest",
                expected: "daily, every or off",
                value: fields
                    .get("digest")
                    .cloned()
                    .unwrap_or_default()
                    .chars()
                    .take(40)
                    .collect(),
            });
        }
        // A zone that is typed is checked even when no block uses it: the server does not ignore a bad value.
        if !self.tz.is_empty() && !prefs::is_zone(&self.tz) {
            return Err(PrefsError {
                field: "tz",
                expected: "the name of an IANA time zone with its exact capital letters, such as Europe/Paris or UTC",
                value: self.tz.chars().take(80).collect(),
            });
        }
        let digest = Some(match self.digest {
            DigestChoice::Off => DigestPrefs {
                mode: DigestMode::Off,
                at: None,
                tz: None,
                every_h: None,
            },
            DigestChoice::Daily => DigestPrefs {
                mode: DigestMode::Daily,
                at: Some(self.at.clone()),
                tz: Some(self.tz.clone()),
                every_h: None,
            },
            DigestChoice::Every => {
                let hours = self.hours.parse::<u16>().map_err(|_| PrefsError {
                    field: "digest.every_h",
                    expected: "a whole number of hours from 1 to 168",
                    value: self.hours.chars().take(20).collect(),
                })?;
                DigestPrefs {
                    mode: DigestMode::Every,
                    at: None,
                    tz: None,
                    every_h: Some(hours),
                }
            }
        });
        let quiet = match (self.from.is_empty(), self.to.is_empty()) {
            (true, true) => None,
            (false, false) => Some(QuietPrefs {
                from: self.from.clone(),
                to: self.to.clone(),
                tz: self.tz.clone(),
                high_rings: self.high_rings,
            }),
            (true, false) => {
                return Err(PrefsError {
                    field: "quiet.from",
                    expected: "both times of the quiet hours, or none",
                    value: String::new(),
                })
            }
            (false, true) => {
                return Err(PrefsError {
                    field: "quiet.to",
                    expected: "both times of the quiet hours, or none",
                    value: String::new(),
                })
            }
        };
        let out = Prefs { quiet, digest };
        prefs::check(&out)?;
        Ok(out)
    }
}

/// What the page needs.
pub struct PrefsPage<'a> {
    pub csrf: &'a str,
    pub form: &'a Form,
    /// The names of the time zones, sorted.
    pub zones: &'a [String],
    pub file: &'a DigestConfig,
    /// The account has values of its own.
    pub has_own: bool,
    pub notice: Option<Notice>,
    /// A refusal of the last POST.
    pub error: Option<&'a PrefsError>,
}

/// The default of the file in one sentence.
fn file_words(file: &DigestConfig) -> String {
    match file {
        DigestConfig::Off => "no summary".into(),
        DigestConfig::Daily {
            hour,
            minute,
            timezone,
        } => {
            format!("daily at {hour:02}:{minute:02}, time zone {timezone}")
        }
        DigestConfig::Every(period) => {
            let secs = period.as_secs();
            if secs % 3600 == 0 {
                format!("every {} h", secs / 3600)
            } else {
                format!("every {} min", secs / 60)
            }
        }
    }
}

fn checked(on: bool) -> &'static str {
    if on {
        " checked"
    } else {
        ""
    }
}

pub fn prefs_page(frame: &Frame, p: &PrefsPage) -> String {
    let f = p.form;
    let mut out = String::new();
    notice_html(&mut out, p.notice);
    if let Some(e) = p.error {
        let _ = write!(
            out,
            r#"<div class="uiBanner uiBanner--inset" role="alert"><span class="uiBanner__text">Nothing is saved. {}</span></div>"#,
            html_escape(&e.to_string())
        );
    }
    let mut zones = String::new();
    for name in p.zones {
        let _ = write!(
            zones,
            r#"<option value="{0}"{1}>{0}</option>"#,
            html_escape(name),
            if *name == f.tz { " selected" } else { "" }
        );
    }
    let reset = if p.has_own {
        r#"<button class="button" type="submit" name="reset" value="1" formnovalidate>Use the defaults of the file</button>"#
    } else {
        ""
    };
    let _ = write!(
        out,
        r##"<div class="page page--prefs"><nav class="crumbs" aria-label="Path"><a href="{page}">{back}Inbox</a><span>/</span><b>Preferences</b></nav>
<section class="sec2" aria-labelledby="pf-t"><div class="sec2__head"><h2 class="title" id="pf-t">Inbox preferences</h2><span class="sec2__note">Only you can change these. They apply to all your phones.</span></div>
<form class="form" method="post" action="{action}"><input type="hidden" name="csrf" value="{csrf}">
<div class="form__row"><span class="lbl" id="pf-d">Summary of open items<span class="hint2">One quiet message with the count and the age. It never rings and it never shows a content.</span></span><div class="stack"><div class="radios radios--col" role="radiogroup" aria-labelledby="pf-d">
<label><input type="radio" name="digest" value="daily"{daily}> Daily at <input class="uiInput uiInput--time" type="time" name="at" value="{at}" aria-label="Time" step="60"> time zone <select class="uiInput uiInput--zone" name="tz" aria-label="Time zone">{zones}</select></label>
<label><input type="radio" name="digest" value="every"{every}> Every <input class="uiInput uiInput--hours" type="number" name="hours" value="{hours}" min="1" max="168" step="1" aria-label="Hours"> hours</label>
<label><input type="radio" name="digest" value="off"{off}> None</label></div></div></div>
<div class="form__row"><span class="lbl" id="pf-q">Quiet hours<span class="hint2">A new item arrives with no sound. It stays in "Open items". Empty both times for no quiet hours.</span></span><div class="stack"><div class="inlineFields"><input class="uiInput uiInput--time" type="time" name="from" value="{from}" aria-label="From" step="60"> to <input class="uiInput uiInput--time" type="time" name="to" value="{to}" aria-label="To" step="60"> <span class="muted">in the time zone above</span></div>
<label class="radios"><span><input type="checkbox" name="high" value="1"{high}> Urgent items still ring</span></label></div></div>
<div class="form__row"><span class="lbl">Default from the file<span class="hint2">Your values replace them.</span></span><div class="note note--row">Summary: {file} (<code>inbox.items.digest.*</code>). Quiet hours: none.</div></div>
<div class="decide__row decide__row--form"><span class="grow"></span>{reset}<a class="button" href="{page}">Cancel</a><button class="button button--primary" type="submit">Save</button></div>
</form></section>
<p class="note">The summary never holds a content. A period goes from 1 hour to 7 days. The server refuses a value that is not valid and changes nothing.</p></div>"##,
        page = ui_page(),
        back = icon("arrow-left"),
        action = PREFS,
        csrf = html_escape(p.csrf),
        daily = checked(f.digest == DigestChoice::Daily),
        every = checked(f.digest == DigestChoice::Every),
        off = checked(f.digest == DigestChoice::Off),
        at = html_escape(&f.at),
        hours = html_escape(&f.hours),
        from = html_escape(&f.from),
        to = html_escape(&f.to),
        high = checked(f.high_rings),
        file = html_escape(&file_words(p.file)),
    );
    fill_page(Tab::Inbox, frame, "Inbox: preferences", &out)
}

fn ui_page() -> &'static str {
    super::inbox::PAGE
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fields(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    fn parse(pairs: &[(&str, &str)]) -> Result<Prefs, PrefsError> {
        let f = fields(pairs);
        Form::from_fields(&f).to_prefs(&f)
    }

    #[test]
    fn a_form_becomes_the_preferences_and_the_preferences_become_the_form_again() {
        let p = parse(&[
            ("digest", "daily"),
            ("at", "09:00"),
            ("tz", "Europe/Paris"),
            ("hours", "4"),
            ("from", "22:00"),
            ("to", "07:00"),
            ("high", "1"),
        ])
        .unwrap();
        assert_eq!(p.digest.as_ref().unwrap().at.as_deref(), Some("09:00"));
        assert_eq!(p.quiet.as_ref().unwrap().tz, "Europe/Paris");
        assert!(p.quiet.as_ref().unwrap().high_rings);
        let form = Form::from_prefs(&p, &DigestConfig::Off);
        assert_eq!(
            (form.digest, form.at.as_str(), form.tz.as_str()),
            (DigestChoice::Daily, "09:00", "Europe/Paris")
        );
        assert_eq!(
            (form.from.as_str(), form.to.as_str(), form.high_rings),
            ("22:00", "07:00", true)
        );
    }

    #[test]
    fn a_bad_value_is_the_error_with_its_field_and_nothing_is_changed() {
        let base = [("digest", "daily"), ("at", "09:00"), ("tz", "Europe/Paris")];
        let with = |name: &str, value: &str| {
            let mut v: Vec<(&str, &str)> =
                base.iter().copied().filter(|(k, _)| *k != name).collect();
            v.push((name, value));
            parse(&v).unwrap_err()
        };
        assert_eq!(with("tz", "Mars/Base").field, "tz");
        assert_eq!(with("tz", "europe/paris").value, "europe/paris");
        assert_eq!(with("at", "9:00").field, "digest.at");
        assert_eq!(with("digest", "weekly").field, "digest");
        let e = parse(&[("digest", "every"), ("hours", "0"), ("tz", "UTC")]).unwrap_err();
        assert_eq!(e.field, "digest.every_h");
        let e = parse(&[("digest", "every"), ("hours", "169"), ("tz", "UTC")]).unwrap_err();
        assert_eq!(e.field, "digest.every_h");
        let e = parse(&[("digest", "off"), ("from", "22:00"), ("tz", "UTC")]).unwrap_err();
        assert_eq!(e.field, "quiet.to");
        // A zone that no block uses is checked too.
        let e = parse(&[("digest", "off"), ("tz", "Nowhere/Here")]).unwrap_err();
        assert_eq!(e.field, "tz");
    }

    #[test]
    fn the_default_of_the_file_fills_the_form_of_an_account_that_has_no_value() {
        let file = DigestConfig::Daily {
            hour: 8,
            minute: 30,
            timezone: "Europe/Paris".into(),
        };
        let form = Form::from_prefs(&Prefs::default(), &file);
        assert_eq!(
            (form.digest, form.at.as_str(), form.tz.as_str()),
            (DigestChoice::Daily, "08:30", "Europe/Paris")
        );
        assert!(form.from.is_empty() && form.to.is_empty());
        let form = Form::from_prefs(
            &Prefs::default(),
            &DigestConfig::Every(std::time::Duration::from_secs(6 * 3600)),
        );
        assert_eq!(
            (form.digest, form.hours.as_str(), form.tz.as_str()),
            (DigestChoice::Every, "6", "UTC")
        );
    }
}
