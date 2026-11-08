//! The seal of the contents of the items at rest.
//!
//! The portal receives the text of an approval or of a question, seals it with a key of the server, and
//! writes only the sealed bytes. It opens the bytes in memory to serve the owner. A copy of the database,
//! a backup or a replica holds no clear text. This is not end-to-end encryption: the portal reads all.
//!
//! The keys come from the blocks `inbox.at_rest.key`. Each key has an id. The key `active_key` seals the new
//! contents. All the keys open. A rotation adds a key, changes `active_key`, and removes the old key when no
//! row names it any more.
//!
//! The module generalises `crate::secretbox`. A key is 32 random bytes, written as base64. The module derives
//! one key for each purpose from it with HMAC-SHA-256 and a label, so no value serves two purposes:
//!
//! - `portal-content/seal/v1`: the AES-256-GCM key that seals a content,
//! - `portal-content/check/v1`: the check value that the database keeps, to tell a wrong key from the right
//!   key at start,
//! - `portal-content/evaluation/v1`: the key of the `evaluation_id` of the PDP.
//!
//! A sealed value is the version byte `1`, the length of the key id (one byte), the key id, a random nonce
//! of 12 bytes, and the ciphertext with its tag of 16 bytes. A random nonce is safe: the portal seals far
//! fewer than 2^32 values with one key. The associated data names the row, the field, the sequence number
//! and the effect (see [`aad`]): a sealed value does not open on another row, another field or another effect.

use aws_lc_rs::aead::{Aad, LessSafeKey, Nonce, UnboundKey, AES_256_GCM, NONCE_LEN};
use zeroize::{Zeroize, Zeroizing};

use crate::crypto::{derive_key, random_bytes};

/// The size of a key, in bytes.
pub const KEY_BYTES: usize = 32;
/// The number of characters of the base64 text of a key.
pub const KEY_TEXT_CHARS: usize = 44;
/// The label of the key that seals.
pub const SEAL_LABEL: &[u8] = b"portal-content/seal/v1";
/// The label of the check value.
pub const CHECK_LABEL: &[u8] = b"portal-content/check/v1";
/// The label of the key of the `evaluation_id`.
pub const EVALUATION_LABEL: &[u8] = b"portal-content/evaluation/v1";
/// The first byte of the associated data.
const AAD_PREFIX: &[u8] = b"portal-content/v1";
const VERSION: u8 = 1;
const TAG_LEN: usize = 16;

/// The field of the body of an item.
pub const FIELD_BODY: &str = "body";
/// The field of the payload of a turn of a thread.
pub const FIELD_TURN: &str = "turn";

/// The associated data of a sealed value: the prefix `portal-content/v1`, the id of the request, the
/// name of the field, the sequence number and the effect (empty when the item has none). Each text has
/// its length in four bytes before it, so two different tuples never give the same bytes.
pub fn aad(request_id: &str, field: &str, seq: u64, effect: Option<&str>) -> Vec<u8> {
    let effect = effect.unwrap_or("");
    let mut out =
        Vec::with_capacity(AAD_PREFIX.len() + request_id.len() + field.len() + effect.len() + 32);
    let mut part = |bytes: &[u8]| {
        out.extend_from_slice(&(bytes.len() as u32).to_be_bytes());
        out.extend_from_slice(bytes);
    };
    part(AAD_PREFIX);
    part(request_id.as_bytes());
    part(field.as_bytes());
    let seq_bytes = seq.to_be_bytes();
    part(&seq_bytes);
    part(effect.as_bytes());
    out
}

/// Reads the text of a key: the base64 text of 32 random bytes, 44 characters, with no space. The error
/// never shows the value.
pub fn parse_secret(text: &[u8]) -> Result<Zeroizing<[u8; KEY_BYTES]>, String> {
    const EXPECTED: &str = "the base64 text of 32 random bytes, 44 characters (make one with `portal inbox content-key`)";
    let text = std::str::from_utf8(text)
        .map_err(|_| format!("the value is not text, expected {EXPECTED}"))?;
    if text.len() != KEY_TEXT_CHARS {
        return Err(format!(
            "the value has {} characters, expected {EXPECTED}",
            text.len()
        ));
    }
    let decoded = Zeroizing::new(
        inbox_core::b64::decode(text)
            .ok_or_else(|| format!("the value is not base64, expected {EXPECTED}"))?,
    );
    if decoded.len() != KEY_BYTES || inbox_core::b64::encode(&decoded) != text {
        return Err(format!(
            "the value is not the canonical base64 of 32 bytes, expected {EXPECTED}"
        ));
    }
    let mut key = Zeroizing::new([0u8; KEY_BYTES]);
    key.copy_from_slice(&decoded);
    if key.iter().all(|b| *b == key[0]) {
        return Err(format!(
            "all bytes of the value are equal, expected {EXPECTED}"
        ));
    }
    Ok(key)
}

