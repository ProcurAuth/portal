//! The decision of the PDP for one question, from the grants (protocol 3.5, 4, 6, 7, 8).
//!
//! This module has no input or output. The caller gives the grants of the subject, the document of
//! the service and the question. The module answers with the decision, the ceilings of
//! `context.limits` and the counters that a use takes units from. The caller counts the units.
//!
//! A decision for a person looks at the grants of the person. A decision for a key has two sides:
//! the grants of the key and the grants of its owner (protocol 3.5). Each side must allow the
//! action on the resource. The owner can be the superadmin: its side is always `true`, with no
//! limit value and no counter. Each limit of both sides applies (protocol 8.1): the
//! lowest value of a ceiling, and the counters of both sides for a budget.
//!
//! A grant below the service applies only when the document of the service declares its chain
//! and lists its type for its action. A grant on the whole service needs no declaration.

#![allow(dead_code)]

use crate::actions::Document;
use crate::grants::{CounterId, Grant, Segment, ACCESS};
use crate::limits::{Kind, Limits, Value};

/// What the question asks about the resource (protocol 4.4).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Matching {
    /// The default: a decision about the node.
    Node,
    /// Whether a grant applies on the node, above it or below it. The answer is for the display
    /// only.
    Subtree,
}

/// One question about one action on one node of one service.
pub struct Question<'a> {
    pub service: &'a str,
    pub action: &'a str,
    /// The path from the service to the node. Empty for the service itself.
    pub chain: &'a [Segment],
    pub matching: Matching,
    /// The last known document of the service. `None`: no grant below the service applies.
    pub document: Option<&'a Document>,
}

/// The owner of a key.
#[derive(Clone, Copy, Debug)]
pub enum Owner<'a> {
    /// The superadmin: the side of the owner is always `true`.
    Superadmin,
    /// A person, with the grants that apply to the person.
    Person(&'a [Grant]),
}

/// Who owns a counter. The PDP finds the row of the counter from this value.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum CounterOwner {
    /// A user of the database, by login.
    User(String),
    /// A key of the database, by id.
    Key(String),
    /// A subject of the configuration file: the login of an account, or the subject id of a key
    ///.
    File(String),
}

/// One counter that a use takes one unit from, with the amount that the grant gives.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Counter {
    pub owner: CounterOwner,
    pub id: CounterId,
    pub amount: u64,
}

/// The answer to one question.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Verdict {
    pub allowed: bool,
    /// The ceilings of `context.limits`, for a `true` answer to a question about a node. Only
    /// ceilings.
    pub ceilings: Limits,
    /// The counters of a use. They are empty when no budget applies to the subject for this
    /// action. Empty also for a `false` answer.
    pub counters: Vec<Counter>,
    /// The action needs the approval of a person: a grant that covers the node has `confirm`, on
    /// the side of the key or on the side of the owner (OR rule, like the limits of protocol 8.1).
    /// When the owner is the superadmin, only the side of the key counts. The `confirm` of a grant
    /// of a person has no effect on a question whose subject is that person (protocol 17.8, spec
    /// question 51): the person would approve their own action. A question about a subtree
    /// (`Matching::Subtree`) answers `false`: it is for the display only. Always `false` for a
    /// `false` answer.
    pub approval: bool,
}

impl Verdict {
    fn denied() -> Self {
        Self {
            allowed: false,
            ceilings: Limits::new(),
            counters: Vec::new(),
            approval: false,
        }
    }
}

/// Whether one grant of the covering set has `confirm`.
fn confirms(grants: &[&Grant]) -> bool {
    grants.iter().any(|grant| grant.confirm)
}

/// Whether a grant applies to the question: its action and service, and the document.
fn applies(grant: &Grant, q: &Question<'_>) -> bool {
    grant.service == q.service
        && (grant.path.is_empty()
            || q.document
                .is_some_and(|document| document.check_grant(grant).is_ok()))
}

/// The grants of one side that give the action on the node: the path of the grant is a prefix of
/// the chain.
fn covering<'a>(grants: &'a [Grant], q: &'a Question<'a>) -> impl Iterator<Item = &'a Grant> + 'a {
    grants.iter().filter(move |grant| {
        grant.action == q.action && applies(grant, q) && q.chain.starts_with(&grant.path)
    })
}

