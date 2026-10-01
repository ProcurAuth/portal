//! Inbox wire protocol and cryptography.
//!
//! This crate performs no I/O. The server, the Android app (through JNI) and the
//! web front (through WebAssembly) drive the same state machines so that the
//! security-sensitive code exists exactly once.
//!
//! Security model:
//! - A password is only ever used inside OPAQUE: it never leaves the client and a
//!   network capture cannot be used for an offline dictionary attack.
//! - Enrollment binds a device-generated X25519 key to the OPAQUE session and pins
//!   the server key on the device.
//! - Every later device session is a Noise IK handshake between those two keys, so
//!   the device never stores the password and can be revoked individually.

pub mod b64;
pub mod error;
pub mod login;
pub mod proto;
pub mod rng;
pub mod suite;

#[cfg(feature = "noise")]
pub mod enroll;
#[cfg(feature = "noise")]
pub mod noise;
#[cfg(feature = "noise")]
pub mod server_key;

pub use error::{Error, Result};

/// Version of the device protocol. Bumped on any incompatible wire change.
pub const PROTOCOL_VERSION: u32 = 1;

/// Re-exported so dependents name the exact same OPAQUE types.
pub use opaque_ke;
