//! The second factor of the sign-in: the pending token between the two steps, the enrolment, the check of a
//! code and the recovery codes.
//!
//! The state between the password step and the code step is a token that the server signs. It lives in a hidden
//! field of the page, so no instance keeps a state and any instance can check it. The secrets are in the database,
//! sealed with `authentication.totp_key` (see [`crate::secretbox`]). The last accepted time step, the recovery codes
//! and the counters of failures are in the database too, so every instance sees them.

use base64ct::{Base64UrlUnpadded, Encoding};
use zeroize::Zeroizing;

use crate::crypto;
use crate::secretbox::SecretBox;
use crate::session::Principal;
use crate::store::{Store, TotpLimits, TotpRow};
use crate::totp;

/// How long a person has to give the code after the password step, in seconds.
pub const PENDING_TTL_SECS: i64 = 300;
/// How long a page of the enrolment stays valid, in seconds: the time to scan the code and to type the first one.
pub const ENROL_TTL_SECS: i64 = 900;
/// The wrong codes that one pending token takes. The next wrong code sends the person back to the password step.
pub const MAX_TRIES: u8 = 5;
/// The failed codes of one account that lock it: 5 in 10 minutes lock the account for 5 minutes. The lock is not
/// permanent, and the counters are in the database.
pub const LIMITS: TotpLimits = TotpLimits {
    max_failures: 5,
    window: 600,
    lockout: 300,
};

const PENDING_DOMAIN: &str = "mfa-pending";

/// What the person has to do at the second step.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stage {
    /// The account has a second factor: give a code.
    Code,
    /// The account has none and the policy is `required`: enrol first.
    Enrol,
}

impl Stage {
    fn as_str(self) -> &'static str {
        match self {
            Self::Code => "code",
            Self::Enrol => "enrol",
        }
    }
}

/// The state between the two steps.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pending {
    pub stage: Stage,
    pub principal: Principal,
    /// The person ticked "Remember me" at the password step.
    pub keep: bool,
    /// The wrong codes that this token took.
    pub tries: u8,
    /// Unix time of the issue of the first token of the sequence. A new token after a wrong code keeps it, so
    /// a sequence never lasts longer than [`PENDING_TTL_SECS`].
    pub issued: i64,
}

#[derive(Debug, PartialEq, Eq)]
pub enum PendingError {
    /// The token is not one that this server signed for this browser and this page.
    Invalid,
    /// The token is valid and old.
    Expired,
}

/// The data that the signature covers besides the token: the browser and the anti-CSRF token of the page.
fn bound(binding: &[u8; 32], csrf: &str) -> Vec<u8> {
    let mut out = binding.to_vec();
    out.push(b'\n');
    out.extend_from_slice(csrf.as_bytes());
    out
}

fn payload(pending: &Pending) -> String {
    let who = match &pending.principal {
        Principal::Superadmin => "s".to_string(),
        Principal::User(login) => format!("u:{login}"),
    };
    format!(
        "{}\n{who}\n{}\n{}\n{}",
        pending.stage.as_str(),
        u8::from(pending.keep),
        pending.tries,
        pending.issued
    )
}

/// The token of a pending sign-in. It is bound to the browser (the `binding` of the session rule, which holds the
/// client address under the default rule) and to the anti-CSRF token of the page. It holds the account, the
/// choice "Remember me", the count of wrong codes and the time of the issue.
pub fn sign_pending(
    secret: &[u8],
    pending: &Pending,
    binding: &[u8; 32],
    csrf: &str,
) -> Result<String, String> {
    let text = payload(pending);
    let mut data = text.clone().into_bytes();
    data.push(b'\n');
    data.extend_from_slice(&bound(binding, csrf));
    let mac = crypto::mac(secret, PENDING_DOMAIN, &data)?;
    Ok(format!(
        "{}.{}",
        Base64UrlUnpadded::encode_string(text.as_bytes()),
        Base64UrlUnpadded::encode_string(&mac)
    ))
}

