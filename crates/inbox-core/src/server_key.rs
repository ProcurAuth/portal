//! The server key file: the OPAQUE server setup and the Noise static key pair.
//!
//! Format: one line of base64 over `version(1) || noise_private(32) ||
//! noise_public(32) || opaque_server_setup`. Losing or replacing this file
//! invalidates every credential record and every enrolled device.

use opaque_ke::ServerSetup;
use zeroize::Zeroizing;

use crate::b64;
use crate::error::{Error, Result};
use crate::noise;
use crate::rng::Rng;
use crate::suite::Suite;

const VERSION: u8 = 1;

pub struct ServerKey {
    pub noise_private: Zeroizing<Vec<u8>>,
    pub noise_public: Vec<u8>,
    pub opaque: ServerSetup<Suite>,
}

impl ServerKey {
    pub fn generate(rng: &mut Rng) -> Result<ServerKey> {
        let kp = noise::generate_keypair()?;
        Ok(ServerKey {
            noise_private: kp.private,
            noise_public: kp.public,
            opaque: ServerSetup::<Suite>::new(rng),
        })
    }

    /// Serialized form, to be written to a 0600 file.
    pub fn encode(&self) -> Zeroizing<String> {
        let mut raw = Zeroizing::new(Vec::with_capacity(512));
        raw.push(VERSION);
        raw.extend_from_slice(&self.noise_private);
        raw.extend_from_slice(&self.noise_public);
        raw.extend_from_slice(&self.opaque.serialize());
        Zeroizing::new(b64::encode(&raw))
    }

    pub fn decode(text: &str) -> Result<ServerKey> {
        let raw = Zeroizing::new(b64::decode(text).ok_or(Error::Malformed("server key: base64"))?);
        if raw.len() < 65 || raw[0] != VERSION {
            return Err(Error::Malformed("server key: version or length"));
        }
        let opaque = ServerSetup::<Suite>::deserialize(&raw[65..])
            .map_err(|_| Error::Malformed("server key: opaque setup"))?;
        Ok(ServerKey {
            noise_private: Zeroizing::new(raw[1..33].to_vec()),
            noise_public: raw[33..65].to_vec(),
            opaque,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        let key = ServerKey::generate(&mut Rng::os()).unwrap();
        let back = ServerKey::decode(&key.encode()).unwrap();
        assert_eq!(back.noise_public, key.noise_public);
        assert_eq!(*back.noise_private, *key.noise_private);
        assert_eq!(back.opaque.serialize(), key.opaque.serialize());
        assert!(ServerKey::decode("AAAA").is_err());
    }
}
