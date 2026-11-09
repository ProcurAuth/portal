//! The text of a key, its digest, its hint and its id (portal 5.1, 4.7).
//!
//! The text of a key is a prefix, then 43 random characters from `a-z`, `A-Z` and `0-9`, then 6
//! characters: the CRC-32 (the CRC-32 of zlib) of the prefix and the 43 characters, in base62. The
//! base62 digits are `0-9`, then `A-Z`, then `a-z`, with the most significant digit first. The
//! text has 53 characters. The prefix gives the kind of the key: `mcp_` for an agent, verified at
//! the key edge, and `api_` for a service, verified by the API of the inbox. The prefix `whk_` is
//! reserved: it is an unknown prefix. A verifier refuses a text with a bad prefix, a bad length, a
//! bad character or a bad checksum before it hashes the text, and then it refuses a key of the
//! other kind (portal 5.1, 8.3).
//!
//! The portal shows the text one time. It keeps only the SHA-256 digest and a hint of at most 4 of
//! the 43 random characters. A standalone key of a service has 47 characters, not 53 (portal
//! 5.1b): its length alone makes the edge refuse it.
//!
//! This module has no input or output, except the read of the random generator of the system.

#![allow(dead_code)]

use sha2::{Digest, Sha256};
use zeroize::Zeroizing;

/// The number of characters of a prefix: three letters and `_`.
pub const PREFIX_CHARS: usize = 4;
/// The number of random characters.
pub const RANDOM_CHARS: usize = 43;
/// The number of characters of the checksum.
pub const CHECKSUM_CHARS: usize = 6;
/// The length of the text of a key.
pub const TEXT_CHARS: usize = PREFIX_CHARS + RANDOM_CHARS + CHECKSUM_CHARS;

/// The kind of a key (portal 5.1). One key has one kind, and the kind decides who verifies it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum KeyKind {
    /// An agent. The key edge verifies it (`/auth/verify-key`).
    Mcp,
    /// A service, a webhook or a script. The API of the inbox verifies it, in process.
    Api,
}

impl KeyKind {
    /// Every kind, in the order of the pages.
    pub const ALL: [KeyKind; 2] = [KeyKind::Mcp, KeyKind::Api];

    /// The prefix of the text of a key of this kind.
    pub const fn prefix(self) -> &'static str {
        match self {
            KeyKind::Mcp => "mcp_",
            KeyKind::Api => "api_",
        }
    }

    /// The name of the kind: the id of the node `key_kind:<name>` and the value of the column
    /// `kind` of the database.
    pub const fn name(self) -> &'static str {
        match self {
            KeyKind::Mcp => "mcp",
            KeyKind::Api => "api",
        }
    }

    /// The label that the pages show.
    pub const fn label(self) -> &'static str {
        match self {
            KeyKind::Mcp => "MCP",
            KeyKind::Api => "API",
        }
    }

    /// The kind with this name. `None`: no such kind.
    pub fn from_name(name: &str) -> Option<KeyKind> {
        KeyKind::ALL.into_iter().find(|k| k.name() == name)
    }

    /// The kind of a text with this prefix. `whk_` is reserved and gives `None`.
    fn from_text(text: &str) -> Option<KeyKind> {
        KeyKind::ALL
            .into_iter()
            .find(|k| text.starts_with(k.prefix()))
    }
}
/// What a key can do when the portal asks a person (design of the dialogue, 1.6). The owner sets it
/// when the key is made, and the form of the key can change it. The agent never sets it. It is a
/// property of the key, not a grant: no limit and no right depends on it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Interaction {
    /// An agent that can read free text and discuss with the person.
    Conversational,
    /// A system that decides without free text. It gets closed questions and no discussion.
    OneShot,
}

impl Interaction {
    /// Every value, in the order of the pages.
    pub const ALL: [Interaction; 2] = [Interaction::Conversational, Interaction::OneShot];

    /// The default of a key of this kind: `conversational` for an `mcp_` key, `one_shot` for an
    /// `api_` key.
    pub const fn default_for(kind: KeyKind) -> Interaction {
        match kind {
            KeyKind::Mcp => Interaction::Conversational,
            KeyKind::Api => Interaction::OneShot,
        }
    }

