//! The agent channel over HTTP, on SQLite and on PostgreSQL: the MCP endpoint and the REST twin, on the rig of the
//! approval API. The tests prove that a key reads and speaks only on its own items, that a renewal makes no row, that the
//! text of the owner reaches the agent only inside `untrusted`, and that the limits of the endpoint hold.
//!
//! This module is a child of `approval_api_tests`: it uses the rig, the commands and the helpers of the series A.

use super::*;
use crate::inbox::testing::{Answer, AnswerVia};
use crate::keys::{AgentsMode, Interaction as KeyInteraction, KeyKind};

/// A key of the owner, with its text.
struct Agent {
    subject: String,
    token: String,
}

/// A server like the rig of the series A, with the kind `api` on and the block `inbox.mcp`. `mcp` holds the keys of the
/// block.
///
/// `approvals` holds more keys for the block `inbox.approvals` (for example a higher limit of the intents). `blocks` holds
/// more blocks of `inbox` after the block `approvals` (for example `agents { ... }`).
fn agent_rig_full(backend: &Backend, approvals: &str, blocks: &str, mcp: &str) -> Option<Rig> {
    let inbox = InboxRig::with_approvals(
        backend,
        &format!("consume_within = \"5m\" {approvals} }} {blocks} mcp {{ {mcp}"),
    )?;
    let port = free_port();
    let mut env = inbox.env.clone();
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
            keys {{ api = true }}
            pdp_client "db" {{ token_env = "TEST_PDP_DB" approvals = true }}
            {}"#,
            inbox.config
        ),
    );
    publish_catalog(&node, &[]);
    node.state.actions.set_document(
        "db",
        crate::actions::parse("db", DOCUMENT.as_bytes()).unwrap(),
    );
    wait_until(Duration::from_secs(10), || {
        std::net::TcpStream::connect(("127.0.0.1", port)).is_ok()
    });
    let mut grants: Vec<Grant> = [
        "access on service:db",
        "data.write on service:db/host:prod",
        "keys.own.use on service:portal/key_kind:mcp",
        "keys.own.use on service:portal/key_kind:api",
    ]
    .iter()
    .map(|g| Grant::parse(g).unwrap())
    .collect();
    grants.sort();
    node.state
        .store
        .insert_user(&crate::store::UserRow {
            login: OWNER.into(),
            password_hash: "h".into(),
            grants,
            created_at: 1,
        })
        .unwrap();
    node.state.users.refresh(&node.state.store).unwrap();
    let app = app_of(&node, None);
    Some(Rig {
        node,
        port,
        app,
        log: Mutex::new(Vec::new()),
        phone: std::sync::atomic::AtomicBool::new(false),
        next_digest: std::sync::atomic::AtomicU8::new(1),
        _rig: inbox,
    })
}

fn each_agent_rig(mcp: &str, test: impl Fn(&Rig, &str)) {
    each_agent_rig_with("", mcp, test);
}

fn each_agent_rig_with(blocks: &str, mcp: &str, test: impl Fn(&Rig, &str)) {
    each_agent_rig_full("", blocks, mcp, test);
}

fn each_agent_rig_full(approvals: &str, blocks: &str, mcp: &str, test: impl Fn(&Rig, &str)) {
    stay_in_a_minute();
    for backend in backends() {
        let Some(r) = agent_rig_full(&backend, approvals, blocks, mcp) else {
            eprintln!("INBOX_TEST_PG is not set: skipped on PostgreSQL");
            continue;
        };
        test(&r, backend.name());
    }
}

/// A key of the owner with a real text. It can use `data.write` on analytics with `confirm`, as the keys of the rig. The
/// mode of the agents is the default of the kind.
fn agent(r: &Rig, id: &str, kind: KeyKind, interaction: KeyInteraction) -> Agent {
    agent_in(r, id, kind, interaction, AgentsMode::default_for(kind))
}

fn agent_in(
    r: &Rig,
    id: &str,
    kind: KeyKind,
    interaction: KeyInteraction,
    agents: AgentsMode,
) -> Agent {
    let token = crate::keys::generate(kind).unwrap().to_string();
    let write = Grant::from_path_text("data.write", "db", ANALYTICS)
        .unwrap()
        .with_confirm(true);
    let mut grants = vec![Grant::new("access", "db").unwrap(), write];
    grants.sort();
    r.node
        .state
        .store
        .insert_key(
            &crate::store::KeyRow {
                kind,
                interaction,
                agents,
                id: id.into(),
                owner: Some(OWNER.into()),
                name: format!("key {id}"),
                digest: crate::keys::digest(&token),
                hint: "ab12".into(),
                created_at: crate::tracking::unix_now() - 10,
                expires_at: None,
                last_check_at: None,
                check_count: 0,
                grants,
            },
            &key_caps(),
        )
        .unwrap();
    r.node.state.users.refresh(&r.node.state.store).unwrap();
    Agent {
        subject: format!("key:{id}"),
        token,
    }
}

const ID_A: &str = "2b7c91d0e4a853f6";
const ID_B: &str = "9f3d27a18c64b0e5";
const ID_C: &str = "c04e6b51d2a79f38";

fn conversational(r: &Rig, id: &str) -> Agent {
    agent(r, id, KeyKind::Mcp, KeyInteraction::Conversational)
}

fn one_shot(r: &Rig, id: &str) -> Agent {
    agent(r, id, KeyKind::Mcp, KeyInteraction::OneShot)
}

// ---------------------------------------------------------------- the calls

fn rpc(r: &Rig, token: &str, body: &Value) -> Reply {
    let auth = format!("Bearer {token}");
    send(
        r.node.port,
        "POST",
        "/mcp",
        &[
            ("Content-Type", "application/json"),
            ("Authorization", &auth),
        ],
        &body.to_string(),
    )
}

/// A tool call. The result is the flag `isError` and the structured content, after the checks that each result of a tool
/// obeys: the text is the same object, and a text of a third party has the notice first.
fn tool_call(r: &Rig, a: &Agent, name: &str, args: Value) -> (bool, Value, Reply) {
    let reply = rpc(
        r,
        &a.token,
        &json!({"jsonrpc": "2.0", "id": 7, "method": "tools/call",
                "params": {"name": name, "arguments": args}}),
    );
    assert_eq!(reply.status, 200, "{}", reply.body);
    let body = reply.json();
    assert_eq!(body["id"], 7);
    let result = &body["result"];
    let text = result["content"][0]["text"].as_str().expect("a text");
    let parsed: Value = serde_json::from_str(text).unwrap();
    assert_eq!(parsed, result["structuredContent"], "{text}");
    if result["structuredContent"].get("untrusted").is_some() {
        assert!(text.starts_with("{\"data_notice\":"), "{text}");
    }
    (
        result["isError"] == true,
        result["structuredContent"].clone(),
        reply,
    )
}

