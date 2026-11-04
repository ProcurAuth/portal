//! The encryption of the second-factor secrets at rest.
//!
//! The key comes from `authentication.totp_key_env` or `authentication.totp_key_file`. It has 32 random bytes,
//! written as 64 hexadecimal characters. The program derives three keys from it with HMAC-SHA-256, each with its
//! own label, so no value serves two purposes:
//!
//! - the AES-256-GCM key that seals a secret,
//! - the check value that the database keeps, to tell a wrong key from the right key at start,
//! - the key of the hashes of the recovery codes.
//!
//! AES-256-GCM comes from `aws-lc-rs`, which the portal already has. A sealed value is the version byte `1`, a random
//! nonce of 12 bytes, and the ciphertext with its tag. The associated data of a secret names the login: a row cannot be
//! moved to another account. A random nonce is safe: the portal seals far fewer than 2^32 values with one key.

use aws_lc_rs::aead::{Aad, LessSafeKey, Nonce, UnboundKey, AES_256_GCM, NONCE_LEN};
use zeroize::{Zeroize, Zeroizing};

use crate::crypto::{derive_key, random_bytes};

/// The size of the key in the configuration, in bytes.
pub const KEY_BYTES: usize = 32;
const VERSION: u8 = 1;

/// The keys that the second factor derives from `authentication.totp_key`.
pub struct SecretBox {
    seal_key: LessSafeKey,
    check: [u8; 32],
    recovery_key: [u8; 32],
}

impl SecretBox {
    /// Reads the text of the key: exactly 64 hexadecimal characters. The error never shows the value.
    pub fn parse_key(text: &[u8]) -> Result<Zeroizing<[u8; KEY_BYTES]>, String> {
        if text.len() != KEY_BYTES * 2 {
            return Err(format!(
                "the value has {} characters, expected 64 hexadecimal characters (32 random bytes, for example from `openssl rand -hex 32`)",
                text.len()
            ));
        }
        let mut key = Zeroizing::new([0u8; KEY_BYTES]);
        for (index, pair) in text.chunks(2).enumerate() {
            let hex = |c: u8| match c {
                b'0'..=b'9' => Some(c - b'0'),
                b'a'..=b'f' => Some(c - b'a' + 10),
                b'A'..=b'F' => Some(c - b'A' + 10),
                _ => None,
            };
            match (hex(pair[0]), hex(pair[1])) {
                (Some(high), Some(low)) => key[index] = (high << 4) | low,
                _ => {
                    return Err(format!(
                        "the character at position {} is not hexadecimal, expected 64 hexadecimal characters (32 random bytes, for example from `openssl rand -hex 32`)",
                        index * 2 + 1
                    ))
                }
            }
        }
        if key.iter().all(|b| *b == key[0]) {
            return Err(
                "all bytes of the value are equal, expected 32 random bytes (for example from `openssl rand -hex 32`)"
                    .to_string(),
            );
        }
        Ok(key)
    }

    pub fn new(key: &[u8; KEY_BYTES]) -> Result<Self, String> {
        let mut seal = derive_key(key, b"totp-seal\n1")?;
        let unbound = UnboundKey::new(&AES_256_GCM, &seal)
            .map_err(|_| "internal error: the sealing key is not valid".to_string());
        seal.zeroize();
        Ok(Self {
            seal_key: LessSafeKey::new(unbound?),
            check: derive_key(key, b"totp-check\n1")?,
            recovery_key: derive_key(key, b"totp-recovery\n1")?,
        })
    }

    /// The value that the database keeps to tell whether a key is the key of its secrets. It reveals nothing
    /// about the key.
    pub fn check_value(&self) -> [u8; 32] {
        self.check
    }

    /// Seals `plain`. `aad` is authenticated and not kept: the opener gives the same bytes.
    pub fn seal(&self, plain: &[u8], aad: &[u8]) -> Result<Vec<u8>, String> {
        let mut nonce = [0u8; NONCE_LEN];
        random_bytes(&mut nonce)?;
        let mut buffer = plain.to_vec();
        let sealed = self.seal_key.seal_in_place_append_tag(
            Nonce::assume_unique_for_key(nonce),
            Aad::from(aad),
            &mut buffer,
        );
        if sealed.is_err() {
            buffer.zeroize();
            return Err("internal error: the encryption failed".to_string());
        }
        let mut out = Vec::with_capacity(1 + NONCE_LEN + buffer.len());
        out.push(VERSION);
        out.extend_from_slice(&nonce);
        out.extend_from_slice(&buffer);
        Ok(out)
    }

