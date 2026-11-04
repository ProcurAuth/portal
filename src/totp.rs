//! Time-based one-time passwords (RFC 6238) and the recovery codes of the second factor.
//!
//! The parameters are the ones that every authenticator app accepts: HMAC-SHA-1, 6 digits and a step of
//! 30 seconds. HMAC-SHA-1 is safe for this use: the attack on SHA-1 is a collision attack, and an HMAC does not
//! depend on the collision resistance of its hash. The portal uses HMAC-SHA-1 for TOTP only. The HMAC comes from
//! `aws-lc-rs`, which the portal already has. This module adds no dependency.

use aws_lc_rs::hmac;
use zeroize::Zeroize;

use crate::util::{constant_time_eq, percent_encode_query_component};

/// The number of digits of a code.
pub const DIGITS: u32 = 6;
/// The time of one step, in seconds.
pub const STEP_SECS: i64 = 30;
/// The number of steps before and after the current step that the portal accepts. A clock that is
/// 30 seconds early or late still works.
pub const WINDOW: i64 = 1;
/// The size of a secret, in bytes. RFC 4226 asks for 160 bits.
pub const SECRET_BYTES: usize = 20;
/// The number of recovery codes that the portal gives at each enrolment.
pub const RECOVERY_CODES: usize = 10;

const BASE32: &[u8; 32] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ234567";

/// The base32 text of `bytes` (RFC 4648), without padding.
pub fn base32_encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity((bytes.len() * 8).div_ceil(5));
    let mut buffer = 0u32;
    let mut bits = 0u32;
    for &byte in bytes {
        buffer = (buffer << 8) | u32::from(byte);
        bits += 8;
        while bits >= 5 {
            bits -= 5;
            out.push(char::from(BASE32[((buffer >> bits) & 31) as usize]));
        }
    }
    if bits > 0 {
        out.push(char::from(BASE32[((buffer << (5 - bits)) & 31) as usize]));
    }
    out
}

/// The bytes of a base32 text without padding. Only the tests read a secret back: the program
/// keeps the bytes, and shows the text.
#[cfg(test)]
pub fn base32_decode(text: &str) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    let mut buffer = 0u32;
    let mut bits = 0u32;
    for c in text.bytes() {
        let value = BASE32.iter().position(|&b| b == c)? as u32;
        buffer = (buffer << 5) | value;
        bits += 5;
        if bits >= 8 {
            bits -= 8;
            out.push(((buffer >> bits) & 0xff) as u8);
        }
    }
    Some(out)
}

/// A new secret of 160 bits from the generator of the operating system.
pub fn new_secret() -> Result<[u8; SECRET_BYTES], String> {
    let mut secret = [0u8; SECRET_BYTES];
    crate::crypto::random_bytes(&mut secret)?;
    Ok(secret)
}

/// The HOTP value of RFC 4226 for `counter`, with `digits` digits.
fn hotp(secret: &[u8], counter: u64, digits: u32) -> u32 {
    let key = hmac::Key::new(hmac::HMAC_SHA1_FOR_LEGACY_USE_ONLY, secret);
    let tag = hmac::sign(&key, &counter.to_be_bytes());
    let mac = tag.as_ref();
    let offset = usize::from(mac[mac.len() - 1] & 0x0f);
    let binary = u32::from_be_bytes([
        mac[offset] & 0x7f,
        mac[offset + 1],
        mac[offset + 2],
        mac[offset + 3],
    ]);
    binary % 10u32.pow(digits)
}

/// The text of a code: `digits` digits, with leading zeros.
fn format_code(value: u32, digits: u32) -> String {
    format!("{value:0width$}", width = digits as usize)
}

/// The time step of a Unix time.
pub fn step_of(unix: i64) -> i64 {
    unix.div_euclid(STEP_SECS)
}

/// The code of a given time step, with `digits` digits.
pub fn code_for_step(secret: &[u8], step: i64, digits: u32) -> String {
    format_code(hotp(secret, step.max(0) as u64, digits), digits)
}

/// The code that an authenticator app shows at `unix`.
#[cfg(test)]
pub fn code_at(secret: &[u8], unix: i64) -> String {
    code_for_step(secret, step_of(unix), DIGITS)
}

