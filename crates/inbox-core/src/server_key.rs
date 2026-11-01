//! The server key file: the Noise static key pair (X25519) of the server.
//!
//! Format: one line of base64 (standard, padded) over the 32 bytes of the private
//! key. The public key is derived. Replacing this file invalidates every enrolled
//! device and every enrollment code.

use zeroize::Zeroizing;

use crate::b64;
use crate::error::{Error, Result};
use crate::noise;

pub struct ServerKey {
    pub noise_private: Zeroizing<Vec<u8>>,
    pub noise_public: Vec<u8>,
}

impl ServerKey {
    pub fn generate() -> Result<ServerKey> {
        let kp = noise::generate_keypair()?;
        Ok(ServerKey { noise_private: kp.private, noise_public: kp.public })
    }

    /// Serialized form, to be written to a 0600 file.
    pub fn encode(&self) -> Zeroizing<String> {
        Zeroizing::new(b64::encode(&self.noise_private))
    }

    /// Reads the private key and derives the public key.
    pub fn decode(text: &str) -> Result<ServerKey> {
        let raw = Zeroizing::new(b64::decode(text).ok_or(Error::Malformed("server key: base64"))?);
        if raw.len() != 32 {
            return Err(Error::Malformed("server key: length"));
        }
        let noise_public = noise::public_key(&raw)?;
        Ok(ServerKey { noise_private: raw, noise_public })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        let key = ServerKey::generate().unwrap();
        let text = key.encode();
        assert_eq!(text.len(), 44, "one line of base64 over 32 bytes");
        let back = ServerKey::decode(&format!("{}\n", *text)).unwrap();
        assert_eq!(back.noise_public, key.noise_public);
        assert_eq!(*back.noise_private, *key.noise_private);
    }

    #[test]
    fn bad_input_is_rejected() {
        assert!(ServerKey::decode("AAAA").is_err());
        assert!(ServerKey::decode("not base64 !!").is_err());
        assert!(ServerKey::decode(&b64::encode(&[7u8; 64])).is_err());
    }
}