    /// Opens a sealed value. `None` when the key, the associated data or the bytes are not the ones that sealed it.
    pub fn open(&self, sealed: &[u8], aad: &[u8]) -> Option<Zeroizing<Vec<u8>>> {
        if sealed.len() < 1 + NONCE_LEN + 16 || sealed[0] != VERSION {
            return None;
        }
        let nonce = Nonce::try_assume_unique_for_key(&sealed[1..1 + NONCE_LEN]).ok()?;
        let mut buffer = Zeroizing::new(sealed[1 + NONCE_LEN..].to_vec());
        let plain_len = self
            .seal_key
            .open_in_place(nonce, Aad::from(aad), &mut buffer)
            .ok()?
            .len();
        buffer.truncate(plain_len);
        Some(buffer)
    }

    /// The hash of a recovery code in its canonical form (see [`crate::totp::normalize_recovery`]). The hash is a
    /// keyed HMAC, so the database can find a code by its hash, and a copy of the database lets nobody test guesses
    /// without the key.
    pub fn recovery_hash(&self, canonical: &str) -> [u8; 32] {
        let mut input = b"recovery\n".to_vec();
        input.extend_from_slice(canonical.as_bytes());
        derive_key(&self.recovery_key, &input).unwrap_or([0u8; 32])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(byte: u8) -> [u8; KEY_BYTES] {
        let mut key = [0u8; KEY_BYTES];
        for (i, b) in key.iter_mut().enumerate() {
            *b = byte.wrapping_add(i as u8);
        }
        key
    }

    #[test]
    fn a_sealed_secret_opens_with_the_same_key_and_the_same_login_only() {
        let a = SecretBox::new(&key(1)).unwrap();
        let sealed = a.seal(b"twenty bytes secret!", b"alice").unwrap();
        assert!(!sealed.windows(6).any(|w| w == b"twenty"), "not in clear");
        assert_eq!(&**a.open(&sealed, b"alice").unwrap(), b"twenty bytes secret!");
        assert!(a.open(&sealed, b"bob").is_none(), "another account");
        let b = SecretBox::new(&key(2)).unwrap();
        assert!(b.open(&sealed, b"alice").is_none(), "another key");
        let mut damaged = sealed.clone();
        *damaged.last_mut().unwrap() ^= 1;
        assert!(a.open(&damaged, b"alice").is_none(), "a damaged tag");
        assert!(a.open(&sealed[..20], b"alice").is_none(), "too short");
        // Two seals of the same value differ: the nonce is random.
        assert_ne!(sealed, a.seal(b"twenty bytes secret!", b"alice").unwrap());
    }

    #[test]
    fn the_three_derived_values_differ_and_depend_on_the_key() {
        let a = SecretBox::new(&key(1)).unwrap();
        let b = SecretBox::new(&key(2)).unwrap();
        assert_ne!(a.check_value(), b.check_value());
        assert_ne!(a.recovery_hash("abcdefghij"), b.recovery_hash("abcdefghij"));
        assert_ne!(a.check_value(), a.recovery_hash("abcdefghij"));
        assert_eq!(a.recovery_hash("abcdefghij"), a.recovery_hash("abcdefghij"));
        assert_ne!(a.recovery_hash("abcdefghij"), a.recovery_hash("abcdefghik"));
    }

    #[test]
    fn the_text_of_a_key_is_64_hexadecimal_characters_of_random_bytes() {
        let text = "00112233445566778899aabbccddeeff00112233445566778899AABBCCDDEEFF";
        let parsed = SecretBox::parse_key(text.as_bytes()).unwrap();
        assert_eq!(parsed[0], 0x00);
        assert_eq!(parsed[31], 0xff);
        for (bad, why) in [
            ("", "0 characters"),
            ("abcd", "4 characters"),
            (&"g".repeat(64), "not hexadecimal"),
            (&"ab".repeat(32), "all bytes are equal"),
            (&format!("{text}00"), "66 characters"),
        ] {
            let error = SecretBox::parse_key(bad.as_bytes()).unwrap_err();
            assert!(error.contains(why.split(' ').next().unwrap()), "{error}");
            assert!(!error.contains("gggg"), "the error never shows the value");
        }
    }
}
