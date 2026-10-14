//! The sessions: who a cookie stands for.
//!
//! With one instance they live in its memory (a restart ends them). With a shared database they
//! live in it, so a session opened on one instance is valid on the others and survives a restart;
//! each instance then keeps a short-lived copy of what it checked, so the request path asks the
//! database about one session once every few seconds instead of once per request.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use base64ct::{Base64UrlUnpadded, Encoding};
use sha2::{Digest, Sha256};
use zeroize::Zeroize;

use crate::store::{SharedSession, Store};
use crate::tracking::{log_error, unix_now, EndReason, Event, SessionStart, Tracker};
use crate::util::blocking;

/// How long an instance trusts what it learnt about a session from the shared database. A session
/// ended on another instance (a logout, a deleted account) stops working here within this time.
const CACHE_TTL: Duration = Duration::from_secs(5);
/// When the shared database cannot be reached, a session checked less than this ago is still
/// accepted, so a failover of the database does not sign everybody out.
const STALE_GRACE: Duration = Duration::from_secs(120);
/// A history row is taken for lost (its instance died before saying how it ended) only this long
/// after the session could last have been used: the end of a session that did end is flushed within
/// [`crate::tracking::FLUSH_EVERY`].
const LOST_GRACE_SECS: i64 = 60;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionIssueError {
    Capacity,
    Random,
    /// The shared database refused or could not be reached.
    Storage,
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
    /// When the shared database last confirmed the session (unused with one instance).
    checked: Instant,
    binding: Binding,
    session: Session,
}

pub struct SessionStore {
    /// The sessions of this instance; with a shared database, the copies it keeps of them.
    sessions: Mutex<HashMap<[u8; 32], Entry>>,
    ttl: Duration,
    idle: Duration,
    max_active: usize,
    tracker: Arc<Tracker>,
    shared: Option<Arc<Store>>,
    cache_ttl: Duration,
    /// Live sessions of every instance, as the last sweep counted them.
    shared_active: AtomicUsize,
    shared_errors: AtomicU64,
}

impl SessionStore {
    pub fn new(ttl: Duration, idle: Duration, max_active: usize, tracker: Arc<Tracker>) -> Self {
        Self {
            sessions: Mutex::new(HashMap::new()),
            ttl,
            idle,
            max_active,
            tracker,
            shared: None,
            cache_ttl: CACHE_TTL,
            shared_active: AtomicUsize::new(0),
            shared_errors: AtomicU64::new(0),
        }
    }

    /// Keeps the sessions in the shared `store` instead of in memory.
    pub fn shared(mut self, store: Arc<Store>) -> Self {
        self.shared = Some(store);
        self
    }

    #[cfg(test)]
    pub fn set_cache_ttl(&mut self, cache_ttl: Duration) {
        self.cache_ttl = cache_ttl;
    }

    /// Failures of the shared database that the sessions met (always 0 with one instance).
    pub fn shared_errors(&self) -> u64 {
        self.shared_errors.load(Ordering::Relaxed)
    }

    fn failed(&self, event: &str, error: &str) {
        self.shared_errors.fetch_add(1, Ordering::Relaxed);
        log_error(event, error);
    }

    pub fn issue(&self, binding: Binding, new: NewSession) -> Result<String, SessionIssueError> {
        if let Some(store) = &self.shared {
            return self.issue_shared(store, binding, new);
        }
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
                    checked: now,
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
        if let Some(store) = &self.shared {
            return self.validate_shared(store, digest, binding);
        }
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
        if let Some(store) = &self.shared {
            return self.revoke_shared(store, digest, reason);
        }
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
        if let Some(store) = &self.shared {
            return self.revoke_user_shared(store, login, reason);
        }
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
        self.ended_sid(session.sid, reason);
    }

    fn ended_sid(&self, sid: u64, reason: EndReason) {
        self.tracker.emit(Event::SessionEnded {
            sid,
            reason,
            at: unix_now(),
        });
    }

    pub fn active_count(&self) -> usize {
        if self.shared.is_some() {
            return self.shared_active.load(Ordering::Relaxed);
        }
        let now = Instant::now();
        let mut sessions = self.sessions.lock().expect("session store mutex poisoned");
        self.prune(&mut sessions, now);
        sessions.len()
    }

