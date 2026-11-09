//! The authorizer that the portal gives to the inbox (see `crate::inbox::rights`).
//!
//! The inbox does not read the state of the portal. This module reads it for the inbox: the keys,
//! the grants of the accounts and the counters. It never gives a wrong refusal for a directory that
//! is not ready: it answers "not ready" or "unknown", and the inbox then changes nothing.

use std::collections::BTreeSet;
use std::sync::Weak;

use super::pdp::{decide_key_for, UseVerdict};
use super::*;
use crate::inbox::agents::AgentMode;
use crate::inbox::rights::{
    AgentCheck, AgentKey, Authorizer, Decision, Door, KeyCheck, Services, Standing, Subscribable,
};
use crate::keyauth::{self, Cause};
use crate::portal_actions as actions;

/// The mode of the agents of a key, as the engine of the agents reads it.
fn agent_mode(mode: crate::keys::AgentsMode) -> AgentMode {
    match mode {
        crate::keys::AgentsMode::Key => AgentMode::Key,
        crate::keys::AgentsMode::Discover => AgentMode::Discover,
        crate::keys::AgentsMode::Enrolled => AgentMode::Enrolled,
    }
}

/// The age after which the inbox does not trust the directory of a shared database, when the
/// file has no block `keys` to say it.
const FALLBACK_MAX_AGE: Duration = Duration::from_secs(60);

pub(super) struct PortalAuthorizer {
    /// The state of the portal holds the inbox, so the inbox must not hold the state strongly.
    state: Weak<AppState>,
}

impl PortalAuthorizer {
    pub(super) fn new(state: &Arc<AppState>) -> Self {
        Self {
            state: Arc::downgrade(state),
        }
    }
}

/// Whether the directory is young enough to decide on. With one instance on SQLite, the directory
/// is the one source and it is always young. With a shared database, the last good read of the
/// directory must be recent.
pub(super) fn directory_fresh(state: &AppState) -> bool {
    if !state.store.is_shared() {
        return true;
    }
    let max = state
        .config
        .keys
        .as_ref()
        .map_or(FALLBACK_MAX_AGE, |keys| keys.directory_max_age);
    state.users.age() <= max
}

impl PortalAuthorizer {
    /// The decision for a key on the node `chain` of the service `portal`. `group` is the id of
    /// the group of the chain, for the log.
    fn decide_on(
        &self,
        key_id: &str,
        action: &str,
        chain: &[crate::grants::Segment],
        group: Option<&str>,
        count_use: bool,
    ) -> Decision {
        let Some(state) = self.state.upgrade() else {
            return Decision::NotReady;
        };
        let verdict = decide_key_for(&state, key_id, actions::SERVICE, action, chain, count_use);
        let (decision, word) = match verdict {
            UseVerdict::Allowed => (Decision::Allowed, "allow"),
            UseVerdict::UnknownSubject => (
                Decision::Refused {
                    reason: "key_not_valid",
                },
                "key_not_valid",
            ),
            UseVerdict::Denied => (
                Decision::Refused {
                    reason: "no_permission",
                },
                "deny",
            ),
            UseVerdict::Limited(retry_after) => (Decision::Limited { retry_after }, "limited"),
            UseVerdict::NotReady => (Decision::NotReady, "not_ready"),
            UseVerdict::Unavailable => (Decision::Unavailable, "unavailable"),
        };
        log(
            &state,
            LogLevel::Info,
            "inbox_key_decision",
            &[
                ("key", key_id.to_string()),
                ("action", action.to_string()),
                ("group", group.unwrap_or("").to_string()),
                ("counted", count_use.to_string()),
                ("decision", word.to_string()),
            ],
        );
        decision
    }
}

