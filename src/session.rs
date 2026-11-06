//! The sessions: who a cookie stands for.
//!
//! With one instance, the sessions are in its memory. A restart ends them.
//! With a shared database, the sessions are in the database. A session opened on one instance is valid on the others and survives a restart.
//! Each instance keeps a short-lived copy of what it checked. The request path asks the database about one session once every few seconds, not once per request.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use base64ct::{Base64UrlUnpadded, Encoding};
use sha2::{Digest, Sha256};
use zeroize::Zeroize;

use crate::config::Remember;
use crate::store::{Idle, SharedSession, Store};
use crate::tracking::{log_error, unix_now, EndReason, Event, SessionStart, Tracker};
use crate::util::blocking;

/// How long an instance trusts the data it read about a session from the shared database.
/// A session ended on another instance (a logout or a deleted account) stops working here within this time.
const CACHE_TTL: Duration = Duration::from_secs(5);
/// When the shared database cannot be reached, the instance still accepts a session that it checked less than this time ago.
/// A failover of the database therefore does not sign out all users.
const STALE_GRACE: Duration = Duration::from_secs(120);
/// A history row is marked as lost only after this time has passed since the session could last have been used.
/// A row is lost when its instance stops before it records how the session ended.
/// The end of a session that did end is flushed within [`crate::tracking::FLUSH_EVERY`].
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

/// The stamp of the credential that a sign-in checked: a digest of the password hash of the account, or the
/// credential generation of the superadmin. A session carries the stamp of the proof that opened it. The session is
/// valid only while the principal has the same stamp. A new password, a removed password or a new account of the
/// login gives another stamp. The program makes the stamp and compares it (see `http::credential_stamp`). The store
/// keeps it.
pub type Credential = [u8; 32];

/// Who a session belongs to.
/// The superadmin is the account defined by the environment.
/// The database stores the other accounts. The system looks them up by login.
/// Their roles are read live. A change or a deletion applies to a session that is already open.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Principal {
    Superadmin,
    User(String),
}

/// What a valid cookie stands for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Session {
    /// Random identifier of the session in the history. It is not the cookie value, which is secret.
    pub sid: u64,
    pub principal: Principal,
    /// The superadmin browses as this user. This is not a connection of the user.
    pub impersonated: bool,
    /// The person asked "Remember me" at the sign-in. The session has the longer life and the longer
    /// idle time of `session.remember_ttl` and `session.remember_idle_timeout`.
    pub remembered: bool,
    /// The sign-in used the second factor. A session that "Log in as" opens never has it: the session of the
    /// superadmin is the one that counts.
    pub mfa: bool,
    /// The absolute end of the session, in Unix time.
    pub expires_at: i64,
    /// The stamp of the credential that opened the session.
    pub credential: Credential,
}

/// What `issue` needs to open a session and to describe it in the history.
#[derive(Clone, Debug)]
pub struct NewSession {
    pub principal: Principal,
    /// The stamp of the credential that the proof of the sign-in checked. For a switch of session, the stamp of the
    /// principal of the new session.
    pub credential: Credential,
    /// The login shown in the history. For the superadmin, this is the superadmin's own login.
    pub login: String,
    pub impersonated: bool,
    /// The person asked "Remember me". The store ignores the request when `session.remember` is
    /// `false`, and for an impersonated session: such a session always has the short rule.
    pub remember: bool,
    /// The sign-in used the second factor. The store ignores the flag for an impersonated session.
    pub mfa: bool,
    pub ip: Option<String>,
    pub user_agent: String,
    /// The latest end of the new session, in Unix time. A switch of session ("Log in as",
    /// "Back to superadmin") gives the end of the current session. Thus a switch never extends
    /// the lifetime that the password sign-in gave. `None` gives the full `session.ttl`, or
    /// `session.remember_ttl` for a remembered session.
    pub not_after: Option<i64>,
}

#[derive(Debug)]
struct Entry {
    expires_at: Instant,
    last_seen: Instant,
    /// The time when the shared database last confirmed the session. Not used with one instance.
    checked: Instant,
    binding: Binding,
    session: Session,
}

pub struct SessionStore {
    /// The sessions of this instance. With a shared database, these are the copies that this instance keeps.
    sessions: Mutex<HashMap<[u8; 32], Entry>>,
    ttl: Duration,
    idle: Duration,
    /// The rule of the remembered sessions. `None`: the store opens no remembered session.
    remember: Option<Remember>,
    max_active: usize,
    /// The largest number of live sessions of one principal. An impersonated session does not count.
    max_per_account: usize,
    tracker: Arc<Tracker>,
    shared: Option<Arc<Store>>,
    /// The credential generation of the superadmin (see [`crate::crypto::superadmin_generation`]).
    /// The shared database refuses a superadmin or impersonated session of another generation.
    generation: [u8; 32],
    cache_ttl: Duration,
    /// The live sessions of every instance, as the last sweep counted them.
    shared_active: AtomicUsize,
    shared_errors: AtomicU64,
}

impl SessionStore {
    pub fn new(ttl: Duration, idle: Duration, max_active: usize, tracker: Arc<Tracker>) -> Self {
        Self {
            sessions: Mutex::new(HashMap::new()),
            ttl,
            idle,
            remember: None,
            max_active,
            max_per_account: usize::MAX,
            tracker,
            shared: None,
            generation: [0; 32],
            cache_ttl: CACHE_TTL,
            shared_active: AtomicUsize::new(0),
            shared_errors: AtomicU64::new(0),
        }
    }

    /// Lets a sign-in open a remembered session, with the life and the idle time of `remember`.
    pub fn remember(mut self, remember: Remember) -> Self {
        self.remember = Some(remember);
        self
    }

    /// Limits the live sessions of one principal to `max` (`session.max_sessions_per_account`). A new session of a
    /// principal that has `max` sessions ends the session of that principal that was used least recently. Thus one
    /// account never takes the places of the other accounts. An impersonated session does not count and ends no
    /// session.
    pub fn per_account(mut self, max: usize) -> Self {
        self.max_per_account = max;
        self
    }

