//! The accounts, in memory. The database is the record. This memory is what the request path reads,
//! so `/auth/verify` never waits on the database. Every change goes to the database first. Then the
//! change goes to this memory.
//!
//! With one instance, no more is needed. With several instances (a shared database), the other
//! instances change the accounts too. So [`Directory::refresh`] reads the database again every few
//! seconds. A change made on one instance reaches the others within that time.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};
use std::sync::{Arc, RwLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::decision::CounterOwner;
use crate::grants::{counter_ids, CounterId, Grant};
use crate::store::{DirectorySnapshot, KeyRow, Store, UserRow};

pub const MIN_PASSWORD_BYTES: usize = 10;
const MAX_LOGIN_CHARS: usize = 32;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UserRecord {
    pub password_hash: String,
    /// The grants that the settings pages gave. The database keeps these grants. The grants of the
    /// configuration are not here. See [`crate::grants::Policy`].
    pub grants: Vec<Grant>,
    pub created_at: i64,
}

/// Whose key it is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum KeyOwner {
    /// The superadmin: the key has no owner row (portal 3.7b).
    Superadmin,
    /// An account of the database, by login.
    User(String),
    /// An account of the configuration file, by login (portal 3.4a).
    File(String),
}

/// A key as the directory holds it (portal 4, 5). The directory never holds the text of a key.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KeyRecord {
    pub id: String,
    pub owner: KeyOwner,
    pub name: String,
    pub digest: [u8; 32],
    pub hint: String,
    pub created_at: i64,
    /// `None`: the key has no expiry date.
    pub expires_at: Option<i64>,
    pub grants: Vec<Grant>,
}

impl KeyRecord {
    /// Whether the key comes from the configuration file (portal 3.4c).
    pub fn is_from_file(&self) -> bool {
        matches!(self.owner, KeyOwner::File(_))
    }

    /// The subject id of the key in a question to the PDP (protocol 3.2): `key:<id>`.
    pub fn subject(&self) -> String {
        format!("{}{}", crate::subject_id::KEY_PREFIX, self.id)
    }

    /// The owner of the counters of the key (portal 7.2d).
    pub fn counter_owner(&self) -> CounterOwner {
        if self.is_from_file() {
            CounterOwner::File(self.subject())
        } else {
            CounterOwner::Key(self.id.clone())
        }
    }

    /// Whether the key is expired at `now` (portal 5.5).
    pub fn expired(&self, now: i64) -> bool {
        self.expires_at.is_some_and(|at| at <= now)
    }

    /// Whether the key is too old at `now`, for a maximum lifetime of its owner (portal 5.5b). A
    /// key of the file has no age (portal 3.4c).
    pub fn too_old(&self, now: i64, max_lifetime: Option<u64>) -> bool {
        !self.is_from_file()
            && max_lifetime.is_some_and(|max| {
                now.saturating_sub(self.created_at) > i64::try_from(max).unwrap_or(i64::MAX)
            })
    }
}

fn key_record(row: KeyRow) -> KeyRecord {
    KeyRecord {
        id: row.id,
        owner: match row.owner {
            Some(login) => KeyOwner::User(login),
            None => KeyOwner::Superadmin,
        },
        name: row.name,
        digest: row.digest,
        hint: row.hint,
        created_at: row.created_at,
        expires_at: row.expires_at,
        grants: row.grants,
    }
}

/// The grants of one subject by service, in the order of the grants (portal 10.7d). A question
/// reads only the grants of its service and copies none.
pub type GrantIndex = HashMap<String, Arc<[Grant]>>;

/// Groups `grants` by service. Each group keeps the order of the grants.
pub fn index_of(grants: &[Grant]) -> GrantIndex {
    let mut groups: HashMap<String, Vec<Grant>> = HashMap::new();
    for grant in grants {
        groups
            .entry(grant.service.clone())
            .or_default()
            .push(grant.clone());
    }
    groups
        .into_iter()
        .map(|(service, group)| (service, Arc::from(group)))
        .collect()
}

