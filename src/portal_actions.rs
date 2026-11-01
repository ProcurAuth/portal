//! The service `portal`: the actions that manage the keys (portal 6).
//!
//! The portal is a service with its own action document. The code holds the document: no read of
//! the catalogue replaces it, and the PDP reads it from memory. Each grant of an action of the
//! service is on the node `service:portal`.
//!
//! The actions of `keys.own` need `keys.own.read` to show their page, and the actions of
//! `keys.all` need `users.read` (portal 6.3). The PDP treats an action as absent when the user
//! does not have the action that it needs (portal 6.3a).

#![allow(dead_code)]

use std::sync::{Arc, OnceLock};

use crate::actions::Document;
use crate::grants::Grant;

/// The id of the service.
pub const SERVICE: &str = "portal";

pub const KEYS_OWN_READ: &str = "keys.own.read";
pub const KEYS_OWN_CREATE: &str = "keys.own.create";
pub const KEYS_OWN_EDIT: &str = "keys.own.edit";
pub const KEYS_OWN_REVOKE: &str = "keys.own.revoke";
pub const KEYS_OWN_USE: &str = "keys.own.use";
pub const KEYS_ALL_READ: &str = "keys.all.read";
pub const KEYS_ALL_REVOKE: &str = "keys.all.revoke";
pub const USERS_READ: &str = "users.read";

/// The name of the ceiling of `keys.own.use`: the longest lifetime of a key, in seconds.
pub const MAX_KEY_LIFETIME: &str = "max_key_lifetime";

/// The document of portal 6.1a.
const DOCUMENT: &str = r#"{ "server": "portal",
  "resource_types": [],
  "actions": [ { "name": "keys.own.read", "resource_types": ["service"], "description": "See the keys of oneself." },
               { "name": "keys.own.create", "resource_types": ["service"], "description": "Make a key of oneself." },
               { "name": "keys.own.edit", "resource_types": ["service"],
                 "description": "Change the name, the grants and the limit values of a key of oneself." },
               { "name": "keys.own.revoke", "resource_types": ["service"], "description": "Revoke a key of oneself." },
               { "name": "keys.own.use", "resource_types": ["service"],
                 "description": "Use the keys of oneself at the edge.", "limits": ["max_key_lifetime"] },
               { "name": "keys.all.read", "resource_types": ["service"], "description": "See the keys of all the users." },
               { "name": "keys.all.revoke", "resource_types": ["service"], "description": "Revoke a key of any user." },
               { "name": "users.read", "resource_types": ["service"], "description": "See the users and their grants." } ],
  "limits": [ { "name": "max_key_lifetime", "label": "Longest key lifetime",
                "description": "The longest lifetime of a key, in seconds.",
                "unit": "second", "kind": "ceiling", "min": 0, "max": 31536000 } ] }"#;

/// The document of the service `portal`.
pub fn document() -> Arc<Document> {
    static CELL: OnceLock<Arc<Document>> = OnceLock::new();
    Arc::clone(CELL.get_or_init(|| {
        // The text is a constant of the code, and a test parses it.
        Arc::new(
            crate::actions::parse(SERVICE, DOCUMENT.as_bytes())
                .unwrap_or_else(|e| panic!("the document of the service portal is bad: {e}")),
        )
    }))
}

/// The names of the actions of the service.
pub fn action_names() -> Vec<&'static str> {
    vec![
        KEYS_OWN_READ,
        KEYS_OWN_CREATE,
        KEYS_OWN_EDIT,
        KEYS_OWN_REVOKE,
        KEYS_OWN_USE,
        KEYS_ALL_READ,
        KEYS_ALL_REVOKE,
        USERS_READ,
    ]
}

/// Whether the name is the name of an action of the service `portal`.
pub fn is_action(name: &str) -> bool {
    action_names().contains(&name)
}

/// The actions that an action needs (portal 6.3).
pub fn needs(action: &str) -> &'static [&'static str] {
    match action {
        KEYS_OWN_CREATE | KEYS_OWN_EDIT | KEYS_OWN_REVOKE => &[KEYS_OWN_READ],
        KEYS_ALL_REVOKE => &[KEYS_ALL_READ, USERS_READ],
        KEYS_ALL_READ => &[USERS_READ],
        _ => &[],
    }
}

/// Whether the grants hold the action of the service `portal` on the node `service:portal`.
pub fn holds(grants: &[Grant], action: &str) -> bool {
    grants
        .iter()
        .any(|g| g.service == SERVICE && g.path.is_empty() && g.action == action)
}

/// Whether the action counts for these grants: the grants hold it, and hold each action that it
/// needs (portal 6.3a).
pub fn effective(grants: &[Grant], action: &str) -> bool {
    holds(grants, action) && needs(action).iter().all(|need| holds(grants, need))
}

/// The longest lifetime of a key that the grants give, in seconds: the lowest value of the
/// ceiling `max_key_lifetime` on the grants of `keys.own.use`. `None`: no value.
pub fn max_key_lifetime(grants: &[Grant]) -> Option<u64> {
    grants
        .iter()
        .filter(|g| g.service == SERVICE && g.path.is_empty() && g.action == KEYS_OWN_USE)
        .filter_map(|g| match g.limits.get(MAX_KEY_LIFETIME) {
            Some(crate::limits::Value::Ceiling(n)) => Some(*n),
            _ => None,
        })
        .min()
}

