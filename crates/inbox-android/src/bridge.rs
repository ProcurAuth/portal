//! JNI-free bridge logic, so it can be unit-tested on the host.
//!
//! Handles are `Box` pointers turned into `i64` for Java. Each handle owns a
//! `Mutex`, so a misbehaving caller using one handle from two threads cannot race.

use std::sync::Mutex;

use inbox_core::Error;
use inbox_core::b64;
use inbox_core::enroll::{EnrollCode, EnrollInitiator};
use inbox_core::noise::{self, Initiator, Keypair, Session};
use serde_json::{Value, json};

/// Error code and message surfaced to Kotlin as JSON `{"error": {...}}`.
pub fn error_json(e: &Error) -> Value {
    let code = match e {
        // Local validation of what the user typed.
        Error::Malformed("enroll code") => "bad_code",
        Error::Malformed("device name") => "bad_device_name",
        // Wrong server key, tampered or replayed message 2.
        Error::AuthenticationFailed => "auth_failed",
        Error::UnsupportedProtocol(_) | Error::Malformed(_) | Error::UnexpectedMessage(_) => {
            "protocol"
        }
        Error::Internal(_) => "internal",
    };
    json!({ "error": { "code": code, "message": e.to_string() } })
}

// ---------------------------------------------------------------- enrollment

/// Reads an enrollment code. Returns `{"code": {...}}` or `{"error": {...}}`.
/// The secret never reaches Kotlin.
pub fn enroll_parse(text: &str) -> Value {
    match EnrollCode::parse(text) {
        Ok(code) => json!({ "code": {
            "address": code.address,
            "server_public_key": b64::encode(&code.server_public_key),
            "fingerprint": code.fingerprint(),
        }}),
        Err(e) => error_json(&e),
    }
}

struct Pending {
    initiator: EnrollInitiator,
    device: Keypair,
    code: EnrollCode,
}

pub struct Enroll {
    pending: Mutex<Option<Pending>>,
}

/// Starts an enrollment with a fresh device key. Returns the handle and Noise message 1.
pub fn enroll_start(
    code_text: &str,
    device_name: &str,
    app_version: &str,
) -> Result<(Box<Enroll>, Vec<u8>), Error> {
    let code = EnrollCode::parse(code_text)?;
    let device = noise::generate_keypair()?;
    let (initiator, msg1) =
        EnrollInitiator::start(&device.private, &code, device_name, app_version)?;
    let pending = Pending { initiator, device, code };
    Ok((Box::new(Enroll { pending: Mutex::new(Some(pending)) }), msg1))
}

