//! The identity token of contract 11.3: a JWT signed with Ed25519 (`EdDSA`).
//!
//! `/auth/verify` gives one token to a service that it lets a user open. Traefik copies the token
//! to the request (`authResponseHeaders`). The service verifies the signature, `iss`, `aud` and
//! `exp`. The token identifies the human. It contains no permission: the service asks the PDP.
//!
//! The server keeps one token for each session and each audience. It gives the same token again
//! until half of its life is gone, so `/auth/verify` signs only a few times for each session.
//!
//! Interface: [`Signer`] (the key, from the configuration), [`Signer::token`] (a token, from the
//! cache or new), [`Signer::public_key_pem`] (for the services).

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Duration;

use aws_lc_rs::signature::{Ed25519KeyPair, KeyPair};
use base64ct::{Base64, Base64UrlUnpadded, Encoding};
use sha2::{Digest, Sha256};
use zeroize::Zeroize;

/// The DER prefix of an Ed25519 public key in a SubjectPublicKeyInfo (RFC 8410).
const SPKI_PREFIX: [u8; 12] = [
    0x30, 0x2a, 0x30, 0x05, 0x06, 0x03, 0x2b, 0x65, 0x70, 0x03, 0x21, 0x00,
];
/// The cache holds at most this many tokens. When it is full, the expired tokens go first, then all.
const MAX_CACHED_TOKENS: usize = 16_384;

/// The signing key and the claims that do not change.
pub struct Signer {
    key: Ed25519KeyPair,
    issuer: String,
    ttl: Duration,
    /// The JWK thumbprint of the public key (RFC 7638): the `kid` of each token.
    kid: String,
    /// `(session id, audience)` to `(token, expiry in Unix seconds)`.
    cache: Mutex<HashMap<(u64, String), (String, i64)>>,
}

impl Signer {
    /// Read the key: a PKCS#8 Ed25519 private key in PEM, or the base64 text of the PEM on one
    /// line. The error does not show the key.
    pub fn new(key_text: &[u8], issuer: &str, ttl: Duration) -> Result<Self, String> {
        let mut der = decode_key_text(key_text)?;
        let key = Ed25519KeyPair::from_pkcs8_maybe_unchecked(&der).map_err(|e| {
            format!("identity.signing_key: the value is not a PKCS#8 Ed25519 private key ({e})")
        });
        der.zeroize();
        let key = key?;
        let kid = thumbprint(key.public_key().as_ref());
        Ok(Self {
            key,
            issuer: issuer.to_string(),
            ttl,
            kid,
            cache: Mutex::new(HashMap::new()),
        })
    }

    pub fn kid(&self) -> &str {
        &self.kid
    }

    /// The public key as a PEM SubjectPublicKeyInfo, for `identity.public_keys_file` of a service.
    pub fn public_key_pem(&self) -> String {
        let mut der = SPKI_PREFIX.to_vec();
        der.extend_from_slice(self.key.public_key().as_ref());
        let body = Base64::encode_string(&der);
        format!("-----BEGIN PUBLIC KEY-----\n{body}\n-----END PUBLIC KEY-----\n")
    }

    /// The token of `subject` for the service `audience`, at the time `now` (Unix seconds).
    /// `session` is the id of the session: a new session never gets the token of an old one.
    pub fn token(&self, session: u64, subject: &str, audience: &str, now: i64) -> String {
        let ttl = self.ttl.as_secs() as i64;
        let key = (session, audience.to_string());
        let mut cache = self.cache.lock().unwrap_or_else(|p| p.into_inner());
        if let Some((token, exp)) = cache.get(&key) {
            // Half of the life is left: the service gets at least `ttl / 2` of use.
            if *exp - now >= (ttl + 1) / 2 {
                return token.clone();
            }
        }
        let exp = now + ttl;
        let token = self.sign(subject, audience, now, exp);
        if cache.len() >= MAX_CACHED_TOKENS {
            cache.retain(|_, (_, expiry)| *expiry > now);
            if cache.len() >= MAX_CACHED_TOKENS {
                cache.clear();
            }
        }
        cache.insert(key, (token.clone(), exp));
        token
    }