    /// The limits of the shared database for a new session.
    fn limits(&self) -> crate::store::SessionLimits {
        crate::store::SessionLimits {
            max_active: self.max_active,
            max_per_account: self.max_per_account,
        }
    }

    /// Whether a new session gets the remembered rule.
    fn is_remembered(&self, new: &NewSession) -> bool {
        new.remember && !new.impersonated && self.remember.is_some()
    }

    /// Whether a new session records the second factor. An impersonated session records none.
    fn has_mfa(new: &NewSession) -> bool {
        new.mfa && !new.impersonated
    }

    /// The life of a session of the rule that `remembered` selects.
    fn ttl_of(&self, remembered: bool) -> Duration {
        match (remembered, &self.remember) {
            (true, Some(remember)) => remember.ttl,
            _ => self.ttl,
        }
    }

    /// The idle time of a session of the rule that `remembered` selects.
    fn idle_of(&self, remembered: bool) -> Duration {
        match (remembered, &self.remember) {
            (true, Some(remember)) => remember.idle_timeout,
            _ => self.idle,
        }
    }

    /// The idle times of both rules, for the shared database.
    fn idle_pair(&self) -> Idle {
        Idle::new(self.idle, self.idle_of(true))
    }

    /// Keeps the sessions in the shared `store`, not in memory. `generation` is the credential
    /// generation of the superadmin. The store accepts a superadmin or impersonated session only
    /// when its row carries this generation.
    pub fn shared(mut self, store: Arc<Store>, generation: [u8; 32]) -> Self {
        self.shared = Some(store);
        self.generation = generation;
        self
    }

    /// The end of a session issued at `now`: the life of its rule later, but not after `not_after`.
    fn expiry(&self, now: i64, not_after: Option<i64>, remembered: bool) -> i64 {
        let full = now.saturating_add(self.ttl_of(remembered).as_secs() as i64);
        not_after.map_or(full, |bound| bound.min(full))
    }

    /// The life of a session issued at `now`: the life of its rule, but not past `not_after`.
    fn lifetime(&self, now: i64, not_after: Option<i64>, remembered: bool) -> Duration {
        let ttl = self.ttl_of(remembered);
        match not_after {
            None => ttl,
            Some(bound) => ttl.min(Duration::from_secs(bound.saturating_sub(now).max(0) as u64)),
        }
    }

    /// Ends the superadmin and impersonated sessions of the other credential generations in the
    /// shared database. The program calls this at start. A rotation of the superadmin login, of
    /// its password or of the session secret thus ends the old sessions on every instance.
    /// Returns the number of sessions ended. The value is always 0 with one instance.
    pub fn end_other_generations(&self) -> usize {
        let Some(store) = &self.shared else { return 0 };
        match blocking(|| store.sessions_end_other_generations(&self.generation)) {
            Ok(ended) => {
                for sid in &ended {
                    self.ended_sid(*sid, EndReason::Revoked);
                }
                ended.len()
            }
            Err(error) => {
                self.failed("session_generation_cleanup_failed", &error);
                0
            }
        }
    }

    #[cfg(test)]
    pub fn set_cache_ttl(&mut self, cache_ttl: Duration) {
        self.cache_ttl = cache_ttl;
    }

    /// Failures of the shared database that the sessions met. The value is always 0 with one instance.
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
        let now_unix = unix_now();
        let remembered = self.is_remembered(&new);
        let mfa = Self::has_mfa(&new);
        let expires_unix = self.expiry(now_unix, new.not_after, remembered);
        let lifetime = self.lifetime(now_unix, new.not_after, remembered);
        let now = Instant::now();
        let mut sessions = self.sessions.lock().expect("session store mutex poisoned");
        self.prune(&mut sessions, now);
        if !new.impersonated {
            self.make_room_for(&mut sessions, &new.principal);
        }
        if sessions.len() >= self.max_active {
            return Err(SessionIssueError::Capacity);
        }