/// Checks the grants of one user of the file or of the database against the rules of the service
/// `portal` (portal 6.3, 6.3a): each grant is on the node `service:portal` and names a known
/// action, and each action has the actions that it needs. The grants of other services are not
/// checked.
pub fn check_user_grants(grants: &[Grant]) -> Result<(), String> {
    for grant in grants.iter().filter(|g| g.service == SERVICE) {
        if !grant.path.is_empty() {
            return Err(format!(
                "the grant {grant} is below the service portal: each grant of the service portal is on service:portal"
            ));
        }
        if grant.action == crate::grants::ACCESS {
            continue;
        }
        if !is_action(&grant.action) {
            return Err(format!(
                "the service portal has no action {:?}: the actions are {}",
                grant.action,
                action_names().join(", ")
            ));
        }
        for need in needs(&grant.action) {
            if !holds(grants, need) {
                return Err(format!(
                    "the action {} needs the action {need}: add the grant `{need} on service:portal`",
                    grant.action
                ));
            }
        }
    }
    Ok(())
}

/// A key never holds an action of the service `portal` (portal 6.5).
pub fn check_key_grants(grants: &[Grant]) -> Result<(), String> {
    match grants
        .iter()
        .find(|g| g.service == SERVICE && g.action != crate::grants::ACCESS)
    {
        Some(grant) => Err(format!(
            "the grant {grant} is an action of the service portal: a key never holds one"
        )),
        None => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn grants(texts: &[&str]) -> Vec<Grant> {
        texts.iter().map(|t| Grant::parse(t).unwrap()).collect()
    }

    #[test]
    fn the_document_of_the_service_is_good_and_lists_the_eight_actions() {
        let document = document();
        let names: Vec<&str> = document.actions.iter().map(|a| a.name.as_str()).collect();
        assert_eq!(names, action_names());
        assert_eq!(document.limits.len(), 1);
        assert_eq!(document.limits[0].name, MAX_KEY_LIFETIME);
        assert_eq!(document.limits[0].range, Some((0, 31_536_000)));
        let use_action = document
            .actions
            .iter()
            .find(|a| a.name == KEYS_OWN_USE)
            .unwrap();
        assert_eq!(use_action.limits, [MAX_KEY_LIFETIME]);
        assert!(
            document.types.is_empty(),
            "no resource type below the service"
        );
    }

    #[test]
    fn an_action_is_effective_only_with_the_actions_that_it_needs() {
        let all = grants(&[
            "keys.own.read on service:portal",
            "keys.own.create on service:portal",
            "keys.all.revoke on service:portal",
        ]);
        assert!(effective(&all, KEYS_OWN_READ));
        assert!(effective(&all, KEYS_OWN_CREATE));
        assert!(
            !effective(&all, KEYS_ALL_REVOKE),
            "it needs keys.all.read and users.read"
        );
        assert!(!effective(&all, KEYS_OWN_EDIT), "the user does not hold it");
        let alone = grants(&["keys.own.create on service:portal"]);
        assert!(holds(&alone, KEYS_OWN_CREATE));
        assert!(
            !effective(&alone, KEYS_OWN_CREATE),
            "it needs keys.own.read"
        );
        assert!(effective(
            &grants(&["keys.own.use on service:portal"]),
            KEYS_OWN_USE
        ));
    }

    #[test]
    fn the_grants_of_a_user_follow_the_rules_of_the_service() {
        assert!(check_user_grants(&grants(&["access on service:db"])).is_ok());
        assert!(check_user_grants(&grants(&[
            "keys.own.read on service:portal",
            "keys.own.edit on service:portal",
        ]))
        .is_ok());
        for bad in [
            vec!["keys.own.edit on service:portal"],
            vec!["keys.all.read on service:portal"],
            vec![
                "keys.all.revoke on service:portal",
                "keys.all.read on service:portal",
            ],
            vec!["keys.own.nothing on service:portal"],
            vec!["keys.own.read on service:portal/host:x"],
        ] {
            assert!(check_user_grants(&grants(&bad)).is_err(), "{bad:?}");
        }
        // A grant of another service with the same action name is not a grant of the portal.
        assert!(check_user_grants(&grants(&["keys.own.edit on service:other"])).is_ok());
    }

    #[test]
    fn a_key_never_holds_an_action_of_the_service_portal() {
        assert!(check_key_grants(&grants(&[
            "access on service:db",
            "data.read on service:db/host:x"
        ]))
        .is_ok());
        assert!(check_key_grants(&grants(&["access on service:portal"])).is_ok());
        assert!(check_key_grants(&grants(&["keys.own.read on service:portal"])).is_err());
    }

    #[test]
    fn the_lowest_longest_lifetime_applies() {
        use crate::limits::Limits;
        let with = |value: &str| {
            Grant::parse("keys.own.use on service:portal")
                .unwrap()
                .with_limits(Limits::from_json(value).unwrap())
        };
        assert_eq!(max_key_lifetime(&[with("{}")]), None);
        assert_eq!(
            max_key_lifetime(&[with(r#"{"max_key_lifetime":2592000}"#)]),
            Some(2_592_000)
        );
        assert_eq!(
            max_key_lifetime(&[
                with(r#"{"max_key_lifetime":100}"#),
                with(r#"{"max_key_lifetime":7}"#)
            ]),
            Some(7)
        );
        assert_eq!(
            max_key_lifetime(&[with(r#"{"max_key_lifetime":0}"#)]),
            Some(0)
        );
        assert_eq!(
            max_key_lifetime(&grants(&["keys.own.read on service:portal"])),
            None
        );
    }
}
