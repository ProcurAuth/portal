//! The service `portal`: the actions of the features of the portal itself (portal 6).
//!
//! The portal is one internal scope with its own action document: the settings pages call it
//! "Server permissions". The code holds the document: no read of the catalogue replaces it, and the
//! PDP reads it from memory. It has two features. Each feature is in the document only when the
//! file turns it on.
//!
//! - **Keys** (the block `keys`): `keys.own.*`, `keys.all.*` and `users.read`. The actions
//!   `keys.own.create` and `keys.own.use` apply to the resource type `key_kind`. Its nodes
//!   `key_kind:mcp` and `key_kind:api` are in the code, and the file turns each kind on (`keys.mcp`,
//!   `keys.api`).
//! - **Inbox** (the block `inbox`): `inbox.receive`, `alert.send` and `alert.read` apply to the
//!   resource type `inbox_group`. The nodes are the groups of the block `inbox`. `alert.send_user`,
//!   `phone.read`, `phone.revoke` and `phone.log` apply to the service. The actions `alert.send`
//!   and `alert.send_user` list the budget `alerts`. `approval.read` and `approval.cancel` apply to
//!   the service: the first shows the metadata of the requests of all the accounts, the second
//!   cancels an open request. `question.ask`, `question.read` and `question.cancel` apply to the service
//!   too: the first lists the budget `questions`. No action reads a content, and no action decides. The value of a budget is on the grant `access`
//!   of the service, as for each other service. The PDP counts it as a use of the key or of the
//!   account. A group with `critical = true` never counts a use, so it never has a limit. An
//!   alert to an account is never critical: it always counts.
//!
//! The actions of `keys.own` need `keys.own.read` to show their page, and the actions of
//! `keys.all` need `users.read` (portal 6.3). The PDP treats an action as absent when the user
//! does not have the action that it needs (portal 6.3a). A key never holds a `keys.*` action. An
//! API key can hold `alert.send`, `alert.send_user` and `alert.read` (see [`KEY_ACTIONS`]).

#![allow(dead_code)]

use std::sync::Arc;

use crate::actions::{Document, Resource};
use crate::config::{InboxConfig, KeysConfig};
use crate::decision::{self, Matching, Question};
use crate::grants::{Grant, Segment};
use crate::keys::KeyKind;

/// The id of the service.
pub const SERVICE: &str = "portal";
/// The resource type of a group of the inbox.
pub const INBOX_GROUP: &str = "inbox_group";
/// The resource type of a kind of key.
pub const KEY_KIND: &str = "key_kind";

pub const KEYS_OWN_READ: &str = "keys.own.read";
pub const KEYS_OWN_CREATE: &str = "keys.own.create";
pub const KEYS_OWN_EDIT: &str = "keys.own.edit";
pub const KEYS_OWN_REVOKE: &str = "keys.own.revoke";
pub const KEYS_OWN_USE: &str = "keys.own.use";
pub const KEYS_ALL_READ: &str = "keys.all.read";
pub const KEYS_ALL_REVOKE: &str = "keys.all.revoke";
pub const USERS_READ: &str = "users.read";
pub const INBOX_RECEIVE: &str = "inbox.receive";
pub const ALERT_SEND: &str = "alert.send";
pub const ALERT_SEND_USER: &str = "alert.send_user";
pub const ALERT_READ: &str = "alert.read";
pub const PHONE_READ: &str = "phone.read";
pub const PHONE_REVOKE: &str = "phone.revoke";
pub const PHONE_LOG: &str = "phone.log";
pub const APPROVAL_READ: &str = "approval.read";
pub const APPROVAL_CANCEL: &str = "approval.cancel";
pub const QUESTION_ASK: &str = "question.ask";
pub const QUESTION_READ: &str = "question.read";
pub const QUESTION_CANCEL: &str = "question.cancel";

/// The name of the ceiling of `keys.own.use`: the longest lifetime of a key, in seconds.
pub const MAX_KEY_LIFETIME: &str = "max_key_lifetime";
/// The name of the budget of `alert.send`.
pub const ALERTS: &str = "alerts";
/// The name of the budget of `question.ask`. A question, a `say` and a `supersede` count one unit each. A renewal of a
/// lease counts nothing.
pub const QUESTIONS: &str = "questions";

/// The actions of the keys, in the order of the document.
const KEY_GROUP: [&str; 8] = [
    KEYS_OWN_READ,
    KEYS_OWN_CREATE,
    KEYS_OWN_EDIT,
    KEYS_OWN_REVOKE,
    KEYS_OWN_USE,
    KEYS_ALL_READ,
    KEYS_ALL_REVOKE,
    USERS_READ,
];

/// The actions of the inbox, in the order of the document.
const INBOX_GROUP_ACTIONS: [&str; 12] = [
    INBOX_RECEIVE,
    ALERT_SEND,
    ALERT_SEND_USER,
    ALERT_READ,
    PHONE_READ,
    PHONE_REVOKE,
    PHONE_LOG,
    APPROVAL_READ,
    APPROVAL_CANCEL,
    QUESTION_ASK,
    QUESTION_READ,
    QUESTION_CANCEL,
];

/// The actions of the service `portal` that a key can hold, other than `access`. A key never holds
/// a `keys.*` action (portal 6.5). The kind of the key limits the list (see [`key_actions`]).
/// `question.read` is not in the list: a key reads its own questions with no permission, and the
/// action gives an account the metadata of the questions of all the accounts.
pub const KEY_ACTIONS: [&str; 5] = [
    ALERT_SEND,
    ALERT_READ,
    ALERT_SEND_USER,
    QUESTION_ASK,
    QUESTION_CANCEL,
];

/// The actions that a key of this kind can hold. An MCP key holds the two actions of the
/// questions. An API key holds them and the actions of the alerts.
pub fn key_actions(kind: KeyKind) -> &'static [&'static str] {
    match kind {
        KeyKind::Api => &KEY_ACTIONS,
        KeyKind::Mcp => &[QUESTION_ASK, QUESTION_CANCEL],
    }
}