/// Reads a pending token. The signature, the browser and the page must match, and the token must be younger than
/// [`PENDING_TTL_SECS`].
pub fn open_pending(
    secret: &[u8],
    token: &str,
    binding: &[u8; 32],
    csrf: &str,
    now: i64,
) -> Result<Pending, PendingError> {
    let (text, mac) = token.split_once('.').ok_or(PendingError::Invalid)?;
    let text = Base64UrlUnpadded::decode_vec(text).map_err(|_| PendingError::Invalid)?;
    let mac = Base64UrlUnpadded::decode_vec(mac).map_err(|_| PendingError::Invalid)?;
    let mut data = text.clone();
    data.push(b'\n');
    data.extend_from_slice(&bound(binding, csrf));
    if !crypto::mac_matches(secret, PENDING_DOMAIN, &data, &mac) {
        return Err(PendingError::Invalid);
    }
    let text = String::from_utf8(text).map_err(|_| PendingError::Invalid)?;
    let mut fields = text.split('\n');
    let (Some(stage), Some(who), Some(keep), Some(tries), Some(issued), None) = (
        fields.next(),
        fields.next(),
        fields.next(),
        fields.next(),
        fields.next(),
        fields.next(),
    ) else {
        return Err(PendingError::Invalid);
    };
    let pending = Pending {
        stage: match stage {
            "code" => Stage::Code,
            "enrol" => Stage::Enrol,
            _ => return Err(PendingError::Invalid),
        },
        principal: match who {
            "s" => Principal::Superadmin,
            other => Principal::User(
                other
                    .strip_prefix("u:")
                    .filter(|login| !login.is_empty())
                    .ok_or(PendingError::Invalid)?
                    .to_string(),
            ),
        },
        keep: keep == "1",
        tries: tries.parse().map_err(|_| PendingError::Invalid)?,
        issued: issued.parse().map_err(|_| PendingError::Invalid)?,
    };
    if now - pending.issued > PENDING_TTL_SECS || now < pending.issued - 5 {
        return Err(PendingError::Expired);
    }
    Ok(pending)
}

/// What a check of a code found.
#[derive(Debug, PartialEq, Eq)]
pub enum Verdict {
    /// A code of the authenticator app. It is accepted once.
    Totp,
    /// A recovery code. It is used up.
    Recovery,
    /// The code is wrong, or it was used already. `locked_until` is set when this failure locked the account.
    Wrong { locked_until: Option<i64> },
    /// The account takes no code until this Unix time.
    Locked { until: i64 },
    /// The account has no second factor.
    NotEnrolled,
}

/// What a confirmation of an enrolment found.
#[derive(Debug, PartialEq, Eq)]
pub enum Enrolled {
    /// The second factor is on. The recovery codes are shown once.
    Done(Vec<String>),
    /// The code is wrong for the secret of the page.
    WrongCode,
    /// The page is not one that this server made for this account, or it is old.
    BadPage,
    /// The account has a second factor already.
    Already,
}

/// The page of an enrolment: the secret and what a person needs to add it to an app.
pub struct Enrolment {
    /// The sealed secret and the time limit, for the hidden field of the form.
    pub token: String,
    /// The secret in base32, in groups of four.
    pub grouped: String,
    /// The QR code of the link, as inline SVG markup. `None` when the link is too long for a code.
    pub qr: Option<String>,
}

/// The second factor of this instance: the keys derived from `authentication.totp_key`.
pub struct Mfa {
    sealbox: SecretBox,
}

impl Mfa {
    pub fn new(sealbox: SecretBox) -> Self {
        Self { sealbox }
    }

    #[cfg(test)]
    pub fn check_value(&self) -> [u8; 32] {
        self.sealbox.check_value()
    }

    fn enrol_aad(login: &str, expires: i64) -> Vec<u8> {
        format!("enrol\n{login}\n{expires}").into_bytes()
    }

    fn secret_aad(login: &str) -> Vec<u8> {
        format!("secret\n{login}").into_bytes()
    }

    /// A new secret for `login`, with its page. Nothing is stored: the secret is in the token, sealed and bound
    /// to the account, until a good code confirms it.
    pub fn begin(&self, login: &str, issuer: &str, now: i64) -> Result<Enrolment, String> {
        let secret = Zeroizing::new(totp::new_secret()?);
        let expires = now + ENROL_TTL_SECS;
        let sealed = self
            .sealbox
            .seal(&*secret, &Self::enrol_aad(login, expires))?;
        let uri = totp::otpauth_uri(issuer, login, &*secret);
        Ok(Enrolment {
            token: format!("{expires}.{}", Base64UrlUnpadded::encode_string(&sealed)),
            grouped: totp::grouped(&*secret),
            qr: qr_svg(&uri),
        })
    }

