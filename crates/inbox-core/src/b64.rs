//! Base64 helpers. The wire and the key files use the standard alphabet with padding.
//! The enrollment code uses the URL-safe alphabet without padding.

use base64::Engine as _;
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};

pub fn encode(bytes: &[u8]) -> String {
    STANDARD.encode(bytes)
}

pub fn decode(text: &str) -> Option<Vec<u8>> {
    STANDARD.decode(text.trim()).ok()
}

/// Encodes with the URL-safe alphabet and no padding.
pub fn encode_url(bytes: &[u8]) -> String {
    URL_SAFE_NO_PAD.encode(bytes)
}

/// Decodes the URL-safe alphabet. Padding, whitespace and non-canonical trailing bits are errors.
pub fn decode_url(text: &str) -> Option<Vec<u8>> {
    URL_SAFE_NO_PAD.decode(text).ok()
}

/// `#[serde(with = "inbox_core::b64::serde_bytes")]` for `Vec<u8>` fields.
pub mod serde_bytes {
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(bytes: &[u8], s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&super::encode(bytes))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<u8>, D::Error> {
        let text = <&str>::deserialize(d)?;
        super::decode(text).ok_or_else(|| serde::de::Error::custom("invalid base64"))
    }
}