/// What the file turns on. The document of the service follows it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Features {
    /// The groups of the block `inbox`, each with its id and its label. `None`: the file has no
    /// block `inbox`.
    pub inbox_groups: Option<Vec<(String, String)>>,
    /// The kinds of key that the file turns on. Empty: the file has no block `keys`.
    pub kinds: Vec<KeyKind>,
}

impl Features {
    pub fn new(inbox: Option<&InboxConfig>, keys: Option<&KeysConfig>) -> Features {
        Features {
            inbox_groups: inbox.map(|inbox| {
                inbox
                    .groups
                    .iter()
                    .map(|(id, group)| (id.clone(), label_text(&group.name)))
                    .collect()
            }),
            kinds: keys.map(KeysConfig::kinds).unwrap_or_default(),
        }
    }

    /// The file has the block `keys`, with one kind on at least.
    pub fn has_keys(&self) -> bool {
        !self.kinds.is_empty()
    }

    pub fn has_inbox(&self) -> bool {
        self.inbox_groups.is_some()
    }

    /// Whether the document of these features holds this action.
    pub fn has_action(&self, action: &str) -> bool {
        (self.has_keys() && KEY_GROUP.contains(&action))
            || (self.has_inbox() && INBOX_GROUP_ACTIONS.contains(&action))
    }
}

/// The label of a group: the name, on one line.
fn label_text(name: &str) -> String {
    name.chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect::<String>()
        .trim()
        .to_string()
}

fn action_json(name: &str) -> serde_json::Value {
    let (types, description, limits): (&[&str], &str, &[&str]) = match name {
        KEYS_OWN_READ => (&["service"], "See the keys of oneself.", &[]),
        KEYS_OWN_CREATE => (&[KEY_KIND], "Make a key of oneself, of this kind.", &[]),
        KEYS_OWN_EDIT => (
            &["service"],
            "Change the name, the grants and the limit values of a key of oneself.",
            &[],
        ),
        KEYS_OWN_REVOKE => (&["service"], "Revoke a key of oneself.", &[]),
        KEYS_OWN_USE => (
            &[KEY_KIND],
            "Use the keys of oneself of this kind.",
            &[MAX_KEY_LIFETIME],
        ),
        KEYS_ALL_READ => (&["service"], "See the keys of all the users.", &[]),
        KEYS_ALL_REVOKE => (&["service"], "Revoke a key of any user.", &[]),
        USERS_READ => (&["service"], "See the users and their grants.", &[]),
        INBOX_RECEIVE => (
            &[INBOX_GROUP],
            "Subscribe the phones of the account to the group, to get its messages.",
            &[],
        ),
        ALERT_SEND => (&[INBOX_GROUP], "Send an alert to the group.", &[ALERTS]),
        ALERT_SEND_USER => (
            &["service"],
            "Send an alert to any account. The alert goes to each phone of the account.",
            &[ALERTS],
        ),
        ALERT_READ => (
            &[INBOX_GROUP],
            "See the alerts of the group and their delivery.",
            &[],
        ),
        PHONE_READ => (&["service"], "See all the phones and their state.", &[]),
        PHONE_REVOKE => (
            &["service"],
            "Revoke a phone. The phone gets no message after this.",
            &[],
        ),
        PHONE_LOG => (&["service"], "See the connection log of the phones.", &[]),
        APPROVAL_READ => (
            &["service"],
            "See the requests of all the accounts: state, key, service and times. The content stays hidden.",
            &[],
        ),
        APPROVAL_CANCEL => (&["service"], "Cancel an open request of any account.", &[]),
        QUESTION_ASK => (
            &["service"],
            "Ask a question to the owner of the key. A question, a message in its thread and a replacement count one unit of the budget.",
            &[QUESTIONS],
        ),
        QUESTION_READ => (
            &["service"],
            "See the questions of all the accounts: state, key and times. The content stays hidden.",
            &[],
        ),
        QUESTION_CANCEL => (
            &["service"],
            "Withdraw an open question. A key withdraws its own questions. An account with this action and `question.read` cancels the open question of any account.",
            &[],
        ),
        other => unreachable!("the action {other} is not an action of the service portal"),
    };
    serde_json::json!({ "name": name, "resource_types": types, "description": description,
                        "limits": limits })
}

/// The document of the service `portal` for these features: the fixed part, then one resource for
/// each group of the inbox and each kind of key that is on.
pub fn document(features: &Features) -> Arc<Document> {
    let mut types = Vec::new();
    let mut actions = Vec::new();
    let mut limits = Vec::new();
    if features.has_keys() {
        types.push(serde_json::json!({ "name": KEY_KIND, "parent": "service",
            "description": "A kind of key: MCP for the agents, API for the services." }));
        actions.extend(KEY_GROUP.iter().map(|a| action_json(a)));
        limits.push(
            serde_json::json!({ "name": MAX_KEY_LIFETIME, "label": "Longest key lifetime",
            "description": "The longest lifetime of a key of this kind, in seconds.",
            "unit": "seconds", "kind": "ceiling", "min": 0, "max": 31_536_000 }),
        );
    }
    if features.has_inbox() {
        types.push(
            serde_json::json!({ "name": INBOX_GROUP, "parent": "service",
            "description": "A group of recipients of the inbox." }),
        );
        actions.extend(INBOX_GROUP_ACTIONS.iter().map(|a| action_json(a)));
        limits.push(serde_json::json!({ "name": ALERTS, "label": "Alerts sent",
            "description": "The alerts that the subject sends in a time window. A critical group has no limit. An alert to an account always counts.",
            "unit": "alert", "kind": "budget" }));
        limits.push(serde_json::json!({ "name": QUESTIONS, "label": "Questions asked",
            "description": "The questions, the messages in a thread and the replacements that the subject sends in a time window. A renewal of a lease does not count.",
            "unit": "question", "kind": "budget" }));
    }
    let text = serde_json::json!({ "server": SERVICE, "resource_types": types,
                                   "actions": actions, "limits": limits });
    let mut document = crate::actions::parse(SERVICE, text.to_string().as_bytes())
        // The text is built from constants of the code, and a test parses it.
        .unwrap_or_else(|e| panic!("the document of the service portal is bad: {e}"));
    let mut resources = Vec::new();
    for kind in &features.kinds {
        if let Ok(segment) = Segment::new(KEY_KIND, kind.name()) {
            resources.push(Resource {
                path: vec![segment],
                label: format!("{} keys", kind.label()),
                actions: Vec::new(),
            });
        }
    }
    for (id, label) in features.inbox_groups.iter().flatten() {
        if let Ok(segment) = Segment::new(INBOX_GROUP, id) {
            resources.push(Resource {
                path: vec![segment],
                label: label.clone(),
                // An empty list means: each action that lists the type `inbox_group`.
                actions: Vec::new(),
            });
        }
    }
    document.resources = resources;
    Arc::new(document)
}

