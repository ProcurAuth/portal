//! Base64 helpers (standard alphabet, padded) used on the wire and in key files.

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;

pub fn encode(bytes: &[u8]) -> String {
    STANDARD.encode(bytes)
}

pub fn decode(text: &str) -> Option<Vec<u8>> {
    STANDARD.decode(text.trim()).ok()
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