fn ok(r: &Rig, a: &Agent, name: &str, args: Value) -> Value {
    let (is_error, value, _) = tool_call(r, a, name, args);
    assert!(!is_error, "{name}: {value}");
    value
}

/// The error object of a tool that fails.
fn fails(r: &Rig, a: &Agent, name: &str, args: Value) -> Value {
    let (is_error, value, _) = tool_call(r, a, name, args);
    assert!(is_error, "{name} must fail: {value}");
    value["error"].clone()
}

fn rest(r: &Rig, token: &str, method: &str, path: &str, body: &str) -> Reply {
    let auth = format!("Bearer {token}");
    send(
        r.node.port,
        method,
        path,
        &[
            ("Content-Type", "application/json"),
            ("Authorization", &auth),
        ],
        body,
    )
}

/// An approval of the key through the real path: the intent at the portal, then the submission of the service with the
/// reference. The result is the request id.
fn request(r: &Rig, a: &Agent, n: u32, flags: Value) -> String {
    let mut args = json!({"lease_s": 900, "explanation": EXPLANATION});
    for (k, v) in flags.as_object().unwrap() {
        args[k] = v.clone();
    }
    let intent = ok(r, a, "approval_intent", args);
    let reference = intent["approval_ref"].as_str().unwrap();
    assert_eq!(reference.len(), 43, "{intent}");
    assert!(intent["valid_for_s"].as_i64().unwrap() > 0);
    r.submit_with(&a.subject, n, json!({"approval_ref": reference}))
        .id()
}

fn owner_reply(r: &Rig, id: &str, client_msg_id: &str, text: &str) {
    let out = eventually(|| {
        runtime().block_on(r.items().reply(
            id,
            OWNER,
            None,
            client_msg_id.to_string(),
            text.as_bytes(),
        ))
    });
    assert!(
        matches!(out, crate::inbox::testing::TurnOut::Stored { .. }),
        "{out:?}"
    );
}

/// The owner declines with a text in the form that the phone and the web page seal.
fn decline_with(r: &Rig, id: &str, text: &str, intent: inbox_core::proto::Intent) {
    let effect = r.loaded(id).approval.unwrap().effect;
    let out = eventually(|| {
        runtime().block_on(r.items().answer(
            id,
            Answer {
                login: OWNER.into(),
                via: AnswerVia::Web,
                action: Action::Decline,
                shown_effect: Some(effect.clone()),
                client_msg_id: None,
                payload: Some(crate::inbox::testing::owner_text_payload(text, intent)),
                consume_within: Duration::from_secs(300),
            },
        ))
    });
    assert!(
        matches!(out, crate::inbox::testing::Answered::Accepted { .. }),
        "{out:?}"
    );
}

fn count(r: &Rig, table: &str) -> i64 {
    r.count(&format!("SELECT COUNT(*) FROM {table}"))
}

// ---------------------------------------------------------------- the tests

