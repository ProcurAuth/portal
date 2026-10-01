//! JNI-free bridge logic, so it can be unit-tested on the host.
//!
//! Handles are `Box` pointers turned into `i64` for Java. Each handle owns a
//! `Mutex`, so a misbehaving caller using one handle from two threads cannot race.

use std::sync::Mutex;

use inbox_core::Error;
use inbox_core::b64;
use inbox_core::enroll::{ClientStep, EnrollClient};
use inbox_core::noise::{Initiator, Session};
use inbox_core::rng::Rng;
use serde_json::{Value, json};

/// Error code and message surfaced to Kotlin as JSON `{"error": {...}}`.
pub fn error_json(e: &Error) -> Value {
    let (code, message) = match e {
        Error::Rejected { code, message } => (code.clone(), message.clone()),
        Error::AuthenticationFailed => ("auth_failed".to_string(), e.to_string()),
        Error::UnsupportedProtocol(_) => ("protocol".to_string(), e.to_string()),
        // Local validation of what the user typed.
        Error::Malformed("user name" | "device name") => ("malformed".to_string(), e.to_string()),
        Error::Malformed(_) | Error::UnexpectedMessage(_) => {
            ("protocol".to_string(), e.to_string())
        }
        Error::Internal(_) => ("internal".to_string(), e.to_string()),
    };
    json!({ "error": { "code": code, "message": message } })
}

// ---------------------------------------------------------------- enrollment

pub struct Enroll {
    client: Mutex<Option<EnrollClient>>,
}

/// Starts an enrollment under the user's pseudonym (empty: none). Returns the
/// handle and the first text frame to send.
pub fn enroll_start(
    password: &[u8],
    device_name: &str,
    user_name: &str,
) -> Result<(Box<Enroll>, String), Error> {
    let user = Some(user_name.trim()).filter(|u| !u.is_empty());
    let (client, msg) = EnrollClient::start_as(password, device_name, user, Rng::os())?;
    Ok((Box::new(Enroll { client: Mutex::new(Some(client)) }), msg))
}

/// Feeds a server text frame. Returns `{"send": "..."}`, `{"done": {...}}` or `{"error": {...}}`.
pub fn enroll_on_message(handle: &Enroll, text: &str) -> Value {
    let mut guard = match handle.client.lock() {
        Ok(g) => g,
        Err(p) => p.into_inner(),
    };
    let Some(client) = guard.as_mut() else {
        return error_json(&Error::UnexpectedMessage("enrollment finished"));
    };
    match client.on_message(text) {
        Ok(ClientStep::Send(msg)) => json!({ "send": msg }),
        Ok(ClientStep::Done(d)) => {
            *guard = None;
            json!({ "done": {
                "device_id": d.device_id,
                "server_name": d.server_name,
                "device_private_key": b64::encode(&d.device_private_key),
                "device_public_key": b64::encode(&d.device_public_key),
                "server_public_key": b64::encode(&d.server_public_key),
            }})
        }
        Err(e) => {
            *guard = None;
            error_json(&e)
        }
    }
}

// ---------------------------------------------------------------- sessions

enum State {
    Handshake(Box<Initiator>),
    Transport(Session),
    Dead,
}

pub struct Conn {
    state: Mutex<State>,
}

fn lock(c: &Conn) -> std::sync::MutexGuard<'_, State> {
    match c.state.lock() {
        Ok(g) => g,
        Err(p) => p.into_inner(),
    }
}

/// Starts a Noise IK handshake. Returns the handle and handshake message 1.
pub fn session_start(
    device_private: &[u8],
    server_public: &[u8],
    hello: &[u8],
) -> Result<(Box<Conn>, Vec<u8>), Error> {
    let (init, msg1) = Initiator::start(device_private, server_public, hello)?;
    Ok((Box::new(Conn { state: Mutex::new(State::Handshake(Box::new(init))) }), msg1))
}

/// Consumes handshake message 2; returns the accept payload.
pub fn session_finish(conn: &Conn, msg2: &[u8]) -> Result<Vec<u8>, Error> {
    let mut st = lock(conn);
    match std::mem::replace(&mut *st, State::Dead) {
        State::Handshake(init) => {
            let (session, payload) = init.finish(msg2)?;
            *st = State::Transport(session);
            Ok(payload)
        }
        other => {
            *st = other;
            Err(Error::UnexpectedMessage("handshake already finished"))
        }
    }
}

pub fn session_encrypt(conn: &Conn, plaintext: &[u8]) -> Result<Vec<u8>, Error> {
    match &mut *lock(conn) {
        State::Transport(s) => s.encrypt(plaintext),
        _ => Err(Error::UnexpectedMessage("session not established")),
    }
}