/// The grants of one side that give the action on the node or on a node below it.
fn extending<'a>(grants: &'a [Grant], q: &'a Question<'a>) -> impl Iterator<Item = &'a Grant> + 'a {
    grants.iter().filter(move |grant| {
        grant.action == q.action
            && applies(grant, q)
            && grant.path.len() > q.chain.len()
            && grant.path.starts_with(q.chain)
    })
}

/// The lowest value of each ceiling that the action lists, among the grants (protocol 7.3, 8.1). A
/// value for a limit that the document does not declare as a ceiling, or that the action does not
/// list, gives nothing (protocol 6.10, 7.3).
fn ceilings_of<'a>(grants: impl Iterator<Item = &'a Grant>, q: &Question<'_>) -> Limits {
    let Some(document) = q.document else {
        return Limits::new();
    };
    let Some(action) = document.actions.iter().find(|a| a.name == q.action) else {
        return Limits::new();
    };
    let mut out = Limits::new();
    for grant in grants {
        for (name, value) in grant.limits.iter() {
            let listed = action.limits.iter().any(|l| l == name);
            let declared = document
                .limits
                .iter()
                .any(|d| d.name == name && d.kind == Kind::Ceiling);
            if !(listed && declared && matches!(value, Value::Ceiling(_))) {
                continue;
            }
            let mut one = Limits::new();
            // A grant holds at most 8 limits, so this set holds at most 8.
            let _ = one.set(name, value.clone());
            out = out.lower(&one);
        }
    }
    out
}

/// The counters of one subject for the action of a use: for each budget that the action lists and
/// that the document declares, one counter for each window of the value on the grant `access` on
/// the service (protocol 6.2, 6.7, 8.3). When two grants `access` give the same window, the
/// lower amount applies.
fn counters_of(owner: &CounterOwner, grants: &[Grant], q: &Question<'_>) -> Vec<Counter> {
    let Some(document) = q.document else {
        return Vec::new();
    };
    let Some(action) = document.actions.iter().find(|a| a.name == q.action) else {
        return Vec::new();
    };
    let mut out: Vec<Counter> = Vec::new();
    for budget in action.limits.iter().filter(|name| {
        document
            .limits
            .iter()
            .any(|d| &d.name == *name && d.kind == Kind::Budget)
    }) {
        for grant in grants
            .iter()
            .filter(|g| g.action == ACCESS && g.path.is_empty() && g.service == q.service)
        {
            let Some(Value::Budget(value)) = grant.limits.get(budget) else {
                continue;
            };
            for item in value.items() {
                let id = CounterId {
                    service: q.service.to_string(),
                    budget: budget.clone(),
                    window: item.window,
                };
                match out.iter_mut().find(|c| c.id == id) {
                    Some(counter) => counter.amount = counter.amount.min(item.amount),
                    None => out.push(Counter {
                        owner: owner.clone(),
                        id,
                        amount: item.amount,
                    }),
                }
            }
        }
    }
    out
}

/// The decision for a person with these grants. The `confirm` of a grant has no effect here: it
/// applies to the keys of the person only (see [`key`]).
pub fn person(login: &str, grants: &[Grant], q: &Question<'_>) -> Verdict {
    let node: Vec<&Grant> = covering(grants, q).collect();
    match q.matching {
        Matching::Node if node.is_empty() => Verdict::denied(),
        Matching::Node => Verdict {
            allowed: true,
            approval: false,
            ceilings: ceilings_of(node.into_iter(), q),
            counters: counters_of(&CounterOwner::User(login.to_string()), grants, q),
        },
        Matching::Subtree => Verdict {
            allowed: !node.is_empty() || extending(grants, q).next().is_some(),
            ceilings: Limits::new(),
            counters: Vec::new(),
            approval: false,
        },
    }
}

/// The decision for a person of the configuration file: its counters are the rows of its login in
/// the file.
pub fn file_person(login: &str, grants: &[Grant], q: &Question<'_>) -> Verdict {
    let mut verdict = person(login, grants, q);
    for counter in &mut verdict.counters {
        counter.owner = CounterOwner::File(login.to_string());
    }
    verdict
}

