//! What happened, recorded: sessions (who, from where, how long), the services they used, and the
//! administrative events. Nothing in this module runs on the request path.
//!
//! Requests only push small values into memory. These values are a bounded channel for lifecycle
//! events, and two maps for the "session was seen" and "service was used" counters. One writer
//! thread empties them into the database every few seconds. Each flush uses a single transaction.
//! The database is therefore never touched per request. A slow database cannot slow a login down.
//! When the channel is full, an event is dropped and counted. Each instance of a shared database
//! has its own writer. The writers write disjoint events.
//!
//! The module keeps no request path and no query. It keeps only the service that the path belongs to.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{sync_channel, Receiver, RecvTimeoutError, SyncSender, TrySendError};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::store::Store;
use crate::util::json_escape;

/// The maximum number of lifecycle events that wait for the writer. Events beyond this number are
/// dropped and counted.
const CHANNEL_CAPACITY: usize = 4096;
const MAX_BATCH_EVENTS: usize = 1024;
const PRUNE_EVERY: Duration = Duration::from_secs(24 * 3600);
/// The interval at which the writer empties the in-memory counters into the database.
pub const FLUSH_EVERY: Duration = Duration::from_secs(15);

pub fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// The reason why a session ended. The reason is stored as text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EndReason {
    Logout,
    Expired,
    Idle,
    Revoked,
    Restart,
    UserDeleted,
    Impersonation,
}

impl EndReason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Logout => "logout",
            Self::Expired => "expired",
            Self::Idle => "idle",
            Self::Revoked => "revoked",
            Self::Restart => "restart",
            Self::UserDeleted => "user_deleted",
            Self::Impersonation => "impersonation",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AuditKind {
    LoginFailed,
    UserAdded,
    UserDeleted,
    UserUpdated,
    PasswordReset,
    ImpersonationStarted,
    ImpersonationEnded,
    /// The superadmin, or the command `portal end-sessions`, ended every session.
    SessionsEnded,
    /// An account turned its second factor on.
    TotpEnabled,
    /// An account turned its second factor off.
    TotpDisabled,
    /// The superadmin, or the command `portal reset-two-factor`, removed the second factor of an account.
    TotpReset,
    /// An account made its recovery codes again.
    RecoveryRegenerated,
    /// An account signed in with a recovery code.
    RecoveryUsed,
    /// A second-factor code was wrong for an account that has a second factor.
    MfaFailed,
}

impl AuditKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::LoginFailed => "login_failed",
            Self::UserAdded => "user_added",
            Self::UserDeleted => "user_deleted",
            Self::UserUpdated => "user_updated",
            Self::PasswordReset => "password_reset",
            Self::ImpersonationStarted => "impersonation_started",
            Self::ImpersonationEnded => "impersonation_ended",
            Self::SessionsEnded => "sessions_ended",
            Self::TotpEnabled => "totp_enabled",
            Self::TotpDisabled => "totp_disabled",
            Self::TotpReset => "totp_reset",
            Self::RecoveryRegenerated => "recovery_regenerated",
            Self::RecoveryUsed => "recovery_used",
            Self::MfaFailed => "mfa_failed",
        }
    }
}

#[derive(Clone, Debug)]
pub struct SessionStart {
    pub sid: u64,
    pub login: String,
    pub superadmin: bool,
    pub impersonated: bool,
    /// The person asked "Keep me signed in" and the session got the remembered rule.
    pub remembered: bool,
    /// The sign-in used the second factor.
    pub mfa: bool,
    pub ip: Option<String>,
    pub user_agent: String,
    pub at: i64,
}

#[derive(Clone, Debug)]
pub struct AuditEvent {
    pub kind: AuditKind,
    /// The account concerned. This field is absent for a failed login on a name that does not exist.
    /// What was typed there is sometimes a password.
    pub login: Option<String>,
    /// The person who did the action. This field is used for administrative actions.
    pub actor: Option<String>,
    pub ip: Option<String>,
    pub user_agent: String,
    pub at: i64,
}

