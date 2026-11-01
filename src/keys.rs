//! The text of a key, its digest, its hint and its id (portal 5.1, 4.7).
//!
//! The text of a key is `mcp_`, then 43 random characters from `a-z`, `A-Z` and `0-9`, then 6
//! characters: the CRC-32 (the CRC-32 of zlib) of the 43 characters, in base62. The base62 digits
//! are `0-9`, then `A-Z`, then `a-z`, with the most significant digit first. The text has 53
//! characters. The edge refuses a text with a bad prefix, a bad length, a bad character or a bad
//! checksum before it hashes the text (portal 8.3).
//!
//! The portal shows the text one time. It keeps only the SHA-256 digest and a hint of at most 4 of
//! the 43 random characters. A standalone key of a service has 47 characters, not 53 (portal
//! 5.1b): its length alone makes the edge refuse it.
//!
//! This module has no input or output, except the read of the random generator of the system.

#![allow(dead_code)]

use sha2::{Digest, Sha256};
use zeroize::Zeroizing;

/// The prefix of the text of a key.
pub const PREFIX: &str = "mcp_";
/// The number of random characters.
pub const RANDOM_CHARS: usize = 43;
/// The number of characters of the checksum.
pub const CHECKSUM_CHARS: usize = 6;
/// The length of the text of a key.
pub const TEXT_CHARS: usize = PREFIX.len() + RANDOM_CHARS + CHECKSUM_CHARS;
/// The number of characters of a hint.
pub const HINT_CHARS: usize = 4;
/// The number of characters of the id of a key of the database.
pub const ID_CHARS: usize = 16;
/// The longest name of a key, in characters.
pub const MAX_NAME_CHARS: usize = 64;

const ALPHABET: &[u8; 62] = b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz";
/// The random characters come from `a-z`, `A-Z` and `0-9`: the alphabet of base62.
const RANDOM_LIMIT: u8 = 248; // 4 * 62: a byte from this value up is not used, so each character is uniform.

/// Why a text is not the text of a key.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FormatError {
    /// The text does not start with `mcp_`.
    Prefix,
    /// The text does not have 53 characters.
    Length,
    /// A character is not in `a-z`, `A-Z` or `0-9`.
    Character,
    /// The checksum does not match the 43 random characters.
    Checksum,
}

/// The CRC-32 of zlib (polynomial 0xEDB88320, initial value and final xor 0xFFFFFFFF).
fn crc32(data: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for byte in data {
        crc ^= u32::from(*byte);
        for _ in 0..8 {
            let mask = (crc & 1).wrapping_neg();
            crc = (crc >> 1) ^ (0xEDB8_8320 & mask);
        }
    }
    !crc
}

/// A number in base62 with 6 digits, the most significant digit first. A number below 62^6 fits:
/// the largest CRC-32 is below this value.
fn base62_6(mut value: u32) -> [u8; CHECKSUM_CHARS] {
    let mut out = [b'0'; CHECKSUM_CHARS];
    for digit in out.iter_mut().rev() {
        *digit = ALPHABET[(value % 62) as usize];
        value /= 62;
    }
    out
}

/// The checksum characters of 43 random characters.
fn checksum(random: &[u8]) -> [u8; CHECKSUM_CHARS] {
    base62_6(crc32(random))
}

/// A new text of a key. The 43 random characters come from the random generator of the system.
pub fn generate() -> Result<Zeroizing<String>, String> {
    let mut random = Zeroizing::new([0u8; RANDOM_CHARS]);
    let mut filled = 0;
    while filled < RANDOM_CHARS {
        let mut bytes = Zeroizing::new([0u8; 64]);
        getrandom::fill(&mut bytes[..]).map_err(|e| format!("OS random generator failed: {e}"))?;
        for byte in bytes.iter() {
            if *byte < RANDOM_LIMIT && filled < RANDOM_CHARS {
                random[filled] = ALPHABET[usize::from(*byte % 62)];
                filled += 1;
            }
        }
    }
    let mut text = Zeroizing::new(String::with_capacity(TEXT_CHARS));
    text.push_str(PREFIX);
    text.extend(random.iter().map(|b| char::from(*b)));
    text.extend(checksum(&random[..]).iter().map(|b| char::from(*b)));
    Ok(text)
}