    pub fn capacity(&self) -> usize {
        self.max_active
    }

    // Shared database ------------------------------------------------------------------------

    fn issue_shared(
        &self,
        store: &Store,
        binding: Binding,
        new: NewSession,
    ) -> Result<String, SessionIssueError> {
        let mut raw = [0u8; 32];
        let mut sid = [0u8; 8];
        if getrandom::fill(&mut raw).is_err() || getrandom::fill(&mut sid).is_err() {
            raw.zeroize();
            return Err(SessionIssueError::Random);
        }
        let token = Base64UrlUnpadded::encode_string(&raw);
        let digest = digest_raw_token(&raw);
        raw.zeroize();

        let now = unix_now();
        let session = Session {
            sid: u64::from_be_bytes(sid),
            principal: new.principal.clone(),
            impersonated: new.impersonated,
        };
        let stored = SharedSession {
            sid: session.sid,
            login: match &new.principal {
                Principal::Superadmin => None,
                Principal::User(login) => Some(login.clone()),
            },
            impersonated: new.impersonated,
            expires_at: now + self.ttl.as_secs() as i64,
        };
        // The history row is written with the session, so no instance meets a session without it.
        let start = SessionStart {
            sid: session.sid,
            login: new.login,
            superadmin: new.principal == Principal::Superadmin,
            impersonated: new.impersonated,
            ip: new.ip,
            user_agent: new.user_agent,
            at: now,
        };
        match blocking(|| store.session_insert(&digest, &binding, &stored, &start, self.max_active)) {
            Ok(true) => {}
            Ok(false) => return Err(SessionIssueError::Capacity),
            Err(error) => {
                self.failed("session_insert_failed", &error);
                return Err(SessionIssueError::Storage);
            }
        }
        let at = Instant::now();
        let mut cache = self.sessions.lock().expect("session store mutex poisoned");
        self.prune_copies(&mut cache, at);
        cache.insert(
            digest,
            Entry {
                expires_at: at + self.ttl,
                last_seen: at,
                checked: at,
                binding,
                session,
            },
        );
        Ok(token)
    }

    fn validate_shared(
        &self,
        store: &Store,
        digest: [u8; 32],
        binding: &Binding,
    ) -> Option<Session> {
        let now = Instant::now();
        let copy = {
            let mut cache = self.sessions.lock().expect("session store mutex poisoned");
            self.prune_copies(&mut cache, now);
            cache
                .get(&digest)
                .map(|entry| (entry.binding, entry.checked, entry.session.clone()))
        };
        if let Some((copy_binding, checked, session)) = &copy {
            if copy_binding != binding {
                // Used from elsewhere: refused, and the owner's session is left alone.
                return None;
            }
            if now.duration_since(*checked) < self.cache_ttl {
                self.tracker.touch(session.sid, unix_now());
                return Some(session.clone());
            }
        }
        let at = unix_now();
        match blocking(|| store.session_touch(&digest, binding, at, self.idle)) {
            Ok(Some(stored)) => {
                let session = session_of(stored.clone());
                let left = Duration::from_secs(stored.expires_at.saturating_sub(at).max(0) as u64);
                self.sessions
                    .lock()
                    .expect("session store mutex poisoned")
                    .insert(
                        digest,
                        Entry {
                            expires_at: now + left,
                            last_seen: now,
                            checked: now,
                            binding: *binding,
                            session: session.clone(),
                        },
                    );
                self.tracker.touch(session.sid, at);
                Some(session)
            }
            Ok(None) => {
                self.sessions
                    .lock()
                    .expect("session store mutex poisoned")
                    .remove(&digest);
                None
            }
            Err(error) => {
                self.failed("session_check_failed", &error);
                copy.filter(|(_, checked, _)| now.duration_since(*checked) < STALE_GRACE)
                    .map(|(_, _, session)| session)
            }
        }
    }

