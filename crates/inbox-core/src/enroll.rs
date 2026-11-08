//! Device enrollment with a one-time code (protocol 4).
//!
//! An account of the portal makes an [`EnrollCode`]. The person types it on the
//! phone, or taps it as a link. The phone then runs one Noise IK handshake to the
//! server key that the code holds (the pinned key):
//!
//! ```text
//! device                                      server
//!   Noise message 1 {secret, device_name, ..} ->
//!                                             <-  Noise message 2 {device_id, user_name, ..}
//! ```
//!
//! The phone is enrolled only after it reads message 2. Message 2 can come only from
//! the holder of the pinned server key. On refusal, the server sends no message 2.
//! It closes the WebSocket with [`crate::proto::close::ENROLL_REFUSED`].

use sha2::{Digest, Sha256};
use zeroize::Zeroize;

use crate::PROTOCOL_VERSION;
use crate::b64;
use crate::error::{Error, Result};
use crate::noise::{ENROLL_PROLOGUE, Initiator, Responder};
use crate::proto::{EnrollAccept, EnrollHello};

/// Length of the secret of a code, in bytes.
pub const SECRET_LEN: usize = 16;
/// Upper bound on the device name, in bytes.
pub const MAX_DEVICE_NAME: usize = 64;
/// Upper bound on the server address of a code, in bytes.
pub const MAX_ADDRESS: usize = 512;
/// Upper bound on the application version, in bytes.
pub const MAX_APP_VERSION: usize = 64;

const SCHEME: &str = "portal-inbox://enroll?";

/// What the person types or taps to enroll a phone.
///
/// Text form: `portal-inbox://enroll?s=<address>&k=<server key>&c=<secret>`. The address
/// is percent-encoded. The key and the secret are base64url without padding.
#[derive(Clone, PartialEq, Eq)]
pub struct EnrollCode {
    /// The address of the server, as the app accepts it: `host:port`, a domain or an http(s) URL.
    pub address: String,
    /// The pinned public key of the server (X25519).
    pub server_public_key: [u8; 32],
    /// The one-time secret.
    pub secret: [u8; SECRET_LEN],
}

impl std::fmt::Debug for EnrollCode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EnrollCode")
            .field("address", &self.address)
            .field("server_public_key", &b64::encode_url(&self.server_public_key))
            .field("secret", &"<hidden>")
            .finish()
    }
}

impl Drop for EnrollCode {
    fn drop(&mut self) {
        self.secret.zeroize();
    }
}

impl EnrollCode {
    /// The text form of the code.
    pub fn encode(&self) -> String {
        format!(
            "{SCHEME}s={}&k={}&c={}",
            percent_encode(&self.address),
            b64::encode_url(&self.server_public_key),
            b64::encode_url(&self.secret)
        )
    }

    /// Reads the text form. Surrounding white space is ignored. Any other deviation is an
    /// error: wrong scheme, unknown or duplicate or missing key, bad length, bad base64,
    /// bad percent-encoding, fragment.
    pub fn parse(text: &str) -> Result<EnrollCode> {
        const BAD: Error = Error::Malformed("enroll code");
        let query = text.trim().strip_prefix(SCHEME).ok_or(BAD)?;
        let (mut address, mut key, mut secret) = (None, None, None);
        for pair in query.split('&') {
            let (name, value) = pair.split_once('=').ok_or(BAD)?;
            let slot = match name {
                "s" => &mut address,
                "k" => &mut key,
                "c" => &mut secret,
                _ => return Err(BAD),
            };
            if slot.replace(value).is_some() {
                return Err(BAD);
            }
        }
        let address = percent_decode(address.ok_or(BAD)?).ok_or(BAD)?;
        if address.is_empty()
            || address.len() > MAX_ADDRESS
            || address.chars().any(|c| c.is_control() || c.is_whitespace())
        {
            return Err(BAD);
        }
        let server_public_key = decode_fixed::<32>(key.ok_or(BAD)?).ok_or(BAD)?;
        let secret = decode_fixed::<SECRET_LEN>(secret.ok_or(BAD)?).ok_or(BAD)?;
        Ok(EnrollCode { address, server_public_key, secret })
    }

    /// The fingerprint of the pinned key (80 bits, see [`key_fingerprint`]), for a person to compare.
    pub fn fingerprint(&self) -> String {
        key_fingerprint(&self.server_public_key)
    }
}

/// The bytes of the SHA-256 digest that the fingerprint shows: 80 bits. A different key with the
/// same fingerprint costs about 2^80 digests.
const FINGERPRINT_BYTES: usize = 10;

