//! The state of a key and of its owner, for the edge and the PDP (portal 8.3, 9.2, 9.3), and the
//! count of the key checks that passed (portal 5.5a).
//!
//! A key check reads only memory: the directory, the grants of the owner and the clock. It never
//! reads or writes a database row. The checks that passed are counted in memory and written by
//! [`crate::sync`] at most 15 seconds later.

#![allow(dead_code)]

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::grants::Grant;
use crate::http::AppState;
use crate::portal_actions;
use crate::store::KeyCheck;
use crate::users::{KeyOwner, KeyRecord};

/// Why a key is refused. The edge logs the cause and counts it; the answer to the client never
/// shows it (portal 8.3, 10.5d).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Cause {
    /// The request has no `Authorization` header with a key.
    NoKey,
    /// The text has not the form of a key.
    BadFormat,
    /// The text is a good key of the other kind: this verifier serves one kind only (portal 8.3a).
    WrongKind,
    /// The directory holds no key with this digest.
    UnknownKey,
    /// The key is expired.
    Expired,
    /// The key is older than the `max_key_lifetime` of its owner.
    KeyTooOld,
    /// The owner of the key does not exist.
    OwnerUnknown,
    /// The owner does not have `keys.own.use` on the kind of the key.
    OwnerCannotUseKeys,
    /// No service owns the path of the request.
    NoService,
    /// The request has no `X-Forwarded-Uri` header, or a bad one.
    BadForwardedHeader,
    /// The decision for `access` on the service is `false`.
    NoAccess,
    /// The configuration has no block `keys`, or it turns the kind of the key off.
    KeysDisabled,
    /// The last good read of the directory is older than `keys.directory_max_age`.
    DirectoryTooOld,
    /// The last good document of the service is older than `access.actions_max_age`.
    ActionsTooOld,
}

impl Cause {
    pub const ALL: [Cause; 14] = [
        Cause::NoKey,
        Cause::BadFormat,
        Cause::WrongKind,
        Cause::UnknownKey,
        Cause::Expired,
        Cause::KeyTooOld,
        Cause::OwnerUnknown,
        Cause::OwnerCannotUseKeys,
        Cause::NoService,
        Cause::BadForwardedHeader,
        Cause::NoAccess,
        Cause::KeysDisabled,
        Cause::DirectoryTooOld,
        Cause::ActionsTooOld,
    ];

    /// The name of portal 10.5d.
    pub fn as_str(self) -> &'static str {
        match self {
            Cause::NoKey => "no_key",
            Cause::BadFormat => "bad_format",
            Cause::WrongKind => "wrong_kind",
            Cause::UnknownKey => "unknown_key",
            Cause::Expired => "expired",
            Cause::KeyTooOld => "key_too_old",
            Cause::OwnerUnknown => "owner_unknown",
            Cause::OwnerCannotUseKeys => "owner_cannot_use_keys",
            Cause::NoService => "no_service",
            Cause::BadForwardedHeader => "bad_forwarded_header",
            Cause::NoAccess => "no_access",
            Cause::KeysDisabled => "keys_disabled",
            Cause::DirectoryTooOld => "directory_too_old",
            Cause::ActionsTooOld => "actions_too_old",
        }
    }
}

/// The owner side of the decision for a key.
#[derive(Clone, Debug)]
pub enum OwnerSide {
    /// The superadmin: always `true`, with no limit value and no counter (portal 9.1a).
    Superadmin,
    /// A person with the grants that apply to the person.
    Person {
        login: String,
        /// The account comes from the file: its counters are the rows of the file.
        from_file: bool,
    },
}

/// A key that passed the checks of its own state and of its owner.
pub struct Principal {
    pub key: Arc<KeyRecord>,
    pub owner: OwnerSide,
}

impl Principal {
    /// The login that the identity token gives in the claim `owner` (protocol 9.3). For the key of
    /// the superadmin, it is the current login of the superadmin (portal 3.7b).
    pub fn owner_login<'a>(&'a self, superadmin: &'a str) -> &'a str {
        match &self.owner {
            OwnerSide::Superadmin => superadmin,
            OwnerSide::Person { login, .. } => login,
        }
    }
}