    fn revoke_shared(&self, store: &Store, digest: [u8; 32], reason: EndReason) -> Option<Session> {
        let copy = self
            .sessions
            .lock()
            .expect("session store mutex poisoned")
            .remove(&digest);
        match blocking(|| store.session_delete(&digest)) {
            Ok(Some(stored)) => {
                let session = session_of(stored);
                self.ended(&session, reason);
                Some(session)
            }
            // Another instance ended it first, and recorded it.
            Ok(None) => None,
            Err(error) => {
                // It stays valid elsewhere until it expires; the failure is counted and logged.
                self.failed("session_revoke_failed", &error);
                copy.map(|entry| entry.session)
            }
        }
    }

    fn revoke_user_shared(&self, store: &Store, login: &str, reason: EndReason) -> usize {
        self.sessions
            .lock()
            .expect("session store mutex poisoned")
            .retain(|_, entry| !matches!(&entry.session.principal, Principal::User(l) if l == login));
        match blocking(|| store.sessions_delete_user(login)) {
            Ok(ended) => {
                for sid in &ended {
                    self.ended_sid(*sid, reason);
                }
                ended.len()
            }
            Err(error) => {
                self.failed("session_revoke_user_failed", &error);
                0
            }
        }
    }

    /// Ends the sessions of every instance that ran out (lifetime or idle time), records how, and
    /// counts the live ones. Run by every instance every few seconds: the database hands each ended
    /// session to exactly one of them.
    pub fn sweep(&self) {
        let Some(store) = &self.shared else { return };
        let now = unix_now();
        match store.sessions_expire(now, self.idle) {
            Ok(ended) => {
                for (sid, expired) in ended {
                    self.ended_sid(
                        sid,
                        if expired {
                            EndReason::Expired
                        } else {
                            EndReason::Idle
                        },
                    );
                }
            }
            Err(error) => self.failed("session_sweep_failed", &error),
        }
        match store.sessions_count() {
            Ok(count) => self.shared_active.store(count, Ordering::Relaxed),
            Err(error) => self.failed("session_count_failed", &error),
        }
        let mut cache = self.sessions.lock().expect("session store mutex poisoned");
        self.prune_copies(&mut cache, Instant::now());
    }

    /// Closes the history rows of sessions that ended without anybody recording it (an instance
    /// died before it flushed). They end at the last time they were seen.
    pub fn close_lost_history(&self) {
        let Some(store) = &self.shared else { return };
        let before = unix_now() - self.idle.as_secs() as i64 - LOST_GRACE_SECS;
        if let Err(error) = store.close_lost_sessions(before, EndReason::Restart.as_str()) {
            self.failed("session_history_cleanup_failed", &error);
        }
    }

