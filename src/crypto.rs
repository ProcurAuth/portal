use argon2::{
    password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString},
    Algorithm, Argon2, Params, Version,
};
use base64ct::{Base64UrlUnpadded, Encoding};
use hmac::{Hmac, Mac};
use sha2::Sha256;
use zeroize::Zeroize;

use crate::util::constant_time_eq;

type HmacSha256 = Hmac<Sha256>;

pub fn verify_password(password: &[u8], hash: &str) -> bool {
    let parsed = match PasswordHash::new(hash) {
        Ok(v) => v,
        Err(_) => return false,
    };
    Argon2::default().verify_password(password, &parsed).is_ok()
}

pub fn hash_password(password: &[u8]) -> Result<String, String> {
    if password.is_empty() {
        return Err("password must not be empty".into());
    }
    let mut salt_bytes = [0u8; 16];
    fill_random(&mut salt_bytes)?;
    let salt = SaltString::encode_b64(&salt_bytes)
        .map_err(|e| format!("cannot encode password salt: {e}"))?;
    salt_bytes.zeroize();

    let params = Params::new(19_456, 2, 1, None)
        .map_err(|e| format!("cannot configure Argon2: {e}"))?;
    let argon2 = Argon2::new(Algorithm::Argon2id, Version::V0x13, params);
    argon2
        .hash_password(password, &salt)
        .map(|v| v.to_string())
        .map_err(|e| format!("password hashing failed: {e}"))
}

pub fn generate_session_secret() -> Result<Vec<u8>, String> {
    let mut secret = vec![0u8; 32];
    if let Err(error) = fill_random(&mut secret) {
        secret.zeroize();
        return Err(error);
    }
    Ok(secret)
}

pub fn new_csrf_token() -> Result<String, String> {
    let mut raw = [0u8; 24];
    fill_random(&mut raw)?;
    let token = Base64UrlUnpadded::encode_string(&raw);
    raw.zeroize();
    Ok(token)
}

pub fn verify_csrf(cookie: &str, form: &str) -> bool {
    if cookie.len() < 16 || cookie.len() > 128 || form.len() != cookie.len() {
        return false;
    }
    constant_time_eq(cookie.as_bytes(), form.as_bytes())
}

/// The anti-CSRF token of a button on a page that stays open (sign out, account administration):
/// bound to the session it acts for and to what it is for (`purpose`), so it stays valid as long
/// as the page does (a cookie-based token would expire under an idle tab) and cannot be replayed
/// on another action.
pub fn sign_action_token(secret: &[u8], purpose: &str, session_token: &str) -> Result<String, String> {
    let mac = sign(secret, format!("{purpose}\n{session_token}").as_bytes())?;
    Ok(Base64UrlUnpadded::encode_string(&mac))
}

pub fn verify_action_token(secret: &[u8], purpose: &str, session_token: &str, token: &str) -> bool {
    let mac = match Base64UrlUnpadded::decode_vec(token) {
        Ok(v) if v.len() == 32 => v,
        _ => return false,
    };
    verify_mac(secret, format!("{purpose}\n{session_token}").as_bytes(), &mac)
}

pub fn sign_return_uri(secret: &[u8], uri: &str) -> Result<String, String> {
    let mac = sign(secret, format!("return\n{uri}").as_bytes())?;
    Ok(Base64UrlUnpadded::encode_string(&mac))
}

pub fn verify_return_uri_signature(secret: &[u8], uri: &str, signature: &str) -> bool {
    let mac = match Base64UrlUnpadded::decode_vec(signature) {
        Ok(v) if v.len() == 32 => v,
        _ => return false,
    };
    verify_mac(secret, format!("return\n{uri}").as_bytes(), &mac)
}

fn sign(secret: &[u8], data: &[u8]) -> Result<[u8; 32], String> {
    let mut mac = HmacSha256::new_from_slice(secret)
        .map_err(|_| "invalid HMAC secret length".to_string())?;
    mac.update(data);
    let bytes = mac.finalize().into_bytes();
    let mut out = [0u8; 32];
    out.copy_from_slice(&bytes);
    Ok(out)
}

fn verify_mac(secret: &[u8], data: &[u8], expected: &[u8]) -> bool {
    let mut mac = match HmacSha256::new_from_slice(secret) {
        Ok(v) => v,
        Err(_) => return false,
    };
    mac.update(data);
    mac.verify_slice(expected).is_ok()
}

fn fill_random(buf: &mut [u8]) -> Result<(), String> {
    getrandom::fill(buf).map_err(|e| format!("OS random generator failed: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn csrf_comparison() {
        assert!(verify_csrf("abcdefghijklmnop", "abcdefghijklmnop"));
        assert!(!verify_csrf("abcdefghijklmnop", "abcdefghijklmnoq"));
    }

    #[test]
    fn action_token_is_bound_to_its_session_and_its_purpose() {
        let key = b"01234567890123456789012345678901";
        let token = sign_action_token(key, "logout", "session-a").unwrap();
        assert!(verify_action_token(key, "logout", "session-a", &token));
        assert!(!verify_action_token(key, "logout", "session-b", &token));
        assert!(!verify_action_token(key, "users", "session-a", &token), "another purpose");
        assert!(!verify_action_token(b"another-key-another-key-another!", "logout", "session-a", &token));
        assert!(!verify_action_token(key, "logout", "session-a", "short"));
        let uri_signature = sign_return_uri(key, "session-a").unwrap();
        assert!(!verify_action_token(key, "logout", "session-a", &uri_signature), "domains are separated");
    }

    #[test]
    fn return_uri_signature_roundtrip() {
        let key = b"01234567890123456789012345678901";
        let uri = "/grafana/d/a?x=1";
        let sig = sign_return_uri(key, uri).unwrap();
        assert!(verify_return_uri_signature(key, uri, &sig));
        assert!(!verify_return_uri_signature(key, "/other", &sig));
    }
}