/// The grants of a person that apply: the account of the file, or the account of the database
/// (the grants of the database count only when the mode uses them). `None`: no such person
/// (portal 3.4a, 3.4b).
pub fn person_grants(state: &AppState, login: &str) -> Option<(Vec<Grant>, bool)> {
    if let Some(account) = state.config.access.accounts.get(login) {
        return Some((account.grants.clone(), true));
    }
    let stored = state.users.grants_of(login)?;
    if state.policy.mode.uses_stored() {
        Some((stored, false))
    } else {
        Some((Vec::new(), false))
    }
}

/// The grants of a person on one service, and whether the account comes from the file. The
/// directory holds the grants by service, so the call copies no grant (portal 10.7d). `None`: no
/// such person.
pub fn person_grants_on(
    state: &AppState,
    login: &str,
    service: &str,
) -> Option<(Arc<[Grant]>, bool)> {
    if state.config.access.accounts.contains_key(login) {
        return state
            .users
            .file_grants_on(login, service)
            .map(|grants| (grants, true));
    }
    let stored = state.users.grants_on(login, service)?;
    if state.policy.mode.uses_stored() {
        Some((stored, false))
    } else {
        Some((Arc::from(Vec::new()), false))
    }
}

/// Whether the login names a person: the superadmin, an account of the file or an account of the
/// database. This is the one check that the inbox uses for the target of an alert to an account:
/// an account of the file is an account like any other. It reads memory only.
pub fn person_exists(state: &AppState, login: &str) -> bool {
    crate::util::constant_time_eq(login.as_bytes(), state.superadmin_login.as_bytes())
        || person_grants_on(state, login, portal_actions::SERVICE).is_some()
}

/// The directory of the key is too old (portal 10.3): the last good read of the database is older
/// than `keys.directory_max_age`. With SQLite, the one instance holds the directory, so it is
/// never too old.
pub fn directory_too_old(state: &AppState) -> bool {
    let Some(keys) = &state.config.keys else {
        return false;
    };
    state.store.is_shared() && state.users.age() > keys.directory_max_age
}

/// Finds the owner of a key and checks the state of the key and of the owner at `now` (Unix
/// seconds): the kind of the key is on, the key is not expired, the owner exists and has
/// `keys.own.use` on the kind of the key, and the key is not older than the `max_key_lifetime` of
/// the owner for this kind (portal 8.3, 9.3).
pub fn resolve(state: &AppState, key: &Arc<KeyRecord>, now: i64) -> Result<Principal, Cause> {
    // The kind of the key must be on (`keys.mcp`, `keys.api`).
    if !state
        .config
        .keys
        .as_ref()
        .is_some_and(|keys| keys.kinds().contains(&key.kind))
    {
        return Err(Cause::KeysDisabled);
    }
    if directory_too_old(state) {
        return Err(Cause::DirectoryTooOld);
    }
    if key.expired(now) {
        return Err(Cause::Expired);
    }
    let owner = match &key.owner {
        // The superadmin has the right `keys.own.use` and no value of `max_key_lifetime`, so its
        // key is never too old and can have no expiry date (portal 8.3).
        KeyOwner::Superadmin => OwnerSide::Superadmin,
        KeyOwner::User(login) | KeyOwner::File(login) => {
            // The right to use the keys comes from the grants of the service `portal` only.
            let (grants, from_file) = person_grants_on(state, login, portal_actions::SERVICE)
                .ok_or(Cause::OwnerUnknown)?;
            // A key of the file follows its account of the file, and a key of the database its
            // account of the database.
            if from_file != key.is_from_file() {
                return Err(Cause::OwnerUnknown);
            }
            if !portal_actions::effective_kind(&grants, portal_actions::KEYS_OWN_USE, key.kind) {
                return Err(Cause::OwnerCannotUseKeys);
            }
            if key.too_old(now, portal_actions::max_key_lifetime(&grants, key.kind)) {
                return Err(Cause::KeyTooOld);
            }
            OwnerSide::Person {
                login: login.clone(),
                from_file,
            }
        }
    };
    Ok(Principal {
        key: Arc::clone(key),
        owner,
    })
}

/// The count of the key checks by result (portal 10.5): one number for each cause, and one for the
/// checks that passed.
#[derive(Default)]
pub struct CheckStats {
    ok: std::sync::atomic::AtomicU64,
    causes: [std::sync::atomic::AtomicU64; Cause::ALL.len()],
}

impl CheckStats {
    pub fn passed(&self) {
        self.ok.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }

