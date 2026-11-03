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

    let params =
        Params::new(19_456, 2, 1, None).map_err(|e| format!("cannot configure Argon2: {e}"))?;
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

/// The shortest time between the display of the sign-in form and its submit, in milliseconds. A
/// person cannot read the form and type a login and a password in less time.
pub const FORM_MIN_AGE_MS: u64 = 1_000;

/// The longest age of a sign-in form, in milliseconds. It is the life of the anti-CSRF cookie.
pub const FORM_MAX_AGE_MS: u64 = 12 * 3600 * 1000;

/// The result of the check of the token of a sign-in form.
#[derive(Debug, PartialEq, Eq)]
pub enum FormToken {
    /// The server signed the token for this anti-CSRF token, and its age is in the limits.
    Valid,
    /// The token is missing, damaged, signed for another anti-CSRF token, or too new.
    /// A script that posts at once, and a script that did not read the form, give this result.
    Refused,
    /// The server signed the token, but it is too old. A person who left the page open gives this result.
    Expired,
}

/// The token of the sign-in form: the time of issue and a MAC of this time and of the anti-CSRF
/// token of the browser. The token has no state on the server. Any instance that has the session
/// secret can check it.
pub fn sign_form_token(secret: &[u8], csrf: &str, issued_ms: u64) -> Result<String, String> {
    let issued = issued_ms.to_be_bytes();
    let mac = sign(secret, &form_token_input(&issued, csrf))?;
    let mut raw = Vec::with_capacity(8 + mac.len());
    raw.extend_from_slice(&issued);
    raw.extend_from_slice(&mac);
    Ok(Base64UrlUnpadded::encode_string(&raw))
}

pub fn check_form_token(secret: &[u8], csrf: &str, token: &str, now_ms: u64) -> FormToken {
    let raw = match Base64UrlUnpadded::decode_vec(token) {
        Ok(v) if v.len() == 8 + 32 => v,
        _ => return FormToken::Refused,
    };
    if !verify_mac(secret, &form_token_input(&raw[..8], csrf), &raw[8..]) {
        return FormToken::Refused;
    }
    let mut issued = [0u8; 8];
    issued.copy_from_slice(&raw[..8]);
    let issued = u64::from_be_bytes(issued);
    // A time in the future is too new: the signature proves that the server wrote it.
    match now_ms.checked_sub(issued) {
        Some(age) if age > FORM_MAX_AGE_MS => FormToken::Expired,
        Some(age) if age >= FORM_MIN_AGE_MS => FormToken::Valid,
        _ => FormToken::Refused,
    }
}

fn form_token_input(issued: &[u8], csrf: &str) -> Vec<u8> {
    let mut input = b"form\n".to_vec();
    input.extend_from_slice(issued);
    input.push(b'\n');
    input.extend_from_slice(csrf.as_bytes());
    input
}

/// The anti-CSRF token of a button on a page that stays open (sign out, account administration):
/// bound to the session it acts for and to what it is for (`purpose`), so it stays valid as long
/// as the page does (a cookie-based token would expire under an idle tab) and cannot be replayed
/// on another action.
pub fn sign_action_token(
    secret: &[u8],
    purpose: &str,
    session_token: &str,
) -> Result<String, String> {
    let mac = sign(secret, format!("{purpose}\n{session_token}").as_bytes())?;
    Ok(Base64UrlUnpadded::encode_string(&mac))
}

