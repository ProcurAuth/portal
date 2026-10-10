//! The accounts, in memory. The database is the record; this is what the request path reads, so
//! `/auth/verify` never waits on the disk. A single replica is the only writer, so the two cannot
//! drift: every change goes to SQLite first, then here.

use std::collections::HashMap;
use std::sync::RwLock;

use crate::store::UserRow;

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
}

impl Directory {
    pub fn new(rows: Vec<UserRow>, max_users: usize) -> Self {
        let users = rows
            .into_iter()
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
            .collect();
        Self {
            users: RwLock::new(users),
            max_users,
        }
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
        self.users
            .write()
            .unwrap_or_else(|p| p.into_inner())
            .insert(login, user);
    }

    /// New roles, and a new password hash when the password changed. `false` if no such user.
    pub fn update(&self, login: &str, password_hash: Option<String>, roles: Vec<String>) -> bool {
        let mut users = self.users.write().unwrap_or_else(|p| p.into_inner());
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
        self.users
            .write()
            .unwrap_or_else(|p| p.into_inner())
            .remove(login)
            .is_some()
    }
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