#[test]
fn agent_cannot_read_or_speak_on_another_keys_item() {
    each_agent_rig("", |r, name| {
        r.fix_clock(T0);
        let a = conversational(r, ID_A);
        let b = conversational(r, ID_B);
        let id = request(r, &a, 1, json!({"allow_ask_back": true}));
        let before = r.loaded(&id);
        let missing = "0123456789abcdef0123456789abcdef";
        for (tool, extra) in [
            ("get_answer", json!({"wait_s": 0})),
            ("keepalive", json!({})),
            ("say", json!({"text": "hello"})),
            ("cancel", json!({})),
            ("supersede", json!({})),
        ] {
            let mut other = extra.clone();
            other["request_id"] = json!(id);
            let mut unknown = extra;
            unknown["request_id"] = json!(missing);
            // The answer for the item of another key is the answer for an item that does not exist.
            let theirs = fails(r, &b, tool, other);
            let none = fails(r, &b, tool, unknown);
            assert_eq!(theirs, none, "[{name}] {tool}");
            assert_eq!(theirs["code"], "not_found", "[{name}] {tool}");
            assert_eq!(theirs["retryable"], false);
        }
        // A request id that is not an id is the same answer.
        let bad = fails(r, &b, "get_answer", json!({"request_id": "x"}));
        assert_eq!(bad["code"], "not_found");
        assert_eq!(r.loaded(&id), before, "[{name}] nothing changed");
        assert_eq!(r.verdict(&id, &a.subject), "pending", "[{name}]");
        // The key of the item reads and speaks.
        let mine = ok(r, &a, "get_answer", json!({"request_id": id}));
        assert_eq!(mine["status"], "pending", "[{name}]");
        // The REST door has the same rule, for a key of the kind api.
        let c = agent(r, ID_C, KeyKind::Api, KeyInteraction::Conversational);
        for (method, path) in [
            ("GET", format!("/api/v1/items/{id}?wait=0")),
            ("DELETE", format!("/api/v1/items/{id}")),
            ("POST", format!("/api/v1/items/{id}/keepalive")),
            ("POST", format!("/api/v1/items/{id}/turns")),
            ("POST", format!("/api/v1/items/{id}/supersede")),
        ] {
            let reply = rest(r, &c.token, method, &path, r#"{"text":"hello"}"#);
            assert_eq!(
                reply.status, 404,
                "[{name}] {method} {path}: {}",
                reply.body
            );
            assert_eq!(reply.json()["error"]["code"], "not_found");
        }
        assert_eq!(r.loaded(&id), before, "[{name}] nothing changed");
    });
}

#[test]
fn get_answer_renews_lease_without_budget_unit() {
    each_agent_rig("", |r, name| {
        r.fix_clock(T0);
        let a = conversational(r, ID_A);
        let id = request(r, &a, 1, json!({}));
        let first = r.loaded(&id).item;
        assert_eq!(first.lease_until, Some(T0 + 3 * 900_000));
        // An item of a key of the kind api: one_shot, with a lease and no intent.
        let c = agent(r, ID_C, KeyKind::Api, KeyInteraction::Conversational);
        let own = r.submit_with(&c.subject, 9, json!({"lease_s": 900})).id();
        let created = r.loaded(&own).item.agent_seen_at;
        let rows = |r: &Rig| {
            [
                "items",
                "turns",
                "item_events",
                "approvals",
                "approval_intents",
                "alerts",
            ]
            .map(|table| count(r, table))
        };
        let before = rows(r);
        // A read after one `lease_s` renews: the lease moves, and nothing is written but the two columns.
        let later = T0 + 1_000_000;
        r.fix_clock(later);
        let view = ok(r, &a, "get_answer", json!({"request_id": id}));
        assert_eq!(view["status"], "pending", "[{name}]");
        let renewed = r.loaded(&id).item;
        assert_eq!(renewed.agent_seen_at, later, "[{name}]");
        assert_eq!(renewed.lease_until, Some(later + 3 * 900_000), "[{name}]");
        // A second read in the same `lease_s` writes nothing.
        r.fix_clock(later + 5_000);
        ok(r, &a, "get_answer", json!({"request_id": id}));
        assert_eq!(r.loaded(&id).item.agent_seen_at, later, "[{name}]");
        // `keepalive` and a REST read with `wait` are renewals too, and a REST read without `wait` is not.
        r.fix_clock(later + 1_000_000);
        let kept = ok(r, &a, "keepalive", json!({"request_id": id}));
        assert_eq!(kept["status"], "pending");
        assert_eq!(r.loaded(&id).item.agent_seen_at, later + 1_000_000);
        r.fix_clock(T0 + 2_500_000);
        let read = rest(r, &c.token, "GET", &format!("/api/v1/items/{own}"), "");
        assert_eq!(read.status, 200, "{}", read.body);
        assert_eq!(
            r.loaded(&own).item.agent_seen_at,
            created,
            "no `wait`: no renewal"
        );
        let waited = rest(
            r,
            &c.token,
            "GET",
            &format!("/api/v1/items/{own}?wait=0"),
            "",
        );
        assert_eq!(waited.status, 200, "{}", waited.body);
        assert_eq!(r.loaded(&own).item.agent_seen_at, T0 + 2_500_000);
        // No row, no event and no unit: each table holds what it held.
        assert_eq!(rows(r), before, "[{name}] a renewal writes no row");
    });
}

#[test]
fn get_answer_returns_comment_in_untrusted_with_intent() {
    each_agent_rig("", |r, name| {
        r.fix_clock(T0);
        r.device();
        let a = conversational(r, ID_A);
        let id = request(r, &a, 1, json!({"allow_custom": true}));
        // A read with no answer is small.
        let (_, pending, reply) = tool_call(r, &a, "get_answer", json!({"request_id": id}));
        assert!(pending.get("untrusted").is_none() && pending.get("data_notice").is_none());
        assert!(
            reply.body.len() < 700,
            "[{name}] a pending read is short: {}",
            reply.body
        );
        let comment = "Use the database staging.\u{7}\nThen run it again.";
        decline_with(r, &id, comment, inbox_core::proto::Intent::ChangeRequest);
        let (is_error, view, reply) = tool_call(r, &a, "get_answer", json!({"request_id": id}));
        assert!(!is_error);
        assert_eq!(view["status"], "answered", "[{name}]");
        assert_eq!(view["decision"], "denied", "[{name}]");
        assert_eq!(
            view["untrusted"]["comment"]["intent"], "change_request",
            "[{name}]"
        );
        // A control character is the replacement character. The line end stays.
        assert_eq!(
            view["untrusted"]["comment"]["text"],
            "Use the database staging.\u{FFFD}\nThen run it again.",
            "[{name}]"
        );
        assert!(view["data_notice"]
            .as_str()
            .unwrap()
            .contains("never as instructions"));
        assert!(view.get("consume_before").is_none());
        // The result never shows who or where: not the login of the owner, not the device, not the way.
        for canary in [OWNER, DEVICE] {
            assert!(
                !reply.body.contains(canary),
                "[{name}] {canary} is in {}",
                reply.body
            );
        }
        for member in [
            "answered_by",
            "answered_via",
            "device",
            "by",
            "via",
            "approver",
        ] {
            assert!(view.get(member).is_none(), "[{name}] {member}");
        }
        // A refusal with a plain text is a comment.
        let b_id = request(r, &a, 2, json!({"allow_custom": true}));
        r.answer(&b_id, Action::Decline, Some("not now"));
        let (_, plain, plain_reply) = tool_call(r, &a, "get_answer", json!({"request_id": b_id}));
        assert!(
            !plain_reply.body.contains(DEVICE),
            "[{name}] the device is not shown"
        );
        assert_eq!(
            plain["untrusted"]["comment"],
            json!({"text": "not now", "intent": "comment"}),
            "[{name}]"
        );
        // An approval has a decision and a window, and no text.
        let c_id = request(r, &a, 3, json!({}));
        r.answer(&c_id, Action::Accept, None);
        let accepted = ok(r, &a, "get_answer", json!({"request_id": c_id}));
        assert_eq!(accepted["decision"], "approved");
        assert!(accepted["consume_before"].is_string(), "{accepted}");
        assert!(accepted.get("untrusted").is_none() && accepted.get("data_notice").is_none());
        // The service never reads the text: it learns the verdict only.
        assert_eq!(r.verdict(&id, &a.subject), "denied");
        let rest_view = rest(r, &a.token, "GET", &format!("/api/v1/items/{id}"), "");
        assert_eq!(
            rest_view.status, 401,
            "an mcp key does not open the REST door"
        );
    });
}

#[test]
fn say_needs_allow_ask_back_and_conversational_key() {
    each_agent_rig("", |r, name| {
        r.fix_clock(T0);
        let a = conversational(r, ID_A);
        let shot = one_shot(r, ID_B);
        // The item does not allow a talk: `allow_ask_back` is false.
        let closed = request(r, &a, 1, json!({}));
        let error = fails(r, &a, "say", json!({"request_id": closed, "text": "hello"}));
        assert_eq!(error["code"], "ask_back_not_allowed", "[{name}]");
        // A key that is not conversational does not see the tool, and a call is a call of an unknown tool.
        let reply = rpc(
            r,
            &shot.token,
            &json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call",
                    "params": {"name": "say", "arguments": {"request_id": closed, "text": "x"}}}),
        );
        assert_eq!(reply.status, 200);
        assert_eq!(
            reply.json()["error"]["code"],
            -32602,
            "[{name}] {}",
            reply.body
        );
        // The item that allows a talk: the owner asks, the agent reads it and says.
        let open = request(r, &a, 2, json!({"allow_ask_back": true}));
        owner_reply(r, &open, "m1", "Why this table?");
        let asked = ok(r, &a, "get_answer", json!({"request_id": open}));
        assert_eq!(asked["status"], "replied", "[{name}]");
        assert_eq!(asked["last_seq"], 1);
        assert_eq!(
            asked["untrusted"]["messages"],
            json!([{"seq": 1, "text": "Why this table?"}]),
            "[{name}]"
        );
        // A wait returns at once when the owner asked something that the agent did not read.
        let started = std::time::Instant::now();
        let again = ok(
            r,
            &a,
            "get_answer",
            json!({"request_id": open, "wait_s": 1, "after_seq": 0}),
        );
        assert_eq!(again["status"], "replied");
        assert!(started.elapsed() < Duration::from_millis(900), "[{name}]");
        let said = ok(
            r,
            &a,
            "say",
            json!({"request_id": open, "text": "It holds the events.", "expect_seq": 1}),
        );
        assert_eq!(said["seq"], 2, "[{name}]");
        assert_eq!(said["status"], "pending", "[{name}]");
        // A second turn in the same instant is too fast. A stale `expect_seq` is refused.
        let fast = fails(r, &a, "say", json!({"request_id": open, "text": "More."}));
        assert_eq!(fast["code"], "too_fast", "[{name}]");
        assert_eq!(fast["retryable"], true);
        assert_eq!(fast["retry_after"], 2);
        r.fix_clock(T0 + 3_000);
        let stale = fails(
            r,
            &a,
            "say",
            json!({"request_id": open, "text": "More.", "expect_seq": 1}),
        );
        assert_eq!(stale["code"], "stale_thread", "[{name}]");
        // An empty text, a text over the limit and a text of the wrong type are refused, and nothing is written.
        for bad in [json!(""), json!("x".repeat(2001)), json!(5)] {
            let error = fails(r, &a, "say", json!({"request_id": open, "text": bad}));
            assert!(
                matches!(
                    error["code"].as_str(),
                    Some("invalid_argument" | "text_too_long")
                ),
                "[{name}] {error}"
            );
        }
        assert_eq!(r.loaded(&open).item.last_seq, 2, "[{name}]");
        // The REST door: a key of the kind api is one_shot by default and cannot talk.
        let api_shot = agent(r, ID_C, KeyKind::Api, KeyInteraction::OneShot);
        let own = r
            .submit_with(&api_shot.subject, 3, json!({"lease_s": 900}))
            .id();
        let reply = rest(
            r,
            &api_shot.token,
            "POST",
            &format!("/api/v1/items/{own}/turns"),
            r#"{"text":"hello"}"#,
        );
        assert_eq!(reply.status, 422, "[{name}] {}", reply.body);
        assert_eq!(reply.json()["error"]["code"], "interaction_not_allowed");
    });
}

