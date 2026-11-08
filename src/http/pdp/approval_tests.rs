//! The approval gate of the PDP over HTTP, with the inbox and its items on SQLite and on PostgreSQL.
//!
//! The conformance cases D76 to D89 and E19 prove the answers of the protocol (see `conformance_tests`). These tests
//! prove what the cases cannot: the single use of an approval under a race, the order consume, count, release, the
//! fault between the steps, the flag of the server, and what an answer holds.

use std::sync::atomic::Ordering;
use std::sync::{Arc, Barrier};

use inbox_core::proto::{Decision, ItemState};
use serde_json::{json, Value};

use super::approval::{self, Bound, InvalidId};
use crate::grants::Grant;
use crate::http::cluster_tests::*;
use crate::http::pdp_rig::{app_of, seed, InboxRig, Seed};
use crate::store::testing::{backends, Backend};

/// The service `db`: `data.write` can need an approval and counts the budget `requests`.
const DOCUMENT: &str = r#"{"server":"db",
  "resource_types":[{"name":"host","parent":"service","description":"A host."},
                    {"name":"database","parent":"host","description":"A database."}],
  "actions":[
    {"name":"data.read","resource_types":["host","database"],"description":"Read.","limits":["requests"]},
    {"name":"data.write","resource_types":["host","database"],"description":"Write.",
     "limits":["requests"],"approval":"optional"}],
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

/// A server with the inbox and the service `db`. `approvals` is the flag of the block `pdp_client`. The owner `alice`
/// has `access` and `data.write` on the host `prod`.
fn fixture(backend: &Backend, approvals: bool) -> Option<Fixture> {
    let rig = InboxRig::new(backend, "5m")?;
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
            pdp_client "db" {{ token_env = "TEST_PDP_DB" approvals = {approvals} }}
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
        pdp(
            self.port,
            "/access/v1/evaluation",
            Some(PDP_DB_TOKEN),
            body,
            None,
        )
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
fn budget_exceeded_keeps_approval_usable_release_path() {
    stay_in_a_minute();
    each_fixture(true, |f, name| {
        let key = f.key("9b1c04e2aa71f0c3", 1, Some(1));
        let (first, second, third) = (effect(1), effect(2), effect(3));
        for e in [&first, &second, &third] {
            f.approved(&key, e);
        }
        assert_eq!(f.ask(&key, &first, true)["decision"], true, "[{name}]");
        // The budget is spent. The approval of the second effect is consumed, then given back.
        let refused = f.ask(&key, &second, true);
        assert_eq!(refused["decision"], false, "[{name}] {refused}");
        assert_eq!(reason(&refused), Some("budget_exceeded"), "[{name}]");
        assert!(refused["context"]["retry_after"]
            .as_u64()
            .is_some_and(|s| s >= 1));
        assert_eq!(
            f.metric("released"),
            1,
            "[{name}] the approval was given back"
        );
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
        // A count that fails: HTTP 503, no decision, and the approval is given back.
        f.node.state.pdp_fault.store(2, Ordering::SeqCst);
        let failed = f.post(&f.body(&key, "analytics", Some(&third), true));
        f.node.state.pdp_fault.store(0, Ordering::SeqCst);
        assert_eq!(failed.status, 503, "[{name}] {}", failed.body);
        assert!(
            f.usable(&key, &third),
            "[{name}] given back after a failed count"
        );
    });
}

#[test]
fn crash_between_consume_and_count_never_double_executes() {
    stay_in_a_minute();
    each_fixture(true, |f, name| {
        let key = f.key("9b1c04e2aa71f0c3", 1, None);
        let e = effect(1);
        f.approved(&key, &e);
        // The process stops after the consumption: no count, no give-back. The service gets no decision (HTTP 503).
        f.node.state.pdp_fault.store(1, Ordering::SeqCst);
        let crashed = f.post(&f.body(&key, "analytics", Some(&e), true));
        f.node.state.pdp_fault.store(0, Ordering::SeqCst);
        assert_eq!(crashed.status, 503, "[{name}] {}", crashed.body);
        // The approval is lost: the agent asks again. It never gives a second execution.
        let mut executions = 0;
        for _ in 0..5 {
            let answer = f.ask(&key, &e, true);
            if answer["decision"] == true {
                executions += 1;
            }
            assert_eq!(
                reason(&answer),
                Some("approval_required"),
                "[{name}] {answer}"
            );
        }
        assert_eq!(executions, 0, "[{name}]");
        assert_eq!(
            reason(&f.ask(&key, &e, false)),
            Some("approval_required"),
            "[{name}]"
        );
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
fn confirm_with_client_without_approvals_flag_plain_false() {
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
        // The approval stays: no consumption without the flag.
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