struct ContentKey {
    id: String,
    seal: LessSafeKey,
    check: [u8; 32],
    evaluation: [u8; 32],
}

impl ContentKey {
    fn new(id: &str, key: &[u8; KEY_BYTES]) -> Result<Self, String> {
        let mut seal = derive_key(key, SEAL_LABEL)?;
        let unbound = UnboundKey::new(&AES_256_GCM, &seal)
            .map_err(|_| "internal error: the sealing key is not valid".to_string());
        seal.zeroize();
        Ok(Self {
            id: id.to_string(),
            seal: LessSafeKey::new(unbound?),
            check: derive_key(key, CHECK_LABEL)?,
            evaluation: derive_key(key, EVALUATION_LABEL)?,
        })
    }
}

impl Drop for ContentKey {
    fn drop(&mut self) {
        self.check.zeroize();
        self.evaluation.zeroize();
    }
}

/// The keys of the server for the contents. The program builds it one time at start.
pub struct ContentBox {
    keys: Vec<ContentKey>,
    active: usize,
}

impl std::fmt::Debug for ContentBox {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ContentBox")
            .field("keys", &self.keys.iter().map(|k| &k.id).collect::<Vec<_>>())
            .field("active", &self.keys[self.active].id)
            .field("secrets", &"<hidden>")
            .finish()
    }
}

impl ContentBox {
    /// Builds the box. `keys` holds the id and the bytes of each key. `active` is the id of the key that
    /// seals. An id that occurs two times, an `active` that no key has and an empty list are errors.
    pub fn new(
        keys: &[(String, Zeroizing<[u8; KEY_BYTES]>)],
        active: &str,
    ) -> Result<Self, String> {
        if keys.is_empty() {
            return Err("inbox.at_rest: expected 1 to 8 blocks key, got none".into());
        }
        let mut built: Vec<ContentKey> = Vec::with_capacity(keys.len());
        for (id, bytes) in keys {
            if id.is_empty() || id.len() > 255 {
                return Err(format!(
                    "inbox.at_rest.key {id:?}: expected an id of 1 to 16 characters"
                ));
            }
            if built.iter().any(|k| k.id == *id) {
                return Err(format!("inbox.at_rest.key {id:?}: the id occurs two times"));
            }
            built.push(ContentKey::new(id, bytes)?);
        }
        let active = built.iter().position(|k| k.id == active).ok_or_else(|| {
            format!("inbox.at_rest.active_key: expected the id of a block key, got {active:?}")
        })?;
        Ok(Self {
            keys: built,
            active,
        })
    }

    /// The id of the key that seals the new contents.
    pub fn active_kid(&self) -> &str {
        &self.keys[self.active].id
    }

    /// The ids of all the keys, in the order of the file.
    pub fn kids(&self) -> impl Iterator<Item = &str> {
        self.keys.iter().map(|k| k.id.as_str())
    }

    pub fn has_kid(&self, kid: &str) -> bool {
        self.keys.iter().any(|k| k.id == kid)
    }

    /// Seals `plain` with the active key. `aad` is authenticated and not kept: the opener gives the same
    /// bytes (see [`aad`]).
    pub fn seal(&self, plain: &[u8], aad: &[u8]) -> Result<Vec<u8>, String> {
        let key = &self.keys[self.active];
        let mut nonce = [0u8; NONCE_LEN];
        random_bytes(&mut nonce)?;
        let mut buffer = Zeroizing::new(plain.to_vec());
        key.seal
            .seal_in_place_append_tag(
                Nonce::assume_unique_for_key(nonce),
                Aad::from(aad),
                &mut *buffer,
            )
            .map_err(|_| "internal error: the encryption failed".to_string())?;
        let mut out = Vec::with_capacity(2 + key.id.len() + NONCE_LEN + buffer.len());
        out.push(VERSION);
        out.push(key.id.len() as u8);
        out.extend_from_slice(key.id.as_bytes());
        out.extend_from_slice(&nonce);
        out.extend_from_slice(&buffer);
        Ok(out)
    }