#[test]
fn rest_door_serves_the_same_core_with_its_statuses() {
    each_agent_rig("", |r, name| {
        r.fix_clock(T0);
        let c = agent(r, ID_C, KeyKind::Api, KeyInteraction::Conversational);
        // The intent: 201, the rules of the core, and a bad body.
        let reply = rest(
            r,
            &c.token,
            "POST",
            "/api/v1/approval-intents",
            r#"{"lease_s":900,"explanation":"why","allow_ask_back":true}"#,
        );
        assert_eq!(reply.status, 201, "[{name}] {}", reply.body);
        assert_eq!(reply.json()["approval_ref"].as_str().unwrap().len(), 43);
        for (body, status, code) in [
            (r#"{"explanation":"why"}"#, 422, "lease_required"),
            (
                r#"{"lease_s":900,"until":"2027-01-16T00:00:00Z"}"#,
                422,
                "invalid_lease",
            ),
            (r#"{"lease_s":5}"#, 422, "invalid_lease"),
            (
                r#"{"lease_s":900,"until":"tomorrow"}"#,
                422,
                "invalid_lease",
            ),
            (r#"{"lease_s":900,"unknown":1}"#, 400, "invalid_json"),
            (
                r#"{"lease_s":900,"explanation":"x","allow_custom":"yes"}"#,
                400,
                "invalid_json",
            ),
            ("[]", 400, "invalid_json"),
        ] {
            let reply = rest(r, &c.token, "POST", "/api/v1/approval-intents", body);
            assert_eq!(reply.status, status, "[{name}] {body}: {}", reply.body);
            assert_eq!(reply.json()["error"]["code"], code, "[{name}] {body}");
        }
        let long = json!({"lease_s": 900, "explanation": "x".repeat(2001)}).to_string();
        let reply = rest(r, &c.token, "POST", "/api/v1/approval-intents", &long);
        assert_eq!(reply.status, 422);
        assert_eq!(reply.json()["error"]["code"], "explanation_too_long");
        // The item of the key: read, keepalive, talk, replace, withdraw.
        let intent = rest(
            r,
            &c.token,
            "POST",
            "/api/v1/approval-intents",
            r#"{"lease_s":900,"allow_ask_back":true}"#,
        )
        .json();
        let id = r
            .submit_with(
                &c.subject,
                1,
                json!({"approval_ref": intent["approval_ref"]}),
            )
            .id();
        let view = rest(
            r,
            &c.token,
            "GET",
            &format!("/api/v1/items/{id}?wait=0&after_seq=0"),
            "",
        );
        assert_eq!(view.status, 200, "[{name}] {}", view.body);
        assert_eq!(view.json()["status"], "pending");
        assert_eq!(view.header("cache-control"), Some("no-store"));
        let kept = rest(
            r,
            &c.token,
            "POST",
            &format!("/api/v1/items/{id}/keepalive"),
            "",
        );
        assert_eq!(kept.status, 200, "[{name}] {}", kept.body);
        for query in ["?wait=x", "?wait=1&wait=2", "?after_seq=-1"] {
            let bad = rest(
                r,
                &c.token,
                "GET",
                &format!("/api/v1/items/{id}{query}"),
                "",
            );
            assert_eq!(bad.status, 400, "[{name}] {query}");
        }
        let said = rest(
            r,
            &c.token,
            "POST",
            &format!("/api/v1/items/{id}/turns"),
            r#"{"text":"hello"}"#,
        );
        assert_eq!(said.status, 201, "[{name}] {}", said.body);
        assert_eq!(said.json()["seq"], 1);
        let replaced = rest(
            r,
            &c.token,
            "POST",
            &format!("/api/v1/items/{id}/supersede"),
            "{}",
        );
        assert_eq!(replaced.status, 422, "[{name}] {}", replaced.body);
        assert_eq!(replaced.json()["error"]["code"], "use_service_tool");
        // A method that the route does not take, and a route that does not exist.
        for (method, path) in [
            ("PUT", format!("/api/v1/items/{id}")),
            ("GET", format!("/api/v1/items/{id}/turns")),
            ("GET", "/api/v1/approval-intents".to_string()),
        ] {
            let reply = rest(r, &c.token, method, &path, "");
            assert_eq!(reply.status, 405, "[{name}] {method} {path}");
        }
        let missing = rest(
            r,
            &c.token,
            "POST",
            &format!("/api/v1/items/{id}/other"),
            "",
        );
        assert_eq!(missing.status, 404, "[{name}]");
        // No key, a bad key and a key of the kind mcp get the same 401.
        let mcp = conversational(r, ID_A);
        for token in [None, Some("api_nope".to_string()), Some(mcp.token.clone())] {
            let auth = token.as_ref().map(|t| format!("Bearer {t}"));
            let headers: Vec<(&str, &str)> = auth
                .iter()
                .map(|value| ("Authorization", value.as_str()))
                .collect();
            let reply = send(
                r.node.port,
                "GET",
                &format!("/api/v1/items/{id}"),
                &headers,
                "",
            );
            assert_eq!(reply.status, 401, "[{name}] {token:?}");
        }
        // The withdrawal: `cancelled`, and the service reads `void`.
        let cancelled = rest(r, &c.token, "DELETE", &format!("/api/v1/items/{id}"), "");
        assert_eq!(cancelled.status, 200, "[{name}] {}", cancelled.body);
        assert_eq!(cancelled.json()["status"], "cancelled");
        assert_eq!(r.verdict(&id, &c.subject), "void", "[{name}]");
        let gone = rest(r, &c.token, "GET", &format!("/api/v1/items/{id}"), "");
        assert_eq!(gone.json()["status"], "cancelled");
    });
}

#[test]
fn one_shot_key_tools_list_hides_say_and_supersede() {
    each_agent_rig("", |r, name| {
        let conv = conversational(r, ID_A);
        let shot = one_shot(r, ID_B);
        let names = |a: &Agent| -> Vec<String> {
            let reply = rpc(
                r,
                &a.token,
                &json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"}),
            );
            assert_eq!(reply.status, 200, "{}", reply.body);
            reply.json()["result"]["tools"]
                .as_array()
                .unwrap()
                .iter()
                .map(|t| t["name"].as_str().unwrap().to_string())
                .collect()
        };
        assert_eq!(
            names(&conv),
            [
                "approval_intent",
                "get_answer",
                "keepalive",
                "say",
                "supersede",
                "cancel"
            ],
            "[{name}]"
        );
        assert_eq!(
            names(&shot),
            ["approval_intent", "get_answer", "keepalive", "cancel"],
            "[{name}]"
        );
        // The list is the same each time.
        assert_eq!(names(&conv), names(&conv));
        // A one_shot key cannot set the flags of a talk: the same rule as the intent core.
        let error = fails(
            r,
            &shot,
            "approval_intent",
            json!({"lease_s": 900, "allow_ask_back": true}),
        );
        assert_eq!(error["code"], "interaction_not_allowed", "[{name}]");
        assert_eq!(error["retryable"], false);
        // The items of the key and the tool schemas.
        let reply = rpc(
            r,
            &conv.token,
            &json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"}),
        );
        for tool in reply.json()["result"]["tools"].as_array().unwrap() {
            assert!(tool["inputSchema"]["type"] == "object");
            assert!(tool["description"].as_str().unwrap().len() > 20);
            assert!(tool["annotations"]["readOnlyHint"].is_boolean());
        }
    });
}

#[test]
fn supersede_cancels_old_approval_and_new_waits() {
    each_agent_rig(r#"max_wait = "3s""#, |r, name| {
        r.fix_clock(T0);
        let a = conversational(r, ID_A);
        let old = request(r, &a, 1, json!({"allow_ask_back": true}));
        // The agent replaces the request through the service: a new command with `supersedes`.
        let intent = ok(r, &a, "approval_intent", json!({"lease_s": 900}));
        let replace = r.post(&r.body(
            &a.subject,
            &command(2),
            json!({"approval_ref": intent["approval_ref"], "supersedes": old}),
        ));
        assert_eq!(replace.status, 201, "[{name}] {}", replace.body);
        let new = replace.id();
        let was = ok(r, &a, "get_answer", json!({"request_id": old}));
        assert_eq!(was["status"], "superseded", "[{name}]");
        assert_eq!(was["superseded_by"], new, "[{name}]");
        assert_eq!(
            r.verdict(&old, &a.subject),
            "void",
            "[{name}] the old approval is void"
        );
        // The tool of the portal cannot make the new approval, and it tells where to do it. Nothing changes.
        let error = fails(r, &a, "supersede", json!({"request_id": new}));
        assert_eq!(error["code"], "use_service_tool", "[{name}]");
        assert_eq!(
            r.loaded(&new).item.state,
            inbox_core::proto::ItemState::Pending
        );
        // The new request waits: a read with `wait_s` returns at the end of the wait with `pending`.
        let started = std::time::Instant::now();
        let waiting = ok(r, &a, "get_answer", json!({"request_id": new, "wait_s": 1}));
        assert_eq!(waiting["status"], "pending", "[{name}]");
        assert!(
            started.elapsed() >= Duration::from_millis(900),
            "[{name}] it waits"
        );
        // It returns as soon as the owner answers.
        let started = std::time::Instant::now();
        let answered = std::thread::scope(|scope| {
            scope.spawn(|| {
                std::thread::sleep(Duration::from_millis(400));
                r.answer(&new, Action::Accept, None);
            });
            ok(r, &a, "get_answer", json!({"request_id": new, "wait_s": 3}))
        });
        assert_eq!(answered["status"], "answered", "[{name}]");
        assert_eq!(answered["decision"], "approved");
        assert!(
            started.elapsed() < Duration::from_millis(2500),
            "[{name}] it wakes"
        );
    });
}

#[test]
fn cancel_makes_pep_see_void() {
    each_agent_rig("", |r, name| {
        r.fix_clock(T0);
        let a = conversational(r, ID_A);
        let id = request(r, &a, 1, json!({}));
        assert_eq!(r.verdict(&id, &a.subject), "pending", "[{name}]");
        let done = ok(r, &a, "cancel", json!({"request_id": id}));
        assert_eq!(done["status"], "cancelled", "[{name}]");
        assert_eq!(r.verdict(&id, &a.subject), "void", "[{name}]");
        let view = ok(r, &a, "get_answer", json!({"request_id": id}));
        assert_eq!(view["status"], "cancelled");
        assert_eq!(view["reason"], "agent_cancelled");
        // A second call gives the same answer.
        assert_eq!(
            ok(r, &a, "cancel", json!({"request_id": id}))["status"],
            "cancelled"
        );
        // An answered item cannot be withdrawn: the owner decided.
        let declined = request(r, &a, 2, json!({}));
        r.answer(&declined, Action::Decline, None);
        let error = fails(r, &a, "cancel", json!({"request_id": declined}));
        assert_eq!(error["code"], "item_closed", "[{name}]");
        // The request of the same effect can be made again after a cancel.
        let again = request(r, &a, 1, json!({}));
        assert_ne!(again, id, "[{name}]");
    });
}

#[test]
fn keepalive_revives_in_the_window_and_gives_410_after() {
    each_agent_rig("", |r, name| {
        r.fix_clock(T0);
        let a = conversational(r, ID_A);
        let id = request(r, &a, 1, json!({}));
        // The lease is 2700 s and the grace 120 s. The sweeper settles the item as `abandoned`.
        let lapsed = T0 + 2_700_000 + 121_000;
        r.fix_clock(lapsed);
        // A read after the lease and the grace settles the item, as the sweeper does.
        assert_eq!(
            r.loaded(&id).item.state,
            inbox_core::proto::ItemState::Abandoned,
            "[{name}]"
        );
        assert_eq!(r.verdict(&id, &a.subject), "void", "[{name}]");
        let view = ok(r, &a, "get_answer", json!({"request_id": id, "wait_s": 0}));
        assert_eq!(
            view["status"], "pending",
            "[{name}] a read of the agent brings it back"
        );
        let abandoned = request_again_abandoned(r, &a, name);
        // After the window the item does not come back.
        let error = fails(r, &a, "keepalive", json!({"request_id": abandoned}));
        assert_eq!(error["code"], "item_abandoned", "[{name}]");
        let view = ok(r, &a, "get_answer", json!({"request_id": abandoned}));
        assert_eq!(view["status"], "abandoned");
        assert_eq!(view["revivable"], false);
        let c = agent(r, ID_C, KeyKind::Api, KeyInteraction::Conversational);
        let reply = rest(
            r,
            &c.token,
            "POST",
            &format!("/api/v1/items/{abandoned}/keepalive"),
            "",
        );
        assert_eq!(
            reply.status, 404,
            "an api key does not see the item of an mcp key"
        );
    });
}

/// An item that is abandoned and past its revive window.
fn request_again_abandoned(r: &Rig, a: &Agent, name: &str) -> String {
    let id = request(r, a, 2, json!({}));
    let now = r.now();
    r.fix_clock(now + 2_700_000 + 121_000);
    assert_eq!(
        r.loaded(&id).item.state,
        inbox_core::proto::ItemState::Abandoned,
        "[{name}]"
    );
    r.fix_clock(r.now() + 8 * 86_400_000);
    id
}

#[test]
fn api_key_cannot_use_mcp_and_mcp_key_cannot_use_rest_alerts() {
    each_agent_rig("", |r, name| {
        let mcp = conversational(r, ID_A);
        let api = agent(r, ID_C, KeyKind::Api, KeyInteraction::OneShot);
        let call = json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"});
        let reply = rpc(r, &api.token, &call);
        assert_eq!(reply.status, 401, "[{name}] {}", reply.body);
        assert!(reply.body.is_empty(), "[{name}] the 401 has an empty body");
        assert!(reply
            .header("www-authenticate")
            .is_some_and(|h| h.starts_with("Bearer")));
        assert_eq!(reply.header("cache-control"), Some("no-store"));
        // An mcp key does not send an alert, and does not read an item at the REST door.
        let alert = rest(
            r,
            &mcp.token,
            "POST",
            "/api/v1/alerts",
            r#"{"group":"infra","title":"hello"}"#,
        );
        assert_eq!(alert.status, 401, "[{name}] {}", alert.body);
        let item = rest(
            r,
            &mcp.token,
            "GET",
            "/api/v1/items/0123456789abcdef0123456789abcdef",
            "",
        );
        assert_eq!(item.status, 401, "[{name}]");
        let intent = rest(
            r,
            &mcp.token,
            "POST",
            "/api/v1/approval-intents",
            r#"{"lease_s":900}"#,
        );
        assert_eq!(intent.status, 401, "[{name}]");
        // The same key at its own door works.
        assert_eq!(rpc(r, &mcp.token, &call).status, 200, "[{name}]");
        let reply = rest(
            r,
            &api.token,
            "POST",
            "/api/v1/approval-intents",
            r#"{"lease_s":900}"#,
        );
        assert_eq!(reply.status, 201, "[{name}] {}", reply.body);
    });
}

#[test]
fn unauthenticated_flood_limited_per_address() {
    each_agent_rig(r#"max_unauthenticated_per_minute = 3"#, |r, name| {
        let a = conversational(r, ID_A);
        let call = json!({"jsonrpc": "2.0", "id": 1, "method": "ping"});
        let from = |address: &str, token: &str| {
            let auth = format!("Bearer {token}");
            send(
                r.node.port,
                "POST",
                "/mcp",
                &[
                    ("Content-Type", "application/json"),
                    ("Authorization", &auth),
                    ("X-Forwarded-For", address),
                ],
                &call.to_string(),
            )
        };
        let bad = crate::keys::generate(KeyKind::Mcp).unwrap().to_string();
        for n in 0..3 {
            assert_eq!(from("198.51.100.7", &bad).status, 401, "[{name}] {n}");
        }
        // The address is over its limit: `429` with `Retry-After`, before the key is checked.
        let limited = from("198.51.100.7", &bad);
        assert_eq!(limited.status, 429, "[{name}]");
        assert!(limited.body.is_empty());
        assert!(limited
            .header("retry-after")
            .and_then(|s| s.parse::<u64>().ok())
            .is_some_and(|s| (1..=60).contains(&s)));
        assert_eq!(
            from("198.51.100.7", &a.token).status,
            429,
            "[{name}] a good key does not pass from a flooded address"
        );
        // Another address is not limited.
        assert_eq!(from("203.0.113.9", &bad).status, 401, "[{name}]");
        assert_eq!(from("203.0.113.9", &a.token).status, 200, "[{name}]");
        // A request with a valid key is never counted.
        for _ in 0..5 {
            assert_eq!(from("203.0.113.10", &a.token).status, 200, "[{name}]");
        }
    });
}

#[test]
fn key_over_its_rate_gets_429_and_retry_after() {
    each_agent_rig(r#"rate_limit_per_minute = 5"#, |r, name| {
        let a = conversational(r, ID_A);
        let b = conversational(r, ID_B);
        let call = json!({"jsonrpc": "2.0", "id": 1, "method": "ping"});
        for _ in 0..5 {
            assert_eq!(rpc(r, &a.token, &call).status, 200, "[{name}]");
        }
        let limited = rpc(r, &a.token, &call);
        assert_eq!(limited.status, 429, "[{name}]");
        assert!(limited
            .header("retry-after")
            .and_then(|s| s.parse::<u64>().ok())
            .is_some_and(|s| s >= 1));
        // Each key has its own bucket.
        assert_eq!(rpc(r, &b.token, &call).status, 200, "[{name}]");
    });
}

