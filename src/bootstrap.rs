//! The checks and the work of the start of the program that need the database and the file
//! together.
//!
//! The file and the database are two sources of accounts. An account and its keys come from one
//! source: a login that both hold, or a key that both hold, stops the program. The counter rows of
//! the subjects of the file are made at each start, and the rows of a subject that the file no
//! longer has are deleted.

use std::collections::HashSet;

use crate::config::Config;
use crate::decision::CounterOwner;
use crate::grants::{CounterId, ACCESS};
use crate::store::{DirectorySnapshot, Store};
use crate::users::{Directory, KeyOwner, KeyRecord};

/// The keys of the file as the directory holds them. The key of the file has no creation date and
/// no expiry date.
pub fn file_keys(config: &Config) -> Vec<KeyRecord> {
    let mut out = Vec::new();
    for (login, account) in &config.access.accounts {
        for key in &account.keys {
            out.push(KeyRecord {
                id: key.id.clone(),
                owner: KeyOwner::File(login.clone()),
                name: key.name.clone(),
                digest: key.digest,
                hint: String::new(),
                created_at: 0,
                expires_at: None,
                kind: crate::keys::KeyKind::Mcp,
                interaction: key.interaction,
                // A key of the file is an MCP key. The file has no attribute for the mode of the agents.
                agents: crate::keys::AgentsMode::default_for(crate::keys::KeyKind::Mcp),
                grants: key.grants.clone(),
            });
        }
    }
    out
}

/// Refuses a database that holds an account with the login of the superadmin. The program
/// recognises the superadmin by its login: the rights of the Inbox, the direct alerts, the PDP, the second
/// factor and the identity token. The login therefore has one owner. The error does not show the login, because
/// the configuration hides it.
pub fn check_superadmin_against_database(
    superadmin_login: &str,
    snapshot: &DirectorySnapshot,
) -> Result<(), String> {
    if snapshot
        .users
        .iter()
        .any(|user| user.login == superadmin_login)
    {
        return Err("authentication.superadmin_login is also the login of an account of the database: an account comes from one source, so delete that account or give the superadmin another login (the value is hidden)".into());
    }
    Ok(())
}

/// Refuses a file that holds a login, a key id or a key digest that the database holds too
///. Each error names the login and, for a key, its name.
pub fn check_file_against_database(
    config: &Config,
    snapshot: &DirectorySnapshot,
) -> Result<(), String> {
    for (login, account) in &config.access.accounts {
        if snapshot.users.iter().any(|user| user.login == *login) {
            return Err(format!(
                "the login {login:?} is in the configuration file (a block user) and in the database: an account comes from one source, so remove the block or delete the account"
            ));
        }
        for key in &account.keys {
            if let Some(other) = snapshot.keys.iter().find(|k| k.id == key.id) {
                return Err(format!(
                    "user {login:?}, key {:?}: the subject id key:{} is also the id of a key of the database (named {:?})",
                    key.name, key.id, other.name
                ));
            }
            if snapshot.keys.iter().any(|k| k.digest == key.digest) {
                return Err(format!(
                    "user {login:?}, key {:?}: the digest of the key is also the digest of a key of the database",
                    key.name
                ));
            }
        }
    }
    Ok(())
}

/// Refuses a user of the database that holds more grants than `users.max_grants`.
/// The error names the login, the number of grants and the maximum.
pub fn check_user_grants(config: &Config, snapshot: &DirectorySnapshot) -> Result<(), String> {
    let maximum = config.users.max_grants;
    match snapshot
        .users
        .iter()
        .find(|user| user.grants.len() > maximum)
    {
        Some(user) => Err(format!(
            "user {:?}: the database holds {} grants, users.max_grants is {maximum}",
            user.login,
            user.grants.len()
        )),
        None => Ok(()),
    }
}

/// Refuses a file that gives a right on an external service. An external service is a link: an account of the file
/// may hold `access` on it, and nothing else. A key of the file never holds a right on it. A name of a block
/// `pdp_client` is a service that asks the PDP: it is never an external service. Each error names the block, the
/// attribute and the grant. `ids` are the ids of the external services that apply: the blocks `external_service`
/// of the file, then the rows of the database. A grant on an id that no external service has is not checked: the
/// start cannot know that the id will name an external service.
pub fn check_externals(
    config: &Config,
    ids: &[String],
    pdp_names: &[String],
) -> Result<(), String> {
    let external = |service: &str| ids.iter().any(|id| id == service);
    for (login, account) in &config.access.accounts {
        if let Some(grant) = account
            .grants
            .iter()
            .find(|g| external(&g.service) && (g.action != ACCESS || !g.path.is_empty()))
        {
            return Err(format!(
                "user {login:?}, grants: \"{grant}\" names the external service {}: an external service is a link, and an account holds only \"access on service:{}\" on it",
                grant.service, grant.service
            ));
        }
        for key in &account.keys {
            if let Some(grant) = key.grants.iter().find(|g| external(&g.service)) {
                return Err(format!(
                    "user {login:?}, key {:?}, grants: \"{grant}\" names the external service {}: a key holds no right on an external service",
                    key.name, grant.service
                ));
            }
        }
    }
    if let Some(name) = pdp_names.iter().find(|name| external(name)) {
        return Err(format!(
            "pdp_client {name:?}: the name is the id of an external service, and an external service is never a client of the PDP: delete the external service or rename the block"
        ));
    }
    Ok(())
}

