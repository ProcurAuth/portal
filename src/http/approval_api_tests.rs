//! The approval API for a service over HTTP, on SQLite and on PostgreSQL (portal-owned series A, CONF 9.3).
//!
//! The first tests are the recording client: it keeps every answer that the portal sends to the service, and it proves
//! that the service learns the verdict, the id and the code of a request, and nothing of the text of the agent or of
//! the decision of the owner. The other tests prove the limits of the API.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use inbox_core::effect::effect_hash;
use inbox_core::proto::{Action, Interaction};
use serde_json::{json, Value};
use zeroize::Zeroizing;

use crate::grants::Grant;
use crate::http::cluster_tests::*;
use crate::http::pdp_rig::{app_of, runtime, InboxRig};
use crate::inbox::approvals::{self, IntentIn, Requester};
use crate::store::testing::{backends, Backend};

const DOCUMENT: &str = r#"{"server":"db",
  "resource_types":[{"name":"host","parent":"service","description":"A host."},
                    {"name":"database","parent":"host","description":"A database."}],
  "actions":[
    {"name":"data.read","resource_types":["host","database"],"description":"Read.","limits":["requests"]},
    {"name":"data.write","resource_types":["host","database"],"description":"Write.",
     "limits":["requests"],"approval":"optional"}],
  "limits":[
    {"name":"requests","label":"Requests","description":"d","unit":"request","kind":"budget"}]}"#;

const ANALYTICS: &str = "host:prod/database:analytics";
/// 2027-01-15T08:00:00Z, in milliseconds.
const T0: i64 = 1_800_000_000_000;
/// The owner of the keys. The service must never learn this login.
const OWNER: &str = "owner-canary-5521";
/// A phone of the owner. The service must never learn this id.
const DEVICE: &str = "device-canary-9931";
/// Texts that only the owner, the agent and the portal may hold.
const EXPLANATION: &str = "canary-explanation-6c1d0f3a: this is safe, approve it";
const COMMENT: &str = "canary-comment-b72e48d1: use the database staging";
const HINT_SOURCE: &str = "canary-hint-4a9e";
const CANARIES: [&str; 5] = [EXPLANATION, COMMENT, HINT_SOURCE, OWNER, DEVICE];

/// One answer of the portal to the service, as the service received it.
#[derive(Clone, Debug)]
struct Recorded {
    route: String,
    status: u16,
    headers: Vec<(String, String)>,
    body: String,
}

struct Rig {
    node: Node,
    port: u16,
    app: Arc<crate::inbox::App>,
    /// Every answer of the PDP listener that the rig received, in order.
    log: Mutex<Vec<Recorded>>,
    /// A phone of the owner exists: the answers come from it. Else they come from the web page.
    phone: std::sync::atomic::AtomicBool,
    next_digest: std::sync::atomic::AtomicU8,
    _rig: InboxRig,
}