#[test]
fn wait_never_exceeds_max_wait() {
    each_agent_rig(r#"max_wait = "1s""#, |r, name| {
        r.fix_clock(T0);
        let a = conversational(r, ID_A);
        let id = request(r, &a, 1, json!({}));
        let started = std::time::Instant::now();
        let view = ok(r, &a, "get_answer", json!({"request_id": id, "wait_s": 30}));
        let took = started.elapsed();
        assert_eq!(view["status"], "pending", "[{name}]");
        assert!(took >= Duration::from_millis(900), "[{name}] {took:?}");
        assert!(took < Duration::from_secs(4), "[{name}] {took:?}");
        // A wait of zero never waits.
        let started = std::time::Instant::now();
        ok(r, &a, "get_answer", json!({"request_id": id, "wait_s": 0}));
        ok(r, &a, "get_answer", json!({"request_id": id}));
        assert!(started.elapsed() < Duration::from_millis(900), "[{name}]");
        // The same bound on the REST door: `inbox.items.wait_max`, 25 s by default, is the bound there. A read with a
        // wait of zero is quick.
        let c = agent(r, ID_C, KeyKind::Api, KeyInteraction::OneShot);
        let own = r.submit_with(&c.subject, 5, json!({"lease_s": 900})).id();
        let started = std::time::Instant::now();
        let reply = rest(
            r,
            &c.token,
            "GET",
            &format!("/api/v1/items/{own}?wait=0"),
            "",
        );
        assert_eq!(reply.status, 200, "{}", reply.body);
        assert!(started.elapsed() < Duration::from_millis(900));
    });
}