/// The decision for a key (protocol 3.5): both sides must allow, and the limits of both sides
/// apply (protocol 8.1). `key` names the counters of the key, and `owner_counters` the counters
/// of the owner. `owner_counters` is ignored for the superadmin.
pub fn key(
    key_counters: &CounterOwner,
    key_grants: &[Grant],
    owner: Owner<'_>,
    owner_counters: &CounterOwner,
    q: &Question<'_>,
) -> Verdict {
    let key_node: Vec<&Grant> = covering(key_grants, q).collect();
    match (q.matching, owner) {
        (Matching::Node, Owner::Superadmin) => {
            if key_node.is_empty() {
                return Verdict::denied();
            }
            Verdict {
                allowed: true,
                approval: confirms(&key_node),
                ceilings: ceilings_of(key_node.into_iter(), q),
                counters: counters_of(key_counters, key_grants, q),
            }
        }
        (Matching::Node, Owner::Person(owner_grants)) => {
            let owner_node: Vec<&Grant> = covering(owner_grants, q).collect();
            if key_node.is_empty() || owner_node.is_empty() {
                return Verdict::denied();
            }
            let approval = confirms(&key_node) || confirms(&owner_node);
            let ceilings =
                ceilings_of(key_node.into_iter(), q).lower(&ceilings_of(owner_node.into_iter(), q));
            let mut counters = counters_of(key_counters, key_grants, q);
            counters.extend(counters_of(owner_counters, owner_grants, q));
            Verdict {
                allowed: true,
                ceilings,
                counters,
                approval,
            }
        }
        (Matching::Subtree, Owner::Superadmin) => Verdict {
            allowed: !key_node.is_empty() || extending(key_grants, q).next().is_some(),
            ceilings: Limits::new(),
            counters: Vec::new(),
            approval: false,
        },
        (Matching::Subtree, Owner::Person(owner_grants)) => Verdict {
            allowed: shares_a_node(key_grants, owner_grants, q),
            ceilings: Limits::new(),
            counters: Vec::new(),
            approval: false,
        },
    }
}

