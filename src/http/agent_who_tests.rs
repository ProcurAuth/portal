//! Who is the agent: the token, the label and the hints on the agent channel, on SQLite and on PostgreSQL (lot G1b).
//!
//! Several agents share one key. The agent channel reads the token of an agent in the header `Agent-Token` or in the
//! argument `agent_token`, and the text that the agent says about itself in `agent_label`. The tests prove the six codes
//! of the table of the channel, the rule "a read goes by the key and a write goes by the agent", the flood that goes to
//! the bucket without an error, the label that is refused and never cleaned, the hints that the transport shows, and the
//! values that the service never sees.
//!
//! This module is a child of `agent_channel_tests`: it uses its rig and its helpers.

use super::*;
use crate::inbox::agents::AgentKind;
use crate::inbox::testing::AgentOp;

/// The block of the agents: three agents for a key, and no limit of the rate that the tests reach.
const BLOCKS: &str = "agents { max_per_key = 3 new_per_hour = 10 }";

fn each(test: impl Fn(&Rig, &str)) {
    each_agent_rig_with(BLOCKS, "", test);
}

// ---------------------------------------------------------------- the calls

/// The codes of a refusal that a loaded machine can give for a moment: the shared database answers late.
fn is_busy(code: Option<&str>) -> bool {
    matches!(
        code,
        Some("storage_degraded" | "unavailable" | "replication_unavailable")
    )
}

/// A tool call with more headers. The result is the flag `isError`, the structured content and the reply. A call that the
/// storage refused for a moment is tried again a few times: the database of the test shares its server.
fn call_h(
    r: &Rig,
    a: &Agent,
    extra: &[(&str, &str)],
    name: &str,
    args: Value,
) -> (bool, Value, Reply) {
    let auth = format!("Bearer {}", a.token);
    let mut headers = vec![
        ("Content-Type", "application/json"),
        ("Authorization", auth.as_str()),
    ];
    headers.extend_from_slice(extra);
    let body = json!({"jsonrpc": "2.0", "id": 7, "method": "tools/call",
                      "params": {"name": name, "arguments": args}});
    let mut attempt = 0;
    loop {
        let reply = send(r.node.port, "POST", "/mcp", &headers, &body.to_string());
        assert_eq!(reply.status, 200, "{}", reply.body);
        let result = &reply.json()["result"];
        let text = result["content"][0]["text"].as_str().expect("a text");
        let parsed: Value = serde_json::from_str(text).unwrap();
        assert_eq!(parsed, result["structuredContent"], "{text}");
        let busy = result["isError"] == true && is_busy(parsed["error"]["code"].as_str());
        attempt += 1;
        if busy && attempt < 12 {
            std::thread::sleep(Duration::from_millis(400));
            continue;
        }
        return (
            result["isError"] == true,
            result["structuredContent"].clone(),
            reply,
        );
    }
}

fn ok_h(r: &Rig, a: &Agent, extra: &[(&str, &str)], name: &str, args: Value) -> Value {
    let (is_error, value, _) = call_h(r, a, extra, name, args);
    assert!(!is_error, "{name}: {value}");
    value
}

fn err_h(r: &Rig, a: &Agent, extra: &[(&str, &str)], name: &str, args: Value) -> Value {
    let (is_error, value, _) = call_h(r, a, extra, name, args);
    assert!(is_error, "{name} must fail: {value}");
    value["error"].clone()
}

fn rest_h(
    r: &Rig,
    a: &Agent,
    extra: &[(&str, &str)],
    method: &str,
    path: &str,
    body: &str,
) -> Reply {
    let auth = format!("Bearer {}", a.token);
    let mut headers = vec![
        ("Content-Type", "application/json"),
        ("Authorization", auth.as_str()),
    ];
    headers.extend_from_slice(extra);
    let mut reply = send(r.node.port, method, path, &headers, body);
    for _ in 0..11 {
        if reply.status != 503 {
            break;
        }
        std::thread::sleep(Duration::from_millis(400));
        reply = send(r.node.port, method, path, &headers, body);
    }
    reply
}

fn intent_args() -> Value {
    json!({"lease_s": 900, "explanation": EXPLANATION})
}

fn with(mut args: Value, key: &str, value: Value) -> Value {
    args[key] = value;
    args
}

fn token_of(v: &Value) -> String {
    v["agent_token"].as_str().expect("a token").to_string()
}

fn agent_rows(r: &Rig) -> i64 {
    r.count("SELECT COUNT(*) FROM agents WHERE kind = 'agent'")
}

/// The item that the service makes with the reference of an intent.
fn item_of(r: &Rig, a: &Agent, n: u32, intent: &Value) -> String {
    let make = || {
        r.submit_with(
            &a.subject,
            n,
            json!({"approval_ref": intent["approval_ref"]}),
        )
    };
    let mut created = make();
    for _ in 0..11 {
        if created.status != 503 {
            break;
        }
        std::thread::sleep(Duration::from_millis(400));
        created = make();
    }
    assert_eq!(created.status, 201, "{}", created.body);
    created.id()
}

fn agent_of(r: &Rig, id: &str) -> String {
    r.loaded(id).item.agent_id
}

fn agent_row(r: &Rig, agent_id: &str) -> crate::inbox::agents::AgentRow {
    eventually(|| runtime().block_on(r.items().agent(agent_id))).expect("the agent")
}

/// What the portal remembers of the hints of an agent.
fn observed_of(r: &Rig, agent_id: &str) -> crate::inbox::agents::Observed {
    r.items()
        .observed(&agent_row(r, agent_id))
        .expect("the hints of the agent")
}

fn events_of(r: &Rig, agent_id: &str) -> Vec<String> {
    eventually(|| runtime().block_on(r.items().agent_events(agent_id, 50)))
        .into_iter()
        .map(|e| e.event)
        .collect()
}

fn owner_op(r: &Rig, agent_id: &str, op: AgentOp) {
    let done =
        eventually(|| runtime().block_on(r.items().agent_op(OWNER, agent_id, op.clone(), "web")))
            .unwrap();
    assert!(done.changed);
}

/// Changes the mode of a key as the owner does on the page of the keys.
fn set_mode(r: &Rig, id: &str, mode: AgentsMode) {
    let store = &r.node.state.store;
    let key = store
        .load_directory()
        .unwrap()
        .keys
        .into_iter()
        .find(|k| k.id == id)
        .expect("the key");
    store
        .update_key(
            id,
            Some(OWNER),
            &key.name,
            crate::store::KeyModes {
                interaction: key.interaction,
                agents: mode,
            },
            &key.grants,
            &key_caps(),
        )
        .unwrap();
    r.node.state.users.refresh(store).unwrap();
}

// ---------------------------------------------------------------- the groups and the token

