//! The service `inbox`: the rights on the alerts, the phones and the connection log.
//!
//! The inbox is the second internal service of the portal, after the service `portal`. Its action
//! document is in the code. The groups of the block `inbox` of the file are its resources: they are
//! static, so the document lists each group as the node `group:<id>`. The settings pages then show
//! the groups without a typed id.
//!
//! The resource type `group` is a child of the service. The actions `group.subscribe`,
//! `alert.send` and `alert.read` apply to a group. The actions `device.read`, `device.revoke` and
//! `connection.read` apply to the service. The action `alert.send` lists the budget `alerts`.
//! The value of a budget is on the grant `access` of the service, as for each other service. The
//! PDP counts it as a use of the key or of the account. A group with `critical = true` never
//! counts a use, so it never has a limit.

use std::sync::Arc;

use crate::actions::{Document, Resource};
use crate::config::InboxConfig;
use crate::decision::{self, Matching, Question};
use crate::grants::{Grant, Segment};

/// The id of the service.
pub const SERVICE: &str = "inbox";
/// The resource type of a group of alerts.
pub const GROUP: &str = "group";

pub const GROUP_SUBSCRIBE: &str = "group.subscribe";
pub const ALERT_SEND: &str = "alert.send";
pub const ALERT_READ: &str = "alert.read";
pub const DEVICE_READ: &str = "device.read";
pub const DEVICE_REVOKE: &str = "device.revoke";
pub const CONNECTION_READ: &str = "connection.read";

/// The actions of the service, in the order of the document.
#[cfg(test)]
const ACTIONS: [&str; 6] = [
    GROUP_SUBSCRIBE,
    ALERT_SEND,
    ALERT_READ,
    DEVICE_READ,
    DEVICE_REVOKE,
    CONNECTION_READ,
];

/// The document of the service, without the resources.
const DOCUMENT: &str = r#"{ "server": "inbox",
  "resource_types": [ { "name": "group", "parent": "service", "description": "A group of alerts." } ],
  "actions": [
    { "name": "group.subscribe", "resource_types": ["group"],
      "description": "Subscribe the phones of the account to the group, to get its alerts." },
    { "name": "alert.send", "resource_types": ["group"], "limits": ["alerts"],
      "description": "Send an alert to the group." },
    { "name": "alert.read", "resource_types": ["group"],
      "description": "See the alerts of the group and their delivery." },
    { "name": "device.read", "resource_types": ["service"],
      "description": "See all the phones and their state." },
    { "name": "device.revoke", "resource_types": ["service"],
      "description": "Revoke a phone. The phone gets no alert after this." },
    { "name": "connection.read", "resource_types": ["service"],
      "description": "See the connection log of the phones." } ],
  "limits": [ { "name": "alerts", "label": "Alerts sent",
                "description": "The alerts that the subject sends in a time window. A critical group has no limit.",
                "unit": "alert", "kind": "budget" } ] }"#;

/// The document of the service for these groups: the fixed part, then one resource for each group.
pub fn document(config: &InboxConfig) -> Arc<Document> {
    let mut document = crate::actions::parse(SERVICE, DOCUMENT.as_bytes())
        // The text is a constant of the code, and a test parses it.
        .unwrap_or_else(|e| panic!("the document of the service inbox is bad: {e}"));
    document.resources = config
        .groups
        .iter()
        .filter_map(|(id, group)| {
            let path = vec![Segment::new(GROUP, id).ok()?];
            Some(Resource {
                path,
                label: label_text(&group.name),
                // An empty list means: each action that lists the type `group`.
                actions: Vec::new(),
            })
        })
        .collect();
    Arc::new(document)
}

/// The page of the inbox in the settings. The gear of the launcher links to it for an account
/// that is not the superadmin.
pub const PAGE: &str = "/auth/settings/inbox";