/// What the directory holds: the accounts and the keys of the database, and the keys of the file.
#[derive(Debug, Default)]
struct Inner {
    users: HashMap<String, UserRecord>,
    /// The grants of each user by service.
    index: HashMap<String, GrantIndex>,
    keys: HashMap<String, Arc<KeyRecord>>,
    by_digest: HashMap<[u8; 32], String>,
    /// The version of the database directory that these accounts and keys come from.
    version: i64,
}

impl Inner {
    fn from_snapshot(snapshot: DirectorySnapshot) -> Self {
        let keys: HashMap<String, Arc<KeyRecord>> = snapshot
            .keys
            .into_iter()
            .map(|row| {
                let key = key_record(row);
                (key.id.clone(), Arc::new(key))
            })
            .collect();
        let users = records(snapshot.users);
        Self {
            index: users
                .iter()
                .map(|(login, user)| (login.clone(), index_of(&user.grants)))
                .collect(),
            users,
            by_digest: keys.values().map(|k| (k.digest, k.id.clone())).collect(),
            keys,
            version: snapshot.version,
        }
    }
}

#[derive(Debug)]
pub struct Directory {
    inner: RwLock<Inner>,
    /// The keys of the configuration file, with their accounts. They never change while the
    /// program runs (portal 3.4d).
    file_keys: HashMap<String, Arc<KeyRecord>>,
    /// The grants of each account of the file by service (portal 10.7d).
    file_index: HashMap<String, GrantIndex>,
    file_digests: HashMap<[u8; 32], String>,
    max_users: usize,
    /// Counts the changes made through this instance. A refresh that reads the database before one
    /// of these changes does not overwrite the change with older rows.
    changes: AtomicU64,
    /// The value of `changes` when the directory was last read from the database.
    loaded_changes: AtomicU64,
    /// Unix milliseconds when the last refresh started.
    refreshed_at: AtomicU64,
    /// Unix milliseconds when the database last answered a read of the accounts. A directory
    /// that a failed refresh leaves as it was grows older. The value starts at the creation.
    last_success: AtomicU64,
    refresh_errors: AtomicU64,
    /// The version of the directory that the last good round used. It is -1 before the first.
    version: AtomicI64,
}

impl Directory {
    /// A directory with these accounts and no key.
    #[cfg(test)]
    pub fn new(rows: Vec<UserRow>, max_users: usize) -> Self {
        Self::from_snapshot(
            DirectorySnapshot {
                version: 0,
                users: rows,
                keys: Vec::new(),
            },
            max_users,
        )
    }

    pub fn from_snapshot(snapshot: DirectorySnapshot, max_users: usize) -> Self {
        let inner = Inner::from_snapshot(snapshot);
        let version = inner.version;
        Self {
            inner: RwLock::new(inner),
            file_keys: HashMap::new(),
            file_index: HashMap::new(),
            file_digests: HashMap::new(),
            max_users,
            changes: AtomicU64::new(0),
            loaded_changes: AtomicU64::new(0),
            refreshed_at: AtomicU64::new(0),
            last_success: AtomicU64::new(now_millis()),
            refresh_errors: AtomicU64::new(0),
            version: AtomicI64::new(version),
        }
    }