    /// The name: the value of the column `interaction`, of the field of the form and of the
    /// attribute of the file.
    pub const fn name(self) -> &'static str {
        match self {
            Interaction::Conversational => "conversational",
            Interaction::OneShot => "one_shot",
        }
    }

    /// The label that the pages show.
    pub const fn label(self) -> &'static str {
        match self {
            Interaction::Conversational => "Conversational",
            Interaction::OneShot => "One shot",
        }
    }

    /// The value with this name. `None`: no such value.
    pub fn from_name(name: &str) -> Option<Interaction> {
        Interaction::ALL.into_iter().find(|i| i.name() == name)
    }
}

/// What the portal does with the agents that share one key (lot G1b, proposal 4.1). The owner sets it when the key is made,
/// and the form of the key can change it. The agent never sets it. It has an effect only when the file has the block
/// `inbox.agents`: without the block, each key acts as `key`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum AgentsMode {
    /// One agent for the key: its bucket. No token, no notice.
    Key,
    /// A call that makes an item and has no token makes an agent, and the result gives its token.
    Discover,
    /// The owner makes the agents. A call that makes an item and has no valid token is refused.
    Enrolled,
}

impl AgentsMode {
    /// Every value, in the order of the pages.
    pub const ALL: [AgentsMode; 3] = [AgentsMode::Key, AgentsMode::Discover, AgentsMode::Enrolled];

    /// The default of a key of this kind: `discover` for an `mcp_` key, `key` for an `api_` key. A script that has one
    /// process stays as it was.
    pub const fn default_for(kind: KeyKind) -> AgentsMode {
        match kind {
            KeyKind::Mcp => AgentsMode::Discover,
            KeyKind::Api => AgentsMode::Key,
        }
    }

    /// The name: the value of the column `agents` and of the field of the form.
    pub const fn name(self) -> &'static str {
        match self {
            AgentsMode::Key => "key",
            AgentsMode::Discover => "discover",
            AgentsMode::Enrolled => "enrolled",
        }
    }

    /// The label that the pages show.
    pub const fn label(self) -> &'static str {
        match self {
            AgentsMode::Key => "One agent",
            AgentsMode::Discover => "Discover",
            AgentsMode::Enrolled => "Enrolled",
        }
    }

    /// The value with this name. `None`: no such value.
    pub fn from_name(name: &str) -> Option<AgentsMode> {
        AgentsMode::ALL.into_iter().find(|m| m.name() == name)
    }
}

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
    /// The text does not start with the prefix of a kind (`mcp_` or `api_`).
    Prefix,
    /// The text does not have 53 characters.
    Length,
    /// A character is not in `a-z`, `A-Z` or `0-9`.
    Character,
    /// The checksum does not match the prefix and the 43 random characters.
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

/// The checksum characters of the prefix and the 43 random characters.
fn checksum(covered: &[u8]) -> [u8; CHECKSUM_CHARS] {
    base62_6(crc32(covered))
}

/// A new text of a key of this kind. The 43 random characters come from the random generator of
/// the system.
pub fn generate(kind: KeyKind) -> Result<Zeroizing<String>, String> {
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
    text.push_str(kind.prefix());
    text.extend(random.iter().map(|b| char::from(*b)));
    let sum = checksum(text.as_bytes());
    text.extend(sum.iter().map(|b| char::from(*b)));
    Ok(text)
}

/// Checks the form of a text: the prefix, the length, the characters and the checksum. It does
/// not hash the text, and it does not look at a database. The answer is the kind of the key. The
/// verifier then checks that the kind is the kind that it serves.
pub fn check_format(text: &str) -> Result<KeyKind, FormatError> {
    let Some(kind) = KeyKind::from_text(text) else {
        return Err(FormatError::Prefix);
    };
    if text.len() != TEXT_CHARS {
        return Err(FormatError::Length);
    }
    if !text.as_bytes()[PREFIX_CHARS..]
        .iter()
        .all(u8::is_ascii_alphanumeric)
    {
        return Err(FormatError::Character);
    }
    let (covered, given) = text.as_bytes().split_at(PREFIX_CHARS + RANDOM_CHARS);
    if checksum(covered).as_slice() != given {
        return Err(FormatError::Checksum);
    }
    Ok(kind)
}

/// The SHA-256 digest of the text of a key. The portal keeps this value and never the text.
pub fn digest(text: &str) -> [u8; 32] {
    Sha256::digest(text.as_bytes()).into()
}

