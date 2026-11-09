//! Tests of the agents on SQLite and on PostgreSQL, with a clock that the test moves by hand (lot G1a).
//!
//! Each test runs on SQLite and, when `INBOX_TEST_PG` names a server, on PostgreSQL too. The rig is the one of
//! `items_tests.rs`. The tests cover the resolution, the limits and the flood bucket, the binding of a call to an
//! item, the actions of the owner, the lock of a new agent, the purge and the seal of the hints.

use std::time::Duration;

use inbox_core::proto::{Action, ItemState};

use super::agents::{
    AgentCall, AgentError, AgentKind, AgentMode, AgentRules, AgentState, AgentStatus, Hints,
    Resolved,
};
use super::agents_ops::{AgentOp, OpError};
use super::items::{Caller, Wait};
use super::items_ops::SupersedeOut;
use super::items_store::{start_checks, Answered, Created, Items, NewIntent};
use super::items_tests::{answer, approval, content, effect_of, envs, CANARY};
use super::store::Store;

const KEY1: &str = "key1";
const KEY2: &str = "key2";
const OWNER: &str = "alice";

fn discover() -> AgentCall {
    AgentCall {
        mode: AgentMode::Discover,
        ..Default::default()
    }
}

fn with_token(token: &str) -> AgentCall {
    AgentCall {
        mode: AgentMode::Discover,
        token: Some(token.to_string()),
        hints: Hints::default(),
    }
}

/// An agent made by a call with no token, for `key`. The result is its id and its token.
async fn make(items: &Items<'_>, key: &str) -> (String, String) {
    let r = items
        .resolve_agent(key, OWNER, &discover(), true)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(r.status, AgentStatus::New);
    (r.agent_id, r.token.unwrap().to_string())
}

/// The agent of a call with a token.
async fn who(
    items: &Items<'_>,
    key: &str,
    token: &str,
    creating: bool,
) -> Result<Resolved, AgentError> {
    items
        .resolve_agent(key, OWNER, &with_token(token), creating)
        .await
        .map(|r| r.expect("a token always gives an agent"))
}

async fn op(
    items: &Items<'_>,
    agent: &str,
    op: AgentOp,
) -> Result<super::agents_ops::OpDone, OpError> {
    items.agent_op(OWNER, agent, op, "web").await.unwrap()
}

/// An approval for `key` that belongs to `agent` (`None`: the bucket). The number makes the effect.
async fn item_of(items: &Items<'_>, n: u32, key: &str, agent: Option<&str>) -> String {
    let (mut item, body) = approval(n);
    item.requester_key_id = key.into();
    item.agent_id = agent.map(str::to_string);
    match items.create(item, &body).await.unwrap() {
        Created::New(l) => l.item.request_id.clone(),
        other => panic!("not created: {other:?}"),
    }
}

