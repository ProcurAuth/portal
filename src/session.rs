use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use base64ct::{Base64UrlUnpadded, Encoding};
use sha2::{Digest, Sha256};
use zeroize::Zeroize;

use crate::tracking::{unix_now, EndReason, Event, SessionStart, Tracker};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionIssueError {
    Capacity,
    Random,
}

/// What a session cookie is tied to (see `transport::binding`).
pub type Binding = [u8; 32];

/// Who a session belongs to. The superadmin is the account defined by the environment; the others
/// live in the database and are looked up by login (their roles are read live, so a change or a
/// deletion applies to a session already open).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Principal {
    Superadmin,
    User(String),
}

/// What a valid cookie stands for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Session {
    /// Random identifier of the session in the history (not the cookie value, which is secret).
    pub sid: u64,
    pub principal: Principal,
    /// The superadmin is browsing as this user: it is not a connection of the user.
    pub impersonated: bool,
}

/// What `issue` needs to open a session and to describe it in the history.
#[derive(Clone, Debug)]
pub struct NewSession {
    pub principal: Principal,
    /// The login shown in the history (the superadmin's own login for the superadmin).
    pub login: String,
    pub impersonated: bool,
    pub ip: Option<String>,
    pub user_agent: String,
}

#[derive(Debug)]
struct Entry {
    expires_at: Instant,
    last_seen: Instant,
    binding: Binding,
    session: Session,
}

pub struct SessionStore {
    sessions: Mutex<HashMap<[u8; 32], Entry>>,
    ttl: Duration,
    idle: Duration,
    max_active: usize,
    tracker: Arc<Tracker>,
}

impl SessionStore {
    pub fn new(ttl: Duration, idle: Duration, max_active: usize, tracker: Arc<Tracker>) -> Self {
        Self {
            sessions: Mutex::new(HashMap::new()),
            ttl,
            idle,
            max_active,
            tracker,
        }
    }

    pub fn issue(&self, binding: Binding, new: NewSession) -> Result<String, SessionIssueError> {
        let now = Instant::now();
        let mut sessions = self.sessions.lock().expect("session store mutex poisoned");
        self.prune(&mut sessions, now);
        if sessions.len() >= self.max_active {
            return Err(SessionIssueError::Capacity);
        }

        // A 256-bit random token has negligible collision probability, but keep
        // the insertion fail-closed and bounded even if the OS RNG misbehaves.
        for _ in 0..4 {
            let mut raw = [0u8; 32];
            if getrandom::fill(&mut raw).is_err() {
                raw.zeroize();
                return Err(SessionIssueError::Random);
            }
            let token = Base64UrlUnpadded::encode_string(&raw);
            let digest = digest_raw_token(&raw);
            raw.zeroize();

            if sessions.contains_key(&digest) {
                continue;
            }
            let mut sid = [0u8; 8];
            if getrandom::fill(&mut sid).is_err() {
                return Err(SessionIssueError::Random);
            }
            let session = Session {
                sid: u64::from_be_bytes(sid),
                principal: new.principal.clone(),
                impersonated: new.impersonated,
            };
            self.tracker.emit(Event::SessionStarted(SessionStart {
                sid: session.sid,
                login: new.login,
                superadmin: new.principal == Principal::Superadmin,
                impersonated: new.impersonated,
                ip: new.ip,
                user_agent: new.user_agent,
                at: unix_now(),
            }));
            sessions.insert(
                digest,
                Entry {
                    expires_at: now + self.ttl,
                    last_seen: now,
                    binding,
                    session,
                },
            );
            return Ok(token);
        }

        Err(SessionIssueError::Random)
    }