/// One sign-in attempt that reached the password check. The attempt is kept to show who tries to
/// sign in, and from where. The password never reaches this type.
#[derive(Clone, Debug)]
pub struct LoginAttempt {
    /// The text typed in the login field. The text is already made safe by [`crate::util::typed_login`].
    pub login: String,
    /// The typed login names an account. The superadmin account is included.
    pub known: bool,
    pub success: bool,
    pub ip: Option<String>,
    pub user_agent: String,
    pub at: i64,
}

#[derive(Clone, Debug)]
pub enum Event {
    SessionStarted(SessionStart),
    SessionEnded {
        sid: u64,
        reason: EndReason,
        at: i64,
    },
    Audit(AuditEvent),
    Login(LoginAttempt),
}

/// The hits of one session on one service since the last flush.
#[derive(Clone, Debug)]
pub struct Hit {
    pub first_at: i64,
    pub last_at: i64,
    pub hits: u64,
    pub ip: Option<String>,
}

/// The data that one flush writes.
#[derive(Default)]
pub struct Batch {
    pub events: Vec<Event>,
    pub hits: HashMap<(u64, String), Hit>,
    pub seen: HashMap<u64, i64>,
}

pub struct Tracker {
    tx: Option<SyncSender<Event>>,
    hits: Mutex<HashMap<(u64, String), Hit>>,
    seen: Mutex<HashMap<u64, i64>>,
    dropped: AtomicU64,
    flush_errors: AtomicU64,
}

impl Tracker {
    /// A tracker that records nothing. Use it in the tests that check which code emits events.
    #[cfg(test)]
    pub fn disabled() -> Arc<Self> {
        Arc::new(Self::new(None))
    }

    /// A tracker whose events can be read back. Use it in the tests that check what emits events.
    #[cfg(test)]
    pub fn recording() -> (Arc<Self>, Receiver<Event>) {
        let (tx, rx) = sync_channel(CHANNEL_CAPACITY);
        (Arc::new(Self::new(Some(tx))), rx)
    }

    /// Closes the sessions that a previous run left open. This step runs only with a SQLite file.
    /// Sessions of a shared database outlive a restart. After this step, the writer thread starts.
    pub fn start(
        store: Arc<Store>,
        flush_every: Duration,
        retention_days: u32,
    ) -> Result<Arc<Self>, String> {
        store.close_open_sessions(unix_now(), EndReason::Restart.as_str())?;
        let (tx, rx) = sync_channel(CHANNEL_CAPACITY);
        let tracker = Arc::new(Self::new(Some(tx)));
        let writer = Arc::clone(&tracker);
        thread::Builder::new()
            .name("portal-tracking".into())
            .spawn(move || writer.run(rx, &store, flush_every, retention_days))
            .map_err(|e| format!("cannot start the tracking writer: {e}"))?;
        Ok(tracker)
    }

    fn new(tx: Option<SyncSender<Event>>) -> Self {
        Self {
            tx,
            hits: Mutex::new(HashMap::new()),
            seen: Mutex::new(HashMap::new()),
            dropped: AtomicU64::new(0),
            flush_errors: AtomicU64::new(0),
        }
    }

    /// This function never blocks. When the channel is full, the event is lost and counted.
    pub fn emit(&self, event: Event) {
        let Some(tx) = &self.tx else { return };
        match tx.try_send(event) {
            Ok(()) => {}
            Err(TrySendError::Full(_)) | Err(TrySendError::Disconnected(_)) => {
                self.dropped.fetch_add(1, Ordering::Relaxed);
            }
        }
    }

    /// Records that the session was used at `at`.
    pub fn touch(&self, sid: u64, at: i64) {
        if self.tx.is_none() {
            return;
        }
        let mut seen = self.seen.lock().unwrap_or_else(|p| p.into_inner());
        let slot = seen.entry(sid).or_insert(at);
        if *slot < at {
            *slot = at;
        }
    }

    /// Records that the session used `service_id`. The gate let this request through.
    pub fn service_hit(&self, sid: u64, service_id: &str, ip: Option<String>, at: i64) {
        if self.tx.is_none() {
            return;
        }
        let mut hits = self.hits.lock().unwrap_or_else(|p| p.into_inner());
        hits.entry((sid, service_id.to_string()))
            .and_modify(|hit| {
                hit.hits += 1;
                hit.last_at = hit.last_at.max(at);
            })
            .or_insert(Hit {
                first_at: at,
                last_at: at,
                hits: 1,
                ip,
            });
    }

