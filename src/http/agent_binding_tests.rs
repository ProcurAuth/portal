//! The binding of an item to its agent on the path of the PEP, with the real approval API, on SQLite and on PostgreSQL
//!. The agent calls `approval_intent` and gets an `approval_ref`. The service submits with that reference and
//! never sees the token, the id or the hints of the agent. A submission with no intent belongs to the bucket of the key.
//! A chain (`previous_id`, `supersedes`) stays inside one agent. A blocked agent has no live reference.

use super::*;
use crate::inbox::agents::{AgentCall, AgentKind, AgentMode, AgentStatus};
use crate::inbox::approvals::{ApprovalError, IntentOut};
use crate::inbox::testing::AgentOp;
use inbox_core::proto::ItemState;

/// The block of the agents. It closes the block `inbox.approvals` and opens the block `inbox.agents`.
const AGENTS: &str = "} agents { max_per_key = 3 new_per_hour = 10";

/// A key of the mode `discover`: a call with no token makes an agent.
fn discover(r: &Rig, subject: &str) -> Requester {
    let mut who = r.requester(subject, Interaction::Conversational);
    who.agent = AgentCall {
        mode: AgentMode::Discover,
        ..Default::default()
    };
    who
}

fn holding(who: &Requester, token: &str) -> Requester {
    let mut who = who.clone();
    who.agent.token = Some(token.to_string());
    who
}

/// An intent, tried again while the storage is away for a moment (the database of the test shares its server).
fn intent_out(r: &Rig, who: &Requester, input: IntentIn) -> Result<IntentOut, ApprovalError> {
    let mut result = runtime().block_on(approvals::intent(&r.app, who, input.clone()));
    for _ in 0..12 {
        if !matches!(&result, Err(e) if e.status == 503) {
            break;
        }
        std::thread::sleep(Duration::from_millis(400));
        result = runtime().block_on(approvals::intent(&r.app, who, input.clone()));
    }
    result
}

/// An intent that a new agent makes, and the item that the service makes with it. The result is the agent, its token and
/// the request id.
fn first_item(r: &Rig, who: &Requester, key: &str, n: u32) -> (String, String, String) {
    let out = intent_out(r, who, explained()).unwrap();
    assert_eq!(out.agent.status, AgentStatus::New);
    let token = out.agent.token.as_ref().expect("the token").to_string();
    let created = r.post(&r.body(
        key,
        &command(n),
        json!({ "approval_ref": out.approval_ref }),
    ));
    assert_eq!(created.status, 201, "{}", created.body);
    (out.agent.agent_id.clone(), token, created.id())
}

fn agent_rows(r: &Rig) -> i64 {
    r.count("SELECT COUNT(*) FROM agents WHERE kind = 'agent'")
}

#[test]
fn intent_with_token_gives_the_item_to_that_agent() {
    each_rig(true, AGENTS, |r, name| {
        r.device();
        let key = r.key_a();
        let who = discover(r, &key);
        let (agent, token, id) = first_item(r, &who, &key, 1);
        assert_eq!(r.loaded(&id).item.agent_id, agent, "[{name}]");
        // The next intent with the token: the same agent, no new token, and the item goes to it.
        let again = intent_out(r, &holding(&who, &token), explained()).unwrap();
        assert_eq!(
            (again.agent.agent_id.as_str(), again.agent.token.is_none()),
            (agent.as_str(), true),
            "[{name}]"
        );
        let second = r.post(&r.body(
            &key,
            &command(2),
            json!({ "approval_ref": again.approval_ref }),
        ));
        assert_eq!(r.loaded(&second.id()).item.agent_id, agent, "[{name}]");
        // A call with no token makes another agent, and its item is its own.
        let (other, other_token, other_id) = first_item(r, &who, &key, 3);
        assert_ne!(other, agent, "[{name}]");
        assert_eq!(r.loaded(&other_id).item.agent_id, other, "[{name}]");
        assert_ne!(other_token, token);
        assert_eq!(agent_rows(r), 2, "[{name}]");
        // The service never saw the token, the id of the agent or the hints: no answer holds them.
        for rec in r.log.lock().unwrap().iter() {
            for secret in [&token[4..], &other_token[4..], &agent[..], &other[..]] {
                assert!(
                    !rec.body.contains(secret)
                        && !rec
                            .headers
                            .iter()
                            .any(|(h, v)| h.contains(secret) || v.contains(secret)),
                    "[{name}] {} {} shows an agent value: {}",
                    rec.route,
                    rec.status,
                    rec.body
                );
            }
        }
        check_what_the_service_learned(r, name);
    });
}

