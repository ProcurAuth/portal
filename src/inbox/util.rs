//! Small helpers that the modules of the inbox share.

use std::time::{SystemTime, UNIX_EPOCH};

use inbox_core::rng::Rng;

/// Unix time in milliseconds.
pub fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// 128 random bits as lowercase hex: alert, device, session and login ids.
pub fn random_id() -> String {
    hex(&Rng::os().bytes::<16>())
}

pub fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push(DIGITS[(b >> 4) as usize] as char);
        out.push(DIGITS[(b & 0xf) as usize] as char);
    }
    out
}

pub fn is_hex_id(text: &str) -> bool {
    text.len() == 32
        && text
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// Truncates to at most `max` characters, ending with "…" when cut.
pub fn truncate_chars(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let mut out: String = text.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}

/// Reads a time in RFC 3339 form as Unix milliseconds. A fraction of a second is cut to milliseconds. `None` for a
/// text that is not a time of this form, and for a time before 1970.
pub fn parse_rfc3339_ms(text: &str) -> Option<i64> {
    let bytes = text.as_bytes();
    if !text.is_ascii() || bytes.len() < 20 {
        return None;
    }
    let digits = |from: usize, to: usize| -> Option<i64> {
        let part = text.get(from..to)?;
        part.bytes()
            .all(|b| b.is_ascii_digit())
            .then(|| part.parse().ok())
            .flatten()
    };
    let (year, month, day) = (digits(0, 4)?, digits(5, 7)?, digits(8, 10)?);
    if bytes[4] != b'-' || bytes[7] != b'-' || !matches!(bytes[10], b'T' | b't') {
        return None;
    }
    let (hour, minute, second) = (digits(11, 13)?, digits(14, 16)?, digits(17, 19)?);
    if bytes[13] != b':' || bytes[16] != b':' {
        return None;
    }
    let mut at = 19;
    let mut millis = 0i64;
    if bytes[at] == b'.' {
        at += 1;
        let start = at;
        while at < bytes.len() && bytes[at].is_ascii_digit() {
            at += 1;
        }
        if at == start {
            return None;
        }
        let fraction = &text[start..at];
        let first3: String = fraction.chars().chain("000".chars()).take(3).collect();
        millis = first3.parse().ok()?;
    }
    let offset_seconds = match bytes.get(at)? {
        b'Z' | b'z' if at + 1 == bytes.len() => 0,
        sign @ (b'+' | b'-') if at + 6 == bytes.len() => {
            if bytes[at + 3] != b':' {
                return None;
            }
            let (oh, om) = (digits(at + 1, at + 3)?, digits(at + 4, at + 6)?);
            if oh > 23 || om > 59 {
                return None;
            }
            let total = oh * 3600 + om * 60;
            if *sign == b'+' {
                total
            } else {
                -total
            }
        }
        _ => return None,
    };
    let leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
    let month_days = [
        31,
        if leap { 29 } else { 28 },
        31,
        30,
        31,
        30,
        31,
        31,
        30,
        31,
        30,
        31,
    ];
    if !(1..=12).contains(&month)
        || day < 1
        || day > month_days[(month - 1) as usize]
        || hour > 23
        || minute > 59
        || second > 60
    {
        return None;
    }
    // Days from 1970-01-01 (the civil-from-days algorithm of H. Hinnant, in the other direction).
    let (y, m) = if month <= 2 {
        (year - 1, month + 9)
    } else {
        (year, month - 3)
    };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * m + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    let seconds = days * 86_400 + hour * 3600 + minute * 60 + second.min(59) - offset_seconds;
    (seconds >= 0).then_some(seconds * 1000 + millis)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn times_in_rfc_3339_form_are_read() {
        assert_eq!(parse_rfc3339_ms("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(
            parse_rfc3339_ms("2030-01-01T00:00:00Z"),
            Some(1_893_456_000_000)
        );
        assert_eq!(
            parse_rfc3339_ms("2030-01-01T01:00:00.250+01:00"),
            Some(1_893_456_000_250)
        );
        assert_eq!(
            parse_rfc3339_ms("2028-02-29T12:00:00z"),
            Some(1_835_438_400_000)
        );
        for bad in [
            "",
            "tomorrow",
            "2030-01-01",
            "2030-13-01T00:00:00Z",
            "2029-02-29T00:00:00Z",
            "2030-01-01T00:00:00",
            "2030-01-01T00:00:00+0100",
            "2030-01-01T00:00:00.Z",
            "1969-12-31T23:59:59Z",
        ] {
            assert_eq!(parse_rfc3339_ms(bad), None, "{bad}");
        }
    }

    #[test]
    fn ids_and_truncation() {
        let id = random_id();
        assert!(is_hex_id(&id));
        assert_ne!(id, random_id());
        assert_eq!(truncate_chars("héllo", 3), "hé…");
        assert_eq!(truncate_chars("abc", 10), "abc");
    }
}