/// Whether a grant of the key and a grant of the owner cover one node that is the node of the
/// question, above it or below it (protocol 4.4). Two grants cover a common node when one path is
/// a prefix of the other. The longer path is then the node, and it must be a prefix of the chain
/// or start with it.
fn shares_a_node(key_grants: &[Grant], owner_grants: &[Grant], q: &Question<'_>) -> bool {
    let related = |path: &[Segment]| q.chain.starts_with(path) || path.starts_with(q.chain);
    for key_grant in key_grants
        .iter()
        .filter(|g| g.action == q.action && applies(g, q) && related(&g.path))
    {
        for owner_grant in owner_grants
            .iter()
            .filter(|g| g.action == q.action && applies(g, q) && related(&g.path))
        {
            let (long, short) = if key_grant.path.len() >= owner_grant.path.len() {
                (&key_grant.path, &owner_grant.path)
            } else {
                (&owner_grant.path, &key_grant.path)
            };
            if long.starts_with(short) {
                return true;
            }
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::actions::parse;

    /// The service `datastore` of protocol section 13: hosts, databases and tables, a budget and
    /// two ceilings.
    fn datastore() -> Document {
        parse(
            "datastore",
            br#"{"server":"datastore",
              "resource_types":[
                {"name":"host","parent":"service","description":"A host."},
                {"name":"database","parent":"host","description":"A database."},
                {"name":"table","parent":"database","description":"A table."}],
              "actions":[
                {"name":"data.read","resource_types":["host","database","table"],
                 "description":"Read.","limits":["max_rows","max_duration","requests"]},
                {"name":"data.write","resource_types":["host","database"],
                 "description":"Write.","limits":["max_duration"]}],
              "limits":[
                {"name":"requests","label":"Requests","description":"d","unit":"request","kind":"budget"},
                {"name":"max_rows","label":"Rows","description":"d","unit":"rows","kind":"ceiling","min":1,"max":100000},
                {"name":"max_duration","label":"Duration","description":"d","unit":"seconds","kind":"ceiling","min":1,"max":3600}]}"#,
        )
        .unwrap()
    }

    fn grant(text: &str, limits: &str) -> Grant {
        Grant::parse(text)
            .unwrap()
            .with_limits(Limits::from_json(limits).unwrap())
    }

    fn path(text: &str) -> Vec<Segment> {
        crate::grants::parse_path(text).unwrap()
    }

    fn ask<'a>(
        document: &'a Document,
        action: &'a str,
        chain: &'a [Segment],
        matching: Matching,
    ) -> Question<'a> {
        Question {
            service: "datastore",
            action,
            chain,
            matching,
            document: Some(document),
        }
    }

    const ACCESS_BUDGET: &str =
        r#"{"requests":[{"amount":10,"window":"second"},{"amount":300,"window":"minute"}]}"#;

    /// Alice of protocol section 13.
    fn alice() -> Vec<Grant> {
        vec![
            grant("access on service:datastore", ACCESS_BUDGET),
            grant(
                "data.read on service:datastore/host:prod/database:test1",
                r#"{"max_rows":1000}"#,
            ),
            grant(
                "data.read on service:datastore/host:prod/database:test2",
                r#"{"max_rows":5000,"max_duration":30}"#,
            ),
            grant(
                "data.read on service:datastore/host:prod/database:test2/table:orders",
                r#"{"max_rows":100}"#,
            ),
            grant(
                "data.write on service:datastore/host:prod/database:test2",
                r#"{"max_duration":10}"#,
            ),
        ]
    }

    #[test]
    fn a_grant_covers_its_node_and_each_node_below_but_not_a_node_above() {
        let document = datastore();
        let alice = alice();
        let test2 = path("host:prod/database:test2");
        let orders = path("host:prod/database:test2/table:orders");
        let table_of_test2 = path("host:prod/database:test2/table:items");
        let host = path("host:prod");
        let test3 = path("host:prod/database:test3");
        for (chain, allowed) in [
            (&test2, true),
            (&orders, true),
            (&table_of_test2, true),
            (&host, false),
            (&test3, false),
            (&Vec::new(), false),
        ] {
            let verdict = person(
                "alice",
                &alice,
                &ask(&document, "data.read", chain, Matching::Node),
            );
            assert_eq!(verdict.allowed, allowed, "{chain:?}");
        }
        // Another action on the same node.
        let write = person(
            "alice",
            &alice,
            &ask(&document, "data.write", &test2, Matching::Node),
        );
        assert!(write.allowed);
        assert!(
            person(
                "alice",
                &alice,
                &ask(&document, "data.write", &orders, Matching::Node)
            )
            .allowed,
            "the grant on the database covers its tables"
        );
    }

    #[test]
    fn the_ceilings_are_the_lowest_values_of_the_grants_that_cover_the_node() {
        let document = datastore();
        let alice = alice();
        let orders = path("host:prod/database:test2/table:orders");
        let verdict = person(
            "alice",
            &alice,
            &ask(&document, "data.read", &orders, Matching::Node),
        );
        // `max_rows` 100 on the table and 5000 on the database: 100. `max_duration` 30 only on the
        // database: it stays.
        assert_eq!(
            verdict.ceilings.to_json(),
            r#"{"max_duration":30,"max_rows":100}"#
        );
        let test2 = path("host:prod/database:test2");
        let verdict = person(
            "alice",
            &alice,
            &ask(&document, "data.read", &test2, Matching::Node),
        );
        assert_eq!(
            verdict.ceilings.to_json(),
            r#"{"max_duration":30,"max_rows":5000}"#
        );
        // The ceiling of the grant of another action does not apply.
        let verdict = person(
            "alice",
            &alice,
            &ask(&document, "data.write", &test2, Matching::Node),
        );
        assert_eq!(verdict.ceilings.to_json(), r#"{"max_duration":10}"#);
    }

    #[test]
    fn a_value_for_a_limit_that_the_action_does_not_list_gives_nothing() {
        let document = datastore();
        let test2 = path("host:prod/database:test2");
        // `max_rows` is not in the list of `data.write`, and `unknown` is not declared.
        let grants = vec![grant(
            "data.write on service:datastore/host:prod/database:test2",
            r#"{"max_rows":1,"unknown":2,"max_duration":7}"#,
        )];
        let verdict = person(
            "a",
            &grants,
            &ask(&document, "data.write", &test2, Matching::Node),
        );
        assert!(verdict.allowed, "the right of the grant still applies");
        assert_eq!(verdict.ceilings.to_json(), r#"{"max_duration":7}"#);
    }

    #[test]
    fn a_use_takes_one_unit_from_each_window_of_each_budget_that_the_action_lists() {
        let document = datastore();
        let alice = alice();
        let test2 = path("host:prod/database:test2");
        let verdict = person(
            "alice",
            &alice,
            &ask(&document, "data.read", &test2, Matching::Node),
        );
        let windows: Vec<(&str, u64)> = verdict
            .counters
            .iter()
            .map(|c| (c.id.window.name(), c.amount))
            .collect();
        assert_eq!(windows, [("second", 10), ("minute", 300)]);
        assert!(verdict
            .counters
            .iter()
            .all(|c| c.owner == CounterOwner::User("alice".into()) && c.id.budget == "requests"));
        // `data.write` does not list the budget: no counter.
        let verdict = person(
            "alice",
            &alice,
            &ask(&document, "data.write", &test2, Matching::Node),
        );
        assert!(verdict.counters.is_empty());
        // A person with no budget value has no counter.
        let plain = vec![grant("data.read on service:datastore/host:prod", "{}")];
        let host = path("host:prod");
        let verdict = person(
            "bob",
            &plain,
            &ask(&document, "data.read", &host, Matching::Node),
        );
        assert!(verdict.allowed && verdict.counters.is_empty());
    }

    #[test]
    fn a_file_person_counts_on_the_rows_of_the_file() {
        let document = datastore();
        let test2 = path("host:prod/database:test2");
        let verdict = file_person(
            "carol",
            &alice(),
            &ask(&document, "data.read", &test2, Matching::Node),
        );
        assert!(verdict
            .counters
            .iter()
            .all(|c| c.owner == CounterOwner::File("carol".into())));
        assert_eq!(verdict.counters.len(), 2);
    }

    #[test]
    fn two_grants_access_give_the_lower_amount_for_a_window() {
        let document = datastore();
        let grants = vec![
            grant(
                "access on service:datastore",
                r#"{"requests":[{"amount":10,"window":"minute"}]}"#,
            ),
            grant(
                "access on service:datastore",
                r#"{"requests":[{"amount":4,"window":"minute"},{"amount":9,"window":"hour"}]}"#,
            ),
            grant("data.read on service:datastore/host:prod", "{}"),
        ];
        let host = path("host:prod");
        let verdict = person(
            "a",
            &grants,
            &ask(&document, "data.read", &host, Matching::Node),
        );
        let mut got: Vec<_> = verdict
            .counters
            .iter()
            .map(|c| (c.id.window.name(), c.amount))
            .collect();
        got.sort();
        assert_eq!(got, [("hour", 9), ("minute", 4)]);
    }

    #[test]
    fn a_grant_below_the_service_needs_the_document() {
        let alice = alice();
        let test2 = path("host:prod/database:test2");
        let none = Question {
            service: "datastore",
            action: "data.read",
            chain: &test2,
            matching: Matching::Node,
            document: None,
        };
        assert!(!person("alice", &alice, &none).allowed);
        // A grant on a type that the action does not list does not apply.
        let document = datastore();
        let grants = vec![grant(
            "data.write on service:datastore/host:prod/database:test2/table:orders",
            "{}",
        )];
        let orders = path("host:prod/database:test2/table:orders");
        assert!(
            !person(
                "a",
                &grants,
                &ask(&document, "data.write", &orders, Matching::Node)
            )
            .allowed
        );
    }

    #[test]
    fn the_subtree_answer_includes_a_grant_below_the_node_and_gives_no_limit() {
        let document = datastore();
        let alice = alice();
        let host = path("host:prod");
        let verdict = person(
            "alice",
            &alice,
            &ask(&document, "data.read", &host, Matching::Subtree),
        );
        assert!(verdict.allowed, "a grant on a database below the host");
        assert!(verdict.ceilings.is_empty() && verdict.counters.is_empty());
        let node = person(
            "alice",
            &alice,
            &ask(&document, "data.read", &host, Matching::Node),
        );
        assert!(!node.allowed);
        let other = path("host:dev");
        assert!(
            !person(
                "alice",
                &alice,
                &ask(&document, "data.read", &other, Matching::Subtree)
            )
            .allowed
        );
    }

    /// The key of Alice in protocol section 13: it reads `test2` with `max_rows` 10000 and a budget
    /// of 20 each minute.
    fn alice_key() -> Vec<Grant> {
        vec![
            grant(
                "access on service:datastore",
                r#"{"requests":[{"amount":20,"window":"minute"}]}"#,
            ),
            grant(
                "data.read on service:datastore/host:prod/database:test2",
                r#"{"max_rows":10000}"#,
            ),
        ]
    }

    #[test]
    fn a_key_needs_a_grant_of_the_key_and_a_right_of_the_owner() {
        let document = datastore();
        let key_grants = alice_key();
        let alice = alice();
        let test2 = path("host:prod/database:test2");
        let orders = path("host:prod/database:test2/table:orders");
        let test1 = path("host:prod/database:test1");
        let key_owner = CounterOwner::Key("k1".into());
        let owner_owner = CounterOwner::User("alice".into());
        let decide = |chain: &[Segment], owner: Owner<'_>, grants: &[Grant]| {
            key(
                &key_owner,
                grants,
                owner,
                &owner_owner,
                &ask(&document, "data.read", chain, Matching::Node),
            )
        };
        assert!(decide(&test2, Owner::Person(&alice), &key_grants).allowed);
        // The key has no grant on test1, although the owner has one.
        assert!(!decide(&test1, Owner::Person(&alice), &key_grants).allowed);
        // The owner loses the right: the key gets nothing (protocol 3.5).
        let reduced: Vec<Grant> = alice
            .iter()
            .filter(|g| !g.to_string().ends_with("database:test2"))
            .cloned()
            .collect();
        assert!(!decide(&test2, Owner::Person(&reduced), &key_grants).allowed);
        // The owner keeps only the table `orders`: the key reads the table, not the database.
        assert!(decide(&orders, Owner::Person(&reduced), &key_grants).allowed);
        // The side of the owner is always true for the superadmin.
        assert!(decide(&test2, Owner::Superadmin, &key_grants).allowed);
        assert!(!decide(&test1, Owner::Superadmin, &key_grants).allowed);
    }

    #[test]
    fn a_key_takes_the_lowest_ceiling_and_counts_on_its_counters_and_on_those_of_its_owner() {
        let document = datastore();
        let key_grants = alice_key();
        let alice = alice();
        let test2 = path("host:prod/database:test2");
        let key_owner = CounterOwner::Key("k1".into());
        let owner_owner = CounterOwner::User("alice".into());
        let q = ask(&document, "data.read", &test2, Matching::Node);
        let verdict = key(
            &key_owner,
            &key_grants,
            Owner::Person(&alice),
            &owner_owner,
            &q,
        );
        // The value of the key, 10000, is above the value of its owner: 5000 applies.
        assert_eq!(
            verdict.ceilings.to_json(),
            r#"{"max_duration":30,"max_rows":5000}"#
        );
        let counters: Vec<(CounterOwner, &str, u64)> = verdict
            .counters
            .iter()
            .map(|c| (c.owner.clone(), c.id.window.name(), c.amount))
            .collect();
        assert_eq!(
            counters,
            [
                (key_owner.clone(), "minute", 20),
                (owner_owner.clone(), "second", 10),
                (owner_owner.clone(), "minute", 300),
            ]
        );
        // The key of the superadmin has only its own counters and its own ceilings.
        let verdict = key(&key_owner, &key_grants, Owner::Superadmin, &owner_owner, &q);
        assert_eq!(verdict.ceilings.to_json(), r#"{"max_rows":10000}"#);
        assert_eq!(verdict.counters.len(), 1);
        assert_eq!(verdict.counters[0].owner, key_owner);
        // A lower value of the key stays: the key narrows the owner (protocol 8.1).
        let narrow = vec![grant(
            "data.read on service:datastore/host:prod/database:test2",
            r#"{"max_rows":7}"#,
        )];
        let verdict = key(&key_owner, &narrow, Owner::Person(&alice), &owner_owner, &q);
        assert_eq!(
            verdict.ceilings.to_json(),
            r#"{"max_duration":30,"max_rows":7}"#
        );
    }

    #[test]
    fn the_subtree_answer_for_a_key_needs_one_node_that_both_sides_cover() {
        let document = datastore();
        let key_owner = CounterOwner::Key("k1".into());
        let owner_owner = CounterOwner::User("alice".into());
        // The example of protocol 4.4: a key with `data.read` on `test2` and an owner with
        // `data.read` only on `test1`.
        let key_grants = vec![grant(
            "data.read on service:datastore/host:prod/database:test2",
            "{}",
        )];
        let owner_grants = vec![grant(
            "data.read on service:datastore/host:prod/database:test1",
            "{}",
        )];
        let q = |chain: &'static [Segment]| ask(&document, "data.read", chain, Matching::Subtree);
        let no_chain: &'static [Segment] = &[];
        let verdict = key(
            &key_owner,
            &key_grants,
            Owner::Person(&owner_grants),
            &owner_owner,
            &q(no_chain),
        );
        assert!(!verdict.allowed);
        // The owner holds the whole host: the node `test2` is covered by both.
        let owner_host = vec![grant("data.read on service:datastore/host:prod", "{}")];
        let verdict = key(
            &key_owner,
            &key_grants,
            Owner::Person(&owner_host),
            &owner_owner,
            &q(no_chain),
        );
        assert!(verdict.allowed);
        // The node of the question is below the longer path: both cover it.
        let deep: &'static [Segment] =
            Box::leak(path("host:prod/database:test2/table:orders").into_boxed_slice());
        assert!(
            key(
                &key_owner,
                &key_grants,
                Owner::Person(&owner_host),
                &owner_owner,
                &q(deep)
            )
            .allowed
        );
        // A node that diverges from the longer path has no common node.
        let other: &'static [Segment] =
            Box::leak(path("host:prod/database:test3").into_boxed_slice());
        assert!(
            !key(
                &key_owner,
                &key_grants,
                Owner::Person(&owner_host),
                &owner_owner,
                &q(other)
            )
            .allowed
        );
        // The superadmin as owner: the grants of the key decide.
        assert!(
            key(
                &key_owner,
                &key_grants,
                Owner::Superadmin,
                &owner_owner,
                &q(no_chain)
            )
            .allowed
        );
    }

    /// A grant of `data.write` with `confirm` on the host `prod`, with no limit value.
    fn confirming(text: &str) -> Grant {
        Grant::parse(text).unwrap().with_confirm(true)
    }

    fn write_chain() -> Vec<Segment> {
        path("host:prod/database:test2")
    }

    #[test]
    fn confirm_true_when_key_or_owner_grant_has_it() {
        let document = datastore();
        let chain = write_chain();
        let q = ask(&document, "data.write", &chain, Matching::Node);
        let plain = vec![grant("data.write on service:datastore/host:prod", "{}")];
        let asking = vec![confirming("data.write on service:datastore/host:prod")];
        let who = CounterOwner::Key("k".into());
        let owner_who = CounterOwner::User("alice".into());
        // Nobody asks: no approval.
        let verdict = key(&who, &plain, Owner::Person(&plain), &owner_who, &q);
        assert!(verdict.allowed && !verdict.approval);
        // The side of the key asks.
        let verdict = key(&who, &asking, Owner::Person(&plain), &owner_who, &q);
        assert!(verdict.allowed && verdict.approval, "key side");
        // The side of the owner asks: every key of the owner asks.
        let verdict = key(&who, &plain, Owner::Person(&asking), &owner_who, &q);
        assert!(verdict.allowed && verdict.approval, "owner side");
        // The grant of a person asks for nothing when the question is the question of the person
        // (protocol 17.8): it applies to the keys of the person only.
        let above = vec![confirming("data.write on service:datastore/host:prod")];
        let own = person("alice", &above, &q);
        assert!(own.allowed && !own.approval, "own use: confirm is ignored");
        // A grant of another action, or on another node, does not ask.
        let other = vec![
            confirming("data.read on service:datastore/host:prod"),
            confirming("data.write on service:datastore/host:dev"),
        ];
        let verdict = person("alice", &other, &q);
        assert!(!verdict.allowed && !verdict.approval, "no covering grant");
        // Two covering grants of the owner: one with confirm is enough for a key (OR rule).
        let mixed = vec![
            grant("data.write on service:datastore/host:prod", "{}"),
            confirming("data.write on service:datastore/host:prod/database:test2"),
        ];
        assert!(!person("alice", &mixed, &q).approval);
        let verdict = key(&who, &plain, Owner::Person(&mixed), &owner_who, &q);
        assert!(
            verdict.allowed && verdict.approval,
            "owner side, two grants"
        );
        // A denied question never asks.
        let denied = key(&who, &asking, Owner::Person(&[]), &owner_who, &q);
        assert!(!denied.allowed && !denied.approval);
    }

    #[test]
    fn superadmin_owner_uses_key_confirm_only() {
        let document = datastore();
        let chain = write_chain();
        let q = ask(&document, "data.write", &chain, Matching::Node);
        let plain = vec![grant("data.write on service:datastore/host:prod", "{}")];
        let asking = vec![confirming("data.write on service:datastore/host:prod")];
        let who = CounterOwner::Key("k".into());
        let owner_who = CounterOwner::User("root".into());
        // The superadmin has no grant: there is no owner side to ask.
        let verdict = key(&who, &plain, Owner::Superadmin, &owner_who, &q);
        assert!(verdict.allowed && !verdict.approval);
        // The grants that the owner row would hold do not count: only the key decides.
        let verdict = key(&who, &asking, Owner::Superadmin, &owner_who, &q);
        assert!(verdict.allowed && verdict.approval);
    }

    #[test]
    fn subtree_answers_true_with_confirm() {
        let document = datastore();
        let host = path("host:prod");
        let q = ask(&document, "data.write", &host, Matching::Subtree);
        let asking = vec![confirming(
            "data.write on service:datastore/host:prod/database:test2",
        )];
        let who = CounterOwner::Key("k".into());
        let owner_who = CounterOwner::User("alice".into());
        // The grant is below the node: the display question is true, and confirm changes nothing.
        let verdict = person("alice", &asking, &q);
        assert!(verdict.allowed);
        let verdict = key(&who, &asking, Owner::Person(&asking), &owner_who, &q);
        assert!(verdict.allowed);
        let verdict = key(&who, &asking, Owner::Superadmin, &owner_who, &q);
        assert!(verdict.allowed);
        // A question about a subtree is for the display only: it never asks for an approval.
        assert!(!verdict.approval);
    }

    #[test]
    fn confirm_is_not_a_limit() {
        // A grant with confirm has no limit value and no counter: the decision does not change.
        let document = datastore();
        let chain = write_chain();
        let q = ask(&document, "data.write", &chain, Matching::Node);
        let plain = vec![
            grant("access on service:datastore", ACCESS_BUDGET),
            grant(
                "data.write on service:datastore/host:prod",
                r#"{"max_duration":10}"#,
            ),
        ];
        let asking: Vec<Grant> = plain
            .iter()
            .cloned()
            .map(|g| g.with_confirm(true))
            .collect();
        let a = person("alice", &plain, &q);
        let b = person("alice", &asking, &q);
        assert_eq!(a.ceilings, b.ceilings);
        assert_eq!(a.counters, b.counters);
        // The grant of a person asks for nothing for the person. It asks for the key of the person.
        assert!(!a.approval && !b.approval);
        let who = CounterOwner::Key("k".into());
        let owner_who = CounterOwner::User("alice".into());
        let verdict = key(&who, &plain, Owner::Person(&asking), &owner_who, &q);
        assert!(verdict.allowed && verdict.approval);
        assert_eq!(verdict.ceilings, a.ceilings);
    }
}
