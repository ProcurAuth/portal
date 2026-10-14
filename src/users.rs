//! The accounts, in memory. The database is the record; this is what the request path reads, so
//! `/auth/verify` never waits on the database. Every change goes to the database first, then here.
//!
//! With one instance that is all there is to it. With several (a shared database) the others change
//! the accounts too, so [`Directory::refresh`] reads the database again every few seconds; a change
//! made on one instance reaches the others within that time.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::RwLock;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::store::{Store, UserRow};

pub const MIN_PASSWORD_BYTES: usize = 10;
const MAX_LOGIN_CHARS: usize = 32;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UserRecord {
    pub password_hash: String,
    pub roles: Vec<String>,
    pub created_at: i64,
}

#[derive(Debug)]
pub struct Directory {
    users: RwLock<HashMap<String, UserRecord>>,
    max_users: usize,
    /// Counts the changes made through this instance, so a refresh that read the database before one
    /// of them does not overwrite it with older rows.
    changes: AtomicU64,
    /// Unix milliseconds of the last refresh that was started.
    refreshed_at: AtomicU64,
    refresh_errors: AtomicU64,
}

impl Directory {
    pub fn new(rows: Vec<UserRow>, max_users: usize) -> Self {
        Self {
            users: RwLock::new(records(rows)),
            max_users,
            changes: AtomicU64::new(0),
            refreshed_at: AtomicU64::new(0),
            refresh_errors: AtomicU64::new(0),
        }
    }

    /// Replaces the accounts with what the database holds now. A change made here while the
    /// database was being read wins: the next refresh brings the rest. `true` when it replaced.
    pub fn refresh(&self, store: &Store) -> Result<bool, String> {
        self.refreshed_at.store(now_millis(), Ordering::Relaxed);
        let seen = self.changes.load(Ordering::Acquire);
        let rows = store.load_users().inspect_err(|_| {
            self.refresh_errors.fetch_add(1, Ordering::Relaxed);
        })?;
        Ok(self.replace_unless_changed(seen, rows))
    }

    /// A refresh for a request that met a login it does not know (an account created a moment ago on
    /// another instance), unless one started less than `min_gap` ago: unknown logins cannot be used
    /// to make the database work on every request.
    pub fn refresh_if_due(&self, store: &Store, min_gap: Duration) -> bool {
        let last = self.refreshed_at.load(Ordering::Relaxed);
        let now = now_millis();
        if now.saturating_sub(last) < min_gap.as_millis() as u64
            || self
                .refreshed_at
                .compare_exchange(last, now, Ordering::Relaxed, Ordering::Relaxed)
                .is_err()
        {
            return false;
        }
        self.refresh(store).unwrap_or(false)
    }

    pub fn refresh_errors(&self) -> u64 {
        self.refresh_errors.load(Ordering::Relaxed)
    }

    /// `seen` is the change counter from before `rows` were read.
    fn replace_unless_changed(&self, seen: u64, rows: Vec<UserRow>) -> bool {
        let mut users = self.users.write().unwrap_or_else(|p| p.into_inner());
        if self.changes.load(Ordering::Acquire) != seen {
            return false;
        }
        *users = records(rows);
        true
    }

    fn read(&self) -> std::sync::RwLockReadGuard<'_, HashMap<String, UserRecord>> {
        self.users.read().unwrap_or_else(|p| p.into_inner())
    }

    pub fn get(&self, login: &str) -> Option<UserRecord> {
        self.read().get(login).cloned()
    }

    pub fn roles_of(&self, login: &str) -> Option<Vec<String>> {
        self.read().get(login).map(|user| user.roles.clone())
    }

    pub fn count(&self) -> usize {
        self.read().len()
    }

    pub fn is_full(&self) -> bool {
        self.count() >= self.max_users
    }

    /// Sorted by login.
    pub fn list(&self) -> Vec<(String, UserRecord)> {
        let mut users: Vec<_> = self
            .read()
            .iter()
            .map(|(login, user)| (login.clone(), user.clone()))
            .collect();
        users.sort_by(|a, b| a.0.cmp(&b.0));
        users
    }

    pub fn insert(&self, login: String, user: UserRecord) {
        let mut users = self.users.write().unwrap_or_else(|p| p.into_inner());
        self.changes.fetch_add(1, Ordering::AcqRel);
        users.insert(login, user);
    }

    /// New roles, and a new password hash when the password changed. `false` if no such user.
    pub fn update(&self, login: &str, password_hash: Option<String>, roles: Vec<String>) -> bool {
        let mut users = self.users.write().unwrap_or_else(|p| p.into_inner());
        self.changes.fetch_add(1, Ordering::AcqRel);
        match users.get_mut(login) {
            Some(user) => {
                if let Some(hash) = password_hash {
                    user.password_hash = hash;
                }
                user.roles = roles;
                true
            }
            None => false,
        }
    }

    pub fn remove(&self, login: &str) -> bool {
        let mut users = self.users.write().unwrap_or_else(|p| p.into_inner());
        self.changes.fetch_add(1, Ordering::AcqRel);
        users.remove(login).is_some()
    }
}