    /// The session `token` stands for, if it is live and was issued for `binding`. A valid use
    /// renews the idle timer. A mismatching binding (the cookie is used from elsewhere) is refused
    /// and leaves the session alone, so a thief cannot end the owner's session by trying it.
    pub fn validate(&self, token: &str, binding: &Binding) -> Option<Session> {
        let digest = digest_encoded_token(token)?;
        let now = Instant::now();
        let mut sessions = self.sessions.lock().expect("session store mutex poisoned");
        self.prune(&mut sessions, now);
        match sessions.get_mut(&digest) {
            Some(entry) if entry.binding == *binding => {
                entry.last_seen = now;
                self.tracker.touch(entry.session.sid, unix_now());
                Some(entry.session.clone())
            }
            _ => None,
        }
    }

    /// Ends the session `token` stands for and returns it.
    pub fn revoke(&self, token: &str, reason: EndReason) -> Option<Session> {
        let digest = digest_encoded_token(token)?;
        let entry = self
            .sessions
            .lock()
            .expect("session store mutex poisoned")
            .remove(&digest)?;
        self.ended(&entry.session, reason);
        Some(entry.session)
    }

    /// Ends every session of one user (the account was deleted); returns how many.
    pub fn revoke_user(&self, login: &str, reason: EndReason) -> usize {
        let mut sessions = self.sessions.lock().expect("session store mutex poisoned");
        let before = sessions.len();
        sessions.retain(|_, entry| {
            let owned = matches!(&entry.session.principal, Principal::User(l) if l == login);
            if owned {
                self.ended(&entry.session, reason);
            }
            !owned
        });
        before - sessions.len()
    }

    fn ended(&self, session: &Session, reason: EndReason) {
        self.tracker.emit(Event::SessionEnded {
            sid: session.sid,
            reason,
            at: unix_now(),
        });
    }

    pub fn active_count(&self) -> usize {
        let now = Instant::now();
        let mut sessions = self.sessions.lock().expect("session store mutex poisoned");
        self.prune(&mut sessions, now);
        sessions.len()
    }

    pub fn capacity(&self) -> usize {
        self.max_active
    }

    fn prune(&self, sessions: &mut HashMap<[u8; 32], Entry>, now: Instant) {
        sessions.retain(|_, entry| {
            let live = entry.expires_at > now;
            let active = now.duration_since(entry.last_seen) < self.idle;
            if !(live && active) {
                let reason = if live {
                    EndReason::Idle
                } else {
                    EndReason::Expired
                };
                self.ended(&entry.session, reason);
            }
            live && active
        });
    }
}

fn digest_encoded_token(token: &str) -> Option<[u8; 32]> {
    // 32 bytes encoded as unpadded base64url are exactly 43 ASCII bytes.
    if token.len() != 43 || !token.is_ascii() {
        return None;
    }
    let mut raw = Base64UrlUnpadded::decode_vec(token).ok()?;
    if raw.len() != 32 {
        raw.zeroize();
        return None;
    }
    let digest = digest_raw_token(&raw);
    raw.zeroize();
    Some(digest)
}