pub fn session_decrypt(conn: &Conn, frame: &[u8]) -> Result<Vec<u8>, Error> {
    let mut st = lock(conn);
    let result = match &mut *st {
        State::Transport(s) => s.decrypt(frame),
        _ => return Err(Error::UnexpectedMessage("session not established")),
    };
    if result.is_err() {
        // A failed decryption desynchronizes the nonces: the session is unusable.
        *st = State::Dead;
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use inbox_core::enroll::EnrollServer;
    use inbox_core::login;
    use inbox_core::noise::Responder;
    use inbox_core::server_key::ServerKey;
    use inbox_core::suite::Role;

    #[test]
    fn enrollment_then_session_round_trip() {
        let key = ServerKey::generate(&mut Rng::os()).unwrap();
        let rec = login::register(&key.opaque, Role::Device, b"secret-pw", &mut Rng::os()).unwrap();
        let rec = login::parse_record(&rec).unwrap();
        let mut server = EnrollServer::new(&key, &rec);

        let (h, m1) = enroll_start(b"secret-pw", "Pixel", "alice").unwrap();
        let m2 = server.on_start(EnrollServer::parse(&m1).unwrap(), &mut Rng::os()).unwrap();
        let out = enroll_on_message(&h, &m2);
        let m3 = out["send"].as_str().unwrap().to_string();
        let verified = server.on_finish(EnrollServer::parse(&m3).unwrap()).unwrap();
        assert_eq!(verified.user_name.as_deref(), Some("alice"), "pseudo reaches the server");
        let m4 = server.accepted("0123456789abcdef0123456789abcdef", "Prod").unwrap();
        let done = enroll_on_message(&h, &m4);
        let done = &done["done"];
        assert_eq!(done["server_name"], "Prod");
        let sk = b64::decode(done["device_private_key"].as_str().unwrap()).unwrap();
        let spk = b64::decode(done["server_public_key"].as_str().unwrap()).unwrap();
        assert_eq!(spk, key.noise_public);
        // Handle is exhausted after completion.
        assert!(enroll_on_message(&h, &m4).get("error").is_some());

        let (conn, msg1) = session_start(&sk, &spk, b"{\"hello\":1}").unwrap();
        let (resp, _remote, hello) = Responder::start(&key.noise_private, &msg1).unwrap();
        assert_eq!(hello, b"{\"hello\":1}");
        let (mut srv, msg2) = resp.finish(b"accept").unwrap();
        assert_eq!(session_finish(&conn, &msg2).unwrap(), b"accept");
        let ct = session_encrypt(&conn, b"ready").unwrap();
        assert_eq!(srv.decrypt(&ct).unwrap(), b"ready");
        let ct = srv.encrypt(b"welcome").unwrap();
        assert_eq!(session_decrypt(&conn, &ct).unwrap(), b"welcome");
        assert!(session_decrypt(&conn, b"garbage-frame-garbage").is_err());
        assert!(session_encrypt(&conn, b"x").is_err(), "session dead after bad frame");
    }

    #[test]
    fn empty_pseudo_enrolls_without_user_name() {
        let key = ServerKey::generate(&mut Rng::os()).unwrap();
        let rec = login::register(&key.opaque, Role::Device, b"secret-pw", &mut Rng::os()).unwrap();
        let rec = login::parse_record(&rec).unwrap();
        let mut server = EnrollServer::new(&key, &rec);
        let (h, m1) = enroll_start(b"secret-pw", "Pixel", "  ").unwrap();
        let m2 = server.on_start(EnrollServer::parse(&m1).unwrap(), &mut Rng::os()).unwrap();
        let m3 = enroll_on_message(&h, &m2)["send"].as_str().unwrap().to_string();
        let verified = server.on_finish(EnrollServer::parse(&m3).unwrap()).unwrap();
        assert!(verified.user_name.is_none());
        let err =
            enroll_start(b"pw", "Pixel", "bad\u{7}name").err().expect("control characters refused");
        assert_eq!(error_json(&err)["error"]["code"], "malformed");
    }

    #[test]
    fn wrong_password_reports_error_code() {
        let key = ServerKey::generate(&mut Rng::os()).unwrap();
        let rec = login::register(&key.opaque, Role::Device, b"secret-pw", &mut Rng::os()).unwrap();
        let rec = login::parse_record(&rec).unwrap();
        let mut server = EnrollServer::new(&key, &rec);
        let (h, m1) = enroll_start(b"wrong-pw", "Pixel", "alice").unwrap();
        let m2 = server.on_start(EnrollServer::parse(&m1).unwrap(), &mut Rng::os()).unwrap();
        // Client cannot finish OPAQUE with a wrong password.
        let out = enroll_on_message(&h, &m2);
        assert_eq!(out["error"]["code"], "auth_failed");
        let rejected = EnrollServer::rejected("bad_password", "wrong password");
        let (h2, _) = enroll_start(b"x", "Pixel", "alice").unwrap();
        assert_eq!(enroll_on_message(&h2, &rejected)["error"]["code"], "bad_password");
    }
}
