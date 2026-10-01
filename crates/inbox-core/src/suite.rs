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

/// The two password-protected roles. Each has its own credential record, OPAQUE
/// credential identifier and context, so a device password cannot open the admin
/// front and a transcript of one role cannot be replayed against the other.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    Device,
    Admin,
}

impl Role {
    pub fn parse(text: &str) -> Option<Role> {
        match text {
            "device" => Some(Role::Device),
            "admin" => Some(Role::Admin),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Role::Device => "device",
            Role::Admin => "admin",
        }
    }

    pub(crate) fn credential_identifier(self) -> &'static [u8] {
        match self {
            Role::Device => b"inbox/v1/credential/device",
            Role::Admin => b"inbox/v1/credential/admin",
        }
    }

    pub(crate) fn context(self) -> &'static [u8] {
        match self {
            Role::Device => b"inbox/v1/login/device",
            Role::Admin => b"inbox/v1/login/admin",
        }
    }
}