#[test]
fn submission_without_intent_goes_to_the_bucket() {
    each_rig(true, AGENTS, |r, name| {
        let key = r.key_a();
        // A submission with `lease_s` and no reference: the bucket of the key. No agent is made.
        let id = r.submit(&key, 1).id();
        let item_agent = r.loaded(&id).item.agent_id;
        let row = runtime()
            .block_on(r.items().agent(&item_agent))
            .unwrap()
            .unwrap();
        assert_eq!(
            (row.kind, row.key_id.as_str()),
            (AgentKind::Bucket, key.trim_start_matches("key:")),
            "[{name}]"
        );
        assert_eq!(agent_rows(r), 0, "[{name}]");
        // A key in the mode `key` gets the bucket from the intent too, with the status `key`.
        let plain = r.requester(&key, Interaction::Conversational);
        let out = intent_out(r, &plain, explained()).unwrap();
        assert_eq!(
            (
                out.agent.status,
                out.agent.agent_id.as_str(),
                out.agent.token.is_none()
            ),
            (AgentStatus::Key, item_agent.as_str(), true),
            "[{name}]"
        );
        // Without the block `inbox.agents` the mode of the key does not matter either: see the engine tests.
        let id2 = r
            .post(&r.body(
                &key,
                &command(2),
                json!({ "approval_ref": out.approval_ref }),
            ))
            .id();
        assert_eq!(r.loaded(&id2).item.agent_id, item_agent, "[{name}]");
    });
}

#[test]
fn supersedes_across_agents_is_invalid_argument() {
    each_rig(true, AGENTS, |r, name| {
        r.device();
        let key = r.key_a();
        let who = discover(r, &key);
        let (agent_a, token_a, id_a) = first_item(r, &who, &key, 1);
        // The agent B (new) asks to replace the request of A: the PEP gets `invalid_argument`, and A keeps its request.
        let b = intent_out(r, &who, explained()).unwrap();
        let refused = r.post(&r.body(
            &key,
            &command(2),
            json!({ "approval_ref": b.approval_ref, "supersedes": id_a }),
        ));
        assert_eq!(
            refused.error(),
            (422, "invalid_argument".into()),
            "[{name}] {}",
            refused.body
        );
        assert_eq!(r.loaded(&id_a).item.state, ItemState::Pending, "[{name}]");
        assert_eq!(
            r.count("SELECT COUNT(*) FROM items"),
            1,
            "[{name}] nothing was made"
        );
        // A submission with no intent (the bucket) cannot replace it either.
        let plain = r.post(&r.body(
            &key,
            &command(3),
            json!({"lease_s": 900, "supersedes": id_a}),
        ));
        assert_eq!(plain.error(), (422, "invalid_argument".into()), "[{name}]");
        assert_eq!(r.loaded(&id_a).item.state, ItemState::Pending, "[{name}]");
        // The same agent can replace its own request.
        let own = intent_out(r, &holding(&who, &token_a), explained()).unwrap();
        let replaced = r.post(&r.body(
            &key,
            &command(4),
            json!({ "approval_ref": own.approval_ref, "supersedes": id_a }),
        ));
        assert_eq!(replaced.status, 201, "[{name}] {}", replaced.body);
        let new = r.loaded(&replaced.id());
        assert_eq!(new.item.agent_id, agent_a, "[{name}]");
        assert_eq!(
            r.loaded(&id_a).item.state,
            ItemState::Superseded,
            "[{name}]"
        );
    });
}

#[test]
fn previous_id_of_another_agent_is_refused() {
    each_rig(true, AGENTS, |r, name| {
        r.device();
        let key = r.key_a();
        let who = discover(r, &key);
        let (agent_a, token_a, id_a) = first_item(r, &who, &key, 1);
        r.answer(&id_a, Action::Decline, Some(COMMENT));
        let follow = |who: &Requester| {
            intent_out(
                r,
                who,
                IntentIn {
                    previous_id: Some(id_a.clone()),
                    ..explained()
                },
            )
        };
        // The agent A follows its own request.
        let ok = follow(&holding(&who, &token_a)).unwrap();
        assert_eq!(ok.agent.agent_id, agent_a, "[{name}]");
        // A call with no token would make a new agent, and a new agent has no thread: refused, and nothing is made.
        let rows = agent_rows(r);
        let (status, code) = match follow(&who) {
            Err(e) => (e.status, e.code),
            Ok(_) => panic!("[{name}] a new agent borrowed a thread"),
        };
        assert_eq!((status, code), (403, "agent_mismatch"), "[{name}]");
        assert_eq!(agent_rows(r), rows, "[{name}] the refusal made no agent");
        // Another agent that exists is refused too.
        let (_, token_b, _) = first_item(r, &who, &key, 2);
        let e = follow(&holding(&who, &token_b)).expect_err("refused");
        assert_eq!((e.status, e.code), (403, "agent_mismatch"), "[{name}]");
        // The bucket: a key in the mode `key` cannot follow the request of an agent, and can follow its own.
        let plain = r.requester(&key, Interaction::Conversational);
        let e = follow(&plain).expect_err("refused");
        assert_eq!((e.status, e.code), (403, "agent_mismatch"), "[{name}]");
        let own = r.submit(&key, 5).id();
        r.answer(&own, Action::Decline, None);
        let ok = intent_out(
            r,
            &plain,
            IntentIn {
                previous_id: Some(own),
                ..explained()
            },
        )
        .unwrap();
        assert_eq!(ok.agent.kind, AgentKind::Bucket, "[{name}]");
    });
}