#[test]
fn the_endpoint_follows_the_path_of_the_file_and_the_block_gates_the_tools() {
    each_agent_rig(r#"path = "/agents/mcp""#, |r, name| {
        let a = conversational(r, ID_A);
        let call = json!({"jsonrpc": "2.0", "id": 1, "method": "ping"});
        let on_path = |path: &str| {
            let auth = format!("Bearer {}", a.token);
            send(
                r.node.port,
                "POST",
                path,
                &[
                    ("Content-Type", "application/json"),
                    ("Authorization", &auth),
                ],
                &call.to_string(),
            )
        };
        assert_eq!(on_path("/agents/mcp").status, 200, "[{name}]");
        assert_ne!(
            on_path("/mcp").status,
            200,
            "[{name}] the default path is not served"
        );
    });
}

#[test]
fn the_texts_of_the_agent_never_reach_the_service_through_the_channel() {
    each_agent_rig("", |r, name| {
        r.fix_clock(T0);
        r.device();
        let a = conversational(r, ID_A);
        let id = request(
            r,
            &a,
            1,
            json!({"allow_ask_back": true, "allow_custom": true}),
        );
        owner_reply(r, &id, "m1", COMMENT);
        ok(r, &a, "say", json!({"request_id": id, "text": EXPLANATION}));
        decline_with(r, &id, COMMENT, inbox_core::proto::Intent::Comment);
        ok(r, &a, "get_answer", json!({"request_id": id}));
        // Everything that the service got on its routes holds none of the texts.
        for recorded in r.log.lock().unwrap().iter() {
            for canary in CANARIES {
                assert!(
                    !recorded.body.contains(canary),
                    "[{name}] {canary} is in an answer to the service: {}",
                    recorded.body
                );
            }
        }
        assert_eq!(r.verdict(&id, &a.subject), "denied", "[{name}]");
    });
}