        // A 256-bit random token has a negligible collision probability.
        // Keep the insertion fail-closed and bounded, even if the OS RNG misbehaves.
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
                remembered,
                mfa,
                expires_at: expires_unix,
                credential: new.credential,
            };
            self.tracker.emit(Event::SessionStarted(SessionStart {
                sid: session.sid,
                login: new.login,
                superadmin: new.principal == Principal::Superadmin,
                impersonated: new.impersonated,
                remembered,
                mfa,
                ip: new.ip,
                user_agent: new.user_agent,
                at: unix_now(),
            }));
            sessions.insert(
                digest,
                Entry {
                    expires_at: now + lifetime,
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

    /// The session that `token` stands for. The session must be live and must have been issued for `binding`.
    /// A valid use renews the idle timer.
    /// A binding that does not match means the cookie is used from elsewhere. The use is refused.
    /// The session is left alone. A thief cannot end the owner's session by trying it.
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

    /// The same as [`Self::validate`], and the idle timer stays as it is. A page that asks in the
    /// background uses this: an open page must not keep a session alive, and its requests must
    /// not write to the shared database.
    pub fn peek(&self, token: &str, binding: &Binding) -> Option<Session> {
        let digest = digest_encoded_token(token)?;
        if let Some(store) = &self.shared {
            return self.peek_shared(store, digest, binding);
        }
        let now = Instant::now();
        let mut sessions = self.sessions.lock().expect("session store mutex poisoned");
        self.prune(&mut sessions, now);
        match sessions.get(&digest) {
            Some(entry) if entry.binding == *binding => Some(entry.session.clone()),
            _ => None,
        }
    }

    /// Ends the session that `token` stands for. Returns the session.
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

    /// Ends every session of one user. Use this when the account was deleted. Returns the number of sessions ended.
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

    /// Ends every session of every account, the superadmin included. This is the incident action.
    /// With a shared database, it ends the sessions of all instances. Another instance stops accepting
    /// its copy of a session within [`CACHE_TTL`]. Returns the number of sessions ended.
    /// If the shared database fails, the sessions of this instance end and the call returns the error.
    pub fn end_all(&self, reason: EndReason) -> Result<usize, String> {
        let mut sessions = self.sessions.lock().expect("session store mutex poisoned");
        let copies: Vec<Session> = sessions.drain().map(|(_, entry)| entry.session).collect();
        drop(sessions);
        let Some(store) = &self.shared else {
            for session in &copies {
                self.ended(session, reason);
            }
            return Ok(copies.len());
        };
        match blocking(|| store.sessions_delete_all()) {
            Ok(ended) => {
                for sid in &ended {
                    self.ended_sid(*sid, reason);
                }
                Ok(ended.len())
            }
            Err(error) => {
                self.failed("session_end_all_failed", &error);
                Err(error)
            }
        }
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
        let remembered = self.is_remembered(&new);
        let mfa = Self::has_mfa(&new);
        let expires_at = self.expiry(now, new.not_after, remembered);
        let session = Session {
            sid: u64::from_be_bytes(sid),
            principal: new.principal.clone(),
            impersonated: new.impersonated,
            remembered,
            mfa,
            expires_at,
            credential: new.credential,
        };
        let stored = SharedSession {
            sid: session.sid,
            login: match &new.principal {
                Principal::Superadmin => None,
                Principal::User(login) => Some(login.clone()),
            },
            impersonated: new.impersonated,
            remembered,
            mfa,
            expires_at,
            credential: new.credential,
        };
        // The history row is written with the session. Therefore, no instance meets a session without a history row.
        let start = SessionStart {
            sid: session.sid,
            login: new.login,
            superadmin: new.principal == Principal::Superadmin,
            impersonated: new.impersonated,
            remembered,
            mfa,
            ip: new.ip,
            user_agent: new.user_agent,
            at: now,
        };
        let inserted = blocking(|| {
            store.session_insert(
                &digest,
                &binding,
                &stored,
                &start,
                self.limits(),
                &self.generation,
            )
        });
        let ended = match inserted {
            Ok(Some(ended)) => ended,
            Ok(None) => return Err(SessionIssueError::Capacity),
            Err(error) => {
                self.failed("session_insert_failed", &error);
                return Err(SessionIssueError::Storage);
            }
        };
        for sid in &ended {
            self.ended_sid(*sid, EndReason::Revoked);
        }
        let at = Instant::now();
        let lifetime = self.lifetime(now, new.not_after, remembered);
        let mut cache = self.sessions.lock().expect("session store mutex poisoned");
        self.prune_copies(&mut cache, at);
        // The sessions that the new one replaced end here at once. Another instance refuses its copy at the end of the
        // life of a copy.
        cache.retain(|_, entry| !ended.contains(&entry.session.sid));
        cache.insert(
            digest,
            Entry {
                expires_at: at + lifetime,
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
                // The cookie is used from elsewhere. Refuse the request. Leave the owner's session alone.
                return None;
            }
            if now.duration_since(*checked) < self.cache_ttl {
                self.tracker.touch(session.sid, unix_now());
                return Some(session.clone());
            }
        }
        let at = unix_now();
        match blocking(|| {
            store.session_touch(&digest, binding, at, self.idle_pair(), &self.generation)
        }) {
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

    fn peek_shared(&self, store: &Store, digest: [u8; 32], binding: &Binding) -> Option<Session> {
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
                return None;
            }
            if now.duration_since(*checked) < self.cache_ttl {
                return Some(session.clone());
            }
        }
        match blocking(|| {
            store.session_peek(
                &digest,
                binding,
                unix_now(),
                self.idle_pair(),
                &self.generation,
            )
        }) {
            Ok(Some(stored)) => Some(session_of(stored)),
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
            // Another instance ended the session first, and recorded it.
            Ok(None) => None,
            Err(error) => {
                // The session stays valid on other instances until it expires. Count and log the failure.
                self.failed("session_revoke_failed", &error);
                copy.map(|entry| entry.session)
            }
        }
    }

    fn revoke_user_shared(&self, store: &Store, login: &str, reason: EndReason) -> usize {
        self.sessions
            .lock()
            .expect("session store mutex poisoned")
            .retain(
                |_, entry| !matches!(&entry.session.principal, Principal::User(l) if l == login),
            );
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

    /// Ends the sessions that ran out on any instance. A session runs out by its lifetime or by its idle time.
    /// Records how each session ended. Counts the live sessions.
    /// Every instance runs this every few seconds. The database gives each ended session to exactly one instance.
    pub fn sweep(&self) {
        let Some(store) = &self.shared else { return };
        let now = unix_now();
        match store.sessions_expire(now, self.idle_pair()) {
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

    /// Closes the history rows of sessions that nobody recorded as ended.
    /// This happens when an instance stops before it flushes its records.
    /// Each row ends at the last time its session was seen.
    pub fn close_lost_history(&self) {
        let Some(store) = &self.shared else { return };
        let before = unix_now() - self.idle.as_secs() as i64 - LOST_GRACE_SECS;
        if let Err(error) = store.close_lost_sessions(before, EndReason::Restart.as_str()) {
            self.failed("session_history_cleanup_failed", &error);
        }
    }

    /// Removes copies that expired. Also removes copies that were checked too long ago to be useful.
    fn prune_copies(&self, cache: &mut HashMap<[u8; 32], Entry>, now: Instant) {
        cache.retain(|_, entry| {
            entry.expires_at > now && now.duration_since(entry.checked) < STALE_GRACE
        });
    }

    /// Ends the sessions of `principal` that were used least recently, until a new session of the principal makes at
    /// most `max_per_account`. An impersonated session does not count.
    fn make_room_for(&self, sessions: &mut HashMap<[u8; 32], Entry>, principal: &Principal) {
        let mut mine: Vec<([u8; 32], Instant)> = sessions
            .iter()
            .filter(|(_, entry)| {
                entry.session.principal == *principal && !entry.session.impersonated
            })
            .map(|(digest, entry)| (*digest, entry.last_seen))
            .collect();
        if mine.len() < self.max_per_account {
            return;
        }
        mine.sort_by_key(|(_, seen)| *seen);
        let surplus = mine.len() + 1 - self.max_per_account;
        for (digest, _) in mine.into_iter().take(surplus) {
            if let Some(entry) = sessions.remove(&digest) {
                self.ended(&entry.session, EndReason::Revoked);
            }
        }
    }

    fn prune(&self, sessions: &mut HashMap<[u8; 32], Entry>, now: Instant) {
        sessions.retain(|_, entry| {
            let live = entry.expires_at > now;
            let active =
                now.duration_since(entry.last_seen) < self.idle_of(entry.session.remembered);
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
        remembered: stored.remembered,
        mfa: stored.mfa,
        expires_at: stored.expires_at,
        credential: stored.credential,
    }
}

fn digest_encoded_token(token: &str) -> Option<[u8; 32]> {
    // 32 bytes, encoded as unpadded base64url, are exactly 43 ASCII bytes.
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
            credential: [0; 32],
            login: login.into(),
            impersonated: false,
            remember: false,
            mfa: false,
            ip: Some("10.0.0.2".into()),
            user_agent: "Browser/1.0".into(),
            not_after: None,
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
    fn end_all_ends_every_session_the_superadmin_included_and_records_why() {
        let (tracker, events) = Tracker::recording();
        let store = SessionStore::new(HOUR, HOUR, 8, tracker);
        let alice = store.issue(HERE, user("alice")).unwrap();
        let bob = store.issue(HERE, user("bob")).unwrap();
        let root = store
            .issue(
                HERE,
                NewSession {
                    principal: Principal::Superadmin,
                    credential: [0; 32],
                    login: "root".into(),
                    impersonated: false,
                    remember: false,
                    mfa: false,
                    ip: None,
                    user_agent: String::new(),
                    not_after: None,
                },
            )
            .unwrap();
        assert_eq!(store.end_all(EndReason::Revoked), Ok(3));
        for token in [&alice, &bob, &root] {
            assert!(store.validate(token, &HERE).is_none());
        }
        assert_eq!(store.active_count(), 0);
        let reasons: Vec<EndReason> = events
            .try_iter()
            .filter_map(|event| match event {
                Event::SessionEnded { reason, .. } => Some(reason),
                _ => None,
            })
            .collect();
        assert_eq!(reasons, vec![EndReason::Revoked; 3]);
        assert_eq!(store.end_all(EndReason::Revoked), Ok(0), "nothing is left");
        assert!(
            store.issue(HERE, user("alice")).is_ok(),
            "a new sign-in works after the incident"
        );
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

    /// One principal keeps at most its number of sessions: its new session ends its session that was used least
    /// recently. Another principal and an impersonated session take no place of it.
    #[test]
    fn one_principal_keeps_its_number_of_sessions_and_its_least_used_one_ends() {
        let (tracker, events) = Tracker::recording();
        let store = SessionStore::new(HOUR, HOUR, 8, tracker).per_account(2);
        let first = store.issue(HERE, user("mallory")).unwrap();
        let second = store.issue(HERE, user("mallory")).unwrap();
        std::thread::sleep(Duration::from_millis(5));
        assert!(store.validate(&first, &HERE).is_some());
        let third = store.issue(HERE, user("mallory")).unwrap();
        assert!(
            store.validate(&second, &HERE).is_none(),
            "the session used least recently ended"
        );
        assert!(store.validate(&first, &HERE).is_some());
        assert!(store.validate(&third, &HERE).is_some());
        let bob = store.issue(HERE, user("bob")).unwrap();
        let mut as_mallory = user("mallory");
        as_mallory.impersonated = true;
        store.issue(HERE, as_mallory).unwrap();
        assert_eq!(store.active_count(), 4);
        for token in [&first, &third, &bob] {
            assert!(store.validate(token, &HERE).is_some());
        }
        let ended: Vec<EndReason> = events
            .try_iter()
            .filter_map(|event| match event {
                Event::SessionEnded { reason, .. } => Some(reason),
                _ => None,
            })
            .collect();
        assert_eq!(ended, [EndReason::Revoked]);
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
    fn a_peek_does_not_renew_the_idle_timer() {
        let store = store(HOUR, Duration::from_millis(120), 4);
        let token = store.issue(HERE, user("alice")).unwrap();
        for _ in 0..3 {
            std::thread::sleep(Duration::from_millis(60));
            // The first peeks fall inside the idle window of the issue.
            let _ = store.peek(&token, &HERE);
        }
        assert!(
            store.peek(&token, &HERE).is_none(),
            "a peek never kept the session alive"
        );
        assert!(store.peek(&token, &ELSEWHERE).is_none());
        let other = store.issue(HERE, user("bob")).unwrap();
        assert!(store.peek(&other, &HERE).is_some());
        assert!(
            store.peek(&other, &ELSEWHERE).is_none(),
            "the binding holds for a peek"
        );
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
                    credential: [0; 32],
                    login: "root".into(),
                    impersonated: false,
                    remember: false,
                    mfa: false,
                    ip: None,
                    user_agent: String::new(),
                    not_after: None,
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

    /// "Log in as" and "Back to superadmin" open new sessions. They carry the end of the session
    /// that the password opened: a switch never gives a fresh lifetime.
    #[test]
    fn a_session_issued_with_an_upper_bound_ends_at_that_bound() {
        let store = store(HOUR, HOUR, 8);
        let first = store.issue(HERE, user("alice")).unwrap();
        let bound = store.validate(&first, &HERE).unwrap().expires_at;
        let now = unix_now();
        assert!((now + 3599..=now + 3600).contains(&bound), "{bound} {now}");

        let mut next = user("alice");
        next.not_after = Some(bound);
        let token = store.issue(HERE, next).unwrap();
        assert_eq!(store.validate(&token, &HERE).unwrap().expires_at, bound);

        let mut ended = user("alice");
        ended.not_after = Some(unix_now());
        let token = store.issue(HERE, ended).unwrap();
        assert!(
            store.validate(&token, &HERE).is_none(),
            "the bound has passed: the session is already over"
        );
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

    fn remembering(ttl: Duration, idle: Duration, remember: Remember) -> SessionStore {
        store(ttl, idle, 8).remember(remember)
    }

    fn remembered(login: &str) -> NewSession {
        NewSession {
            remember: true,
            ..user(login)
        }
    }

    const DAY: Duration = Duration::from_secs(86_400);

    #[test]
    fn a_remembered_session_lives_for_the_remember_ttl_and_the_others_for_the_ttl() {
        let remember = Remember {
            ttl: 30 * DAY,
            idle_timeout: 7 * DAY,
        };
        let store = remembering(DAY, Duration::from_secs(1800), remember);
        let now = unix_now();
        let short = store.issue(HERE, user("alice")).unwrap();
        let long = store.issue(HERE, remembered("alice")).unwrap();
        let short = store.validate(&short, &HERE).unwrap();
        let long = store.validate(&long, &HERE).unwrap();
        assert!(!short.remembered);
        assert!(long.remembered);
        assert!((now + 86_399..=now + 86_401).contains(&short.expires_at));
        let thirty = 30 * 86_400;
        assert!((now + thirty - 1..=now + thirty + 1).contains(&long.expires_at));
    }

    #[test]
    fn a_remember_request_is_ignored_without_the_rule_and_for_an_impersonated_session() {
        // `session.remember = false`: the store has no rule.
        let store = store(HOUR, HOUR, 8);
        let token = store.issue(HERE, remembered("alice")).unwrap();
        let session = store.validate(&token, &HERE).unwrap();
        assert!(!session.remembered);
        let now = unix_now();
        assert!((now + 3599..=now + 3600).contains(&session.expires_at));

        // "Log in as": the short rule, whatever the request says.
        let remember = Remember {
            ttl: 30 * DAY,
            idle_timeout: 7 * DAY,
        };
        let store = remembering(HOUR, HOUR, remember);
        let mut new = remembered("alice");
        new.impersonated = true;
        let token = store.issue(HERE, new).unwrap();
        let session = store.validate(&token, &HERE).unwrap();
        assert!(session.impersonated && !session.remembered);
        assert!((now + 3599..=now + 3601).contains(&session.expires_at));
    }

    #[test]
    fn a_remembered_session_has_its_own_idle_time() {
        let remember = Remember {
            ttl: HOUR,
            idle_timeout: Duration::from_secs(5),
        };
        let store = remembering(HOUR, Duration::from_millis(120), remember);
        let short = store.issue(HERE, user("alice")).unwrap();
        let long = store.issue(HERE, remembered("alice")).unwrap();
        std::thread::sleep(Duration::from_millis(250));
        assert!(
            store.validate(&short, &HERE).is_none(),
            "idle for longer than the idle time of the normal rule"
        );
        assert!(
            store.validate(&long, &HERE).is_some(),
            "the idle time of a remembered session is longer"
        );
        assert_eq!(store.active_count(), 1);
    }

    #[test]
    fn a_remembered_session_in_use_still_ends_at_its_absolute_life() {
        let remember = Remember {
            ttl: Duration::from_millis(300),
            idle_timeout: Duration::from_millis(250),
        };
        let store = remembering(HOUR, HOUR, remember);
        let token = store.issue(HERE, remembered("alice")).unwrap();
        for _ in 0..4 {
            std::thread::sleep(Duration::from_millis(60));
            assert!(
                store.validate(&token, &HERE).is_some(),
                "in use, inside its life"
            );
        }
        std::thread::sleep(Duration::from_millis(100));
        assert!(store.validate(&token, &HERE).is_none(), "past its life");
    }

    #[test]
    fn the_history_row_of_a_session_says_whether_it_is_remembered() {
        let remember = Remember {
            ttl: HOUR,
            idle_timeout: HOUR,
        };
        let (tracker, events) = Tracker::recording();
        let store = SessionStore::new(HOUR, HOUR, 8, tracker).remember(remember);
        store.issue(HERE, user("alice")).unwrap();
        store.issue(HERE, remembered("alice")).unwrap();
        let flags: Vec<bool> = events
            .try_iter()
            .filter_map(|event| match event {
                Event::SessionStarted(start) => Some(start.remembered),
                _ => None,
            })
            .collect();
        assert_eq!(flags, [false, true]);
    }
}

#[cfg(test)]
mod shared_tests {
    //! The sessions of several instances, on a PostgreSQL database. The tests are skipped without one.

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
        fn new(
            store: Store,
            ttl: Duration,
            idle: Duration,
            max: usize,
            cache_ttl: Duration,
        ) -> Self {
            Self::build(store, ttl, idle, max, cache_ttl, [0; 32], None)
        }

        /// An instance that opens remembered sessions with the rule `remember`.
        fn remembering(store: Store, ttl: Duration, idle: Duration, remember: Remember) -> Self {
            Self::build(store, ttl, idle, 8, Duration::ZERO, [0; 32], Some(remember))
        }

        /// An instance whose superadmin credentials give `generation`.
        fn with_generation(store: Store, generation: [u8; 32]) -> Self {
            Self::build(store, HOUR, HOUR, 8, Duration::ZERO, generation, None)
        }

        fn build(
            store: Store,
            ttl: Duration,
            idle: Duration,
            max: usize,
            cache_ttl: Duration,
            generation: [u8; 32],
            remember: Option<Remember>,
        ) -> Self {
            let store = Arc::new(store);
            let (tracker, events) = Tracker::recording();
            let mut sessions =
                SessionStore::new(ttl, idle, max, tracker).shared(Arc::clone(&store), generation);
            if let Some(remember) = remember {
                sessions = sessions.remember(remember);
            }
            sessions.set_cache_ttl(cache_ttl);
            Self {
                sessions,
                events,
                store,
            }
        }

        /// The sessions that ended on this instance, as this instance recorded them.
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

    fn instances(
        ttl: Duration,
        idle: Duration,
        max: usize,
        cache_ttl: Duration,
    ) -> Option<(Instance, Instance)> {
        let (first, second) = postgres()?.fresh_pair();
        Some((
            Instance::new(first, ttl, idle, max, cache_ttl),
            Instance::new(second, ttl, idle, max, cache_ttl),
        ))
    }

    fn user(login: &str) -> NewSession {
        NewSession {
            principal: Principal::User(login.into()),
            credential: [0; 32],
            login: login.into(),
            impersonated: false,
            remember: false,
            mfa: false,
            ip: Some("10.0.0.2".into()),
            user_agent: "Browser/1.0".into(),
            not_after: None,
        }
    }

    #[test]
    fn a_session_opened_on_one_instance_is_valid_on_the_other_until_it_ends() {
        let Some((a, b)) = instances(HOUR, HOUR, 8, Duration::ZERO) else {
            return;
        };
        let token = a.sessions.issue(HERE, user("alice")).unwrap();
        let on_a = a.sessions.validate(&token, &HERE).unwrap();
        let on_b = b.sessions.validate(&token, &HERE).unwrap();
        assert_eq!(on_a, on_b);
        assert_eq!(on_b.principal, Principal::User("alice".into()));
        assert!(
            b.sessions.validate(&token, &ELSEWHERE).is_none(),
            "the binding holds across instances"
        );
        assert!(
            a.sessions.validate(&token, &HERE).is_some(),
            "a refused thief does not end it"
        );

        assert_eq!(a.sessions.revoke(&token, EndReason::Logout), Some(on_a));
        assert!(b.sessions.validate(&token, &HERE).is_none());
        assert!(b.sessions.revoke(&token, EndReason::Logout).is_none());
        let ended: Vec<_> = a.ended().into_iter().chain(b.ended()).collect();
        assert_eq!(ended.len(), 1, "one logout, recorded once: {ended:?}");
        assert_eq!(ended[0].1, EndReason::Logout);
    }

    #[test]
    fn the_superadmin_and_impersonation_survive_the_trip() {
        let Some((a, b)) = instances(HOUR, HOUR, 8, Duration::ZERO) else {
            return;
        };
        let token = a
            .sessions
            .issue(
                HERE,
                NewSession {
                    principal: Principal::Superadmin,
                    credential: [0; 32],
                    login: "root".into(),
                    impersonated: false,
                    remember: false,
                    mfa: false,
                    ip: None,
                    user_agent: String::new(),
                    not_after: None,
                },
            )
            .unwrap();
        assert_eq!(
            b.sessions.validate(&token, &HERE).unwrap().principal,
            Principal::Superadmin
        );
        let mut as_alice = user("alice");
        as_alice.impersonated = true;
        let token = a.sessions.issue(HERE, as_alice).unwrap();
        let seen = b.sessions.validate(&token, &HERE).unwrap();
        assert!(seen.impersonated);
        assert_eq!(seen.principal, Principal::User("alice".into()));
    }

    #[test]
    fn a_copy_keeps_the_request_path_off_the_database_but_not_for_longer_than_its_ttl() {
        let Some((a, b)) = instances(HOUR, HOUR, 8, Duration::from_millis(400)) else {
            return;
        };
        let token = a.sessions.issue(HERE, user("alice")).unwrap();
        assert!(b.sessions.validate(&token, &HERE).is_some());
        a.sessions.revoke(&token, EndReason::Logout);
        assert!(
            b.sessions.validate(&token, &HERE).is_some(),
            "the other instance still holds a copy it checked a moment ago"
        );
        sleep(Duration::from_millis(500));
        assert!(
            b.sessions.validate(&token, &HERE).is_none(),
            "the copy is old: the database is asked again"
        );
        assert_eq!(b.sessions.shared_errors(), 0);
    }

    #[test]
    fn a_session_is_in_the_history_the_moment_it_is_issued_and_is_not_announced_twice() {
        let Some((a, b)) = instances(HOUR, HOUR, 8, Duration::ZERO) else {
            return;
        };
        a.sessions.issue(HERE, user("alice")).unwrap();
        let snapshot = b
            .store
            .snapshot(&[("alice".into(), false)], unix_now())
            .unwrap();
        assert_eq!(snapshot.accounts[0].recent.len(), 1);
        assert_eq!(snapshot.accounts[0].recent[0].ended_at, None);
        assert!(
            a.events.try_recv().is_err(),
            "the start was written with the session"
        );
    }

    #[test]
    fn deleting_a_user_ends_their_sessions_wherever_they_were_opened() {
        let Some((a, b)) = instances(HOUR, HOUR, 8, Duration::ZERO) else {
            return;
        };
        let alice1 = a.sessions.issue(HERE, user("alice")).unwrap();
        let alice2 = b.sessions.issue(HERE, user("alice")).unwrap();
        let bob = a.sessions.issue(HERE, user("bob")).unwrap();
        assert_eq!(b.sessions.revoke_user("alice", EndReason::UserDeleted), 2);
        assert!(a.sessions.validate(&alice1, &HERE).is_none());
        assert!(a.sessions.validate(&alice2, &HERE).is_none());
        assert!(a.sessions.validate(&bob, &HERE).is_some());
        assert_eq!(b.ended().len(), 2);
        assert!(b
            .ended()
            .iter()
            .all(|(_, reason)| *reason == EndReason::UserDeleted));
        assert_eq!(a.sessions.revoke_user("alice", EndReason::UserDeleted), 0);
    }

    #[test]
    fn a_session_that_ran_out_is_refused_at_once_and_recorded_by_one_instance_only() {
        // The end of a session is a Unix second. A life of 1 second can end less than 1 second after the issue, when
        // the issue falls at the end of a second. A life of 3 seconds leaves 2 seconds for the first check.
        let Some((a, b)) = instances(Duration::from_secs(3), HOUR, 8, Duration::ZERO) else {
            return;
        };
        let token = a.sessions.issue(HERE, user("alice")).unwrap();
        assert!(b.sessions.validate(&token, &HERE).is_some());
        sleep(Duration::from_millis(3_200));
        assert!(
            b.sessions.validate(&token, &HERE).is_none(),
            "refused before any sweep"
        );
        a.sessions.sweep();
        b.sessions.sweep();
        let ended: Vec<_> = a.ended().into_iter().chain(b.ended()).collect();
        assert_eq!(ended.len(), 1, "{ended:?}");
        assert_eq!(ended[0].1, EndReason::Expired);
        assert_eq!(a.sessions.active_count(), 0);
    }

    #[test]
    fn an_idle_session_is_ended_unless_the_other_instance_kept_it_alive() {
        let Some((a, b)) = instances(HOUR, Duration::from_secs(2), 8, Duration::ZERO) else {
            return;
        };
        let kept = a.sessions.issue(HERE, user("alice")).unwrap();
        let dropped = a.sessions.issue(HERE, user("bob")).unwrap();
        for _ in 0..3 {
            sleep(Duration::from_millis(900));
            assert!(
                b.sessions.validate(&kept, &HERE).is_some(),
                "used on the other instance"
            );
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
    fn a_peek_on_the_shared_database_reads_and_does_not_keep_the_session_alive() {
        let Some((a, b)) = instances(HOUR, Duration::from_secs(2), 8, Duration::ZERO) else {
            return;
        };
        let token = a.sessions.issue(HERE, user("alice")).unwrap();
        for _ in 0..3 {
            sleep(Duration::from_millis(900));
            let _ = b.sessions.peek(&token, &HERE);
        }
        assert!(
            b.sessions.peek(&token, &HERE).is_none(),
            "idle for longer than the timeout, whatever the peeks"
        );
        let fresh = a.sessions.issue(HERE, user("bob")).unwrap();
        assert!(b.sessions.peek(&fresh, &HERE).is_some());
        assert!(b.sessions.peek(&fresh, &ELSEWHERE).is_none());
    }

    #[test]
    fn the_capacity_counts_the_sessions_of_every_instance() {
        let Some((a, b)) = instances(HOUR, HOUR, 2, Duration::ZERO) else {
            return;
        };
        a.sessions.issue(HERE, user("alice")).unwrap();
        b.sessions.issue(HERE, user("bob")).unwrap();
        assert_eq!(
            a.sessions.issue(HERE, user("carol")),
            Err(SessionIssueError::Capacity)
        );
        assert_eq!(
            b.sessions.issue(HERE, user("carol")),
            Err(SessionIssueError::Capacity)
        );
    }

    #[test]
    fn a_shared_session_issued_with_an_upper_bound_ends_at_that_bound() {
        let Some((a, b)) = instances(HOUR, HOUR, 8, Duration::ZERO) else {
            return;
        };
        let first = a.sessions.issue(HERE, user("alice")).unwrap();
        let bound = b.sessions.validate(&first, &HERE).unwrap().expires_at;
        let mut next = user("alice");
        next.not_after = Some(bound);
        let token = a.sessions.issue(HERE, next).unwrap();
        assert_eq!(
            b.sessions.validate(&token, &HERE).unwrap().expires_at,
            bound
        );
        let mut ended = user("alice");
        ended.not_after = Some(unix_now());
        let token = a.sessions.issue(HERE, ended).unwrap();
        assert!(b.sessions.validate(&token, &HERE).is_none());
    }

    fn superadmin(impersonating: Option<&str>) -> NewSession {
        match impersonating {
            None => NewSession {
                principal: Principal::Superadmin,
                credential: [0; 32],
                login: "root".into(),
                impersonated: false,
                remember: false,
                mfa: false,
                ip: None,
                user_agent: String::new(),
                not_after: None,
            },
            Some(login) => NewSession {
                impersonated: true,
                remember: false,
                mfa: false,
                ..user(login)
            },
        }
    }

    /// A new superadmin login, password or session secret gives a new credential generation.
    /// The sessions of the superadmin and the impersonated sessions of an old generation then
    /// stop on every instance. The sessions of the database users stay.
    #[test]
    fn end_all_ends_the_sessions_of_every_instance_and_the_superadmin_too() {
        let Some((a, b)) = instances(HOUR, HOUR, 8, Duration::ZERO) else {
            return;
        };
        let alice = a.sessions.issue(HERE, user("alice")).unwrap();
        let root = b.sessions.issue(HERE, superadmin(None)).unwrap();
        let as_bob = a.sessions.issue(HERE, superadmin(Some("bob"))).unwrap();
        assert!(b.sessions.validate(&alice, &HERE).is_some());
        assert_eq!(a.sessions.end_all(EndReason::Revoked), Ok(3));
        for token in [&alice, &root, &as_bob] {
            assert!(a.sessions.validate(token, &HERE).is_none());
            assert!(b.sessions.validate(token, &HERE).is_none());
        }
        let ended: Vec<_> = a.ended().into_iter().chain(b.ended()).collect();
        assert_eq!(ended.len(), 3, "each session is recorded once: {ended:?}");
        assert!(ended
            .iter()
            .all(|(_, reason)| *reason == EndReason::Revoked));
        assert_eq!(a.sessions.end_all(EndReason::Revoked), Ok(0));
    }

    #[test]
    fn the_end_sessions_command_empties_the_database_and_writes_one_audit_event() {
        let Some((a, b)) = instances(HOUR, HOUR, 8, Duration::ZERO) else {
            return;
        };
        let alice = a.sessions.issue(HERE, user("alice")).unwrap();
        let root = b.sessions.issue(HERE, superadmin(None)).unwrap();
        assert_eq!(a.store.count("sessions"), 2);
        assert_eq!(a.store.end_all_sessions(unix_now()), Ok(2));
        assert_eq!(a.store.count("sessions"), 0);
        assert!(a.sessions.validate(&alice, &HERE).is_none());
        assert!(b.sessions.validate(&root, &HERE).is_none());
        assert_eq!(a.store.count("audit_events"), 1);
        assert_eq!(a.store.end_all_sessions(unix_now()), Ok(0));
    }

    #[test]
    fn a_new_superadmin_generation_ends_the_old_superadmin_and_impersonated_sessions() {
        let Some(backend) = postgres() else {
            return;
        };
        let (first, second) = backend.fresh_pair();
        let old = Instance::with_generation(first, [1; 32]);
        let root = old.sessions.issue(HERE, superadmin(None)).unwrap();
        let as_alice = old.sessions.issue(HERE, superadmin(Some("alice"))).unwrap();
        let alice = old.sessions.issue(HERE, user("alice")).unwrap();
        assert!(old.sessions.validate(&root, &HERE).is_some());

        let new = Instance::with_generation(second, [2; 32]);
        assert!(new.sessions.validate(&root, &HERE).is_none());
        assert!(new.sessions.validate(&as_alice, &HERE).is_none());
        assert!(new.sessions.validate(&alice, &HERE).is_some());
        let fresh = new.sessions.issue(HERE, superadmin(None)).unwrap();
        assert!(
            old.sessions.validate(&fresh, &HERE).is_none(),
            "an old instance refuses the rows of the new generation"
        );

        // At start, the new instance deletes the rows of the other generations.
        assert_eq!(new.sessions.end_other_generations(), 2);
        assert!(old.sessions.validate(&root, &HERE).is_none());
        assert!(new.sessions.validate(&fresh, &HERE).is_some());
        assert!(new.sessions.validate(&alice, &HERE).is_some());
        assert_eq!(
            new.ended()
                .iter()
                .filter(|(_, reason)| *reason == EndReason::Revoked)
                .count(),
            2
        );
    }

    #[test]
    fn a_database_that_went_away_does_not_sign_everybody_out() {
        let Some((a, _b)) = instances(HOUR, HOUR, 8, Duration::ZERO) else {
            return;
        };
        let token = a.sessions.issue(HERE, user("alice")).unwrap();
        assert!(a.sessions.validate(&token, &HERE).is_some());
        a.store.go_down();
        assert!(
            a.sessions.validate(&token, &HERE).is_some(),
            "checked a moment ago: still accepted"
        );
        assert!(
            a.sessions.shared_errors() >= 1,
            "and the failure is counted"
        );
        assert!(
            a.sessions.validate(&token, &ELSEWHERE).is_none(),
            "the binding is still enforced from the copy"
        );
        let stranger = "A".repeat(43);
        assert!(
            a.sessions.validate(&stranger, &HERE).is_none(),
            "an unknown cookie is not"
        );
        assert_eq!(
            a.sessions.issue(HERE, user("bob")),
            Err(SessionIssueError::Storage)
        );
    }

    const DAY: Duration = Duration::from_secs(86_400);

    fn remembered(login: &str) -> NewSession {
        NewSession {
            remember: true,
            ..user(login)
        }
    }

    #[test]
    fn a_remembered_session_has_the_long_life_on_every_instance_and_survives_a_restart() {
        let (first, second) = postgres().map(|pg| pg.fresh_pair()).unzip();
        let (Some(first), Some(second)) = (first, second) else {
            return;
        };
        let remember = Remember {
            ttl: 30 * DAY,
            idle_timeout: 7 * DAY,
        };
        let a = Instance::remembering(first, DAY, Duration::from_secs(1800), remember);
        let b = Instance::remembering(second, DAY, Duration::from_secs(1800), remember);
        let now = unix_now();
        let short = a.sessions.issue(HERE, user("alice")).unwrap();
        let long = a.sessions.issue(HERE, remembered("alice")).unwrap();
        let seen_short = b.sessions.validate(&short, &HERE).unwrap();
        let seen_long = b.sessions.validate(&long, &HERE).unwrap();
        assert!(!seen_short.remembered && seen_long.remembered);
        assert!((now + 86_399..=now + 86_401).contains(&seen_short.expires_at));
        let thirty = 30 * 86_400;
        assert!((now + thirty - 1..=now + thirty + 1).contains(&seen_long.expires_at));
        // The history row of the shared database carries the flag.
        let snapshot = b
            .store
            .snapshot(&[("alice".into(), false)], unix_now())
            .unwrap();
        let flags: Vec<bool> = snapshot.accounts[0]
            .recent
            .iter()
            .map(|r| r.remembered)
            .collect();
        assert_eq!(flags.iter().filter(|f| **f).count(), 1, "{flags:?}");
        // A restart: a new store on the same database finds both sessions, with their flag.
        let (tracker, _events) = Tracker::recording();
        let mut restarted = SessionStore::new(DAY, Duration::from_secs(1800), 8, tracker)
            .shared(Arc::clone(&b.store), [0; 32])
            .remember(remember);
        restarted.set_cache_ttl(Duration::ZERO);
        assert!(restarted.validate(&long, &HERE).unwrap().remembered);
        assert!(!restarted.validate(&short, &HERE).unwrap().remembered);
        // A logout ends it on every instance.
        assert!(a.sessions.revoke(&long, EndReason::Logout).is_some());
        assert!(b.sessions.validate(&long, &HERE).is_none());
        assert!(restarted.validate(&long, &HERE).is_none());
    }

    #[test]
    fn the_idle_rule_of_a_remembered_session_holds_on_every_instance_and_in_the_sweep() {
        let (first, second) = postgres().map(|pg| pg.fresh_pair()).unzip();
        let (Some(first), Some(second)) = (first, second) else {
            return;
        };
        let remember = Remember {
            ttl: HOUR,
            idle_timeout: Duration::from_secs(600),
        };
        let a = Instance::remembering(first, HOUR, Duration::from_secs(1), remember);
        let b = Instance::remembering(second, HOUR, Duration::from_secs(1), remember);
        let short = a.sessions.issue(HERE, user("alice")).unwrap();
        let long = a.sessions.issue(HERE, remembered("alice")).unwrap();
        sleep(Duration::from_millis(2_200));
        assert!(
            b.sessions.validate(&short, &HERE).is_none(),
            "idle for longer than the idle time of the normal rule"
        );
        assert!(
            b.sessions.validate(&long, &HERE).is_some(),
            "the remembered session is still inside its idle time"
        );
        // The sweep ends the sessions of the normal rule that idled out. It keeps the remembered session.
        let third = a.sessions.issue(HERE, user("bob")).unwrap();
        sleep(Duration::from_millis(2_200));
        a.sessions.sweep();
        let reasons: Vec<EndReason> = a
            .ended()
            .into_iter()
            .chain(b.ended())
            .map(|(_, r)| r)
            .collect();
        assert!(reasons.contains(&EndReason::Idle), "{reasons:?}");
        assert!(b.sessions.validate(&third, &HERE).is_none());
        assert!(b.sessions.validate(&long, &HERE).is_some());
    }
}