#[test]
fn same_effect_by_a_second_agent_returns_the_first_item_and_records_also_requested() {
    each_rig(true, AGENTS, |r, name| {
        r.device();
        let key = r.key_a();
        let who = discover(r, &key);
        let (agent_a, _, id) = first_item(r, &who, &key, 1);
        // The agent B asks for the same effect while the request of A is open: it gets the request of A.
        let b = intent_out(r, &who, explained()).unwrap();
        let again = r.post(&r.body(&key, &command(1), json!({ "approval_ref": b.approval_ref })));
        assert_eq!(again.status, 200, "[{name}] {}", again.body);
        assert_eq!(again.id(), id, "[{name}]");
        assert_eq!(
            r.loaded(&id).item.agent_id,
            agent_a,
            "the item keeps its first agent"
        );
        let events = |agent: &str| {
            r.count(&format!(
                "SELECT COUNT(*) FROM item_events WHERE request_id = '{id}' AND event = 'also_requested' AND detail = 'agent={agent}'"
            ))
        };
        assert_eq!(events(&b.agent.agent_id), 1, "[{name}]");
        // Again, with the same agent: the event is written once.
        let token_b = b.agent.token.as_ref().unwrap().to_string();
        let b2 = intent_out(r, &holding(&who, &token_b), explained()).unwrap();
        r.post(&r.body(
            &key,
            &command(1),
            json!({ "approval_ref": b2.approval_ref }),
        ));
        assert_eq!(events(&b.agent.agent_id), 1, "[{name}]");
        // The same agent asks again: no event.
        let a_again = r.post(&r.body(&key, &command(1), json!({"lease_s": 900})));
        assert_eq!(a_again.id(), id, "[{name}]");
        let bucket = r.loaded(&id).item.agent_id;
        assert_ne!(bucket, b.agent.agent_id);
        // A submission with no intent is the bucket: the event names the bucket.
        let bucket_id = runtime()
            .block_on(r.items().bucket(key.trim_start_matches("key:"), OWNER))
            .unwrap()
            .agent_id;
        assert_eq!(events(&bucket_id), 1, "[{name}]");
        assert_eq!(
            events(&agent_a),
            0,
            "[{name}] the first agent is not 'also'"
        );
        // The page of the request reads the event: the agents that asked after the first one, once each, oldest first.
        let also: Vec<String> = eventually(|| runtime().block_on(r.items().also_requested_by(&id)))
            .into_iter()
            .map(|a| a.agent_id)
            .collect();
        assert_eq!(
            also,
            [b.agent.agent_id.clone(), bucket_id.clone()],
            "[{name}]"
        );
        assert!(
            eventually(|| runtime().block_on(r.items().also_requested_by(&"0".repeat(32))))
                .is_empty(),
            "[{name}] an item that does not exist has none"
        );
        // The service sees the same answer as for a repeat of its own request: the id and the code.
        check_what_the_service_learned(r, name);
    });
}

#[test]
fn blocked_agent_intent_ref_is_dead() {
    each_rig(true, AGENTS, |r, name| {
        r.device();
        let key = r.key_a();
        let who = discover(r, &key);
        let (agent, token, open_id) = first_item(r, &who, &key, 1);
        // A reference that A made and the service did not use yet.
        let live = intent_out(r, &holding(&who, &token), explained()).unwrap();
        let done = runtime()
            .block_on(r.items().agent_op(OWNER, &agent, AgentOp::Block, "web"))
            .unwrap()
            .unwrap();
        assert_eq!((done.cancelled, done.killed_intents), (1, 1), "[{name}]");
        // The open request of the agent is void for the service.
        assert_eq!(r.verdict(&open_id, &key), "void", "[{name}]");
        // The reference is dead: the same answer as for a bad reference.
        let dead = r.post(&r.body(
            &key,
            &command(2),
            json!({ "approval_ref": live.approval_ref }),
        ));
        assert_eq!(
            dead.error(),
            (422, "invalid_argument".into()),
            "[{name}] {}",
            dead.body
        );
        assert_eq!(
            r.count("SELECT COUNT(*) FROM items WHERE state = 'pending'"),
            0,
            "[{name}]"
        );
        // The agent gets `agent_blocked` at its next intent.
        let e = intent_out(r, &holding(&who, &token), explained()).expect_err("refused");
        assert_eq!((e.status, e.code), (403, "agent_blocked"), "[{name}]");
        // The same key with no token makes a new agent: the block is for the token.
        let (fresh, _, _) = first_item(r, &who, &key, 3);
        assert_ne!(fresh, agent, "[{name}]");
    });
}