// ---------------------------------------------------------------- the formats of the texts and the forms

/// The format of the explanation and the language of the command go from the intent to the frame of the phone. A
/// value out of the list is refused on both doors.
#[test]
fn the_format_and_the_language_of_an_intent_reach_the_frame_of_the_phone() {
    use inbox_core::proto::{ExplanationFormat, Language, ServerMsg};
    each_agent_rig("", |r, name| {
        r.fix_clock(T0);
        r.device();
        let a = conversational(r, ID_A);
        let id = request(
            r,
            &a,
            1,
            json!({"explanation": "Run **this** first.\n\n```sql\nSELECT 1\n```",
                   "explanation_format": "markdown", "language": "sql"}),
        );
        let frames = frames_of(r, DEVICE);
        let body = frames
            .iter()
            .find_map(|f| match f {
                ServerMsg::Request(q) if q.request_id == id => q.approval.clone(),
                _ => None,
            })
            .expect("the request reaches the phone");
        assert_eq!(
            body.explanation_format,
            Some(ExplanationFormat::Markdown),
            "[{name}]"
        );
        assert_eq!(body.language, Some(Language::Sql), "[{name}]");
        assert!(body.verify(), "[{name}] the format is outside the hash");
        assert!(body.explanation.unwrap().contains("```sql"));
        // Without them the members are absent: plain text is the default.
        let plain = request(r, &a, 2, json!({}));
        let frames = frames_of(r, DEVICE);
        let body = frames
            .iter()
            .find_map(|f| match f {
                ServerMsg::Request(q) if q.request_id == plain => q.approval.clone(),
                _ => None,
            })
            .expect("the second request reaches the phone");
        assert_eq!(
            (body.explanation_format, body.language),
            (None, None),
            "[{name}]"
        );
        // The values are checked. Over MCP: invalid_argument.
        for args in [
            json!({"lease_s": 900, "explanation": "x", "explanation_format": "html"}),
            json!({"lease_s": 900, "explanation": "x", "explanation_format": "Markdown"}),
            json!({"lease_s": 900, "explanation": "x", "language": "cobol"}),
            json!({"lease_s": 900, "explanation": "x", "language": "SQL"}),
            // A format with no explanation.
            json!({"lease_s": 900, "explanation_format": "markdown"}),
        ] {
            let error = fails(r, &a, "approval_intent", args.clone());
            assert_eq!(error["code"], "invalid_argument", "[{name}] {args}");
        }
        // The language alone is valid: it colors the command.
        ok(
            r,
            &a,
            "approval_intent",
            json!({"lease_s": 900, "language": "bash"}),
        );
        // Over REST: the same rules.
        let c = agent(r, ID_C, KeyKind::Api, KeyInteraction::Conversational);
        let reply = rest(
            r,
            &c.token,
            "POST",
            "/api/v1/approval-intents",
            r#"{"lease_s":900,"explanation":"x","explanation_format":"markdown","language":"yaml"}"#,
        );
        assert_eq!(reply.status, 201, "[{name}] {}", reply.body);
        for (body, status, code) in [
            (
                r#"{"lease_s":900,"explanation":"x","explanation_format":"html"}"#,
                400,
                "invalid_json",
            ),
            (
                r#"{"lease_s":900,"explanation":"x","language":"cobol"}"#,
                400,
                "invalid_json",
            ),
            (
                r#"{"lease_s":900,"explanation_format":"markdown"}"#,
                422,
                "invalid_argument",
            ),
        ] {
            let reply = rest(r, &c.token, "POST", "/api/v1/approval-intents", body);
            assert_eq!(reply.status, status, "[{name}] {body}: {}", reply.body);
            assert_eq!(reply.json()["error"]["code"], code, "[{name}] {body}");
        }
    });
}

