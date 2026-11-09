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

// ---------------------------------------------------------------- effect and view

fn effect_error_json(e: &inbox_core::effect::EffectError) -> Value {
    json!({ "error": { "code": e.code(), "message": e.reason() } })
}

/// The length of a Noise public key in bytes.
const PUBLIC_KEY_LEN: usize = 32;

/// The fingerprint of a pinned server key: the same function as the one that the enrollment code
/// uses (`inbox_core::enroll::key_fingerprint`), so the rule exists once. Five groups of 4 lower
/// case hexadecimal characters with one space between them. A key of another length is refused.
pub fn fingerprint(public_key: &[u8]) -> Result<String, String> {
    if public_key.len() != PUBLIC_KEY_LEN {
        return Err(format!("a public key has {PUBLIC_KEY_LEN} bytes, not {}", public_key.len()));
    }
    Ok(inbox_core::enroll::key_fingerprint(public_key))
}

/// The effect of an operation (protocol 17.3). `op_json` is the JSON text of `op`, read with the
/// strict rules (no float, no duplicate member name). Returns `{"effect": "sha256:..."}` or
/// `{"error": {"code": "invalid_op", ...}}`.
pub fn effect_compute(service: &str, action: &str, resource: &str, op_json: &str) -> Value {
    match inbox_core::effect::effect_hash_text(service, action, resource, op_json) {
        Ok(effect) => json!({ "effect": effect }),
        Err(e) => effect_error_json(&e),
    }
}

/// The view of an approval with the values of `op`. Returns
/// `{"view": {"title", "fields": [{"label", "value"}], "body", "body_label"?}}` or
/// `{"error": {"code": "invalid_op" | "invalid_view", ...}}`.
pub fn view_resolve(op_json: &str, view_json: &str) -> Value {
    use inbox_core::effect::{Op, parse_view, resolve_view};
    let op = match Op::parse(op_json) {
        Ok(op) => op,
        Err(e) => return effect_error_json(&e),
    };
    let view = match parse_view(view_json) {
        Ok(v) => v,
        Err(e) => return effect_error_json(&e),
    };
    match resolve_view(op.as_value(), &view) {
        Ok(v) => json!({ "view": v }),
        Err(e) => effect_error_json(&e),
    }
}

/// The tree of a Markdown text, as JSON text. It never fails: over a limit the tree is one `text` block with
/// `"fallback": true`. The shape is the serialization of `inbox_core::render::markdown::Doc`.
pub fn markdown_parse(text: &str) -> String {
    serde_json::to_string(&inbox_core::render::markdown::parse(text))
        .unwrap_or_else(|_| r#"{"fallback":true,"blocks":[]}"#.to_string())
}

/// The tokens of a code text, as a JSON array `[[start, end, "class"], ...]`. The offsets are in UTF-16 code units,
/// as Java and Kotlin index a string. The tokens cover the whole text. A language that is not in the closed list gives
/// `plain`.
pub fn highlight_tokens(language: &str, text: &str) -> String {
    use inbox_core::render::highlight::{Language, tokens_utf16};
    let lang = Language::parse(language).unwrap_or(Language::Plain);
    let rows: Vec<Value> = tokens_utf16(lang, text)
        .into_iter()
        .map(|(start, end, class)| json!([start, end, class.as_str()]))
        .collect();
    Value::Array(rows).to_string()
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

    /// The golden vectors of `inbox-core` (`enroll.rs`). The Kotlin test `FingerprintTest` holds
    /// the same strings: the two languages must give the same text.
    #[test]
    fn fingerprint_of_a_key_has_the_golden_value_and_refuses_a_wrong_length() {
        assert_eq!(fingerprint(&[0u8; 32]).unwrap(), "6668 7aad f862 bd77 6c8f");
        let key = |n: u16| {
            let mut key = [0u8; 32];
            key[30..].copy_from_slice(&n.to_be_bytes());
            key
        };
        assert_eq!(fingerprint(&key(50323)).unwrap(), "9361 3343 0b8c 6a2b c988");
        assert_eq!(fingerprint(&key(54260)).unwrap(), "9361 3343 acd6 e90c 30a9");
        let server = ServerKey::generate().unwrap();
        assert_eq!(
            fingerprint(&server.noise_public).unwrap(),
            enroll_parse(&code_text(&server))["code"]["fingerprint"].as_str().unwrap(),
            "the enrollment code and the stored key give the same text"
        );
        assert!(fingerprint(&[0u8; 31]).is_err());
        assert!(fingerprint(&[0u8; 33]).is_err());
        assert!(fingerprint(&[]).is_err());
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

    #[test]
    fn effect_and_view_reach_kotlin_as_json() {
        let op = r#"{"sql":"DELETE 1","host":"prod","n":2}"#;
        let r = effect_compute("datastore", "data.write", "host:prod", op);
        let effect = r["effect"].as_str().unwrap();
        assert!(inbox_core::effect::is_effect(effect), "{r}");
        // Same hash for the other order of the members.
        let reordered = r#"{"n":2,"host":"prod","sql":"DELETE 1"}"#;
        assert_eq!(effect_compute("datastore", "data.write", "host:prod", reordered), r);
        // A float and a duplicate member are refused.
        assert_eq!(effect_compute("s", "a", "r", r#"{"x":1.5}"#)["error"]["code"], "invalid_op");
        assert_eq!(
            effect_compute("s", "a", "r", r#"{"x":1,"x":2}"#)["error"]["code"],
            "invalid_op"
        );
        let view = r#"{"title":"Write","fields":[["Host","/host"],["N","/n"]],"body":"/sql","body_label":"SQL"}"#;
        let v = view_resolve(op, view);
        assert_eq!(v["view"]["body"], "DELETE 1");
        assert_eq!(v["view"]["fields"][0], json!({"label": "Host", "value": "prod"}));
        assert_eq!(v["view"]["fields"][1]["value"], "2");
        let missing = r#"{"title":"Write","fields":[],"body":"/nope"}"#;
        assert_eq!(view_resolve(op, missing)["error"]["code"], "invalid_view");
        assert_eq!(view_resolve(op, "not json")["error"]["code"], "invalid_view");
        assert_eq!(view_resolve("[1]", view)["error"]["code"], "invalid_op");
    }

    #[test]
    fn markdown_and_highlight_bridges() {
        let tree: Value =
            serde_json::from_str(&markdown_parse("a **b**\n\n```sql\nselect 1\n```")).unwrap();
        assert_eq!(tree["blocks"][0]["t"], "paragraph");
        assert_eq!(tree["blocks"][1]["lang"], "sql");
        assert!(tree.get("fallback").is_none());
        let long: Value = serde_json::from_str(&markdown_parse(&"x".repeat(9000))).unwrap();
        assert_eq!(long["fallback"], true);
        // Offsets are UTF-16 units: the emoji counts for two.
        let rows: Value =
            serde_json::from_str(&highlight_tokens("sql", "select '\u{1f600}' -- c")).unwrap();
        let rows = rows.as_array().unwrap();
        assert_eq!(rows[0], json!([0, 6, "keyword"]));
        assert!(rows.iter().any(|r| r[2] == "string" && r[1].as_u64() == Some(11)), "{rows:?}");
        assert_eq!(rows.last().unwrap()[2], "comment");
        assert_eq!(rows.last().unwrap()[1].as_u64(), Some(16));
        // An unknown language gives one plain token, an empty text gives none.
        assert_eq!(highlight_tokens("cobol", "x y"), r#"[[0,3,"plain"]]"#);
        assert_eq!(highlight_tokens("rust", ""), "[]");
    }
}