impl Authorizer for PortalAuthorizer {
    fn check_agent_key(&self, token: &str, door: Door) -> AgentCheck {
        use crate::keys::KeyKind;
        let Some(state) = self.state.upgrade() else {
            return AgentCheck::NotReady;
        };
        let kind = match door {
            Door::Mcp => KeyKind::Mcp,
            Door::Rest => KeyKind::Api,
        };
        // The file turns the kind on (`keys.mcp`, `keys.api`), and the text has the form of the kind.
        let door_open = state
            .config
            .keys
            .as_ref()
            .is_some_and(|keys| keys.kinds().contains(&kind));
        if !door_open || crate::keys::check_format(token) != Ok(kind) {
            return AgentCheck::NotAKey;
        }
        if keyauth::directory_too_old(&state) {
            state.key_stats.failed(Cause::DirectoryTooOld);
            return AgentCheck::NotReady;
        }
        let digest = crate::keys::digest(token);
        let key = state.users.key_by_digest(&digest).or_else(|| {
            // The key can come from another instance, a few seconds ago (portal 10.7c).
            refresh_directory_if_due(&state)
                .then(|| state.users.key_by_digest(&digest))
                .flatten()
        });
        let Some(key) = key else {
            state.key_stats.failed(Cause::UnknownKey);
            return AgentCheck::Invalid;
        };
        if key.kind != kind {
            state.key_stats.failed(Cause::WrongKind);
            return AgentCheck::Invalid;
        }
        let now = unix_now();
        match keyauth::resolve(&state, &key, now) {
            Ok(principal) => {
                state.key_checks.record(&key, now);
                state.key_stats.passed();
                AgentCheck::Valid(AgentKey {
                    key_id: key.id.clone(),
                    owner: principal.owner_login(&state.superadmin_login).to_string(),
                    interaction: match key.interaction {
                        crate::keys::Interaction::Conversational => {
                            inbox_core::proto::Interaction::Conversational
                        }
                        crate::keys::Interaction::OneShot => {
                            inbox_core::proto::Interaction::OneShot
                        }
                    },
                    agents: agent_mode(key.agents),
                })
            }
            Err(Cause::DirectoryTooOld) => {
                state.key_stats.failed(Cause::DirectoryTooOld);
                AgentCheck::NotReady
            }
            Err(cause) => {
                state.key_stats.failed(cause);
                AgentCheck::Invalid
            }
        }
    }

    fn check_key(&self, token: &str) -> KeyCheck {
        let Some(state) = self.state.upgrade() else {
            return KeyCheck::NotReady;
        };
        // The key of the database that this door accepts has the kind `api`, and the file turns
        // this kind on (portal 8.3a).
        let door_open = state.config.keys.as_ref().is_some_and(|keys| keys.api);
        if !door_open || crate::keys::check_format(token) != Ok(crate::keys::KeyKind::Api) {
            return KeyCheck::NotAKey;
        }
        if keyauth::directory_too_old(&state) {
            state.key_stats.failed(Cause::DirectoryTooOld);
            return KeyCheck::NotReady;
        }
        let digest = crate::keys::digest(token);
        let key = state.users.key_by_digest(&digest).or_else(|| {
            // The key can come from another instance, a few seconds ago (portal 10.7c).
            refresh_directory_if_due(&state)
                .then(|| state.users.key_by_digest(&digest))
                .flatten()
        });
        let Some(key) = key else {
            state.key_stats.failed(Cause::UnknownKey);
            return KeyCheck::Invalid;
        };
        if key.kind != crate::keys::KeyKind::Api {
            state.key_stats.failed(Cause::WrongKind);
            return KeyCheck::Invalid;
        }
        let now = unix_now();
        match keyauth::resolve(&state, &key, now) {
            Ok(_) => {
                state.key_checks.record(&key, now);
                state.key_stats.passed();
                KeyCheck::Valid {
                    key_id: key.id.clone(),
                }
            }
            Err(Cause::DirectoryTooOld) => {
                state.key_stats.failed(Cause::DirectoryTooOld);
                KeyCheck::NotReady
            }
            Err(cause) => {
                state.key_stats.failed(cause);
                KeyCheck::Invalid
            }
        }
    }

    fn decide(&self, key_id: &str, action: &str, group: &str, count_use: bool) -> Decision {
        let Some(chain) = actions::group_chain(group) else {
            return Decision::Refused {
                reason: "no_permission",
            };
        };
        self.decide_on(key_id, action, &chain, Some(group), count_use)
    }

    fn decide_service(&self, key_id: &str, action: &str, count_use: bool) -> Decision {
        self.decide_on(key_id, action, &[], None, count_use)
    }

    fn user_exists(&self, login: &str) -> Option<bool> {
        let state = self.state.upgrade()?;
        if !directory_fresh(&state) {
            return None;
        }
        Some(keyauth::person_exists(&state, login))
    }

