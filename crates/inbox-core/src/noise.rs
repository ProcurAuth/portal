//! Noise IK between a device key and a pinned server key. Two uses share this code.
//! They differ only by the prologue: [`SESSION_PROLOGUE`] for device sessions,
//! [`ENROLL_PROLOGUE`] for enrollment (see [`crate::enroll`]).
//!
//! In a session, message 1 (device -> server) carries [`crate::proto::SessionHello`], message 2
//! (server -> device) carries [`crate::proto::SessionAccept`]. Every later frame is
//! one Noise transport message holding one JSON protocol message.
//!
//! Message 1 can be replayed by an observer, but the replayer cannot read message 2
//! nor produce a valid transport frame. The server therefore treats a session as
//! established only after the first transport frame decrypts.

use snow::params::DHChoice;
use snow::resolvers::{CryptoResolver, DefaultResolver};
use snow::{Builder, HandshakeState, TransportState};
use zeroize::Zeroizing;

use crate::error::{Error, Result};

pub const NOISE_PARAMS: &str = "Noise_IK_25519_ChaChaPoly_BLAKE2s";
/// Prologue of `/v1/session`.
pub const SESSION_PROLOGUE: &[u8] = b"inbox/v1/session";
/// Prologue of `/v1/enroll`.
pub const ENROLL_PROLOGUE: &[u8] = b"inbox/v1/enroll";

/// Largest Noise message; one WebSocket frame carries exactly one.
pub const MAX_MESSAGE: usize = 65535;
/// Largest plaintext that fits in one transport message.
pub const MAX_PLAINTEXT: usize = MAX_MESSAGE - 16;

fn builder() -> Builder<'static> {
    Builder::new(NOISE_PARAMS.parse().expect("valid Noise parameters"))
}

/// A static X25519 key pair.
pub struct Keypair {
    pub private: Zeroizing<Vec<u8>>,
    pub public: Vec<u8>,
}

pub fn generate_keypair() -> Result<Keypair> {
    let kp = builder().generate_keypair().map_err(|_| Error::Internal("keypair generation"))?;
    Ok(Keypair { private: Zeroizing::new(kp.private), public: kp.public })
}

/// Computes the X25519 public key of a private key.
pub fn public_key(private: &[u8]) -> Result<Vec<u8>> {
    check_key(private)?;
    let mut dh = DefaultResolver
        .resolve_dh(&DHChoice::Curve25519)
        .ok_or(Error::Internal("x25519 unavailable"))?;
    dh.set(private);
    Ok(dh.pubkey().to_vec())
}

fn check_key(key: &[u8]) -> Result<()> {
    if key.len() == 32 { Ok(()) } else { Err(Error::Malformed("x25519 key length")) }
}

/// An established, encrypted session.
pub struct Session {
    state: TransportState,
}

impl Session {
    pub fn encrypt(&mut self, plaintext: &[u8]) -> Result<Vec<u8>> {
        if plaintext.len() > MAX_PLAINTEXT {
            return Err(Error::Internal("message too large"));
        }
        let mut out = vec![0u8; plaintext.len() + 16];
        let n = self
            .state
            .write_message(plaintext, &mut out)
            .map_err(|_| Error::Internal("encrypt"))?;
        out.truncate(n);
        Ok(out)
    }

    pub fn decrypt(&mut self, message: &[u8]) -> Result<Vec<u8>> {
        if message.len() > MAX_MESSAGE {
            return Err(Error::Malformed("frame too large"));
        }
        let mut out = vec![0u8; message.len()];
        let n =
            self.state.read_message(message, &mut out).map_err(|_| Error::AuthenticationFailed)?;
        out.truncate(n);
        Ok(out)
    }
}

/// Device side of the handshake.
pub struct Initiator {
    hs: HandshakeState,
}

impl Initiator {
    /// Returns the initiator and handshake message 1 carrying `hello`.
    /// The prologue is [`SESSION_PROLOGUE`].
    pub fn start(
        device_private: &[u8],
        server_public: &[u8],
        hello: &[u8],
    ) -> Result<(Initiator, Vec<u8>)> {
        Self::start_with_prologue(SESSION_PROLOGUE, device_private, server_public, hello)
    }

    /// Same as [`Initiator::start`] with an explicit prologue.
    pub fn start_with_prologue(
        prologue: &[u8],
        device_private: &[u8],
        server_public: &[u8],
        hello: &[u8],
    ) -> Result<(Initiator, Vec<u8>)> {
        check_key(device_private)?;
        check_key(server_public)?;
        let mut hs = builder()
            .local_private_key(device_private)
            .and_then(|b| b.remote_public_key(server_public))
            .and_then(|b| b.prologue(prologue))
            .and_then(|b| b.build_initiator())
            .map_err(|_| Error::Internal("noise initiator"))?;
        let mut msg = vec![0u8; MAX_MESSAGE];
        let n =
            hs.write_message(hello, &mut msg).map_err(|_| Error::Internal("noise message 1"))?;
        msg.truncate(n);
        Ok((Initiator { hs }, msg))
    }