    pub fn failed(&self, cause: Cause) {
        if let Some(index) = Cause::ALL.iter().position(|c| *c == cause) {
            self.causes[index].fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
    }

    /// `(result, count)`: `ok`, then each cause in the order of [`Cause::ALL`].
    pub fn counts(&self) -> Vec<(&'static str, u64)> {
        let read = |c: &std::sync::atomic::AtomicU64| c.load(std::sync::atomic::Ordering::Relaxed);
        std::iter::once(("ok", read(&self.ok)))
            .chain(
                Cause::ALL
                    .iter()
                    .zip(&self.causes)
                    .map(|(cause, count)| (cause.as_str(), read(count))),
            )
            .collect()
    }

    pub fn render(&self) -> String {
        let mut out = String::from(
            "# HELP portal_key_checks_total Key checks of the edge, by result.\n# TYPE portal_key_checks_total counter\n",
        );
        for (result, count) in self.counts() {
            out.push_str(&format!(
                "portal_key_checks_total{{result=\"{result}\"}} {count}\n"
            ));
        }
        out
    }
}

/// The most source addresses that the log of the failed checks tracks (portal 10.5c).
pub const MAX_LOGGED_ADDRESSES: usize = 10_000;
/// The time that an address stays in the table after its line.
const LOG_GAP: Duration = Duration::from_secs(60);

/// Lets the edge write at most one line each minute for each source address of a failed check
/// (portal 10.5a). When the table is full, the edge adds no address, and it counts the checks that
/// it does not log.
#[derive(Default)]
pub struct FailureLog {
    seen: Mutex<HashMap<String, std::time::Instant>>,
    unlogged: std::sync::atomic::AtomicU64,
}

impl FailureLog {
    /// Whether a line for this address is due. A true answer starts the minute of the address.
    pub fn due(&self, address: &str, now: std::time::Instant) -> bool {
        let mut seen = self.seen.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(at) = seen.get(address) {
            if now.saturating_duration_since(*at) < LOG_GAP {
                return false;
            }
        }
        if seen.len() >= MAX_LOGGED_ADDRESSES && !seen.contains_key(address) {
            seen.retain(|_, at| now.saturating_duration_since(*at) < LOG_GAP);
            if seen.len() >= MAX_LOGGED_ADDRESSES {
                self.unlogged
                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                return false;
            }
        }
        seen.insert(address.to_string(), now);
        true
    }

    /// The number of failed checks that the full table kept out of the log, since the last call.
    pub fn take_unlogged(&self) -> u64 {
        self.unlogged.swap(0, std::sync::atomic::Ordering::Relaxed)
    }

    pub fn entries(&self) -> usize {
        self.seen.lock().unwrap_or_else(|p| p.into_inner()).len()
    }
}

/// The longest time between two writes of the checks of the keys (portal 5.5a).
pub const WRITE_EVERY: Duration = Duration::from_secs(15);

/// The checks that passed, since the last write, and for a key of the file, since the start.
#[derive(Default)]
pub struct KeyChecks {
    pending: Mutex<HashMap<String, (i64, i64)>>,
    file: Mutex<HashMap<String, (i64, i64)>>,
}

impl KeyChecks {
    /// Counts one check that passed at `now` (Unix seconds).
    pub fn record(&self, key: &KeyRecord, now: i64) {
        let table = if key.is_from_file() {
            &self.file
        } else {
            &self.pending
        };
        let mut table = table.lock().unwrap_or_else(|p| p.into_inner());
        let entry = table.entry(key.id.clone()).or_insert((0, 0));
        entry.0 = entry.0.max(now);
        entry.1 += 1;
    }

    /// The checks to write, and the table that holds them is empty again.
    pub fn take(&self) -> Vec<KeyCheck> {
        let mut table = self.pending.lock().unwrap_or_else(|p| p.into_inner());
        table
            .drain()
            .map(|(id, (last_check_at, count))| KeyCheck {
                id,
                last_check_at,
                count,
            })
            .collect()
    }

    /// Puts checks back after a failed write.
    pub fn put_back(&self, checks: Vec<KeyCheck>) {
        let mut table = self.pending.lock().unwrap_or_else(|p| p.into_inner());
        for check in checks {
            let entry = table.entry(check.id).or_insert((0, 0));
            entry.0 = entry.0.max(check.last_check_at);
            entry.1 += check.count;
        }
    }

