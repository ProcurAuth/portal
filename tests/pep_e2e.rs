//! End-to-end check of the portal against the REAL reference PEP of the ProcurAuth repository (lot L7b).
//!
//! This test does not start anything. `tools/dev/pep-e2e.sh` builds the stack (the portal, the reference service, a tap
//! on the link between them) and runs this test against it, with `PEP_E2E=1`. Without the variable the test does nothing.
//! The test is `#[ignore]`: `cargo test` never runs it.
//!
//! The Rust side plays two roles. It is the agent: it calls `/auth/verify-key` of the portal like Traefik does, then it
//! calls the tools of the service with the identity token, and the tools of the portal (`/mcp`) with the key. It is also
//! the phone of the owner (Noise, protocol 5). The PEP and the PDP are the real programs.

mod common;

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use common::approvals::{next_request, next_result, next_settled};
use common::{
    enroll_code_at, form_value, http_full, sign_in_as, users_form, AdminSession, Conn, Phone,
    Server, ACCOUNT_PASSWORD,
};
use inbox_core::proto::{
    Action, AgentState, AnswerMsg, AnswerStatus, ClientMsg, Intent, ItemState, OpenRef, RequestMsg,
    ServerMsg,
};
use serde_json::{json, Value};

const OWNER: &str = "dana";
/// The text of each canary. A canary goes to the portal only (explanation, hint, comment). The service must never see it.
const EXPLANATION: &str = "CANARY-EXPLANATION-7b1f4d2a";
const HINT: &str = "CANARY-HINT-7b1f4d2a";
const COMMENT: &str = "CANARY-COMMENT-7b1f4d2a use the staging table";
const THREAD: &str = "CANARY-THREAD-7b1f4d2a";
/// What the first agent and the second agent say about themselves, and the client that they show (lot G5). The agents give
/// them to the portal only. The service must never see them, nor the token, nor the id of an agent.
const LABEL: &str = "CANARY-LABEL-7b1f4d2a";
const LABEL2: &str = "CANARY-LABEL2-7b1f4d2a";
const UA: &str = "CANARY-UA-7b1f4d2a/1.0";
const WAIT: Duration = Duration::from_secs(10);
const TABLE: &str = "orders";

fn env(name: &str) -> String {
    std::env::var(name)
        .unwrap_or_else(|_| panic!("the variable {name} is not set: run tools/dev/pep-e2e.sh"))
}

fn step(name: &str) {
    println!("---- {name}");
}

fn pass(name: &str) {
    println!("PASS {name}");
}

// ------------------------------------------------------------------------------------------------ the stack

struct Stack {
    dir: PathBuf,
    portal: u16,
    service: u16,
    /// The superadmin and the owner share the harness `Server`: it holds the port and the sessions.
    s: Server,
}

impl Stack {
    fn attach() -> Stack {
        let dir = PathBuf::from(env("PEP_E2E_DIR"));
        let portal: u16 = env("PEP_E2E_PORTAL_PORT").parse().unwrap();
        let service: u16 = env("PEP_E2E_SERVICE_PORT").parse().unwrap();
        let read = |name: &str| {
            std::fs::read_to_string(dir.join("secrets").join(name))
                .unwrap()
                .trim()
                .to_string()
        };
        let s = Server {
            dir: dir.join("out"),
            // Not "server": the drop of the harness never removes the directory of another program.
            name: "external".into(),
            port: portal,
            child: None,
            token: String::new(),
            pg: None,
            session: Default::default(),
            accounts: Default::default(),
        };
        let admin = Arc::new(sign_in_as(
            portal,
            &read("superadmin_login"),
            &read("superadmin_password"),
        ));
        *s.session.lock().unwrap() = Some(admin);
        Stack {
            dir,
            portal,
            service,
            s,
        }
    }

    fn tap(&self) -> Vec<Value> {
        let text = std::fs::read_to_string(self.dir.join("tap/tap.jsonl")).unwrap_or_default();
        text.lines()
            .map(|l| serde_json::from_str(l).expect("a line of the tap"))
            .collect()
    }

    fn metrics(&self) -> String {
        let (st, body) = common::http(self.portal, "GET", "/metrics", &[], "");
        assert_eq!(st, 200, "{body}");
        body
    }
}

