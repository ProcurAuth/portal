//! Randomness sources.
//!
//! Native targets use the operating system generator. WebAssembly has no OS
//! generator without extra glue, so the host passes 32 bytes from
//! `crypto.getRandomValues` and a ChaCha20 stream is derived from them.

use rand_chacha::ChaCha20Rng;
use rand_core::{CryptoRng, RngCore, SeedableRng};

/// A cryptographically secure generator, either OS-backed or host-seeded.
pub enum Rng {
    #[cfg(feature = "os-rng")]
    Os(rand_core::OsRng),
    Seeded(Box<ChaCha20Rng>),
}

impl Rng {
    #[cfg(feature = "os-rng")]
    pub fn os() -> Rng {
        Rng::Os(rand_core::OsRng)
    }

    /// `seed` must come from a cryptographically secure source.
    pub fn from_seed(seed: [u8; 32]) -> Rng {
        Rng::Seeded(Box::new(ChaCha20Rng::from_seed(seed)))
    }

    pub fn bytes<const N: usize>(&mut self) -> [u8; N] {
        let mut out = [0u8; N];
        self.fill_bytes(&mut out);
        out
    }
}

impl RngCore for Rng {
    fn next_u32(&mut self) -> u32 {
        match self {
            #[cfg(feature = "os-rng")]
            Rng::Os(r) => r.next_u32(),
            Rng::Seeded(r) => r.next_u32(),
        }
    }

    fn next_u64(&mut self) -> u64 {
        match self {
            #[cfg(feature = "os-rng")]
            Rng::Os(r) => r.next_u64(),
            Rng::Seeded(r) => r.next_u64(),
        }
    }

    fn fill_bytes(&mut self, dest: &mut [u8]) {
        match self {
            #[cfg(feature = "os-rng")]
            Rng::Os(r) => r.fill_bytes(dest),
            Rng::Seeded(r) => r.fill_bytes(dest),
        }
    }

    fn try_fill_bytes(&mut self, dest: &mut [u8]) -> Result<(), rand_core::Error> {
        match self {
            #[cfg(feature = "os-rng")]
            Rng::Os(r) => r.try_fill_bytes(dest),
            Rng::Seeded(r) => r.try_fill_bytes(dest),
        }
    }
}

impl CryptoRng for Rng {}