    fn subscribable(&self, login: &str) -> Subscribable {
        let Some(state) = self.state.upgrade() else {
            return Subscribable::Unknown;
        };
        if !directory_fresh(&state) {
            return Subscribable::Unknown;
        }
        let Some(inbox) = &state.config.inbox else {
            return Subscribable::Unknown;
        };
        let ids = inbox.groups.keys().cloned();
        if constant_time_eq(login.as_bytes(), state.superadmin_login.as_bytes()) {
            return Subscribable::Groups(ids.collect());
        }
        let Some((grants, _)) = keyauth::person_grants_on(&state, login, actions::SERVICE) else {
            // With a shared database, an account that the directory does not know is not a refusal:
            // it can be new on another instance, or be in the file of another instance (see
            // `standing_of`).
            return if state.store.is_shared() {
                Subscribable::Unknown
            } else {
                Subscribable::Absent
            };
        };
        let Some(document) = state.actions.declared(actions::SERVICE) else {
            return Subscribable::Unknown;
        };
        let allowed: BTreeSet<String> =
            actions::groups_with(&document, login, &grants, actions::INBOX_RECEIVE, ids)
                .into_iter()
                .collect();
        Subscribable::Groups(allowed)
    }

    fn standing(&self, login: &str) -> Standing {
        match self.state.upgrade() {
            Some(state) => standing_of(&state, login),
            None => Standing::Unknown,
        }
    }

    fn directory_stamp(&self) -> Option<u64> {
        let state = self.state.upgrade()?;
        directory_fresh(&state).then(|| state.users.generation())
    }

    fn services(&self, login: &str) -> Option<Services> {
        let state = self.state.upgrade()?;
        services_of(&state, login)
    }

    fn key_services(&self, key_id: &str) -> Option<Services> {
        use crate::users::KeyOwner;
        let state = self.state.upgrade()?;
        if !directory_fresh(&state) {
            return None;
        }
        match &state.users.key_by_id(key_id)?.owner {
            KeyOwner::Superadmin => Some(Services::All),
            KeyOwner::User(login) | KeyOwner::File(login) => services_of(&state, login),
        }
    }

    fn key_name(&self, key_id: &str) -> Option<String> {
        let state = self.state.upgrade()?;
        state.users.key_by_id(key_id).map(|k| k.name.clone())
    }

    fn key_agent_mode(&self, key_id: &str) -> Option<AgentMode> {
        let state = self.state.upgrade()?;
        state.users.key_by_id(key_id).map(|k| agent_mode(k.agents))
    }

    fn audience(&self, service: &str) -> BTreeSet<String> {
        let Some(state) = self.state.upgrade() else {
            return BTreeSet::new();
        };
        let mut logins: BTreeSet<String> = state
            .users
            .list()
            .into_iter()
            .map(|(login, _)| login)
            .collect();
        logins.extend(state.config.access.accounts.keys().cloned());
        logins.insert(state.superadmin_login.clone());
        logins
            .into_iter()
            .filter(|login| services_of(&state, login).is_some_and(|seen| seen.sees(Some(service))))
            .collect()
    }
}

/// The services whose alerts the person `login` may see: the services that the launcher shows to
/// the person (see `Services::of_grants`). The superadmin sees every service. `None`: the
/// directory is not ready or is old, or it does not know the person. The inbox then shows no alert
/// about a service to this person.
fn services_of(state: &AppState, login: &str) -> Option<Services> {
    if constant_time_eq(login.as_bytes(), state.superadmin_login.as_bytes()) {
        return Some(Services::All);
    }
    if !directory_fresh(state) {
        return None;
    }
    let (grants, _) = keyauth::person_grants(state, login)?;
    Some(Services::of_grants(Some(&grants)))
}

/// Whether an account has `login`: the superadmin, an account of the file or an account of the
/// database. With SQLite, this instance holds every account, so a login that it does not know is
/// absent. With a shared database, the call reads the accounts again first: an account can be new
/// on another instance. Even then, the file of another instance can hold the login, so the answer
/// is only "not here".
fn standing_of(state: &AppState, login: &str) -> Standing {
    if !directory_fresh(state) {
        return Standing::Unknown;
    }
    if keyauth::person_exists(state, login) {
        return Standing::Exists;
    }
    if !state.store.is_shared() {
        return Standing::Absent;
    }
    let since = crate::users::Directory::clock_millis();
    if !blocking(|| state.users.refresh_since(&state.store, since)) {
        return Standing::Unknown;
    }
    if keyauth::person_exists(state, login) {
        Standing::Exists
    } else {
        Standing::NotHere
    }
}