    /// The time of the last check and the number of checks of a key of the file, since the start
    /// (portal 5.5a).
    pub fn file_key(&self, id: &str) -> Option<(i64, i64)> {
        self.file
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .get(id)
            .copied()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::grants::Grant;

    #[test]
    fn the_log_of_the_failed_checks_has_one_line_each_minute_for_each_address() {
        let log = FailureLog::default();
        let start = std::time::Instant::now();
        assert!(log.due("10.0.0.1", start));
        assert!(!log.due("10.0.0.1", start + Duration::from_secs(30)));
        assert!(log.due("10.0.0.2", start + Duration::from_secs(30)));
        assert!(log.due("10.0.0.1", start + Duration::from_secs(61)));
        assert_eq!(log.entries(), 2);
        // A full table adds no address and counts what it does not log.
        let big = FailureLog::default();
        for n in 0..MAX_LOGGED_ADDRESSES {
            assert!(big.due(&format!("a{n}"), start));
        }
        assert!(!big.due("new", start + Duration::from_secs(1)));
        assert!(!big.due("new", start + Duration::from_secs(2)));
        assert_eq!(big.take_unlogged(), 2);
        assert_eq!(big.take_unlogged(), 0);
        // After a minute, the old addresses leave and the new one comes.
        assert!(big.due("new", start + Duration::from_secs(70)));
        assert!(big.entries() <= MAX_LOGGED_ADDRESSES);
    }

    #[test]
    fn the_stats_count_the_passed_checks_and_each_cause() {
        let stats = CheckStats::default();
        stats.passed();
        stats.passed();
        stats.failed(Cause::UnknownKey);
        stats.failed(Cause::NoAccess);
        stats.failed(Cause::NoAccess);
        let counts = stats.counts();
        assert_eq!(counts[0], ("ok", 2));
        assert!(counts.contains(&("unknown_key", 1)));
        assert!(counts.contains(&("no_access", 2)));
        assert!(counts.contains(&("expired", 0)));
        assert!(stats
            .render()
            .contains("portal_key_checks_total{result=\"no_access\"} 2"));
    }

    fn key(id: &str, file: bool) -> KeyRecord {
        KeyRecord {
            kind: crate::keys::KeyKind::Mcp,
            id: id.into(),
            owner: if file {
                KeyOwner::File("carol".into())
            } else {
                KeyOwner::User("alice".into())
            },
            name: id.into(),
            digest: [1; 32],
            hint: String::new(),
            created_at: 0,
            expires_at: None,
            grants: Vec::<Grant>::new(),
        }
    }

    #[test]
    fn each_cause_has_the_name_of_the_register_and_a_name_that_is_unique() {
        let names: Vec<&str> = Cause::ALL.iter().map(|c| c.as_str()).collect();
        assert_eq!(
            names,
            [
                "no_key",
                "bad_format",
                "wrong_kind",
                "unknown_key",
                "expired",
                "key_too_old",
                "owner_unknown",
                "owner_cannot_use_keys",
                "no_service",
                "bad_forwarded_header",
                "no_access",
                "keys_disabled",
                "directory_too_old",
                "actions_too_old",
            ]
        );
    }

    #[test]
    fn the_checks_keep_the_later_time_and_add_the_counts_until_they_are_taken() {
        let checks = KeyChecks::default();
        let k = key("k1", false);
        checks.record(&k, 100);
        checks.record(&k, 90);
        checks.record(&k, 120);
        checks.record(&key("k2", false), 5);
        let mut taken = checks.take();
        taken.sort_by(|a, b| a.id.cmp(&b.id));
        assert_eq!(
            taken,
            [
                KeyCheck {
                    id: "k1".into(),
                    last_check_at: 120,
                    count: 3
                },
                KeyCheck {
                    id: "k2".into(),
                    last_check_at: 5,
                    count: 1
                },
            ]
        );
        assert!(checks.take().is_empty());
        // A failed write puts them back, and the next checks add to them.
        checks.put_back(taken);
        checks.record(&k, 130);
        let again: Vec<KeyCheck> = checks.take().into_iter().filter(|c| c.id == "k1").collect();
        assert_eq!(again[0].count, 4);
        assert_eq!(again[0].last_check_at, 130);
    }

    #[test]
    fn a_key_of_the_file_is_counted_in_memory_only() {
        let checks = KeyChecks::default();
        let k = key("34b1100f45b235ba", true);
        checks.record(&k, 10);
        checks.record(&k, 20);
        assert!(
            checks.take().is_empty(),
            "no row in the database for a key of the file"
        );
        assert_eq!(checks.file_key("34b1100f45b235ba"), Some((20, 2)));
        assert_eq!(checks.file_key("other"), None);
    }
}
