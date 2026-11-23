//! The approval gate of the PDP over HTTP, with the inbox and its items on SQLite and on PostgreSQL.
//!
//! The conformance cases D76 to D89 and E19 prove the answers of the protocol (see `conformance_tests`). These tests
//! prove what the cases cannot: the single use of an approval under a race, the order count then consume, the units
//! that go back, the fault between the steps, the flag of the server, and what an answer holds.

use std::sync::atomic::Ordering;
use std::sync::{Arc, Barrier};

use inbox_core::proto::{Decision, ItemState};
use serde_json::{json, Value};

use super::approval::{self, Bound, InvalidId};
use crate::grants::Grant;
use crate::http::cluster_tests::*;
use crate::http::pdp_rig::{app_of, seed, InboxRig, Seed};
use crate::store::testing::{backends, Backend};

/// The service `db`: `data.write` counts the budget `requests`.
const DOCUMENT: &str = r#"{"server":"db",
  "resource_types":[{"name":"host","parent":"service","description":"A host."},
                    {"name":"database","parent":"host","description":"A database."}],
  "actions":[
    {"name":"data.read","resource_types":["host","database"],"description":"Read.","limits":["requests"]},
    {"name":"data.write","resource_types":["host","database"],"description":"Write.",
     "limits":["requests"]},
    {"name":"mcp.session","resource_types":["service"],"description":"Open a session."}],
  "limits":[
    {"name":"requests","label":"Requests","description":"d","unit":"request","kind":"budget"}]}"#;

/// The owner that every seeded answer names. No answer of the PDP may hold it.
const CANARY_OWNER: &str = "canary-owner-5521";
const ANALYTICS: &str = "host:prod/database:analytics";
const STAGING: &str = "host:prod/database:staging";

struct Fixture {
    node: Node,
    port: u16,
    app: Arc<crate::inbox::App>,
    _rig: InboxRig,
}

/// A server with the inbox and the service `db`. `approvals`: the file has the block `inbox.approvals`. The owner `alice`
/// has `access` and `data.write` on the host `prod`.
fn fixture(backend: &Backend, approvals: bool) -> Option<Fixture> {
    let rig = if approvals {
        InboxRig::new(backend, "5m")?
    } else {
        InboxRig::without_approvals(backend)?
    };
    let port = free_port();
    let mut env = rig.env.clone();
    env.push(("TEST_PDP_DB".into(), PDP_DB_TOKEN.into()));
    let node = start_custom(
        Arc::new(backend.fresh()),
        None,
        Custom {
            env,
            boot_inbox: true,
            ..Custom::default()
        },
        &format!(
            r#"identity {{ issuer = "https://portal.test" signing_key_env = "TEST_IDENTITY_KEY" }}
            pdp {{ listen_address = "127.0.0.1:{port}" url = "http://pdp.test:{port}" plain_http = true max_evaluations = 8 }}
            keys {{}}
            pdp_client "db" {{ token_env = "TEST_PDP_DB" }}
            {}"#,
            rig.config
        ),
    );
    publish_catalog(&node, &[]);
    node.state.actions.set_document(
        "db",
        crate::actions::parse("db", DOCUMENT.as_bytes()).unwrap(),
    );
    wait_until(std::time::Duration::from_secs(10), || {
        std::net::TcpStream::connect(("127.0.0.1", port)).is_ok()
    });
    let grants: Vec<Grant> = [
        "access on service:db",
        "data.write on service:db/host:prod",
        "keys.own.use on service:portal/key_kind:mcp",
    ]
    .iter()
    .map(|g| Grant::parse(g).unwrap())
    .collect();
    let mut grants = grants;
    grants.sort();
    node.state
        .store
        .insert_user(&crate::store::UserRow {
            login: "alice".into(),
            password_hash: "h".into(),
            grants,
            created_at: 1,
        })
        .unwrap();
    node.state.users.refresh(&node.state.store).unwrap();
    let app = app_of(&node, None);
    Some(Fixture {
        node,
        port,
        app,
        _rig: rig,
    })
}

/// Runs `test` on each backend that can run it.
fn each_fixture(approvals: bool, test: impl Fn(&Fixture, &str)) {
    for backend in backends() {
        let Some(f) = fixture(&backend, approvals) else {
            eprintln!("INBOX_TEST_PG is not set: skipped on PostgreSQL");
            continue;
        };
        test(&f, backend.name());
    }
}

impl Fixture {
    /// A key of `alice` that can use `data.write` on analytics with `confirm`. `budget` is the amount of
    /// `requests` each minute of the key, or no budget.
    fn key(&self, id: &str, byte: u8, budget: Option<u32>) -> String {
        let access = match budget {
            Some(amount) => Grant::new("access", "db")
                .unwrap()
                .with_limits(limits(&format!(
                    r#"{{"requests":[{{"amount":{amount},"window":"minute"}}]}}"#
                ))),
            None => Grant::new("access", "db").unwrap(),
        };
        let write = Grant::from_path_text("data.write", "db", ANALYTICS)
            .unwrap()
            .with_confirm(true);
        add_key(&self.node, id, Some("alice"), byte, vec![access, write]);
        format!("key:{id}")
    }

    fn now(&self) -> i64 {
        use crate::inbox::Clock;
        self.app.clock.now_ms()
    }

    /// One approval for `subject` on analytics, answered with `approve` and usable for 5 minutes.
    fn approved(&self, subject: &str, effect: &str) -> String {
        self.seed(
            subject,
            ANALYTICS,
            effect,
            ItemState::Answered,
            Some(Decision::Approve),
            |s| {
                s.consume_before = Some(self.now() + 300_000);
            },
        )
    }

