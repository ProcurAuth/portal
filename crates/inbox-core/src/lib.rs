//! Inbox wire protocol and cryptography.
//!
//! This crate performs no I/O. The server and the Android app (through JNI) drive
//! the same state machines so that the security-sensitive code exists exactly once.
//!
//! Security model:
//! - An account of the portal makes an enrollment code. The code holds the server
//!   address, the public key of the server and a one-time secret.
//! - The phone pins the server key of the code. It runs Noise IK to that key and
//!   sends the secret in the first message. Only the holder of the server key can read it.
//! - The server links the new device key to the account of the code.
//! - Every later session is a Noise IK handshake between the device key and the
//!   pinned server key. The server can revoke each device alone.

#![forbid(unsafe_code)]

pub mod b64;
pub mod effect;
pub mod enroll;
pub mod error;
pub mod form;
pub mod noise;
pub mod proto;
pub mod render;
pub mod rng;
pub mod server_key;

pub use error::{Error, Result};

/// Version of the device protocol. Change it on any incompatible wire change.
/// 4: enrollment with a one-time code and Noise IK (no OPAQUE, no shared password).
///    The login of the account is the user name of the device. It is read-only.
/// 5: items with an answer (`request`, `message`, `settled`, `digest`, `answer_result` and the
///    client frames `answer`, `reply`, `refresh`, `sync`, `set_prefs`), the receipt `ack` by `id`,
///    the effect hash of an approval, `prefs` and `body_purge_after_ms` in `welcome`.
///    No compatibility with 4.
pub const PROTOCOL_VERSION: u32 = 5;