fn digest_raw_token(raw: &[u8]) -> [u8; 32] {
    let digest = Sha256::digest(raw);
    let mut out = [0u8; 32];
    out.copy_from_slice(&digest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const HOUR: Duration = Duration::from_secs(3600);
    const HERE: Binding = [1; 32];
    const ELSEWHERE: Binding = [2; 32];

    fn store(ttl: Duration, idle: Duration, max: usize) -> SessionStore {
        SessionStore::new(ttl, idle, max, Tracker::disabled())
    }

    fn user(login: &str) -> NewSession {
        NewSession {
            principal: Principal::User(login.into()),
            login: login.into(),
            impersonated: false,
            ip: Some("10.0.0.2".into()),
            user_agent: "Browser/1.0".into(),
        }
    }

    #[test]
    fn opaque_session_roundtrip_and_revoke() {
        let store = store(HOUR, HOUR, 4);
        let token = store.issue(HERE, user("alice")).unwrap();
        assert_eq!(token.len(), 43);
        let session = store.validate(&token, &HERE).unwrap();
        assert_eq!(session.principal, Principal::User("alice".into()));
        assert!(!session.impersonated);
        assert_eq!(store.active_count(), 1);
        assert!(store.revoke(&token, EndReason::Logout).is_some());
        assert!(store.validate(&token, &HERE).is_none());
        assert_eq!(store.active_count(), 0);
    }

    #[test]
    fn rejects_malformed_tokens() {
        let store = store(HOUR, HOUR, 4);
        assert!(store.validate("", &HERE).is_none());
        assert!(store.validate("not-a-session-token", &HERE).is_none());
        assert!(store
            .revoke("not-a-session-token", EndReason::Logout)
            .is_none());
    }

    #[test]
    fn enforces_capacity() {
        let store = store(HOUR, HOUR, 1);
        let _ = store.issue(HERE, user("alice")).unwrap();
        assert_eq!(
            store.issue(HERE, user("alice")),
            Err(SessionIssueError::Capacity)
        );
    }

    #[test]
    fn a_cookie_used_from_another_binding_is_refused_and_does_not_end_the_session() {
        let store = store(HOUR, HOUR, 4);
        let token = store.issue(HERE, user("alice")).unwrap();
        assert!(store.validate(&token, &ELSEWHERE).is_none());
        assert!(
            store.validate(&token, &HERE).is_some(),
            "the owner keeps the session"
        );
    }

    #[test]
    fn an_idle_session_ends_and_use_renews_it() {
        let store = store(HOUR, Duration::from_millis(120), 4);
        let token = store.issue(HERE, user("alice")).unwrap();
        for _ in 0..4 {
            std::thread::sleep(Duration::from_millis(60));
            assert!(
                store.validate(&token, &HERE).is_some(),
                "use inside the idle window renews it"
            );
        }
        std::thread::sleep(Duration::from_millis(200));
        assert!(
            store.validate(&token, &HERE).is_none(),
            "idle for longer than the timeout"
        );
        assert_eq!(store.active_count(), 0);
    }

    #[test]
    fn the_absolute_ttl_still_applies_to_an_active_session() {
        let store = store(Duration::from_millis(150), HOUR, 4);
        let token = store.issue(HERE, user("alice")).unwrap();
        std::thread::sleep(Duration::from_millis(80));
        assert!(store.validate(&token, &HERE).is_some());
        std::thread::sleep(Duration::from_millis(100));
        assert!(store.validate(&token, &HERE).is_none());
    }

    #[test]
    fn deleting_a_user_ends_all_of_its_sessions_and_only_its_own() {
        let store = store(HOUR, HOUR, 8);
        let a1 = store.issue(HERE, user("alice")).unwrap();
        let a2 = store.issue(ELSEWHERE, user("alice")).unwrap();
        let bob = store.issue(HERE, user("bob")).unwrap();
        let admin = store
            .issue(
                HERE,
                NewSession {
                    principal: Principal::Superadmin,
                    login: "root".into(),
                    impersonated: false,
                    ip: None,
                    user_agent: String::new(),
                },
            )
            .unwrap();
        assert_eq!(store.revoke_user("alice", EndReason::UserDeleted), 2);
        assert!(store.validate(&a1, &HERE).is_none());
        assert!(store.validate(&a2, &ELSEWHERE).is_none());
        assert!(store.validate(&bob, &HERE).is_some());
        assert_eq!(
            store.validate(&admin, &HERE).unwrap().principal,
            Principal::Superadmin
        );
    }

    #[test]
    fn an_impersonated_session_says_so() {
        let store = store(HOUR, HOUR, 4);
        let mut new = user("alice");
        new.impersonated = true;
        let token = store.issue(HERE, new).unwrap();
        assert!(store.validate(&token, &HERE).unwrap().impersonated);
    }

    #[test]
    fn two_sessions_get_different_history_ids() {
        let store = store(HOUR, HOUR, 4);
        let t1 = store.issue(HERE, user("alice")).unwrap();
        let t2 = store.issue(HERE, user("alice")).unwrap();
        assert_ne!(
            store.validate(&t1, &HERE).unwrap().sid,
            store.validate(&t2, &HERE).unwrap().sid
        );
    }
}