    fn seed(
        &self,
        subject: &str,
        path: &str,
        effect: &str,
        state: ItemState,
        decision: Option<Decision>,
        adjust: impl FnOnce(&mut Seed<'static>),
    ) -> String {
        let mut s = Seed {
            service: "db",
            subject: Box::leak(subject.to_string().into_boxed_str()),
            owner: CANARY_OWNER,
            action: "data.write",
            resource_path: Box::leak(path.to_string().into_boxed_str()),
            effect: Box::leak(effect.to_string().into_boxed_str()),
            state,
            decision,
            answered_at: Some(self.now()),
            consume_before: None,
            consumed_at: None,
        };
        adjust(&mut s);
        seed(&self.app, &s)
    }

    fn body(&self, subject: &str, database: &str, effect: Option<&str>, usage: bool) -> String {
        let mut context = serde_json::Map::new();
        if usage {
            context.insert("usage".into(), json!({"units": 1}));
        }
        if let Some(effect) = effect {
            context.insert("approval".into(), json!({"effect": effect}));
        }
        json!({
            "subject": {"type": "user", "id": subject},
            "action": {"name": "data.write"},
            "resource": {"type": "database", "id": database, "properties": {"ancestors": [
                {"type": "service", "id": "db"}, {"type": "host", "id": "prod"}]}},
            "context": context
        })
        .to_string()
    }

    fn post(&self, body: &str) -> Reply {
        self.post_to("/access/v1/evaluation", body)
    }

    fn post_to(&self, path: &str, body: &str) -> Reply {
        pdp(self.port, path, Some(PDP_DB_TOKEN), body, None)
    }

    /// The answer of a question about analytics.
    fn ask(&self, subject: &str, effect: &str, usage: bool) -> Value {
        let reply = self.post(&self.body(subject, "analytics", Some(effect), usage));
        assert_eq!(reply.status, 200, "{}", reply.body);
        reply.json()
    }

    fn metric(&self, reason: &str) -> u64 {
        let text = self.node.state.pdp_stats.render();
        let prefix = format!("portal_pdp_approval_total{{reason=\"{reason}\"}} ");
        text.lines()
            .find_map(|l| l.strip_prefix(&prefix))
            .unwrap_or_else(|| panic!("no metric {reason}"))
            .parse()
            .unwrap()
    }

    /// Whether the approval of the key for the effect can be used, without consuming it.
    fn usable(&self, subject: &str, effect: &str) -> bool {
        self.ask(subject, effect, false)["decision"] == true
    }
}

fn effect(n: u32) -> String {
    format!("sha256:{n:064x}")
}

fn reason(answer: &Value) -> Option<&str> {
    answer["context"]["reason"].as_str()
}

fn units(answer: &Value) -> Option<u64> {
    answer["context"]["usage"]["units"].as_u64()
}

#[test]
fn consume_takes_one_unit_once() {
    stay_in_a_minute();
    each_fixture(true, |f, name| {
        let key = f.key("9b1c04e2aa71f0c3", 1, Some(2));
        let (first, second) = (effect(1), effect(2));
        f.approved(&key, &first);
        f.approved(&key, &second);
        let used = f.ask(&key, &first, true);
        assert_eq!(used["decision"], true, "[{name}] {used}");
        assert_eq!(units(&used), Some(1), "[{name}] one unit");
        // The same effect again: the approval is consumed and the answer takes no unit.
        let again = f.ask(&key, &first, true);
        assert_eq!(again["decision"], false, "[{name}] {again}");
        assert_eq!(reason(&again), Some("approval_required"), "[{name}]");
        assert!(
            again["context"]["usage"].is_null(),
            "[{name}] no usage on a false answer"
        );
        // The budget of the key is 2 each minute. A refused use that took a unit would make this one fail.
        let other = f.ask(&key, &second, true);
        assert_eq!(
            other["decision"], true,
            "[{name}] the second unit is free: {other}"
        );
        assert_eq!(f.metric("consumed"), 2, "[{name}]");
    });
}

#[test]
fn budget_exceeded_keeps_approval_usable() {
    stay_in_a_minute();
    each_fixture(true, |f, name| {
        let key = f.key("9b1c04e2aa71f0c3", 1, Some(1));
        let (first, second, third) = (effect(1), effect(2), effect(3));
        for e in [&first, &second, &third] {
            f.approved(&key, e);
        }
        assert_eq!(f.ask(&key, &first, true)["decision"], true, "[{name}]");
        // The budget is spent. The count refuses before the consumption: the approval of the second effect is not touched.
        let refused = f.ask(&key, &second, true);
        assert_eq!(refused["decision"], false, "[{name}] {refused}");
        assert_eq!(reason(&refused), Some("budget_exceeded"), "[{name}]");
        assert!(refused["context"]["retry_after"]
            .as_u64()
            .is_some_and(|s| s >= 1));
        assert_eq!(
            f.metric("consumed"),
            1,
            "[{name}] only the first use consumed"
        );
        assert_eq!(f.metric("refunded"), 0, "[{name}] nothing to give back");
        assert!(
            f.usable(&key, &second),
            "[{name}] the approval is still usable"
        );
        // Several refusals leave the approval usable.
        for _ in 0..3 {
            assert_eq!(
                reason(&f.ask(&key, &second, true)),
                Some("budget_exceeded"),
                "[{name}]"
            );
        }
        assert!(f.usable(&key, &second), "[{name}]");
        // A count that fails: HTTP 503, no decision, and the approval is not touched.
        f.node.state.pdp_fault.store(2, Ordering::SeqCst);
        let failed = f.post(&f.body(&key, "analytics", Some(&third), true));
        f.node.state.pdp_fault.store(0, Ordering::SeqCst);
        assert_eq!(failed.status, 503, "[{name}] {}", failed.body);
        assert!(
            f.usable(&key, &third),
            "[{name}] still usable after a failed count"
        );
    });
}

/// The process stops after the count and before the consumption (protocol 17.2). The approval is not lost and is not
/// consumed: the agent asks again. One unit stays counted (the over-count of protocol 10.6), and the approval runs one
/// time at most.
#[test]
fn crash_between_count_and_consume_never_loses_the_approval_and_never_double_executes() {
    stay_in_a_minute();
    each_fixture(true, |f, name| {
        let key = f.key("9b1c04e2aa71f0c3", 1, Some(2));
        let (e, other) = (effect(1), effect(2));
        f.approved(&key, &e);
        f.approved(&key, &other);
        let before = f.node.state.store.counted_units();
        f.node.state.pdp_fault.store(1, Ordering::SeqCst);
        let crashed = f.post(&f.body(&key, "analytics", Some(&e), true));
        f.node.state.pdp_fault.store(0, Ordering::SeqCst);
        // The service gets no decision (HTTP 503).
        assert_eq!(crashed.status, 503, "[{name}] {}", crashed.body);
        assert_eq!(
            f.node.state.store.counted_units(),
            before + 1,
            "[{name}] the unit of the key stays counted"
        );
        // The approval is not lost and not consumed.
        assert!(f.usable(&key, &e), "[{name}] the approval is still usable");
        assert_eq!(f.metric("consumed"), 0, "[{name}]");
        // The agent asks again: one execution, and never a second one.
        let mut executions = 0;
        for _ in 0..5 {
            let answer = f.ask(&key, &e, true);
            if answer["decision"] == true {
                executions += 1;
            } else {
                // The budget of the key is 2 each minute: the crash took one unit, the use took the other.
                assert!(
                    matches!(
                        reason(&answer),
                        Some("approval_required" | "budget_exceeded")
                    ),
                    "[{name}] {answer}"
                );
            }
        }
        assert_eq!(executions, 1, "[{name}] one execution");
        assert_eq!(f.metric("consumed"), 1, "[{name}]");
        // The over-count is one unit: the second approval meets the spent budget.
        assert_eq!(
            reason(&f.ask(&key, &other, true)),
            Some("budget_exceeded"),
            "[{name}]"
        );
    });
}

/// The consumption fails with an error of the store, after the count: the units go back, the approval stays usable.
#[test]
fn a_failed_consumption_gives_the_units_back_and_leaves_the_approval_usable() {
    stay_in_a_minute();
    each_fixture(true, |f, name| {
        let key = f.key("9b1c04e2aa71f0c3", 1, Some(1));
        let e = effect(1);
        f.approved(&key, &e);
        let before = f.node.state.store.counted_units();
        f.node.state.pdp_fault.store(3, Ordering::SeqCst);
        let failed = f.post(&f.body(&key, "analytics", Some(&e), true));
        f.node.state.pdp_fault.store(0, Ordering::SeqCst);
        assert_eq!(failed.status, 503, "[{name}] {}", failed.body);
        assert_eq!(
            f.node.state.store.counted_units(),
            before,
            "[{name}] the units are back"
        );
        assert_eq!(f.metric("refunded"), 1, "[{name}]");
        assert_eq!(f.metric("consumed"), 0, "[{name}]");
        assert!(f.usable(&key, &e), "[{name}]");
        // The budget of the key is 1 each minute: the use works, because the unit went back.
        let used = f.ask(&key, &e, true);
        assert_eq!(used["decision"], true, "[{name}] {used}");
        assert_eq!(units(&used), Some(1), "[{name}]");
    });
}

/// Callers that use one approval at the same time with a large budget: one `true`, and the units of the others go back.
#[test]
fn a_race_for_one_approval_counts_one_use() {
    each_fixture(true, |f, name| {
        let key = f.key("9b1c04e2aa71f0c3", 1, Some(1000));
        for round in 0..5 {
            let e = effect(100 + round);
            f.approved(&key, &e);
            let before = f.node.state.store.counted_units();
            let (trues, required) = race(f, &key, &e, 6);
            assert_eq!((trues, required), (1, 5), "[{name}] round {round}");
            assert_eq!(
                f.node.state.store.counted_units(),
                before + 1,
                "[{name}] round {round}: one use took one unit"
            );
        }
    });
}

/// `callers` threads ask for the use of one approval at the same time. The result is the number of `true` answers.
fn race(f: &Fixture, subject: &str, e: &str, callers: usize) -> (usize, usize) {
    let barrier = Arc::new(Barrier::new(callers));
    let body = f.body(subject, "analytics", Some(e), true);
    let port = f.port;
    let handles: Vec<_> = (0..callers)
        .map(|_| {
            let (barrier, body) = (Arc::clone(&barrier), body.clone());
            std::thread::spawn(move || {
                barrier.wait();
                let reply = pdp(
                    port,
                    "/access/v1/evaluation",
                    Some(PDP_DB_TOKEN),
                    &body,
                    None,
                );
                assert_eq!(reply.status, 200, "{}", reply.body);
                let answer = reply.json();
                (
                    answer["decision"] == true,
                    reason(&answer).map(str::to_string),
                )
            })
        })
        .collect();
    let answers: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();
    let trues = answers.iter().filter(|(t, _)| *t).count();
    let required = answers
        .iter()
        .filter(|(t, r)| !t && r.as_deref() == Some("approval_required"))
        .count();
    (trues, required)
}

#[test]
fn two_resume_calls_race_one_true() {
    each_fixture(true, |f, name| {
        let key = f.key("9b1c04e2aa71f0c3", 1, None);
        for round in 0..10 {
            let e = effect(round);
            f.approved(&key, &e);
            let (trues, required) = race(f, &key, &e, 2);
            assert_eq!((trues, required), (1, 1), "[{name}] round {round}");
        }
    });
}

#[test]
#[ignore = "slow: 100 concurrent connections to the PDP"]
fn hundred_concurrent_resumes_one_true() {
    each_fixture(true, |f, name| {
        let key = f.key("9b1c04e2aa71f0c3", 1, None);
        let e = effect(1);
        f.approved(&key, &e);
        let (trues, required) = race(f, &key, &e, 100);
        assert_eq!((trues, required), (1, 99), "[{name}]");
    });
}

#[test]
fn revoked_key_or_removed_grant_after_approval_is_false() {
    each_fixture(true, |f, name| {
        // A key whose grant of the action is gone: the approval gives nothing.
        add_key(
            &f.node,
            "4d2e8f6a10b37c95",
            Some("alice"),
            2,
            vec![Grant::new("access", "db").unwrap()],
        );
        let no_grant = "key:4d2e8f6a10b37c95";
        let e = effect(1);
        f.approved(no_grant, &e);
        let answer = f.ask(no_grant, &e, true);
        assert_eq!(answer["decision"], false, "[{name}] {answer}");
        assert!(
            answer.get("context").is_none(),
            "[{name}] a plain false: {answer}"
        );
        // A key that is revoked after its approval.
        let key = f.key("9b1c04e2aa71f0c3", 1, None);
        f.approved(&key, &e);
        assert!(f.usable(&key, &e), "[{name}]");
        let revoked = f
            .node
            .state
            .store
            .revoke_keys(Some("alice"), crate::tracking::unix_now());
        assert_eq!(revoked, Ok(2), "[{name}]");
        f.node.state.users.refresh(&f.node.state.store).unwrap();
        let answer = f.ask(&key, &e, true);
        assert_eq!(answer["decision"], false, "[{name}] {answer}");
        assert_eq!(reason(&answer), Some("unknown_subject"), "[{name}]");
        // The approval was not consumed: the grants decide first.
        let items = f.app.items().unwrap();
        let hash = crate::inbox::approval_key_hash("db", &key, "data.write", ANALYTICS, &e);
        let status = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(items.status_for_pdp(&hash))
            .unwrap();
        assert!(
            matches!(status, crate::inbox::Consumed::Usable(_)),
            "[{name}] {status:?}"
        );
    });
}

#[test]
fn other_subject_resource_or_effect_not_applicable() {
    each_fixture(true, |f, name| {
        let other = f.key("4d2e8f6a10b37c95", 2, None);
        // The key can also write to the database staging, with `confirm`.
        add_key(
            &f.node,
            "5c3d9e7b2a1f8046",
            Some("alice"),
            3,
            vec![
                Grant::new("access", "db").unwrap(),
                Grant::from_path_text("data.write", "db", ANALYTICS)
                    .unwrap()
                    .with_confirm(true),
                Grant::from_path_text("data.write", "db", STAGING)
                    .unwrap()
                    .with_confirm(true),
            ],
        );
        let key = "key:5c3d9e7b2a1f8046".to_string();
        let e = effect(1);
        f.approved(&key, &e);
        for (label, answer) in [
            ("another subject", f.ask(&other, &e, true)),
            ("another effect", f.ask(&key, &effect(2), true)),
            (
                "another resource",
                f.post(&f.body(&key, "staging", Some(&e), true)).json(),
            ),
        ] {
            assert_eq!(
                reason(&answer),
                Some("approval_required"),
                "[{name}] {label}: {answer}"
            );
            assert!(
                answer["context"]["evaluation_id"].is_string(),
                "[{name}] {label}"
            );
        }
        assert!(f.usable(&key, &e), "[{name}] the approval is intact");
        assert_eq!(
            f.ask(&key, &e, true)["decision"],
            true,
            "[{name}] the same subject, resource and effect"
        );
    });
}

#[test]
fn abandoned_or_lapsed_reads_as_approval_expired() {
    each_fixture(true, |f, name| {
        let key = f.key("9b1c04e2aa71f0c3", 1, None);
        let (abandoned, lapsed, cancelled) = (effect(1), effect(2), effect(3));
        f.seed(
            &key,
            ANALYTICS,
            &abandoned,
            ItemState::Abandoned,
            None,
            |_| {},
        );
        f.seed(
            &key,
            ANALYTICS,
            &cancelled,
            ItemState::Cancelled,
            None,
            |_| {},
        );
        // The owner approved, and nobody used it before `consume_before`.
        f.seed(
            &key,
            ANALYTICS,
            &lapsed,
            ItemState::Answered,
            Some(Decision::Approve),
            |s| {
                s.consume_before = Some(s.answered_at.unwrap() - 1_000);
            },
        );
        for e in [&abandoned, &lapsed, &cancelled] {
            let used = f.ask(&key, e, true);
            assert_eq!(used["decision"], false, "[{name}]");
            assert_eq!(reason(&used), Some("approval_expired"), "[{name}] {used}");
            assert!(used["context"]["evaluation_id"].is_null(), "[{name}]");
            // Without usage the agent asks again: the answer is as for no approval.
            let read = f.ask(&key, e, false);
            assert_eq!(reason(&read), Some("approval_required"), "[{name}] {read}");
            assert!(read["context"]["evaluation_id"].is_string(), "[{name}]");
        }
    });
}

#[test]
fn confirm_without_the_block_approvals_plain_false() {
    each_fixture(false, |f, name| {
        let key = f.key("9b1c04e2aa71f0c3", 1, None);
        let e = effect(1);
        f.approved(&key, &e);
        for usage in [false, true] {
            let answer = f.ask(&key, &e, usage);
            assert_eq!(
                answer,
                json!({"decision": false}),
                "[{name}] a plain false: {answer}"
            );
        }
        let no_effect = f.post(&f.body(&key, "analytics", None, false)).json();
        assert_eq!(no_effect, json!({"decision": false}), "[{name}]");
        assert_eq!(f.metric("not_enabled"), 3, "[{name}]");
        // The approval stays: no consumption without the block.
        assert_eq!(f.metric("consumed"), 0, "[{name}]");
    });
}

#[test]
fn answers_carry_only_listed_reasons() {
    stay_in_a_minute();
    each_fixture(true, |f, name| {
        let key = f.key("9b1c04e2aa71f0c3", 1, Some(1));
        let now = f.now();
        let mut cases: Vec<(String, bool)> = Vec::new();
        let mut add = |state, decision, adjust: &dyn Fn(&mut Seed<'static>)| {
            let e = effect(cases.len() as u32 + 10);
            f.seed(&key, ANALYTICS, &e, state, decision, |s| adjust(s));
            cases.push((e, true));
            cases.push((cases[cases.len() - 1].0.clone(), false));
        };
        add(ItemState::Pending, None, &|_| {});
        add(ItemState::Replied, None, &|_| {});
        add(ItemState::Answered, Some(Decision::Deny), &|s| {
            s.answered_at = Some(now)
        });
        add(ItemState::Answered, Some(Decision::Deny), &|s| {
            s.answered_at = Some(now - 3_600_000)
        });
        add(ItemState::Answered, Some(Decision::Approve), &|s| {
            s.consume_before = Some(now + 300_000)
        });
        add(ItemState::Answered, Some(Decision::Approve), &|s| {
            s.consume_before = Some(now - 1)
        });
        add(ItemState::Abandoned, None, &|_| {});
        add(ItemState::Expired, None, &|_| {});
        add(ItemState::Superseded, None, &|_| {});
        let listed = [
            "approval_required",
            "approval_pending",
            "approval_denied",
            "approval_expired",
            "budget_exceeded",
        ];
        let mut seen = std::collections::BTreeSet::new();
        for (e, usage) in &cases {
            let reply = f.post(&f.body(&key, "analytics", Some(e), *usage));
            assert_eq!(reply.status, 200, "[{name}] {}", reply.body);
            let answer = reply.json();
            assert!(
                !reply.body.contains(CANARY_OWNER) && !reply.body.contains("seeded"),
                "[{name}] nothing of the approver or the device: {}",
                reply.body
            );
            if let Some(context) = answer["context"].as_object() {
                for member in context.keys() {
                    assert!(
                        ["reason", "evaluation_id", "retry_after", "limits", "usage"]
                            .contains(&member.as_str()),
                        "[{name}] unexpected member {member}: {answer}"
                    );
                }
            }
            if answer["decision"] == false {
                let r = reason(&answer)
                    .unwrap_or_else(|| panic!("[{name}] a false without a reason: {answer}"));
                assert!(listed.contains(&r), "[{name}] {r}");
                seen.insert(r.to_string());
                let with_id = r == "approval_required" || r == "approval_pending";
                assert_eq!(
                    answer["context"]["evaluation_id"].is_string(),
                    with_id,
                    "[{name}] {r}: the evaluation_id only with required and pending"
                );
            }
        }
        // Each reason of the protocol came up, and so did the refusal by the budget.
        for r in &listed[..4] {
            assert!(seen.contains(*r), "[{name}] {r} never came up: {seen:?}");
        }
    });
}

#[test]
fn the_evaluation_id_of_an_answer_binds_the_question_and_works_on_another_instance() {
    each_fixture(true, |f, name| {
        let key = f.key("9b1c04e2aa71f0c3", 1, None);
        let e = effect(1);
        let answer = f.ask(&key, &e, false);
        assert_eq!(reason(&answer), Some("approval_required"), "[{name}]");
        let id = answer["context"]["evaluation_id"]
            .as_str()
            .unwrap()
            .to_string();
        let content = f.app.secrets.content.as_ref().unwrap();
        let bound = Bound {
            service: "db",
            subject: &key,
            action: "data.write",
            resource: ANALYTICS,
            effect: &e,
        };
        let now = f.now();
        assert_eq!(
            approval::check_evaluation_id(content, now, 300, &id, &bound),
            Ok(()),
            "[{name}]"
        );
        let other = Bound {
            subject: "key:4d2e8f6a10b37c95",
            ..bound
        };
        assert_eq!(
            approval::check_evaluation_id(content, now, 300, &id, &other),
            Err(InvalidId::Mismatch),
            "[{name}]"
        );
    });
}

#[test]
fn the_metadata_publishes_the_approval_members_and_a_bad_effect_gets_400() {
    each_fixture(true, |f, name| {
        let reply = send(f.port, "GET", "/.well-known/authzen-configuration", &[], "");
        assert_eq!(reply.status, 200, "[{name}]");
        let meta = reply.json();
        // The values of the harness: lease 30s to 7d, grace 2m, revive 7d, 3 revivals, life 90d, wait 25s, consume 5m,
        // deny 5m, evaluation 5m. The names are those of the reference PDP of the protocol.
        for (member, expected) in [
            ("procurauth_approval_lease_min_seconds", 30),
            ("procurauth_approval_lease_max_seconds", 604_800),
            ("procurauth_approval_grace_seconds", 120),
            ("procurauth_approval_revive_seconds", 604_800),
            ("procurauth_approval_max_revivals", 3),
            ("procurauth_approval_max_life_seconds", 7_776_000),
            ("procurauth_approval_wait_max_seconds", 25),
            ("procurauth_approval_consume_seconds", 300),
            ("procurauth_approval_deny_seconds", 300),
            ("procurauth_approval_evaluation_ttl_seconds", 300),
        ] {
            assert_eq!(meta[member], expected, "[{name}] {member}");
        }
        let key = f.key("9b1c04e2aa71f0c3", 1, None);
        for bad in [
            json!("sha256:00"),
            json!({"effect": "sha256:ABC"}),
            json!({"effect": effect(1), "op": {}}),
            json!({}),
        ] {
            let body = json!({
                "subject": {"type": "user", "id": key},
                "action": {"name": "data.write"},
                "resource": {"type": "database", "id": "analytics", "properties": {"ancestors": [
                    {"type": "service", "id": "db"}, {"type": "host", "id": "prod"}]}},
                "context": {"approval": bad}
            });
            let reply = f.post(&body.to_string());
            assert_eq!(reply.status, 400, "[{name}] {bad}: {}", reply.body);
        }
    });
}

#[test]
fn revoking_a_key_cancels_its_open_approvals() {
    each_fixture(true, |f, name| {
        let key = f.key("9b1c04e2aa71f0c3", 1, None);
        let other = f.key("4d2e8f6a10b37c95", 2, None);
        let (waits, other_waits) = (effect(1), effect(2));
        f.seed(&key, ANALYTICS, &waits, ItemState::Pending, None, |_| {});
        f.seed(
            &other,
            ANALYTICS,
            &other_waits,
            ItemState::Pending,
            None,
            |_| {},
        );
        assert_eq!(
            reason(&f.ask(&key, &waits, true)),
            Some("approval_pending"),
            "[{name}]"
        );
        let cancelled = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(f.node.state.inbox.cancel_key_items("9b1c04e2aa71f0c3"));
        assert_eq!(cancelled, 1, "[{name}] only the items of this key");
        // The cancelled approval reads as void: expired for a use, required for a read. The other key is not touched.
        assert_eq!(
            reason(&f.ask(&key, &waits, true)),
            Some("approval_expired"),
            "[{name}]"
        );
        assert_eq!(
            reason(&f.ask(&key, &waits, false)),
            Some("approval_required"),
            "[{name}]"
        );
        assert_eq!(
            reason(&f.ask(&other, &other_waits, true)),
            Some("approval_pending"),
            "[{name}]"
        );
    });
}

// ---------------------------------------------------------------- who asks: person, key, mcp.session

impl Fixture {
    /// A person with `data.write` on the host `prod`. `confirm`: the grant has `confirm`.
    fn person(&self, login: &str, confirm: bool) {
        let write = Grant::from_path_text("data.write", "db", "host:prod")
            .unwrap()
            .with_confirm(confirm);
        self.person_with(login, vec![write]);
    }