/// The digits of a typed code, or `None` when the text is not a code of [`DIGITS`] digits. A person can type
/// a space or a dash in the middle of the code: the function removes them.
pub fn normalize_code(typed: &str) -> Option<String> {
    let digits: String = typed
        .chars()
        .filter(|c| !c.is_whitespace() && *c != '-')
        .collect();
    (digits.len() == DIGITS as usize && digits.bytes().all(|b| b.is_ascii_digit())).then_some(digits)
}

/// Checks a typed code at `now` against the steps `now - WINDOW` to `now + WINDOW`.
///
/// It returns the step of the code. It returns `None` when no step matches, and when the matching step is not
/// after `last_step`: a code is good once. The caller records the step with the same rule in the database.
/// The comparison of each candidate takes the same time whatever the code.
pub fn verify(secret: &[u8], typed: &str, now: i64, last_step: i64) -> Option<i64> {
    let code = normalize_code(typed)?;
    let current = step_of(now);
    let mut found = None;
    for step in (current - WINDOW)..=(current + WINDOW) {
        let expected = code_for_step(secret, step, DIGITS);
        if constant_time_eq(expected.as_bytes(), code.as_bytes()) && step > last_step {
            found = found.or(Some(step));
        }
    }
    found
}

/// The `otpauth://` link that an authenticator app reads. `issuer` is the name of the server and `account` is
/// the login.
pub fn otpauth_uri(issuer: &str, account: &str, secret: &[u8]) -> String {
    let issuer = percent_encode_query_component(issuer);
    format!(
        "otpauth://totp/{issuer}:{}?secret={}&issuer={issuer}&algorithm=SHA1&digits={DIGITS}&period={STEP_SECS}",
        percent_encode_query_component(account),
        base32_encode(secret)
    )
}

/// The secret in groups of four characters, for a person who types it.
pub fn grouped(secret: &[u8]) -> String {
    let text = base32_encode(secret);
    text.as_bytes()
        .chunks(4)
        .map(|chunk| String::from_utf8_lossy(chunk).into_owned())
        .collect::<Vec<_>>()
        .join(" ")
}

/// A recovery code: two groups of five characters of the base32 alphabet, in lower case (`abcde-fghij`).
/// It has 50 bits.
pub fn new_recovery_code() -> Result<String, String> {
    let mut raw = [0u8; 10];
    crate::crypto::random_bytes(&mut raw)?;
    let text: String = raw
        .iter()
        .map(|byte| char::from(BASE32[usize::from(byte & 31)]).to_ascii_lowercase())
        .collect();
    raw.zeroize();
    Ok(format!("{}-{}", &text[..5], &text[5..]))
}

/// The recovery codes of one enrolment.
pub fn new_recovery_codes() -> Result<Vec<String>, String> {
    (0..RECOVERY_CODES).map(|_| new_recovery_code()).collect()
}