    /// A new token, not from the cache.
    pub fn sign(&self, subject: &str, audience: &str, iat: i64, exp: i64) -> String {
        let header = serde_json::json!({ "alg": "EdDSA", "typ": "JWT", "kid": self.kid });
        let claims = serde_json::json!({
            "iss": self.issuer,
            "sub": subject,
            "aud": audience,
            "iat": iat,
            "exp": exp,
        });
        let signing_input = format!(
            "{}.{}",
            Base64UrlUnpadded::encode_string(header.to_string().as_bytes()),
            Base64UrlUnpadded::encode_string(claims.to_string().as_bytes())
        );
        let signature = self.key.sign(signing_input.as_bytes());
        format!(
            "{signing_input}.{}",
            Base64UrlUnpadded::encode_string(signature.as_ref())
        )
    }

    #[cfg(test)]
    pub fn cached(&self) -> usize {
        self.cache.lock().unwrap_or_else(|p| p.into_inner()).len()
    }
}

/// The DER of the key: the PEM armor is optional, the base64 body is mandatory.
fn decode_key_text(text: &[u8]) -> Result<Vec<u8>, String> {
    let text = std::str::from_utf8(text)
        .map_err(|_| "identity.signing_key: the value is not text".to_string())?;
    let mut body = String::new();
    let mut lines = text.lines().map(str::trim).filter(|l| !l.is_empty()).peekable();
    let armored = lines.peek().is_some_and(|l| l.starts_with("-----"));
    if armored && lines.next() != Some("-----BEGIN PRIVATE KEY-----") {
        return Err(
            "identity.signing_key: expected -----BEGIN PRIVATE KEY----- (PKCS#8), the key is not shown"
                .into(),
        );
    }
    let mut closed = !armored;
    for line in lines {
        if line == "-----END PRIVATE KEY-----" && armored {
            closed = true;
            continue;
        }
        if closed && armored {
            return Err("identity.signing_key: text after -----END PRIVATE KEY-----".into());
        }
        body.push_str(line);
    }
    if !closed {
        return Err("identity.signing_key: -----END PRIVATE KEY----- is missing".into());
    }
    let der = Base64::decode_vec(&body)
        .map_err(|_| "identity.signing_key: the body is not valid base64".to_string());
    body.zeroize();
    der
}

/// RFC 7638 thumbprint of an Ed25519 public key (RFC 8037: `kty` `OKP`, `crv` `Ed25519`).
fn thumbprint(public_key: &[u8]) -> String {
    let jwk = format!(
        r#"{{"crv":"Ed25519","kty":"OKP","x":"{}"}}"#,
        Base64UrlUnpadded::encode_string(public_key)
    );
    Base64UrlUnpadded::encode_string(&Sha256::digest(jwk.as_bytes()))
}

#[cfg(test)]
pub mod testing {
    use aws_lc_rs::rand::SystemRandom;
    use aws_lc_rs::signature::Ed25519KeyPair;
    use base64ct::{Base64, Encoding};