/// The service of the inbox as the catalogue lists it. It owns no path: the portal gives the
/// pages itself. The inbox is a feature of the portal, as the keys are. Only the editors of
/// permissions show it, as "Inbox (portal)". The launcher and the services page leave it out.
pub fn service() -> crate::catalog::Service {
    crate::catalog::Service {
        id: SERVICE.to_string(),
        group: "Portal".to_string(),
        name: "Inbox (portal)".to_string(),
        description: "Alerts to phones. A feature of the portal.".to_string(),
        icon: Some("tb:bell".to_string()),
        href: PAGE.to_string(),
        new_tab: false,
        weight: 0,
        health_url: None,
        actions_url: None,
        exclude: Vec::new(),
        health: crate::catalog::Health::Unknown,
        runtime: None,
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

/// The chain of the node `group:<id>`.
pub fn group_chain(group: &str) -> Option<Vec<Segment>> {
    Segment::new(GROUP, group).ok().map(|s| vec![s])
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

    fn config() -> InboxConfig {
        let raw = r#"
            server { listen_address = "0.0.0.0:4181" trusted_proxies = [] }
            authentication { superadmin_login_env = "L" superadmin_password_env = "P" }
            inbox {
              server_key_env = "K"
              devices {
                listen = "0.0.0.0:8080" public_address = "inbox.example.org:8080"
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

    fn grants(texts: &[&str]) -> Vec<Grant> {
        texts.iter().map(|t| Grant::parse(t).unwrap()).collect()
    }

    #[test]
    fn the_document_lists_the_six_actions_the_group_type_and_one_resource_for_each_group() {
        let document = document(&config());
        let names: Vec<&str> = document.actions.iter().map(|a| a.name.as_str()).collect();
        assert_eq!(names, ACTIONS);
        assert_eq!(document.types.len(), 1);
        assert_eq!(document.types[0].name, GROUP);
        assert_eq!(document.types[0].parent, "service");
        for name in [GROUP_SUBSCRIBE, ALERT_SEND, ALERT_READ] {
            let action = document.actions.iter().find(|a| a.name == name).unwrap();
            assert_eq!(action.resource_types, ["group"], "{name}");
        }
        for name in [DEVICE_READ, DEVICE_REVOKE, CONNECTION_READ] {
            let action = document.actions.iter().find(|a| a.name == name).unwrap();
            assert_eq!(action.resource_types, ["service"], "{name}");
        }
        let send = document
            .actions
            .iter()
            .find(|a| a.name == ALERT_SEND)
            .unwrap();
        assert_eq!(send.limits, ["alerts"]);
        assert_eq!(document.limits.len(), 1);
        assert_eq!(document.limits[0].kind, crate::limits::Kind::Budget);
        let labels: Vec<(String, String)> = document
            .resources
            .iter()
            .map(|r| (crate::grants::path_text(&r.path), r.label.clone()))
            .collect();
        assert_eq!(
            labels,
            [
                ("group:infra".to_string(), "Infra".to_string()),
                ("group:oncall".to_string(), "On call".to_string())
            ]
        );
    }

    #[test]
    fn a_grant_on_a_group_allows_that_group_only() {
        let document = document(&config());
        let held = grants(&["alert.send on service:inbox/group:infra"]);
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
        assert!(!allows(&document, "bob", &held, DEVICE_READ, None));
        let service = grants(&["device.read on service:inbox"]);
        assert!(allows(&document, "bob", &service, DEVICE_READ, None));
        assert!(!allows(&document, "bob", &service, DEVICE_REVOKE, None));
        assert_eq!(
            groups_with(
                &document,
                "bob",
                &grants(&["group.subscribe on service:inbox/group:oncall"]),
                GROUP_SUBSCRIBE,
                ["infra".to_string(), "oncall".to_string()].into_iter()
            ),
            ["oncall"]
        );
    }

    #[test]
    fn a_grant_that_does_not_follow_the_document_is_refused() {
        let document = document(&config());
        // The action alert.send does not apply to the whole service.
        assert!(document
            .check_grant(&Grant::parse("alert.send on service:inbox/group:infra").unwrap())
            .is_ok());
        assert!(document
            .check_grant(&Grant::parse("device.read on service:inbox/group:infra").unwrap())
            .is_err());
        assert!(document
            .check_grant(&Grant::parse("alert.send on service:inbox/host:infra").unwrap())
            .is_err());
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
    fn a_key_needs_the_permission_on_its_own_side_and_on_the_side_of_its_owner() {
        use crate::decision::{key, CounterOwner, Owner};
        let document = document(&config());
        let chain = group_chain("infra").unwrap();
        let q = key_question(&document, &chain, ALERT_SEND);
        let send = grants(&["alert.send on service:inbox/group:infra"]);
        let none = grants(&[]);
        let owner = CounterOwner::User("owner".into());
        let keyed = CounterOwner::Key("k".into());
        let decide = |key_grants: &[Grant], owner_grants: &[Grant]| {
            key(&keyed, key_grants, Owner::Person(owner_grants), &owner, &q).allowed
        };
        assert!(decide(&send, &send), "both sides allow");
        assert!(!decide(&send, &none), "the owner has no permission");
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
    fn the_budget_alerts_makes_one_counter_for_each_window_and_each_side() {
        use crate::decision::{key, CounterOwner, Owner};
        use crate::limits::Limits;
        let document = document(&config());
        let chain = group_chain("infra").unwrap();
        let q = key_question(&document, &chain, ALERT_SEND);
        let budget = Limits::from_json(
            r#"{"alerts":[{"amount":5,"window":"minute"},{"amount":50,"window":"hour"}]}"#,
        )
        .unwrap();
        let owner_grants = vec![
            Grant::parse("access on service:inbox")
                .unwrap()
                .with_limits(budget),
            Grant::parse("alert.send on service:inbox/group:infra").unwrap(),
        ];
        let key_grants = grants(&["alert.send on service:inbox/group:infra"]);
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
            .all(|c| c.id.budget == "alerts" && c.id.service == SERVICE));
        // Another action has no counter: the budget belongs to alert.send only.
        let read_chain = group_chain("infra").unwrap();
        let read_q = key_question(&document, &read_chain, ALERT_READ);
        let read_grants = grants(&["alert.read on service:inbox/group:infra"]);
        let owner_read = vec![
            owner_grants[0].clone(),
            Grant::parse("alert.read on service:inbox/group:infra").unwrap(),
        ];
        let verdict = key(
            &CounterOwner::Key("k".into()),
            &read_grants,
            Owner::Person(&owner_read),
            &CounterOwner::User("owner".into()),
            &read_q,
        );
        assert!(verdict.allowed && verdict.counters.is_empty());
    }

    #[test]
    fn a_budget_of_alerts_is_valid_on_access_and_a_ceiling_is_not() {
        use crate::limits::Limits;
        let document = document(&config());
        let budget = Limits::from_json(r#"{"alerts":[{"amount":5,"window":"minute"}]}"#).unwrap();
        assert!(budget.check(&document.limits, "access", &[]).is_ok());
        // The budget is not a value of the action alert.send.
        assert!(budget
            .check(&document.limits, ALERT_SEND, &["alerts".to_string()])
            .is_err());
        // Each limit of the service is a budget: access takes it, an action takes no number.
        let inputs = document.limit_inputs("access");
        assert_eq!(inputs.len(), 1);
        assert!(inputs[0].budget && inputs[0].name == "alerts");
        assert!(document.limit_inputs(ALERT_SEND).is_empty());
    }
}