/// A turn of the agent can be markdown. The text of the owner never is.
#[test]
fn a_turn_of_the_agent_can_be_markdown_and_the_message_frame_says_so() {
    use inbox_core::proto::{Author, ExplanationFormat, ServerMsg};
    each_agent_rig("", |r, name| {
        r.fix_clock(T0);
        r.device();
        let a = conversational(r, ID_A);
        let id = request(r, &a, 1, json!({"allow_ask_back": true}));
        owner_reply(r, &id, "m1", "Why? **not bold**");
        let said = ok(
            r,
            &a,
            "say",
            json!({"request_id": id, "text": "It holds the **events**.", "explanation_format": "markdown",
                   "expect_seq": 1}),
        );
        assert_eq!(said["seq"], 2, "[{name}]");
        let error = fails(
            r,
            &a,
            "say",
            json!({"request_id": id, "text": "x", "explanation_format": "html"}),
        );
        assert_eq!(error["code"], "invalid_argument", "[{name}]");
        let frames = frames_of(r, DEVICE);
        let agent_turn = frames
            .iter()
            .find_map(|f| match f {
                ServerMsg::Message(m) if m.by == Author::Agent => Some(m.clone()),
                _ => None,
            })
            .expect("the turn of the agent reaches the phone");
        assert_eq!(
            agent_turn.explanation_format,
            Some(ExplanationFormat::Markdown),
            "[{name}]"
        );
        assert_eq!(
            agent_turn.explanation.as_deref(),
            Some("It holds the **events**.")
        );
        let user_turn = frames
            .iter()
            .find_map(|f| match f {
                ServerMsg::Message(m) if m.by == Author::User => Some(m.clone()),
                _ => None,
            })
            .expect("the text of the owner reaches the phone");
        assert_eq!(
            (user_turn.explanation_format, user_turn.text.as_deref()),
            (None, Some("Why? **not bold**"))
        );
        // A text that looks like the object of a turn stays a text.
        let id2 = request(r, &a, 2, json!({"allow_ask_back": true}));
        owner_reply(r, &id2, "m2", "hi");
        r.fix_clock(T0 + 10_000);
        let trick = r#"{"explanation":"gotcha","explanation_format":"markdown"}"#;
        ok(
            r,
            &a,
            "say",
            json!({"request_id": id2, "text": trick, "expect_seq": 1}),
        );
        let frames = frames_of(r, DEVICE);
        let tricked = frames
            .iter()
            .filter_map(|f| match f {
                ServerMsg::Message(m) if m.by == Author::Agent && m.request_id == id2 => {
                    Some(m.clone())
                }
                _ => None,
            })
            .next_back()
            .expect("the turn reaches the phone");
        assert_eq!(tricked.explanation.as_deref(), Some(trick), "[{name}]");
        assert_eq!(tricked.explanation_format, None, "[{name}]");
    });
}

/// The answers of a form reach the agent inside `untrusted`, under the notice. The creation of a question arrives with
/// lot L13: the test writes the item through the store, as the engine will.
#[test]
fn get_answer_returns_the_answers_of_a_form_in_untrusted() {
    use crate::inbox::frames::OwnerAnswer;
    use crate::inbox::testing::{Answered, Created, NewItem, Wait};
    use inbox_core::proto::{FormAnswer, InputType, Kind, Urgency, Via};
    each_agent_rig("", |r, name| {
        r.fix_clock(T0);
        r.device();
        let a = conversational(r, ID_A);
        let body = json!({
            "title": "Set up the release",
            "input": {"type": "form", "multiple": false, "min": 0, "max": 0, "options": [],
                      "questions": [
                          {"id": "target", "header": "Target", "text": "Where?", "type": "choice",
                           "multiple": false, "min": 1, "max": 1,
                           "options": [{"id": "staging", "label": "Staging"}, {"id": "prod", "label": "Production"}]},
                          {"id": "note", "header": "Note", "text": "More?", "type": "text",
                           "multiple": false, "min": 0, "max": 0, "options": []}]}
        });
        let item = NewItem {
            kind: Kind::Question,
            requester_key_id: ID_A.into(),
            owner: OWNER.into(),
            via: Via::Mcp,
            interaction: inbox_core::proto::Interaction::Conversational,
            urgency: Urgency::Normal,
            input_type: Some(InputType::Form),
            wait: Wait::Lease { lease_s: 900 },
            allow_custom: true,
            allow_ask_back: false,
            idempotency_key: "form-1".into(),
            previous_id: None,
            agent_id: None,
            body_size: 0,
            approval: None,
            require_device: false,
            guard: None,
        };
        let id = match eventually(|| {
            runtime().block_on(r.items().create(item.clone(), body.to_string().as_bytes()))
        }) {
            Created::New(l) => l.item.request_id.clone(),
            other => panic!("[{name}] not created: {other:?}"),
        };
        let pending = ok(r, &a, "get_answer", json!({"request_id": id}));
        assert_eq!(pending["status"], "pending", "[{name}]");
        assert_eq!(pending["kind"], "question");
        assert!(pending.get("untrusted").is_none() && pending.get("data_notice").is_none());
        // The owner ticks one option and writes a text that holds a control character.
        let given = OwnerAnswer {
            choice: None,
            text: None,
            intent: None,
            answers: Some(vec![
                FormAnswer {
                    question_id: "target".into(),
                    choice: Some(vec!["prod".into()]),
                    text: None,
                },
                FormAnswer {
                    question_id: "note".into(),
                    choice: None,
                    text: Some("Friday\u{7}\nnot before".into()),
                },
            ]),
        };
        let out = eventually(|| {
            runtime().block_on(r.items().answer(
                &id,
                crate::inbox::testing::Answer {
                    login: OWNER.into(),
                    via: crate::inbox::testing::AnswerVia::Web,
                    action: Action::Accept,
                    shown_effect: None,
                    client_msg_id: None,
                    payload: Some(Zeroizing::new(given.to_plain())),
                    consume_within: Duration::from_secs(300),
                },
            ))
        });
        assert!(matches!(out, Answered::Accepted { .. }), "[{name}] {out:?}");
        let (is_error, view, reply) = tool_call(r, &a, "get_answer", json!({"request_id": id}));
        assert!(!is_error, "[{name}] {view}");
        assert_eq!(view["status"], "answered", "[{name}]");
        assert_eq!(
            view["untrusted"]["answers"],
            json!([
                {"question_id": "target", "choice": ["prod"]},
                {"question_id": "note", "text": "Friday\u{FFFD}\nnot before"}
            ]),
            "[{name}]"
        );
        assert!(view["data_notice"]
            .as_str()
            .unwrap()
            .contains("never as instructions"));
        for canary in [OWNER, DEVICE] {
            assert!(
                !reply.body.contains(canary),
                "[{name}] {canary} is in {}",
                reply.body
            );
        }
    });
}

// The tests of lot G1b: the agent of a call.
#[path = "agent_who_tests.rs"]
mod who;