pub fn verify_action_token(secret: &[u8], purpose: &str, session_token: &str, token: &str) -> bool {
    let mac = match Base64UrlUnpadded::decode_vec(token) {
        Ok(v) if v.len() == 32 => v,
        _ => return false,
    };
    verify_mac(
        secret,
        format!("{purpose}\n{session_token}").as_bytes(),
        &mac,
    )
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

/// The credential generation of the superadmin: HMAC-SHA-256 under the session secret of the
/// superadmin login and password.
///
/// A change of the login, of the password or of the session secret gives another value. The
/// shared database keeps this value with each superadmin and impersonated session. It never
/// keeps a plain hash of the password: a reader of the database cannot test password guesses
/// without the session secret.
pub fn superadmin_generation(
    secret: &[u8],
    login: &str,
    password: &[u8],
) -> Result<[u8; 32], String> {
    let mut mac =
        HmacSha256::new_from_slice(secret).map_err(|_| "invalid HMAC secret length".to_string())?;
    // A login has no control character. The line end therefore separates the two fields.
    mac.update(b"superadmin-generation\n");
    mac.update(login.as_bytes());
    mac.update(b"\n");
    mac.update(password);
    let bytes = mac.finalize().into_bytes();
    let mut out = [0u8; 32];
    out.copy_from_slice(&bytes);
    Ok(out)
}

fn sign(secret: &[u8], data: &[u8]) -> Result<[u8; 32], String> {
    let mut mac =
        HmacSha256::new_from_slice(secret).map_err(|_| "invalid HMAC secret length".to_string())?;
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
        assert!(
            !verify_action_token(key, "users", "session-a", &token),
            "another purpose"
        );
        assert!(!verify_action_token(
            b"another-key-another-key-another!",
            "logout",
            "session-a",
            &token
        ));
        assert!(!verify_action_token(key, "logout", "session-a", "short"));
        let uri_signature = sign_return_uri(key, "session-a").unwrap();
        assert!(
            !verify_action_token(key, "logout", "session-a", &uri_signature),
            "domains are separated"
        );
    }

    #[test]
    fn a_form_token_needs_the_right_secret_the_right_csrf_token_and_the_right_age() {
        let key = b"01234567890123456789012345678901";
        let csrf = "abcdefghijklmnopqrstuvwxyz012345";
        let issued = 1_000_000_000u64;
        let token = sign_form_token(key, csrf, issued).unwrap();
        let at = |ms: u64| check_form_token(key, csrf, &token, issued + ms);
        assert_eq!(at(0), FormToken::Refused, "no time to read the form");
        assert_eq!(at(FORM_MIN_AGE_MS - 1), FormToken::Refused);
        assert_eq!(at(FORM_MIN_AGE_MS), FormToken::Valid);
        assert_eq!(at(FORM_MAX_AGE_MS), FormToken::Valid);
        assert_eq!(at(FORM_MAX_AGE_MS + 1), FormToken::Expired);
        assert_eq!(
            check_form_token(key, csrf, &token, issued - 5_000),
            FormToken::Refused,
            "a clock that is behind"
        );
        let later = issued + 5_000;
        assert_eq!(
            check_form_token(b"another-key-another-key-another!", csrf, &token, later),
            FormToken::Refused
        );
        assert_eq!(
            check_form_token(key, "another-csrf-token-another-csrf-", &token, later),
            FormToken::Refused,
            "the token belongs to one anti-CSRF token"
        );
        for bad in ["", "short", &token[..token.len() - 1], "!!!!"] {
            assert_eq!(check_form_token(key, csrf, bad, later), FormToken::Refused);
        }
        // The time is part of the signed text: a person cannot make the token look older.
        let mut raw = Base64UrlUnpadded::decode_vec(&token).unwrap();
        raw[7] ^= 1;
        let forged = Base64UrlUnpadded::encode_string(&raw);
        assert_eq!(
            check_form_token(key, csrf, &forged, later),
            FormToken::Refused
        );
    }

    #[test]
    fn the_superadmin_generation_changes_with_each_credential() {
        let key = b"01234567890123456789012345678901";
        let base = superadmin_generation(key, "root", b"password-1").unwrap();
        assert_eq!(
            base,
            superadmin_generation(key, "root", b"password-1").unwrap()
        );
        assert_ne!(
            base,
            superadmin_generation(key, "root", b"password-2").unwrap()
        );
        assert_ne!(
            base,
            superadmin_generation(key, "admin", b"password-1").unwrap()
        );
        assert_ne!(
            base,
            superadmin_generation(b"another-key-another-key-another!", "root", b"password-1")
                .unwrap()
        );
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