    /// The page of an enrolment that a person already has: the same secret, from its token. `None` when the token is not
    /// one that this server made for this account, or it is old.
    pub fn resume(&self, login: &str, token: &str, issuer: &str, now: i64) -> Option<Enrolment> {
        let secret = self.open_enrolment(login, token, now)?;
        let uri = totp::otpauth_uri(issuer, login, &secret);
        Some(Enrolment {
            token: token.to_string(),
            grouped: totp::grouped(&secret),
            qr: qr_svg(&uri),
        })
    }

    fn open_enrolment(&self, login: &str, token: &str, now: i64) -> Option<Zeroizing<Vec<u8>>> {
        let (expires, sealed) = token.split_once('.')?;
        let expires: i64 = expires.parse().ok()?;
        if expires < now {
            return None;
        }
        let sealed = Base64UrlUnpadded::decode_vec(sealed).ok()?;
        self.sealbox
            .open(&sealed, &Self::enrol_aad(login, expires))
            .filter(|secret| secret.len() == totp::SECRET_BYTES)
    }

    /// Turns the second factor on for `login` when `typed` is a good code for the secret that `token` holds. The
    /// code of the confirmation is used up: it cannot sign in.
    pub fn confirm(
        &self,
        store: &Store,
        login: &str,
        token: &str,
        typed: &str,
        now: i64,
    ) -> Result<Enrolled, String> {
        let Some(secret) = self.open_enrolment(login, token, now) else {
            return Ok(Enrolled::BadPage);
        };
        let Some(step) = totp::verify(&secret, typed, now, 0) else {
            return Ok(Enrolled::WrongCode);
        };
        let codes = totp::new_recovery_codes()?;
        let sealed = self.sealbox.seal(&secret, &Self::secret_aad(login))?;
        if store.totp_enable(login, &sealed, &self.recovery_hashes(&codes), step, now)? {
            Ok(Enrolled::Done(codes))
        } else {
            Ok(Enrolled::Already)
        }
    }

    fn recovery_hashes(&self, codes: &[String]) -> Vec<[u8; 32]> {
        codes
            .iter()
            .filter_map(|code| totp::normalize_recovery(code))
            .map(|code| self.sealbox.recovery_hash(&code))
            .collect()
    }

    /// Makes new recovery codes for an account that has a second factor. The old codes stop working.
    pub fn regenerate(&self, store: &Store, login: &str) -> Result<Option<Vec<String>>, String> {
        let codes = totp::new_recovery_codes()?;
        Ok(store
            .totp_replace_recovery(login, &self.recovery_hashes(&codes))?
            .then_some(codes))
    }

    /// Checks a typed code of the second step. A code of the app is good once: the database keeps the last accepted
    /// step. A recovery code is good once: the database deletes it. A wrong code counts against the account, in the
    /// database, so every instance adds to the count.
    ///
    /// The error is a failure of the database, or a secret that the key does not open. A wrong key never gives
    /// a wrong code: the caller shows an error of the server and no secret changes.
    pub fn verify(
        &self,
        store: &Store,
        login: &str,
        typed: &str,
        now: i64,
    ) -> Result<Verdict, String> {
        let Some(row) = store.totp_get(login)? else {
            return Ok(Verdict::NotEnrolled);
        };
        if row.locked_until > now {
            return Ok(Verdict::Locked {
                until: row.locked_until,
            });
        }
        if let Some(code) = totp::normalize_code(typed) {
            let secret = self.open_secret(login, &row)?;
            if let Some(step) = totp::verify(&secret, &code, now, row.last_step) {
                if store.totp_accept_step(login, step)? {
                    return Ok(Verdict::Totp);
                }
            }
        } else if let Some(code) = totp::normalize_recovery(typed) {
            if store.totp_use_recovery(login, &self.sealbox.recovery_hash(&code))? {
                return Ok(Verdict::Recovery);
            }
        }
        let locked_until = store.totp_fail(login, now, LIMITS)?;
        Ok(Verdict::Wrong { locked_until })
    }

    fn open_secret(&self, login: &str, row: &TotpRow) -> Result<Zeroizing<Vec<u8>>, String> {
        self.sealbox
            .open(&row.sealed, &Self::secret_aad(login))
            .ok_or_else(|| {
                "the secret of the second factor does not open with authentication.totp_key"
                    .to_string()
            })
    }
}