/// The document of the features for the form of a key of this kind: only the actions that the kind
/// of key can hold, with their types, their nodes and their limits. `None`: the kind holds no
/// action, so the form has no part for the features.
pub fn key_document(features: &Features, kind: KeyKind) -> Option<Arc<Document>> {
    let allowed = key_actions(kind);
    let full = document(features);
    let actions: Vec<crate::actions::Action> = full
        .actions
        .iter()
        .filter(|a| allowed.contains(&a.name.as_str()))
        .cloned()
        .collect();
    if actions.is_empty() {
        return None;
    }
    let types_used: Vec<&str> = actions
        .iter()
        .flat_map(|a| a.sub_resource_types())
        .collect();
    let limits_used: Vec<&String> = actions.iter().flat_map(|a| a.limits.iter()).collect();
    let mut cut = (*full).clone();
    cut.types.retain(|t| types_used.contains(&t.name.as_str()));
    cut.resources.retain(|r| {
        r.path
            .last()
            .is_some_and(|s| types_used.contains(&s.kind.as_str()))
    });
    cut.limits.retain(|l| limits_used.contains(&&l.name));
    cut.actions = actions;
    Some(Arc::new(cut))
}

/// The names of the actions of the document for these features, in the order of the document.
pub fn action_names(features: &Features) -> Vec<&'static str> {
    KEY_GROUP
        .iter()
        .chain(INBOX_GROUP_ACTIONS.iter())
        .copied()
        .filter(|a| features.has_action(a))
        .collect()
}

/// Whether the name is the name of an action of the service `portal`, with every feature on.
pub fn is_action(name: &str) -> bool {
    KEY_GROUP.contains(&name) || INBOX_GROUP_ACTIONS.contains(&name)
}

/// The actions that an action needs (portal 6.3).
pub fn needs(action: &str) -> &'static [&'static str] {
    match action {
        KEYS_OWN_CREATE | KEYS_OWN_EDIT | KEYS_OWN_REVOKE => &[KEYS_OWN_READ],
        KEYS_ALL_REVOKE => &[KEYS_ALL_READ, USERS_READ],
        KEYS_ALL_READ => &[USERS_READ],
        PHONE_REVOKE => &[PHONE_READ],
        APPROVAL_CANCEL => &[APPROVAL_READ],
        _ => &[],
    }
}

/// Whether the grants hold the action on the node `service:portal`.
pub fn holds(grants: &[Grant], action: &str) -> bool {
    grants
        .iter()
        .any(|g| g.service == SERVICE && g.path.is_empty() && g.action == action)
}

/// Whether the grants hold the action on the node `service:portal/key_kind:<kind>`.
pub fn holds_kind(grants: &[Grant], action: &str, kind: KeyKind) -> bool {
    grants.iter().any(|g| {
        g.service == SERVICE
            && g.action == action
            && matches!(g.path.as_slice(), [s] if s.kind == KEY_KIND && s.id == kind.name())
    })
}

/// Whether the action counts for these grants: the grants hold it on the service, and hold each
/// action that it needs (portal 6.3a).
pub fn effective(grants: &[Grant], action: &str) -> bool {
    holds(grants, action) && needs(action).iter().all(|need| holds(grants, need))
}

/// The same for an action on a kind of key: the grants hold it on the node of the kind, and hold
/// each action that it needs on the service.
pub fn effective_kind(grants: &[Grant], action: &str, kind: KeyKind) -> bool {
    holds_kind(grants, action, kind) && needs(action).iter().all(|need| holds(grants, need))
}

/// The kinds of key for which the action counts.
pub fn kinds_with(grants: &[Grant], action: &str) -> Vec<KeyKind> {
    KeyKind::ALL
        .into_iter()
        .filter(|kind| effective_kind(grants, action, *kind))
        .collect()
}

/// The longest lifetime of a key of this kind that the grants give, in seconds: the lowest value of
/// the ceiling `max_key_lifetime` on the grants of `keys.own.use` on the kind. `None`: no value.
pub fn max_key_lifetime(grants: &[Grant], kind: KeyKind) -> Option<u64> {
    grants
        .iter()
        .filter(|g| g.service == SERVICE && g.action == KEYS_OWN_USE)
        .filter(|g| matches!(g.path.as_slice(), [s] if s.kind == KEY_KIND && s.id == kind.name()))
        .filter_map(|g| match g.limits.get(MAX_KEY_LIFETIME) {
            Some(crate::limits::Value::Ceiling(n)) => Some(*n),
            _ => None,
        })
        .min()
}

/// The longest lifetime among the kinds of key, for a count that has no kind: `None` when one kind
/// has no value, so the count never leaves out a key that is still valid.
pub fn max_key_lifetime_any(grants: &[Grant]) -> Option<u64> {
    let mut longest = 0;
    for kind in KeyKind::ALL {
        longest = longest.max(max_key_lifetime(grants, kind)?);
    }
    Some(longest)
}