#[test]
fn two_agents_with_one_key_have_two_groups() {
    each(|r, name| {
        r.fix_clock(T0);
        r.device();
        let k = conversational(r, ID_A);
        // A call with no token makes an agent, and the result gives its token one time.
        let one = ok_h(r, &k, &[], "approval_intent", intent_args());
        let two = ok_h(r, &k, &[], "approval_intent", intent_args());
        for v in [&one, &two] {
            assert_eq!(v["agent_status"], "new", "[{name}] {v}");
            let token = token_of(v);
            assert!(
                token.starts_with("agt_") && token.len() == 47,
                "[{name}] {token}"
            );
        }
        assert_ne!(token_of(&one), token_of(&two), "[{name}]");
        let id1 = item_of(r, &k, 1, &one);
        let id2 = item_of(r, &k, 2, &two);
        let (a1, a2) = (agent_of(r, &id1), agent_of(r, &id2));
        assert_ne!(a1, a2, "[{name}] two groups");
        assert_eq!(agent_rows(r), 2, "[{name}]");
        // The token in the argument: the same agent, and no new token.
        let again = ok_h(
            r,
            &k,
            &[],
            "approval_intent",
            with(intent_args(), "agent_token", one["agent_token"].clone()),
        );
        assert!(again.get("agent_token").is_none(), "[{name}] {again}");
        assert_eq!(again["agent_status"], "new", "[{name}]");
        assert_eq!(agent_of(r, &item_of(r, &k, 3, &again)), a1, "[{name}]");
        // The token in the header: the same agent.
        let token_two = token_of(&two);
        let by_header = ok_h(
            r,
            &k,
            &[("Agent-Token", token_two.as_str())],
            "approval_intent",
            intent_args(),
        );
        assert!(by_header.get("agent_token").is_none(), "[{name}]");
        assert_eq!(agent_of(r, &item_of(r, &k, 4, &by_header)), a2, "[{name}]");
        // The same value in the header and in the argument is one token.
        let both = ok_h(
            r,
            &k,
            &[("Agent-Token", token_two.as_str())],
            "approval_intent",
            with(intent_args(), "agent_token", json!(token_two)),
        );
        assert_eq!(both["agent_status"], "new", "[{name}]");
        // An empty argument is no token: the call makes an agent.
        let empty = ok_h(
            r,
            &k,
            &[],
            "approval_intent",
            with(intent_args(), "agent_token", json!("")),
        );
        assert!(empty.get("agent_token").is_some(), "[{name}] {empty}");
        assert_eq!(agent_rows(r), 3, "[{name}]");
        // The owner allows an agent: the next call says `known`.
        owner_op(r, &a1, AgentOp::Allow { name: None });
        let known = ok_h(
            r,
            &k,
            &[],
            "approval_intent",
            with(intent_args(), "agent_token", one["agent_token"].clone()),
        );
        assert_eq!(known["agent_status"], "known", "[{name}] {known}");
        assert!(known.get("agent_token").is_none(), "[{name}]");
    });
}

#[test]
fn agent_without_token_makes_a_new_agent_each_time_until_the_cap() {
    each(|r, name| {
        r.fix_clock(T0);
        r.device();
        let k = conversational(r, ID_A);
        for n in 1..=3u32 {
            let v = ok_h(r, &k, &[], "approval_intent", intent_args());
            assert_eq!(v["agent_status"], "new", "[{name}] {n}");
            assert!(v.get("agent_token").is_some(), "[{name}] {n}");
        }
        assert_eq!(agent_rows(r), 3, "[{name}]");
        // The fourth call is not an error: it goes to the bucket, with no token and the status `unattributed`.
        let flood = ok_h(r, &k, &[], "approval_intent", intent_args());
        assert_eq!(flood["agent_status"], "unattributed", "[{name}] {flood}");
        assert!(flood.get("agent_token").is_none(), "[{name}] {flood}");
        assert_eq!(agent_rows(r), 3, "[{name}] no fourth agent");
        let id = item_of(r, &k, 1, &flood);
        let row = agent_row(r, &agent_of(r, &id));
        assert_eq!(
            row.kind,
            AgentKind::Bucket,
            "[{name}] the item is in the bucket"
        );
        // The same on the REST door.
        let c = agent_in(
            r,
            ID_C,
            KeyKind::Api,
            KeyInteraction::Conversational,
            AgentsMode::Discover,
        );
        for n in 0..3 {
            let reply = rest_h(
                r,
                &c,
                &[],
                "POST",
                "/api/v1/approval-intents",
                &intent_args().to_string(),
            );
            // The cap counts the agents of one key: this key has its own.
            assert_eq!(reply.status, 201, "[{name}] {n} {}", reply.body);
        }
        let over = rest_h(
            r,
            &c,
            &[],
            "POST",
            "/api/v1/approval-intents",
            &intent_args().to_string(),
        );
        assert_eq!(over.status, 201, "[{name}] {}", over.body);
        assert_eq!(over.json()["agent_status"], "unattributed", "[{name}]");
        assert!(over.json().get("agent_token").is_none(), "[{name}]");
    });
}

#[test]
fn token_in_header_and_in_argument_must_agree() {
    each(|r, name| {
        r.fix_clock(T0);
        r.device();
        let k = conversational(r, ID_A);
        let one = ok_h(r, &k, &[], "approval_intent", intent_args());
        let other = ok_h(r, &k, &[], "approval_intent", intent_args());
        let (a, b) = (token_of(&one), token_of(&other));
        let rows = agent_rows(r);
        let intents = count(r, "approval_intents");
        // Two values that differ: the error, on each tool, and nothing is written.
        let e = err_h(
            r,
            &k,
            &[("Agent-Token", a.as_str())],
            "approval_intent",
            with(intent_args(), "agent_token", json!(b)),
        );
        assert_eq!(e["code"], "agent_token_conflict", "[{name}] {e}");
        assert_eq!(e["retryable"], false, "[{name}]");
        let id = item_of(r, &k, 1, &one);
        for (tool, extra) in [
            ("get_answer", json!({})),
            ("keepalive", json!({})),
            ("say", json!({"text": "hello"})),
            ("cancel", json!({})),
            ("supersede", json!({})),
        ] {
            let mut args = extra;
            args["request_id"] = json!(id);
            args["agent_token"] = json!(b);
            let e = err_h(r, &k, &[("Agent-Token", a.as_str())], tool, args);
            assert_eq!(e["code"], "agent_token_conflict", "[{name}] {tool}");
        }
        assert_eq!(agent_rows(r), rows, "[{name}]");
        assert_eq!(count(r, "approval_intents"), intents, "[{name}]");
        assert_eq!(
            r.loaded(&id).item.state,
            inbox_core::proto::ItemState::Pending
        );
        // Two headers with two values are a conflict too.
        let e = err_h(
            r,
            &k,
            &[("Agent-Token", a.as_str()), ("Agent-Token", b.as_str())],
            "get_answer",
            json!({"request_id": id}),
        );
        assert_eq!(e["code"], "agent_token_conflict", "[{name}]");
        // The REST door: the header and the body member. The status is 422.
        let c = agent_in(
            r,
            ID_C,
            KeyKind::Api,
            KeyInteraction::Conversational,
            AgentsMode::Discover,
        );
        let reply = rest_h(
            r,
            &c,
            &[("Agent-Token", a.as_str())],
            "POST",
            "/api/v1/approval-intents",
            &with(intent_args(), "agent_token", json!(b)).to_string(),
        );
        assert_eq!(reply.status, 422, "[{name}] {}", reply.body);
        assert_eq!(reply.json()["error"]["code"], "agent_token_conflict");
        let reply = rest_h(
            r,
            &c,
            &[("Agent-Token", a.as_str()), ("Agent-Token", b.as_str())],
            "GET",
            &format!("/api/v1/items/{id}"),
            "",
        );
        assert_eq!(reply.status, 422, "[{name}] {}", reply.body);
        assert_eq!(reply.json()["error"]["code"], "agent_token_conflict");
        // A key in the mode `key` has the same rule: the token is ignored, but a conflict is a fault of the client.
        let reply = rest_h(
            r,
            &agent(r, ID_B, KeyKind::Api, KeyInteraction::Conversational),
            &[("Agent-Token", a.as_str())],
            "POST",
            "/api/v1/approval-intents",
            &with(intent_args(), "agent_token", json!(b)).to_string(),
        );
        assert_eq!(reply.status, 422, "[{name}] {}", reply.body);
    });
}