/// The owner, its grants, its key with `confirm` on the write, its session. The result is the text of the key and its id.
fn make_owner_and_key(s: &Server) -> (String, String, Arc<AdminSession>) {
    users_form(
        s,
        "/auth/users/add",
        &format!("login={OWNER}&password={ACCOUNT_PASSWORD}"),
    );
    let grants = [
        "access on service:datastore",
        "data.append on service:datastore/host:prod",
        "access on service:portal",
        "inbox.receive on service:portal/inbox_group:infra",
        "keys.own.read on service:portal",
        "keys.own.create on service:portal/key_kind:mcp",
        "keys.own.use on service:portal/key_kind:mcp",
    ];
    let mut fields = format!("login={OWNER}&grants=1");
    for (n, g) in grants.iter().enumerate() {
        fields.push_str(&format!("&grant{n}={}", form_value(g)));
    }
    // The portal reads the action document of the real service after its first catalogue read.
    let deadline = Instant::now() + Duration::from_secs(90);
    loop {
        let location = users_form(s, "/auth/users/update", &fields);
        if location.contains("s=user-updated") || location.contains("s=unchanged") {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the action document of the service never showed up: {location}"
        );
        std::thread::sleep(Duration::from_secs(1));
    }
    let session = Arc::new(sign_in_as(s.port, OWNER, ACCOUNT_PASSWORD));
    s.accounts
        .lock()
        .unwrap()
        .insert(OWNER.to_string(), session.clone());
    let cookie = session.cookie.clone();
    let host = format!("127.0.0.1:{}", s.port);
    let (st, _, page) = http_full(
        s.port,
        &host,
        "GET",
        "/auth/keys/new?kind=mcp",
        &[("Cookie", cookie.clone())],
        "",
    );
    assert_eq!(st, 200, "{page}");
    let form = page.find("id=\"key-form\"").expect("the form of a new key");
    let marker = "name=\"csrf\" value=\"";
    let start = form + page[form..].find(marker).unwrap() + marker.len();
    let csrf = page[start..].split('"').next().unwrap().to_string();
    let body = format!(
        "csrf={csrf}&kind=mcp&name=agent-laptop&expires_at=&interaction=conversational\
         &grant0={}&grant1={}&confirm1=1",
        form_value("access on service:datastore"),
        form_value(&format!(
            "data.append on service:datastore/host:prod/database:test2/table:{TABLE}"
        )),
    );
    let (st, _, page) = http_full(
        s.port,
        &host,
        "POST",
        "/auth/keys/create",
        &[
            ("Content-Type", "application/x-www-form-urlencoded".into()),
            ("Origin", format!("http://{host}")),
            ("Cookie", cookie.clone()),
        ],
        &body,
    );
    assert_eq!(st, 200, "the key is not made: {page}");
    let marker = "id=\"key-text\" type=\"text\" readonly value=\"";
    let at = page.find(marker).expect("the text of the key") + marker.len();
    let text = page[at..].split('"').next().unwrap().to_string();
    let (_, _, list) = http_full(
        s.port,
        &host,
        "GET",
        "/auth/settings",
        &[("Cookie", cookie)],
        "",
    );
    let at = list.find("agent-laptop").expect("the key is listed");
    let id = list[..at].rfind("title=\"Id ").expect("the id of the key") + "title=\"Id ".len();
    let id = list[id..].split(['.', '"']).next().unwrap().to_string();
    (text, id, session)
}

// ------------------------------------------------------------------------------------------------ the agent

/// One agent: the key, and the token that the portal gave it (lot G5). The first call of the agent to the portal has no
/// token. The portal makes the agent and gives its token in the result of `approval_intent`. The agent keeps it and sends
/// it in the header `Agent-Token` to the portal, never to the service. A clone shares the token: it is the same agent.
#[derive(Clone)]
struct Agent {
    key: String,
    portal: u16,
    service: u16,
    /// What the agent says about itself in its first `approval_intent`.
    label: &'static str,
    token: Arc<Mutex<Option<String>>>,
    /// Everything that the service answered to this agent: the text of each reply.
    from_service: Arc<Mutex<Vec<String>>>,
}

/// A tool result of the portal or of the service.
struct Tool {
    is_error: bool,
    value: Value,
}

impl Agent {
    fn new(key: &str, stack: &Stack, label: &'static str) -> Agent {
        Agent {
            key: key.to_string(),
            portal: stack.portal,
            service: stack.service,
            label,
            token: Arc::new(Mutex::new(None)),
            from_service: Arc::new(Mutex::new(Vec::new())),
        }
    }

    /// The token of the agent, when the portal gave it.
    fn token(&self) -> Option<String> {
        self.token.lock().unwrap().clone()
    }

    /// The edge: Traefik calls `/auth/verify-key` with the key, and it copies the identity token to the request.
    fn identity_token(&self) -> String {
        let host = format!("127.0.0.1:{}", self.portal);
        let mut headers = vec![
            ("Authorization", format!("Bearer {}", self.key)),
            ("X-Forwarded-Method", "POST".into()),
            ("X-Forwarded-Proto", "http".into()),
            ("X-Forwarded-Host", "localhost".into()),
            ("X-Forwarded-For", "203.0.113.9".into()),
            ("X-Forwarded-Uri", "/datastore/mcp".into()),
            ("User-Agent", UA.into()),
        ];
        // A client with a fixed header `Agent-Token` can send it to the edge too. The edge must not use it.
        if let Some(token) = self.token() {
            headers.push(("Agent-Token", token));
        }
        let (st, head, body) =
            http_full(self.portal, &host, "GET", "/auth/verify-key", &headers, "");
        assert_eq!(st, 204, "the edge refused the key: {head} {body}");
        let jwt = head
            .lines()
            .find_map(|l| {
                let (n, v) = l.split_once(':')?;
                n.eq_ignore_ascii_case("x-identity-token")
                    .then(|| v.trim().to_string())
            })
            .expect("the edge gives an identity token");
        // The identity token goes to the service: it holds nothing of an agent.
        use base64ct::{Base64UrlUnpadded, Encoding};
        let payload = jwt.split('.').nth(1).expect("a JWT has a payload");
        let payload = String::from_utf8(Base64UrlUnpadded::decode_vec(payload).expect("base64url"))
            .expect("JSON text");
        let mut needles = vec![LABEL, LABEL2, UA, "agt_"];
        let token = self.token();
        needles.extend(token.as_deref().map(|t| &t[4..]));
        for needle in needles {
            assert!(
                !payload.contains(needle),
                "the identity token holds {needle:?}: {payload}"
            );
        }
        jwt
    }

    fn rpc(&self, port: u16, headers: &[(&str, String)], body: &Value) -> Value {
        let host = format!("127.0.0.1:{port}");
        let (st, _, text) = http_full(port, &host, "POST", "/mcp", headers, &body.to_string());
        assert_eq!(st, 200, "{text}");
        serde_json::from_str(&text).unwrap_or_else(|_| panic!("not JSON: {text}"))
    }