async fn agent_of(items: &Items<'_>, id: &str) -> String {
    items
        .get(id, &Caller::System)
        .await
        .unwrap()
        .unwrap()
        .item
        .agent_id
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn resolve_mode_key_returns_bucket_and_ignores_token() {
    for env in envs().await {
        let items = env.items();
        let call = AgentCall {
            mode: AgentMode::Key,
            token: Some("not a token at all".into()),
            hints: Hints::default(),
        };
        let first = items
            .resolve_agent(KEY1, OWNER, &call, true)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            (first.kind, first.status, first.state),
            (AgentKind::Bucket, AgentStatus::Key, AgentState::Known),
            "{}",
            env.name
        );
        assert!(first.token.is_none());
        let again = items
            .resolve_agent(KEY1, OWNER, &call, true)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(again.agent_id, first.agent_id, "one bucket for the key");
        // A call that makes no item gets no agent in this mode.
        assert!(items
            .resolve_agent(KEY1, OWNER, &call, false)
            .await
            .unwrap()
            .is_none());
        assert_eq!(env.count("SELECT COUNT(*) FROM agents").await, 1);
        // Another key has another bucket.
        let other = items
            .resolve_agent(KEY2, OWNER, &call, true)
            .await
            .unwrap()
            .unwrap();
        assert_ne!(other.agent_id, first.agent_id);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn without_the_block_every_mode_is_the_mode_key() {
    for mut env in envs().await {
        env.rules.agents = AgentRules::off();
        let items = env.items();
        let r = items
            .resolve_agent(KEY1, OWNER, &discover(), true)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            (r.kind, r.status),
            (AgentKind::Bucket, AgentStatus::Key),
            "{}",
            env.name
        );
        assert_eq!(
            env.count("SELECT COUNT(*) FROM agents WHERE kind = 'agent'")
                .await,
            0
        );
        // The lock never applies without the block.
        let (item, body) = approval(1);
        let Created::New(l) = items.create(item, &body).await.unwrap() else {
            panic!()
        };
        env.device("d1", OWNER).await;
        let effect = effect_of(&items, &l.item.request_id).await;
        let out = items
            .answer(&l.item.request_id, answer("d1", &effect, Action::Accept))
            .await
            .unwrap();
        assert!(matches!(out, Answered::Accepted { .. }), "{}", env.name);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn no_token_on_a_read_never_creates_an_agent() {
    for env in envs().await {
        let items = env.items();
        // No token and no item to make: no agent, no row, not even the bucket.
        assert!(items
            .resolve_agent(KEY1, OWNER, &discover(), false)
            .await
            .unwrap()
            .is_none());
        // A token of the right form that nobody holds: unknown, and nothing is written.
        let ghost = super::agents::new_token();
        let err = who(&items, KEY1, &ghost, false).await.unwrap_err();
        assert!(matches!(err, AgentError::Unknown), "{}", env.name);
        // A text that is not a token: unknown at once.
        let err = who(&items, KEY1, "garbage", false).await.unwrap_err();
        assert!(matches!(err, AgentError::Unknown));
        assert_eq!(env.count("SELECT COUNT(*) FROM agents").await, 0);
        // The mode `enrolled` refuses a creating call with no token, and makes nothing.
        let enrolled = AgentCall {
            mode: AgentMode::Enrolled,
            ..Default::default()
        };
        let err = items
            .resolve_agent(KEY1, OWNER, &enrolled, true)
            .await
            .unwrap_err();
        assert!(matches!(err, AgentError::TokenRequired));
        assert_eq!(env.count("SELECT COUNT(*) FROM agents").await, 0);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn resolve_unknown_foreign_key_and_foreign_owner_token_give_the_same_error() {
    for env in envs().await {
        let items = env.items();
        let (_, token) = make(&items, KEY1).await;
        let ghost = super::agents::new_token();
        let mut errors = Vec::new();
        errors.push(who(&items, KEY1, &ghost, true).await.unwrap_err());
        // The token of key1 used with key2.
        errors.push(who(&items, KEY2, &token, true).await.unwrap_err());
        // The token of alice used by bob, on the same key.
        errors.push(
            items
                .resolve_agent(KEY1, "bob", &with_token(&token), true)
                .await
                .unwrap_err(),
        );
        for e in &errors {
            assert_eq!(
                (e.code(), e.status()),
                ("agent_unknown", 422),
                "{}",
                env.name
            );
        }
        // The right key and owner still work.
        assert!(who(&items, KEY1, &token, true).await.is_ok());
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn resolve_blocked_merged_follows_alias_one_hop() {
    for env in envs().await {
        let items = env.items();
        let (a, token_a) = make(&items, KEY1).await;
        let (b, token_b) = make(&items, KEY1).await;
        let (d, _) = make(&items, KEY1).await;
        // A is merged into B: the token of A gives B, and the status follows the state of B.
        op(&items, &a, AgentOp::Merge { into: b.clone() })
            .await
            .unwrap();
        let r = who(&items, KEY1, &token_a, true).await.unwrap();
        assert_eq!(r.agent_id, b, "{}", env.name);
        assert_eq!(r.status, AgentStatus::New);
        // B is merged into D: A points at D at once, with no chain.
        op(&items, &b, AgentOp::Merge { into: d.clone() })
            .await
            .unwrap();
        assert_eq!(who(&items, KEY1, &token_a, true).await.unwrap().agent_id, d);
        assert_eq!(who(&items, KEY1, &token_b, true).await.unwrap().agent_id, d);
        assert_eq!(
            env.count(&format!(
                "SELECT COUNT(*) FROM agents WHERE merged_into = '{d}'"
            ))
            .await,
            2
        );
        // D is blocked: both tokens give `agent_blocked`.
        op(&items, &d, AgentOp::Block).await.unwrap();
        for token in [&token_a, &token_b] {
            let e = who(&items, KEY1, token, true).await.unwrap_err();
            assert_eq!((e.code(), e.status()), ("agent_blocked", 403));
        }
        // Unblock: the tokens work again.
        op(&items, &d, AgentOp::Unblock).await.unwrap();
        let r = who(&items, KEY1, &token_a, true).await.unwrap();
        assert_eq!((r.agent_id, r.status), (d, AgentStatus::Known));
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cap_and_rate_are_counted_in_the_transaction_two_writers() {
    for mut env in envs().await {
        env.rules.agents.new_per_hour = 3;
        env.rules.agents.max_per_key = 10;
        let items = env.items();
        let call = discover();
        let calls = (0..8).map(|_| items.resolve_agent(KEY1, OWNER, &call, true));
        let out = futures_util::future::join_all(calls).await;
        let statuses: Vec<_> = out
            .iter()
            .map(|r| r.as_ref().unwrap().as_ref().unwrap().status)
            .collect();
        let made = statuses.iter().filter(|s| **s == AgentStatus::New).count();
        let sent = statuses
            .iter()
            .filter(|s| **s == AgentStatus::Unattributed)
            .count();
        assert_eq!((made, sent), (3, 5), "{}: {statuses:?}", env.name);
        assert_eq!(
            env.count("SELECT COUNT(*) FROM agents WHERE kind = 'agent'")
                .await,
            3
        );
        // The hour passes: the rate is free again.
        env.advance(Duration::from_secs(3601));
        assert_eq!(
            items
                .resolve_agent(KEY1, OWNER, &discover(), true)
                .await
                .unwrap()
                .unwrap()
                .status,
            AgentStatus::New
        );
    }
    for mut env in envs().await {
        env.rules.agents.max_per_key = 2;
        env.rules.agents.new_per_hour = 100;
        let items = env.items();
        let call = discover();
        let calls = (0..6).map(|_| items.resolve_agent(KEY1, OWNER, &call, true));
        let out = futures_util::future::join_all(calls).await;
        let made = out
            .iter()
            .filter(|r| r.as_ref().unwrap().as_ref().unwrap().token.is_some())
            .count();
        assert_eq!(made, 2, "{}: the cap holds with two writers", env.name);
        // Another key has its own room.
        assert_eq!(make(&items, KEY2).await.0.len(), 32);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn over_cap_goes_to_bucket_without_token_and_alerts_once_per_hour() {
    for mut env in envs().await {
        env.rules.agents.max_per_key = 2;
        env.rules.agents.new_per_hour = 100;
        let items = env.items();
        make(&items, KEY1).await;
        make(&items, KEY1).await;
        let bucket = items.bucket(KEY1, OWNER).await.unwrap();
        for _ in 0..5 {
            let r = items
                .resolve_agent(KEY1, OWNER, &discover(), true)
                .await
                .unwrap()
                .unwrap();
            assert_eq!(
                (r.status, r.kind, r.token.is_none(), r.agent_id.as_str()),
                (
                    AgentStatus::Unattributed,
                    AgentKind::Bucket,
                    true,
                    bucket.agent_id.as_str()
                ),
                "{}: the call does not fail",
                env.name
            );
        }
        let alerts = "SELECT COUNT(*) FROM alerts WHERE idempotency_key LIKE 'agents-flood:key1:%'";
        assert_eq!(
            env.count(alerts).await,
            1,
            "{}: one alert in the hour",
            env.name
        );
        let events = format!(
            "SELECT COUNT(*) FROM agent_events WHERE agent_id = '{}' AND event = 'refused_cap'",
            bucket.agent_id
        );
        assert_eq!(env.count(&events).await, 1, "one event in the hour");
        // The next hour: one more alert and one more event.
        env.advance(Duration::from_secs(3601));
        items
            .resolve_agent(KEY1, OWNER, &discover(), true)
            .await
            .unwrap();
        assert_eq!(env.count(alerts).await, 2);
        assert_eq!(env.count(&events).await, 2);
        // The bucket is a real agent row of the key and it cannot be locked: its items are not `new`.
        assert_eq!(bucket.state, AgentState::Known);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn name_unique_on_nfkc_lowercase_inside_the_key_only() {
    for env in envs().await {
        let items = env.items();
        let (a, _) = make(&items, KEY1).await;
        let (b, _) = make(&items, KEY1).await;
        let (c, _) = make(&items, KEY2).await;
        let rename = |id: &str, name: &str| {
            let (id, name) = (id.to_string(), name.to_string());
            let items = &items;
            async move { op(items, &id, AgentOp::Rename { name }).await }
        };
        let done = rename(&a, "Billing bot").await.unwrap();
        assert!(done.changed);
        assert_eq!(done.agent.name.as_deref(), Some("Billing bot"));
        assert_eq!(done.agent.name_key.as_deref(), Some("billing bot"));
        for twin in ["billing bot", "BILLING  BOT", "ｂｉｌｌｉｎｇ ｂｏｔ"] {
            assert_eq!(
                rename(&b, twin).await.unwrap_err(),
                OpError::NameTaken,
                "{twin}"
            );
        }
        // The same name, on another key, is free.
        assert!(rename(&c, "Billing bot").await.is_ok());
        // A bad name is refused and never cleaned. The agent stays as it was.
        for bad in ["", "x\u{202E}y", "tab\there", " lead", &"n".repeat(33)] {
            assert!(
                matches!(rename(&b, bad).await.unwrap_err(), OpError::InvalidName(_)),
                "{bad:?}"
            );
        }
        let row = items.agent(&b).await.unwrap().unwrap();
        assert_eq!((row.name, row.rev), (None, 1), "nothing was written");
        // The same name again for the same agent: an idempotent success that writes nothing.
        let again = rename(&a, "Billing bot").await.unwrap();
        assert!(!again.changed);
        // A merged agent gives its name back to the key.
        let (d, _) = make(&items, KEY1).await;
        op(&items, &a, AgentOp::Merge { into: d }).await.unwrap();
        assert!(rename(&b, "Billing bot").await.is_ok(), "{}", env.name);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn ordinal_is_per_key_and_never_reused() {
    for mut env in envs().await {
        env.rules.agents.purge_after_ms = 1000;
        let items = env.items();
        let rows = |id: String| {
            let items = &items;
            async move { items.agent(&id).await.unwrap().unwrap().ordinal }
        };
        let (a1, _) = make(&items, KEY1).await;
        let (a2, _) = make(&items, KEY1).await;
        let (b1, _) = make(&items, KEY2).await;
        assert_eq!(
            (rows(a1).await, rows(a2.clone()).await, rows(b1).await),
            (1, 2, 1)
        );
        // The agent #2 is erased by the sweeper. The next agent of the key is #3, not #2.
        env.advance(Duration::from_secs(60));
        let report = items.maintain().await.unwrap();
        assert!(report.agents_purged >= 2, "{}: {report:?}", env.name);
        assert!(items.agent(&a2).await.unwrap().is_none());
        let (a3, _) = make(&items, KEY1).await;
        assert_eq!(rows(a3).await, 3, "{}", env.name);
        // The label of an agent that the owner did not name is portal text.
        let row = items.agent(&a2).await.unwrap();
        assert!(row.is_none());
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn item_cannot_name_an_agent_of_another_key() {
    for env in envs().await {
        let items = env.items();
        let (a1, _) = make(&items, KEY1).await;
        let (a2, _) = make(&items, KEY2).await;
        // The engine refuses an agent of another key, and an agent of another owner.
        let (mut item, body) = approval(1);
        item.requester_key_id = KEY2.into();
        item.agent_id = Some(a1.clone());
        assert!(matches!(
            items.create(item, &body).await.unwrap(),
            Created::BadAgent
        ));
        let (mut item, body) = approval(2);
        item.owner = "bob".into();
        item.agent_id = Some(a1.clone());
        assert!(matches!(
            items.create(item, &body).await.unwrap(),
            Created::BadAgent
        ));
        let (mut item, body) = approval(3);
        item.agent_id = Some("0".repeat(32));
        assert!(matches!(
            items.create(item, &body).await.unwrap(),
            Created::BadAgent
        ));
        assert_eq!(env.count("SELECT COUNT(*) FROM items").await, 0);
        // The database refuses it too, whatever the code does.
        let id = item_of(&items, 4, KEY1, Some(&a1)).await;
        let error = items
            .test_exec(&format!(
                "UPDATE items SET agent_id = '{a2}' WHERE request_id = '{id}'"
            ))
            .await
            .unwrap_err()
            .to_lowercase();
        // PostgreSQL gives its error without the text of the constraint: the refusal itself is the proof.
        assert!(
            env.name != "sqlite" || error.contains("foreign key"),
            "{}: {error}",
            env.name
        );
        let error = items
            .test_exec(&format!(
                "INSERT INTO approval_intents (ref_digest, requester_key_id, agent_id, body_size, expires_at)
                 VALUES ('d-fk', '{KEY1}', '{a2}', 0, 1)"
            ))
            .await
            .unwrap_err()
            .to_lowercase();
        assert!(
            env.name != "sqlite" || error.contains("foreign key"),
            "{}: {error}",
            env.name
        );
        assert_eq!(agent_of(&items, &id).await, a1);
        assert_eq!(env.count("SELECT COUNT(*) FROM approval_intents").await, 0);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn merge_moves_items_and_intents_and_keeps_the_alias() {
    for env in envs().await {
        let items = env.items();
        let (a, token_a) = make(&items, KEY1).await;
        let (b, _) = make(&items, KEY1).await;
        let i1 = item_of(&items, 1, KEY1, Some(&a)).await;
        let i2 = item_of(&items, 2, KEY1, Some(&a)).await;
        let i3 = item_of(&items, 3, KEY1, Some(&b)).await;
        for (digest, agent) in [("digest-a", &a), ("digest-b", &b)] {
            items
                .create_intent(NewIntent {
                    digest,
                    key_id: KEY1,
                    agent_id: agent,
                    wait: Wait::Lease { lease_s: 900 },
                    allow_custom: false,
                    allow_ask_back: false,
                    previous_id: None,
                    content: b"{}",
                    ttl_ms: 600_000,
                })
                .await
                .unwrap();
        }
        let named = op(
            &items,
            &b,
            AgentOp::Rename {
                name: "Night ETL".into(),
            },
        )
        .await
        .unwrap();
        let before = named.agent.rev;
        let done = op(&items, &a, AgentOp::Merge { into: b.clone() })
            .await
            .unwrap();
        assert!(done.changed);
        assert_eq!(
            (done.moved_items, done.agent.state),
            (2, AgentState::Merged),
            "{}",
            env.name
        );
        assert_eq!(done.agent.merged_into.as_deref(), Some(b.as_str()));
        let target = done.other.unwrap();
        assert_eq!(
            target.rev,
            before + 1,
            "the target changes too: a device must see it"
        );
        assert_eq!(
            target.name.as_deref(),
            Some("Night ETL"),
            "the name of the target stays"
        );
        for id in [&i1, &i2, &i3] {
            assert_eq!(agent_of(&items, id).await, b, "{}", env.name);
        }
        // The intents of A are B's now, so a submission that names one gives its item to B.
        let moved = items.read_intent("digest-a", KEY1).await.unwrap().unwrap();
        assert_eq!(moved.agent_id, b);
        // The token of A is an alias of B.
        assert_eq!(who(&items, KEY1, &token_a, true).await.unwrap().agent_id, b);
        // The events of both rows are written, with ids and numbers only.
        let at_b = items.agent_events(&b, 10).await.unwrap();
        assert!(
            at_b.iter()
                .any(|e| e.event == "merged_in" && e.actor == OWNER),
            "{at_b:?}"
        );
        let at_a = items.agent_events(&a, 10).await.unwrap();
        assert!(at_a.iter().any(|e| e.event == "merged_into"), "{at_a:?}");
        assert!(at_a.iter().chain(&at_b).all(|e| !e
            .detail
            .clone()
            .unwrap_or_default()
            .contains("canary")));
        // The same merge again is a success that writes nothing.
        let again = op(&items, &a, AgentOp::Merge { into: b.clone() })
            .await
            .unwrap();
        assert!(!again.changed);
        let rev = items.agent(&b).await.unwrap().unwrap().rev;
        assert_eq!(rev, target.rev, "an idempotent merge raises no rev");
        // A new item that names A (a call that raced the merge) goes to B.
        let late = item_of(&items, 5, KEY1, Some(&a)).await;
        assert_eq!(agent_of(&items, &late).await, b);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn merge_across_keys_refused() {
    for env in envs().await {
        let items = env.items();
        let (a, _) = make(&items, KEY1).await;
        let (c, _) = make(&items, KEY2).await;
        let item = item_of(&items, 1, KEY1, Some(&a)).await;
        assert_eq!(
            op(&items, &a, AgentOp::Merge { into: c.clone() })
                .await
                .unwrap_err(),
            OpError::CrossKey,
            "{}",
            env.name
        );
        // An agent of another owner is not found, never "cross key": the answer leaks nothing.
        assert_eq!(
            items
                .agent_op("bob", &a, AgentOp::Merge { into: c.clone() }, "web")
                .await
                .unwrap()
                .unwrap_err(),
            OpError::NotFound
        );
        // Into itself, into a merged agent and into an unknown agent.
        assert_eq!(
            op(&items, &a, AgentOp::Merge { into: a.clone() })
                .await
                .unwrap_err(),
            OpError::BadState
        );
        assert_eq!(
            op(
                &items,
                &a,
                AgentOp::Merge {
                    into: "0".repeat(32)
                }
            )
            .await
            .unwrap_err(),
            OpError::NotFound
        );
        let (d, _) = make(&items, KEY1).await;
        let (e, _) = make(&items, KEY1).await;
        op(&items, &d, AgentOp::Merge { into: e.clone() })
            .await
            .unwrap();
        assert_eq!(
            op(&items, &a, AgentOp::Merge { into: d })
                .await
                .unwrap_err(),
            OpError::BadState
        );
        // Nothing moved.
        assert_eq!(agent_of(&items, &item).await, a);
        assert_eq!(
            items.agent(&a).await.unwrap().unwrap().state,
            AgentState::New
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn bucket_cannot_be_named_merged_or_blocked() {
    for env in envs().await {
        let items = env.items();
        let (a, _) = make(&items, KEY1).await;
        let bucket = items.bucket(KEY1, OWNER).await.unwrap().agent_id;
        for action in [
            AgentOp::Rename {
                name: "Unattributed".into(),
            },
            AgentOp::Allow { name: None },
            AgentOp::Block,
            AgentOp::Unblock,
            AgentOp::Merge { into: a.clone() },
        ] {
            assert_eq!(
                op(&items, &bucket, action.clone()).await.unwrap_err(),
                OpError::BadState,
                "{}: {action:?}",
                env.name
            );
        }
        // Nothing merges into the bucket.
        assert_eq!(
            op(
                &items,
                &a,
                AgentOp::Merge {
                    into: bucket.clone()
                }
            )
            .await
            .unwrap_err(),
            OpError::BadState
        );
        let row = items.agent(&bucket).await.unwrap().unwrap();
        assert_eq!((row.state, row.name, row.rev), (AgentState::Known, None, 1));
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_actions_are_for_the_owner_and_each_one_is_idempotent() {
    for env in envs().await {
        let items = env.items();
        let (a, _) = make(&items, KEY1).await;
        // Another login finds nothing: the same answer as for an unknown id.
        for action in [
            AgentOp::Allow { name: None },
            AgentOp::Block,
            AgentOp::Rename { name: "x".into() },
        ] {
            assert_eq!(
                items
                    .agent_op("bob", &a, action, "web")
                    .await
                    .unwrap()
                    .unwrap_err(),
                OpError::NotFound,
                "{}",
                env.name
            );
        }
        assert_eq!(
            items.agent(&a).await.unwrap().unwrap().state,
            AgentState::New
        );
        // Allow, with a name. A second allow is a success with no change.
        let first = op(
            &items,
            &a,
            AgentOp::Allow {
                name: Some("Report writer".into()),
            },
        )
        .await
        .unwrap();
        assert!(first.changed);
        assert_eq!(first.agent.state, AgentState::Known);
        let second = op(
            &items,
            &a,
            AgentOp::Allow {
                name: Some("Report writer".into()),
            },
        )
        .await
        .unwrap();
        assert!(!second.changed);
        assert_eq!(second.agent.rev, first.agent.rev);
        // A name that is taken leaves a `new` agent as it was: no half action.
        let (b, _) = make(&items, KEY1).await;
        assert_eq!(
            op(
                &items,
                &b,
                AgentOp::Allow {
                    name: Some("report WRITER".into())
                }
            )
            .await
            .unwrap_err(),
            OpError::NameTaken
        );
        let row = items.agent(&b).await.unwrap().unwrap();
        assert_eq!((row.state, row.rev), (AgentState::New, 1));
        // Block, block again, unblock, unblock again.
        assert!(op(&items, &a, AgentOp::Block).await.unwrap().changed);
        assert!(!op(&items, &a, AgentOp::Block).await.unwrap().changed);
        assert!(op(&items, &a, AgentOp::Unblock).await.unwrap().changed);
        assert!(!op(&items, &a, AgentOp::Unblock).await.unwrap().changed);
        // A blocked agent can be named, so the owner can label it. It cannot be allowed: it is unblocked first.
        op(&items, &a, AgentOp::Block).await.unwrap();
        let named = op(
            &items,
            &a,
            AgentOp::Rename {
                name: "Old writer".into(),
            },
        )
        .await
        .unwrap();
        assert_eq!(
            (named.changed, named.agent.state),
            (true, AgentState::Blocked)
        );
        assert_eq!(
            op(&items, &a, AgentOp::Allow { name: None })
                .await
                .unwrap_err(),
            OpError::BadState
        );
        // The audit: one row for each change, with the login of the owner.
        let events = items.agent_events(&a, 20).await.unwrap();
        let names: Vec<_> = events.iter().rev().map(|e| e.event.as_str()).collect();
        assert_eq!(
            names,
            [
                "created",
                "allowed",
                "named",
                "blocked",
                "unblocked",
                "blocked",
                "renamed"
            ],
            "{}",
            env.name
        );
        assert!(events
            .iter()
            .filter(|e| e.event != "created")
            .all(|e| e.actor == OWNER));
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn block_cancels_open_items_with_reason_agent_blocked_and_kills_intents() {
    for env in envs().await {
        let items = env.items();
        let (a, token_a) = make(&items, KEY1).await;
        let (b, _) = make(&items, KEY1).await;
        let own = [
            item_of(&items, 1, KEY1, Some(&a)).await,
            item_of(&items, 2, KEY1, Some(&a)).await,
        ];
        let theirs = item_of(&items, 3, KEY1, Some(&b)).await;
        let intent = |digest: &'static str, agent: String| {
            let items = &items;
            async move {
                items
                    .create_intent(NewIntent {
                        digest,
                        key_id: KEY1,
                        agent_id: &agent,
                        wait: Wait::Lease { lease_s: 900 },
                        allow_custom: false,
                        allow_ask_back: false,
                        previous_id: None,
                        content: b"{\"explanation\":\"x\"}",
                        ttl_ms: 600_000,
                    })
                    .await
                    .unwrap();
            }
        };
        intent("live-a", a.clone()).await;
        intent("live-b", b.clone()).await;
        assert!(items.read_intent("live-a", KEY1).await.unwrap().is_some());
        let done = op(&items, &a, AgentOp::Block).await.unwrap();
        assert_eq!(
            (done.cancelled, done.killed_intents),
            (2, 1),
            "{}",
            env.name
        );
        for id in &own {
            let l = items.get(id, &Caller::System).await.unwrap().unwrap();
            assert_eq!(
                (l.item.state, l.item.settle_reason.as_deref()),
                (ItemState::Cancelled, Some("agent_blocked")),
                "{}",
                env.name
            );
        }
        // Another agent of the key is not touched.
        assert_eq!(env.state_of(&theirs).await, ItemState::Pending);
        assert!(items.read_intent("live-b", KEY1).await.unwrap().is_some());
        // The intent of the blocked agent is dead, as if the reference were bad.
        assert!(items.read_intent("live-a", KEY1).await.unwrap().is_none());
        assert_eq!(env.count("SELECT COUNT(*) FROM approval_intents WHERE ref_digest = 'live-a' AND body_ct IS NOT NULL").await, 0);
        // The token of the blocked agent gives `agent_blocked`, and a new item for it is refused.
        let e = who(&items, KEY1, &token_a, true).await.unwrap_err();
        assert_eq!(e.code(), "agent_blocked");
        let (mut racing, body) = approval(9);
        racing.agent_id = Some(a.clone());
        assert!(matches!(
            items.create(racing, &body).await.unwrap(),
            Created::AgentBlocked
        ));
        // Unblock revives nothing: the cancelled items stay cancelled.
        op(&items, &a, AgentOp::Unblock).await.unwrap();
        assert_eq!(env.state_of(&own[0]).await, ItemState::Cancelled);
        assert!(items.read_intent("live-a", KEY1).await.unwrap().is_none());
        assert!(who(&items, KEY1, &token_a, true).await.is_ok());
        // The cancel wrote the event of the item with the login of the owner.
        let events = env
            .count(&format!(
                "SELECT COUNT(*) FROM item_events WHERE request_id = '{}' AND event = 'cancelled' AND actor = 'user:{OWNER}'",
                own[0]
            ))
            .await;
        assert_eq!(events, 1);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn accept_of_a_new_agent_is_agent_locked_but_decline_and_reply_work() {
    for mut env in envs().await {
        env.rules.agents.lock_new = true;
        env.device("d1", OWNER).await;
        let items = env.items();
        let (a, _) = make(&items, KEY1).await;
        let first = item_of(&items, 1, KEY1, Some(&a)).await;
        let second = item_of(&items, 2, KEY1, Some(&a)).await;
        let effect = effect_of(&items, &first).await;
        // The accept is refused by the server. Nothing was written: the item is open.
        let out = items
            .answer(&first, answer("d1", &effect, Action::Accept))
            .await
            .unwrap();
        let Answered::AgentLocked(l) = out else {
            panic!("{}: {out:?}", env.name)
        };
        assert_eq!(l.item.state, ItemState::Pending);
        assert_eq!(env.state_of(&first).await, ItemState::Pending);
        assert_eq!(env.column(&first, "last_seq").await, 0);
        // A reply to the agent and a decline stay open.
        assert!(matches!(
            items
                .reply(&first, OWNER, Some("d1".into()), "m-1".into(), b"why this?")
                .await
                .unwrap(),
            super::items_ops::TurnOut::Stored { .. }
        ));
        let effect2 = effect_of(&items, &second).await;
        let out = items
            .answer(&second, answer("d1", &effect2, Action::Decline))
            .await
            .unwrap();
        assert!(
            matches!(out, Answered::Accepted { .. }),
            "{}: {out:?}",
            env.name
        );
        // A bucket item is never locked.
        let plain = item_of(&items, 3, KEY1, None).await;
        let effect3 = effect_of(&items, &plain).await;
        let out = items
            .answer(&plain, answer("d1", &effect3, Action::Accept))
            .await
            .unwrap();
        assert!(matches!(out, Answered::Accepted { .. }));
        // The owner allows the agent: the accept works.
        op(&items, &a, AgentOp::Allow { name: None }).await.unwrap();
        let out = items
            .answer(&first, answer("d1", &effect, Action::Accept))
            .await
            .unwrap();
        assert!(
            matches!(out, Answered::Accepted { .. }),
            "{}: {out:?}",
            env.name
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn lock_new_false_lets_a_new_agent_be_approved() {
    for mut env in envs().await {
        env.rules.agents.lock_new = false;
        env.device("d1", OWNER).await;
        let items = env.items();
        let (a, _) = make(&items, KEY1).await;
        let id = item_of(&items, 1, KEY1, Some(&a)).await;
        assert_eq!(
            items.agent(&a).await.unwrap().unwrap().state,
            AgentState::New
        );
        let effect = effect_of(&items, &id).await;
        let out = items
            .answer(&id, answer("d1", &effect, Action::Accept))
            .await
            .unwrap();
        assert!(
            matches!(out, Answered::Accepted { .. }),
            "{}: {out:?}",
            env.name
        );
        assert_eq!(env.state_of(&id).await, ItemState::Answered);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_chain_stays_inside_one_agent() {
    for env in envs().await {
        let items = env.items();
        let (a, _) = make(&items, KEY1).await;
        let (b, _) = make(&items, KEY1).await;
        let old = item_of(&items, 1, KEY1, Some(&a)).await;
        let supersede = |n: u32, agent: Option<&str>| {
            let (mut item, body) = approval(n);
            item.agent_id = agent.map(str::to_string);
            let (items, old) = (&items, old.clone());
            async move { items.supersede(&old, KEY1, item, &body).await.unwrap() }
        };
        // Another agent, and the bucket, cannot take the place of the item of A. Nothing is written.
        assert!(
            matches!(supersede(2, Some(&b)).await, SupersedeOut::AgentMismatch),
            "{}",
            env.name
        );
        assert!(matches!(
            supersede(3, None).await,
            SupersedeOut::AgentMismatch
        ));
        assert!(matches!(
            supersede(4, Some(&"0".repeat(32))).await,
            SupersedeOut::BadAgent
        ));
        assert_eq!(env.state_of(&old).await, ItemState::Pending);
        assert_eq!(env.count("SELECT COUNT(*) FROM items").await, 1);
        // The same agent can.
        let SupersedeOut::Created { new, .. } = supersede(5, Some(&a)).await else {
            panic!()
        };
        assert_eq!(new.item.agent_id, a);
        assert_eq!(env.state_of(&old).await, ItemState::Superseded);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_agent_of_a_new_item_is_the_target_of_a_call_before_any_write() {
    for mut env in envs().await {
        env.rules.agents.max_per_key = 1;
        let items = env.items();
        // Nothing exists: a call with no token would make an agent.
        assert_eq!(
            items.agent_target(KEY1, OWNER, &discover()).await.unwrap(),
            None
        );
        assert_eq!(
            env.count("SELECT COUNT(*) FROM agents").await,
            0,
            "{}",
            env.name
        );
        let (a, token) = make(&items, KEY1).await;
        assert_eq!(
            items
                .agent_target(KEY1, OWNER, &with_token(&token))
                .await
                .unwrap(),
            Some(a.clone())
        );
        // At the cap, the call would go to the bucket.
        let bucket = items.bucket(KEY1, OWNER).await.unwrap().agent_id;
        assert_eq!(
            items.agent_target(KEY1, OWNER, &discover()).await.unwrap(),
            Some(bucket.clone())
        );
        let e = items
            .agent_target(KEY1, OWNER, &with_token("garbage"))
            .await
            .unwrap_err();
        assert!(matches!(e, AgentError::Unknown));
        // The mode `key` always gives the bucket.
        let key_mode = AgentCall::default();
        assert_eq!(
            items.agent_target(KEY1, OWNER, &key_mode).await.unwrap(),
            Some(bucket)
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn per_key_reads_vs_per_agent_writes_at_engine_level() {
    for env in envs().await {
        let items = env.items();
        let (a, token_a) = make(&items, KEY1).await;
        let (b, token_b) = make(&items, KEY1).await;
        let of_a = item_of(&items, 1, KEY1, Some(&a)).await;
        let of_bucket = item_of(&items, 2, KEY1, None).await;
        let load = |id: &str| {
            let (items, id) = (&items, id.to_string());
            async move {
                items
                    .get(&id, &Caller::Agent(KEY1.into()))
                    .await
                    .unwrap()
                    .unwrap()
            }
        };
        let (la, lb) = (load(&of_a).await, load(&of_bucket).await);
        let ra = who(&items, KEY1, &token_a, false).await.unwrap();
        let rb = who(&items, KEY1, &token_b, false).await.unwrap();
        assert_eq!(
            (ra.agent_id.as_str(), rb.agent_id.as_str()),
            (a.as_str(), b.as_str())
        );
        let code = |r: Result<(), AgentError>| r.err().map(|e| e.code());
        // Reads go by the key: no token is allowed, a token must be the agent of the item.
        assert_eq!(
            code(items.check_item_agent(&la, None, false).await),
            None,
            "{}",
            env.name
        );
        assert_eq!(
            code(items.check_item_agent(&la, Some(&ra), false).await),
            None
        );
        assert_eq!(
            code(items.check_item_agent(&la, Some(&rb), false).await),
            Some("agent_mismatch")
        );
        // Writes go by the agent: the token is required on the item of an agent, and it must match.
        assert_eq!(
            code(items.check_item_agent(&la, None, true).await),
            Some("agent_token_required")
        );
        assert_eq!(
            code(items.check_item_agent(&la, Some(&rb), true).await),
            Some("agent_mismatch")
        );
        assert_eq!(
            code(items.check_item_agent(&la, Some(&ra), true).await),
            None
        );
        // The item of the bucket needs the key only.
        assert_eq!(code(items.check_item_agent(&lb, None, true).await), None);
        assert_eq!(
            code(items.check_item_agent(&lb, Some(&ra), true).await),
            None
        );
        assert_eq!(
            code(items.check_item_agent(&lb, Some(&ra), false).await),
            Some("agent_mismatch")
        );
        // A merge moves the item: the token of the merged agent is the agent of the item now.
        op(&items, &a, AgentOp::Merge { into: b.clone() })
            .await
            .unwrap();
        let la = load(&of_a).await;
        let ra = who(&items, KEY1, &token_a, false).await.unwrap();
        assert_eq!(
            code(items.check_item_agent(&la, Some(&ra), true).await),
            None
        );
        assert_eq!(
            code(items.check_item_agent(&la, Some(&rb), true).await),
            None
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_hints_are_sealed_updated_when_they_change_and_never_in_an_event() {
    for env in envs().await {
        let items = env.items();
        let first = AgentCall {
            mode: AgentMode::Discover,
            token: None,
            hints: Hints {
                says: Some(CANARY.into()),
                client: Some("claude-code/2.1".into()),
                user_agent: Some("node".into()),
                net: Some("203.0.113.0/24".into()),
            },
        };
        let made = items
            .resolve_agent(KEY1, OWNER, &first, true)
            .await
            .unwrap()
            .unwrap();
        let token = made.token.as_ref().unwrap().to_string();
        let row = items.agent(&made.agent_id).await.unwrap().unwrap();
        let seen = items.observed(&row).unwrap();
        assert_eq!(seen.says.as_deref(), Some(CANARY), "{}", env.name);
        assert_eq!((seen.nets_seen(), seen.says_changes), (1, 0));
        // The same hints with the token write nothing: the row keeps its rev.
        let same = AgentCall {
            token: Some(token.clone()),
            ..first.clone()
        };
        items.resolve_agent(KEY1, OWNER, &same, true).await.unwrap();
        assert_eq!(
            items.agent(&made.agent_id).await.unwrap().unwrap().rev,
            row.rev
        );
        // Another text and another range: the record changes, the rev rises, two events are written.
        let changed = AgentCall {
            token: Some(token.clone()),
            hints: Hints {
                says: Some("night-etl".into()),
                net: Some("198.51.100.0/24".into()),
                ..Default::default()
            },
            ..first.clone()
        };
        items
            .resolve_agent(KEY1, OWNER, &changed, true)
            .await
            .unwrap();
        let row2 = items.agent(&made.agent_id).await.unwrap().unwrap();
        assert_eq!(row2.rev, row.rev + 1);
        let seen2 = items.observed(&row2).unwrap();
        assert_eq!(seen2.says_first.as_deref(), Some(CANARY));
        assert_eq!(
            (seen2.says.as_deref(), seen2.says_changes, seen2.nets_seen()),
            (Some("night-etl"), 1, 2)
        );
        let events = items.agent_events(&made.agent_id, 10).await.unwrap();
        let names: Vec<_> = events.iter().map(|e| e.event.as_str()).collect();
        assert!(
            names.contains(&"says_changed") && names.contains(&"net_changed"),
            "{names:?}"
        );
        // A text with a hidden character is not kept.
        let hidden = AgentCall {
            token: Some(token),
            hints: Hints {
                says: Some("a\u{202E}b".into()),
                ..Default::default()
            },
            ..first
        };
        items
            .resolve_agent(KEY1, OWNER, &hidden, true)
            .await
            .unwrap();
        let row3 = items.agent(&made.agent_id).await.unwrap().unwrap();
        assert_eq!(
            items.observed(&row3).unwrap().says.as_deref(),
            Some("night-etl")
        );
        // The text is sealed: the column has no clear copy, and no event or detail has it.
        let find = match env.store {
            Store::Sqlite(_) => "instr(hints_ct, CAST('canary-8f3a91c7' AS BLOB)) > 0",
            Store::Pg(_) => "position('canary-8f3a91c7'::bytea in hints_ct) > 0",
        };
        assert_eq!(
            env.count(&format!("SELECT COUNT(*) FROM agents WHERE {find}"))
                .await,
            0
        );
        assert_eq!(
            env.count("SELECT COUNT(*) FROM agent_events WHERE detail LIKE '%canary%'")
                .await,
            0
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn rekey_and_key_purge_visit_the_hints() {
    for env in envs().await {
        let items = env.items();
        let call = AgentCall {
            mode: AgentMode::Discover,
            hints: Hints {
                says: Some("billing-bot".into()),
                ..Default::default()
            },
            ..Default::default()
        };
        let made = items
            .resolve_agent(KEY1, OWNER, &call, true)
            .await
            .unwrap()
            .unwrap();
        let row = items.agent(&made.agent_id).await.unwrap().unwrap();
        assert!(items.observed(&row).is_some());
        // A file that lost the key of the hints stops the start: the agents are counted with the other contents.
        let other = content(&[("k2", 2)], "k2");
        let error = start_checks(&env.store, &other).await.unwrap_err();
        assert!(error.contains("k1"), "{}: {error}", env.name);
        // `content-purge` erases the hints of that key and keeps the row.
        let (_, _, _, agents) = super::items_store::purge_key(&env.store, "k1")
            .await
            .unwrap();
        assert_eq!(agents, 1, "{}", env.name);
        let after = items.agent(&made.agent_id).await.unwrap().unwrap();
        assert!(after.hints.is_none() && items.observed(&after).is_none());
        assert!(start_checks(&env.store, &other).await.is_ok());
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn sweeper_purges_only_unreferenced_idle_agents() {
    for mut env in envs().await {
        env.rules.agents.max_per_key = 20;
        env.rules.agents.new_per_hour = 20;
        env.rules.agents.purge_after_ms = 60_000;
        env.rules.agents.idle_after_ms = 30_000;
        let items = env.items();
        let (with_item, _) = make(&items, KEY1).await;
        let (with_intent, _) = make(&items, KEY1).await;
        let (idle, _) = make(&items, KEY1).await;
        let (alias, _) = make(&items, KEY1).await;
        let (target, _) = make(&items, KEY1).await;
        op(
            &items,
            &alias,
            AgentOp::Merge {
                into: target.clone(),
            },
        )
        .await
        .unwrap();
        item_of(&items, 1, KEY1, Some(&with_item)).await;
        items
            .create_intent(NewIntent {
                digest: "keep",
                key_id: KEY1,
                agent_id: &with_intent,
                wait: Wait::Lease { lease_s: 900 },
                allow_custom: false,
                allow_ask_back: false,
                previous_id: None,
                content: b"{}",
                ttl_ms: 600_000,
            })
            .await
            .unwrap();
        // Two minutes later, with the intent still live. The idle agent goes, and so does the alias: nothing refers to
        // them. The target stays while its alias exists. The agent with an item and the agent with a live intent stay.
        env.advance(Duration::from_secs(120));
        let views = items.list_agents(OWNER, Some(KEY1)).await.unwrap();
        let idle_ids: Vec<_> = views
            .iter()
            .filter(|v| v.idle)
            .map(|v| v.agent.agent_id.clone())
            .collect();
        assert!(
            idle_ids.contains(&idle) && !idle_ids.contains(&with_item),
            "{}",
            env.name
        );
        let report = items.maintain().await.unwrap();
        assert_eq!(report.agents_purged, 2, "{}: {report:?}", env.name);
        for gone in [&idle, &alias] {
            assert!(items.agent(gone).await.unwrap().is_none(), "{gone}");
        }
        for kept in [&with_item, &with_intent, &target] {
            assert!(items.agent(kept).await.unwrap().is_some(), "{kept}");
        }
        assert_eq!(
            env.count(&format!(
                "SELECT COUNT(*) FROM agent_events WHERE agent_id = '{idle}'"
            ))
            .await,
            0,
            "the events go with the agent"
        );
        // The intent expires and is deleted: its agent goes, and the target goes now that its alias is gone.
        env.advance(Duration::from_secs(700));
        let report = items.maintain().await.unwrap();
        assert_eq!(report.agents_purged, 2, "{}: {report:?}", env.name);
        assert!(items.agent(&with_intent).await.unwrap().is_none());
        assert!(items.agent(&target).await.unwrap().is_none());
        // The agent with an item stays, and so does the bucket, while an agent of the key remains.
        assert!(items.agent(&with_item).await.unwrap().is_some());
        let bucket = items.bucket(KEY1, OWNER).await.unwrap();
        env.advance(Duration::from_secs(700));
        items.maintain().await.unwrap();
        assert!(
            items.agent(&bucket.agent_id).await.unwrap().is_some(),
            "{}",
            env.name
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_lists_count_open_items_and_the_events_come_newest_first() {
    for env in envs().await {
        let items = env.items();
        let (a, _) = make(&items, KEY1).await;
        let (c, _) = make(&items, KEY2).await;
        item_of(&items, 1, KEY1, Some(&a)).await;
        item_of(&items, 2, KEY1, Some(&a)).await;
        item_of(&items, 3, KEY1, None).await;
        let all = items.list_agents(OWNER, None).await.unwrap();
        let open = |id: &str| all.iter().find(|v| v.agent.agent_id == id).map(|v| v.open);
        assert_eq!(open(&a), Some(2), "{}", env.name);
        assert_eq!(open(&c), Some(0));
        let bucket = items.bucket(KEY1, OWNER).await.unwrap().agent_id;
        assert_eq!(open(&bucket), Some(1));
        // The list of an owner holds nothing of another owner, and a key filter keeps one key.
        assert!(items.list_agents("bob", None).await.unwrap().is_empty());
        let one = items.list_agents(OWNER, Some(KEY2)).await.unwrap();
        assert_eq!(
            one.iter()
                .filter(|v| v.agent.kind == AgentKind::Agent)
                .count(),
            1
        );
        // A new agent is not idle at once, and the bucket is never idle.
        assert!(all.iter().all(|v| !v.idle));
        env.advance(Duration::from_secs(15 * 86_400));
        let later = items.list_agents(OWNER, None).await.unwrap();
        let idle = |id: &str| {
            later
                .iter()
                .find(|v| v.agent.agent_id == id)
                .map(|v| v.idle)
        };
        assert_eq!(
            (idle(&a), idle(&c), idle(&bucket)),
            (Some(false), Some(true), Some(false))
        );
        // The events: the newest first.
        op(&items, &a, AgentOp::Allow { name: None }).await.unwrap();
        env.advance(Duration::from_secs(1));
        op(
            &items,
            &a,
            AgentOp::Rename {
                name: "Night ETL".into(),
            },
        )
        .await
        .unwrap();
        let events = items.agent_events(&a, 10).await.unwrap();
        assert_eq!(events[0].event, "named", "{}", env.name);
        assert_eq!(events.last().unwrap().event, "created");
        assert_eq!(items.agent_events(&a, 1).await.unwrap().len(), 1);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_metrics_count_the_agents_the_refusals_and_the_actions() {
    for mut env in envs().await {
        env.rules.agents.max_per_key = 1;
        let metrics = super::metrics::InboxMetrics::default();
        let mut items = env.items();
        items.metrics = Some(&metrics);
        let (a, token) = make(&items, KEY1).await;
        items
            .resolve_agent(KEY1, OWNER, &discover(), true)
            .await
            .unwrap();
        let _ = who(&items, KEY1, "garbage", true).await;
        op(&items, &a, AgentOp::Block).await.unwrap();
        let _ = who(&items, KEY1, &token, true).await;
        let text = metrics.render(None);
        for line in [
            "portal_agents_created_total{origin=\"call\"} 1",
            "portal_agent_refused_total{reason=\"cap\"} 1",
            "portal_agent_refused_total{reason=\"unknown\"} 1",
            "portal_agent_refused_total{reason=\"blocked\"} 1",
            "portal_agent_actions_total{op=\"block\",via=\"web\"} 1",
        ] {
            assert!(text.contains(line), "{}: {line}\n{text}", env.name);
        }
        // The token is in no metric.
        assert!(!text.contains(&token[4..]));
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn base_database_without_agents_table_stops_the_start() {
    for env in envs().await {
        assert!(
            start_checks(&env.store, &env.content).await.is_ok(),
            "{}",
            env.name
        );
        // A database that a build before the agents made has no table `agents`.
        let drop = match env.store {
            Store::Sqlite(_) => "DROP TABLE agent_events; DROP TABLE agents;",
            Store::Pg(_) => "DROP TABLE agents CASCADE;",
        };
        env.sql(drop).await;
        let error = start_checks(&env.store, &env.content).await.unwrap_err();
        assert!(
            error.contains("table agents") && error.contains("README, Upgrades"),
            "{}: {error}",
            env.name
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn last_seen_is_written_at_most_once_in_60_seconds() {
    for env in envs().await {
        let items = env.items();
        let (a, token) = make(&items, KEY1).await;
        let seen = || async { items.agent(&a).await.unwrap().unwrap().last_seen_at };
        let t0 = seen().await;
        env.advance(Duration::from_secs(30));
        who(&items, KEY1, &token, false).await.unwrap();
        assert_eq!(
            seen().await,
            t0,
            "{}: 30 seconds is inside the step",
            env.name
        );
        env.advance(Duration::from_secs(31));
        who(&items, KEY1, &token, false).await.unwrap();
        assert_eq!(seen().await, t0 + 61_000, "{}", env.name);
        // The touch never changes `rev`: a device does not need a new frame for a call.
        assert_eq!(items.agent(&a).await.unwrap().unwrap().rev, 1);
        // A blocked agent that calls is alive too: its row is not erased as idle.
        op(&items, &a, AgentOp::Block).await.unwrap();
        env.advance(Duration::from_secs(120));
        assert!(who(&items, KEY1, &token, true).await.is_err());
        assert_eq!(seen().await, t0 + 181_000, "{}", env.name);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn enrolled_mode_accepts_a_known_token_and_refuses_a_call_with_none() {
    for env in envs().await {
        let items = env.items();
        let (a, token) = make(&items, KEY1).await;
        let enrolled = |token: Option<&str>| AgentCall {
            mode: AgentMode::Enrolled,
            token: token.map(str::to_string),
            hints: Hints::default(),
        };
        let r = items
            .resolve_agent(KEY1, OWNER, &enrolled(Some(&token)), true)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            (r.agent_id.as_str(), r.token.is_none()),
            (a.as_str(), true),
            "{}",
            env.name
        );
        let e = items
            .resolve_agent(KEY1, OWNER, &enrolled(None), true)
            .await
            .unwrap_err();
        assert_eq!((e.code(), e.status()), ("agent_token_required", 422));
        // A read with no token is allowed in this mode: the item decides.
        assert!(items
            .resolve_agent(KEY1, OWNER, &enrolled(None), false)
            .await
            .unwrap()
            .is_none());
        assert_eq!(
            env.count("SELECT COUNT(*) FROM agents WHERE kind = 'agent'")
                .await,
            1,
            "the refusal made no agent"
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_items_of_one_agent_can_be_listed() {
    for env in envs().await {
        let items = env.items();
        let (a, _) = make(&items, KEY1).await;
        let (b, _) = make(&items, KEY1).await;
        item_of(&items, 1, KEY1, Some(&a)).await;
        item_of(&items, 2, KEY1, Some(&a)).await;
        item_of(&items, 3, KEY1, Some(&b)).await;
        let listed = |agent: &str, open: Option<bool>| {
            let filter = super::items_ops::ListFilter {
                agent_id: Some(agent.to_string()),
                open,
                limit: 10,
                ..Default::default()
            };
            let items = &items;
            async move { items.list(filter).await.unwrap().len() }
        };
        assert_eq!(
            (listed(&a, None).await, listed(&b, None).await),
            (2, 1),
            "{}",
            env.name
        );
        op(&items, &a, AgentOp::Block).await.unwrap();
        assert_eq!(
            (listed(&a, Some(true)).await, listed(&a, Some(false)).await),
            (0, 2)
        );
    }
}