/// Checks the form of a text: the prefix, the length, the characters and the checksum. It does
/// not hash the text.
pub fn check_format(text: &str) -> Result<(), FormatError> {
    let Some(rest) = text.strip_prefix(PREFIX) else {
        return Err(FormatError::Prefix);
    };
    if text.len() != TEXT_CHARS {
        return Err(FormatError::Length);
    }
    if !rest.bytes().all(|b| b.is_ascii_alphanumeric()) {
        return Err(FormatError::Character);
    }
    let (random, given) = rest.as_bytes().split_at(RANDOM_CHARS);
    if checksum(random).as_slice() != given {
        return Err(FormatError::Checksum);
    }
    Ok(())
}

/// The SHA-256 digest of the text of a key. The portal keeps this value and never the text.
pub fn digest(text: &str) -> [u8; 32] {
    Sha256::digest(text.as_bytes()).into()
}

/// The hint of a key: the first 4 of its 43 random characters. A text with a bad form gives an
/// empty hint.
pub fn hint(text: &str) -> String {
    text.strip_prefix(PREFIX)
        .map(|rest| rest.chars().take(HINT_CHARS).collect())
        .unwrap_or_default()
}

/// A new id of a key of the database: 16 characters from `0-9` and `a-f` (portal 4.7).
pub fn new_id() -> Result<String, String> {
    let mut bytes = [0u8; ID_CHARS / 2];
    getrandom::fill(&mut bytes).map_err(|e| format!("OS random generator failed: {e}"))?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}