#[test]
fn unknown_token_is_agent_unknown_and_the_same_for_every_cause() {
    each(|r, name| {
        r.fix_clock(T0);
        r.device();
        let a = conversational(r, ID_A);
        let b = conversational(r, ID_B);
        let mine = ok_h(r, &a, &[], "approval_intent", intent_args());
        let id = item_of(r, &a, 1, &mine);
        let foreign = token_of(&ok_h(r, &b, &[], "approval_intent", intent_args()));
        let rows = agent_rows(r);
        let unknown = crate::inbox::agents::new_token().to_string();
        // A token that nobody has, a token of another key, and a text that is not a token: one answer.
        let mut seen = Vec::new();
        for token in [
            unknown.as_str(),
            foreign.as_str(),
            "agt_short",
            "not-a-token",
            "AGT_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        ] {
            let e = err_h(
                r,
                &a,
                &[],
                "approval_intent",
                with(intent_args(), "agent_token", json!(token)),
            );
            assert_eq!(e["code"], "agent_unknown", "[{name}] {token}: {e}");
            assert_eq!(e["retryable"], false, "[{name}]");
            assert!(
                e["message"].as_str().unwrap().contains("approval_intent"),
                "[{name}] the message names the way out"
            );
            seen.push(e);
        }
        assert!(seen.iter().all(|e| *e == seen[0]), "[{name}] one answer");
        // Each tool gives the same error. The header gives it too.
        for (tool, extra) in [
            ("get_answer", json!({})),
            ("keepalive", json!({})),
            ("say", json!({"text": "hello"})),
            ("cancel", json!({})),
            ("supersede", json!({})),
        ] {
            let mut args = extra;
            args["request_id"] = json!(id);
            args["agent_token"] = json!(foreign);
            assert_eq!(
                err_h(r, &a, &[], tool, args.clone()),
                seen[0],
                "[{name}] {tool}"
            );
            args.as_object_mut().unwrap().remove("agent_token");
            assert_eq!(
                err_h(r, &a, &[("Agent-Token", unknown.as_str())], tool, args),
                seen[0],
                "[{name}] {tool} by the header"
            );
        }
        // A garbage token never makes a row: only the two real agents exist.
        assert_eq!(agent_rows(r), rows, "[{name}]");
        // The REST door: 422 with the same code.
        let c = agent_in(
            r,
            ID_C,
            KeyKind::Api,
            KeyInteraction::Conversational,
            AgentsMode::Discover,
        );
        let reply = rest_h(
            r,
            &c,
            &[("Agent-Token", foreign.as_str())],
            "POST",
            "/api/v1/approval-intents",
            &intent_args().to_string(),
        );
        assert_eq!(reply.status, 422, "[{name}] {}", reply.body);
        assert_eq!(reply.json()["error"]["code"], "agent_unknown");
        // The item of the agent is still pending, and the reads of the key still work with no token.
        assert_eq!(
            ok_h(r, &a, &[], "get_answer", json!({"request_id": id}))["status"],
            "pending"
        );
    });
}

#[test]
fn blocked_agent_gets_agent_blocked_on_every_tool() {
    each(|r, name| {
        r.fix_clock(T0);
        r.device();
        let k = conversational(r, ID_A);
        let one = ok_h(
            r,
            &k,
            &[],
            "approval_intent",
            with(intent_args(), "allow_ask_back", json!(true)),
        );
        let token = token_of(&one);
        let id = item_of(r, &k, 1, &one);
        let agent_id = agent_of(r, &id);
        owner_op(r, &agent_id, AgentOp::Block);
        // The call of the blocked agent is refused, whatever the tool: the header or the argument.
        let e = err_h(
            r,
            &k,
            &[],
            "approval_intent",
            with(intent_args(), "agent_token", json!(token)),
        );
        assert_eq!(
            (e["code"].as_str(), &e["retryable"]),
            (Some("agent_blocked"), &json!(false)),
            "[{name}] {e}"
        );
        for (tool, extra) in [
            ("get_answer", json!({})),
            ("keepalive", json!({})),
            ("say", json!({"text": "hello"})),
            ("cancel", json!({})),
            ("supersede", json!({})),
        ] {
            let mut args = extra;
            args["request_id"] = json!(id);
            let e = err_h(r, &k, &[("Agent-Token", token.as_str())], tool, args);
            assert_eq!(e["code"], "agent_blocked", "[{name}] {tool}");
        }
        // The REST door gives 403.
        let c = agent_in(
            r,
            ID_C,
            KeyKind::Api,
            KeyInteraction::Conversational,
            AgentsMode::Discover,
        );
        let made = rest_h(
            r,
            &c,
            &[],
            "POST",
            "/api/v1/approval-intents",
            &intent_args().to_string(),
        );
        let c_token = made.json()["agent_token"].as_str().unwrap().to_string();
        owner_op(
            r,
            &agent_of(r, &item_of(r, &c, 2, &made.json())),
            AgentOp::Block,
        );
        let reply = rest_h(
            r,
            &c,
            &[("Agent-Token", c_token.as_str())],
            "POST",
            "/api/v1/approval-intents",
            &intent_args().to_string(),
        );
        assert_eq!(reply.status, 403, "[{name}] {}", reply.body);
        assert_eq!(reply.json()["error"]["code"], "agent_blocked");
        // The key with no token is not blocked: a call makes a new agent. The block is for the token.
        let fresh = ok_h(r, &k, &[], "approval_intent", intent_args());
        assert_eq!(fresh["agent_status"], "new", "[{name}]");
        // A read with no token still works, and it tells the item was cancelled by the block.
        let read = ok_h(r, &k, &[], "get_answer", json!({"request_id": id}));
        assert_eq!(read["status"], "cancelled", "[{name}] {read}");
        assert_eq!(read["reason"], "agent_blocked", "[{name}] {read}");
    });
}