    /// Forgets copies that expired or were checked too long ago to be of use.
    fn prune_copies(&self, cache: &mut HashMap<[u8; 32], Entry>, now: Instant) {
        cache.retain(|_, entry| {
            entry.expires_at > now && now.duration_since(entry.checked) < STALE_GRACE
        });
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

fn session_of(stored: SharedSession) -> Session {
    Session {
        sid: stored.sid,
        principal: match stored.login {
            Some(login) => Principal::User(login),
            None => Principal::Superadmin,
        },
        impersonated: stored.impersonated,
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

#[cfg(test)]
mod shared_tests {
    //! The sessions of several instances, on a PostgreSQL database (skipped without one).

    use super::*;
    use crate::store::testing::postgres;
    use std::sync::mpsc::Receiver;
    use std::thread::sleep;

    const HOUR: Duration = Duration::from_secs(3600);
    const HERE: Binding = [1; 32];
    const ELSEWHERE: Binding = [2; 32];

    struct Instance {
        sessions: SessionStore,
        events: Receiver<Event>,
        store: Arc<Store>,
    }

    impl Instance {
        fn new(store: Store, ttl: Duration, idle: Duration, max: usize, cache_ttl: Duration) -> Self {
            let store = Arc::new(store);
            let (tracker, events) = Tracker::recording();
            let mut sessions = SessionStore::new(ttl, idle, max, tracker).shared(Arc::clone(&store));
            sessions.set_cache_ttl(cache_ttl);
            Self { sessions, events, store }
        }

        /// The sessions that ended on this instance, as it recorded them.
        fn ended(&self) -> Vec<(u64, EndReason)> {
            self.events
                .try_iter()
                .filter_map(|event| match event {
                    Event::SessionEnded { sid, reason, .. } => Some((sid, reason)),
                    _ => None,
                })
                .collect()
        }
    }

    fn instances(ttl: Duration, idle: Duration, max: usize, cache_ttl: Duration) -> Option<(Instance, Instance)> {
        let (first, second) = postgres()?.fresh_pair();
        Some((
            Instance::new(first, ttl, idle, max, cache_ttl),
            Instance::new(second, ttl, idle, max, cache_ttl),
        ))
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
    fn a_session_opened_on_one_instance_is_valid_on_the_other_until_it_ends() {
        let Some((a, b)) = instances(HOUR, HOUR, 8, Duration::ZERO) else { return };
        let token = a.sessions.issue(HERE, user("alice")).unwrap();
        let on_a = a.sessions.validate(&token, &HERE).unwrap();
        let on_b = b.sessions.validate(&token, &HERE).unwrap();
        assert_eq!(on_a, on_b);
        assert_eq!(on_b.principal, Principal::User("alice".into()));
        assert!(b.sessions.validate(&token, &ELSEWHERE).is_none(), "the binding holds across instances");
        assert!(a.sessions.validate(&token, &HERE).is_some(), "a refused thief does not end it");

        assert_eq!(a.sessions.revoke(&token, EndReason::Logout), Some(on_a));
        assert!(b.sessions.validate(&token, &HERE).is_none());
        assert!(b.sessions.revoke(&token, EndReason::Logout).is_none());
        let ended: Vec<_> = a.ended().into_iter().chain(b.ended()).collect();
        assert_eq!(ended.len(), 1, "one logout, recorded once: {ended:?}");
        assert_eq!(ended[0].1, EndReason::Logout);
    }

    #[test]
    fn the_superadmin_and_impersonation_survive_the_trip() {
        let Some((a, b)) = instances(HOUR, HOUR, 8, Duration::ZERO) else { return };
        let token = a
            .sessions
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
        assert_eq!(b.sessions.validate(&token, &HERE).unwrap().principal, Principal::Superadmin);
        let mut as_alice = user("alice");
        as_alice.impersonated = true;
        let token = a.sessions.issue(HERE, as_alice).unwrap();
        let seen = b.sessions.validate(&token, &HERE).unwrap();
        assert!(seen.impersonated);
        assert_eq!(seen.principal, Principal::User("alice".into()));
    }

    #[test]
    fn a_copy_keeps_the_request_path_off_the_database_but_not_for_longer_than_its_ttl() {
        let Some((a, b)) = instances(HOUR, HOUR, 8, Duration::from_millis(400)) else { return };
        let token = a.sessions.issue(HERE, user("alice")).unwrap();
        assert!(b.sessions.validate(&token, &HERE).is_some());
        a.sessions.revoke(&token, EndReason::Logout);
        assert!(
            b.sessions.validate(&token, &HERE).is_some(),
            "the other instance still holds a copy it checked a moment ago"
        );
        sleep(Duration::from_millis(500));
        assert!(b.sessions.validate(&token, &HERE).is_none(), "the copy is old: the database is asked again");
        assert_eq!(b.sessions.shared_errors(), 0);
    }

    #[test]
    fn a_session_is_in_the_history_the_moment_it_is_issued_and_is_not_announced_twice() {
        let Some((a, b)) = instances(HOUR, HOUR, 8, Duration::ZERO) else { return };
        a.sessions.issue(HERE, user("alice")).unwrap();
        let snapshot = b.store.snapshot(&[("alice".into(), false)], unix_now()).unwrap();
        assert_eq!(snapshot.accounts[0].recent.len(), 1);
        assert_eq!(snapshot.accounts[0].recent[0].ended_at, None);
        assert!(a.events.try_recv().is_err(), "the start was written with the session");
    }

    #[test]
    fn deleting_a_user_ends_their_sessions_wherever_they_were_opened() {
        let Some((a, b)) = instances(HOUR, HOUR, 8, Duration::ZERO) else { return };
        let alice1 = a.sessions.issue(HERE, user("alice")).unwrap();
        let alice2 = b.sessions.issue(HERE, user("alice")).unwrap();
        let bob = a.sessions.issue(HERE, user("bob")).unwrap();
        assert_eq!(b.sessions.revoke_user("alice", EndReason::UserDeleted), 2);
        assert!(a.sessions.validate(&alice1, &HERE).is_none());
        assert!(a.sessions.validate(&alice2, &HERE).is_none());
        assert!(a.sessions.validate(&bob, &HERE).is_some());
        assert_eq!(b.ended().len(), 2);
        assert!(b.ended().iter().all(|(_, reason)| *reason == EndReason::UserDeleted));
        assert_eq!(a.sessions.revoke_user("alice", EndReason::UserDeleted), 0);
    }

    #[test]
    fn a_session_that_ran_out_is_refused_at_once_and_recorded_by_one_instance_only() {
        let Some((a, b)) = instances(Duration::from_secs(1), HOUR, 8, Duration::ZERO) else { return };
        let token = a.sessions.issue(HERE, user("alice")).unwrap();
        assert!(b.sessions.validate(&token, &HERE).is_some());
        sleep(Duration::from_millis(2_200));
        assert!(b.sessions.validate(&token, &HERE).is_none(), "refused before any sweep");
        a.sessions.sweep();
        b.sessions.sweep();
        let ended: Vec<_> = a.ended().into_iter().chain(b.ended()).collect();
        assert_eq!(ended.len(), 1, "{ended:?}");
        assert_eq!(ended[0].1, EndReason::Expired);
        assert_eq!(a.sessions.active_count(), 0);
    }

    #[test]
    fn an_idle_session_is_ended_unless_the_other_instance_kept_it_alive() {
        let Some((a, b)) = instances(HOUR, Duration::from_secs(2), 8, Duration::ZERO) else { return };
        let kept = a.sessions.issue(HERE, user("alice")).unwrap();
        let dropped = a.sessions.issue(HERE, user("bob")).unwrap();
        for _ in 0..3 {
            sleep(Duration::from_millis(900));
            assert!(b.sessions.validate(&kept, &HERE).is_some(), "used on the other instance");
        }
        a.sessions.sweep();
        let ended = a.ended();
        assert_eq!(ended.len(), 1, "{ended:?}");
        assert_eq!(ended[0].1, EndReason::Idle);
        assert!(a.sessions.validate(&dropped, &HERE).is_none());
        assert!(a.sessions.validate(&kept, &HERE).is_some());
        assert_eq!(a.sessions.active_count(), 1);
    }

    #[test]
    fn the_capacity_counts_the_sessions_of_every_instance() {
        let Some((a, b)) = instances(HOUR, HOUR, 2, Duration::ZERO) else { return };
        a.sessions.issue(HERE, user("alice")).unwrap();
        b.sessions.issue(HERE, user("bob")).unwrap();
        assert_eq!(a.sessions.issue(HERE, user("carol")), Err(SessionIssueError::Capacity));
        assert_eq!(b.sessions.issue(HERE, user("carol")), Err(SessionIssueError::Capacity));
    }

    #[test]
    fn a_database_that_went_away_does_not_sign_everybody_out() {
        let Some((a, _b)) = instances(HOUR, HOUR, 8, Duration::ZERO) else { return };
        let token = a.sessions.issue(HERE, user("alice")).unwrap();
        assert!(a.sessions.validate(&token, &HERE).is_some());
        a.store.go_down();
        assert!(
            a.sessions.validate(&token, &HERE).is_some(),
            "checked a moment ago: still accepted"
        );
        assert!(a.sessions.shared_errors() >= 1, "and the failure is counted");
        assert!(
            a.sessions.validate(&token, &ELSEWHERE).is_none(),
            "the binding is still enforced from the copy"
        );
        let stranger = "A".repeat(43);
        assert!(a.sessions.validate(&stranger, &HERE).is_none(), "an unknown cookie is not");
        assert_eq!(a.sessions.issue(HERE, user("bob")), Err(SessionIssueError::Storage));
    }
}
