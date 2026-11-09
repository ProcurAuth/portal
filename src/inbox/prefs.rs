//! The preferences of an account: the time zones, the quiet hours and the schedule of the digest.
//!
//! Every function here is pure. The time comes as an argument, so a test moves it by hand. The time zone database is
//! the one that `jiff` embeds in the binary (feature `tzdb-bundle-always`). The server never reads the zone files of the
//! host. The crate is for the server only: `inbox-core` and the phone do not use it.
//!
//! # What the account can set
//!
//! An account sets the digest (`off`, `daily` at `HH:MM` in a time zone, or `every` N hours) and the quiet hours (from,
//! to, time zone, and whether an item of urgency `high` still rings). A block that the account sets replaces the block
//! of the file. A block that the account does not set keeps the default of the file. No value of an account goes past a
//! limit of the file: [`check`] refuses a period outside the range of the key `inbox.items.digest.every` (1 hour to 7
//! days), and a quiet window never holds an item back (the server only sets the flag `quiet`).
//!
//! No value is changed to make it fit. A bad value gets an error that names the field, the expected form and the value.

use inbox_core::proto::{DigestMode, Prefs, QuietPrefs};
use jiff::civil::Time;
use jiff::tz::TimeZone;
use jiff::Timestamp;

use crate::config::DigestConfig;

/// The shortest period of the digest of an account, in hours. It is the minimum of the key `inbox.items.digest.every`.
pub const DIGEST_EVERY_MIN_H: u16 = 1;
/// The longest period of the digest of an account, in hours. It is the maximum of the key `inbox.items.digest.every`.
pub const DIGEST_EVERY_MAX_H: u16 = 168;
/// A due time that is older than this is not sent: the server was away. The next due time comes.
pub const DIGEST_GRACE_MS: i64 = 15 * 60 * 1000;

const HOUR_MS: i64 = 3_600_000;

/// A value of a preference that the server refuses. Nothing was stored and nothing was changed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrefsError {
    /// The name of the field, such as `digest.tz`.
    pub field: &'static str,
    /// What the field accepts.
    pub expected: &'static str,
    /// The value that came.
    pub value: String,
}

impl std::fmt::Display for PrefsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}: expected {}, got {:?}",
            self.field, self.expected, self.value
        )
    }
}

fn refuse<T>(field: &'static str, expected: &'static str, value: &str) -> Result<T, PrefsError> {
    Err(PrefsError {
        field,
        expected,
        value: value.chars().take(80).collect(),
    })
}

const EXPECT_TIME: &str = "a time of the day as HH:MM, from 00:00 to 23:59";
const EXPECT_ZONE: &str =
    "the name of an IANA time zone with its exact capital letters, such as Europe/Paris or UTC";

/// The zone of an IANA name. `None` when the database has no zone with exactly this name: the server does not change
/// the case of a name and does not guess.
pub fn zone(name: &str) -> Option<TimeZone> {
    let found = TimeZone::get(name).ok()?;
    (found.iana_name() == Some(name)).then_some(found)
}

/// Whether the name is a zone of the database.
pub fn is_zone(name: &str) -> bool {
    zone(name).is_some()
}

/// The names of all the zones of the database, sorted. The page of the preferences lists them.
pub fn zone_names() -> Vec<String> {
    let mut names: Vec<String> = jiff::tz::db()
        .available()
        .map(|n| n.as_str().to_string())
        .filter(|n| is_zone(n))
        .collect();
    names.sort();
    names
}

/// The minutes after midnight of `HH:MM`.
pub fn minutes_of(text: &str) -> Option<u16> {
    let b = text.as_bytes();
    let ok = b.len() == 5 && b[2] == b':' && b[..2].iter().chain(&b[3..]).all(u8::is_ascii_digit);
    if !ok {
        return None;
    }
    let (h, m) = (
        u16::from(b[0] - b'0') * 10 + u16::from(b[1] - b'0'),
        u16::from(b[3] - b'0') * 10 + u16::from(b[4] - b'0'),
    );
    (h < 24 && m < 60).then_some(h * 60 + m)
}