    /// Adds the grants of the accounts of the file, by service (portal 10.7d).
    pub fn with_file_accounts<'a>(
        mut self,
        accounts: impl Iterator<Item = (&'a String, &'a [Grant])>,
    ) -> Self {
        for (login, grants) in accounts {
            self.file_index.insert(login.clone(), index_of(grants));
        }
        self
    }

    /// Adds the keys of the configuration file (portal 3.4c). A key of the file holds no database
    /// row.
    pub fn with_file_keys(mut self, keys: Vec<KeyRecord>) -> Self {
        for key in keys {
            self.file_digests.insert(key.digest, key.id.clone());
            self.file_keys.insert(key.id.clone(), Arc::new(key));
        }
        self
    }

    /// Replaces the directory with what the database holds now, when the version changed. A
    /// change made here during the database read wins. The next refresh brings the rest. Returns
    /// `true` when it replaced. A refresh that finds the same version only marks the round as
    /// successful (portal 10.7).
    pub fn refresh(&self, store: &Store) -> Result<bool, String> {
        self.refreshed_at.store(now_millis(), Ordering::Relaxed);
        let seen = self.changes.load(Ordering::Acquire);
        let version = store.directory_version().inspect_err(|_| {
            self.refresh_errors.fetch_add(1, Ordering::Relaxed);
        })?;
        if version == self.version.load(Ordering::Acquire)
            && seen == self.loaded_changes.load(Ordering::Acquire)
        {
            self.last_success.store(now_millis(), Ordering::Relaxed);
            return Ok(false);
        }
        let snapshot = store.load_directory().inspect_err(|_| {
            self.refresh_errors.fetch_add(1, Ordering::Relaxed);
        })?;
        self.last_success.store(now_millis(), Ordering::Relaxed);
        Ok(self.replace_unless_changed(seen, snapshot))
    }

    /// A refresh for a request that has a login the instance does not know. Such a login can belong
    /// to an account created a moment ago on another instance. The refresh does not run when a
    /// refresh started less than `min_gap` ago. Unknown logins cannot make the database work on
    /// every request.
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

    /// The time since the database last answered a read of the accounts. With a shared store,
    /// the value is the age of the data that this instance uses.
    pub fn age(&self) -> Duration {
        let last = self.last_success.load(Ordering::Relaxed);
        Duration::from_millis(now_millis().saturating_sub(last))
    }

    /// The version of the database directory that this instance uses.
    #[cfg(test)]
    pub fn version(&self) -> i64 {
        self.version.load(Ordering::Acquire)
    }

    #[cfg(test)]
    pub fn set_last_success(&self, millis: u64) {
        self.last_success.store(millis, Ordering::Relaxed);
    }

    /// `seen` is the change counter. It has the value from before the database read the
    /// `snapshot`.
    fn replace_unless_changed(&self, seen: u64, snapshot: DirectorySnapshot) -> bool {
        let mut inner = self.inner.write().unwrap_or_else(|p| p.into_inner());
        if self.changes.load(Ordering::Acquire) != seen {
            return false;
        }
        *inner = Inner::from_snapshot(snapshot);
        self.version.store(inner.version, Ordering::Release);
        self.loaded_changes.store(seen, Ordering::Release);
        true
    }

    fn read(&self) -> std::sync::RwLockReadGuard<'_, Inner> {
        self.inner.read().unwrap_or_else(|p| p.into_inner())
    }

    pub fn get(&self, login: &str) -> Option<UserRecord> {
        self.read().users.get(login).cloned()
    }

    /// The stored grants of an account. `None` means that the account does not exist.
    pub fn grants_of(&self, login: &str) -> Option<Vec<Grant>> {
        self.read().users.get(login).map(|user| user.grants.clone())
    }

    /// The stored grants of an account on one service. `None` means that the account does not
    /// exist. The grants are not copied (portal 10.7d).
    pub fn grants_on(&self, login: &str, service: &str) -> Option<Arc<[Grant]>> {
        let inner = self.read();
        inner.users.get(login)?;
        Some(
            inner
                .index
                .get(login)
                .and_then(|index| index.get(service))
                .cloned()
                .unwrap_or_else(|| Arc::from(Vec::new())),
        )
    }

    /// The grants of an account of the file on one service.
    pub fn file_grants_on(&self, login: &str, service: &str) -> Option<Arc<[Grant]>> {
        let index = self.file_index.get(login)?;
        Some(
            index
                .get(service)
                .cloned()
                .unwrap_or_else(|| Arc::from(Vec::new())),
        )
    }

    pub fn count(&self) -> usize {
        self.read().users.len()
    }

    pub fn is_full(&self) -> bool {
        self.count() >= self.max_users
    }

    /// Sort order: by login.
    pub fn list(&self) -> Vec<(String, UserRecord)> {
        let mut users: Vec<_> = self
            .read()
            .users
            .iter()
            .map(|(login, user)| (login.clone(), user.clone()))
            .collect();
        users.sort_by(|a, b| a.0.cmp(&b.0));
        users
    }

    pub fn insert(&self, login: String, user: UserRecord) {
        let mut inner = self.inner.write().unwrap_or_else(|p| p.into_inner());
        self.changes.fetch_add(1, Ordering::AcqRel);
        inner.index.insert(login.clone(), index_of(&user.grants));
        inner.users.insert(login, user);
    }

    /// Sets a new password hash and new grants. Each value changes only when it is given.
    /// Returns `false` when there is no such user.
    pub fn update(
        &self,
        login: &str,
        password_hash: Option<String>,
        grants: Option<Vec<Grant>>,
    ) -> bool {
        let mut inner = self.inner.write().unwrap_or_else(|p| p.into_inner());
        self.changes.fetch_add(1, Ordering::AcqRel);
        match inner.users.get_mut(login) {
            Some(user) => {
                if let Some(hash) = password_hash {
                    user.password_hash = hash;
                }
                if let Some(grants) = grants {
                    user.grants = grants;
                    let index = index_of(&user.grants);
                    inner.index.insert(login.to_string(), index);
                }
                true
            }
            None => false,
        }
    }

    pub fn remove(&self, login: &str) -> bool {
        let mut inner = self.inner.write().unwrap_or_else(|p| p.into_inner());
        self.changes.fetch_add(1, Ordering::AcqRel);
        let removed = inner.users.remove(login).is_some();
        inner.index.remove(login);
        if removed {
            // The keys of the user go with the user (portal 5.7).
            let gone: Vec<String> = inner
                .keys
                .values()
                .filter(|k| k.owner == KeyOwner::User(login.to_string()))
                .map(|k| k.id.clone())
                .collect();
            for id in gone {
                if let Some(key) = inner.keys.remove(&id) {
                    inner.by_digest.remove(&key.digest);
                }
            }
        }
        removed
    }

    // Keys ------------------------------------------------------------------------------------

    /// The key with this SHA-256 digest, from the database or from the file (portal 8.3).
    pub fn key_by_digest(&self, digest: &[u8; 32]) -> Option<Arc<KeyRecord>> {
        if let Some(id) = self.file_digests.get(digest) {
            return self.file_keys.get(id).cloned();
        }
        let inner = self.read();
        inner
            .by_digest
            .get(digest)
            .and_then(|id| inner.keys.get(id))
            .cloned()
    }

    /// The key with this id, from the database or from the file. The id is the local part of the
    /// subject id of the key (protocol 3.2).
    pub fn key_by_id(&self, id: &str) -> Option<Arc<KeyRecord>> {
        self.file_keys
            .get(id)
            .cloned()
            .or_else(|| self.read().keys.get(id).cloned())
    }

    /// The keys of the database, then the keys of the file.
    pub fn keys(&self) -> Vec<Arc<KeyRecord>> {
        let mut keys: Vec<Arc<KeyRecord>> = self.read().keys.values().cloned().collect();
        keys.sort_by(|a, b| (a.created_at, &a.id).cmp(&(b.created_at, &b.id)));
        let mut file: Vec<Arc<KeyRecord>> = self.file_keys.values().cloned().collect();
        file.sort_by(|a, b| a.id.cmp(&b.id));
        keys.extend(file);
        keys
    }

    #[cfg(test)]
    pub fn key_count(&self) -> usize {
        self.read().keys.len() + self.file_keys.len()
    }

    /// The number of grants of the users and the keys of the database (portal 10.7d).
    pub fn grant_count(&self) -> usize {
        let inner = self.read();
        inner.users.values().map(|u| u.grants.len()).sum::<usize>()
            + inner.keys.values().map(|k| k.grants.len()).sum::<usize>()
    }

    /// The counters that the grants of the directory need: one for each window of each budget
    /// value on a grant `access` (portal 7.2a). `file` gives the grants of the accounts of the
    /// file. `active` tells whether a key of the database needs its counters: it never does for a
    /// revoked, expired or too-old key. A key of the file needs its counters always.
    pub fn counters_needed(
        &self,
        file: &HashMap<String, Vec<Grant>>,
        mut active: impl FnMut(&KeyRecord) -> bool,
    ) -> HashSet<(CounterOwner, CounterId)> {
        let inner = self.read();
        let mut out = HashSet::new();
        for (login, user) in &inner.users {
            for id in counter_ids(&user.grants) {
                out.insert((CounterOwner::User(login.clone()), id));
            }
        }
        for (login, grants) in file {
            for id in counter_ids(grants) {
                out.insert((CounterOwner::File(login.clone()), id));
            }
        }
        for key in inner.keys.values().filter(|k| active(k)) {
            for id in counter_ids(&key.grants) {
                out.insert((key.counter_owner(), id));
            }
        }
        for key in self.file_keys.values() {
            for id in counter_ids(&key.grants) {
                out.insert((key.counter_owner(), id));
            }
        }
        out
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
                    grants: row.grants,
                    created_at: row.created_at,
                },
            )
        })
        .collect()
}