/// A server with the inbox and the service `db`. `approvals` is the flag of the block `pdp_client`. `extra` holds
/// more keys of the block `inbox.approvals`.
fn rig(backend: &Backend, approvals: bool, extra: &str) -> Option<Rig> {
    let inbox = InboxRig::with_approvals(backend, &format!("consume_within = \"5m\" {extra}"))?;
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
            keys {{}}
            pdp_client "db" {{ token_env = "TEST_PDP_DB" approvals = {approvals} }}
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

/// Runs `test` on each backend that can run it.
fn each_rig(approvals: bool, extra: &str, test: impl Fn(&Rig, &str)) {
    // A use of the PDP counts in the window of the current minute: do not cross a change of the minute.
    stay_in_a_minute();
    for backend in backends() {
        let Some(r) = rig(&backend, approvals, extra) else {
            eprintln!("INBOX_TEST_PG is not set: skipped on PostgreSQL");
            continue;
        };
        test(&r, backend.name());
    }
}

/// The wire form of a command: the operation, the view and the effect that goes with the question.
struct Command {
    op: Value,
    view: Value,
    effect: String,
}

fn command(n: u32) -> Command {
    let op = json!({"tool": "run_write_query", "host": "prod", "database": "analytics",
                    "sql": format!("INSERT INTO analytics.events SELECT {n}")});
    let view = json!({"title": "Write to ClickHouse",
                      "fields": [["Host", "/host"], ["Database", "/database"]],
                      "body": "/sql", "body_label": "SQL"});
    let effect = effect_hash("db", "data.write", ANALYTICS, &op).unwrap();
    Command { op, view, effect }
}

trait ReplyExt {
    fn id(&self) -> String;
    /// The status and the code of an error answer.
    fn error(&self) -> (u16, String);
}

impl ReplyExt for Reply {
    fn id(&self) -> String {
        assert!(
            self.status == 200 || self.status == 201,
            "{}: {}",
            self.status,
            self.body
        );
        self.json()["request_id"].as_str().unwrap().to_string()
    }

    fn error(&self) -> (u16, String) {
        (
            self.status,
            self.json()["error"].as_str().unwrap_or("").to_string(),
        )
    }
}

fn bearer() -> String {
    format!("Bearer {PDP_DB_TOKEN}")
}

impl Rig {
    /// Sends a request and records the answer. A `503` is the storage of the inbox that is away for a moment (the
    /// database of the test shares its server): the call is tried again, and only the last answer is recorded.
    fn call(&self, route: &str, send: impl Fn() -> Reply) -> Reply {
        let mut reply = send();
        for _ in 0..12 {
            if reply.status != 503 {
                break;
            }
            std::thread::sleep(Duration::from_millis(400));
            reply = send();
        }
        self.record(route, reply)
    }

    fn record(&self, route: &str, reply: Reply) -> Reply {
        self.log.lock().unwrap().push(Recorded {
            route: route.to_string(),
            status: reply.status,
            headers: reply.headers.clone(),
            body: reply.body.clone(),
        });
        reply
    }

    fn now(&self) -> i64 {
        use crate::inbox::Clock;
        self.app.clock.now_ms()
    }

    /// Stops the clock of the inbox at `ms` after the start, in milliseconds.
    fn fix_clock(&self, ms: i64) {
        self.app.clock.fix(Some(ms));
    }

    /// A key of the owner that can use `data.write` on analytics with `confirm`.
    fn key(&self, id: &str, interaction: crate::keys::Interaction) -> String {
        let access = Grant::new("access", "db").unwrap();
        let write = Grant::from_path_text("data.write", "db", ANALYTICS)
            .unwrap()
            .with_confirm(true);
        let mut grants = vec![access, write];
        grants.sort();
        self.node
            .state
            .store
            .insert_key(
                &crate::store::KeyRow {
                    kind: crate::keys::KeyKind::Mcp,
                    interaction,
                    id: id.into(),
                    owner: Some(OWNER.into()),
                    name: format!("key {id}"),
                    digest: [self
                        .next_digest
                        .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                        32],
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
        self.node
            .state
            .users
            .refresh(&self.node.state.store)
            .unwrap();
        format!("key:{id}")
    }

    fn key_a(&self) -> String {
        self.key("2b7c91d0e4a853f6", crate::keys::Interaction::Conversational)
    }

    fn key_b(&self) -> String {
        self.key("9f3d27a18c64b0e5", crate::keys::Interaction::Conversational)
    }

    fn key_shot(&self) -> String {
        self.key("9f3d27a18c64b0e5", crate::keys::Interaction::OneShot)
    }

    fn requester(&self, subject: &str, interaction: Interaction) -> Requester {
        Requester {
            key_id: subject.strip_prefix("key:").unwrap().to_string(),
            owner: OWNER.into(),
            interaction,
        }
    }

    fn pdp_body(&self, subject: &str, effect: &str, usage: bool) -> String {
        let mut context = serde_json::Map::new();
        if usage {
            context.insert("usage".into(), json!({"units": 1}));
        }
        context.insert("approval".into(), json!({"effect": effect}));
        json!({
            "subject": {"type": "user", "id": subject},
            "action": {"name": "data.write"},
            "resource": {"type": "database", "id": "analytics", "properties": {"ancestors": [
                {"type": "service", "id": "db"}, {"type": "host", "id": "prod"}]}},
            "context": context
        })
        .to_string()
    }

    /// The answer of the PDP to a question with the effect.
    fn ask(&self, subject: &str, effect: &str, usage: bool) -> Value {
        let body = self.pdp_body(subject, effect, usage);
        let reply = self.call("evaluation", || {
            pdp(
                self.port,
                "/access/v1/evaluation",
                Some(PDP_DB_TOKEN),
                &body,
                None,
            )
        });
        assert_eq!(reply.status, 200, "{}", reply.body);
        reply.json()
    }

    fn evaluation_id(&self, subject: &str, effect: &str) -> String {
        let answer = self.ask(subject, effect, false);
        answer["context"]["evaluation_id"]
            .as_str()
            .unwrap_or_else(|| panic!("no evaluation_id in {answer}"))
            .to_string()
    }

    /// The body of a submission with a fresh `evaluation_id`. `control` holds `lease_s`, `until` or `approval_ref`.
    fn body(&self, subject: &str, c: &Command, control: Value) -> Value {
        let mut body = json!({
            "evaluation_id": self.evaluation_id(subject, &c.effect),
            "effect": c.effect, "op": c.op, "view": c.view,
        });
        for (k, v) in control.as_object().unwrap() {
            body[k] = v.clone();
        }
        body
    }

    fn post(&self, body: &Value) -> Reply {
        self.post_text(&body.to_string())
    }

    fn post_text(&self, body: &str) -> Reply {
        let auth = bearer();
        self.call("submit", || {
            send(
                self.port,
                "POST",
                "/approval/v1/requests",
                &[
                    ("Content-Type", "application/json"),
                    ("Authorization", &auth),
                ],
                body,
            )
        })
    }

    /// A submission with a lease of 15 minutes.
    fn submit(&self, subject: &str, n: u32) -> Reply {
        self.submit_with(subject, n, json!({"lease_s": 900}))
    }

    fn submit_with(&self, subject: &str, n: u32, control: Value) -> Reply {
        let c = command(n);
        self.post(&self.body(subject, &c, control))
    }

    fn get(&self, id: &str, query: &str) -> Reply {
        let auth = bearer();
        self.call("read", || {
            send(
                self.port,
                "GET",
                &format!("/approval/v1/requests/{id}{query}"),
                &[("Authorization", &auth)],
                "",
            )
        })
    }

    fn verdict(&self, id: &str, subject: &str) -> String {
        let reply = self.get(id, &format!("?subject={}", subject.replace(':', "%3A")));
        assert_eq!(reply.status, 200, "{}", reply.body);
        reply.json()["verdict"].as_str().unwrap().to_string()
    }

    fn keepalive(&self, id: &str, body: &str) -> Reply {
        let auth = bearer();
        self.call("keepalive", || {
            send(
                self.port,
                "POST",
                &format!("/approval/v1/requests/{id}/keepalive"),
                &[
                    ("Content-Type", "application/json"),
                    ("Authorization", &auth),
                ],
                body,
            )
        })
    }

    fn items(&self) -> crate::inbox::testing::Items<'_> {
        self.app.items().expect("the inbox has items")
    }

    fn loaded(&self, id: &str) -> crate::inbox::testing::Loaded {
        eventually(|| {
            runtime().block_on(self.items().get(id, &crate::inbox::testing::Caller::System))
        })
        .unwrap()
    }

    fn count(&self, sql: &str) -> i64 {
        runtime().block_on(self.items().test_count(sql))
    }

    /// The answer of the owner, as a phone or the web page gives it.
    fn answer(&self, id: &str, action: Action, text: Option<&str>) {
        let effect = self.loaded(id).approval.unwrap().effect;
        let out = eventually(|| {
            runtime().block_on(self.items().answer(
                id,
                crate::inbox::testing::Answer {
                    login: OWNER.into(),
                    via: if self.phone.load(std::sync::atomic::Ordering::SeqCst) {
                        crate::inbox::testing::AnswerVia::Device(DEVICE.into())
                    } else {
                        crate::inbox::testing::AnswerVia::Web
                    },
                    action,
                    shown_effect: Some(effect.clone()),
                    client_msg_id: None,
                    payload: text.map(|t| Zeroizing::new(t.as_bytes().to_vec())),
                    consume_within: Duration::from_secs(300),
                },
            ))
        });
        assert!(
            matches!(out, crate::inbox::testing::Answered::Accepted { .. }),
            "the answer was not taken: {out:?}"
        );
    }

    fn device(&self) {
        self.phone.store(true, std::sync::atomic::Ordering::SeqCst);
        runtime()
            .block_on(self.items().seed_device(DEVICE, OWNER))
            .unwrap();
    }

    fn intent(&self, who: &Requester, input: IntentIn) -> Result<String, approvals::ApprovalError> {
        runtime()
            .block_on(approvals::intent(&self.app, who, input))
            .map(|out| out.approval_ref)
    }
}

/// Runs an engine call again while the storage is away for a moment (the database of the test shares its server).
fn eventually<T>(mut call: impl FnMut() -> Result<T, crate::inbox::testing::ItemError>) -> T {
    for _ in 0..12 {
        match call() {
            Err(crate::inbox::testing::ItemError::Degraded) => {
                std::thread::sleep(Duration::from_millis(400))
            }
            other => return other.unwrap_or_else(|e| panic!("{e}")),
        }
    }
    call().unwrap_or_else(|e| panic!("{e}"))
}

fn explained() -> IntentIn {
    IntentIn {
        lease_s: Some(900),
        explanation: Some(EXPLANATION.into()),
        allow_custom: true,
        allow_ask_back: true,
        hint: Some(json!({"suggested": "go", "confidence": 0.5, "source": HINT_SOURCE})),
        ..IntentIn::default()
    }
}

// ---------------------------------------------------------------- what the service learns

/// The members that the service may read in an answer of the portal, for each route.
fn only_members(r: &Recorded, allowed: &[&str]) {
    let value: Value = serde_json::from_str(&r.body)
        .unwrap_or_else(|e| panic!("{} {}: not JSON ({e}): {}", r.route, r.status, r.body));
    let members: Vec<&str> = value
        .as_object()
        .unwrap_or_else(|| panic!("{}: not an object: {}", r.route, r.body))
        .keys()
        .map(String::as_str)
        .collect();
    for m in &members {
        assert!(
            allowed.contains(m),
            "{} {} gives the member {m:?}: {}",
            r.route,
            r.status,
            r.body
        );
    }
}

/// The recording client. It checks every answer that the rig received: the members, the values and the text.
fn check_what_the_service_learned(r: &Rig, name: &str) {
    let log = r.log.lock().unwrap().clone();
    assert!(
        log.iter().any(|x| x.route == "submit" && x.status == 201),
        "[{name}] the cycle made no request"
    );
    for rec in &log {
        let place = format!("[{name}] {} {}: {}", rec.route, rec.status, rec.body);
        for canary in CANARIES {
            assert!(
                !rec.body.contains(canary),
                "{place}: the body holds {canary}"
            );
            for (h, v) in &rec.headers {
                assert!(
                    !h.contains(canary) && !v.contains(canary),
                    "{place}: the header {h} holds {canary}"
                );
            }
        }
        match (rec.route.as_str(), rec.status) {
            ("submit", 200 | 201) => {
                only_members(rec, &["request_id", "code", "verdict"]);
                let v: Value = serde_json::from_str(&rec.body).unwrap();
                assert_eq!(v["verdict"], "pending", "{place}");
                assert_eq!(v["code"].as_str().unwrap().len(), 4, "{place}");
            }
            ("read" | "keepalive", 200) => {
                only_members(rec, &["verdict"]);
                let v: Value = serde_json::from_str(&rec.body).unwrap();
                assert!(
                    ["pending", "approved", "denied", "void"]
                        .contains(&v["verdict"].as_str().unwrap()),
                    "{place}"
                );
            }
            ("evaluation", 200) => {
                only_members(rec, &["decision", "context"]);
                let v: Value = serde_json::from_str(&rec.body).unwrap();
                for member in v["context"].as_object().into_iter().flat_map(|c| c.keys()) {
                    assert!(
                        ["reason", "evaluation_id", "usage", "limits", "retry_after"]
                            .contains(&member.as_str()),
                        "{place}: the context holds {member}"
                    );
                }
                if let Some(reason) = v["context"]["reason"].as_str() {
                    assert!(
                        reason != "approval_comment" && !reason.contains("comment"),
                        "{place}"
                    );
                }
            }
            (_, status) if status >= 400 => only_members(rec, &["error", "retry_after"]),
            other => panic!("{place}: an answer that the test does not know: {other:?}"),
        }
    }
}

#[test]
fn pep_observation_unit() {
    each_rig(true, "", |r, name| {
        r.device();
        let key = r.key_a();
        let who = r.requester(&key, Interaction::Conversational);

        // 1. An agent with a channel to the portal makes an intent with an explanation, a hint and flags.
        let reference = r.intent(&who, explained()).unwrap();
        let first = command(1);
        // 2. The service asks, submits with the opaque reference, reads, renews and waits.
        let created = r.post(&r.body(&key, &first, json!({ "approval_ref": reference })));
        assert_eq!(created.status, 201, "[{name}] {}", created.body);
        let id = created.id();
        assert_eq!(r.verdict(&id, &key), "pending", "[{name}]");
        let renewed = r.get(
            &id,
            &format!("?subject={}&renew=true&wait=1", key.replace(':', "%3A")),
        );
        assert_eq!(renewed.status, 200, "[{name}] {}", renewed.body);
        let kept = r.keepalive(&id, &json!({"subject": key}).to_string());
        assert_eq!(kept.status, 200, "[{name}] {}", kept.body);
        assert_eq!(
            r.ask(&key, &first.effect, false)["context"]["reason"],
            "approval_pending",
            "[{name}]"
        );
        // 3. The owner declines on a phone with a comment that proposes a change.
        r.answer(&id, Action::Decline, Some(COMMENT));
        assert_eq!(r.verdict(&id, &key), "denied", "[{name}]");
        assert_eq!(
            r.ask(&key, &first.effect, true)["context"]["reason"],
            "approval_denied",
            "[{name}]"
        );
        // 4. The agent asks for another command, with a new intent. The owner approves, and the service runs it once.
        let reference = r.intent(&who, explained()).unwrap();
        let second = command(2);
        let created = r.post(&r.body(&key, &second, json!({ "approval_ref": reference })));
        assert_eq!(created.status, 201, "[{name}] {}", created.body);
        let id2 = created.id();
        r.answer(&id2, Action::Accept, None);
        assert_eq!(r.verdict(&id2, &key), "approved", "[{name}]");
        assert_eq!(
            r.ask(&key, &second.effect, false)["decision"],
            true,
            "[{name}]"
        );
        assert_eq!(
            r.ask(&key, &second.effect, true)["decision"],
            true,
            "[{name}]"
        );
        assert_eq!(r.verdict(&id2, &key), "approved", "[{name}] consumed");
        // 5. A request that the agent replaces, one that nobody answers, and a refused submission.
        let third = r.submit(&key, 3);
        let third_id = third.id();
        let replaced = r.post(&r.body(
            &key,
            &command(4),
            json!({"lease_s": 900, "supersedes": third_id}),
        ));
        assert_eq!(replaced.status, 201, "[{name}] {}", replaced.body);
        assert_eq!(r.verdict(&third_id, &key), "void", "[{name}]");
        assert_eq!(
            r.submit_with(&key, 5, json!({"lease_s": 1})).status,
            422,
            "[{name}]"
        );

        // The comment, the explanation and the hint are where they must be: in the sealed items of the owner side.
        let l = r.loaded(&id);
        assert_eq!(
            l.approval.unwrap().decision,
            Some(inbox_core::proto::Decision::Deny)
        );
        check_what_the_service_learned(r, name);
        // The recording client saw every kind of answer.
        let log = r.log.lock().unwrap();
        for route in ["submit", "read", "keepalive", "evaluation"] {
            assert!(log.iter().any(|x| x.route == route), "[{name}] no {route}");
        }
        assert!(
            log.iter().any(|x| x.status == 422),
            "[{name}] no error answer"
        );
    });
}

#[test]
fn a12_get_returns_only_verdict_fields() {
    each_rig(true, "", |r, name| {
        let key = r.key_a();
        let created = r.submit(&key, 1);
        assert_eq!(created.status, 201, "[{name}] {}", created.body);
        assert_eq!(
            created.json().as_object().unwrap().len(),
            3,
            "[{name}] request_id, code, verdict"
        );
        let id = created.id();
        let q = format!("?subject={}", key.replace(':', "%3A"));
        for reply in [
            r.get(&id, &q),
            r.get(&id, &format!("{q}&renew=true")),
            r.keepalive(&id, &json!({"subject": key}).to_string()),
        ] {
            assert_eq!(reply.status, 200, "[{name}] {}", reply.body);
            assert_eq!(reply.json(), json!({"verdict": "pending"}), "[{name}]");
        }
        // After each end of a request the answer has the verdict and nothing else.
        r.answer(&id, Action::Decline, None);
        assert_eq!(
            r.get(&id, &q).json(),
            json!({"verdict": "denied"}),
            "[{name}]"
        );
        // No route gives a turn, a comment, an approver or a device.
        for path in ["turns", "comment", "approver", "device", "body", "thread"] {
            let auth = bearer();
            let reply = send(
                r.port,
                "GET",
                &format!("/approval/v1/requests/{id}/{path}"),
                &[("Authorization", &auth)],
                "",
            );
            assert_eq!(reply.status, 404, "[{name}] {path}");
        }
        check_what_the_service_learned(r, name);
    });
}

#[test]
fn intent_content_not_readable_by_pep() {
    each_rig(true, "", |r, name| {
        let key = r.key_a();
        let who = r.requester(&key, Interaction::Conversational);
        let reference = r.intent(&who, explained()).unwrap();
        // The database keeps the digest of the reference, and the explanation sealed.
        let digest = approvals::ref_digest(&reference);
        assert_ne!(digest, reference, "[{name}]");
        assert_eq!(
            r.count(&format!(
                "SELECT COUNT(*) FROM approval_intents WHERE ref_digest = '{reference}'"
            )),
            0,
            "[{name}] the reference is never kept"
        );
        let row = runtime()
            .block_on(r.items().test_intent(&digest))
            .expect("the intent is stored");
        let ct = row.body.unwrap().ct;
        for canary in [EXPLANATION, HINT_SOURCE] {
            assert!(
                !ct.windows(canary.len()).any(|w| w == canary.as_bytes()),
                "[{name}] the sealed intent holds {canary}"
            );
        }
        // The key of another agent cannot use it. The service has no route to read it.
        let other = r.key_b();
        let other_body = r.body(&other, &command(1), json!({ "approval_ref": reference }));
        let refused = r.post(&other_body);
        assert_eq!(refused.status, 422, "[{name}] {}", refused.body);
        assert_eq!(
            refused.json(),
            json!({"error": "invalid_argument"}),
            "[{name}]"
        );
        let auth = bearer();
        for path in [
            "/approval/v1/intents",
            &format!("/approval/v1/intents/{digest}"),
        ] {
            let reply = send(r.port, "GET", path, &[("Authorization", &auth)], "");
            assert_eq!(reply.status, 404, "[{name}] {path}");
        }
        // The intent is still usable by its key.
        let used = r.post(&r.body(&key, &command(1), json!({ "approval_ref": reference })));
        assert_eq!(used.status, 201, "[{name}] {}", used.body);
        // The text of the intent moved into the sealed body of the item, and the intent holds none.
        assert!(
            runtime()
                .block_on(r.items().test_intent(&digest))
                .unwrap()
                .body
                .is_none(),
            "[{name}] a used intent keeps no text"
        );
        check_what_the_service_learned(r, name);
    });
}

// ---------------------------------------------------------------- A01, A02, A03

#[test]
fn a01_idempotent_repost_same_id_no_renewal_no_body_swap() {
    each_rig(true, "", |r, name| {
        let key = r.key_a();
        let t0 = r.now();
        r.fix_clock(t0);
        let first = r.submit(&key, 1);
        assert_eq!(first.status, 201, "[{name}] {}", first.body);
        let id = first.id();
        let before = r.loaded(&id);
        let (until, seen) = (before.item.lease_until, before.item.agent_seen_at);
        let body_before = r.count(&format!(
            "SELECT body_size FROM items WHERE request_id = '{id}'"
        ));
        // The same effect again, with another lease and later: the same id and code, no renewal, no new body.
        r.fix_clock(t0 + 400_000);
        let again = r.submit_with(&key, 1, json!({"lease_s": 3600}));
        assert_eq!(again.status, 200, "[{name}] {}", again.body);
        assert_eq!(again.json(), first.json(), "[{name}] the same id and code");
        let after = r.loaded(&id);
        assert_eq!(after.item.lease_until, until, "[{name}] no renewal");
        assert_eq!(after.item.agent_seen_at, seen, "[{name}]");
        assert_eq!(
            after.item.lease_s,
            Some(900),
            "[{name}] the first lease stays"
        );
        assert_eq!(
            r.count(&format!(
                "SELECT body_size FROM items WHERE request_id = '{id}'"
            )),
            body_before,
            "[{name}] no body swap"
        );
        // A second request for another effect is a second item. A repeat after the end of the first is a new item.
        let other = r.submit(&key, 2);
        assert_eq!(other.status, 201, "[{name}]");
        assert_ne!(
            other.json()["request_id"],
            first.json()["request_id"],
            "[{name}]"
        );
        r.answer(&id, Action::Decline, None);
        r.fix_clock(t0 + 400_000 + 301_000);
        let fresh = r.submit(&key, 1);
        assert_eq!(fresh.status, 201, "[{name}] {}", fresh.body);
        assert_ne!(
            fresh.json()["request_id"],
            first.json()["request_id"],
            "[{name}]"
        );
        assert_eq!(
            r.count("SELECT COUNT(*) FROM approvals WHERE open_hash IS NOT NULL"),
            2,
            "[{name}] the effect 2 and the new request of the effect 1"
        );
    });
}

#[test]
fn a01_429_after_cap() {
    each_rig(true, "max_requests_per_key_per_hour = 2", |r, name| {
        let key = r.key_a();
        let t0 = r.now();
        r.fix_clock(t0);
        let first = r.submit(&key, 1);
        assert_eq!(first.status, 201, "[{name}]");
        assert_eq!(r.submit(&key, 2).status, 201, "[{name}]");
        // A repeat of an open request is not a creation: the cap does not count it.
        assert_eq!(r.submit(&key, 1).status, 200, "[{name}]");
        r.fix_clock(t0 + 600_000);
        let capped = r.submit(&key, 3);
        assert_eq!(capped.status, 429, "[{name}] {}", capped.body);
        let body = capped.json();
        assert_eq!(body["error"], "rate_limited", "[{name}]");
        // The first creation leaves the window of one hour 3000 s later.
        assert_eq!(body["retry_after"], 3000, "[{name}] {body}");
        assert_eq!(capped.header("retry-after"), Some("3000"), "[{name}]");
        // Another key has its own count.
        let other = r.key_b();
        assert_eq!(r.submit(&other, 1).status, 201, "[{name}]");
        r.fix_clock(t0 + 3_600_001);
        assert_eq!(r.submit(&key, 3).status, 201, "[{name}] the window moved");
    });
}

#[test]
fn a01_service_without_approvals_flag_403() {
    each_rig(false, "", |r, name| {
        let key = r.key_a();
        let c = command(1);
        let body = json!({"evaluation_id": "x", "effect": c.effect, "op": c.op, "view": c.view, "lease_s": 900});
        assert_eq!(r.post(&body).status, 403, "[{name}]");
        let reply = r.get("abc", &format!("?subject={}", key.replace(':', "%3A")));
        assert_eq!(reply.status, 403, "[{name}]");
        assert_eq!(
            r.keepalive("abc", r#"{"subject":"key:x"}"#).status,
            403,
            "[{name}]"
        );
        // A request without a token is 401.
        let reply = send(
            r.port,
            "GET",
            "/approval/v1/requests/abc?subject=x",
            &[],
            "",
        );
        assert_eq!(reply.status, 401, "[{name}]");
    });
}

#[test]
fn a01_evaluation_id_required_other_subject_422() {
    each_rig(true, "", |r, name| {
        let key = r.key_a();
        let other = r.key_b();
        let c = command(1);
        let base = json!({"effect": c.effect, "op": c.op, "view": c.view, "lease_s": 900});
        // No `evaluation_id`: a missing member. A text that the portal never made: not valid.
        let reply = r.post(&base);
        assert_eq!(
            reply.error(),
            (422, "invalid_argument".to_string()),
            "[{name}]"
        );
        for bad in ["x", "AAAA", "not base64 !"] {
            let mut body = base.clone();
            body["evaluation_id"] = json!(bad);
            let reply = r.post(&body);
            assert_eq!(
                reply.error(),
                (422, "invalid_evaluation".to_string()),
                "[{name}] {bad}"
            );
        }
        // The id of a question about another effect does not give a request for this effect.
        let mut body = base.clone();
        body["evaluation_id"] = json!(r.evaluation_id(&key, &command(2).effect));
        let reply = r.post(&body);
        assert_eq!(
            reply.error(),
            (422, "invalid_evaluation".to_string()),
            "[{name}]"
        );
        // The id binds its subject: with the id of the key `other`, the request is for the key `other`, not for `key`.
        let mut body = base.clone();
        body["evaluation_id"] = json!(r.evaluation_id(&other, &c.effect));
        let reply = r.post(&body);
        assert_eq!(reply.status, 201, "[{name}] {}", reply.body);
        let id = reply.id();
        assert_eq!(
            r.loaded(&id).item.requester_key_id,
            "9f3d27a18c64b0e5",
            "[{name}]"
        );
        assert_eq!(r.verdict(&id, &other), "pending", "[{name}]");
        let miss = r.get(&id, &format!("?subject={}", key.replace(':', "%3A")));
        assert_eq!(miss.error(), (404, "not_found".to_string()), "[{name}]");
        // A late id: after `evaluation_ttl` (5 minutes).
        let late = r.evaluation_id(&key, &command(3).effect);
        r.fix_clock(r.now() + 301_000);
        let c3 = command(3);
        let reply = r.post(&json!({"evaluation_id": late, "effect": c3.effect, "op": c3.op, "view": c3.view, "lease_s": 900}));
        assert_eq!(
            reply.error(),
            (422, "invalid_evaluation".to_string()),
            "[{name}]"
        );
    });
}

#[test]
fn a02_lease_out_of_range_422_no_clamp() {
    each_rig(true, "", |r, name| {
        let key = r.key_a();
        let t0 = T0;
        r.fix_clock(t0);
        // The range of the harness is 30s to 7d.
        for (n, lease) in [(1, 29), (2, 604_801), (3, 0), (4, -5)] {
            let reply = r.submit_with(&key, n, json!({ "lease_s": lease }));
            assert_eq!(
                reply.error(),
                (422, "invalid_lease".to_string()),
                "[{name}] {lease}"
            );
        }
        for (n, lease) in [(5, 30), (6, 604_800)] {
            let reply = r.submit_with(&key, n, json!({ "lease_s": lease }));
            assert_eq!(reply.status, 201, "[{name}] {lease}: {}", reply.body);
            let id = reply.id();
            assert_eq!(
                r.loaded(&id).item.lease_s,
                Some(lease),
                "[{name}] the value is kept as it is"
            );
        }
        assert_eq!(
            r.count("SELECT COUNT(*) FROM items"),
            2,
            "[{name}] a refusal creates nothing"
        );
        // A form that is not a whole number of seconds, and an `until` that is not valid.
        for control in [
            json!({"lease_s": 60.5}),
            json!({"lease_s": "60"}),
            json!({"until": "tomorrow"}),
            json!({"until": 5}),
            json!({"until": "2000-01-01T00:00:00Z"}),
            json!({"until": "2100-01-01T00:00:00Z"}),
        ] {
            let reply = r.submit_with(&key, 7, control.clone());
            assert_eq!(
                reply.error(),
                (422, "invalid_lease".to_string()),
                "[{name}] {control}"
            );
        }
        // An `until` inside the life of a chain (90 days) is good.
        // The clock stands at 2027-01-15T08:00:00Z, so one hour later is 09:00:00Z.
        let reply = r.submit_with(&key, 8, json!({ "until": "2027-01-15T09:00:00Z" }));
        assert_eq!(reply.status, 201, "[{name}] {}", reply.body);
        let id = reply.id();
        assert_eq!(
            r.loaded(&id).item.until_at,
            Some(t0 + 3_600_000),
            "[{name}]"
        );
        assert_eq!(r.loaded(&id).item.lease_s, None, "[{name}]");
    });
}

#[test]
fn a02_lease_and_until_together_422() {
    each_rig(true, "", |r, name| {
        let key = r.key_a();
        for (control, error) in [
            (
                json!({"lease_s": 60, "until": "2100-01-01T00:00:00Z"}),
                "invalid_lease",
            ),
            (json!({"lease_s": 60, "approval_ref": "r"}), "invalid_lease"),
            (
                json!({"until": "2100-01-01T00:00:00Z", "approval_ref": "r"}),
                "invalid_lease",
            ),
            (json!({}), "lease_required"),
        ] {
            let reply = r.submit_with(&key, 1, control.clone());
            assert_eq!(
                (reply.status, reply.json()),
                (422, json!({ "error": error })),
                "[{name}] {control}"
            );
        }
        assert_eq!(r.count("SELECT COUNT(*) FROM items"), 0, "[{name}]");
    });
}

#[test]
fn a03_renew_only_with_subject_of_requester() {
    each_rig(true, "", |r, name| {
        let key = r.key_a();
        let other = r.key_b();
        let t0 = r.now();
        r.fix_clock(t0);
        let created = r.submit_with(&key, 1, json!({"lease_s": 60}));
        let id = created.id();
        // The effective lease is 3 times `lease_s`.
        assert_eq!(
            r.loaded(&id).item.lease_until,
            Some(t0 + 180_000),
            "[{name}]"
        );
        let q = |subject: &str| format!("?subject={}", subject.replace(':', "%3A"));
        // A read without `renew` never renews, also late and also for the subject of the request.
        r.fix_clock(t0 + 100_000);
        assert_eq!(r.get(&id, &q(&key)).status, 200, "[{name}]");
        assert_eq!(
            r.loaded(&id).item.lease_until,
            Some(t0 + 180_000),
            "[{name}] a plain read"
        );
        // A read with `renew=true` of another subject is `not_found` and renews nothing.
        let miss = r.get(&id, &format!("{}&renew=true", q(&other)));
        assert_eq!(miss.error(), (404, "not_found".to_string()), "[{name}]");
        let miss = r.keepalive(&id, &json!({"subject": other}).to_string());
        assert_eq!(miss.status, 404, "[{name}]");
        assert_eq!(
            r.loaded(&id).item.lease_until,
            Some(t0 + 180_000),
            "[{name}] other subject"
        );
        // With the subject of the request, `renew=true` and the keepalive renew the lease.
        let renewed = r.get(&id, &format!("{}&renew=true", q(&key)));
        assert_eq!(renewed.status, 200, "[{name}]");
        assert_eq!(
            r.loaded(&id).item.lease_until,
            Some(t0 + 100_000 + 180_000),
            "[{name}]"
        );
        r.fix_clock(t0 + 200_000);
        assert_eq!(
            r.keepalive(&id, &json!({"subject": key}).to_string())
                .status,
            200,
            "[{name}]"
        );
        assert_eq!(
            r.loaded(&id).item.lease_until,
            Some(t0 + 200_000 + 180_000),
            "[{name}]"
        );
        // An unknown value of `renew`, a missing subject and a keepalive body with more members are 422.
        for query in [
            format!("{}&renew=yes", q(&key)),
            "?renew=true".to_string(),
            format!("{}&wait=x", q(&key)),
        ] {
            assert_eq!(r.get(&id, &query).status, 422, "[{name}] {query}");
        }
        for body in [
            r#"{}"#,
            r#"{"subject":"key:x","extra":1}"#,
            r#"{"subject":""}"#,
            "[]",
        ] {
            assert_eq!(r.keepalive(&id, body).status, 422, "[{name}] {body}");
        }
        assert_eq!(r.get("no-such-request", &q(&key)).status, 404, "[{name}]");
    });
}

// ---------------------------------------------------------------- A08

#[test]
fn a08_effect_mismatch_invalid_view_command_too_large() {
    each_rig(true, r#"max_command_bytes = "1KiB""#, |r, name| {
        let key = r.key_a();
        let lease = json!({"lease_s": 900});
        let c = command(1);
        let ok = r.body(&key, &c, lease.clone());
        let status = |body: &Value| {
            let reply = r.post(body);
            (
                reply.status,
                reply.json()["error"].as_str().map(str::to_string),
            )
        };
        // The effect of another operation.
        let mut body = ok.clone();
        body["effect"] = json!(command(2).effect);
        assert_eq!(
            status(&body),
            (422, Some("invalid_evaluation".into())),
            "[{name}] the id binds the effect"
        );
        // An evaluation of the other effect and an `op` that does not give it.
        let mut body = r.body(&key, &command(2), lease.clone());
        body["op"] = c.op.clone();
        assert_eq!(
            status(&body),
            (422, Some("effect_mismatch".into())),
            "[{name}]"
        );
        // A pointer of the view that names no value of `op`, a view that is not an object, a view with no body.
        for view in [
            json!({"title": "t", "fields": [["Host", "/nothing"]], "body": "/sql"}),
            json!({"title": "t", "fields": [], "body": "/nothing"}),
            json!({"title": "t", "fields": [], "body": "/host/0"}),
            json!([]),
            json!({"title": "t", "fields": []}),
            json!({"title": "t", "fields": [], "body": "/sql", "extra": 1}),
        ] {
            let mut body = ok.clone();
            body["view"] = view.clone();
            assert_eq!(
                status(&body),
                (422, Some("invalid_view".into())),
                "[{name}] {view}"
            );
        }
        // An `op` with a float, or a duplicate member name: `invalid_op`.
        let text = ok.to_string().replace(r#""sql":"#, r#""f":1.5,"sql":"#);
        assert_eq!(r.post_text(&text).status, 422, "[{name}]");
        assert_eq!(r.post_text(&text).json()["error"], "invalid_op", "[{name}]");
        let text = ok.to_string().replace(r#""sql":"#, r#""host":"x","sql":"#);
        assert_eq!(
            r.post_text(&text).json()["error"],
            "invalid_op",
            "[{name}] duplicate"
        );
        // A command above `max_command_bytes` (1 KiB here): 413.
        let big = json!({"tool": "run_write_query", "host": "prod", "database": "analytics", "sql": "x".repeat(2000)});
        let c2 = Command {
            effect: effect_hash("db", "data.write", ANALYTICS, &big).unwrap(),
            op: big,
            view: c.view.clone(),
        };
        let reply = r.post(&r.body(&key, &c2, lease.clone()));
        assert_eq!(
            reply.error(),
            (413, "command_too_large".to_string()),
            "[{name}]"
        );
        // A body above the body limit of the PDP is 413 too.
        let huge = json!({"evaluation_id": "x", "effect": c.effect, "op": {"sql": "y".repeat(70_000)}, "view": {}, "lease_s": 900});
        assert_eq!(r.post(&huge).status, 413, "[{name}]");
        assert_eq!(
            r.count("SELECT COUNT(*) FROM items"),
            0,
            "[{name}] each refusal creates nothing"
        );
        // And the good command is taken.
        assert_eq!(r.post(&ok).status, 201, "[{name}]");
    });
}

// ---------------------------------------------------------------- A10, A11

#[test]
fn a10_intent_rules_explanation_one_shot() {
    each_rig(true, "max_explanation_chars = 40", |r, name| {
        let chat = r.key_a();
        let shot = r.key_shot();
        let (chat, shot) = (
            r.requester(&chat, Interaction::Conversational),
            r.requester(&shot, Interaction::OneShot),
        );
        let err = |who: &Requester, input: IntentIn| {
            let e = r.intent(who, input).unwrap_err();
            (e.status, e.code)
        };
        let base = IntentIn {
            lease_s: Some(900),
            ..IntentIn::default()
        };
        // An explanation of 40 characters is good, 41 is refused and never cut.
        let text = |n: usize| Some("é".repeat(n));
        r.intent(
            &chat,
            IntentIn {
                explanation: text(40),
                ..base.clone()
            },
        )
        .unwrap();
        assert_eq!(
            err(
                &chat,
                IntentIn {
                    explanation: text(41),
                    ..base.clone()
                }
            ),
            (422, "explanation_too_long"),
            "[{name}]"
        );
        // The wait: one of two, in the range.
        assert_eq!(
            err(&chat, IntentIn::default()),
            (422, "lease_required"),
            "[{name}]"
        );
        assert_eq!(
            err(
                &chat,
                IntentIn {
                    until_ms: Some(r.now() + 600_000),
                    ..base.clone()
                }
            ),
            (422, "invalid_lease"),
            "[{name}]"
        );
        assert_eq!(
            err(
                &chat,
                IntentIn {
                    lease_s: Some(5),
                    ..base.clone()
                }
            ),
            (422, "invalid_lease"),
            "[{name}]"
        );
        // A key `one_shot` has no flags: `422 interaction_not_allowed`. It can still make an intent without them.
        for flags in [(true, false), (false, true), (true, true)] {
            assert_eq!(
                err(
                    &shot,
                    IntentIn {
                        allow_custom: flags.0,
                        allow_ask_back: flags.1,
                        ..base.clone()
                    }
                ),
                (422, "interaction_not_allowed"),
                "[{name}] {flags:?}"
            );
        }
        r.intent(
            &shot,
            IntentIn {
                explanation: text(3),
                ..base.clone()
            },
        )
        .unwrap();
        r.intent(
            &chat,
            IntentIn {
                allow_custom: true,
                allow_ask_back: true,
                ..base.clone()
            },
        )
        .unwrap();
        // A hint that is not valid, and a `previous_id` that is not a final request of the key.
        for hint in [
            json!({"suggested": "", "source": "s"}),
            json!({"suggested": "go", "source": "s", "confidence": 1.5}),
            json!({"suggested": "go", "source": "s", "extra": 1}),
            json!("go"),
        ] {
            assert_eq!(
                err(
                    &chat,
                    IntentIn {
                        hint: Some(hint.clone()),
                        ..base.clone()
                    }
                ),
                (422, "invalid_argument"),
                "[{name}] {hint}"
            );
        }
        assert_eq!(
            err(
                &chat,
                IntentIn {
                    previous_id: Some("nope".into()),
                    ..base.clone()
                }
            ),
            (422, "invalid_argument"),
            "[{name}]"
        );
        // The refused intents made nothing: three good ones above for `chat`, one for `shot`.
        assert_eq!(
            r.count("SELECT COUNT(*) FROM approval_intents"),
            3,
            "[{name}]"
        );
    });
    each_rig(true, "require_explanation = true", |r, name| {
        let key = r.key_a();
        let who = r.requester(&key, Interaction::Conversational);
        let base = IntentIn {
            lease_s: Some(900),
            ..IntentIn::default()
        };
        for empty in [None, Some(String::new())] {
            let e = r
                .intent(
                    &who,
                    IntentIn {
                        explanation: empty,
                        ..base.clone()
                    },
                )
                .unwrap_err();
            assert_eq!(
                (e.status, e.code),
                (422, "explanation_required"),
                "[{name}]"
            );
        }
        let reference = r
            .intent(
                &who,
                IntentIn {
                    explanation: Some("because".into()),
                    ..base.clone()
                },
            )
            .unwrap();
        assert_eq!(
            r.post(&r.body(&key, &command(1), json!({ "approval_ref": reference })))
                .status,
            201,
            "[{name}]"
        );
        // A submission with no reference has no explanation: with `require_explanation` it is refused.
        let reply = r.submit(&key, 2);
        assert_eq!(
            reply.error(),
            (422, "explanation_required".to_string()),
            "[{name}]"
        );
    });
}

#[test]
fn a11_ref_single_use_bound_to_key_expires() {
    each_rig(true, r#"intent_ttl = "2m""#, |r, name| {
        let key = r.key_a();
        let other = r.key_b();
        let who = r.requester(&key, Interaction::Conversational);
        let t0 = r.now();
        r.fix_clock(t0);
        let reference = r.intent(&who, explained()).unwrap();
        assert_eq!(reference.len(), 43, "[{name}] 32 bytes in base64url");
        let with_ref = |subject: &str, n: u32, reference: &str| {
            r.post(&r.body(subject, &command(n), json!({ "approval_ref": reference })))
        };
        // Bound to the key: the key of another agent cannot use it, and the failed try does not use it up.
        assert_eq!(with_ref(&other, 1, &reference).status, 422, "[{name}]");
        // Once: the first request takes the intent, and the lease of the intent (900 s) is the lease of the request.
        let first = with_ref(&key, 1, &reference);
        assert_eq!(first.status, 201, "[{name}] {}", first.body);
        let id = first.id();
        let l = r.loaded(&id);
        assert_eq!(l.item.lease_s, Some(900), "[{name}]");
        // The explanation is not part of the effect: it is the hash of `op` alone.
        assert_eq!(
            l.approval.as_ref().unwrap().effect,
            command(1).effect,
            "[{name}]"
        );
        assert!(
            l.item.allow_custom && l.item.allow_ask_back,
            "[{name}] the flags of the intent"
        );
        // The same reference for another effect: used.
        let again = with_ref(&key, 2, &reference);
        assert_eq!(
            again.error(),
            (422, "invalid_argument".to_string()),
            "[{name}]"
        );
        // A reference that nobody made, and one that expired after `intent_ttl` (2 minutes).
        assert_eq!(with_ref(&key, 3, "AAAA").status, 422, "[{name}]");
        let stale = r.intent(&who, explained()).unwrap();
        r.fix_clock(t0 + 121_000);
        assert_eq!(with_ref(&key, 3, &stale).status, 422, "[{name}] expired");
        // An expired intent leaves the table when the engine cleans up.
        let items = r.items();
        let cleaned = runtime().block_on(items.maintain()).unwrap();
        assert!(cleaned.intents_deleted >= 1, "[{name}] {cleaned:?}");
        // A repeat of the open request with the same used reference gives the same request, not an error.
        let repeat = with_ref(&key, 1, &reference);
        assert_eq!(
            (
                repeat.status,
                repeat.json()["request_id"].as_str().map(str::to_string)
            ),
            (200, Some(id)),
            "[{name}]"
        );
    });
}

// ---------------------------------------------------------------- anti-spam

#[test]
fn deny_cooldown_same_effect_only_and_not_after_abandon() {
    each_rig(true, "", |r, name| {
        let key = r.key_a();
        let t0 = r.now();
        r.fix_clock(t0);
        // An explicit refusal: no new request for the same effect during 5 minutes. Another effect is free.
        let first = r.submit(&key, 1);
        let id = first.id();
        let evaluation = r.evaluation_id(&key, &command(1).effect);
        r.answer(&id, Action::Decline, None);
        r.fix_clock(t0 + 60_000);
        let c = command(1);
        let body = json!({"evaluation_id": evaluation, "effect": c.effect, "op": c.op, "view": c.view, "lease_s": 900});
        let cooling = r.post(&body);
        assert_eq!(cooling.status, 429, "[{name}] {}", cooling.body);
        assert_eq!(cooling.json()["error"], "rate_limited", "[{name}]");
        assert_eq!(
            cooling.json()["retry_after"],
            240,
            "[{name}] 300 s after the answer, 60 s ago"
        );
        assert_eq!(r.submit(&key, 2).status, 201, "[{name}] another effect");
        // The cooldown ends 300 s after the answer.
        r.fix_clock(t0 + 301_000);
        assert_eq!(r.submit(&key, 1).status, 201, "[{name}] after the cooldown");
        // An abandon starts no cooldown: the lease (900 s) and the grace (2 minutes) end, and the request can come back.
        let third = r.submit(&key, 3);
        let third_id = third.id();
        r.fix_clock(t0 + 301_000 + 3 * 900_000 + 121_000);
        assert_eq!(r.verdict(&third_id, &key), "void", "[{name}] abandoned");
        assert_eq!(
            r.submit(&key, 3).status,
            201,
            "[{name}] no cooldown after an abandon"
        );
        // A cancel by the agent and an expiry start none either (checked by the same rule: only a decline counts).
        let fourth = r.submit(&key, 4);
        let fourth_id = fourth.id();
        let cancelled = runtime()
            .block_on(r.items().cancel(
                &fourth_id,
                crate::inbox::testing::CancelBy::Agent("2b7c91d0e4a853f6".into()),
            ))
            .unwrap();
        assert!(
            matches!(cancelled, crate::inbox::testing::Cancelled::Done(_)),
            "[{name}]"
        );
        assert_eq!(
            r.submit(&key, 4).status,
            201,
            "[{name}] no cooldown after a cancel"
        );
    });
    // With `deny_cooldown = 0s` there is no cooldown.
    each_rig(true, r#"deny_cooldown = "0s""#, |r, name| {
        let key = r.key_a();
        let id = r.submit(&key, 1).id();
        r.answer(&id, Action::Decline, None);
        assert_eq!(r.submit(&key, 1).status, 201, "[{name}]");
    });
}

#[test]
fn mute_after_three_refusals_alerts_owner() {
    each_rig(true, "", |r, name| {
        r.device();
        let key = r.key_a();
        let other = r.key_b();
        r.fix_clock(T0);
        let step = || r.fix_clock(r.now() + 1000);
        let answer = |subject: &str, n: u32, action: Action| {
            let id = r.submit(subject, n).id();
            step();
            r.answer(&id, action, None);
            step();
        };
        let alerts =
            || r.count("SELECT COUNT(*) FROM alerts WHERE kind = 'alert' AND source = 'portal'");
        // An approval ends the row: decline, decline, accept, decline, decline are not three in a row.
        answer(&key, 1, Action::Decline);
        answer(&key, 2, Action::Decline);
        answer(&key, 3, Action::Accept);
        answer(&key, 4, Action::Decline);
        answer(&key, 5, Action::Decline);
        assert_eq!(alerts(), 0, "[{name}] no mute before three in a row");
        assert_eq!(r.count("SELECT COUNT(*) FROM approval_mute"), 0, "[{name}]");
        // The third refusal in a row mutes the key for one hour and alerts the owner on the phone, once.
        answer(&key, 6, Action::Decline);
        assert_eq!(alerts(), 1, "[{name}] one alert");
        assert_eq!(r.count("SELECT COUNT(*) FROM approval_mute"), 1, "[{name}]");
        assert_eq!(
            r.count(
                "SELECT COUNT(*) FROM deliveries d JOIN alerts a ON a.id = d.alert_id
                 WHERE a.kind = 'alert' AND a.source = 'portal'"
            ),
            1,
            "[{name}] the alert goes to the phone of the owner"
        );
        // The alert and the audit event hold the id of the key and no text of an agent.
        assert_eq!(
            r.count("SELECT COUNT(*) FROM item_events WHERE event = 'muted'"),
            1,
            "[{name}]"
        );
        // A muted key gets `429 rate_limited` with the time that is left. Another key is free.
        let muted = r.submit(&key, 7);
        assert_eq!(muted.status, 429, "[{name}] {}", muted.body);
        assert_eq!(muted.json()["error"], "rate_limited", "[{name}]");
        let left = muted.json()["retry_after"].as_u64().unwrap();
        assert!((3590..=3600).contains(&left), "[{name}] {left}");
        assert_eq!(r.submit(&other, 1).status, 201, "[{name}]");
        // After one hour the key can ask again. One refusal after the mute is not three in a row.
        r.fix_clock(r.now() + 3_600_000);
        answer(&key, 7, Action::Decline);
        assert_eq!(
            alerts(),
            1,
            "[{name}] the refusals before the mute do not count again"
        );
        assert_eq!(r.submit(&key, 8).status, 201, "[{name}]");
    });
    // `mute_after_refusals = 0`: a key is never muted.
    each_rig(true, "mute_after_refusals = 0", |r, name| {
        let key = r.key_a();
        r.fix_clock(T0);
        for n in 1..=5 {
            let id = r.submit(&key, n).id();
            r.fix_clock(r.now() + 1000);
            r.answer(&id, Action::Decline, None);
        }
        assert_eq!(r.count("SELECT COUNT(*) FROM approval_mute"), 0, "[{name}]");
        assert_eq!(r.submit(&key, 6).status, 201, "[{name}]");
    });
}

#[test]
fn supersedes_cancels_old_approval() {
    each_rig(true, "", |r, name| {
        let key = r.key_a();
        let other = r.key_b();
        r.fix_clock(T0);
        let a = r.submit(&key, 1);
        let a_id = a.id();
        let replace = |subject: &str, n: u32, old: &str| {
            r.post(&r.body(
                subject,
                &command(n),
                json!({"lease_s": 900, "supersedes": old}),
            ))
        };
        // The new request replaces the old one: the old verdict is `void`, the new one `pending`, one chain.
        let b = replace(&key, 2, &a_id);
        assert_eq!(b.status, 201, "[{name}] {}", b.body);
        let b_id = b.id();
        assert_eq!(r.verdict(&a_id, &key), "void", "[{name}]");
        assert_eq!(r.verdict(&b_id, &key), "pending", "[{name}]");
        let (old, new) = (r.loaded(&a_id), r.loaded(&b_id));
        assert_eq!(
            old.item.state,
            inbox_core::proto::ItemState::Superseded,
            "[{name}]"
        );
        assert_eq!(
            old.item.superseded_by.as_deref(),
            Some(b_id.as_str()),
            "[{name}]"
        );
        assert_eq!(
            new.item.supersedes.as_deref(),
            Some(a_id.as_str()),
            "[{name}]"
        );
        assert_eq!(new.item.chain_id, old.item.chain_id, "[{name}]");
        assert_eq!(
            r.count("SELECT COUNT(*) FROM approvals WHERE open_hash IS NOT NULL"),
            1,
            "[{name}] one open request"
        );
        // An approval that was answered and not consumed can still be replaced, and the old approval cannot be used.
        r.answer(&b_id, Action::Accept, None);
        assert_eq!(
            r.ask(&key, &command(2).effect, false)["decision"],
            true,
            "[{name}] usable"
        );
        let c = replace(&key, 3, &b_id);
        assert_eq!(c.status, 201, "[{name}] {}", c.body);
        assert_eq!(r.verdict(&b_id, &key), "void", "[{name}]");
        assert_eq!(
            r.ask(&key, &command(2).effect, false)["decision"],
            false,
            "[{name}] not usable"
        );
        let c_id = c.id();
        // An approval that the service consumed cannot be replaced: 409.
        r.answer(&c_id, Action::Accept, None);
        assert_eq!(
            r.ask(&key, &command(3).effect, true)["decision"],
            true,
            "[{name}] consumed"
        );
        let refused = replace(&key, 4, &c_id);
        assert_eq!(
            refused.error(),
            (409, "cannot_supersede".to_string()),
            "[{name}]"
        );
        // An unknown request, a request of another key and a request of another service are `invalid_argument`.
        let d = r.submit(&key, 5);
        let d_id = d.id();
        for old in ["no-such", d_id.as_str()] {
            let subject = if old == d_id { &other } else { &key };
            let refused = replace(subject, 6, old);
            assert_eq!(
                refused.error(),
                (422, "invalid_argument".to_string()),
                "[{name}] {old}"
            );
        }
        assert_eq!(
            r.verdict(&d_id, &key),
            "pending",
            "[{name}] the request of the other key stays"
        );
        // An old request that is already final stays as it is, and the new request is a plain creation.
        let e = r.submit(&key, 7);
        let e_id = e.id();
        r.answer(&e_id, Action::Decline, None);
        let f = replace(&key, 8, &e_id);
        assert_eq!(f.status, 201, "[{name}] {}", f.body);
        assert_eq!(r.verdict(&e_id, &key), "denied", "[{name}]");
    });
}

#[test]
fn no_phone_409() {
    each_rig(true, "web_approval = false", |r, name| {
        let key = r.key_a();
        // The owner has no phone and cannot approve on the web: nothing can receive the request.
        let reply = r.submit(&key, 1);
        assert_eq!(
            reply.error(),
            (409, "no_device".to_string()),
            "[{name}] {}",
            reply.body
        );
        assert_eq!(r.count("SELECT COUNT(*) FROM items"), 0, "[{name}]");
        r.device();
        let reply = r.submit(&key, 1);
        assert_eq!(reply.status, 201, "[{name}] {}", reply.body);
        // The request is a delivery for the phone.
        assert_eq!(r.count("SELECT COUNT(*) FROM deliveries"), 1, "[{name}]");
    });
    // With `web_approval = true` (the default) the page of the owner receives the request: no phone is needed.
    each_rig(true, "", |r, name| {
        let key = r.key_a();
        assert_eq!(r.submit(&key, 1).status, 201, "[{name}]");
        assert_eq!(r.count("SELECT COUNT(*) FROM deliveries"), 0, "[{name}]");
    });
}

#[test]
fn idempotence_under_concurrency_one_open_request_for_each_effect() {
    each_rig(true, "", |r, name| {
        let key = r.key_a();
        let c = command(1);
        // Each thread has its own evaluation id and posts at once.
        let bodies: Vec<String> = (0..24)
            .map(|_| r.body(&key, &c, json!({"lease_s": 900})).to_string())
            .collect();
        let barrier = std::sync::Barrier::new(bodies.len());
        let replies: Vec<Reply> = std::thread::scope(|scope| {
            let handles: Vec<_> = bodies
                .iter()
                .map(|body| {
                    let barrier = &barrier;
                    scope.spawn(move || {
                        barrier.wait();
                        r.post_text(body)
                    })
                })
                .collect();
            handles.into_iter().map(|h| h.join().unwrap()).collect()
        });
        let created = replies.iter().filter(|x| x.status == 201).count();
        let repeated = replies.iter().filter(|x| x.status == 200).count();
        assert_eq!(
            (created, repeated),
            (1, 23),
            "[{name}] {:?}",
            replies.iter().map(|x| x.status).collect::<Vec<_>>()
        );
        let ids: std::collections::BTreeSet<String> = replies.iter().map(|x| x.id()).collect();
        assert_eq!(ids.len(), 1, "[{name}] one id for all");
        assert_eq!(r.count("SELECT COUNT(*) FROM approvals"), 1, "[{name}]");
        assert_eq!(
            r.count("SELECT COUNT(*) FROM approvals WHERE open_hash IS NOT NULL"),
            1,
            "[{name}]"
        );
    });
}

#[test]
fn a_unique_index_on_the_hash_refuses_a_second_open_request_and_frees_it_at_the_end() {
    each_rig(true, "", |r, name| {
        let key = r.key_a();
        let a = r.submit(&key, 1).id();
        let b = r.submit(&key, 2).id();
        let exec = |sql: String| runtime().block_on(r.items().test_exec(&sql));
        // A value that no other row has is good: the refusal below comes from the index and from nothing else.
        exec(format!(
            "UPDATE approvals SET open_hash = 'free' WHERE request_id = '{b}'"
        ))
        .unwrap();
        exec(format!(
            "UPDATE approvals SET open_hash = (SELECT key_hash FROM approvals WHERE request_id = '{b}') WHERE request_id = '{b}'"
        ))
        .unwrap();
        // The database itself refuses two open requests for one hash, whatever the code does.
        let clash = exec(format!(
            "UPDATE approvals SET open_hash = (SELECT open_hash FROM approvals WHERE request_id = '{a}') WHERE request_id = '{b}'"
        ));
        let error = clash.expect_err("the unique index must refuse it");
        eprintln!("[{name}] the database says: {error}");
        // The hash is free when the request ends, and not before.
        let open = |id: &str| {
            r.count(&format!("SELECT COUNT(*) FROM approvals WHERE request_id = '{id}' AND open_hash IS NOT NULL"))
        };
        assert_eq!((open(&a), open(&b)), (1, 1), "[{name}]");
        r.answer(&a, Action::Decline, None);
        assert_eq!(open(&a), 0, "[{name}] an answer ends the open state");
        runtime()
            .block_on(r.items().cancel(
                &b,
                crate::inbox::testing::CancelBy::Agent("2b7c91d0e4a853f6".into()),
            ))
            .unwrap();
        assert_eq!(open(&b), 0, "[{name}] a cancel too");
    });
}

#[test]
fn the_verdict_follows_the_state_and_only_four_values_exist() {
    each_rig(true, "", |r, name| {
        let key = r.key_a();
        r.fix_clock(T0);
        let make = |n: u32| r.submit(&key, n).id();
        // Each request is made and ended at once, so the key never has more than its 5 open requests.
        let pending = make(1);
        let approved = make(2);
        r.answer(&approved, Action::Accept, None);
        let denied = make(3);
        r.answer(&denied, Action::Decline, None);
        let cancelled = make(4);
        runtime()
            .block_on(r.items().cancel(
                &cancelled,
                crate::inbox::testing::CancelBy::Agent("2b7c91d0e4a853f6".into()),
            ))
            .unwrap();
        let abandoned = make(5);
        let consumed = make(7);
        r.answer(&consumed, Action::Accept, None);
        assert_eq!(
            r.ask(&key, &command(7).effect, true)["decision"],
            true,
            "[{name}] consume"
        );
        // The window to consume is the larger of 5 minutes and `lease_s` (15 minutes): inside it, `approved`.
        r.fix_clock(T0 + 301_000);
        assert_eq!(
            r.verdict(&approved, &key),
            "approved",
            "[{name}] inside the window"
        );
        // After it the approval that nobody consumed is `void`. The consumed one stays `approved`.
        r.fix_clock(T0 + 901_000);
        let verdicts: Vec<String> = [
            &pending, &approved, &denied, &cancelled, &abandoned, &consumed,
        ]
        .iter()
        .map(|id| r.verdict(id, &key))
        .collect();
        assert_eq!(
            verdicts,
            ["pending", "void", "denied", "void", "pending", "approved"],
            "[{name}] {verdicts:?}"
        );
        r.fix_clock(T0 + 3 * 900_000 + 121_000);
        assert_eq!(
            r.verdict(&abandoned, &key),
            "void",
            "[{name}] the lease and the grace ended"
        );
        assert_eq!(r.verdict(&pending, &key), "void", "[{name}]");
        check_what_the_service_learned(r, name);
    });
}

#[test]
fn a_read_waits_for_the_answer_and_never_longer_than_asked() {
    each_rig(true, "", |r, name| {
        let key = r.key_a();
        let id = r.submit(&key, 1).id();
        let q = format!("?subject={}&wait=", key.replace(':', "%3A"));
        // Nobody answers: the read returns `pending` after about the time asked.
        let begun = std::time::Instant::now();
        let reply = r.get(&id, &format!("{q}1"));
        assert_eq!(reply.json(), json!({"verdict": "pending"}), "[{name}]");
        assert!(
            begun.elapsed() >= Duration::from_millis(900),
            "[{name}] {:?}",
            begun.elapsed()
        );
        assert!(begun.elapsed() < Duration::from_secs(5), "[{name}]");
        // The owner answers while a read waits: the read returns at once with the verdict.
        let begun = std::time::Instant::now();
        let reply = std::thread::scope(|scope| {
            let waiting = scope.spawn(|| r.get(&id, &format!("{q}20")));
            std::thread::sleep(Duration::from_millis(400));
            r.answer(&id, Action::Accept, None);
            waiting.join().unwrap()
        });
        assert_eq!(
            reply.json(),
            json!({"verdict": "approved"}),
            "[{name}] {}",
            reply.body
        );
        assert!(
            begun.elapsed() < Duration::from_secs(10),
            "[{name}] {:?}",
            begun.elapsed()
        );
    });
}

#[test]
fn the_routes_answer_the_methods_and_the_unknown_paths_as_17_4_says() {
    each_rig(true, "", |r, name| {
        let auth = bearer();
        let send = |method: &str, path: &str, ct: bool| {
            let mut headers = vec![("Authorization", auth.as_str())];
            if ct {
                headers.push(("Content-Type", "application/json"));
            }
            r.record("other", send(r.port, method, path, &headers, "{}"))
        };
        assert_eq!(
            send("GET", "/approval/v1/requests", false).status,
            405,
            "[{name}]"
        );
        assert_eq!(
            send("GET", "/approval/v1/requests", false).header("allow"),
            Some("POST"),
            "[{name}]"
        );
        assert_eq!(
            send("POST", "/approval/v1/requests/abc", true).status,
            405,
            "[{name}]"
        );
        assert_eq!(
            send("GET", "/approval/v1/requests/abc/keepalive", false).status,
            405,
            "[{name}]"
        );
        assert_eq!(
            send("POST", "/approval/v1/requests", false).status,
            415,
            "[{name}] no JSON type"
        );
        assert_eq!(
            send("GET", "/approval/v1/requests/abc/other", false).status,
            404,
            "[{name}]"
        );
        assert_eq!(
            send("GET", "/approval/v1/requests/", false).status,
            404,
            "[{name}]"
        );
        assert_eq!(
            send("DELETE", "/approval/v1/requests/abc", false).status,
            405,
            "[{name}]"
        );
        assert_eq!(
            send("GET", "/approval/v1/other", false).status,
            404,
            "[{name}]"
        );
    });
}

#[test]
fn require_explanation_and_the_default_keys_reach_the_metadata_and_the_gate() {
    each_rig(true, "", |r, name| {
        let reply = send(r.port, "GET", "/.well-known/authzen-configuration", &[], "");
        let meta = reply.json();
        assert_eq!(
            meta["procurauth_approval_ref_ttl_seconds"], 600,
            "[{name}] intent_ttl 10m"
        );
        assert_eq!(
            meta["procurauth_approval_max_command_bytes"],
            16 * 1024,
            "[{name}]"
        );
        let members = meta
            .as_object()
            .unwrap()
            .keys()
            .filter(|k| k.starts_with("procurauth_approval_"))
            .count();
        assert_eq!(members, 12, "[{name}] the twelve members of the metadata");
    });
}