/// The hint of a key: the first 4 of its 43 random characters. A text with a bad form gives an
/// empty hint.
pub fn hint(text: &str) -> String {
    KeyKind::from_text(text)
        .map(|_| text.chars().skip(PREFIX_CHARS).take(HINT_CHARS).collect())
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
    fn a_generated_text_has_the_form_of_5_1_for_each_kind() {
        for kind in KeyKind::ALL {
            let text = generate(kind).unwrap();
            assert_eq!(text.len(), 53);
            assert!(text.starts_with(kind.prefix()));
            assert_eq!(check_format(&text), Ok(kind));
            assert!(text[4..].bytes().all(|b| b.is_ascii_alphanumeric()));
            let other = generate(kind).unwrap();
            assert_ne!(*text, *other, "two texts are never equal");
            // The checksum covers the prefix and the 43 characters.
            assert_eq!(
                text[47..].as_bytes(),
                checksum(text[..47].as_bytes()),
                "the last 6 characters"
            );
            let mut covered = text[..47].to_string();
            covered.replace_range(10..11, if &covered[10..11] == "a" { "b" } else { "a" });
            assert_ne!(
                checksum(covered.as_bytes()).as_slice(),
                text[47..].as_bytes()
            );
        }
        assert_ne!(KeyKind::Mcp.prefix(), KeyKind::Api.prefix());
    }

    #[test]
    fn a_changed_prefix_fails_the_checksum() {
        let api = generate(KeyKind::Api).unwrap();
        let as_mcp = api.replacen("api_", "mcp_", 1);
        assert_eq!(check_format(&as_mcp), Err(FormatError::Checksum));
        let mcp = generate(KeyKind::Mcp).unwrap();
        assert_eq!(
            check_format(&mcp.replacen("mcp_", "api_", 1)),
            Err(FormatError::Checksum)
        );
    }

    #[test]
    fn the_reserved_prefix_and_an_unknown_prefix_are_unknown() {
        let good = generate(KeyKind::Mcp).unwrap();
        for prefix in ["whk_", "mcq_", "API_", "key_"] {
            let text = good.replacen("mcp_", prefix, 1);
            assert_eq!(check_format(&text), Err(FormatError::Prefix), "{prefix}");
        }
        assert_eq!(KeyKind::from_name("whk"), None);
        assert_eq!(KeyKind::from_name("api"), Some(KeyKind::Api));
        assert_eq!(KeyKind::from_name("mcp"), Some(KeyKind::Mcp));
    }

    #[test]
    fn a_text_with_a_bad_form_is_refused_with_its_cause() {
        let good = generate(KeyKind::Mcp).unwrap();
        assert_eq!(check_format(&good), Ok(KeyKind::Mcp));
        assert_eq!(check_format(""), Err(FormatError::Prefix));
        assert_eq!(check_format("Bearer x"), Err(FormatError::Prefix));
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
    fn the_mode_of_the_agents_has_one_name_each_and_a_default_by_kind() {
        for mode in AgentsMode::ALL {
            assert_eq!(AgentsMode::from_name(mode.name()), Some(mode));
        }
        assert_eq!(
            AgentsMode::ALL.map(AgentsMode::name),
            ["key", "discover", "enrolled"]
        );
        assert_eq!(AgentsMode::from_name("Key"), None);
        assert_eq!(AgentsMode::from_name("auto"), None);
        assert_eq!(AgentsMode::from_name(""), None);
        assert_eq!(AgentsMode::default_for(KeyKind::Mcp), AgentsMode::Discover);
        assert_eq!(AgentsMode::default_for(KeyKind::Api), AgentsMode::Key);
    }

    #[test]
    fn the_digest_the_hint_and_the_ids_follow_the_paragraphs() {
        let text = generate(KeyKind::Api).unwrap();
        assert_eq!(
            digest(&text),
            <[u8; 32]>::from(Sha256::digest(text.as_bytes()))
        );
        assert_eq!(hint(&text), text[4..8]);
        assert_eq!(hint("whk_abcdefgh"), "");
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

    #[test]
    fn key_interaction_default_by_kind() {
        assert_eq!(
            Interaction::default_for(KeyKind::Mcp),
            Interaction::Conversational
        );
        assert_eq!(Interaction::default_for(KeyKind::Api), Interaction::OneShot);
        // The names are the values of the column, of the field of the form and of the file.
        for each in Interaction::ALL {
            assert_eq!(Interaction::from_name(each.name()), Some(each));
        }
        assert_eq!(
            Interaction::from_name("conversational"),
            Some(Interaction::Conversational)
        );
        assert_eq!(
            Interaction::from_name("one_shot"),
            Some(Interaction::OneShot)
        );
        for bad in ["", "oneshot", "One_Shot", "interactive", " one_shot"] {
            assert_eq!(Interaction::from_name(bad), None, "{bad:?}");
        }
    }
}