/// Refuses a row of the database that does not follow the rules of the external services: the name and its id, the
/// link, the description and the icon. A row that the form of the Settings page made follows them. A row that an
/// older version or a hand made it can break them: the start stops, and the error names the row. The row stays in
/// the database: delete or fix it, then start again.
pub fn check_external_rows(snapshot: &DirectorySnapshot) -> Result<(), String> {
    for row in &snapshot.externals {
        crate::catalog::external::check_entry(row).map_err(|reason| {
            format!(
                "the database holds an external service that the rules refuse: the row {:?} of the table external_services: {reason}; fix or delete the row, then start again",
                row.id
            )
        })?;
    }
    Ok(())
}

/// The counters that the grants of the directory and of the file need.
pub fn needed_counters(
    config: &Config,
    directory: &Directory,
    now: i64,
) -> HashSet<(CounterOwner, CounterId)> {
    let file: std::collections::HashMap<String, Vec<crate::grants::Grant>> = config
        .access
        .accounts
        .iter()
        .map(|(login, account)| (login.clone(), account.grants.clone()))
        .collect();
    directory.counters_needed(&file, |key| {
        // A revoked key is not in the directory. An expired or too-old key needs no counter.
        if key.expired(now) {
            return false;
        }
        let max = match &key.owner {
            KeyOwner::User(login) => directory
                .grants_of(login)
                .and_then(|grants| crate::portal_actions::max_key_lifetime(&grants, key.kind)),
            _ => None,
        };
        !key.too_old(now, max)
    })
}

/// The subjects of the file that hold counter rows: the logins of its accounts and the subject
/// ids of its keys.
pub fn file_subjects(config: &Config) -> Vec<String> {
    let mut out = Vec::new();
    for (login, account) in &config.access.accounts {
        out.push(login.clone());
        for key in &account.keys {
            out.push(format!("{}{}", crate::subject_id::KEY_PREFIX, key.id));
        }
    }
    out
}

/// What the start does with the counters: it checks that the rows fit in `keys.max_counter_rows`
/// and the grants in `keys.max_directory_grants`, makes the rows that the directory and the file
/// need, and deletes the rows of the subjects that the file no longer has.
pub fn prepare_counters(
    config: &Config,
    store: &Store,
    directory: &Directory,
    now: i64,
) -> Result<HashSet<(CounterOwner, CounterId)>, String> {
    let limits = &config.key_limits;
    let needed = needed_counters(config, directory, now);
    let existing = store.counter_rows()?;
    let needed_rows = needed.len() as i64;
    // The grants alone need more rows than the table can hold: nothing to make.
    if needed_rows > limits.max_counter_rows as i64 {
        return Err(format!(
            "keys.max_counter_rows is {}: the database holds {existing} counter rows and the grants need {needed_rows}",
            limits.max_counter_rows
        ));
    }
    let file_grants: usize = config
        .access
        .accounts
        .values()
        .map(|a| a.grants.len() + a.keys.iter().map(|k| k.grants.len()).sum::<usize>())
        .sum();
    let total_grants = directory.grant_count() + file_grants;
    if total_grants > limits.max_directory_grants {
        return Err(format!(
            "keys.max_directory_grants is {}: the database and the file hold {total_grants} grants",
            limits.max_directory_grants
        ));
    }
    store.delete_file_counters(&file_subjects(config))?;
    let rows: Vec<(CounterOwner, CounterId)> = needed.iter().cloned().collect();
    store.ensure_counter_rows(&rows)?;
    let after = store.counter_rows()?;
    if after > limits.max_counter_rows as i64 {
        return Err(format!(
            "keys.max_counter_rows is {}: the database holds {after} counter rows after the start made the rows that the grants need",
            limits.max_counter_rows
        ));
    }
    Ok(needed)
}

/// Makes the counter rows that a new version of the directory needs and that the instance did not
/// make yet. `ensured` holds the counters that the instance already has. Returns the
/// number of rows that the call asked the database to make.
pub fn ensure_new_counters(
    config: &Config,
    store: &Store,
    directory: &Directory,
    ensured: &std::sync::Mutex<HashSet<(CounterOwner, CounterId)>>,
    rows_made: &std::sync::atomic::AtomicU64,
    now: i64,
) -> Result<usize, String> {
    let needed = needed_counters(config, directory, now);
    let fresh: Vec<(CounterOwner, CounterId)> = {
        let known = ensured.lock().unwrap_or_else(|p| p.into_inner());
        needed
            .iter()
            .filter(|c| !known.contains(*c))
            .cloned()
            .collect()
    };
    if fresh.is_empty() {
        return Ok(0);
    }
    let made = store.ensure_counter_rows(&fresh)?;
    rows_made.fetch_add(made as u64, std::sync::atomic::Ordering::Relaxed);
    ensured
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .extend(fresh);
    Ok(made)
}