fn now_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn records(rows: Vec<UserRow>) -> HashMap<String, UserRecord> {
    rows.into_iter()
        .map(|row| {
            (
                row.login,
                UserRecord {
                    password_hash: row.password_hash,
                    roles: row.roles,
                    created_at: row.created_at,
                },
            )
        })
        .collect()
}

/// A login as stored: trimmed, lower case, `[a-z0-9._-]{1,32}`.
pub fn normalize_login(raw: &str) -> Result<String, &'static str> {
    let login = raw.trim().to_ascii_lowercase();
    let valid = !login.is_empty()
        && login.len() <= MAX_LOGIN_CHARS
        && login.bytes().all(|b| {
            b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'.' | b'_' | b'-')
        });
    if valid {
        Ok(login)
    } else {
        Err("invalid-login")
    }
}

pub fn check_new_password(password: &str, max_bytes: usize) -> Result<(), &'static str> {
    if password.len() < MIN_PASSWORD_BYTES || password.len() > max_bytes {
        Err("weak-password")
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(login: &str, roles: &[&str]) -> UserRow {
        UserRow {
            login: login.into(),
            password_hash: "h".into(),
            roles: roles.iter().map(|r| r.to_string()).collect(),
            created_at: 1,
        }
    }

    #[test]
    fn lookup_list_insert_and_remove() {
        let dir = Directory::new(vec![row("bob", &["dev"]), row("alice", &["ops", "dev"])], 3);
        assert_eq!(dir.count(), 2);
        assert_eq!(dir.roles_of("alice").unwrap(), ["ops", "dev"]);
        assert!(dir.get("nobody").is_none());
        assert_eq!(dir.list()[0].0, "alice", "sorted by login");
        assert!(!dir.is_full());
        dir.insert(
            "carol".into(),
            UserRecord {
                password_hash: "h".into(),
                roles: vec![],
                created_at: 2,
            },
        );
        assert!(dir.is_full());
        assert!(dir.update("alice", Some("h2".into()), vec!["viewer".into()]));
        assert_eq!(dir.get("alice").unwrap().password_hash, "h2");
        assert_eq!(dir.roles_of("alice").unwrap(), ["viewer"]);
        assert!(dir.update("alice", None, vec![]));
        assert_eq!(
            dir.get("alice").unwrap().password_hash,
            "h2",
            "kept without a new hash"
        );
        assert!(!dir.update("nobody", None, vec![]));
        assert!(dir.remove("bob"));
        assert!(!dir.remove("bob"));
        assert_eq!(dir.count(), 2);
    }

    #[test]
    fn a_refresh_takes_what_the_database_holds() {
        let store = Store::open_in_memory();
        store.insert_user(&row("alice", &["ops"])).unwrap();
        let dir = Directory::new(Vec::new(), 8);
        assert!(dir.refresh(&store).unwrap());
        assert_eq!(dir.roles_of("alice").unwrap(), ["ops"]);
        store.remove_user("alice").unwrap();
        store.insert_user(&row("bob", &[])).unwrap();
        assert!(dir.refresh(&store).unwrap());
        assert!(dir.get("alice").is_none(), "a removal elsewhere is picked up");
        assert!(dir.get("bob").is_some());
    }

    #[test]
    fn a_change_made_here_is_not_undone_by_a_refresh_that_read_before_it() {
        let dir = Directory::new(Vec::new(), 8);
        let seen = dir.changes.load(Ordering::Acquire);
        let read_before_the_insert = vec![row("alice", &[])];
        dir.insert(
            "carol".into(),
            UserRecord {
                password_hash: "h".into(),
                roles: vec![],
                created_at: 1,
            },
        );
        assert!(!dir.replace_unless_changed(seen, read_before_the_insert));
        assert!(dir.get("carol").is_some(), "the insert is still there");
        assert!(dir.get("alice").is_none(), "the stale rows were dropped");
    }

    #[test]
    fn logins_are_normalised_and_bounded() {
        assert_eq!(normalize_login("  Alice.B-1_x ").unwrap(), "alice.b-1_x");
        assert_eq!(
            normalize_login("a\n").unwrap(),
            "a",
            "surrounding whitespace is trimmed"
        );
        for bad in ["", "   ", "a b", "é", "a/b", "a<b", &"x".repeat(33), "a\nb"] {
            assert!(normalize_login(bad).is_err(), "{bad:?} must be refused");
        }
        assert!(normalize_login(&"x".repeat(32)).is_ok());
    }

    #[test]
    fn new_passwords_have_a_floor_and_a_ceiling() {
        assert!(check_new_password("0123456789", 1024).is_ok());
        assert!(check_new_password("short", 1024).is_err());
        assert!(check_new_password(&"x".repeat(1025), 1024).is_err());
    }
}