#[test]
fn enrolled_mode_refuses_a_call_without_token() {
    each(|r, name| {
        r.fix_clock(T0);
        r.device();
        let k = conversational(r, ID_A);
        let made = ok_h(r, &k, &[], "approval_intent", intent_args());
        let token = token_of(&made);
        let id = item_of(r, &k, 1, &made);
        // The owner changes the mode of the key: the portal reads it at once from its directory.
        set_mode(r, ID_A, AgentsMode::Enrolled);
        let rows = agent_rows(r);
        let e = err_h(r, &k, &[], "approval_intent", intent_args());
        assert_eq!(e["code"], "agent_token_required", "[{name}] {e}");
        assert_eq!(e["retryable"], false, "[{name}]");
        assert_eq!(agent_rows(r), rows, "[{name}] no agent is made");
        // The REST door gives 422.
        // With a valid token: the call works, in the header and in the argument.
        let by_header = ok_h(
            r,
            &k,
            &[("Agent-Token", token.as_str())],
            "approval_intent",
            intent_args(),
        );
        assert!(by_header.get("agent_token").is_none(), "[{name}]");
        assert_eq!(
            agent_of(r, &item_of(r, &k, 2, &by_header)),
            agent_of(r, &id),
            "[{name}]"
        );
        let by_argument = ok_h(
            r,
            &k,
            &[],
            "approval_intent",
            with(intent_args(), "agent_token", json!(token)),
        );
        assert!(by_argument.get("agent_token").is_none(), "[{name}]");
        // A token that is not known is `agent_unknown`, and a read with no token is allowed.
        let e = err_h(
            r,
            &k,
            &[],
            "approval_intent",
            with(
                intent_args(),
                "agent_token",
                json!(crate::inbox::agents::new_token().to_string()),
            ),
        );
        assert_eq!(e["code"], "agent_unknown", "[{name}]");
        assert_eq!(
            ok_h(r, &k, &[], "get_answer", json!({"request_id": id}))["status"],
            "pending"
        );
        // The mode `key` ignores the token and gives the bucket.
        set_mode(r, ID_A, AgentsMode::Key);
        let plain = ok_h(r, &k, &[], "approval_intent", intent_args());
        assert_eq!(plain["agent_status"], "key", "[{name}] {plain}");
        assert!(plain.get("agent_token").is_none(), "[{name}]");
        let ignored = ok_h(
            r,
            &k,
            &[],
            "approval_intent",
            with(intent_args(), "agent_token", json!(token)),
        );
        assert_eq!(ignored["agent_status"], "key", "[{name}]");
        let bucket = item_of(r, &k, 3, &ignored);
        assert_eq!(
            agent_row(r, &agent_of(r, &bucket)).kind,
            AgentKind::Bucket,
            "[{name}]"
        );
        // The REST door, for a key of the kind api in the mode enrolled.
        let c = agent_in(
            r,
            ID_C,
            KeyKind::Api,
            KeyInteraction::Conversational,
            AgentsMode::Enrolled,
        );
        let reply = rest_h(
            r,
            &c,
            &[],
            "POST",
            "/api/v1/approval-intents",
            &intent_args().to_string(),
        );
        assert_eq!(reply.status, 422, "[{name}] {}", reply.body);
        assert_eq!(reply.json()["error"]["code"], "agent_token_required");
    });
}

#[test]
fn a_key_of_the_mode_key_and_a_file_without_the_block_give_the_bucket() {
    // An `api_` key has the mode `key` by default: the status is `key` and the result has no token.
    each(|r, name| {
        r.fix_clock(T0);
        r.device();
        let c = agent(r, ID_C, KeyKind::Api, KeyInteraction::Conversational);
        let reply = rest_h(
            r,
            &c,
            &[],
            "POST",
            "/api/v1/approval-intents",
            &intent_args().to_string(),
        );
        assert_eq!(reply.status, 201, "[{name}] {}", reply.body);
        assert_eq!(reply.json()["agent_status"], "key", "[{name}]");
        assert!(reply.json().get("agent_token").is_none(), "[{name}]");
        assert_eq!(agent_rows(r), 0, "[{name}]");
        let id = item_of(r, &c, 1, &reply.json());
        assert_eq!(
            agent_row(r, &agent_of(r, &id)).kind,
            AgentKind::Bucket,
            "[{name}]"
        );
    });
    // Without the block `inbox.agents`, a key of the mode `discover` acts as the mode `key`.
    each_agent_rig("", |r, name| {
        r.fix_clock(T0);
        r.device();
        let k = conversational(r, ID_A);
        let v = ok_h(r, &k, &[], "approval_intent", intent_args());
        assert_eq!(v["agent_status"], "key", "[{name}] {v}");
        assert!(v.get("agent_token").is_none(), "[{name}]");
        assert_eq!(agent_rows(r), 0, "[{name}]");
    });
}

#[test]
fn the_authorizer_gives_the_mode_of_a_key_to_the_frames() {
    use crate::inbox::agents::AgentMode;
    each(|r, name| {
        let k = conversational(r, ID_A);
        let c = agent(r, ID_C, KeyKind::Api, KeyInteraction::OneShot);
        let authorizer = r.app.authorizer().expect("the authorizer");
        let mode = |key: &str| authorizer.key_agent_mode(key);
        assert_eq!(
            mode(ID_A),
            Some(AgentMode::Discover),
            "[{name}] {}",
            k.subject
        );
        assert_eq!(mode(ID_C), Some(AgentMode::Key), "[{name}] {}", c.subject);
        // A key that does not exist has none: the frames then treat it as the mode `key`.
        assert_eq!(mode(ID_B), None, "[{name}]");
        // The owner changes the mode: the portal reads it at once.
        set_mode(r, ID_A, AgentsMode::Enrolled);
        assert_eq!(mode(ID_A), Some(AgentMode::Enrolled), "[{name}]");
        set_mode(r, ID_C, AgentsMode::Discover);
        assert_eq!(mode(ID_C), Some(AgentMode::Discover), "[{name}]");
    });
}

// ---------------------------------------------------------------- reads by the key, writes by the agent

#[test]
fn read_without_token_is_allowed_read_with_wrong_token_is_agent_mismatch() {
    each(|r, name| {
        r.fix_clock(T0);
        r.device();
        let k = conversational(r, ID_A);
        let one = ok_h(r, &k, &[], "approval_intent", intent_args());
        let two = ok_h(r, &k, &[], "approval_intent", intent_args());
        let id = item_of(r, &k, 1, &one);
        let seen_before = r.loaded(&id).item.agent_seen_at;
        // A read with no token, and a read with the token of the agent of the item.
        for extra in [
            vec![],
            vec![("Agent-Token", one["agent_token"].as_str().unwrap())],
        ] {
            for (tool, args) in [
                ("get_answer", json!({"request_id": id})),
                ("keepalive", json!({"request_id": id})),
            ] {
                let v = ok_h(r, &k, &extra, tool, args);
                assert_eq!(v["status"], "pending", "[{name}] {tool}");
            }
        }
        // A token that is the agent of another item is a mismatch, and the renewal did not happen.
        r.fix_clock(T0 + 2_000_000);
        for tool in ["get_answer", "keepalive"] {
            let e = err_h(
                r,
                &k,
                &[("Agent-Token", two["agent_token"].as_str().unwrap())],
                tool,
                json!({"request_id": id}),
            );
            assert_eq!(e["code"], "agent_mismatch", "[{name}] {tool}");
            assert_eq!(e["retryable"], false, "[{name}]");
        }
        assert_eq!(
            r.loaded(&id).item.agent_seen_at,
            T0,
            "[{name}] a refused read renews nothing (before: {seen_before})"
        );
        // The REST door: 403 for the token of another agent, and 200 with no token or with its own.
        let c = agent_in(
            r,
            ID_C,
            KeyKind::Api,
            KeyInteraction::Conversational,
            AgentsMode::Discover,
        );
        let first = rest_h(
            r,
            &c,
            &[],
            "POST",
            "/api/v1/approval-intents",
            &intent_args().to_string(),
        );
        let own_token = first.json()["agent_token"].as_str().unwrap().to_string();
        let own = item_of(r, &c, 2, &first.json());
        let other_token = ok_h_rest(r, &c, "POST", "/api/v1/approval-intents");
        let path = format!("/api/v1/items/{own}?wait=0");
        let reply = rest_h(
            r,
            &c,
            &[("Agent-Token", other_token.as_str())],
            "GET",
            &path,
            "",
        );
        assert_eq!(reply.status, 403, "[{name}] {}", reply.body);
        assert_eq!(reply.json()["error"]["code"], "agent_mismatch");
        for extra in [vec![], vec![("Agent-Token", own_token.as_str())]] {
            let reply = rest_h(r, &c, &extra, "GET", &path, "");
            assert_eq!(reply.status, 200, "[{name}] {}", reply.body);
        }
    });
}