/// Checks the preferences against the rules above. The first bad field is the error.
pub fn check(prefs: &Prefs) -> Result<(), PrefsError> {
    if let Some(q) = &prefs.quiet {
        if minutes_of(&q.from).is_none() {
            return refuse("quiet.from", EXPECT_TIME, &q.from);
        }
        if minutes_of(&q.to).is_none() {
            return refuse("quiet.to", EXPECT_TIME, &q.to);
        }
        if q.from == q.to {
            return refuse("quiet.to", "a time that differs from quiet.from", &q.to);
        }
        if !is_zone(&q.tz) {
            return refuse("quiet.tz", EXPECT_ZONE, &q.tz);
        }
    }
    if let Some(d) = &prefs.digest {
        match (d.mode, &d.at, &d.tz, d.every_h) {
            (DigestMode::Off, None, None, None) => {}
            (DigestMode::Daily, Some(at), Some(tz), None) => {
                if minutes_of(at).is_none() {
                    return refuse("digest.at", EXPECT_TIME, at);
                }
                if !is_zone(tz) {
                    return refuse("digest.tz", EXPECT_ZONE, tz);
                }
            }
            (DigestMode::Every, None, None, Some(h)) => {
                if !(DIGEST_EVERY_MIN_H..=DIGEST_EVERY_MAX_H).contains(&h) {
                    return refuse(
                        "digest.every_h",
                        "a whole number of hours from 1 to 168 (the range of inbox.items.digest.every)",
                        &h.to_string(),
                    );
                }
            }
            _ => {
                return refuse(
                    "digest",
                    "off alone, daily with at and tz, or every with every_h",
                    &format!("{:?}", d.mode),
                )
            }
        }
    }
    Ok(())
}

// ---------------------------------------------------------------- quiet hours

/// Whether `at_ms` (Unix time) falls inside the quiet hours: `from` included, `to` excluded, in the zone of the window.
/// The window can cross midnight. A zone that the database does not know gives `false`: the server never holds an item
/// back and never guesses.
pub fn in_quiet(q: &QuietPrefs, at_ms: i64) -> bool {
    let (Some(from), Some(to), Some(tz), Ok(at)) = (
        minutes_of(&q.from),
        minutes_of(&q.to),
        zone(&q.tz),
        Timestamp::from_millisecond(at_ms),
    ) else {
        return false;
    };
    let local = at.to_zoned(tz);
    let now =
        u16::try_from(local.hour()).unwrap_or(0) * 60 + u16::try_from(local.minute()).unwrap_or(0);
    if from < to {
        from <= now && now < to
    } else {
        now >= from || now < to
    }
}

// ---------------------------------------------------------------- digest

/// The schedule that applies to an account.
#[derive(Debug, Clone)]
pub enum Plan {
    Off,
    /// One digest each day at `minutes` after midnight of the zone.
    Daily {
        minutes: u16,
        tz: TimeZone,
    },
    /// One digest each `period_ms`. The due times are the multiples of the period since the Unix epoch.
    Every {
        period_ms: i64,
    },
}

/// The schedule of an account: the block of the account when it has one, else the default of the file. `Err` names a
/// zone that the database does not know. The start already checks the zone of the file, and [`check`] the zone of an
/// account, so this is a row that someone changed by hand.
pub fn plan(prefs: &Prefs, file: &DigestConfig) -> Result<Plan, String> {
    let unknown = |tz: &str| format!("the time zone {tz:?} is not in the database");
    if let Some(d) = &prefs.digest {
        return match (d.mode, &d.at, &d.tz, d.every_h) {
            (DigestMode::Off, ..) => Ok(Plan::Off),
            (DigestMode::Daily, Some(at), Some(tz), _) => Ok(Plan::Daily {
                minutes: minutes_of(at).ok_or_else(|| format!("the time {at:?} is not HH:MM"))?,
                tz: zone(tz).ok_or_else(|| unknown(tz))?,
            }),
            (DigestMode::Every, _, _, Some(h)) if h >= DIGEST_EVERY_MIN_H => Ok(Plan::Every {
                period_ms: i64::from(h) * HOUR_MS,
            }),
            _ => Err("the digest of the account is not valid".into()),
        };
    }
    Ok(match file {
        DigestConfig::Off => Plan::Off,
        DigestConfig::Daily {
            hour,
            minute,
            timezone,
        } => Plan::Daily {
            minutes: u16::from(*hour) * 60 + u16::from(*minute),
            tz: zone(timezone).ok_or_else(|| unknown(timezone))?,
        },
        DigestConfig::Every(period) => Plan::Every {
            period_ms: i64::try_from(period.as_millis())
                .unwrap_or(i64::MAX)
                .max(HOUR_MS),
        },
    })
}