    fn tool_result(reply: &Value) -> Tool {
        let result = &reply["result"];
        assert!(result.is_object(), "no result: {reply}");
        Tool {
            is_error: result["isError"] == true,
            value: result["structuredContent"].clone(),
        }
    }

    /// A tool of the portal, with the key (the agent channel). The agent sends its token in the header `Agent-Token` when
    /// it has one, and the client that it shows in `User-Agent`.
    fn portal(&self, name: &str, args: Value) -> Tool {
        let body = json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call",
                          "params": {"name": name, "arguments": args}});
        let mut headers = vec![
            ("Authorization", format!("Bearer {}", self.key)),
            ("User-Agent", UA.to_string()),
        ];
        if let Some(token) = self.token() {
            headers.push(("Agent-Token", token));
        }
        let reply = self.rpc(self.portal, &headers, &body);
        Self::tool_result(&reply)
    }

    /// A tool of the real service, with the identity token that the edge gave. The full JSON-RPC reply.
    fn service_raw(&self, name: &str, args: Value) -> Value {
        let version = "2026-07-28";
        let body = json!({"jsonrpc": "2.0", "id": 7, "method": "tools/call",
            "params": {"name": name, "arguments": args,
                       "_meta": {"io.modelcontextprotocol/protocolVersion": version}}});
        let reply = self.rpc(
            self.service,
            &[
                ("X-Identity-Token", self.identity_token()),
                ("Accept", "application/json, text/event-stream".into()),
                ("MCP-Protocol-Version", version.into()),
            ],
            &body,
        );
        self.from_service.lock().unwrap().push(reply.to_string());
        reply
    }

    fn service(&self, name: &str, args: Value) -> Tool {
        Self::tool_result(&self.service_raw(name, args))
    }

    fn append(&self, id: u32, control: Value) -> Tool {
        let mut args =
            json!({"host": "prod", "database": "test2", "table": TABLE, "rows": [{"id": id}]});
        for (k, v) in control.as_object().unwrap() {
            args[k] = v.clone();
        }
        self.service("append_rows", args)
    }

    fn intent(&self, mut args: Value) -> String {
        // The first call has no token: it says who it is, and the portal makes the agent.
        if self.token().is_none() {
            args["agent_label"] = json!(self.label);
        }
        let tool = self.portal("approval_intent", args);
        assert!(!tool.is_error, "{}", tool.value);
        match (self.token(), tool.value["agent_token"].as_str()) {
            (None, Some(token)) => {
                assert!(token.starts_with("agt_") && token.len() == 47, "{token}");
                assert_eq!(tool.value["agent_status"], "new", "{}", tool.value);
                *self.token.lock().unwrap() = Some(token.to_string());
            }
            (None, None) => panic!("the first call gives the token: {}", tool.value),
            (Some(_), given) => assert!(given.is_none(), "the token shows once"),
        }
        tool.value["approval_ref"].as_str().unwrap().to_string()
    }

    fn status(&self, request_id: &str) -> String {
        let tool = self.service("get_action_status", json!({"request_id": request_id}));
        assert!(!tool.is_error, "{}", tool.value);
        tool.value["status"].as_str().unwrap().to_string()
    }
}

/// The request that waits for an approval: the id and the code. The service did not run the tool.
fn awaiting(tool: &Tool) -> (String, String) {
    assert!(!tool.is_error, "{}", tool.value);
    let action = &tool.value["action"];
    assert_eq!(action["status"], "awaiting_approval", "{}", tool.value);
    assert_eq!(action["executed"], false, "{}", tool.value);
    (
        action["id"].as_str().unwrap().to_string(),
        action["code"].as_str().unwrap().to_string(),
    )
}

// ------------------------------------------------------------------------------------------------ the phone

async fn request_of(conn: &mut Conn, request_id: &str) -> RequestMsg {
    loop {
        let req = next_request(conn, WAIT).await;
        conn.receipt(&req.id).await;
        if req.request_id == request_id {
            return req;
        }
    }
}

/// The `settled` frame of one request. The frames of the requests that the test cancelled earlier are skipped.
async fn settled_for(
    conn: &mut Conn,
    request_id: &str,
    wait: Duration,
) -> inbox_core::proto::SettledMsg {
    let deadline = Instant::now() + wait;
    loop {
        let settled = next_settled(conn, deadline.saturating_duration_since(Instant::now())).await;
        conn.receipt(&settled.id).await;
        if settled.request_id == request_id {
            return settled;
        }
    }
}

fn shown(req: &RequestMsg) -> String {
    let approval = req.approval.as_ref().expect("an approval");
    assert!(
        approval.verify(),
        "the phone computes the effect of the command it got"
    );
    approval.computed_effect().unwrap()
}

fn answer(req: &RequestMsg, id: &str, action: Action, text: Option<&str>) -> ClientMsg {
    let accept = action == Action::Accept;
    ClientMsg::Answer(AnswerMsg {
        request_id: req.request_id.clone(),
        client_msg_id: id.into(),
        action,
        shown_effect: accept.then(|| shown(req)),
        choice: None,
        text: text.map(str::to_string),
        intent: text.map(|_| Intent::ChangeRequest),
        answers: None,
    })
}

async fn decide(conn: &mut Conn, req: &RequestMsg, id: &str, action: Action, text: Option<&str>) {
    conn.send(&answer(req, id, action, text)).await;
    let result = next_result(conn, WAIT).await;
    assert_eq!(result.status, AnswerStatus::Accepted, "{result:?}");
}

/// The civil date of a day of the Unix epoch (Howard Hinnant, `civil_from_days`).
fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (yoe + era * 400 + i64::from(m <= 2), m, d)
}