/// A second agent of the key, made through the REST door. The result is its token.
fn ok_h_rest(r: &Rig, a: &Agent, method: &str, path: &str) -> String {
    let reply = rest_h(r, a, &[], method, path, &intent_args().to_string());
    assert_eq!(reply.status, 201, "{}", reply.body);
    reply.json()["agent_token"].as_str().unwrap().to_string()
}

#[test]
fn per_key_reads_vs_per_agent_writes() {
    each(|r, name| {
        r.fix_clock(T0);
        r.device();
        let k = conversational(r, ID_A);
        let flags = json!({"allow_ask_back": true});
        let one = ok_h(
            r,
            &k,
            &[],
            "approval_intent",
            with(
                intent_args(),
                "allow_ask_back",
                flags["allow_ask_back"].clone(),
            ),
        );
        let two = ok_h(r, &k, &[], "approval_intent", intent_args());
        let (ta, tb) = (token_of(&one), token_of(&two));
        let id = item_of(r, &k, 1, &one);
        // Another agent of the same key reads the item with no token, but it cannot write: it has no token, or it has
        // the token of another agent.
        assert_eq!(
            ok_h(r, &k, &[], "get_answer", json!({"request_id": id}))["status"],
            "pending"
        );
        for (tool, args) in [
            ("say", json!({"request_id": id, "text": "I am the agent A"})),
            ("cancel", json!({"request_id": id})),
            ("supersede", json!({"request_id": id})),
        ] {
            let e = err_h(r, &k, &[], tool, args.clone());
            assert_eq!(
                e["code"], "agent_token_required",
                "[{name}] {tool} with no token: {e}"
            );
            assert_eq!(e["retryable"], false);
            let e = err_h(r, &k, &[("Agent-Token", tb.as_str())], tool, args.clone());
            assert_eq!(
                e["code"], "agent_mismatch",
                "[{name}] {tool} with another token: {e}"
            );
            let mut by_argument = args;
            by_argument["agent_token"] = json!(tb);
            let e = err_h(r, &k, &[], tool, by_argument);
            assert_eq!(
                e["code"], "agent_mismatch",
                "[{name}] {tool} by the argument"
            );
        }
        // Nothing changed: no turn and no cancel.
        assert_eq!(count(r, "turns"), 0, "[{name}]");
        assert_eq!(
            r.loaded(&id).item.state,
            inbox_core::proto::ItemState::Pending,
            "[{name}]"
        );
        // The agent of the item writes: the header or the argument.
        let said = ok_h(
            r,
            &k,
            &[("Agent-Token", ta.as_str())],
            "say",
            json!({"request_id": id, "text": "I am the agent A"}),
        );
        assert_eq!(said["request_id"], json!(id), "[{name}] {said}");
        // `supersede` for an approval gives its own answer with the right token.
        let e = err_h(
            r,
            &k,
            &[],
            "supersede",
            json!({"request_id": id, "agent_token": ta}),
        );
        assert_eq!(e["code"], "use_service_tool", "[{name}] {e}");
        let cancelled = ok_h(
            r,
            &k,
            &[],
            "cancel",
            json!({"request_id": id, "agent_token": ta}),
        );
        assert_eq!(cancelled["status"], "cancelled", "[{name}]");
        // The item of the bucket: a read goes by the key, and a token that is present must still match. A write needs the
        // key only, whatever the token.
        let own = r.submit_with(&k.subject, 2, json!({"lease_s": 900})).id();
        assert_eq!(
            agent_row(r, &agent_of(r, &own)).kind,
            AgentKind::Bucket,
            "[{name}]"
        );
        let read = ok_h(r, &k, &[], "keepalive", json!({"request_id": own}));
        assert_eq!(read["status"], "pending", "[{name}]");
        let e = err_h(
            r,
            &k,
            &[("Agent-Token", tb.as_str())],
            "keepalive",
            json!({"request_id": own}),
        );
        assert_eq!(e["code"], "agent_mismatch", "[{name}]");
        let done = ok_h(
            r,
            &k,
            &[("Agent-Token", tb.as_str())],
            "cancel",
            json!({"request_id": own}),
        );
        assert_eq!(done["status"], "cancelled", "[{name}]");
        // The REST door: the header or the body member, and the status of each error.
        let c = agent_in(
            r,
            ID_C,
            KeyKind::Api,
            KeyInteraction::Conversational,
            AgentsMode::Discover,
        );
        let made = rest_h(
            r,
            &c,
            &[],
            "POST",
            "/api/v1/approval-intents",
            &with(intent_args(), "allow_ask_back", json!(true)).to_string(),
        );
        let (rest_token, rest_item) = (
            made.json()["agent_token"].as_str().unwrap().to_string(),
            item_of(r, &c, 3, &made.json()),
        );
        let turns = format!("/api/v1/items/{rest_item}/turns");
        let no_token = rest_h(r, &c, &[], "POST", &turns, r#"{"text":"hello"}"#);
        assert_eq!(
            (
                no_token.status,
                no_token.json()["error"]["code"]
                    .as_str()
                    .map(str::to_string)
            ),
            (422, Some("agent_token_required".into())),
            "[{name}] {}",
            no_token.body
        );
        let wrong = rest_h(
            r,
            &c,
            &[("Agent-Token", tb.as_str())],
            "POST",
            &turns,
            r#"{"text":"hello"}"#,
        );
        // A token of an agent of another key is unknown for this key.
        assert_eq!(
            (
                wrong.status,
                wrong.json()["error"]["code"].as_str().map(str::to_string)
            ),
            (422, Some("agent_unknown".into())),
            "[{name}] {}",
            wrong.body
        );
        let by_body = rest_h(
            r,
            &c,
            &[],
            "POST",
            &turns,
            &json!({"text": "hello", "agent_token": rest_token}).to_string(),
        );
        assert_eq!(by_body.status, 201, "[{name}] {}", by_body.body);
        let by_header = rest_h(
            r,
            &c,
            &[("Agent-Token", rest_token.as_str())],
            "POST",
            &format!("/api/v1/items/{rest_item}/keepalive"),
            "",
        );
        assert_eq!(by_header.status, 200, "[{name}] {}", by_header.body);
        let keep_body = rest_h(
            r,
            &c,
            &[],
            "POST",
            &format!("/api/v1/items/{rest_item}/keepalive"),
            &json!({"agent_token": rest_token}).to_string(),
        );
        assert_eq!(keep_body.status, 200, "[{name}] {}", keep_body.body);
        let bad_member = rest_h(
            r,
            &c,
            &[],
            "POST",
            &format!("/api/v1/items/{rest_item}/supersede"),
            r#"{"agent_token": 5}"#,
        );
        assert_eq!(bad_member.status, 422, "[{name}] {}", bad_member.body);
        let gone = rest_h(
            r,
            &c,
            &[],
            "DELETE",
            &format!("/api/v1/items/{rest_item}"),
            "",
        );
        assert_eq!(gone.status, 422, "[{name}] {}", gone.body);
        let gone = rest_h(
            r,
            &c,
            &[("Agent-Token", rest_token.as_str())],
            "DELETE",
            &format!("/api/v1/items/{rest_item}"),
            "",
        );
        assert_eq!(gone.status, 200, "[{name}] {}", gone.body);
    });
}

#[test]
fn previous_id_of_another_agent_is_refused_on_the_channel() {
    each(|r, name| {
        r.fix_clock(T0);
        r.device();
        let k = conversational(r, ID_A);
        let one = ok_h(r, &k, &[], "approval_intent", intent_args());
        let id = item_of(r, &k, 1, &one);
        r.answer(&id, Action::Decline, None);
        let follow = |extra: &[(&str, &str)]| {
            call_h(
                r,
                &k,
                extra,
                "approval_intent",
                with(intent_args(), "previous_id", json!(id)),
            )
        };
        // The agent that made the item follows it.
        let token = token_of(&one);
        let (is_error, ok, _) = follow(&[("Agent-Token", token.as_str())]);
        assert!(!is_error, "[{name}] {ok}");
        let rows = agent_rows(r);
        // A call with no token would make a new agent, and a new agent has no thread.
        let (is_error, e, _) = follow(&[]);
        assert!(is_error, "[{name}]");
        assert_eq!(e["error"]["code"], "agent_mismatch", "[{name}] {e}");
        assert_eq!(agent_rows(r), rows, "[{name}] the refusal made no agent");
        // Another agent of the key is refused too.
        let two = token_of(&ok_h(r, &k, &[], "approval_intent", intent_args()));
        let (is_error, e, _) = follow(&[("Agent-Token", two.as_str())]);
        assert!(is_error, "[{name}]");
        assert_eq!(e["error"]["code"], "agent_mismatch", "[{name}]");
    });
}

// ---------------------------------------------------------------- the label

#[test]
fn label_injection_refused() {
    each(|r, name| {
        r.fix_clock(T0);
        r.device();
        let k = conversational(r, ID_A);
        let long = "x".repeat(65);
        let bad: Vec<&str> = vec![
            "",
            " leading space",
            "trailing space ",
            "two\nlines",
            "carriage\rreturn",
            "tab\there",
            "bell\u{7}",
            "reverse \u{202E}text",
            "isolate \u{2066}text",
            "zero\u{200B}width",
            "joiner\u{200D}x",
            "bom\u{FEFF}x",
            "soft\u{00AD}hyphen",
            "line\u{2028}separator",
            "paragraph\u{2029}separator",
            &long,
        ];
        for label in bad {
            let e = err_h(
                r,
                &k,
                &[],
                "approval_intent",
                with(intent_args(), "agent_label", json!(label)),
            );
            assert_eq!(e["code"], "invalid_agent_label", "[{name}] {label:?}: {e}");
            assert_eq!(e["retryable"], false, "[{name}]");
        }
        // A refusal makes no agent and no intent: the portal never cleans the text.
        assert_eq!(agent_rows(r), 0, "[{name}]");
        assert_eq!(count(r, "approval_intents"), 0, "[{name}]");
        // The REST door gives 422.
        let c = agent_in(
            r,
            ID_C,
            KeyKind::Api,
            KeyInteraction::Conversational,
            AgentsMode::Discover,
        );
        let reply = rest_h(
            r,
            &c,
            &[],
            "POST",
            "/api/v1/approval-intents",
            &with(intent_args(), "agent_label", json!("bad\u{202E}label")).to_string(),
        );
        assert_eq!(reply.status, 422, "[{name}] {}", reply.body);
        assert_eq!(reply.json()["error"]["code"], "invalid_agent_label");
        assert_eq!(agent_rows(r), 0, "[{name}]");
        // The longest label, and a label with a space inside, are fine. The text is kept as it is.
        let longest = "é".repeat(64);
        let v = ok_h(
            r,
            &k,
            &[],
            "approval_intent",
            with(intent_args(), "agent_label", json!(longest)),
        );
        let id = item_of(r, &k, 1, &v);
        assert_eq!(
            observed_of(r, &agent_of(r, &id)).says.as_deref(),
            Some(longest.as_str()),
            "[{name}]"
        );
        // The label is not a name: the title of the agent stays the text of the portal.
        assert_eq!(agent_row(r, &agent_of(r, &id)).name, None, "[{name}]");
    });
}

#[test]
fn hints_recorded() {
    each(|r, name| {
        r.fix_clock(T0);
        r.device();
        let k = conversational(r, ID_A);
        // A legacy call: the client is the `User-Agent`. The address range is cut to 24 bits.
        let made = ok_h(
            r,
            &k,
            &[("User-Agent", "canary-ua-7d2e/1.4 (tests)")],
            "approval_intent",
            with(intent_args(), "agent_label", json!("billing-bot")),
        );
        let id = item_of(r, &k, 1, &made);
        let agent_id = agent_of(r, &id);
        let seen = observed_of(r, &agent_id);
        assert_eq!(seen.says.as_deref(), Some("billing-bot"), "[{name}]");
        assert_eq!(
            seen.user_agent.as_deref(),
            Some("canary-ua-7d2e/1.4 (tests)"),
            "[{name}]"
        );
        assert_eq!(
            seen.client.as_deref(),
            Some("canary-ua-7d2e/1.4 (tests)"),
            "[{name}]"
        );
        assert_eq!(seen.net.as_deref(), Some("127.0.0.0/24"), "[{name}]");
        assert_eq!(seen.nets_seen(), 1, "[{name}]");
        // The label shows on the item, as text that is not verified, in the sealed body.
        let loaded = r.loaded(&id);
        let body = eventually(|| runtime().block_on(r.items().serve_body(&loaded)));
        let crate::inbox::testing::Served::Body(plain) = body else {
            panic!("[{name}] no body")
        };
        let body: Value = serde_json::from_slice(&plain).unwrap();
        assert_eq!(body["agent_label"], "billing-bot", "[{name}] {body}");
        // The client of a call of the new line is `clientInfo` of `_meta`, and it replaces the `User-Agent`.
        let token = token_of(&made);
        let auth = format!("Bearer {}", k.token);
        let params = json!({
            "name": "get_answer",
            "arguments": {"request_id": id, "agent_token": token},
            "_meta": {
                "io.modelcontextprotocol/protocolVersion": "2026-07-28",
                "io.modelcontextprotocol/clientInfo": {"name": "claude-code", "version": "2.1.0"}
            }
        });
        let reply = send(
            r.node.port,
            "POST",
            "/mcp",
            &[
                ("Content-Type", "application/json"),
                ("Authorization", auth.as_str()),
                ("MCP-Protocol-Version", "2026-07-28"),
                ("Mcp-Method", "tools/call"),
                ("Mcp-Name", "get_answer"),
                ("User-Agent", "canary-ua-7d2e/1.4 (tests)"),
            ],
            &json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": params})
                .to_string(),
        );
        assert_eq!(reply.status, 200, "[{name}] {}", reply.body);
        assert_eq!(
            reply.json()["result"]["structuredContent"]["status"],
            "pending",
            "[{name}]"
        );
        let seen = observed_of(r, &agent_id);
        assert_eq!(
            seen.client.as_deref(),
            Some("claude-code/2.1.0"),
            "[{name}]"
        );
        assert_eq!(
            seen.user_agent.as_deref(),
            Some("canary-ua-7d2e/1.4 (tests)"),
            "[{name}]"
        );
        // A new text that the agent says is an event, and the first text stays.
        ok_h(
            r,
            &k,
            &[],
            "approval_intent",
            with(
                with(intent_args(), "agent_label", json!("night-etl")),
                "agent_token",
                json!(token),
            ),
        );
        let seen = observed_of(r, &agent_id);
        assert_eq!(
            (
                seen.says.as_deref(),
                seen.says_first.as_deref(),
                seen.says_changes
            ),
            (Some("night-etl"), Some("billing-bot"), 1),
            "[{name}]"
        );
        assert!(
            events_of(r, &agent_id).contains(&"says_changed".to_string()),
            "[{name}]"
        );
    });
}

#[test]
fn rest_get_with_token_in_query_is_refused() {
    each(|r, name| {
        r.fix_clock(T0);
        r.device();
        let c = agent_in(
            r,
            ID_C,
            KeyKind::Api,
            KeyInteraction::Conversational,
            AgentsMode::Discover,
        );
        let made = rest_h(
            r,
            &c,
            &[],
            "POST",
            "/api/v1/approval-intents",
            &intent_args().to_string(),
        );
        let token = made.json()["agent_token"].as_str().unwrap().to_string();
        let id = item_of(r, &c, 1, &made.json());
        for (method, path, body) in [
            ("GET", format!("/api/v1/items/{id}?agent_token={token}"), ""),
            (
                "GET",
                format!("/api/v1/items/{id}?wait=0&agent-token={token}"),
                "",
            ),
            (
                "DELETE",
                format!("/api/v1/items/{id}?agent_token={token}"),
                "",
            ),
            (
                "POST",
                format!("/api/v1/items/{id}/keepalive?agent_token={token}"),
                "",
            ),
            (
                "POST",
                format!("/api/v1/items/{id}/turns?agent_token={token}"),
                r#"{"text":"x"}"#,
            ),
            (
                "POST",
                "/api/v1/approval-intents?agent_token=x".to_string(),
                r#"{"lease_s":900}"#,
            ),
        ] {
            let reply = rest_h(r, &c, &[], method, &path, body);
            assert_eq!(
                reply.status, 400,
                "[{name}] {method} {path}: {}",
                reply.body
            );
            assert_eq!(reply.json()["error"]["code"], "invalid_query", "[{name}]");
            assert!(
                !reply.body.contains(&token[4..]),
                "[{name}] the answer repeats the token"
            );
        }
        assert_eq!(
            r.loaded(&id).item.state,
            inbox_core::proto::ItemState::Pending,
            "[{name}]"
        );
        // The header is the way: the same call works.
        let reply = rest_h(
            r,
            &c,
            &[("Agent-Token", token.as_str())],
            "GET",
            &format!("/api/v1/items/{id}"),
            "",
        );
        assert_eq!(reply.status, 200, "[{name}] {}", reply.body);
    });
}

// ---------------------------------------------------------------- what the service and the logs never see

#[test]
fn service_never_sees_agent_token_agent_label_or_hints() {
    each(|r, name| {
        r.fix_clock(T0);
        r.device();
        let k = conversational(r, ID_A);
        let label = "canary-label-3b8f";
        let ua = "canary-ua-90ac/3.1";
        let made = ok_h(
            r,
            &k,
            &[("User-Agent", ua)],
            "approval_intent",
            with(intent_args(), "agent_label", json!(label)),
        );
        let token = token_of(&made);
        let id = item_of(r, &k, 1, &made);
        let agent_id = agent_of(r, &id);
        // The same agent asks again with its token, and the service makes a second request.
        let again = ok_h(
            r,
            &k,
            &[("Agent-Token", token.as_str()), ("User-Agent", ua)],
            "approval_intent",
            with(intent_args(), "agent_label", json!(label)),
        );
        item_of(r, &k, 2, &again);
        // The service reads the verdict, renews and waits: each answer is recorded.
        assert_eq!(r.verdict(&id, &k.subject), "pending", "[{name}]");
        // Everything that the service got holds none of the values of the agent.
        for rec in r.log.lock().unwrap().iter() {
            for secret in [&token[4..], label, ua, agent_id.as_str(), "127.0.0", "agt_"] {
                assert!(
                    !rec.body.contains(secret)
                        && !rec
                            .headers
                            .iter()
                            .any(|(h, v)| h.contains(secret) || v.contains(secret)),
                    "[{name}] {} {} shows an agent value ({secret}): {}",
                    rec.route,
                    rec.status,
                    rec.body
                );
            }
        }
        check_what_the_service_learned(r, name);
        // The database keeps the digest of the token and never the token.
        let found = r.count(&format!(
            "SELECT COUNT(*) FROM agents WHERE token_digest = '{}'",
            crate::inbox::agents::token_digest(&token)
        ));
        assert_eq!(found, 1, "[{name}]");
        assert_eq!(
            r.count(&format!(
                "SELECT COUNT(*) FROM agents WHERE token_digest LIKE '%{}%'",
                &token[4..]
            )),
            0,
            "[{name}]"
        );
    });
}

#[test]
fn agent_token_never_in_results_after_the_first() {
    each(|r, name| {
        r.fix_clock(T0);
        r.device();
        let k = conversational(r, ID_A);
        let made = ok_h(
            r,
            &k,
            &[],
            "approval_intent",
            with(intent_args(), "allow_ask_back", json!(true)),
        );
        let token = token_of(&made);
        let secret = &token[4..];
        let id = item_of(r, &k, 1, &made);
        let header = [("Agent-Token", token.as_str())];
        // Each later result holds no token: not the intent, and not a read, a renewal, a turn or a cancel.
        let mut replies = Vec::new();
        replies.push(call_h(r, &k, &header, "approval_intent", intent_args()).2);
        replies.push(call_h(r, &k, &header, "get_answer", json!({"request_id": id})).2);
        replies.push(call_h(r, &k, &header, "keepalive", json!({"request_id": id})).2);
        replies.push(
            call_h(
                r,
                &k,
                &header,
                "say",
                json!({"request_id": id, "text": "hello"}),
            )
            .2,
        );
        replies.push(call_h(r, &k, &header, "supersede", json!({"request_id": id})).2);
        replies.push(call_h(r, &k, &header, "cancel", json!({"request_id": id})).2);
        // The errors of the table do not repeat the token either.
        replies.push(
            call_h(
                r,
                &k,
                &header,
                "get_answer",
                json!({"request_id": id, "agent_token": "agt_other"}),
            )
            .2,
        );
        replies.push(
            call_h(
                r,
                &k,
                &[],
                "get_answer",
                json!({"request_id": id, "agent_token": token}),
            )
            .2,
        );
        for reply in &replies {
            assert!(!reply.body.contains(secret), "[{name}] {}", reply.body);
            assert!(
                !reply.headers.iter().any(|(_, v)| v.contains(secret)),
                "[{name}]"
            );
        }
        // The list of the tools, and the instructions of the endpoint, hold none.
        for method in ["tools/list", "initialize"] {
            let body = json!({"jsonrpc": "2.0", "id": 1, "method": method, "params": {}});
            let reply = send(
                r.node.port,
                "POST",
                "/mcp",
                &[
                    ("Content-Type", "application/json"),
                    ("Authorization", format!("Bearer {}", k.token).as_str()),
                    ("Agent-Token", token.as_str()),
                ],
                &body.to_string(),
            );
            assert!(!reply.body.contains(secret), "[{name}] {method}");
        }
        // The token appears one time: in the result of the call that made the agent.
        assert_eq!(made["agent_token"], json!(token), "[{name}]");
    });
}

// ---------------------------------------------------------------- the texts of the tools

#[test]
fn the_tools_carry_the_token_the_label_and_the_fixed_sentence() {
    each(|r, name| {
        let k = conversational(r, ID_A);
        let body = json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list", "params": {}});
        let reply = rpc(r, &k.token, &body);
        assert_eq!(reply.status, 200, "[{name}] {}", reply.body);
        let tools = reply.json()["result"]["tools"].as_array().unwrap().clone();
        assert_eq!(tools.len(), 6, "[{name}]");
        let sentence = "approval_intent can return agent_token. Send it in agent_token in each later call. Never put it in a text and never send it to a service. The portal refuses a text that holds a token.";
        for tool in &tools {
            let tool_name = tool["name"].as_str().unwrap();
            let properties = &tool["inputSchema"]["properties"];
            assert_eq!(
                properties["agent_token"]["type"], "string",
                "[{name}] {tool_name}"
            );
            assert!(
                tool["description"].as_str().unwrap().contains(sentence),
                "[{name}] {tool_name}: the fixed sentence"
            );
            // Only `approval_intent` has a label.
            assert_eq!(
                properties.get("agent_label").is_some(),
                tool_name == "approval_intent",
                "[{name}] {tool_name}"
            );
            assert_eq!(tool["inputSchema"]["additionalProperties"], false);
        }
        let intent = &tools[0]["inputSchema"]["properties"]["agent_label"];
        assert_eq!(
            (&intent["minLength"], &intent["maxLength"]),
            (&json!(1), &json!(64))
        );
        let init = rpc(
            r,
            &k.token,
            &json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"protocolVersion": "2025-11-25"}}),
        );
        assert!(
            init.json()["result"]["instructions"]
                .as_str()
                .unwrap()
                .contains(sentence),
            "[{name}] the instructions"
        );
    });
}