/// The QR code of `data` as inline SVG markup, or `None` when the data does not fit a code. The SVG has no script, no
/// style attribute and no external reference. It has a white background and black modules in both color schemes,
/// because a scanner needs the contrast.
pub fn qr_svg(data: &str) -> Option<String> {
    use qrcode::render::svg;
    let code = qrcode::QrCode::with_error_correction_level(data, qrcode::EcLevel::M).ok()?;
    let svg = code
        .render::<svg::Color>()
        .min_dimensions(200, 200)
        .quiet_zone(true)
        .dark_color(svg::Color("#000000"))
        .light_color(svg::Color("#ffffff"))
        .build();
    // The renderer writes the XML prolog and fixed sizes. The page sizes the image with its class.
    let start = svg.find("<svg")?;
    let svg = &svg[start..];
    let end = svg.find('>')?;
    let mut tag = svg[..end].to_string();
    for name in ["width", "height"] {
        if let Some(at) = tag.find(&format!(" {name}=\"")) {
            let value = at + name.len() + 3;
            let close = tag[value..].find('"')? + value + 1;
            tag.replace_range(at..close, "");
        }
    }
    let tag = tag.replacen(
        "<svg",
        r#"<svg class="qr" role="img" aria-label="QR code of the secret""#,
        1,
    );
    Some(format!("{tag}{}", &svg[end..]))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::testing::each_backend;

    const SECRET: &[u8] = b"0123456789abcdef0123456789abcdef";
    const BINDING: [u8; 32] = [7; 32];
    const CSRF: &str = "csrf-token-of-the-page";

    fn mfa(byte: u8) -> Mfa {
        let mut key = [0u8; 32];
        for (i, b) in key.iter_mut().enumerate() {
            *b = byte.wrapping_add(i as u8);
        }
        Mfa::new(SecretBox::new(&key).unwrap())
    }

    fn pending(tries: u8) -> Pending {
        Pending {
            stage: Stage::Code,
            principal: Principal::User("alice".into()),
            keep: true,
            tries,
            issued: 1_000_000,
        }
    }

    #[test]
    fn a_pending_token_keeps_what_the_password_step_decided() {
        for principal in [Principal::Superadmin, Principal::User("alice.b-c_d".into())] {
            for stage in [Stage::Code, Stage::Enrol] {
                let want = Pending {
                    stage,
                    principal: principal.clone(),
                    keep: true,
                    tries: 3,
                    issued: 1_000_000,
                };
                let token = sign_pending(SECRET, &want, &BINDING, CSRF).unwrap();
                assert_eq!(
                    open_pending(SECRET, &token, &BINDING, CSRF, 1_000_010),
                    Ok(want)
                );
            }
        }
    }

    #[test]
    fn a_pending_token_is_refused_when_it_is_changed_old_or_used_by_another_browser() {
        let token = sign_pending(SECRET, &pending(0), &BINDING, CSRF).unwrap();
        let open = |secret: &[u8], token: &str, binding: &[u8; 32], csrf: &str, now: i64| {
            open_pending(secret, token, binding, csrf, now)
        };
        assert!(open(SECRET, &token, &BINDING, CSRF, 1_000_100).is_ok());
        // The time limit.
        assert_eq!(
            open(SECRET, &token, &BINDING, CSRF, 1_000_000 + PENDING_TTL_SECS),
            Ok(pending(0))
        );
        assert_eq!(
            open(SECRET, &token, &BINDING, CSRF, 1_000_001 + PENDING_TTL_SECS),
            Err(PendingError::Expired)
        );
        // Another client, another page, another server key.
        assert_eq!(
            open(SECRET, &token, &[8; 32], CSRF, 1_000_100),
            Err(PendingError::Invalid)
        );
        assert_eq!(
            open(SECRET, &token, &BINDING, "another", 1_000_100),
            Err(PendingError::Invalid)
        );
        assert_eq!(
            open(
                b"another-key-another-key-another!",
                &token,
                &BINDING,
                CSRF,
                1_000_100
            ),
            Err(PendingError::Invalid)
        );
        // A person cannot change the account, the choice or the count of tries.
        let (text, mac) = token.split_once('.').unwrap();
        let plain = String::from_utf8(Base64UrlUnpadded::decode_vec(text).unwrap()).unwrap();
        for forged in [
            plain.replace("alice", "root"),
            plain.replace("\n1\n0\n", "\n0\n0\n"),
            plain.replace("\n0\n1000000", "\n9\n1000000"),
            plain.replace("code", "enrol"),
        ] {
            let forged = format!(
                "{}.{mac}",
                Base64UrlUnpadded::encode_string(forged.as_bytes())
            );
            assert_eq!(
                open(SECRET, &forged, &BINDING, CSRF, 1_000_100),
                Err(PendingError::Invalid),
                "{forged}"
            );
        }
        for bad in ["", ".", "a.b", "!!.!!", &token[..token.len() - 2]] {
            assert_eq!(
                open(SECRET, bad, &BINDING, CSRF, 1_000_100),
                Err(PendingError::Invalid)
            );
        }
    }

    #[test]
    fn an_enrolment_needs_a_good_code_for_the_secret_of_its_page_and_gives_ten_recovery_codes() {
        let mfa = mfa(1);
        let now = 1_700_000_000;
        each_backend(|backend| {
            let store = backend.fresh();
            let page = mfa.begin("alice", "Portal", now).unwrap();
            // The page has the secret in groups of four, and the QR code of the link.
            assert_eq!(
                totp::base32_decode(&page.grouped.replace(' ', ""))
                    .unwrap()
                    .len(),
                20
            );
            assert!(page.qr.is_some());
            assert!(page
                .qr
                .as_deref()
                .is_some_and(|svg| svg.starts_with("<svg") && !svg.contains("<?xml")));
            let secret = totp::base32_decode(&page.grouped.replace(' ', "")).unwrap();
            let good = totp::code_at(&secret, now);
            // A wrong code, a page for another account, an old page.
            assert_eq!(
                mfa.confirm(&store, "alice", &page.token, "000000", now)
                    .unwrap(),
                Enrolled::WrongCode
            );
            assert_eq!(
                mfa.confirm(&store, "bob", &page.token, &good, now).unwrap(),
                Enrolled::BadPage
            );
            assert_eq!(
                mfa.confirm(
                    &store,
                    "alice",
                    &page.token,
                    &good,
                    now + ENROL_TTL_SECS + 1
                )
                .unwrap(),
                Enrolled::BadPage
            );
            assert!(
                store.totp_get("alice").unwrap().is_none(),
                "nothing stored yet"
            );
            let Enrolled::Done(codes) = mfa
                .confirm(&store, "alice", &page.token, &good, now)
                .unwrap()
            else {
                panic!("a good code turns the second factor on");
            };
            assert_eq!(codes.len(), totp::RECOVERY_CODES);
            let row = store.totp_get("alice").unwrap().unwrap();
            assert_eq!(row.recovery_left as usize, totp::RECOVERY_CODES);
            assert_eq!(row.last_step, totp::step_of(now));
            assert!(
                !row.sealed
                    .windows(secret.len())
                    .any(|w| w == secret.as_slice()),
                "sealed"
            );
            // A second enrolment of the same account is refused, and the code of the first one cannot sign in.
            let again = mfa.begin("alice", "Portal", now).unwrap();
            let other = totp::base32_decode(&again.grouped.replace(' ', "")).unwrap();
            assert_eq!(
                mfa.confirm(
                    &store,
                    "alice",
                    &again.token,
                    &totp::code_at(&other, now),
                    now
                )
                .unwrap(),
                Enrolled::Already
            );
            assert_eq!(
                mfa.verify(&store, "alice", &good, now).unwrap(),
                Verdict::Wrong { locked_until: None },
                "the code that confirmed the enrolment is used up"
            );
        });
    }

    fn enrolled(store: &Store, mfa: &Mfa, login: &str, now: i64) -> (Vec<u8>, Vec<String>) {
        let page = mfa.begin(login, "Portal", now).unwrap();
        let secret = totp::base32_decode(&page.grouped.replace(' ', "")).unwrap();
        let Enrolled::Done(codes) = mfa
            .confirm(store, login, &page.token, &totp::code_at(&secret, now), now)
            .unwrap()
        else {
            panic!("enrolment");
        };
        (secret, codes)
    }

    #[test]
    fn a_code_is_good_once_a_recovery_code_is_good_once_and_a_wrong_code_counts() {
        let mfa = mfa(1);
        let now = 1_700_000_000;
        each_backend(|backend| {
            let store = backend.fresh();
            let (secret, codes) = enrolled(&store, &mfa, "alice", now);
            let next = now + 30;
            let code = totp::code_at(&secret, next);
            assert_eq!(
                mfa.verify(&store, "alice", &code, next).unwrap(),
                Verdict::Totp
            );
            // Replay: the same code, and the code of the step before, are refused.
            assert!(matches!(
                mfa.verify(&store, "alice", &code, next).unwrap(),
                Verdict::Wrong { .. }
            ));
            assert!(matches!(
                mfa.verify(&store, "alice", &totp::code_at(&secret, now), next)
                    .unwrap(),
                Verdict::Wrong { .. }
            ));
            // A code with a space in it, one step later, works.
            let later = totp::code_at(&secret, next + 30);
            let spaced = format!("{} {}", &later[..3], &later[3..]);
            assert_eq!(
                mfa.verify(&store, "alice", &spaced, next + 30).unwrap(),
                Verdict::Totp
            );
            // A recovery code works once, in any typed form.
            let recovery = codes[0].to_uppercase().replace('-', " ");
            assert_eq!(
                mfa.verify(&store, "alice", &recovery, next).unwrap(),
                Verdict::Recovery
            );
            assert!(matches!(
                mfa.verify(&store, "alice", &codes[0], next).unwrap(),
                Verdict::Wrong { .. }
            ));
            assert_eq!(
                store.totp_get("alice").unwrap().unwrap().recovery_left as usize,
                totp::RECOVERY_CODES - 1
            );
            // A login without a second factor.
            assert_eq!(
                mfa.verify(&store, "bob", "123456", next).unwrap(),
                Verdict::NotEnrolled
            );
        });
    }

    #[test]
    fn five_wrong_codes_lock_the_account_for_a_while_and_a_good_code_waits() {
        let mfa = mfa(1);
        let now = 1_700_000_000;
        each_backend(|backend| {
            let store = backend.fresh();
            let (secret, _) = enrolled(&store, &mfa, "alice", now);
            let t = now + 60;
            for n in 1..LIMITS.max_failures {
                assert_eq!(
                    mfa.verify(&store, "alice", "000000", t).unwrap(),
                    Verdict::Wrong { locked_until: None },
                    "failure {n}"
                );
            }
            assert_eq!(
                mfa.verify(&store, "alice", "000000", t).unwrap(),
                Verdict::Wrong {
                    locked_until: Some(t + LIMITS.lockout)
                }
            );
            // The lock holds against a good code too, until it ends. No lock is permanent.
            let good = totp::code_at(&secret, t);
            assert_eq!(
                mfa.verify(&store, "alice", &good, t + 1).unwrap(),
                Verdict::Locked {
                    until: t + LIMITS.lockout
                }
            );
            let after = totp::code_at(&secret, t + LIMITS.lockout);
            assert_eq!(
                mfa.verify(&store, "alice", &after, t + LIMITS.lockout)
                    .unwrap(),
                Verdict::Totp
            );
            // The failures of an old window do not add up.
            for n in 0..LIMITS.max_failures - 1 {
                let at = t + 1_000 + n * 700;
                assert_eq!(
                    mfa.verify(&store, "alice", "000000", at).unwrap(),
                    Verdict::Wrong { locked_until: None },
                    "spread failure {n}"
                );
            }
        });
    }

    #[test]
    fn a_wrong_key_never_resets_a_secret_and_fails_loudly() {
        let (first, second) = (mfa(1), mfa(2));
        let now = 1_700_000_000;
        each_backend(|backend| {
            let store = backend.fresh();
            let (secret, _) = enrolled(&store, &first, "alice", now);
            let before = store.totp_get("alice").unwrap().unwrap();
            let error = second
                .verify(&store, "alice", &totp::code_at(&secret, now + 30), now + 30)
                .unwrap_err();
            assert!(error.contains("authentication.totp_key"), "{error}");
            // The row did not change, and the right key still works.
            assert_eq!(store.totp_get("alice").unwrap().unwrap(), before);
            assert_eq!(
                first
                    .verify(&store, "alice", &totp::code_at(&secret, now + 30), now + 30)
                    .unwrap(),
                Verdict::Totp
            );
            // The check value tells the keys apart at start, and the first call records it.
            assert!(store.totp_bind_key(&first.check_value()).unwrap());
            assert!(store.totp_bind_key(&first.check_value()).unwrap());
            assert!(!store.totp_bind_key(&second.check_value()).unwrap());
            // After the removal of all secrets, a new key can serve.
            assert_eq!(store.totp_remove_all().unwrap(), 1);
            assert!(store.totp_bind_key(&second.check_value()).unwrap());
        });
    }

    #[test]
    fn new_recovery_codes_replace_the_old_ones() {
        let mfa = mfa(1);
        let now = 1_700_000_000;
        each_backend(|backend| {
            let store = backend.fresh();
            let (_, old) = enrolled(&store, &mfa, "alice", now);
            assert!(
                mfa.regenerate(&store, "bob").unwrap().is_none(),
                "no second factor"
            );
            let new = mfa.regenerate(&store, "alice").unwrap().unwrap();
            assert_eq!(new.len(), totp::RECOVERY_CODES);
            assert!(matches!(
                mfa.verify(&store, "alice", &old[0], now).unwrap(),
                Verdict::Wrong { .. }
            ));
            assert_eq!(
                mfa.verify(&store, "alice", &new[0], now).unwrap(),
                Verdict::Recovery
            );
        });
    }

    #[test]
    fn deleting_an_account_deletes_its_second_factor_and_a_new_account_of_the_name_starts_without()
    {
        let mfa = mfa(1);
        let now = 1_700_000_000;
        each_backend(|backend| {
            let store = backend.fresh();
            let user = crate::store::UserRow {
                login: "alice".into(),
                password_hash: "$argon2id$stub".into(),
                grants: Vec::new(),
                created_at: 1,
            };
            store.insert_user(&user).unwrap();
            enrolled(&store, &mfa, "alice", now);
            enrolled(&store, &mfa, "carol", now);
            assert_eq!(store.totp_logins().unwrap(), ["alice", "carol"]);
            assert!(store.remove_user("alice").unwrap());
            assert_eq!(
                store.totp_logins().unwrap(),
                ["carol"],
                "alice lost hers, carol keeps hers"
            );
            assert_eq!(store.count("totp_recovery") as usize, totp::RECOVERY_CODES);
            // A row that an older account left behind does not pass to a new account of the same login.
            enrolled(&store, &mfa, "alice", now);
            assert!(!store.remove_user("nobody").unwrap());
            store.insert_user(&user).unwrap();
            assert_eq!(store.totp_logins().unwrap(), ["carol"]);
        });
    }

    #[test]
    fn two_instances_of_one_database_share_the_secrets_the_steps_the_codes_and_the_failures() {
        let Some(pg) = crate::store::testing::postgres() else {
            return;
        };
        let (a, b) = pg.fresh_pair();
        let mfa = mfa(1);
        let now = 1_700_000_000;
        let (secret, codes) = enrolled(&a, &mfa, "alice", now);
        // Instance B reads what A wrote, and the two share the steps: a code is good once on the pair.
        let code = totp::code_at(&secret, now + 30);
        assert_eq!(
            mfa.verify(&b, "alice", &code, now + 30).unwrap(),
            Verdict::Totp
        );
        assert!(matches!(
            mfa.verify(&a, "alice", &code, now + 30).unwrap(),
            Verdict::Wrong { .. }
        ));
        // A recovery code that B used is gone on A.
        assert_eq!(
            mfa.verify(&b, "alice", &codes[1], now + 30).unwrap(),
            Verdict::Recovery
        );
        assert!(matches!(
            mfa.verify(&a, "alice", &codes[1], now + 30).unwrap(),
            Verdict::Wrong { .. }
        ));
        // The failures of the two instances add up. The wrong recovery code on A counted one: four more lock it.
        let t = now + 100;
        mfa.verify(&a, "alice", "000000", t).unwrap();
        mfa.verify(&b, "alice", "000000", t).unwrap();
        mfa.verify(&a, "alice", "000000", t).unwrap();
        let last = mfa.verify(&b, "alice", "000000", t).unwrap();
        assert_eq!(
            last,
            Verdict::Wrong {
                locked_until: Some(t + LIMITS.lockout)
            },
            "5 failures over both"
        );
        assert!(matches!(
            mfa.verify(&b, "alice", "000000", t + 1).unwrap(),
            Verdict::Locked { .. }
        ));
    }
}