    /// The id of the key that sealed `sealed`, read from its header. The result is `None` when the bytes
    /// are not a sealed value. It tells nothing about the key: only [`ContentBox::open`] proves it.
    pub fn kid_of(sealed: &[u8]) -> Option<&str> {
        if sealed.first() != Some(&VERSION) {
            return None;
        }
        let len = usize::from(*sealed.get(1)?);
        std::str::from_utf8(sealed.get(2..2 + len)?).ok()
    }

    /// Opens a sealed value with the key that its header names. `None` when the key is not in the file,
    /// the associated data is not the one that sealed it, or the bytes changed.
    pub fn open(&self, sealed: &[u8], aad: &[u8]) -> Option<Zeroizing<Vec<u8>>> {
        let kid = Self::kid_of(sealed)?;
        let key = self.keys.iter().find(|k| k.id == kid)?;
        let body = 2 + kid.len();
        if sealed.len() < body + NONCE_LEN + TAG_LEN {
            return None;
        }
        let nonce = Nonce::try_assume_unique_for_key(&sealed[body..body + NONCE_LEN]).ok()?;
        let mut buffer = Zeroizing::new(sealed[body + NONCE_LEN..].to_vec());
        let plain_len = key
            .seal
            .open_in_place(nonce, Aad::from(aad), &mut buffer)
            .ok()?
            .len();
        buffer.truncate(plain_len);
        Some(buffer)
    }

    /// The value that the database keeps to tell whether a key is the key of its contents. It reveals nothing
    /// about the key. `None` when the id is not in the file.
    #[allow(dead_code)] // the PDP of lot L6 reads it
    pub fn check_value(&self, kid: &str) -> Option<[u8; 32]> {
        self.keys.iter().find(|k| k.id == kid).map(|k| k.check)
    }

    /// The id and the check value of each key, in the order of the file.
    pub fn check_values(&self) -> Vec<(String, [u8; 32])> {
        self.keys.iter().map(|k| (k.id.clone(), k.check)).collect()
    }

    /// The key of the `evaluation_id` of the active key, with its id. The PDP signs with it.
    pub fn evaluation_key(&self) -> (&str, &[u8; 32]) {
        let key = &self.keys[self.active];
        (&key.id, &key.evaluation)
    }

    /// The key of the `evaluation_id` of the key `kid`. `None` when the id is not in the file.
    #[allow(dead_code)] // the submission API of lot L7 checks the `evaluation_id` with it
    pub fn evaluation_key_of(&self, kid: &str) -> Option<&[u8; 32]> {
        self.keys
            .iter()
            .find(|k| k.id == kid)
            .map(|k| &k.evaluation)
    }