    pub fn dropped(&self) -> u64 {
        self.dropped.load(Ordering::Relaxed)
    }

    pub fn flush_errors(&self) -> u64 {
        self.flush_errors.load(Ordering::Relaxed)
    }

    fn run(&self, rx: Receiver<Event>, store: &Store, flush_every: Duration, retention_days: u32) {
        let mut next_prune = std::time::Instant::now();
        loop {
            let mut events = Vec::new();
            let disconnected = match rx.recv_timeout(flush_every) {
                Ok(event) => {
                    events.push(event);
                    false
                }
                Err(RecvTimeoutError::Timeout) => false,
                Err(RecvTimeoutError::Disconnected) => true,
            };
            // Every event sent before a counter was bumped is now in the channel.
            // Take all events first. A hit then never reaches the database before its session row.
            while events.len() < MAX_BATCH_EVENTS {
                match rx.try_recv() {
                    Ok(event) => events.push(event),
                    Err(_) => break,
                }
            }
            let batch = Batch {
                events,
                hits: std::mem::take(&mut *self.hits.lock().unwrap_or_else(|p| p.into_inner())),
                seen: std::mem::take(&mut *self.seen.lock().unwrap_or_else(|p| p.into_inner())),
            };
            if let Err(error) = store.apply(&batch) {
                self.flush_errors.fetch_add(1, Ordering::Relaxed);
                log_error("tracking_flush_failed", &error);
            }
            if std::time::Instant::now() >= next_prune {
                next_prune = std::time::Instant::now() + PRUNE_EVERY;
                let cutoff = unix_now() - i64::from(retention_days) * 86_400;
                if let Err(error) = store.prune(cutoff) {
                    log_error("tracking_prune_failed", &error);
                }
            }
            if disconnected {
                return;
            }
        }
    }
}

pub fn log_error(event: &str, error: &str) {
    eprintln!(
        "{{\"ts\":{},\"level\":\"error\",\"event\":\"{}\",\"error\":\"{}\"}}",
        unix_now(),
        event,
        json_escape(error)
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_disabled_tracker_ignores_everything() {
        let tracker = Tracker::disabled();
        tracker.touch(1, 10);
        tracker.service_hit(1, "grafana", None, 10);
        tracker.emit(Event::SessionEnded {
            sid: 1,
            reason: EndReason::Logout,
            at: 10,
        });
        assert_eq!(tracker.dropped(), 0);
        assert!(tracker.hits.lock().unwrap().is_empty());
        assert!(tracker.seen.lock().unwrap().is_empty());
    }

    #[test]
    fn hits_are_counted_per_session_and_service() {
        let (tx, _rx) = sync_channel(4);
        let tracker = Tracker::new(Some(tx));
        tracker.service_hit(7, "grafana", Some("10.0.0.2".into()), 100);
        tracker.service_hit(7, "grafana", Some("10.0.0.2".into()), 130);
        tracker.service_hit(7, "notes", None, 140);
        tracker.service_hit(8, "grafana", None, 150);
        let hits = tracker.hits.lock().unwrap();
        let grafana = &hits[&(7, "grafana".to_string())];
        assert_eq!(
            (grafana.hits, grafana.first_at, grafana.last_at),
            (2, 100, 130)
        );
        assert_eq!(hits.len(), 3);
    }

    #[test]
    fn touch_keeps_the_latest_time() {
        let (tx, _rx) = sync_channel(4);
        let tracker = Tracker::new(Some(tx));
        tracker.touch(3, 50);
        tracker.touch(3, 40);
        tracker.touch(3, 60);
        assert_eq!(tracker.seen.lock().unwrap()[&3], 60);
    }

    #[test]
    fn a_full_channel_drops_and_counts_instead_of_blocking() {
        let (tx, _rx) = sync_channel(1);
        let tracker = Tracker::new(Some(tx));
        for sid in 0..3 {
            tracker.emit(Event::SessionEnded {
                sid,
                reason: EndReason::Idle,
                at: 1,
            });
        }
        assert_eq!(tracker.dropped(), 2);
    }
}
