//! The authorizer that the portal gives to the inbox (see `crate::inbox::rights`).
//!
//! The inbox does not read the state of the portal. This module reads it for the inbox: the keys,
//! the grants of the accounts and the counters. It never gives a wrong refusal for a directory that
//! is not ready: it answers "not ready" or "unknown", and the inbox then changes nothing.

use std::collections::BTreeSet;
use std::sync::Weak;

use super::pdp::{decide_key_for, UseVerdict};
use super::*;
use crate::keyauth::{self, Cause};
use crate::inbox::rights::{Authorizer, Decision, KeyCheck, Subscribable};
use crate::inbox_actions as actions;

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

impl Authorizer for PortalAuthorizer {
    fn check_key(&self, token: &str) -> KeyCheck {
        let Some(state) = self.state.upgrade() else {
            return KeyCheck::NotReady;
        };
        if state.config.keys.is_none() || crate::keys::check_format(token).is_err() {
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
        let Some(state) = self.state.upgrade() else {
            return Decision::NotReady;
        };
        let Some(chain) = actions::group_chain(group) else {
            return Decision::Refused {
                reason: "no_permission",
            };
        };
        let verdict = decide_key_for(&state, key_id, actions::SERVICE, action, &chain, count_use);
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
                ("group", group.to_string()),
                ("counted", count_use.to_string()),
                ("decision", word.to_string()),
            ],
        );
        decision
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
        let Some(document) = state.actions.declared(actions::SERVICE) else {
            return Subscribable::Unknown;
        };
        // An account that the directory does not know is not a refusal: it can be new on another
        // instance.
        let Some((grants, _)) = keyauth::person_grants_on(&state, login, actions::SERVICE) else {
            return Subscribable::Unknown;
        };
        let allowed: BTreeSet<String> =
            actions::groups_with(&document, login, &grants, actions::GROUP_SUBSCRIBE, ids)
                .into_iter()
                .collect();
        Subscribable::Groups(allowed)
    }

    fn directory_stamp(&self) -> Option<u64> {
        let state = self.state.upgrade()?;
        directory_fresh(&state).then(|| state.users.generation())
    }
}