/// The first 20 hexadecimal characters (80 bits, lower case) of the SHA-256 digest of a public
/// key, in five groups of 4 characters with one space between them.
pub fn key_fingerprint(public_key: &[u8]) -> String {
    let digest = Sha256::digest(public_key);
    let groups: Vec<String> = digest[..FINGERPRINT_BYTES]
        .chunks(2)
        .map(|pair| pair.iter().map(|b| format!("{b:02x}")).collect())
        .collect();
    groups.join(" ")
}

fn decode_fixed<const N: usize>(text: &str) -> Option<[u8; N]> {
    b64::decode_url(text)?.try_into().ok()
}

fn percent_encode(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for b in text.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

fn percent_decode(text: &str) -> Option<String> {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let hex = text.get(i + 1..i + 3)?;
            if !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
                return None;
            }
            out.push(u8::from_str_radix(hex, 16).ok()?);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

/// Trims a device name typed by a person and checks it.
pub fn validate_device_name(name: &str) -> Result<String> {
    let name = name.trim();
    check_label(name, MAX_DEVICE_NAME)
        .then(|| name.to_string())
        .ok_or(Error::Malformed("device name"))
}

fn check_label(text: &str, max: usize) -> bool {
    !text.is_empty() && text.len() <= max && !text.chars().any(char::is_control)
}

fn is_hex_id(text: &str) -> bool {
    text.len() == 32 && text.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn check_accept(accept: &EnrollAccept) -> Result<()> {
    if accept.protocol != PROTOCOL_VERSION {
        return Err(Error::UnsupportedProtocol(accept.protocol));
    }
    if !is_hex_id(&accept.device_id) {
        return Err(Error::Malformed("device id"));
    }
    if accept.server_name.is_empty() || accept.user_name.is_empty() {
        return Err(Error::Malformed("enroll accept"));
    }
    Ok(())
}

/// Device side. One instance for each connection.
pub struct EnrollInitiator {
    inner: Initiator,
}

impl EnrollInitiator {
    /// Returns the initiator and Noise message 1. `device_private` is a fresh X25519 key
    /// that the phone made. The device name is trimmed and checked.
    pub fn start(
        device_private: &[u8],
        code: &EnrollCode,
        device_name: &str,
        app_version: &str,
    ) -> Result<(EnrollInitiator, Vec<u8>)> {
        let hello = EnrollHello {
            protocol: PROTOCOL_VERSION,
            secret: code.secret.to_vec(),
            device_name: validate_device_name(device_name)?,
            app_version: app_version.trim().to_string(),
        };
        let mut json =
            serde_json::to_vec(&hello).map_err(|_| Error::Internal("enroll hello encoding"))?;
        let started = Initiator::start_with_prologue(
            ENROLL_PROLOGUE,
            device_private,
            &code.server_public_key,
            &json,
        );
        json.zeroize();
        let (inner, msg1) = started?;
        Ok((EnrollInitiator { inner }, msg1))
    }

    /// Reads Noise message 2. It fails unless the pinned server key made the message
    /// for this handshake. A message of an earlier handshake fails too.
    pub fn finish(self, message: &[u8]) -> Result<EnrollAccept> {
        let (_session, payload) = self.inner.finish(message)?;
        let accept: EnrollAccept =
            serde_json::from_slice(&payload).map_err(|_| Error::Malformed("enroll accept"))?;
        check_accept(&accept)?;
        Ok(accept)
    }
}

/// Server side. One instance for each connection.
pub struct EnrollResponder {
    inner: Responder,
}

impl EnrollResponder {
    /// Reads Noise message 1. Returns the responder, the public key of the device and
    /// the checked hello. The server must then check the secret.
    pub fn start(
        server_private: &[u8],
        message: &[u8],
    ) -> Result<(EnrollResponder, Vec<u8>, EnrollHello)> {
        let (inner, device_public, mut payload) =
            Responder::start_with_prologue(ENROLL_PROLOGUE, server_private, message)?;
        let hello: std::result::Result<EnrollHello, _> = serde_json::from_slice(&payload);
        payload.zeroize();
        let hello = hello.map_err(|_| Error::Malformed("enroll hello"))?;
        if hello.protocol != PROTOCOL_VERSION {
            return Err(Error::UnsupportedProtocol(hello.protocol));
        }
        if hello.secret.len() != SECRET_LEN {
            return Err(Error::Malformed("secret"));
        }
        if !check_label(&hello.device_name, MAX_DEVICE_NAME) {
            return Err(Error::Malformed("device name"));
        }
        if !check_label(&hello.app_version, MAX_APP_VERSION) {
            return Err(Error::Malformed("app version"));
        }
        Ok((EnrollResponder { inner }, device_public, hello))
    }

    /// Returns Noise message 2 that carries `accept`. Call it only after the server stored the device.
    pub fn finish(self, accept: &EnrollAccept) -> Result<Vec<u8>> {
        check_accept(accept)?;
        let json =
            serde_json::to_vec(accept).map_err(|_| Error::Internal("enroll accept encoding"))?;
        let (_session, msg2) = self.inner.finish(&json)?;
        Ok(msg2)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::noise::generate_keypair;

    const DEVICE_ID: &str = "0123456789abcdef0123456789abcdef";

    fn code_for(server_public: &[u8]) -> EnrollCode {
        EnrollCode {
            address: "https://inbox.example.com:8443/prefix".into(),
            server_public_key: server_public.try_into().unwrap(),
            secret: [7u8; SECRET_LEN],
        }
    }

    fn accept() -> EnrollAccept {
        EnrollAccept {
            protocol: PROTOCOL_VERSION,
            device_id: DEVICE_ID.into(),
            server_name: "Prod".into(),
            user_name: "alice".into(),
        }
    }

    #[test]
    fn code_round_trip() {
        let code = code_for(&[9u8; 32]);
        let text = code.encode();
        assert!(text.starts_with(
            "portal-inbox://enroll?s=https%3A%2F%2Finbox.example.com%3A8443%2Fprefix&k="
        ));
        assert_eq!(EnrollCode::parse(&text).unwrap(), code);
        assert_eq!(EnrollCode::parse(&format!("  {text}\n")).unwrap(), code);
        // Key order in the query does not matter.
        let mixed = format!(
            "portal-inbox://enroll?c={}&s=host%3A80&k={}",
            b64::encode_url(&code.secret),
            b64::encode_url(&code.server_public_key)
        );
        assert_eq!(EnrollCode::parse(&mixed).unwrap().address, "host:80");
        // A non-ASCII address survives.
        let mut other = code_for(&[1u8; 32]);
        other.address = "pagé.example".into();
        assert_eq!(EnrollCode::parse(&other.encode()).unwrap(), other);
    }

    #[test]
    fn code_rejections() {
        let good = code_for(&[9u8; 32]).encode();
        let k = b64::encode_url(&[9u8; 32]);
        let c = b64::encode_url(&[7u8; 16]);
        let bad = [
            String::new(),
            "portal-inbox://enroll".to_string(),
            "portal-inbox://enroll?".to_string(),
            good.replace("portal-inbox://", "https://"),
            good.replace("portal-inbox://enroll", "portal-inbox://add"),
            good.to_uppercase(),
            format!("{good}&x=1"),
            format!("{good}&s=other"),
            format!("{good}&k={k}"),
            format!("{good}&c={c}"),
            format!("{good}#frag"),
            format!("{good}&"),
            format!("portal-inbox://enroll?k={k}&c={c}"),
            format!("portal-inbox://enroll?s=host&c={c}"),
            format!("portal-inbox://enroll?s=host&k={k}"),
            format!("portal-inbox://enroll?s=&k={k}&c={c}"),
            format!("portal-inbox://enroll?s=host&k={}&c={c}", b64::encode_url(&[9u8; 31])),
            format!("portal-inbox://enroll?s=host&k={k}&c={}", b64::encode_url(&[7u8; 17])),
            format!("portal-inbox://enroll?s=host&k={k}=&c={c}"),
            format!("portal-inbox://enroll?s=host&k={k}&c={c}="),
            format!("portal-inbox://enroll?s=host&k=+{}&c={c}", &k[1..]),
            format!("portal-inbox://enroll?s=host&k=!!&c={c}"),
            format!("portal-inbox://enroll?s=ho%zzst&k={k}&c={c}"),
            format!("portal-inbox://enroll?s=ho%2&k={k}&c={c}"),
            format!("portal-inbox://enroll?s=ho%FFst&k={k}&c={c}"),
            format!("portal-inbox://enroll?s=ho%20st&k={k}&c={c}"),
            format!("portal-inbox://enroll?s=ho%00st&k={k}&c={c}"),
            format!("portal-inbox://enroll?s={}&k={k}&c={c}", "a".repeat(MAX_ADDRESS + 1)),
            format!("portal-inbox://enroll?s=host&&k={k}&c={c}"),
        ];
        for text in bad {
            assert!(EnrollCode::parse(&text).is_err(), "must be refused: {text}");
        }
        // A last base64 character with non-zero trailing bits is not canonical.
        let loose = format!("portal-inbox://enroll?s=host&k={k}&c={}", "A".repeat(21) + "B");
        assert!(EnrollCode::parse(&loose).is_err());
    }

    #[test]
    fn code_hides_secret_in_debug() {
        let text = format!("{:?}", code_for(&[9u8; 32]));
        assert!(!text.contains(&b64::encode_url(&[7u8; 16])));
        assert!(text.contains("hidden"));
    }

    #[test]
    fn fingerprint_is_eighty_bits_of_sha256_in_groups_of_four() {
        let code = code_for(&[0u8; 32]);
        // SHA-256 of 32 zero bytes: 66687aadf862bd776c8fc18b8e9f8e20089714856ee233b3902a591d0d5f2925
        assert_eq!(code.fingerprint(), "6668 7aad f862 bd77 6c8f");
        assert_eq!(key_fingerprint(&[0u8; 32]), "6668 7aad f862 bd77 6c8f");
        let hex: String = code.fingerprint().split(' ').collect();
        assert_eq!(hex.len() * 4, 80, "the fingerprint shows 80 bits");
        // The digests of these two keys share their first 32 bits (93613343), the old fingerprint
        // of 8 hexadecimal characters. A search of about 2^16 keys found them. The fingerprint of
        // 80 bits tells them apart.
        let key = |n: u16| {
            let mut key = [0u8; 32];
            key[30..].copy_from_slice(&n.to_be_bytes());
            key
        };
        assert_eq!(key_fingerprint(&key(50323)), "9361 3343 0b8c 6a2b c988");
        assert_eq!(key_fingerprint(&key(54260)), "9361 3343 acd6 e90c 30a9");
    }

    #[test]
    fn enrollment_round_trip() {
        let server = generate_keypair().unwrap();
        let device = generate_keypair().unwrap();
        let code = code_for(&server.public);
        let (init, m1) =
            EnrollInitiator::start(&device.private, &code, "  Pixel de garde ", "0.14.0").unwrap();
        let (resp, device_public, hello) = EnrollResponder::start(&server.private, &m1).unwrap();
        assert_eq!(device_public, device.public);
        assert_eq!(hello.protocol, 5);
        assert_eq!(hello.secret, code.secret);
        assert_eq!(hello.device_name, "Pixel de garde");
        assert_eq!(hello.app_version, "0.14.0");
        let m2 = resp.finish(&accept()).unwrap();
        assert_eq!(init.finish(&m2).unwrap(), accept());
    }

    #[test]
    fn wire_payloads_match_the_contract() {
        let hello = EnrollHello {
            protocol: 5,
            secret: vec![0u8; 16],
            device_name: "p".into(),
            app_version: "1".into(),
        };
        assert_eq!(
            serde_json::to_string(&hello).unwrap(),
            r#"{"protocol":5,"secret":"AAAAAAAAAAAAAAAAAAAAAA==","device_name":"p","app_version":"1"}"#
        );
        assert_eq!(
            serde_json::to_string(&accept()).unwrap(),
            format!(
                r#"{{"protocol":5,"device_id":"{DEVICE_ID}","server_name":"Prod","user_name":"alice"}}"#
            )
        );
        assert!(serde_json::from_str::<EnrollAccept>(&format!(
            r#"{{"protocol":5,"device_id":"{DEVICE_ID}","server_name":"P","user_name":"u","x":1}}"#
        ))
        .is_err());
    }

    #[test]
    fn wrong_pinned_key_is_rejected_on_both_sides() {
        let server = generate_keypair().unwrap();
        let other = generate_keypair().unwrap();
        let device = generate_keypair().unwrap();
        // Server side: the code pins another key, so the server cannot read message 1.
        let (_, m1) =
            EnrollInitiator::start(&device.private, &code_for(&other.public), "phone", "1")
                .unwrap();
        assert!(EnrollResponder::start(&server.private, &m1).is_err());
        // Device side: a server with another key answers a handshake of its own.
        // The device pinned to the real key refuses that message 2.
        let (init, _) =
            EnrollInitiator::start(&device.private, &code_for(&server.public), "phone", "1")
                .unwrap();
        let (resp, _, _) = EnrollResponder::start(&other.private, &m1).unwrap();
        let forged = resp.finish(&accept()).unwrap();
        assert_eq!(init.finish(&forged).err(), Some(Error::AuthenticationFailed));
    }

    #[test]
    fn tampered_messages_are_rejected() {
        let server = generate_keypair().unwrap();
        let device = generate_keypair().unwrap();
        let code = code_for(&server.public);
        let (init, m1) = EnrollInitiator::start(&device.private, &code, "phone", "1").unwrap();
        let mut bad1 = m1.clone();
        let last = bad1.len() - 1;
        bad1[last] ^= 1;
        assert!(EnrollResponder::start(&server.private, &bad1).is_err());
        let (resp, _, _) = EnrollResponder::start(&server.private, &m1).unwrap();
        let mut m2 = resp.finish(&accept()).unwrap();
        m2[40] ^= 1;
        assert_eq!(init.finish(&m2).err(), Some(Error::AuthenticationFailed));
    }

    #[test]
    fn replayed_message_2_is_rejected() {
        let server = generate_keypair().unwrap();
        let code = code_for(&server.public);
        let run = |device: &crate::noise::Keypair| {
            let (init, m1) = EnrollInitiator::start(&device.private, &code, "phone", "1").unwrap();
            let (resp, _, _) = EnrollResponder::start(&server.private, &m1).unwrap();
            (init, resp.finish(&accept()).unwrap())
        };
        let (_, old_m2) = run(&generate_keypair().unwrap());
        // Same device key, same code, new handshake: the old message 2 does not fit.
        let device = generate_keypair().unwrap();
        let (_, first_m2) = run(&device);
        let (second, _) = run(&device);
        assert_eq!(second.finish(&first_m2).err(), Some(Error::AuthenticationFailed));
        let (third, _) = run(&generate_keypair().unwrap());
        assert_eq!(third.finish(&old_m2).err(), Some(Error::AuthenticationFailed));
    }

    #[test]
    fn session_prologue_is_not_accepted_for_enrollment() {
        let server = generate_keypair().unwrap();
        let device = generate_keypair().unwrap();
        let (_, m1) = Initiator::start(&device.private, &server.public, b"{}").unwrap();
        assert!(EnrollResponder::start(&server.private, &m1).is_err());
    }

    #[test]
    fn responder_checks_the_hello() {
        let server = generate_keypair().unwrap();
        let device = generate_keypair().unwrap();
        let send = |json: &str| {
            let (_, m1) = Initiator::start_with_prologue(
                ENROLL_PROLOGUE,
                &device.private,
                &server.public,
                json.as_bytes(),
            )
            .unwrap();
            EnrollResponder::start(&server.private, &m1).map(|(_, _, h)| h)
        };
        let secret = "AAAAAAAAAAAAAAAAAAAAAA==";
        let ok =
            format!(r#"{{"protocol":5,"secret":"{secret}","device_name":"p","app_version":"1"}}"#);
        assert!(send(&ok).is_ok());
        assert_eq!(
            send(&ok.replace(r#""protocol":5"#, r#""protocol":4"#)).err(),
            Some(Error::UnsupportedProtocol(4))
        );
        assert!(send(&ok.replace(secret, "AAAA")).is_err());
        assert!(send(&ok.replace(r#""device_name":"p""#, r#""device_name":"""#)).is_err());
        assert!(send(&ok.replace(r#""device_name":"p""#, r#""device_name":"a\u0007b""#)).is_err());
        let long = "x".repeat(MAX_DEVICE_NAME + 1);
        assert!(
            send(&ok.replace(r#""device_name":"p""#, &format!(r#""device_name":"{long}""#)))
                .is_err()
        );
        assert!(send(&ok.replace("}", r#","extra":1}"#)).is_err());
        assert!(send("not json").is_err());
    }

    #[test]
    fn initiator_checks_the_inputs_and_the_accept() {
        let server = generate_keypair().unwrap();
        let device = generate_keypair().unwrap();
        let code = code_for(&server.public);
        assert_eq!(
            EnrollInitiator::start(&device.private, &code, "  ", "1").err(),
            Some(Error::Malformed("device name"))
        );
        assert!(EnrollInitiator::start(&device.private, &code, "a\u{7}", "1").is_err());
        assert!(EnrollInitiator::start(&device.private, &code, &"x".repeat(65), "1").is_err());
        assert!(EnrollInitiator::start(&device.private[..31], &code, "p", "1").is_err());
        // The server side cannot send a bad accept either.
        let (_, m1) = EnrollInitiator::start(&device.private, &code, "p", "1").unwrap();
        let (resp, _, _) = EnrollResponder::start(&server.private, &m1).unwrap();
        let mut bad = accept();
        bad.device_id = "ABC".into();
        assert!(resp.finish(&bad).is_err());
    }
}