    /// Consumes handshake message 2. Fails unless it comes from the pinned server key.
    pub fn finish(mut self, message: &[u8]) -> Result<(Session, Vec<u8>)> {
        let mut payload = vec![0u8; message.len()];
        let n =
            self.hs.read_message(message, &mut payload).map_err(|_| Error::AuthenticationFailed)?;
        payload.truncate(n);
        let state = self.hs.into_transport_mode().map_err(|_| Error::Internal("noise split"))?;
        Ok((Session { state }, payload))
    }
}

/// Server side of the handshake.
pub struct Responder {
    hs: HandshakeState,
}

impl Responder {
    /// Consumes handshake message 1. Returns the responder, the device static public
    /// key (authenticated by the handshake) and the hello payload.
    /// The prologue is [`SESSION_PROLOGUE`].
    pub fn start(server_private: &[u8], message: &[u8]) -> Result<(Responder, Vec<u8>, Vec<u8>)> {
        Self::start_with_prologue(SESSION_PROLOGUE, server_private, message)
    }

    /// Same as [`Responder::start`] with an explicit prologue.
    pub fn start_with_prologue(
        prologue: &[u8],
        server_private: &[u8],
        message: &[u8],
    ) -> Result<(Responder, Vec<u8>, Vec<u8>)> {
        check_key(server_private)?;
        let mut hs = builder()
            .local_private_key(server_private)
            .and_then(|b| b.prologue(prologue))
            .and_then(|b| b.build_responder())
            .map_err(|_| Error::Internal("noise responder"))?;
        let mut payload = vec![0u8; message.len()];
        let n = hs.read_message(message, &mut payload).map_err(|_| Error::AuthenticationFailed)?;
        payload.truncate(n);
        let remote =
            hs.get_remote_static().ok_or(Error::Internal("missing remote static"))?.to_vec();
        Ok((Responder { hs }, remote, payload))
    }

    /// Returns the session and handshake message 2 carrying `accept`.
    pub fn finish(mut self, accept: &[u8]) -> Result<(Session, Vec<u8>)> {
        let mut msg = vec![0u8; MAX_MESSAGE];
        let n = self
            .hs
            .write_message(accept, &mut msg)
            .map_err(|_| Error::Internal("noise message 2"))?;
        msg.truncate(n);
        let state = self.hs.into_transport_mode().map_err(|_| Error::Internal("noise split"))?;
        Ok((Session { state }, msg))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn handshake_and_transport() {
        let device = generate_keypair().unwrap();
        let server = generate_keypair().unwrap();
        let (init, m1) = Initiator::start(&device.private, &server.public, b"hello").unwrap();
        let (resp, remote, hello) = Responder::start(&server.private, &m1).unwrap();
        assert_eq!(remote, device.public);
        assert_eq!(hello, b"hello");
        let (mut s_sess, m2) = resp.finish(b"accept").unwrap();
        let (mut c_sess, accept) = init.finish(&m2).unwrap();
        assert_eq!(accept, b"accept");
        let ct = c_sess.encrypt(b"ping").unwrap();
        assert_eq!(s_sess.decrypt(&ct).unwrap(), b"ping");
        let ct = s_sess.encrypt(b"pong").unwrap();
        assert_eq!(c_sess.decrypt(&ct).unwrap(), b"pong");
        // Replayed or reordered frames are rejected.
        assert!(c_sess.decrypt(&ct).is_err());
    }

    #[test]
    fn public_key_matches_generated_pair() {
        let kp = generate_keypair().unwrap();
        assert_eq!(public_key(&kp.private).unwrap(), kp.public);
        assert!(public_key(&[1u8; 31]).is_err());
    }

    #[test]
    fn prologues_are_separate() {
        let device = generate_keypair().unwrap();
        let server = generate_keypair().unwrap();
        let (_, m1) =
            Initiator::start_with_prologue(ENROLL_PROLOGUE, &device.private, &server.public, b"h")
                .unwrap();
        assert!(Responder::start(&server.private, &m1).is_err());
        assert!(Responder::start_with_prologue(ENROLL_PROLOGUE, &server.private, &m1).is_ok());
    }

    #[test]
    fn wrong_server_key_is_rejected() {
        let device = generate_keypair().unwrap();
        let server = generate_keypair().unwrap();
        let impostor = generate_keypair().unwrap();
        let (init, m1) = Initiator::start(&device.private, &server.public, b"hello").unwrap();
        // The impostor cannot even read message 1, which is encrypted to the real key.
        assert!(Responder::start(&impostor.private, &m1).is_err());
        // A full impostor handshake produces a message 2 the device rejects.
        let (_, m1b) = Initiator::start(&device.private, &impostor.public, b"hello").unwrap();
        let (resp, _, _) = Responder::start(&impostor.private, &m1b).unwrap();
        let (_, m2) = resp.finish(b"accept").unwrap();
        assert!(init.finish(&m2).is_err());
    }
}