    /// A person with `access` on the service `db`, the right to use keys, and these grants.
    fn person_with(&self, login: &str, extra: Vec<Grant>) {
        let mut grants = vec![
            Grant::new("access", "db").unwrap(),
            Grant::parse("keys.own.use on service:portal/key_kind:mcp").unwrap(),
        ];
        grants.extend(extra);
        grants.sort();
        self.node
            .state
            .store
            .insert_user(&crate::store::UserRow {
                login: login.into(),
                password_hash: "h".into(),
                grants,
                created_at: 1,
            })
            .unwrap();
        self.node
            .state
            .users
            .refresh(&self.node.state.store)
            .unwrap();
    }
}

/// Protocol 17.8, spec question 51: the `confirm` of a grant of a person has no effect on the questions of the person.
/// The approval API never gets a person. The same `confirm` asks for the keys of the person.
#[test]
fn the_confirm_of_a_person_is_ignored_for_the_person_and_applies_to_the_keys_of_the_person() {
    each_fixture(true, |f, name| {
        f.person("carol", true);
        let e = effect(7);
        // The person: `true` with no effect, and `true` with an effect and a use. No `evaluation_id`, no reason.
        for (effect, usage) in [
            (None, false),
            (Some(e.as_str()), false),
            (Some(e.as_str()), true),
        ] {
            let reply = f.post(&f.body("carol", "analytics", effect, usage));
            assert_eq!(reply.status, 200, "[{name}] {}", reply.body);
            let answer = reply.json();
            assert_eq!(answer["decision"], true, "[{name}] {answer}");
            assert!(
                answer["context"]["evaluation_id"].is_null(),
                "[{name}] {answer}"
            );
            assert!(reason(&answer).is_none(), "[{name}] {answer}");
        }
        assert_eq!(
            f.metric("approval_required"),
            0,
            "[{name}] the gate never ran for the person"
        );
        // A key of carol: the grant of the owner asks for the key, whatever the grants of the key say.
        let plain = Grant::from_path_text("data.write", "db", ANALYTICS).unwrap();
        add_key(
            &f.node,
            "5a5a5a5a5a5a5a5a",
            Some("carol"),
            9,
            vec![Grant::new("access", "db").unwrap(), plain],
        );
        let answer = f.ask("key:5a5a5a5a5a5a5a5a", &e, false);
        assert_eq!(answer["decision"], false, "[{name}] {answer}");
        assert_eq!(reason(&answer), Some("approval_required"), "[{name}]");
        assert!(answer["context"]["evaluation_id"].is_string(), "[{name}]");
        // The same grant without `confirm`: the key never asks.
        f.person("dave", false);
        add_key(
            &f.node,
            "6b6b6b6b6b6b6b6b",
            Some("dave"),
            10,
            vec![
                Grant::new("access", "db").unwrap(),
                Grant::from_path_text("data.write", "db", ANALYTICS).unwrap(),
            ],
        );
        let answer = f.ask("key:6b6b6b6b6b6b6b6b", &e, false);
        assert_eq!(answer["decision"], true, "[{name}] {answer}");
    });
}

/// Protocol 17.8: `confirm` has no effect on `mcp.session`, as on `access`. A grant of the data that has it (a file
/// written by an older version) never makes `initialize` or `ping` fail.
#[test]
fn the_confirm_of_a_grant_of_mcp_session_has_no_effect() {
    each_fixture(true, |f, name| {
        f.person_with("erin", vec![Grant::new("mcp.session", "db").unwrap()]);
        let session = Grant::new("mcp.session", "db").unwrap().with_confirm(true);
        add_key(
            &f.node,
            "7c7c7c7c7c7c7c7c",
            Some("erin"),
            11,
            vec![Grant::new("access", "db").unwrap(), session],
        );
        let body = json!({
            "subject": {"type": "user", "id": "key:7c7c7c7c7c7c7c7c"},
            "action": {"name": "mcp.session"},
            "resource": {"type": "service", "id": "db"}
        })
        .to_string();
        let reply = f.post(&body);
        assert_eq!(reply.status, 200, "[{name}] {}", reply.body);
        let answer = reply.json();
        assert_eq!(answer["decision"], true, "[{name}] {answer}");
        assert!(reason(&answer).is_none(), "[{name}] {answer}");
    });
}

/// A condition of the owner that can change gives `false` with no reason. `unknown_subject` is for an orphan only.
#[test]
fn a_key_whose_owner_lost_the_right_to_use_keys_gets_false_with_no_reason() {
    each_fixture(true, |f, name| {
        let key = f.key("9b1c04e2aa71f0c3", 1, None);
        let e = effect(8);
        assert_eq!(
            f.ask(&key, &e, false)["context"]["reason"],
            "approval_required",
            "[{name}]"
        );
        // The owner loses `keys.own.use`.
        let without_use: Vec<Grant> = f
            .node
            .state
            .users
            .grants_of("alice")
            .unwrap()
            .into_iter()
            .filter(|g| g.action != "keys.own.use")
            .collect();
        f.node
            .state
            .store
            .update_user("alice", None, Some(&without_use))
            .unwrap();
        f.node.state.users.refresh(&f.node.state.store).unwrap();
        let answer = f.ask(&key, &e, false);
        assert_eq!(answer["decision"], false, "[{name}] {answer}");
        assert!(
            answer.get("context").is_none(),
            "[{name}] a plain false: {answer}"
        );
    });
}

/// Protocol 17.1: an `evaluation_id` has at most 1024 bytes. A question whose id would be longer gets `false` with no
/// reason: the PDP gives no approval for it.
#[test]
fn an_evaluation_id_above_1024_bytes_is_never_given() {
    each_fixture(true, |f, name| {
        let key = f.key("9b1c04e2aa71f0c3", 1, None);
        // A key with `confirm` on the whole host, so that any database is covered.
        add_key(
            &f.node,
            "8d8d8d8d8d8d8d8d",
            Some("alice"),
            12,
            vec![
                Grant::new("access", "db").unwrap(),
                Grant::from_path_text("data.write", "db", "host:prod")
                    .unwrap()
                    .with_confirm(true),
            ],
        );
        let wide = "key:8d8d8d8d8d8d8d8d";
        let e = effect(9);
        let normal = f.ask(wide, &e, false);
        let id = normal["context"]["evaluation_id"].as_str().unwrap();
        assert!(id.len() <= 1024, "[{name}] {}", id.len());
        // A database id of 256 bytes of a multi-byte character, percent-encoded in the sealed facts.
        let long = "\u{20ac}".repeat(85);
        let _ = &key;
        let reply = f.post(&f.body(wide, &long, Some(&e), false));
        assert_eq!(reply.status, 200, "[{name}] {}", reply.body);
        let answer = reply.json();
        assert_eq!(answer["decision"], false, "[{name}] {answer}");
        assert!(
            answer["context"]["evaluation_id"].is_null() && reason(&answer).is_none(),
            "[{name}] {answer}"
        );
    });
}

// ---------------------------------------------------------------- other permissions of an operation

/// A question with `context.permission_only` for a grant with `confirm`.
fn marked(f: &Fixture, subject: &str, context: Value) -> Reply {
    f.post(
        &json!({
            "subject": {"type": "user", "id": subject},
            "action": {"name": "data.write"},
            "resource": {"type": "database", "id": "analytics", "properties": {"ancestors": [
                {"type": "service", "id": "db"}, {"type": "host", "id": "prod"}]}},
            "context": context
        })
        .to_string(),
    )
}

/// The approval follows the action of the use alone (protocol 5.5a, 17.1). The question of another permission carries
/// `context.permission_only`: the PDP ignores `confirm` for it, with no reason of 17.1 and no `evaluation_id`.
#[test]
fn a_question_of_another_permission_never_asks_for_an_approval() {
    each_fixture(true, |f, name| {
        let key = f.key("9b1c04e2aa71f0c3", 1, None);
        // Without the marker: the grant has `confirm`, so the use asks for an approval.
        let plain = marked(f, &key, json!({})).json();
        assert_eq!(
            reason(&plain),
            Some("approval_required"),
            "[{name}] {plain}"
        );
        // With the marker: a plain `true`.
        let reply = marked(f, &key, json!({"permission_only": true}));
        assert_eq!(reply.status, 200, "[{name}] {}", reply.body);
        let answer = reply.json();
        assert_eq!(answer["decision"], true, "[{name}] {answer}");
        assert!(
            answer["context"]["reason"].is_null() && answer["context"]["evaluation_id"].is_null(),
            "[{name}] {answer}"
        );
        // The marker limits the scope: the key still needs the grant.
        add_key(
            &f.node,
            "3e3e3e3e3e3e3e3e",
            Some("alice"),
            20,
            vec![Grant::new("access", "db").unwrap()],
        );
        let none = marked(f, "key:3e3e3e3e3e3e3e3e", json!({"permission_only": true})).json();
        assert_eq!(none["decision"], false, "[{name}] {none}");
        assert!(none.get("context").is_none(), "[{name}] {none}");
        // The marker with a use, with an effect, or with another value is a bad request.
        for bad in [
            json!({"permission_only": true, "usage": {"units": 1}}),
            json!({"permission_only": true, "approval": {"effect": effect(1)}}),
            json!({"permission_only": false}),
            json!({"permission_only": "true"}),
        ] {
            let reply = marked(f, &key, bad.clone());
            assert_eq!(reply.status, 400, "[{name}] {bad}: {}", reply.body);
        }
        // In a batch, the item gets `invalid_request`.
        let item = |context: Value| {
            json!({
                "subject": {"type": "user", "id": key},
                "action": {"name": "data.write"},
                "resource": {"type": "database", "id": "analytics", "properties": {"ancestors": [
                    {"type": "service", "id": "db"}, {"type": "host", "id": "prod"}]}},
                "context": context
            })
        };
        let batch = json!({"evaluations": [item(json!({"permission_only": true})), item(json!({"permission_only": 1})), item(json!({}))]});
        let reply = f.post_to("/access/v1/evaluations", &batch.to_string());
        assert_eq!(reply.status, 200, "[{name}] {}", reply.body);
        let answers = reply.json()["evaluations"].clone();
        assert_eq!(answers[0]["decision"], true, "[{name}] {answers}");
        assert_eq!(
            answers[1]["context"]["reason"], "invalid_request",
            "[{name}] {answers}"
        );
        assert_eq!(
            answers[2]["context"]["reason"], "approval_required",
            "[{name}] {answers}"
        );
        // No request is ever made for the marked question: the approval API sees none.
        assert_eq!(
            f.metric("approval_required"),
            2,
            "[{name}] only the two unmarked questions"
        );
    });
}

// ---------------------------------------------------------------- a route that runs no action (protocol 10.1c)

/// Protocol 10.1c: a service asks `access` on itself for a route that runs no action, for example a static file. The
/// question has no usage and no approval. The PDP answers it from the grants of `access`, as for the edge. The action
/// `access` is not in the document, and `confirm` has no effect on it (17.8). So the answer never has a reason of 17.1,
/// and such a route never waits.
#[test]
fn a_service_asks_access_for_a_route_that_runs_no_action() {
    each_fixture(true, |f, name| {
        let ask = |subject: &str, context: Value| -> Value {
            let reply = f.post(
                &json!({
                    "subject": {"type": "user", "id": subject},
                    "action": {"name": "access"},
                    "resource": {"type": "service", "id": "db"},
                    "context": context
                })
                .to_string(),
            );
            assert_eq!(reply.status, 200, "[{name}] {subject}: {}", reply.body);
            reply.json()
        };
        let plain_true = |answer: &Value, what: &str| {
            assert_eq!(answer["decision"], true, "[{name}] {what}: {answer}");
            assert!(
                reason(answer).is_none() && answer["context"]["evaluation_id"].is_null(),
                "[{name}] {what}: {answer}"
            );
        };
        // A person with the grant `access`.
        plain_true(&ask("alice", json!({})), "a person with access");
        // A person whose grant of `access` has `confirm`, and a key of that person whose grant of `access` has it too.
        let mut grants = vec![
            Grant::new("access", "db").unwrap().with_confirm(true),
            Grant::parse("keys.own.use on service:portal/key_kind:mcp").unwrap(),
        ];
        grants.sort();
        f.node
            .state
            .store
            .insert_user(&crate::store::UserRow {
                login: "gina".into(),
                password_hash: "h".into(),
                grants,
                created_at: 1,
            })
            .unwrap();
        add_key(
            &f.node,
            "4f4f4f4f4f4f4f4f",
            Some("gina"),
            21,
            vec![Grant::new("access", "db").unwrap().with_confirm(true)],
        );
        plain_true(&ask("gina", json!({})), "a person whose access has confirm");
        // An effect on the question changes nothing: `access` never asks for an approval.
        for context in [json!({}), json!({"approval": {"effect": effect(3)}})] {
            plain_true(
                &ask("key:4f4f4f4f4f4f4f4f", context.clone()),
                &format!("a key whose access has confirm, context {context}"),
            );
        }
        // A key without `access`: a plain `false`.
        add_key(
            &f.node,
            "5e5e5e5e5e5e5e5e",
            Some("alice"),
            22,
            vec![Grant::from_path_text("data.write", "db", ANALYTICS).unwrap()],
        );
        let answer = ask("key:5e5e5e5e5e5e5e5e", json!({}));
        assert_eq!(answer["decision"], false, "[{name}] {answer}");
        assert!(reason(&answer).is_none(), "[{name}] {answer}");
        assert_eq!(
            f.metric("approval_required"),
            0,
            "[{name}] the gate never ran for access"
        );
    });
}