/// A login as the database stores it. The login is trimmed and in lower case. It matches
/// `[a-z0-9._-]{1,32}`.
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

    /// The `access` grant on each named service.
    fn row(login: &str, services: &[&str]) -> UserRow {
        UserRow {
            login: login.into(),
            password_hash: "h".into(),
            grants: access(services),
            created_at: 1,
        }
    }

    fn access(services: &[&str]) -> Vec<Grant> {
        services
            .iter()
            .map(|s| Grant::new("access", s).unwrap())
            .collect()
    }

    #[test]
    fn the_age_of_the_directory_grows_until_a_read_of_the_database_succeeds() {
        let dir = Directory::new(vec![row("alice", &[])], 3);
        assert!(dir.age() < Duration::from_secs(5), "it starts fresh");
        // A directory that the database did not answer for a minute.
        dir.set_last_success(now_millis() - 60_000);
        assert!(dir.age() >= Duration::from_secs(60));
        // A refresh that works makes it fresh again.
        let store = Store::open_in_memory();
        dir.refresh(&store).unwrap();
        assert!(dir.age() < Duration::from_secs(5));
    }

    #[test]
    fn lookup_list_insert_and_remove() {
        let dir = Directory::new(vec![row("bob", &["dev"]), row("alice", &["ops", "dev"])], 3);
        assert_eq!(dir.count(), 2);
        assert_eq!(dir.grants_of("alice").unwrap(), access(&["ops", "dev"]));
        assert!(dir.get("nobody").is_none());
        assert_eq!(dir.list()[0].0, "alice", "sorted by login");
        assert!(!dir.is_full());
        dir.insert(
            "carol".into(),
            UserRecord {
                password_hash: "h".into(),
                grants: vec![],
                created_at: 2,
            },
        );
        assert!(dir.is_full());
        assert!(dir.update("alice", Some("h2".into()), Some(access(&["viewer"]))));
        assert_eq!(dir.get("alice").unwrap().password_hash, "h2");
        assert_eq!(dir.grants_of("alice").unwrap(), access(&["viewer"]));
        assert!(dir.update("alice", None, None));
        assert_eq!(
            dir.get("alice").unwrap().password_hash,
            "h2",
            "kept without a new hash"
        );
        assert_eq!(
            dir.grants_of("alice").unwrap(),
            access(&["viewer"]),
            "kept without grants"
        );
        assert!(dir.update("alice", None, Some(vec![])));
        assert!(dir.grants_of("alice").unwrap().is_empty());
        assert!(!dir.update("nobody", None, None));
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
        assert_eq!(dir.grants_of("alice").unwrap(), access(&["ops"]));
        store.remove_user("alice").unwrap();
        store.insert_user(&row("bob", &[])).unwrap();
        assert!(dir.refresh(&store).unwrap());
        assert!(
            dir.get("alice").is_none(),
            "a removal elsewhere is picked up"
        );
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
                grants: vec![],
                created_at: 1,
            },
        );
        assert!(!dir.replace_unless_changed(
            seen,
            crate::store::DirectorySnapshot {
                version: 0,
                users: read_before_the_insert,
                keys: Vec::new()
            }
        ));
        assert!(dir.get("carol").is_some(), "the insert is still there");
        assert!(dir.get("alice").is_none(), "the stale rows were dropped");
    }

    fn key_row(id: &str, owner: Option<&str>, byte: u8) -> KeyRow {
        KeyRow {
            id: id.into(),
            owner: owner.map(Into::into),
            name: format!("key {id}"),
            digest: [byte; 32],
            hint: "ab12".into(),
            created_at: 100,
            expires_at: None,
            last_check_at: None,
            check_count: 0,
            grants: access(&["db"]),
        }
    }

    fn caps() -> crate::store::KeyCaps {
        crate::store::KeyCaps {
            max_per_user: 10,
            max_counter_rows: 1000,
            max_directory_grants: 1000,
            now: 1_000,
            created_not_before: None,
        }
    }

    #[test]
    fn a_key_is_found_by_its_digest_or_its_id_with_its_owner() {
        let store = Store::open_in_memory();
        store.insert_user(&row("alice", &[])).unwrap();
        store
            .insert_key(&key_row("k1", Some("alice"), 1), &caps())
            .unwrap();
        store.insert_key(&key_row("s1", None, 2), &caps()).unwrap();
        let dir = Directory::from_snapshot(store.load_directory().unwrap(), 8);
        let key = dir.key_by_digest(&[1; 32]).unwrap();
        assert_eq!(
            (key.id.as_str(), &key.owner),
            ("k1", &KeyOwner::User("alice".into()))
        );
        assert_eq!(key.subject(), "key:k1");
        assert_eq!(dir.key_by_id("s1").unwrap().owner, KeyOwner::Superadmin);
        assert!(dir.key_by_digest(&[9; 32]).is_none());
        assert!(dir.key_by_id("nope").is_none());
        assert_eq!(dir.key_count(), 2);
        assert_eq!(dir.version(), store.directory_version().unwrap());
    }

    #[test]
    fn a_key_of_the_file_has_an_id_a_subject_and_counters_of_the_file() {
        let file = KeyRecord {
            id: "34b1100f45b235ba".into(),
            owner: KeyOwner::File("carol".into()),
            name: "nightly report".into(),
            digest: [7; 32],
            hint: String::new(),
            created_at: 0,
            expires_at: None,
            grants: access(&["db"]),
        };
        let dir = Directory::new(Vec::new(), 8).with_file_keys(vec![file.clone()]);
        assert_eq!(dir.key_by_digest(&[7; 32]).unwrap().as_ref(), &file);
        assert_eq!(dir.key_by_id("34b1100f45b235ba").unwrap().as_ref(), &file);
        assert!(file.is_from_file());
        assert_eq!(
            file.counter_owner(),
            CounterOwner::File("key:34b1100f45b235ba".into())
        );
        // A key of the file never expires and has no age (portal 3.4c).
        assert!(!file.expired(i64::MAX));
        assert!(!file.too_old(i64::MAX, Some(1)));
        assert_eq!(dir.keys().len(), 1);
    }

    #[test]
    fn a_key_is_expired_at_its_date_and_too_old_after_the_maximum_lifetime_of_its_owner() {
        let mut key = KeyRecord {
            id: "k".into(),
            owner: KeyOwner::User("alice".into()),
            name: "k".into(),
            digest: [1; 32],
            hint: String::new(),
            created_at: 1_000,
            expires_at: Some(2_000),
            grants: Vec::new(),
        };
        assert!(!key.expired(1_999) && key.expired(2_000) && key.expired(2_001));
        key.expires_at = None;
        assert!(!key.expired(i64::MAX));
        assert!(!key.too_old(5_000, None), "no maximum: no age");
        assert!(!key.too_old(1_100, Some(100)), "the age equals the maximum");
        assert!(key.too_old(1_101, Some(100)));
        assert!(
            key.too_old(1_001, Some(0)),
            "the value 0 refuses each key (portal 5.3a)"
        );
    }

    #[test]
    fn a_refresh_reads_the_database_only_when_the_version_changed() {
        let store = Store::open_in_memory();
        let dir = Directory::from_snapshot(store.load_directory().unwrap(), 8);
        assert!(
            !dir.refresh(&store).unwrap(),
            "same version: nothing to read"
        );
        store.insert_user(&row("alice", &["db"])).unwrap();
        store
            .insert_key(&key_row("k1", Some("alice"), 1), &caps())
            .unwrap();
        assert!(dir.refresh(&store).unwrap());
        assert_eq!(dir.version(), store.directory_version().unwrap());
        assert!(dir.key_by_id("k1").is_some() && dir.get("alice").is_some());
        assert!(!dir.refresh(&store).unwrap());
        // A revocation on the database reaches the directory with the next round.
        store.revoke_key("k1", None, 5).unwrap();
        assert!(dir.key_by_id("k1").is_some(), "the round has not run yet");
        assert!(dir.refresh(&store).unwrap());
        assert!(dir.key_by_id("k1").is_none());
        assert!(dir.key_by_digest(&[1; 32]).is_none());
        // A successful round that finds the same version still makes the directory young.
        dir.set_last_success(now_millis() - 60_000);
        dir.refresh(&store).unwrap();
        assert!(dir.age() < Duration::from_secs(5));
    }

    #[test]
    fn removing_a_user_removes_its_keys_from_the_directory() {
        let store = Store::open_in_memory();
        store.insert_user(&row("alice", &[])).unwrap();
        store
            .insert_key(&key_row("k1", Some("alice"), 1), &caps())
            .unwrap();
        store.insert_key(&key_row("s1", None, 2), &caps()).unwrap();
        let dir = Directory::from_snapshot(store.load_directory().unwrap(), 8);
        assert!(dir.remove("alice"));
        assert!(dir.key_by_id("k1").is_none() && dir.key_by_digest(&[1; 32]).is_none());
        assert!(
            dir.key_by_id("s1").is_some(),
            "the key of the superadmin stays"
        );
    }

    #[test]
    fn the_counters_that_the_directory_needs_follow_its_grants() {
        use crate::limits::Limits;
        let budget = Limits::from_json(r#"{"requests":[{"amount":1,"window":"minute"}]}"#).unwrap();
        let with_budget = vec![Grant::new("access", "db").unwrap().with_limits(budget)];
        let mut alice = row("alice", &[]);
        alice.grants = with_budget.clone();
        let mut key = key_row("k1", Some("alice"), 1);
        key.grants = with_budget.clone();
        let mut old = key_row("old", Some("alice"), 2);
        old.grants = with_budget.clone();
        let store = Store::open_in_memory();
        store.insert_user(&alice).unwrap();
        store.insert_key(&key, &caps()).unwrap();
        store.insert_key(&old, &caps()).unwrap();
        let dir = Directory::from_snapshot(store.load_directory().unwrap(), 8);
        let file: HashMap<String, Vec<Grant>> = [("carol".to_string(), with_budget)].into();
        let needed = dir.counters_needed(&file, |k| k.id != "old");
        let owners: Vec<&CounterOwner> = {
            let mut owners: Vec<&CounterOwner> = needed.iter().map(|(o, _)| o).collect();
            owners.sort();
            owners
        };
        assert_eq!(
            owners,
            [
                &CounterOwner::User("alice".into()),
                &CounterOwner::Key("k1".into()),
                &CounterOwner::File("carol".into()),
            ]
        );
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