/// The newest due time that is not after `now_ms`, and not older than [`DIGEST_GRACE_MS`]. `None`: nothing is due.
///
/// A daily digest is due at `HH:MM` of the local day. A time that a change of the clock skips (the hour of the spring
/// change) is due at the first time after the gap, and a time that happens twice (the autumn change) is due the first
/// time. So there is one due time for each local day.
pub fn due_slot(plan: &Plan, now_ms: i64) -> Option<i64> {
    let slot = match plan {
        Plan::Off => return None,
        Plan::Every { period_ms } => now_ms - now_ms.rem_euclid(*period_ms),
        Plan::Daily { minutes, tz } => {
            let now = Timestamp::from_millisecond(now_ms).ok()?;
            let time = Time::new(
                i8::try_from(minutes / 60).ok()?,
                i8::try_from(minutes % 60).ok()?,
                0,
                0,
            )
            .ok()?;
            let today = now.to_zoned(tz.clone()).date();
            let at = |date: jiff::civil::Date| date.to_datetime(time).to_zoned(tz.clone()).ok();
            let first = at(today)?;
            let slot = if first.timestamp() <= now {
                first
            } else {
                at(today.yesterday().ok()?)?
            };
            slot.timestamp().as_millisecond()
        }
    };
    (slot <= now_ms && now_ms - slot <= DIGEST_GRACE_MS).then_some(slot)
}