/// One decision of the PDP as the service reads it: `decision`, and a context of the members of protocol 6 and 17 only. A
/// reason of a refusal is one of the four reasons of the approval (CONF 3.1a, case D90).
fn check_decision(decision: &Value, line: &Value) {
    let keys: Vec<&str> = decision
        .as_object()
        .map(|o| o.keys().map(String::as_str).collect())
        .unwrap_or_default();
    assert!(
        keys.iter().all(|k| ["decision", "context"].contains(k)),
        "{line}"
    );
    for (k, v) in decision["context"].as_object().cloned().unwrap_or_default() {
        assert!(
            [
                "reason",
                "evaluation_id",
                "usage",
                "limits",
                "ceilings",
                "retry_after"
            ]
            .contains(&k.as_str()),
            "D90: the answer of the PDP has a member {k}: {line}"
        );
        if k == "reason" {
            let reason = v.as_str().unwrap();
            assert!(
                [
                    "approval_required",
                    "approval_pending",
                    "approval_denied",
                    "approval_expired"
                ]
                .contains(&reason),
                "D90: reason {reason}: {line}"
            );
        }
    }
}

// ------------------------------------------------------------------------------------------------ the scenarios

/// The scenarios run in one test: they share the owner, the key and the phone, and a key has a rate cap.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "needs the stack of tools/dev/pep-e2e.sh"]
async fn pep_end_to_end() {
    if std::env::var("PEP_E2E").is_err() {
        println!("PEP_E2E is not set: run tools/dev/pep-e2e.sh");
        return;
    }
    let stack = Stack::attach();
    let out = stack.dir.join("out");
    std::fs::write(
        out.join("canaries.txt"),
        [EXPLANATION, HINT, COMMENT, THREAD].join("\n") + "\n",
    )
    .unwrap();

    step("setup: the owner, its key with `confirm`, the phone");
    let (key, key_id, session) = make_owner_and_key(&stack.s);
    let subject = format!("key:{key_id}");
    let agent = Agent::new(&key, &stack, LABEL);
    let code = {
        let (st, v) = common::make_code(&stack.s, OWNER);
        assert_eq!(st, 200, "{v}");
        v["code"].as_str().unwrap().to_string()
    };
    let phone: Phone = enroll_code_at(Some(stack.portal), &code, "pixel")
        .await
        .expect("the phone enrolls");
    let (mut conn, _) = Conn::open(stack.portal, &phone)
        .await
        .expect("the session opens");
    let _ = session;

    // The real service lists the tool with an approval and the tool that reads the verdict.
    let list = agent.rpc(
        agent.service,
        &[
            ("X-Identity-Token", agent.identity_token()),
            ("Accept", "application/json, text/event-stream".into()),
            ("MCP-Protocol-Version", "2026-07-28".into()),
        ],
        &json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list",
                "params": {"_meta": {"io.modelcontextprotocol/protocolVersion": "2026-07-28"}}}),
    );
    let names: Vec<&str> = list["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    assert!(
        names.contains(&"append_rows") && names.contains(&"get_action_status"),
        "{names:?}"
    );
    pass("setup");

    // ---------------------------------------------------------------------------------------- S1
    step("S1 (CONF 3.4): approve on the phone, execute one time, the second call asks again");
    let r1 = agent.intent(json!({"lease_s": 120, "explanation": EXPLANATION, "hint": {"suggested": HINT, "source": "e2e"}}));
    let first = agent.append(101, json!({"approval_ref": r1}));
    let (id1, code1) = awaiting(&first);
    pass("S1 U100 the first call runs nothing and gives id and code");

    let req = request_of(&mut conn, &id1).await;
    let approval = req.approval.clone().expect("an approval");
    assert_eq!(approval.code, code1);
    assert_eq!(approval.subject, subject);
    assert_eq!(approval.action, "data.append");
    assert_eq!(
        approval.explanation.as_deref(),
        Some(EXPLANATION),
        "the phone gets the explanation of the agent"
    );
    let effect = shown(&req);
    assert_eq!(approval.effect, effect);
    pass("S1 the phone received the request with the explanation and the code");

    // The effect that the PEP sent, the effect that the portal recomputed (it answered 201), the effect of the phone.
    let tap = stack.tap();
    let submission = tap
        .iter()
        .find(|l| {
            l["method"] == "POST"
                && l["path"] == "/approval/v1/requests"
                && l["response"]["request_id"] == json!(id1)
        })
        .expect("the tap saw the submission");
    assert_eq!(
        submission["request"]["effect"],
        json!(effect),
        "gap detection: PEP effect = phone effect"
    );
    assert!(submission["request"]["op"].is_object() && submission["request"]["view"].is_object());
    assert!(
        submission["request"].get("explanation").is_none()
            && submission["request"].get("hint").is_none()
    );
    assert_eq!(
        submission["request"]["approval_ref"],
        json!(r1),
        "the PEP passes the opaque reference as it came"
    );
    pass("S1 U108/U109 effect of the PEP = effect of the phone; the submission carries op, view and the opaque reference only");

    // U101: the agent calls again while the request is pending: the same id, the same code, no second request.
    let again = agent.append(101, json!({"approval_ref": r1}));
    assert_eq!(
        awaiting(&again),
        (id1.clone(), code1.clone()),
        "U101: same id and code"
    );
    assert_eq!(agent.status(&id1), "pending");
    let seen = agent.portal("get_answer", json!({"request_id": id1}));
    assert_eq!(seen.value["status"], "pending", "{}", seen.value);
    pass("S1 U101 a call while pending gives the same request; the verdict is pending");

    // SA (lot G5): a second agent of the same key asks for the same effect while the request is open. The portal made it a
    // second agent (its own token). The PEP gives it the request of the first agent (same effect, same id, same code). The
    // portal records `also_requested`, the item keeps its first agent, and the service learns nothing of the two agents.
    step("SA: a second agent asks for the same effect: the same request, and the service sees no agent");
    let agent2 = Agent::new(&agent.key, &stack, LABEL2);
    let r_dup = agent2.intent(json!({"lease_s": 120, "explanation": EXPLANATION}));
    assert_ne!(agent2.token(), agent.token(), "two agents, two tokens");
    let dup = agent2.append(101, json!({"approval_ref": r_dup}));
    assert_eq!(
        awaiting(&dup),
        (id1.clone(), code1.clone()),
        "the same effect while the request is open gives the request of the first agent"
    );
    assert_eq!(agent2.status(&id1), "pending");
    // The first agent still reads its request: the reads go by the key. Its own token is the one of the item.
    let seen = agent.portal("get_answer", json!({"request_id": id1}));
    assert_eq!(seen.value["status"], "pending", "{}", seen.value);
    // A wrong token on a read is refused; the token of the other agent is the wrong one for this item.
    let body = json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call",
        "params": {"name": "get_answer", "arguments": {"request_id": id1, "agent_token": agent2.token().unwrap()}}});
    let wrong = Agent::tool_result(&agent.rpc(
        agent.portal,
        &[("Authorization", format!("Bearer {}", agent.key))],
        &body,
    ));
    assert!(wrong.is_error, "{}", wrong.value);
    assert_eq!(
        wrong.value["error"]["code"], "agent_mismatch",
        "{}",
        wrong.value
    );
    // The phone: the request of the first agent has the agent of the first agent, in portal text. The two frames `agent`
    // carry what each agent said, and no second request frame came for the same effect.
    let a1 = req.from.agent_id.clone();
    assert_eq!(
        (req.from.agent_state, req.from.label.as_str()),
        (AgentState::New, "agent-laptop #1")
    );
    conn.send(&ClientMsg::Sync {
        open: vec![OpenRef {
            request_id: id1.clone(),
            seq: req.seq,
        }],
        agents: Some(vec![]),
    })
    .await;
    let mut frames = std::collections::BTreeMap::new();
    let mut again = Vec::new();
    loop {
        match conn.recv(Duration::from_millis(1500)).await {
            common::Recv::Msg(ServerMsg::Agent(f)) => {
                conn.receipt(&f.id).await;
                frames.insert(f.agent_id.clone(), *f);
            }
            common::Recv::Msg(ServerMsg::Request(r)) => {
                conn.receipt(&r.id).await;
                again.push(r.request_id.clone());
            }
            common::Recv::Msg(_) => {}
            common::Recv::Closed(code) => panic!("closed: {code:?}"),
            common::Recv::Timeout => break,
        }
    }
    // The bucket of the key is a frame too: it holds the requests that no agent claims. It is not an agent.
    frames.retain(|_, f| f.kind == inbox_core::proto::AgentKind::Agent);
    assert_eq!(frames.len(), 2, "two agents, no more: {frames:?}");
    let says = |f: &inbox_core::proto::AgentMsg| f.observed.as_ref().and_then(|o| o.says.clone());
    let a2 = frames
        .values()
        .find(|f| says(f).as_deref() == Some(LABEL2))
        .expect("the frame of the second agent")
        .agent_id
        .clone();
    assert_ne!(a1, a2);
    assert_eq!(says(&frames[&a1]).as_deref(), Some(LABEL));
    assert_eq!(
        frames[&a1]
            .observed
            .as_ref()
            .and_then(|o| o.client.as_deref()),
        Some(UA),
        "the portal shows the client that the agent sent"
    );
    assert!(
        again.iter().all(|id| id == &id1),
        "no request frame for the second agent: {again:?}"
    );
    // The ids and the tokens are known now: the script scans the logs of the stack for them.
    let mut all = vec![
        EXPLANATION.to_string(),
        HINT.into(),
        COMMENT.into(),
        THREAD.into(),
        LABEL.into(),
        LABEL2.into(),
        UA.into(),
        "agt_".into(),
    ];
    for t in [agent.token().unwrap(), agent2.token().unwrap()] {
        all.push(t[4..].to_string());
    }
    std::fs::write(out.join("canaries.txt"), all.join("\n") + "\n").unwrap();
    // The portal logs the id of the agent in its own log (the field `agent`): the service never sees it.
    std::fs::write(
        out.join("canaries-service.txt"),
        [a1.clone(), a2.clone()].join("\n") + "\n",
    )
    .unwrap();
    pass("SA the same effect gives the same request; the phone has two agents, no second request frame");

    decide(&mut conn, &req, "m-1", Action::Accept, None).await;
    assert_eq!(agent.status(&id1), "approved");
    let got = agent.portal("get_answer", json!({"request_id": id1}));
    assert_eq!(got.value["status"], "answered", "{}", got.value);
    assert_eq!(got.value["decision"], "approved", "{}", got.value);
    assert!(got.value["consume_before"].is_string());
    pass(
        "S1 the phone approved: the verdict is approved, the agent reads the answer at the portal",
    );

    let done = agent.append(101, json!({"approval_ref": r1}));
    assert!(!done.is_error, "{}", done.value);
    assert_eq!(done.value["appended"], 1, "{}", done.value);
    let total = done.value["total"].as_u64().unwrap();
    pass("S1 U102 the service ran the tool one time");

    // The approval is consumed: the same call asks again, with a new request.
    let second = agent.append(101, json!({"lease_s": 120}));
    let (id2, code2) = awaiting(&second);
    assert_ne!(id2, id1, "a consumed approval gives a new request");
    assert_ne!(code2, code1);
    let again = agent.portal("cancel", json!({"request_id": id2}));
    assert_eq!(again.value["status"], "cancelled", "{}", again.value);
    assert_eq!(agent.status(&id2), "void", "a cancelled request reads void");
    pass("S1 a second call after the consumption asks again (new id), cancel reads void");

    // ---------------------------------------------------------------------------------------- S2
    step("S2: decline with a comment: the agent reads it at the portal, the service never");
    let r2 =
        agent.intent(json!({"lease_s": 120, "allow_custom": true, "explanation": EXPLANATION}));
    let (id3, _) = awaiting(&agent.append(102, json!({"approval_ref": r2})));
    let req3 = request_of(&mut conn, &id3).await;
    assert!(req3.allow_custom);
    decide(&mut conn, &req3, "m-3", Action::Decline, Some(COMMENT)).await;
    assert_eq!(agent.status(&id3), "denied");
    let denied = agent.portal("get_answer", json!({"request_id": id3}));
    assert_eq!(denied.value["decision"], "denied", "{}", denied.value);
    assert_eq!(
        denied.value["untrusted"]["comment"]["text"], COMMENT,
        "the agent reads the comment at the portal"
    );
    assert_eq!(
        denied.value["untrusted"]["comment"]["intent"],
        "change_request"
    );
    // The same call during the deny time: the tool error has no comment, and nothing ran.
    let refused = agent.service_raw(
        "append_rows",
        json!({"host": "prod", "database": "test2", "table": TABLE, "rows": [{"id": 102}], "lease_s": 120}),
    );
    let refused_tool = Agent::tool_result(&refused);
    assert!(refused_tool.is_error, "{refused}");
    assert_eq!(
        refused_tool.value["error"]["code"], "approval_denied",
        "{refused}"
    );
    assert!(
        !refused.to_string().contains("CANARY"),
        "U105: the service gives no comment: {refused}"
    );
    pass("S2 U105 decline: the agent reads the comment at the portal; the service says approval_denied without it");

    // The change request: another command is another effect, and the deny time does not apply to it.
    let (id4, _) = awaiting(&agent.append(103, json!({"lease_s": 120})));
    let req4 = request_of(&mut conn, &id4).await;
    assert_ne!(
        shown(&req4),
        shown(&req3),
        "U103: another argument, another effect"
    );
    let cancelled = agent.portal("cancel", json!({"request_id": id4}));
    assert_eq!(cancelled.value["status"], "cancelled");
    pass("S2 U103/D85 a changed command has another effect and no deny time");

    // ---------------------------------------------------------------------------------------- S3
    step("S3: the renewal by get_action_status, then the abandonment when the lease lapses");
    let r3 = agent.intent(json!({"lease_s": 5, "explanation": EXPLANATION}));
    let (id5, _) = awaiting(&agent.append(104, json!({"approval_ref": r3})));
    let req5 = request_of(&mut conn, &id5).await;
    // The lease is 3 x 5 s and the grace 5 s: the request would lapse near 20 s. A read of the verdict renews it.
    let began = Instant::now();
    while began.elapsed() < Duration::from_secs(28) {
        assert_eq!(
            agent.status(&id5),
            "pending",
            "a renewed request stays pending"
        );
        tokio::time::sleep(Duration::from_secs(4)).await;
    }
    pass("S3 U106 get_action_status renews the lease: the request lives past its first lease");

    let settled = settled_for(&mut conn, &id5, Duration::from_secs(60)).await;
    assert_eq!(
        (
            settled.request_id.as_str(),
            settled.state,
            settled.reason.as_str()
        ),
        (id5.as_str(), ItemState::Abandoned, "lease_lapsed")
    );
    pass("S3 the phone got settled(abandoned) when the reads stopped");
    let late = answer(&req5, "m-late", Action::Accept, None);
    conn.send(&late).await;
    assert_eq!(
        next_result(&mut conn, WAIT).await.status,
        AnswerStatus::Abandoned
    );
    pass("S3 a late answer of the phone gets abandoned");
    // The agent calls the tool again. The question has no usage and no renewal: an abandoned request asks again.
    let (id6, _) = awaiting(&agent.append(104, json!({"lease_s": 120})));
    assert_ne!(
        id6, id5,
        "after the abandonment the call creates a new request; nothing ran"
    );
    pass("S3 after the abandonment the call creates a new request; nothing ran");
    // A read of the old request, with a newer open request of the same effect: the PEP must get a verdict, not an error.
    let old = agent.status(&id5);
    println!("S3 a read of the abandoned request while a newer request of the same effect is open: {old}");
    assert!(["pending", "void"].contains(&old.as_str()), "{old}");
    for id in [&id6, &id5] {
        let _ = agent.portal("cancel", json!({"request_id": id}));
    }

    // The return of the agent: within `revive_window`, a read of the verdict renews the lease and revives the request.
    let r6 = agent.intent(json!({"lease_s": 5}));
    let (id9, _) = awaiting(&agent.append(107, json!({"approval_ref": r6})));
    let _ = request_of(&mut conn, &id9).await;
    let settled = settled_for(&mut conn, &id9, Duration::from_secs(60)).await;
    assert_eq!(settled.state, ItemState::Abandoned);
    assert_eq!(
        agent.status(&id9),
        "pending",
        "the agent came back inside the revive window"
    );
    let revived = request_of(&mut conn, &id9).await;
    assert_eq!(
        revived.request_id, id9,
        "a revival sends a new request frame to the phone"
    );
    assert_eq!(
        agent.portal("cancel", json!({"request_id": id9})).value["status"],
        "cancelled"
    );
    pass("S3 inside the revive window a read of the agent revives the request (CONF 3.6)");

    // ---------------------------------------------------------------------------------------- S4
    step("S4: a thread at the portal (say) never reaches the service");
    let r4 = agent.intent(json!({"lease_s": 120, "allow_ask_back": true}));
    let (id7, _) = awaiting(&agent.append(105, json!({"approval_ref": r4})));
    let _ = request_of(&mut conn, &id7).await;
    let said = agent.portal("say", json!({"request_id": id7, "text": THREAD}));
    assert!(!said.is_error, "{}", said.value);
    assert_eq!(
        agent.portal("cancel", json!({"request_id": id7})).value["status"],
        "cancelled"
    );
    pass("S4 a message of the agent in the thread");

    // ---------------------------------------------------------------------------------------- S5
    step("S5: one more approval: the table ran once for S1 and for this call, no more");
    let r5 = agent.intent(json!({"lease_s": 120}));
    let (id8, _) = awaiting(&agent.append(106, json!({"approval_ref": r5})));
    let req8 = request_of(&mut conn, &id8).await;
    decide(&mut conn, &req8, "m-8", Action::Accept, None).await;
    let ran = agent.append(106, json!({"approval_ref": r5}));
    assert!(!ran.is_error, "{}", ran.value);
    assert_eq!(
        ran.value["total"].as_u64().unwrap(),
        total + 1,
        "only the two approved calls ran: S1 and S5"
    );
    // A reference works one time: the PEP submits no request while the PDP says `true`, so the reference is still free
    // at the first use. After the consumption the same reference is a used reference, and the portal refuses it.
    let reused = agent.append(106, json!({"approval_ref": r5}));
    assert!(
        reused.is_error,
        "the approval gave one execution only: {}",
        reused.value
    );
    assert_eq!(
        reused.value["error"]["code"], "invalid_argument",
        "{}",
        reused.value
    );
    let replay = agent.append(106, json!({"lease_s": 120}));
    let (id9, _) = awaiting(&replay);
    let _ = agent.portal("cancel", json!({"request_id": id9}));
    pass("S5 the data changed twice in all: executed once for each approval");

    // ---------------------------------------------------------------------------------------- S5b
    step("S5b: the call waits (wait_s): the PEP reads the verdict with a long poll and runs the tool in the same call");
    let r7 = agent.intent(json!({"lease_s": 120}));
    let waiting = {
        let agent = agent.clone();
        tokio::task::spawn_blocking(move || {
            agent.append(108, json!({"approval_ref": r7, "wait_s": 25}))
        })
    };
    // The phone gets the request while the call waits, and it approves one second later.
    let req9 = loop {
        let req = next_request(&mut conn, WAIT).await;
        conn.receipt(&req.id).await;
        if req
            .approval
            .as_ref()
            .is_some_and(|a| a.op.as_value().to_string().contains("108"))
        {
            break req;
        }
    };
    tokio::time::sleep(Duration::from_secs(1)).await;
    decide(&mut conn, &req9, "m-9", Action::Accept, None).await;
    let waited = waiting.await.unwrap();
    assert!(!waited.is_error, "{}", waited.value);
    assert_eq!(
        waited.value["appended"], 1,
        "the same call ran the tool after the wait: {}",
        waited.value
    );
    assert_eq!(
        waited.value["total"].as_u64().unwrap(),
        ran.value["total"].as_u64().unwrap() + 1
    );
    pass("S5b a call with wait_s ran the tool in the same call when the phone approved");

    // ---------------------------------------------------------------------------------------- S5c
    step("S5c: the wait of the call: out of range is refused without change, `until` works");
    for lease in [1_u64, 99_999_999] {
        let bad = agent.append(109, json!({ "lease_s": lease }));
        assert!(bad.is_error, "lease_s {lease}: {}", bad.value);
        assert_eq!(
            bad.value["error"]["code"], "invalid_argument",
            "{}",
            bad.value
        );
    }
    let until = (std::time::SystemTime::now() + Duration::from_secs(3600))
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    // An RFC 3339 text from the seconds, without a date library.
    let (days, rest) = (until / 86_400, until % 86_400);
    let (y, m, d) = civil_from_days(days as i64);
    let text = format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
        rest / 3600,
        rest % 3600 / 60,
        rest % 60
    );
    let (id10, _) = awaiting(&agent.append(109, json!({ "until": text })));
    let req10 = request_of(&mut conn, &id10).await;
    assert_eq!(req10.lease_mode, inbox_core::proto::LeaseMode::Until);
    let _ = agent.portal("cancel", json!({ "request_id": id10 }));
    pass("S5c lease_s out of range is refused (invalid_argument, nothing ran); `until` makes a request in the mode until");

    // ---------------------------------------------------------------------------------------- S6
    step("S6 (CONF 9.2 PEP observation): what the service received is the verdict, the request id and the code");
    let tap = stack.tap();
    assert!(
        tap.len() >= 10,
        "the tap saw the link: {} exchanges",
        tap.len()
    );
    let whole = tap
        .iter()
        .map(|l| l.to_string())
        .collect::<Vec<_>>()
        .join("\n");
    let (t1, t2) = (agent.token().unwrap(), agent2.token().unwrap());
    for needle in [
        EXPLANATION,
        HINT,
        COMMENT,
        THREAD,
        "CANARY",
        OWNER,
        &phone.device_id,
        "pixel",
        "agent-laptop",
        // The agents (lot G5): the tokens, what they said, their client, their ids, and the words of the channel.
        &t1[4..],
        &t2[4..],
        "agt_",
        LABEL,
        LABEL2,
        UA,
        a1.as_str(),
        a2.as_str(),
        "agent_token",
        "agent_label",
        "agent_id",
        "agent_status",
        "observed",
        "similar",
        "also_requested",
    ] {
        assert!(
            !whole.contains(needle),
            "the link of the service holds {needle:?}"
        );
    }
    // What the service answered to the agents (the replies of the tools) holds none of these values either.
    for agent in [&agent, &agent2] {
        let replies = agent.from_service.lock().unwrap().join("\n");
        assert!(replies.len() > 100, "the service answered: {replies}");
        for needle in [
            &t1[4..],
            &t2[4..],
            "agt_",
            LABEL,
            LABEL2,
            UA,
            a1.as_str(),
            a2.as_str(),
            "agent_token",
            "agent_label",
        ] {
            assert!(
                !replies.contains(needle),
                "a reply of the service holds {needle:?}"
            );
        }
    }
    let verdicts = ["pending", "approved", "denied", "void"];
    let mut counts = std::collections::BTreeMap::<String, usize>::new();
    for l in &tap {
        let (method, path) = (l["method"].as_str().unwrap(), l["path"].as_str().unwrap());
        let status = l["status"].as_u64().unwrap();
        let body = &l["response"];
        let kind = if path == "/approval/v1/requests" {
            "submit"
        } else if path.ends_with("/keepalive") {
            "keepalive"
        } else if path.starts_with("/approval/v1/requests/") {
            "status"
        } else if path == "/access/v1/evaluation" {
            "evaluation"
        } else if path == "/access/v1/evaluations" {
            "evaluations"
        } else if path.starts_with("/.well-known/") {
            "metadata"
        } else {
            panic!("the service called an unexpected route: {method} {path}");
        };
        *counts.entry(kind.to_string()).or_default() += 1;
        let keys: Vec<&str> = body
            .as_object()
            .map(|o| o.keys().map(String::as_str).collect())
            .unwrap_or_default();
        match kind {
            "submit" => {
                if matches!(status, 200 | 201) {
                    assert!(
                        keys.iter()
                            .all(|k| ["request_id", "code", "verdict"].contains(k)),
                        "submit answer: {l}"
                    );
                    assert!(verdicts.contains(&body["verdict"].as_str().unwrap()), "{l}");
                } else {
                    // A refusal (the used reference of S5): a code of 17.4, nothing else.
                    assert_eq!(
                        (status, keys.as_slice()),
                        (422, ["error"].as_slice()),
                        "{l}"
                    );
                }
                // The request of the service: only what protocol 17.4 lists.
                let sent: Vec<&str> = l["request"]
                    .as_object()
                    .unwrap()
                    .keys()
                    .map(String::as_str)
                    .collect();
                assert!(
                    sent.iter().all(|k| [
                        "evaluation_id",
                        "effect",
                        "op",
                        "view",
                        "approval_ref",
                        "lease_s",
                        "until",
                        "supersedes"
                    ]
                    .contains(k)),
                    "U109: the service sent a member that is not in 17.4: {sent:?}"
                );
            }
            "status" | "keepalive" => {
                assert_eq!(status, 200, "{l}");
                assert_eq!(keys, ["verdict"], "A12: a read gives the verdict only: {l}");
                assert!(verdicts.contains(&body["verdict"].as_str().unwrap()), "{l}");
            }
            "evaluation" => {
                assert_eq!(status, 200, "{l}");
                check_decision(body, l);
                // The question of the service holds the effect only, never the command (U107).
                let q = &l["request"];
                if let Some(approval) = q["context"].get("approval") {
                    assert_eq!(approval.as_object().unwrap().len(), 1, "U107 {l}");
                    assert!(
                        approval["effect"].as_str().unwrap().starts_with("sha256:"),
                        "{l}"
                    );
                }
                assert!(
                    !q.to_string().contains("rows"),
                    "U107: the question holds no op: {l}"
                );
            }
            "evaluations" => {
                // The list of the tools: one question for each action, no effect.
                assert_eq!(status, 200, "{l}");
                assert_eq!(keys, ["evaluations"], "{l}");
                for item in body["evaluations"].as_array().unwrap() {
                    check_decision(item, l);
                }
            }
            _ => {}
        }
    }
    println!("the service link: {counts:?}");
    assert!(
        counts.contains_key("submit")
            && counts.contains_key("status")
            && counts.contains_key("evaluation")
    );
    // A question with `usage` that got `true` is the use; the agent made three (S1, S5, S5b).
    let used = tap
        .iter()
        .filter(|l| {
            l["request"]["context"].get("usage").is_some() && l["response"]["decision"] == true
        })
        .count();
    assert_eq!(used, 3, "three approvals were consumed (S1, S5, S5b)");
    pass("S6 the service saw only request_id, code, verdict, evaluation_id and the reasons of 17.4; no comment, explanation, hint, approver or device");

    let metrics = stack.metrics();
    let line = |name: &str| {
        metrics
            .lines()
            .filter(|l| l.starts_with(name))
            .collect::<Vec<_>>()
            .join("\n")
    };
    println!("{}", line("portal_pdp_approval_total"));
    assert!(
        metrics.contains("portal_pdp_approval_total"),
        "the portal counts the approval reasons"
    );
    for needle in [
        EXPLANATION,
        HINT,
        COMMENT,
        THREAD,
        LABEL,
        LABEL2,
        UA,
        &t1[4..],
        &t2[4..],
        "agt_",
        a1.as_str(),
        a2.as_str(),
    ] {
        assert!(
            !metrics.contains(needle),
            "the metrics of the portal hold a canary: {needle}"
        );
    }
    // The portal counts the two agents and the refusals, and it holds no text of an agent.
    assert!(
        metrics.contains("portal_agents_created_total{origin=\"call\"} 2"),
        "{}",
        line("portal_agent")
    );
    pass("S6 the metrics of the portal count the reasons and hold no canary");
}