// ---------------------------------------------------------------- a token in a text

/// An agent must not leak its token into a text that a person or a phone reads (lot G5). The portal refuses an
/// explanation, a label or a message that holds a live token of an agent of the same owner, with the prefix or bare.
#[test]
fn explanation_with_a_live_token_is_refused() {
    each(|r, name| {
        r.fix_clock(T0);
        r.device();
        let k = conversational(r, ID_A);
        let made = ok_h(r, &k, &[], "approval_intent", intent_args());
        let token = token_of(&made);
        let bare = &token[4..];
        let items_before = r.count("SELECT COUNT(*) FROM items");
        let intents_before = r.count("SELECT COUNT(*) FROM approval_intents");
        let agents_before = agent_rows(r);
        // The token with its prefix, bare, and glued to other characters of the alphabet: each one is refused, with the
        // code of the table and a message that names no value.
        for text in [
            format!("please use {token} for the next call"),
            format!("token: {bare}"),
            format!("x{bare}y"),
            format!("{token}{token}"),
            format!("line one\n{token}"),
        ] {
            let e = err_h(
                r,
                &k,
                &[],
                "approval_intent",
                with(intent_args(), "explanation", json!(text)),
            );
            assert_eq!(e["code"], "agent_token_in_text", "[{name}] {text}");
            assert_eq!(e["retryable"], false, "[{name}]");
            assert!(!e["message"].to_string().contains(bare), "[{name}] {e}");
        }
        // The label is a text too. It is short, but a token fits in it.
        let e = err_h(
            r,
            &k,
            &[],
            "approval_intent",
            with(intent_args(), "agent_label", json!(token)),
        );
        assert_eq!(e["code"], "agent_token_in_text", "[{name}] {e}");
        // The refusal comes before any write: no item, no intent, no agent.
        assert_eq!(
            r.count("SELECT COUNT(*) FROM items"),
            items_before,
            "[{name}]"
        );
        assert_eq!(
            r.count("SELECT COUNT(*) FROM approval_intents"),
            intents_before,
            "[{name}]"
        );
        assert_eq!(agent_rows(r), agents_before, "[{name}]");
        // A second agent of the same key cannot carry the first token in its text either.
        let second = ok_h(r, &k, &[], "approval_intent", intent_args());
        assert_eq!(second["agent_status"], "new", "[{name}]");
        let e = err_h(
            r,
            &k,
            &[("Agent-Token", token_of(&second).as_str())],
            "approval_intent",
            with(intent_args(), "explanation", json!(format!("copy {bare}"))),
        );
        assert_eq!(e["code"], "agent_token_in_text", "[{name}] {e}");
        // A text that only looks like a token is fine: 43 characters that are no token, a prefix alone, a short text.
        for text in [
            "a".repeat(43),
            format!("agt_{}", "b".repeat(43)),
            "agt_".to_string(),
            format!("{} and more", &bare[..42]),
            "Run the migration on the staging table.".to_string(),
        ] {
            let ok = ok_h(
                r,
                &k,
                &[("Agent-Token", token.as_str())],
                "approval_intent",
                with(intent_args(), "explanation", json!(text)),
            );
            assert!(ok.get("approval_ref").is_some(), "[{name}] {text}: {ok}");
        }
        // A message of the thread (`say`) is checked too. The item allows the talk and the owner asked.
        let open = {
            let intent = ok_h(
                r,
                &k,
                &[("Agent-Token", token.as_str())],
                "approval_intent",
                with(intent_args(), "allow_ask_back", json!(true)),
            );
            item_of(r, &k, 7, &intent)
        };
        owner_reply(r, &open, "m1", "Why this table?");
        let e = err_h(
            r,
            &k,
            &[("Agent-Token", token.as_str())],
            "say",
            json!({"request_id": open, "text": format!("my token is {token}")}),
        );
        assert_eq!(e["code"], "agent_token_in_text", "[{name}] {e}");
        let said = ok_h(
            r,
            &k,
            &[("Agent-Token", token.as_str())],
            "say",
            json!({"request_id": open, "text": "Because the dry run passed."}),
        );
        assert_eq!(said["seq"], 2, "[{name}] {said}");
        // A token that is blocked or merged is still a stored digest: a text that holds it is refused.
        let blocked = agent_of(r, &open);
        owner_op(r, &blocked, AgentOp::Block);
        let e = err_h(
            r,
            &k,
            &[("Agent-Token", token_of(&second).as_str())],
            "approval_intent",
            with(intent_args(), "explanation", json!(format!("old {token}"))),
        );
        assert_eq!(e["code"], "agent_token_in_text", "[{name}] {e}");
        // The REST door has the same rule and the same code.
        let c = agent_in(
            r,
            ID_C,
            KeyKind::Api,
            KeyInteraction::Conversational,
            AgentsMode::Discover,
        );
        let first = rest_h(
            r,
            &c,
            &[],
            "POST",
            "/api/v1/approval-intents",
            &intent_args().to_string(),
        );
        assert_eq!(first.status, 201, "[{name}] {}", first.body);
        let rest_token = first.json()["agent_token"].as_str().unwrap().to_string();
        let leak = rest_h(
            r,
            &c,
            &[("Agent-Token", rest_token.as_str())],
            "POST",
            "/api/v1/approval-intents",
            &with(
                intent_args(),
                "explanation",
                json!(format!("t={rest_token}")),
            )
            .to_string(),
        );
        assert_eq!(leak.status, 422, "[{name}] {}", leak.body);
        assert_eq!(
            leak.json()["error"]["code"],
            "agent_token_in_text",
            "[{name}]"
        );
        assert!(!leak.body.contains(&rest_token[4..]), "[{name}]");
    });
}