    /// A new PKCS#8 key in PEM.
    pub fn new_key_pem() -> String {
        let document = Ed25519KeyPair::generate_pkcs8(&SystemRandom::new()).unwrap();
        format!(
            "-----BEGIN PRIVATE KEY-----\n{}\n-----END PRIVATE KEY-----\n",
            Base64::encode_string(document.as_ref())
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aws_lc_rs::signature::{UnparsedPublicKey, ED25519};

    /// A PKCS#8 version 1 key, the form that `openssl genpkey -algorithm ed25519` writes. The seed is 1 to 32.
    const OPENSSL_KEY: &str = "-----BEGIN PRIVATE KEY-----\nMC4CAQAwBQYDK2VwBCIEIAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8g\n-----END PRIVATE KEY-----\n";

    fn signer() -> Signer {
        Signer::new(OPENSSL_KEY.as_bytes(), "https://portal.test", Duration::from_secs(300)).unwrap()
    }

    fn decode(part: &str) -> serde_json::Value {
        serde_json::from_slice(&Base64UrlUnpadded::decode_vec(part).unwrap()).unwrap()
    }

    fn public_key(signer: &Signer) -> Vec<u8> {
        let pem = signer.public_key_pem();
        let body: String = pem.lines().filter(|l| !l.starts_with("-----")).collect();
        let der = Base64::decode_vec(&body).unwrap();
        assert_eq!(der[..12], SPKI_PREFIX);
        der[12..].to_vec()
    }

    #[test]
    fn a_token_has_the_claims_of_the_contract_and_a_valid_signature() {
        let signer = signer();
        let token = signer.token(7, "alice", "db", 1_000);
        let parts: Vec<&str> = token.split('.').collect();
        assert_eq!(parts.len(), 3);
        let header = decode(parts[0]);
        assert_eq!(header["alg"], "EdDSA");
        assert_eq!(header["typ"], "JWT");
        assert_eq!(header["kid"], signer.kid());
        let claims = decode(parts[1]);
        assert_eq!(claims["iss"], "https://portal.test");
        assert_eq!(claims["sub"], "alice");
        assert_eq!(claims["aud"], "db");
        assert_eq!(claims["iat"], 1_000);
        assert_eq!(claims["exp"], 1_300);
        assert_eq!(claims.as_object().unwrap().len(), 5, "no permission in the token");
        let signature = Base64UrlUnpadded::decode_vec(parts[2]).unwrap();
        let verifier = UnparsedPublicKey::new(&ED25519, public_key(&signer));
        let input = format!("{}.{}", parts[0], parts[1]);
        assert!(verifier.verify(input.as_bytes(), &signature).is_ok());
        assert!(verifier.verify(b"another input", &signature).is_err());
    }

    #[test]
    fn the_cache_gives_one_token_for_each_session_and_audience_until_half_of_its_life() {
        let signer = signer();
        let first = signer.token(1, "alice", "db", 1_000);
        assert_eq!(signer.token(1, "alice", "db", 1_150), first, "half of the life is left");
        assert_ne!(signer.token(1, "alice", "db", 1_151), first, "less than half is left");
        assert_ne!(signer.token(1, "alice", "chat", 1_000), first, "another audience");
        // Another session has its own entry. The signature is deterministic (Ed25519), so the
        // text is the same for the same claims.
        signer.token(2, "alice", "db", 1_000);
        assert_eq!(signer.cached(), 3);
    }

    #[test]
    fn the_key_text_is_a_pkcs8_pem_or_its_body_on_one_line() {
        let body: String = OPENSSL_KEY.lines().filter(|l| !l.starts_with("-----")).collect();
        let one_line = Signer::new(body.as_bytes(), "i", Duration::from_secs(60)).unwrap();
        assert_eq!(one_line.kid(), signer().kid());
        let generated = testing::new_key_pem();
        assert!(Signer::new(generated.as_bytes(), "i", Duration::from_secs(60)).is_ok());
        for bad in [
            "",
            "not base64 !",
            "-----BEGIN PUBLIC KEY-----\nMCowBQYDK2VwAyEA\n-----END PUBLIC KEY-----",
            "-----BEGIN PRIVATE KEY-----\nMC4CAQAwBQYDK2VwBCIEIAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8g",
            "MCowBQYDK2VwAyEA",
        ] {
            let error = Signer::new(bad.as_bytes(), "i", Duration::from_secs(60)).err().unwrap();
            assert!(error.starts_with("identity.signing_key"), "{error}");
            assert!(!error.contains("MC4CAQAw"), "the key is never shown");
        }
    }

    #[test]
    fn the_kid_is_the_rfc_7638_thumbprint() {
        // RFC 8037, appendix A.3: the thumbprint of the key of appendix A.2.
        let public = Base64UrlUnpadded::decode_vec("11qYAYKxCrfVS_7TyWQHOg7hcvPapiMlrwIaaPcHURo")
            .unwrap();
        assert_eq!(thumbprint(&public), "kPrK_qmxVWaYVA9wwBF6Iuo3vVzz7TxHCTwXBygrS4k");
    }
}