/// Checks the grants of one user of the file or of the database against the rules of the service
/// `portal` (portal 6.3, 6.3a): each grant names an action of the document, is on a node where the
/// action applies, and each action has the actions that it needs. The grants of other services are
/// not checked.
pub fn check_user_grants(features: &Features, grants: &[Grant]) -> Result<(), String> {
    let document = document(features);
    for grant in grants.iter().filter(|g| g.service == SERVICE) {
        if grant.action == crate::grants::ACCESS {
            if !grant.path.is_empty() {
                return Err(format!(
                    "the grant {grant} is below the service portal: the action access is on service:portal"
                ));
            }
            continue;
        }
        let Some(action) = document.actions.iter().find(|a| a.name == grant.action) else {
            let names: Vec<&str> = document.actions.iter().map(|a| a.name.as_str()).collect();
            return Err(format!(
                "the service portal has no action {:?}: the actions are {}",
                grant.action,
                names.join(", ")
            ));
        };
        if grant.path.is_empty() {
            if !action.resource_types.iter().any(|t| t == "service") {
                return Err(format!(
                    "the action {} applies to a node: grant it on {}",
                    grant.action,
                    action
                        .sub_resource_types()
                        .map(|t| format!("service:portal/{t}:<id>"))
                        .collect::<Vec<_>>()
                        .join(" or ")
                ));
            }
        } else {
            document
                .check_grant(grant)
                .map_err(|e| format!("the grant {grant}: {e}"))?;
            if !document.resources.iter().any(|r| r.path == grant.path) {
                let nodes: Vec<String> = document
                    .resources
                    .iter()
                    .filter(|r| {
                        r.path.last().map(|s| s.kind.as_str())
                            == grant.path.last().map(|s| s.kind.as_str())
                    })
                    .map(|r| crate::grants::path_text(&r.path))
                    .collect();
                return Err(format!(
                    "the grant {grant} is on a node that does not exist: the nodes are {}",
                    nodes.join(", ")
                ));
            }
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

/// Checks the grants of a key of this kind (portal 6.5): a key holds no action of the service
/// `portal`, except the actions that its kind lists (see [`key_actions`]).
pub fn check_key_grants(kind: KeyKind, grants: &[Grant]) -> Result<(), String> {
    match grants.iter().find(|g| {
        g.service == SERVICE
            && g.action != crate::grants::ACCESS
            && !key_actions(kind).contains(&g.action.as_str())
    }) {
        Some(grant) => Err(format!(
            "the grant {grant} is an action of the service portal that the kind {} cannot hold",
            kind.name()
        )),
        None => Ok(()),
    }
}

// The questions of the inbox ------------------------------------------------------------------

/// The chain of the node `inbox_group:<id>`.
pub fn group_chain(group: &str) -> Option<Vec<Segment>> {
    Segment::new(INBOX_GROUP, group).ok().map(|s| vec![s])
}

/// Whether the grants of an account on the service allow `action` on the node. `group` is `None`
/// for the service itself. The superadmin has no grants: the caller answers for it.
pub fn allows(
    document: &Document,
    login: &str,
    grants: &[Grant],
    action: &str,
    group: Option<&str>,
) -> bool {
    let chain = match group {
        Some(id) => match group_chain(id) {
            Some(chain) => chain,
            None => return false,
        },
        None => Vec::new(),
    };
    let question = Question {
        service: SERVICE,
        action,
        chain: &chain,
        matching: Matching::Node,
        document: Some(document),
    };
    decision::person(login, grants, &question).allowed
}

/// The ids of the groups for which the grants allow `action`.
pub fn groups_with(
    document: &Document,
    login: &str,
    grants: &[Grant],
    action: &str,
    groups: impl Iterator<Item = String>,
) -> Vec<String> {
    groups
        .filter(|id| allows(document, login, grants, action, Some(id)))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;

    fn grants(texts: &[&str]) -> Vec<Grant> {
        texts.iter().map(|t| Grant::parse(t).unwrap()).collect()
    }

    fn inbox_config() -> InboxConfig {
        let raw = r#"
            server { listen_address = "0.0.0.0:4181" trusted_proxies = [] }
            authentication { superadmin_login_env = "L" superadmin_password_env = "P" mfa = "off" }
            inbox {
              server_key_env = "K"
              devices {
                public_address = "inbox.example.org:8080"
                heartbeat_interval = "20s" session_timeout = "60s" resend_after = "15s"
                max_inflight = 32 unreachable_after = "2m" offline_warning_after = "3m"
              }
              storage { delivery_ttl = "7d" alert_retention = "30d" connection_log_retention = "90d" }
              group "infra" { name = "Infra" }
              group "oncall" { name = "On call" critical = true }
            }"#;
        let server_key = inbox_core::server_key::ServerKey::generate().unwrap();
        let key_text = server_key.encode().to_string();
        let (config, _) = Config::parse(raw, &|name| match name {
            "L" => Some("root".into()),
            "P" => Some("a password for the tests".into()),
            "K" => Some(key_text.clone()),
            _ => None,
        })
        .unwrap();
        config.inbox.unwrap()
    }

    fn all_features() -> Features {
        Features::new(
            Some(&inbox_config()),
            Some(&KeysConfig {
                default_lifetime: std::time::Duration::from_secs(86_400),
                directory_max_age: std::time::Duration::from_secs(30),
                max_per_user: 100,
                max_counter_rows: 1000,
                max_directory_grants: 1000,
                history_days: 90,
                mcp: true,
                api: true,
            }),
        )
    }

    fn find<'a>(document: &'a Document, name: &str) -> &'a crate::actions::Action {
        document.actions.iter().find(|a| a.name == name).unwrap()
    }

    #[test]
    fn a_key_never_holds_an_approval_action() {
        let read = Grant::parse("approval.read on service:portal").unwrap();
        for kind in [KeyKind::Mcp, KeyKind::Api] {
            assert!(check_key_grants(kind, std::slice::from_ref(&read)).is_err());
        }
        assert!(!KEY_ACTIONS.contains(&APPROVAL_READ) && !KEY_ACTIONS.contains(&APPROVAL_CANCEL));
    }

    #[test]
    fn the_document_has_the_keys_and_the_inbox_in_one_scope() {
        let document = document(&all_features());
        let names: Vec<&str> = document.actions.iter().map(|a| a.name.as_str()).collect();
        assert_eq!(names, action_names(&all_features()));
        assert_eq!(names.len(), 20);
        assert!(names.contains(&APPROVAL_READ) && names.contains(&APPROVAL_CANCEL));
        // The questions: three actions on the service, and the budget `questions` on the first.
        for name in [QUESTION_ASK, QUESTION_READ, QUESTION_CANCEL] {
            assert!(names.contains(&name), "{name}");
            assert_eq!(find(&document, name).resource_types, ["service"], "{name}");
        }
        assert_eq!(find(&document, QUESTION_ASK).limits, [QUESTIONS]);
        assert!(find(&document, QUESTION_READ).limits.is_empty());
        assert_eq!(
            document
                .limits
                .iter()
                .find(|l| l.name == QUESTIONS)
                .map(|l| l.kind),
            Some(crate::limits::Kind::Budget)
        );
        // No action reads a content, and no action decides.
        assert!(!names
            .iter()
            .any(|n| n.contains("decide") || n.contains("read_body")));
        assert_eq!(needs(APPROVAL_CANCEL), [APPROVAL_READ]);
        let types: Vec<&str> = document.types.iter().map(|t| t.name.as_str()).collect();
        assert_eq!(types, [KEY_KIND, INBOX_GROUP]);
        assert!(document.types.iter().all(|t| t.parent == "service"));
        for name in [INBOX_RECEIVE, ALERT_SEND, ALERT_READ] {
            assert_eq!(
                find(&document, name).resource_types,
                [INBOX_GROUP],
                "{name}"
            );
        }
        for name in [KEYS_OWN_CREATE, KEYS_OWN_USE] {
            assert_eq!(find(&document, name).resource_types, [KEY_KIND], "{name}");
        }
        for name in [
            KEYS_OWN_READ,
            KEYS_OWN_EDIT,
            KEYS_OWN_REVOKE,
            KEYS_ALL_READ,
            KEYS_ALL_REVOKE,
            USERS_READ,
            ALERT_SEND_USER,
            PHONE_READ,
            PHONE_REVOKE,
            PHONE_LOG,
        ] {
            assert_eq!(find(&document, name).resource_types, ["service"], "{name}");
        }
        assert_eq!(find(&document, ALERT_SEND).limits, [ALERTS]);
        assert_eq!(
            find(&document, ALERT_SEND_USER).limits,
            [ALERTS],
            "the same budget counts the alerts to a group and to an account"
        );
        assert_eq!(find(&document, KEYS_OWN_USE).limits, [MAX_KEY_LIFETIME]);
        let limit = |name: &str| document.limits.iter().find(|l| l.name == name).unwrap();
        assert_eq!(limit(ALERTS).kind, crate::limits::Kind::Budget);
        assert_eq!(limit(MAX_KEY_LIFETIME).range, Some((0, 31_536_000)));
        // The convention of the units: a ceiling has a plural noun, a budget has a singular noun.
        assert_eq!(limit(MAX_KEY_LIFETIME).unit, "seconds");
        assert_eq!(limit(ALERTS).unit, "alert");
        assert_eq!(limit(QUESTIONS).unit, "question");
        let labels: Vec<(String, String)> = document
            .resources
            .iter()
            .map(|r| (crate::grants::path_text(&r.path), r.label.clone()))
            .collect();
        assert_eq!(
            labels,
            [
                ("key_kind:mcp".to_string(), "MCP keys".to_string()),
                ("key_kind:api".to_string(), "API keys".to_string()),
                ("inbox_group:infra".to_string(), "Infra".to_string()),
                ("inbox_group:oncall".to_string(), "On call".to_string()),
            ]
        );
    }

    #[test]
    fn a_feature_that_the_file_turns_off_leaves_the_document() {
        let none = document(&Features::default());
        assert!(none.actions.is_empty() && none.types.is_empty() && none.limits.is_empty());
        assert!(none.resources.is_empty());
        let only_keys = Features {
            inbox_groups: None,
            kinds: vec![KeyKind::Api],
        };
        let document_keys = document(&only_keys);
        assert!(document_keys
            .actions
            .iter()
            .all(|a| !INBOX_GROUP_ACTIONS.contains(&a.name.as_str())));
        let paths: Vec<String> = document_keys
            .resources
            .iter()
            .map(|r| crate::grants::path_text(&r.path))
            .collect();
        assert_eq!(paths, ["key_kind:api"], "only the kinds that are on");
        let only_inbox = Features {
            inbox_groups: Some(vec![("infra".into(), "Infra".into())]),
            kinds: Vec::new(),
        };
        let document_inbox = document(&only_inbox);
        assert!(document_inbox
            .actions
            .iter()
            .all(|a| !KEY_GROUP.contains(&a.name.as_str())));
        assert!(document_inbox
            .limit_inputs(crate::grants::ACCESS)
            .iter()
            .any(|i| i.budget));
    }

    #[test]
    fn the_needs_rules_hold_for_the_keys_and_the_phones() {
        assert_eq!(needs(KEYS_OWN_CREATE), [KEYS_OWN_READ]);
        assert_eq!(needs(KEYS_OWN_EDIT), [KEYS_OWN_READ]);
        assert_eq!(needs(KEYS_OWN_REVOKE), [KEYS_OWN_READ]);
        assert_eq!(needs(KEYS_ALL_REVOKE), [KEYS_ALL_READ, USERS_READ]);
        assert_eq!(needs(KEYS_ALL_READ), [USERS_READ]);
        assert_eq!(needs(PHONE_REVOKE), [PHONE_READ]);
        for free in [
            KEYS_OWN_USE,
            INBOX_RECEIVE,
            ALERT_SEND,
            ALERT_SEND_USER,
            ALERT_READ,
            PHONE_READ,
            PHONE_LOG,
        ] {
            assert!(needs(free).is_empty(), "{free}");
        }
        let all = grants(&[
            "keys.own.read on service:portal",
            "keys.own.create on service:portal/key_kind:mcp",
            "keys.all.revoke on service:portal",
        ]);
        assert!(effective(&all, KEYS_OWN_READ));
        assert!(effective_kind(&all, KEYS_OWN_CREATE, KeyKind::Mcp));
        assert!(
            !effective_kind(&all, KEYS_OWN_CREATE, KeyKind::Api),
            "no grant on the api node"
        );
        assert!(
            !effective(&all, KEYS_ALL_REVOKE),
            "it needs keys.all.read and users.read"
        );
        assert!(!effective(&all, KEYS_OWN_EDIT), "the user does not hold it");
        let alone = grants(&["keys.own.create on service:portal/key_kind:mcp"]);
        assert!(holds_kind(&alone, KEYS_OWN_CREATE, KeyKind::Mcp));
        assert!(
            !effective_kind(&alone, KEYS_OWN_CREATE, KeyKind::Mcp),
            "it needs keys.own.read"
        );
        assert_eq!(
            kinds_with(&all, KEYS_OWN_CREATE),
            [KeyKind::Mcp],
            "the kinds that the grants allow"
        );
        let phones = grants(&["phone.revoke on service:portal"]);
        assert!(!effective(&phones, PHONE_REVOKE));
        assert!(effective(
            &grants(&[
                "phone.read on service:portal",
                "phone.revoke on service:portal"
            ]),
            PHONE_REVOKE
        ));
    }

    #[test]
    fn the_grants_of_a_user_follow_the_document() {
        let features = all_features();
        assert!(check_user_grants(&features, &grants(&["access on service:db"])).is_ok());
        assert!(check_user_grants(
            &features,
            &grants(&[
                "keys.own.read on service:portal",
                "keys.own.edit on service:portal",
                "keys.own.create on service:portal/key_kind:mcp",
                "keys.own.use on service:portal/key_kind:api",
                "alert.send on service:portal/inbox_group:infra",
                "inbox.receive on service:portal/inbox_group:oncall",
                "phone.log on service:portal",
                "access on service:portal",
            ])
        )
        .is_ok());
        for bad in [
            // The action needs another action.
            vec!["keys.own.edit on service:portal"],
            vec!["keys.all.read on service:portal"],
            vec![
                "keys.all.revoke on service:portal",
                "keys.all.read on service:portal",
            ],
            vec!["phone.revoke on service:portal"],
            vec!["keys.own.create on service:portal/key_kind:mcp"],
            // An unknown action.
            vec!["keys.own.nothing on service:portal"],
            // An action of a node on the service, and an action of the service on a node.
            vec!["keys.own.use on service:portal"],
            vec!["alert.send on service:portal"],
            vec!["keys.own.read on service:portal/key_kind:mcp"],
            vec!["phone.read on service:portal/inbox_group:infra"],
            // A node that does not exist, a wrong type, a path below the node.
            vec!["alert.send on service:portal/inbox_group:missing"],
            vec!["keys.own.use on service:portal/key_kind:whk"],
            vec!["alert.send on service:portal/key_kind:api"],
            vec!["alert.send on service:portal/inbox_group:infra/host:x"],
        ] {
            assert!(
                check_user_grants(&features, &grants(&bad)).is_err(),
                "{bad:?}"
            );
        }
        // A grant of another service with the same action name is not a grant of the portal.
        assert!(check_user_grants(&features, &grants(&["keys.own.edit on service:other"])).is_ok());
    }

    #[test]
    fn a_grant_of_a_feature_that_is_off_stops_the_check() {
        let keys_only = Features {
            inbox_groups: None,
            kinds: vec![KeyKind::Mcp],
        };
        let error = check_user_grants(
            &keys_only,
            &grants(&["alert.send on service:portal/inbox_group:infra"]),
        )
        .unwrap_err();
        assert!(error.contains("no action \"alert.send\""), "{error}");
        let error = check_user_grants(
            &keys_only,
            &grants(&["keys.own.use on service:portal/key_kind:api"]),
        )
        .unwrap_err();
        assert!(
            error.contains("does not exist") && error.contains("key_kind:mcp"),
            "{error}"
        );
        let nothing = Features::default();
        assert!(
            check_user_grants(&nothing, &grants(&["keys.own.read on service:portal"])).is_err()
        );
        assert!(check_user_grants(&nothing, &grants(&["access on service:portal"])).is_ok());
    }

    #[test]
    fn the_actions_that_a_key_can_hold_follow_its_kind() {
        assert_eq!(
            KEY_ACTIONS,
            [
                ALERT_SEND,
                ALERT_READ,
                ALERT_SEND_USER,
                QUESTION_ASK,
                QUESTION_CANCEL
            ]
        );
        assert_eq!(key_actions(KeyKind::Api), KEY_ACTIONS);
        // An MCP key holds the two actions of the questions and no action of the alerts. `question.read` is the
        // right of an account: a key reads its own questions with no permission.
        assert_eq!(key_actions(KeyKind::Mcp), [QUESTION_ASK, QUESTION_CANCEL]);
        assert!(!KEY_ACTIONS.contains(&QUESTION_READ));
        let good = grants(&[
            "access on service:db",
            "data.read on service:db/host:x",
            "access on service:portal",
            "alert.send on service:portal/inbox_group:infra",
        ]);
        assert!(check_key_grants(KeyKind::Api, &good).is_ok());
        assert!(
            check_key_grants(KeyKind::Mcp, &good).is_err(),
            "no alert action for an MCP key"
        );
        assert!(check_key_grants(KeyKind::Mcp, &good[..3]).is_ok());
        for refused in [
            "keys.own.read on service:portal",
            "inbox.receive on service:portal/inbox_group:infra",
            "phone.read on service:portal",
        ] {
            assert!(
                check_key_grants(KeyKind::Api, &grants(&[refused])).is_err(),
                "{refused}"
            );
        }
    }

    #[test]
    fn the_action_to_alert_an_account_is_flat_on_the_service() {
        let features = all_features();
        let document = document(&features);
        let held = grants(&["alert.send_user on service:portal"]);
        assert!(check_user_grants(&features, &held).is_ok());
        assert!(allows(&document, "bob", &held, ALERT_SEND_USER, None));
        assert!(
            !allows(&document, "bob", &held, ALERT_SEND, Some("infra")),
            "the right to alert an account is not the right to alert a group"
        );
        let group_only = grants(&["alert.send on service:portal/inbox_group:infra"]);
        assert!(!allows(
            &document,
            "bob",
            &group_only,
            ALERT_SEND_USER,
            None
        ));
        // The action has no node: a grant on a group is a bad grant.
        let error = check_user_grants(
            &features,
            &grants(&["alert.send_user on service:portal/inbox_group:infra"]),
        )
        .unwrap_err();
        assert!(error.contains("alert.send_user"), "{error}");
        // A key can hold it. A file without the block `inbox` has no such action.
        assert!(check_key_grants(KeyKind::Api, &held).is_ok());
        assert!(check_key_grants(KeyKind::Mcp, &held).is_err());
        assert!(check_user_grants(&Features::default(), &held).is_err());
        assert!(features.has_action(ALERT_SEND_USER));
        assert!(is_action(ALERT_SEND_USER));
    }

    #[test]
    fn the_document_of_a_key_holds_only_what_the_kind_can_use() {
        let features = all_features();
        // An MCP key holds the two actions of the questions and the budget of the questions.
        let mcp = key_document(&features, KeyKind::Mcp).unwrap();
        let names: Vec<&str> = mcp.actions.iter().map(|a| a.name.as_str()).collect();
        assert_eq!(names, [QUESTION_ASK, QUESTION_CANCEL]);
        assert_eq!(mcp.limits.len(), 1);
        assert_eq!(mcp.limits[0].name, QUESTIONS);
        let api = key_document(&features, KeyKind::Api).unwrap();
        let names: Vec<&str> = api.actions.iter().map(|a| a.name.as_str()).collect();
        assert_eq!(
            names,
            [
                ALERT_SEND,
                ALERT_SEND_USER,
                ALERT_READ,
                QUESTION_ASK,
                QUESTION_CANCEL
            ]
        );
        assert_eq!(api.types.len(), 1);
        assert_eq!(api.types[0].name, INBOX_GROUP);
        assert!(api.resources.iter().all(|r| r.path[0].kind == INBOX_GROUP));
        assert_eq!(api.limits.len(), 2);
        assert!(api.limit_inputs(crate::grants::ACCESS)[0].budget);
        assert!(
            key_document(&Features::default(), KeyKind::Api).is_none(),
            "no inbox, no action that an API key can hold"
        );
    }

    #[test]
    fn the_lowest_longest_lifetime_of_each_kind_applies() {
        use crate::limits::Limits;
        let with = |kind: &str, value: &str| {
            Grant::parse(&format!("keys.own.use on service:portal/key_kind:{kind}"))
                .unwrap()
                .with_limits(Limits::from_json(value).unwrap())
        };
        assert_eq!(max_key_lifetime(&[with("mcp", "{}")], KeyKind::Mcp), None);
        assert_eq!(
            max_key_lifetime(
                &[with("mcp", r#"{"max_key_lifetime":2592000}"#)],
                KeyKind::Mcp
            ),
            Some(2_592_000)
        );
        assert_eq!(
            max_key_lifetime(
                &[with("mcp", r#"{"max_key_lifetime":2592000}"#)],
                KeyKind::Api
            ),
            None,
            "the value of one kind is not the value of the other"
        );
        assert_eq!(
            max_key_lifetime(
                &[
                    with("api", r#"{"max_key_lifetime":100}"#),
                    with("api", r#"{"max_key_lifetime":7}"#)
                ],
                KeyKind::Api
            ),
            Some(7)
        );
        assert_eq!(
            max_key_lifetime(&[with("api", r#"{"max_key_lifetime":0}"#)], KeyKind::Api),
            Some(0)
        );
        assert_eq!(
            max_key_lifetime(&grants(&["keys.own.read on service:portal"]), KeyKind::Mcp),
            None
        );
        // For a count that has no kind, the longest value counts: one kind with no value gives none.
        let both = [
            with("mcp", r#"{"max_key_lifetime":10}"#),
            with("api", r#"{"max_key_lifetime":50}"#),
        ];
        assert_eq!(max_key_lifetime_any(&both), Some(50));
        assert_eq!(max_key_lifetime_any(&both[..1]), None);
    }

    fn key_question<'a>(
        document: &'a Document,
        chain: &'a [Segment],
        action: &'a str,
    ) -> Question<'a> {
        Question {
            service: SERVICE,
            action,
            chain,
            matching: Matching::Node,
            document: Some(document),
        }
    }

    #[test]
    fn a_grant_on_a_group_allows_that_group_only() {
        let document = document(&all_features());
        let held = grants(&["alert.send on service:portal/inbox_group:infra"]);
        assert!(allows(&document, "bob", &held, ALERT_SEND, Some("infra")));
        assert!(!allows(&document, "bob", &held, ALERT_SEND, Some("oncall")));
        assert!(!allows(&document, "bob", &held, ALERT_READ, Some("infra")));
        assert!(!allows(
            &document,
            "bob",
            &held,
            ALERT_SEND,
            Some("missing")
        ));
        assert!(!allows(&document, "bob", &held, PHONE_READ, None));
        let service = grants(&["phone.read on service:portal"]);
        assert!(allows(&document, "bob", &service, PHONE_READ, None));
        assert!(!allows(&document, "bob", &service, PHONE_REVOKE, None));
        assert_eq!(
            groups_with(
                &document,
                "bob",
                &grants(&["inbox.receive on service:portal/inbox_group:oncall"]),
                INBOX_RECEIVE,
                ["infra".to_string(), "oncall".to_string()].into_iter()
            ),
            ["oncall"]
        );
    }

    #[test]
    fn a_grant_that_does_not_follow_the_document_is_refused_by_the_document() {
        let document = document(&all_features());
        assert!(document
            .check_grant(&Grant::parse("alert.send on service:portal/inbox_group:infra").unwrap())
            .is_ok());
        assert!(document
            .check_grant(&Grant::parse("phone.read on service:portal/inbox_group:infra").unwrap())
            .is_err());
        assert!(document
            .check_grant(&Grant::parse("alert.send on service:portal/host:infra").unwrap())
            .is_err());
        assert!(document
            .check_grant(&Grant::parse("keys.own.use on service:portal/key_kind:api").unwrap())
            .is_ok());
    }

    #[test]
    fn a_key_needs_the_permission_on_its_own_side_and_on_the_side_of_its_owner() {
        use crate::decision::{key, CounterOwner, Owner};
        let document = document(&all_features());
        let chain = group_chain("infra").unwrap();
        let q = key_question(&document, &chain, ALERT_SEND);
        let send = grants(&["alert.send on service:portal/inbox_group:infra"]);
        let none = grants(&[]);
        let owner = CounterOwner::User("owner".into());
        let keyed = CounterOwner::Key("k".into());
        let decide = |key_grants: &[Grant], owner_grants: &[Grant]| {
            key(&keyed, key_grants, Owner::Person(owner_grants), &owner, &q).allowed
        };
        assert!(decide(&send, &send), "both sides allow");
        assert!(
            !decide(&send, &none),
            "the owner has no permission: the key loses it at once"
        );
        assert!(!decide(&none, &send), "the key has no permission");
        let other = group_chain("oncall").unwrap();
        let other_q = key_question(&document, &other, ALERT_SEND);
        assert!(!key(&keyed, &send, Owner::Superadmin, &owner, &other_q).allowed);
        assert!(
            key(&keyed, &send, Owner::Superadmin, &owner, &q).allowed,
            "the superadmin as the owner has every permission"
        );
    }

    #[test]
    fn the_budget_alerts_counts_on_access_on_the_portal_and_a_critical_group_never_counts() {
        use crate::decision::{key, CounterOwner, Owner};
        use crate::limits::Limits;
        let document = document(&all_features());
        let chain = group_chain("infra").unwrap();
        let q = key_question(&document, &chain, ALERT_SEND);
        let budget = Limits::from_json(
            r#"{"alerts":[{"amount":5,"window":"minute"},{"amount":50,"window":"hour"}]}"#,
        )
        .unwrap();
        let owner_grants = vec![
            Grant::parse("access on service:portal")
                .unwrap()
                .with_limits(budget),
            Grant::parse("alert.send on service:portal/inbox_group:infra").unwrap(),
            Grant::parse("alert.send on service:portal/inbox_group:oncall").unwrap(),
        ];
        let key_grants = grants(&["alert.send on service:portal/inbox_group:infra"]);
        let verdict = key(
            &CounterOwner::Key("k".into()),
            &key_grants,
            Owner::Person(&owner_grants),
            &CounterOwner::User("owner".into()),
            &q,
        );
        assert!(verdict.allowed);
        assert_eq!(verdict.counters.len(), 2, "minute and hour, on the owner");
        assert!(verdict
            .counters
            .iter()
            .all(|c| c.id.budget == ALERTS && c.id.service == SERVICE));
        // Another action has no counter: the budget belongs to alert.send only.
        let read_q = key_question(&document, &chain, ALERT_READ);
        let read_grants = grants(&["alert.read on service:portal/inbox_group:infra"]);
        let owner_read = vec![
            owner_grants[0].clone(),
            Grant::parse("alert.read on service:portal/inbox_group:infra").unwrap(),
        ];
        let verdict = key(
            &CounterOwner::Key("k".into()),
            &read_grants,
            Owner::Person(&owner_read),
            &CounterOwner::User("owner".into()),
            &read_q,
        );
        assert!(verdict.allowed && verdict.counters.is_empty());
        // The portal asks for a use only for a group that is not critical (see `inbox/api.rs`):
        // the document of the critical group has no other rule, so the check is the group flag.
        let inbox = inbox_config();
        assert!(inbox.groups["oncall"].critical && !inbox.groups["infra"].critical);
    }

    #[test]
    fn a_budget_of_alerts_is_valid_on_access_and_a_ceiling_is_not() {
        use crate::limits::Limits;
        let document = document(&all_features());
        let budget = Limits::from_json(r#"{"alerts":[{"amount":5,"window":"minute"}]}"#).unwrap();
        assert!(budget.check(&document.limits, "access", &[]).is_ok());
        // The budget is not a value of the action alert.send.
        assert!(budget
            .check(&document.limits, ALERT_SEND, &[ALERTS.to_string()])
            .is_err());
        // Each budget of the service is a value of `access`: an action takes no number but a ceiling.
        let inputs = document.limit_inputs("access");
        assert_eq!(inputs.len(), 2);
        assert!(inputs[0].budget && inputs[0].name == ALERTS);
        assert!(inputs[1].budget && inputs[1].name == QUESTIONS);
        assert!(document.limit_inputs(ALERT_SEND).is_empty());
        assert_eq!(
            document.limit_inputs(KEYS_OWN_USE)[0].name,
            MAX_KEY_LIFETIME
        );
    }
}