/// Consumes Noise message 2. Returns `{"done": {...}}` or `{"error": {...}}`.
/// The handle is exhausted afterwards, whatever the result.
pub fn enroll_finish(handle: &Enroll, msg2: &[u8]) -> Value {
    let mut guard = match handle.pending.lock() {
        Ok(g) => g,
        Err(p) => p.into_inner(),
    };
    let Some(p) = guard.take() else {
        return error_json(&Error::UnexpectedMessage("enrollment finished"));
    };
    match p.initiator.finish(msg2) {
        Ok(a) => json!({ "done": {
            "address": p.code.address,
            "device_id": a.device_id,
            "server_name": a.server_name,
            "user_name": a.user_name,
            "device_private_key": b64::encode(&p.device.private),
            "device_public_key": b64::encode(&p.device.public),
            "server_public_key": b64::encode(&p.code.server_public_key),
        }}),
        Err(e) => error_json(&e),
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
    use inbox_core::PROTOCOL_VERSION;
    use inbox_core::enroll::EnrollResponder;
    use inbox_core::noise::Responder;
    use inbox_core::proto::EnrollAccept;
    use inbox_core::server_key::ServerKey;

    fn code_text(key: &ServerKey) -> String {
        EnrollCode {
            address: "inbox.example.com:8080".into(),
            server_public_key: key.noise_public.clone().try_into().unwrap(),
            secret: [3u8; 16],
        }
        .encode()
    }

    #[test]
    fn enrollment_then_session_round_trip() {
        let key = ServerKey::generate().unwrap();
        let text = code_text(&key);

        let parsed = enroll_parse(&text);
        assert_eq!(parsed["code"]["address"], "inbox.example.com:8080");
        // 80 bits of the digest: 20 hexadecimal characters in five groups of 4.
        let fingerprint = parsed["code"]["fingerprint"].as_str().unwrap();
        assert_eq!(fingerprint, inbox_core::enroll::key_fingerprint(&key.noise_public));
        assert_eq!(fingerprint.len(), 24);
        assert_eq!(fingerprint.split(' ').filter(|g| g.len() == 4).count(), 5);
        assert!(!parsed.to_string().contains("secret"), "the secret stays in Rust");

        let (h, m1) = enroll_start(&text, "Pixel", "0.14.0").unwrap();
        let (resp, device_public, hello) = EnrollResponder::start(&key.noise_private, &m1).unwrap();
        assert_eq!(hello.secret, vec![3u8; 16]);
        assert_eq!(hello.device_name, "Pixel");
        let m2 = resp
            .finish(&EnrollAccept {
                protocol: PROTOCOL_VERSION,
                device_id: "0123456789abcdef0123456789abcdef".into(),
                server_name: "Prod".into(),
                user_name: "alice".into(),
            })
            .unwrap();
        let out = enroll_finish(&h, &m2);
        let done = &out["done"];
        assert_eq!(done["server_name"], "Prod");
        assert_eq!(done["user_name"], "alice");
        assert_eq!(done["address"], "inbox.example.com:8080");
        let sk = b64::decode(done["device_private_key"].as_str().unwrap()).unwrap();
        let spk = b64::decode(done["server_public_key"].as_str().unwrap()).unwrap();
        assert_eq!(
            b64::decode(done["device_public_key"].as_str().unwrap()).unwrap(),
            device_public
        );
        assert_eq!(spk, key.noise_public);
        // The handle is exhausted after completion.
        assert!(enroll_finish(&h, &m2).get("error").is_some());

        // The stored keys open a normal session.
        let (conn, msg1) = session_start(&sk, &spk, b"{\"hello\":1}").unwrap();
        let (resp, remote, hello) = Responder::start(&key.noise_private, &msg1).unwrap();
        assert_eq!(remote, device_public);
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

    /// The bridge moves bytes. It does not read the frame, so the field `kind` reaches Kotlin
    /// as the server wrote it, also a kind that this version does not know.
    #[test]
    fn the_field_kind_of_an_alert_frame_reaches_kotlin_unchanged() {
        let key = ServerKey::generate().unwrap();
        let (h, m1) = enroll_start(&code_text(&key), "Pixel", "1").unwrap();
        let (resp, device_public, _) = EnrollResponder::start(&key.noise_private, &m1).unwrap();
        let m2 = resp
            .finish(&EnrollAccept {
                protocol: PROTOCOL_VERSION,
                device_id: "0123456789abcdef0123456789abcdef".into(),
                server_name: "Prod".into(),
                user_name: "alice".into(),
            })
            .unwrap();
        let done = enroll_finish(&h, &m2);
        let sk = b64::decode(done["done"]["device_private_key"].as_str().unwrap()).unwrap();
        let (conn, msg1) = session_start(&sk, &key.noise_public, b"{}").unwrap();
        let (resp, remote, _) = Responder::start(&key.noise_private, &msg1).unwrap();
        assert_eq!(remote, device_public);
        let (mut srv, msg2) = resp.finish(b"accept").unwrap();
        session_finish(&conn, &msg2).unwrap();
        for kind in ["alert", "question"] {
            let frame = format!(
                r#"{{"t":"alert","alert_id":"ab","kind":"{kind}","group":"g","group_name":"G","title":"x","created_at_ms":1}}"#
            );
            let ct = srv.encrypt(frame.as_bytes()).unwrap();
            assert_eq!(session_decrypt(&conn, &ct).unwrap(), frame.as_bytes());
        }
    }

    #[test]
    fn bad_input_reports_error_codes() {
        let key = ServerKey::generate().unwrap();
        let text = code_text(&key);
        assert_eq!(enroll_parse("portal-inbox://add?url=x")["error"]["code"], "bad_code");
        let err = enroll_start("junk", "Pixel", "1").err().expect("bad code refused");
        assert_eq!(error_json(&err)["error"]["code"], "bad_code");
        let err = enroll_start(&text, "  ", "1").err().expect("empty name refused");
        assert_eq!(error_json(&err)["error"]["code"], "bad_device_name");
        let err = enroll_start(&text, "bad\u{7}name", "1").err().expect("control refused");
        assert_eq!(error_json(&err)["error"]["code"], "bad_device_name");
    }

    #[test]
    fn message_2_of_another_server_is_refused() {
        let key = ServerKey::generate().unwrap();
        let other = ServerKey::generate().unwrap();
        let (h, _) = enroll_start(&code_text(&key), "Pixel", "1").unwrap();
        let (h2, m1b) = enroll_start(&code_text(&other), "Pixel", "1").unwrap();
        let (resp, _, _) = EnrollResponder::start(&other.noise_private, &m1b).unwrap();
        let m2 = resp
            .finish(&EnrollAccept {
                protocol: PROTOCOL_VERSION,
                device_id: "0123456789abcdef0123456789abcdef".into(),
                server_name: "Evil".into(),
                user_name: "alice".into(),
            })
            .unwrap();
        assert_eq!(enroll_finish(&h, &m2)["error"]["code"], "auth_failed");
        assert_eq!(enroll_finish(&h2, &m2)["done"]["server_name"], "Evil");
    }
}