    /// Checks the rows of the database against the keys of the file. `rows` holds each key id that sealed
    /// contents, with the number of rows. A key id that the file does not hold stops the start: the
    /// message names the id and the count, and the way out.
    pub fn check_orphans(&self, rows: &[(String, u64)]) -> Result<(), String> {
        let mut lost: Vec<&(String, u64)> = rows
            .iter()
            .filter(|(kid, count)| *count > 0 && !self.has_kid(kid))
            .collect();
        lost.sort();
        match lost.first() {
            None => Ok(()),
            Some((kid, count)) => Err(format!(
                "inbox.at_rest: {count} sealed contents name the key {kid:?}, and the file has no block key {kid:?} (the file has {}); add the block key {kid:?} with its old secret, or run `portal inbox content-purge --config FILE --key {kid}` to erase these contents and keep their metadata",
                self.kids()
                    .map(|k| format!("{k:?}"))
                    .collect::<Vec<_>>()
                    .join(", ")
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bytes(seed: u8) -> Zeroizing<[u8; KEY_BYTES]> {
        let mut key = Zeroizing::new([0u8; KEY_BYTES]);
        for (i, b) in key.iter_mut().enumerate() {
            *b = seed.wrapping_add(i as u8);
        }
        key
    }

    fn one(id: &str, seed: u8) -> ContentBox {
        ContentBox::new(&[(id.to_string(), bytes(seed))], id).unwrap()
    }

    #[test]
    fn seal_open_round_trip() {
        let b = one("k1", 1);
        let aad = aad("req-1", FIELD_BODY, 0, Some("sha256:abc"));
        let sealed = b.seal(b"DROP TABLE users", &aad).unwrap();
        assert!(
            !sealed.windows(4).any(|w| w == b"DROP"),
            "the text is not in the bytes"
        );
        assert_eq!(sealed[0], 1, "the version");
        assert_eq!(ContentBox::kid_of(&sealed), Some("k1"));
        assert_eq!(&**b.open(&sealed, &aad).unwrap(), b"DROP TABLE users");
        // Two seals of one text differ: the nonce is random.
        assert_ne!(sealed, b.seal(b"DROP TABLE users", &aad).unwrap());
        // The empty text seals and opens.
        let empty = b.seal(b"", &aad).unwrap();
        assert_eq!(b.open(&empty, &aad).unwrap().len(), 0);
        // A damaged byte, a cut value and another key do not open.
        let mut damaged = sealed.clone();
        *damaged.last_mut().unwrap() ^= 1;
        assert!(b.open(&damaged, &aad).is_none());
        assert!(b.open(&sealed[..20], &aad).is_none());
        assert!(b.open(&[], &aad).is_none());
        assert!(
            one("k1", 2).open(&sealed, &aad).is_none(),
            "another key, same id"
        );
        assert!(
            one("k2", 1).open(&sealed, &aad).is_none(),
            "the id is not in the file"
        );
    }

    #[test]
    fn aad_binds_request_field_seq_and_effect() {
        let b = one("k1", 1);
        let base = aad("req-1", FIELD_BODY, 3, Some("sha256:aa"));
        let sealed = b.seal(b"secret", &base).unwrap();
        assert!(b.open(&sealed, &base).is_some());
        for (what, other) in [
            ("request", aad("req-2", FIELD_BODY, 3, Some("sha256:aa"))),
            ("field", aad("req-1", FIELD_TURN, 3, Some("sha256:aa"))),
            ("seq", aad("req-1", FIELD_BODY, 4, Some("sha256:aa"))),
            ("effect", aad("req-1", FIELD_BODY, 3, Some("sha256:bb"))),
            ("no effect", aad("req-1", FIELD_BODY, 3, None)),
        ] {
            assert!(b.open(&sealed, &other).is_none(), "{what}");
        }
        // The length prefix: moving a byte from one text to the next gives other bytes.
        assert_ne!(
            aad("ab", "c", 0, None),
            aad("a", "bc", 0, None),
            "the bytes of two tuples never agree"
        );
        // An item without effect and an item with an empty effect are one case.
        assert_eq!(aad("r", "f", 0, None), aad("r", "f", 0, Some("")));
    }

    #[test]
    fn moved_sealed_value_does_not_open() {
        // An attacker that writes in the database moves the sealed body of one row to another row.
        let b = one("k1", 1);
        let of_a = aad("req-a", FIELD_BODY, 0, Some("sha256:aa"));
        let of_b = aad("req-b", FIELD_BODY, 0, Some("sha256:aa"));
        let sealed_a = b.seal(b"the body of A", &of_a).unwrap();
        assert!(b.open(&sealed_a, &of_b).is_none());
        // The attacker rewrites the key id in the header: the other key does not open it either.
        let two = ContentBox::new(
            &[("k1".to_string(), bytes(1)), ("k2".to_string(), bytes(9))],
            "k1",
        )
        .unwrap();
        let mut forged = two.seal(b"x", &of_a).unwrap();
        forged[2 + 1] = b'2';
        assert_eq!(ContentBox::kid_of(&forged), Some("k2"));
        assert!(two.open(&forged, &of_a).is_none());
    }

    #[test]
    fn two_keys_active_writes_all_read() {
        let keys = [
            ("old".to_string(), bytes(1)),
            ("new".to_string(), bytes(50)),
        ];
        let old_only = ContentBox::new(&keys[..1], "old").unwrap();
        let both = ContentBox::new(&keys, "new").unwrap();
        let aad = aad("r", FIELD_BODY, 0, None);
        let sealed_old = old_only.seal(b"from the old key", &aad).unwrap();
        let sealed_new = both.seal(b"from the new key", &aad).unwrap();
        assert_eq!(ContentBox::kid_of(&sealed_old), Some("old"));
        assert_eq!(
            ContentBox::kid_of(&sealed_new),
            Some("new"),
            "the active key seals"
        );
        assert_eq!(
            &**both.open(&sealed_old, &aad).unwrap(),
            b"from the old key"
        );
        assert_eq!(
            &**both.open(&sealed_new, &aad).unwrap(),
            b"from the new key"
        );
        assert!(
            old_only.open(&sealed_new, &aad).is_none(),
            "a box without the new key cannot read"
        );
        assert_eq!(both.active_kid(), "new");
        assert_eq!(both.kids().collect::<Vec<_>>(), ["old", "new"]);
        // The three derived values differ, for each key and between keys.
        let (kid, evaluation) = both.evaluation_key();
        assert_eq!(kid, "new");
        assert_ne!(both.check_value("new").unwrap(), *evaluation);
        assert_ne!(both.check_value("new"), both.check_value("old"));
        assert_ne!(both.evaluation_key_of("old"), both.evaluation_key_of("new"));
        assert!(both.check_value("none").is_none());
    }

    #[test]
    fn the_box_refuses_bad_key_sets_and_hides_its_secrets() {
        let k = |id: &str, seed| (id.to_string(), bytes(seed));
        assert!(ContentBox::new(&[], "a").is_err());
        assert!(ContentBox::new(&[k("a", 1), k("a", 2)], "a")
            .unwrap_err()
            .contains("two times"));
        assert!(ContentBox::new(&[k("a", 1)], "b")
            .unwrap_err()
            .contains("inbox.at_rest.active_key"));
        let shown = format!("{:?}", one("k1", 1));
        assert!(
            shown.contains("k1") && shown.contains("<hidden>"),
            "{shown}"
        );
        assert!(!shown.contains("check"), "{shown}");
    }

    #[test]
    fn the_text_of_a_key_is_base64_of_32_random_bytes() {
        let good = inbox_core::b64::encode(&*bytes(7));
        assert_eq!(good.len(), KEY_TEXT_CHARS);
        assert_eq!(*parse_secret(good.as_bytes()).unwrap(), *bytes(7));
        let equal = inbox_core::b64::encode(&[7u8; 32]);
        let short = inbox_core::b64::encode(&[1u8, 2, 3]);
        let long = inbox_core::b64::encode(&[1u8; 33]);
        for (bad, why) in [
            (String::new(), "0 characters"),
            ("abc".to_string(), "3 characters"),
            (equal, "all bytes"),
            (short, "characters"),
            (long, "characters"),
            (format!("{good}\n"), "45 characters"),
            ("!".repeat(44), "not base64"),
        ] {
            let error = parse_secret(bad.as_bytes()).unwrap_err();
            assert!(error.contains(why), "{why}: {error}");
            assert!(
                !error.contains(bad.trim()) || bad.trim().is_empty(),
                "the error never shows the value: {error}"
            );
        }
    }

    #[test]
    fn a_key_id_that_the_file_lost_is_reported_with_its_count() {
        let b = ContentBox::new(
            &[("k2".to_string(), bytes(1)), ("k1".to_string(), bytes(2))],
            "k2",
        )
        .unwrap();
        assert!(b.check_orphans(&[]).is_ok());
        assert!(b
            .check_orphans(&[("k1".into(), 4), ("k2".into(), 9)])
            .is_ok());
        assert!(
            b.check_orphans(&[("gone".into(), 0)]).is_ok(),
            "no row, no loss"
        );
        let error = b
            .check_orphans(&[("k2".into(), 1), ("gone".into(), 12_034)])
            .unwrap_err();
        assert!(
            error.contains("12034") && error.contains("\"gone\""),
            "{error}"
        );
        assert!(error.contains("content-purge"), "the way out: {error}");
    }

    /// The README and `SECURITY.md` must say that the seal is not end-to-end encryption, what it covers, and what
    /// it does not cover (SPEC 5.11.1, 5.11.12, 5.11.13).
    #[test]
    fn the_documents_say_what_the_seal_covers_and_does_not_cover() {
        let readme = include_str!("../../README.md");
        let security = include_str!("../../docs/inbox/SECURITY.md");
        for (name, text) in [("README.md", readme), ("SECURITY.md", security)] {
            assert!(
                text.contains("**This is not end-to-end encryption.**"),
                "{name} says that the seal is not end-to-end encryption"
            );
            for covered in [
                "leak of the database",
                "backup",
                "replica",
                "disk image",
                "database administrator",
                "write in the database",
            ] {
                assert!(text.contains(covered), "{name} covers: {covered}");
            }
            for not_covered in [
                "has the key and the process",
                "compromised while it runs",
                "stolen phone or browser",
                "plain HTTP hop",
                "outside the data volume",
            ] {
                assert!(
                    text.contains(not_covered),
                    "{name} does not cover: {not_covered}"
                );
            }
        }
        assert!(security.contains("compromised host of the service"));
        assert!(security.contains("A key that leaves with the database"));
    }
}
