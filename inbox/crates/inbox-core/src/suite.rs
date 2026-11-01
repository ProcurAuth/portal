//! OPAQUE cipher suite and role separation.

use opaque_ke::argon2::Argon2;
use opaque_ke::{CipherSuite, Ristretto255, TripleDh};

/// ristretto255 OPRF, 3DH key exchange over ristretto255/SHA-512, and Argon2id key
/// stretching with the argon2 crate defaults (19 MiB, 2 passes, 1 lane).
pub struct Suite;

impl CipherSuite for Suite {
    type OprfCs = Ristretto255;
    type KeyExchange = TripleDh<Ristretto255, sha2::Sha512>;
    type Ksf = Argon2<'static>;
}

/// Password-protected roles. Each role has its own OPAQUE credential identifier
/// and context, so a transcript of one role can never be replayed against another.
/// Only devices authenticate with a password today; the admin front has none.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    Device,
}

impl Role {
    pub fn as_str(self) -> &'static str {
        match self {
            Role::Device => "device",
        }
    }

    pub(crate) fn credential_identifier(self) -> &'static [u8] {
        match self {
            Role::Device => b"inbox/v1/credential/device",
        }
    }

    pub(crate) fn context(self) -> &'static [u8] {
        match self {
            Role::Device => b"inbox/v1/login/device",
        }
    }
}