/// The id of a key of the file: the first 16 hexadecimal characters of the SHA-256 digest of the
/// login, a zero byte and the name (portal 3.4c).
pub fn file_id(login: &str, name: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(login.as_bytes());
    hasher.update([0u8]);
    hasher.update(name.as_bytes());
    hasher.finalize()[..ID_CHARS / 2]
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// The name of a key: 1 to 64 characters, with no control character (portal 4.1).
pub fn check_name(name: &str) -> Result<(), String> {
    let chars = name.chars().count();
    if chars == 0 || chars > MAX_NAME_CHARS || name.chars().any(char::is_control) {
        return Err(format!(
            "the name of a key has 1 to {MAX_NAME_CHARS} characters, with no control character"
        ));
    }
    Ok(())
}

/// The digest of a key as 64 hexadecimal characters, for the configuration (portal 3.4c). Upper
/// and lower case are the same. Any other form is an error.
pub fn parse_digest_hex(text: &str) -> Result<[u8; 32], String> {
    if text.len() != 64 || !text.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err("a digest has 64 hexadecimal characters".into());
    }
    let mut out = [0u8; 32];
    for (index, pair) in text.as_bytes().chunks(2).enumerate() {
        // The characters are ASCII hexadecimal digits: the conversion cannot fail.
        let high = char::from(pair[0]).to_digit(16).unwrap_or(0) as u8;
        let low = char::from(pair[1]).to_digit(16).unwrap_or(0) as u8;
        out[index] = high << 4 | low;
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_checksum_is_the_crc32_of_zlib_in_base62() {
        // The standard check value of CRC-32.
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
        assert_eq!(crc32(b""), 0);
        assert_eq!(&base62_6(0), b"000000");
        assert_eq!(&base62_6(61), b"00000z");
        assert_eq!(&base62_6(62), b"000010");
        assert_eq!(&base62_6(10), b"00000A");
        assert_eq!(&base62_6(36), b"00000a");
        // The largest CRC-32 has 6 digits.
        assert_eq!(base62_6(u32::MAX).len(), 6);
        assert_eq!(&base62_6(u32::MAX), b"4gfFC3");
    }

    #[test]
    fn a_generated_text_has_the_form_of_5_1() {
        let text = generate().unwrap();
        assert_eq!(text.len(), 53);
        assert!(text.starts_with("mcp_"));
        assert_eq!(check_format(&text), Ok(()));
        assert!(text[4..].bytes().all(|b| b.is_ascii_alphanumeric()));
        let other = generate().unwrap();
        assert_ne!(*text, *other, "two texts are never equal");
        // The checksum covers the 43 characters.
        let mut random = text[4..47].to_string();
        assert_eq!(
            text[47..].as_bytes(),
            checksum(random.as_bytes()),
            "the last 6 characters"
        );
        random.replace_range(0..1, if random.starts_with('a') { "b" } else { "a" });
        assert_ne!(
            checksum(random.as_bytes()).as_slice(),
            text[47..].as_bytes()
        );
    }

    #[test]
    fn a_text_with_a_bad_form_is_refused_with_its_cause() {
        let good = generate().unwrap();
        assert_eq!(check_format(&good), Ok(()));
        assert_eq!(check_format(""), Err(FormatError::Prefix));
        assert_eq!(check_format("Bearer x"), Err(FormatError::Prefix));
        assert_eq!(
            check_format(&good.replacen("mcp_", "mcq_", 1)),
            Err(FormatError::Prefix)
        );
        assert_eq!(check_format(&good[..52]), Err(FormatError::Length));
        assert_eq!(
            check_format(&format!("{}a", *good)),
            Err(FormatError::Length)
        );
        // A standalone key has 47 characters (portal 5.1b).
        assert_eq!(check_format(&good[..47]), Err(FormatError::Length));
        let mut bad_character = good.to_string();
        bad_character.replace_range(10..11, "-");
        assert_eq!(check_format(&bad_character), Err(FormatError::Character));
        let mut bad_sum = good.to_string();
        let last = bad_sum.pop().unwrap();
        bad_sum.push(if last == 'a' { 'b' } else { 'a' });
        assert_eq!(check_format(&bad_sum), Err(FormatError::Checksum));
        // A multi-byte character does not pass for a character of the text.
        assert!(check_format(&format!("mcp_{}é", "a".repeat(51))).is_err());
    }

    #[test]
    fn the_digest_the_hint_and_the_ids_follow_the_paragraphs() {
        let text = generate().unwrap();
        assert_eq!(
            digest(&text),
            <[u8; 32]>::from(Sha256::digest(text.as_bytes()))
        );
        assert_eq!(hint(&text), text[4..8]);
        assert_eq!(hint("nothing"), "");
        let id = new_id().unwrap();
        assert_eq!(id.len(), 16);
        assert!(id
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)));
        assert_ne!(id, new_id().unwrap());
        // The example of portal 3.4c.
        assert_eq!(file_id("carol", "nightly report"), "34b1100f45b235ba");
        assert_eq!(
            file_id("carol", "nightly report"),
            file_id("carol", "nightly report")
        );
        assert_ne!(file_id("carol", "a"), file_id("carol", "b"));
        assert_ne!(
            file_id("carol", "a"),
            file_id("caro", "la"),
            "the zero byte separates"
        );
    }

    #[test]
    fn the_name_and_the_hexadecimal_digest_follow_their_rules() {
        assert!(check_name("nightly report").is_ok());
        assert!(check_name(&"é".repeat(64)).is_ok());
        for bad in ["", "a\nb", &"x".repeat(65)] {
            assert!(check_name(bad).is_err(), "{bad:?}");
        }
        let upper = "AB".repeat(32);
        let lower = "ab".repeat(32);
        assert_eq!(parse_digest_hex(&upper), parse_digest_hex(&lower));
        assert_eq!(parse_digest_hex(&lower).unwrap()[0], 0xab);
        for bad in [
            "",
            "ab",
            &"ab".repeat(31),
            &"ab".repeat(33),
            &"zz".repeat(32),
            &" ab".repeat(21),
        ] {
            assert!(parse_digest_hex(bad).is_err(), "{bad:?}");
        }
    }
}