/// The key that makes one digest for each owner and due time, whatever the number of instances.
pub fn digest_key(login: &str, due_ms: i64) -> String {
    format!("digest:{login}:{due_ms}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use inbox_core::proto::DigestPrefs;

    fn ms(text: &str) -> i64 {
        text.parse::<Timestamp>().unwrap().as_millisecond()
    }

    fn paris(at: &str) -> Plan {
        Plan::Daily {
            minutes: minutes_of(at).unwrap(),
            tz: zone("Europe/Paris").unwrap(),
        }
    }

    #[test]
    fn zones_come_from_the_embedded_database_with_their_exact_name() {
        assert!(
            is_zone("Europe/Paris") && is_zone("UTC") && is_zone("America/Argentina/Buenos_Aires")
        );
        assert!(!is_zone("europe/paris"), "no change of the case");
        assert!(!is_zone("Mars/Base") && !is_zone("") && !is_zone("Europe/Pariss"));
        assert!(zone_names().len() > 400);
    }

    #[test]
    fn a_daily_due_time_follows_the_local_clock_across_the_spring_change() {
        // Paris changes to summer time on 2026-03-29 at 02:00: 09:00 is 08:00 UTC before and 07:00 UTC after.
        let plan = paris("09:00");
        assert_eq!(
            due_slot(&plan, ms("2026-03-28T08:00:00Z")),
            Some(ms("2026-03-28T08:00:00Z"))
        );
        assert_eq!(
            due_slot(&plan, ms("2026-03-29T07:00:00Z")),
            Some(ms("2026-03-29T07:00:00Z"))
        );
        assert_eq!(
            due_slot(&plan, ms("2026-03-29T08:00:00Z")),
            None,
            "09:00 UTC+2 is past"
        );
        assert_eq!(due_slot(&plan, ms("2026-03-29T06:59:59Z")), None);
    }

    #[test]
    fn a_time_inside_the_gap_is_due_after_the_gap_and_a_time_that_repeats_is_due_once() {
        let gap = paris("02:30");
        // 02:30 does not exist on 2026-03-29: the due time is 03:30 local, 01:30 UTC.
        assert_eq!(
            due_slot(&gap, ms("2026-03-29T01:30:00Z")),
            Some(ms("2026-03-29T01:30:00Z"))
        );
        // 02:30 happens twice on 2026-10-25: the first one, 00:30 UTC, is the due time. The second is not.
        assert_eq!(
            due_slot(&gap, ms("2026-10-25T00:30:00Z")),
            Some(ms("2026-10-25T00:30:00Z"))
        );
        assert_eq!(due_slot(&gap, ms("2026-10-25T01:30:00Z")), None);
    }

    #[test]
    fn a_due_time_that_is_too_old_is_not_sent() {
        let plan = paris("09:00");
        let due = ms("2026-06-10T07:00:00Z");
        assert_eq!(due_slot(&plan, due + DIGEST_GRACE_MS), Some(due));
        assert_eq!(due_slot(&plan, due + DIGEST_GRACE_MS + 1), None);
    }

    #[test]
    fn every_n_hours_counts_from_the_epoch() {
        let plan = Plan::Every {
            period_ms: 4 * HOUR_MS,
        };
        assert_eq!(
            due_slot(&plan, ms("2026-06-10T08:05:00Z")),
            Some(ms("2026-06-10T08:00:00Z"))
        );
        assert_eq!(due_slot(&plan, ms("2026-06-10T11:59:00Z")), None);
    }

    #[test]
    fn the_quiet_window_can_cross_midnight_and_follows_the_zone() {
        let q = QuietPrefs {
            from: "22:00".into(),
            to: "07:00".into(),
            tz: "Europe/Paris".into(),
            high_rings: false,
        };
        // 2026-06-10 is summer time in Paris: 20:00 UTC is 22:00 local.
        assert!(in_quiet(&q, ms("2026-06-10T20:00:00Z")));
        assert!(in_quiet(&q, ms("2026-06-10T04:59:00Z")));
        assert!(
            !in_quiet(&q, ms("2026-06-10T05:00:00Z")),
            "07:00 is outside"
        );
        assert!(!in_quiet(&q, ms("2026-06-10T19:59:00Z")));
        let day = QuietPrefs {
            from: "12:00".into(),
            to: "13:00".into(),
            tz: "UTC".into(),
            high_rings: true,
        };
        assert!(
            in_quiet(&day, ms("2026-06-10T12:30:00Z"))
                && !in_quiet(&day, ms("2026-06-10T13:00:00Z"))
        );
    }

    #[test]
    fn check_refuses_each_bad_value_and_names_the_field() {
        let quiet = |from: &str, to: &str, tz: &str| Prefs {
            quiet: Some(QuietPrefs {
                from: from.into(),
                to: to.into(),
                tz: tz.into(),
                high_rings: false,
            }),
            digest: None,
        };
        assert!(check(&quiet("22:00", "07:00", "Europe/Paris")).is_ok());
        assert_eq!(
            check(&quiet("24:00", "07:00", "UTC")).unwrap_err().field,
            "quiet.from"
        );
        assert_eq!(
            check(&quiet("22:00", "22:00", "UTC")).unwrap_err().field,
            "quiet.to"
        );
        let error = check(&quiet("22:00", "07:00", "paris")).unwrap_err();
        assert_eq!((error.field, error.value.as_str()), ("quiet.tz", "paris"));
        let every = |h: u16| Prefs {
            quiet: None,
            digest: Some(DigestPrefs {
                mode: DigestMode::Every,
                at: None,
                tz: None,
                every_h: Some(h),
            }),
        };
        assert!(check(&every(1)).is_ok() && check(&every(168)).is_ok());
        assert_eq!(check(&every(0)).unwrap_err().field, "digest.every_h");
        assert_eq!(check(&every(169)).unwrap_err().field, "digest.every_h");
    }

    #[test]
    fn the_cap_of_the_account_is_the_range_of_the_key_of_the_file() {
        use crate::config::keys::KEYS;
        let key = KEYS
            .iter()
            .find(|k| k.block == "inbox.items.digest" && k.name == "every")
            .expect("the key digest.every");
        match key.kind {
            crate::config::keys::Kind::Duration { min, max } => {
                assert_eq!(min.as_secs(), u64::from(DIGEST_EVERY_MIN_H) * 3600);
                assert_eq!(max.as_secs(), u64::from(DIGEST_EVERY_MAX_H) * 3600);
            }
            _ => panic!("the key is a duration"),
        }
    }
}