/// The canonical form of a typed recovery code: ten characters of the alphabet, in lower case, without the
/// dash. `None` when the text is not a recovery code. A person can type a capital letter, a space or no dash.
pub fn normalize_recovery(typed: &str) -> Option<String> {
    let text: String = typed
        .chars()
        .filter(|c| !c.is_whitespace() && *c != '-')
        .map(|c| c.to_ascii_lowercase())
        .collect();
    let valid = text.len() == 10
        && text
            .bytes()
            .all(|b| b.is_ascii_lowercase() || (b'2'..=b'7').contains(&b));
    valid.then_some(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The key of the test vectors of RFC 4226 and RFC 6238: the ASCII text `12345678901234567890`.
    const RFC_SECRET: &[u8] = b"12345678901234567890";

    #[test]
    fn base32_gives_the_vectors_of_rfc_4648() {
        for (plain, coded) in [
            ("", ""),
            ("f", "MY"),
            ("fo", "MZXQ"),
            ("foo", "MZXW6"),
            ("foob", "MZXW6YQ"),
            ("fooba", "MZXW6YTB"),
            ("foobar", "MZXW6YTBOI"),
        ] {
            assert_eq!(base32_encode(plain.as_bytes()), coded);
            assert_eq!(base32_decode(coded).unwrap(), plain.as_bytes());
        }
        assert!(base32_decode("MZXW6=").is_none(), "no padding");
        assert!(base32_decode("mzxw6").is_none(), "upper case only");
    }

    #[test]
    fn hotp_gives_the_vectors_of_rfc_4226_appendix_d() {
        let expected = [
            755224, 287082, 359152, 969429, 338314, 254676, 287922, 162583, 399871, 520489,
        ];
        for (counter, value) in expected.iter().enumerate() {
            assert_eq!(hotp(RFC_SECRET, counter as u64, 6), *value, "{counter}");
        }
    }

    #[test]
    fn totp_gives_the_vectors_of_rfc_6238_appendix_b_with_8_digits_and_6_digits() {
        for (time, eight) in [
            (59i64, "94287082"),
            (1_111_111_109, "07081804"),
            (1_111_111_111, "14050471"),
            (1_234_567_890, "89005924"),
            (2_000_000_000, "69279037"),
            (20_000_000_000, "65353130"),
        ] {
            assert_eq!(
                code_for_step(RFC_SECRET, step_of(time), 8),
                eight,
                "8 digits at {time}"
            );
            // The code of 6 digits is the last 6 digits of the code of 8 digits: the truncation of
            // RFC 4226 is the same, and only the modulus changes.
            assert_eq!(code_at(RFC_SECRET, time), eight[2..], "6 digits at {time}");
        }
    }

    #[test]
    fn a_code_is_good_inside_the_window_and_only_once() {
        let now = 1_111_111_109;
        let step = step_of(now);
        let here = code_for_step(RFC_SECRET, step, DIGITS);
        let before = code_for_step(RFC_SECRET, step - 1, DIGITS);
        let after = code_for_step(RFC_SECRET, step + 1, DIGITS);
        let far = code_for_step(RFC_SECRET, step + 2, DIGITS);
        assert_eq!(verify(RFC_SECRET, &here, now, 0), Some(step));
        assert_eq!(verify(RFC_SECRET, &before, now, 0), Some(step - 1));
        assert_eq!(verify(RFC_SECRET, &after, now, 0), Some(step + 1));
        assert_eq!(verify(RFC_SECRET, &far, now, 0), None, "two steps ahead");
        let late = code_for_step(RFC_SECRET, step - 2, DIGITS);
        assert_eq!(verify(RFC_SECRET, &late, now, 0), None, "two steps behind");
        // The step that was accepted is not accepted again, and no older step is accepted.
        assert_eq!(verify(RFC_SECRET, &here, now, step), None);
        assert_eq!(verify(RFC_SECRET, &before, now, step), None);
        assert_eq!(verify(RFC_SECRET, &after, now, step), Some(step + 1));
        assert_eq!(
            verify(RFC_SECRET, "000000x", now, 0),
            None,
            "not a code"
        );
    }

    #[test]
    fn a_typed_code_can_have_a_space_or_a_dash() {
        assert_eq!(normalize_code("123 456").as_deref(), Some("123456"));
        assert_eq!(normalize_code(" 123-456 ").as_deref(), Some("123456"));
        for bad in ["", "12345", "1234567", "12345a", "abcdef"] {
            assert_eq!(normalize_code(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn the_link_and_the_groups_hold_the_secret_in_base32() {
        let uri = otpauth_uri("Portal", "alice", RFC_SECRET);
        assert_eq!(
            uri,
            "otpauth://totp/Portal:alice?secret=GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ&issuer=Portal&algorithm=SHA1&digits=6&period=30"
        );
        assert_eq!(
            grouped(RFC_SECRET),
            "GEZD GNBV GY3T QOJQ GEZD GNBV GY3T QOJQ"
        );
        // A name with a space or a colon cannot break the link.
        let odd = otpauth_uri("My Server", "a:b", RFC_SECRET);
        assert!(odd.starts_with("otpauth://totp/My%20Server:a%3Ab?"), "{odd}");
    }

    #[test]
    fn a_recovery_code_has_two_groups_of_five_and_the_typed_forms_agree() {
        let codes = new_recovery_codes().unwrap();
        assert_eq!(codes.len(), RECOVERY_CODES);
        for code in &codes {
            assert_eq!(code.len(), 11, "{code}");
            assert_eq!(code.as_bytes()[5], b'-');
            let canonical = normalize_recovery(code).expect("a code reads back");
            assert_eq!(canonical.len(), 10);
            assert_eq!(normalize_recovery(&code.to_uppercase()), Some(canonical.clone()));
            assert_eq!(normalize_recovery(&code.replace('-', " ")), Some(canonical));
        }
        let distinct: std::collections::HashSet<&String> = codes.iter().collect();
        assert_eq!(distinct.len(), codes.len());
        for bad in ["", "abcde-fghi", "abcde-fghij1", "abcde-fghi1", "abcde-fghi8"] {
            assert_eq!(normalize_recovery(bad), None, "{bad:?}");
        }
    }
}
