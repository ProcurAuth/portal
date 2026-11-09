//! The black-box rig of the approvals: the real binary with the PDP, the items of the inbox, a service `db` that asks
//! for an approval, an owner with an agent key, and the calls of the PEP (the service) on the PDP listener.
//!
//! The portal learns the service `db` from the labels of a container and its actions from a document. This rig serves
//! both: a fake Docker socket and a fake page of actions, each on its own thread. The owner then holds a key whose
//! grant of `data.write` has `confirm`.

use std::io::{Read, Write};
use std::net::TcpListener;
use std::os::unix::net::UnixListener;
use std::time::{Duration, Instant};

use inbox_core::effect::effect_hash;
use inbox_core::proto::{ClientMsg, MessageMsg, RequestMsg, ServerMsg, SettledMsg};
use serde_json::{json, Value};

use super::{
    enroll_as, form_value, free_port, http_full, portal_ok, sign_in_as, users_form, Conn, PgAdmin,
    Phone, Recv, Server, ACCOUNT_PASSWORD,
};

/// The token of the PDP client of the service `db`.
pub const PDP_TOKEN: &str = "the PDP token of the e2e service db, 32 bytes or more";
/// The login of the owner of the agent key.
pub const OWNER: &str = "dana";
pub const RESOURCE: &str = "host:prod/database:analytics";

const DOCUMENT: &str = r#"{"server":"db",
  "resource_types":[{"name":"host","parent":"service","description":"A host."},
                    {"name":"database","parent":"host","description":"A database."}],
  "actions":[
    {"name":"data.read","resource_types":["host","database"],"description":"Read.","limits":["requests"]},
    {"name":"data.write","resource_types":["host","database"],"description":"Write.",
     "limits":["requests"],"approval":"optional"}],
  "limits":[
    {"name":"requests","label":"Requests","description":"d","unit":"request","kind":"budget"}]}"#;

/// The items and their content key, as the blocks of the file read them.
const ITEMS: &str = r#"
  items {
    max_lifetime = "90d"
    max_open_per_key = 20
    max_open_per_owner = 50
    max_open_total = 1000
    retention = "180d"
    body_purge_after = "30d"
    audit_retention = "365d"
    lease { min = "5s" max = "7d" grace = "5s" revive_window = "7d" }
    digest { mode = "off" }
  }
  at_rest {
    active_key = "k1"
    key "k1" { secret_file = "CONTENT_KEY" }
  }
  approvals {
    consume_within = "5m"
    deny_cooldown = "0s"
    mute_after_refusals = 0
    max_requests_per_key_per_hour = 500
  }
  mcp {}
"#;

/// A command of the service: the operation, its view and its effect.
pub struct Command {
    pub op: Value,
    pub view: Value,
    pub effect: String,
}

pub fn command(n: u32) -> Command {
    let op = json!({"tool": "run_write_query", "host": "prod", "database": "analytics",
                    "sql": format!("INSERT INTO analytics.events SELECT {n}")});
    let view = json!({"title": "Write to ClickHouse",
                      "fields": [["Host", "/host"], ["Database", "/database"]],
                      "body": "/sql", "body_label": "SQL"});
    let effect = effect_hash("db", "data.write", RESOURCE, &op).unwrap();
    Command { op, view, effect }
}

/// One answer of the PDP listener.
pub struct Reply {
    pub status: u16,
    pub body: String,
}

impl Reply {
    pub fn json(&self) -> Value {
        serde_json::from_str(&self.body).unwrap_or(Value::Null)
    }
}

/// What a request of a real agent gave: the request, and what the channel gave back.
#[derive(Debug)]
pub struct AgentRequest {
    pub request_id: String,
    pub code: String,
    /// The token of the agent. Only the call that made the agent has it.
    pub token: Option<String>,
    /// `agent_status` of the intent: `new`, `known`, `unattributed` or `key`.
    pub status: String,
}

/// The server, the service, the owner and its agent key.
pub struct Pep {
    pub s: Server,
    pub pdp_port: u16,
    pub key_id: String,
    /// The text of the agent key, shown once when the owner made it.
    pub key_text: String,
}

fn serve_unix(path: std::path::PathBuf, body: String) {
    let listener = UnixListener::bind(&path).expect("the fake Docker socket");
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let mut buf = [0u8; 4096];
            let _ = stream.read(&mut buf);
            let head = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            let _ = stream.write_all(head.as_bytes());
            let _ = stream.write_all(body.as_bytes());
        }
    });
}

fn serve_tcp(listener: TcpListener, body: &'static str) {
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let mut buf = [0u8; 4096];
            let _ = stream.read(&mut buf);
            let head = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            let _ = stream.write_all(head.as_bytes());
            let _ = stream.write_all(body.as_bytes());
        }
    });
}

impl Pep {
    /// Starts the server with the approvals and makes the owner, the agent key and the service.
    pub fn new() -> Pep {
        Pep::with(|_| {})
    }

    /// The same, after `tweak` changed the files of the server and before it starts.
    pub fn with(tweak: impl FnOnce(&Server)) -> Pep {
        Pep::with_mode(tweak, None)
    }

    /// The same, with the mode of the agents of the key of the rig (`Some("enrolled")`). The field exists only when the file has
    /// the block `inbox.agents`, so `tweak` adds the block.
    pub fn with_mode(tweak: impl FnOnce(&Server), agents: Option<&str>) -> Pep {
        let mut s = Server::setup();
        // The test phones send no heartbeat: the session must outlive the pauses of the test.
        let text = std::fs::read_to_string(s.config())
            .unwrap()
            .replace("session_timeout = \"10s\"", "session_timeout = \"120s\"")
            .replace(
                "unreachable_after = \"10s\"",
                "unreachable_after = \"120s\"",
            );
        std::fs::write(s.config(), text).unwrap();
        let dir = s.dir.clone();
        let p = |n: &str| dir.join(n).to_string_lossy().to_string();

        // The service: a fake Docker socket that lists one container, and a fake page of actions.
        let actions = TcpListener::bind("127.0.0.1:0").unwrap();
        let actions_port = actions.local_addr().unwrap().port();
        serve_tcp(actions, DOCUMENT);
        let containers = json!([{
            "Id": "a", "Names": ["/db"], "State": "running", "Status": "Up 1 minute",
            "Labels": {
                "portal.name": "Db", "portal.id": "db", "portal.href": "/db/",
                "portal.actions": format!("http://127.0.0.1:{actions_port}/.well-known/procurauth/actions"),
            }
        }]);
        serve_unix(dir.join("docker.sock"), containers.to_string());

        // The files of the blocks.
        portal_ok(&["inbox", "content-key", "--out", &p("content_key")]);
        std::fs::write(dir.join("pdp_token"), PDP_TOKEN).unwrap();
        std::fs::write(
            dir.join("identity.pem"),
            "-----BEGIN PRIVATE KEY-----\nMC4CAQAwBQYDK2VwBCIEIAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8g\n-----END PRIVATE KEY-----\n",
        )
        .unwrap();

        let from = "keys {\n  mcp = false\n  api = true\n}";
        let text = std::fs::read_to_string(s.config()).unwrap();
        assert!(text.contains(from));
        let pdp_port = free_port();
        let text = text.replace(from, "keys {\n  mcp = true\n  api = true\n}")
            + &format!(
                r#"
identity {{
  issuer = "https://portal.e2e"
  signing_key_file = "{identity}"
}}
pdp {{
  listen_address = "127.0.0.1:{pdp_port}"
  url = "http://127.0.0.1:{pdp_port}"
  plain_http = true
}}
pdp_client "db" {{
  token_file = "{token}"
  approvals = true
}}
discovery {{
  docker_socket = "{socket}"
  interval = "2s"
}}
"#,
                identity = p("identity.pem"),
                token = p("pdp_token"),
                socket = p("docker.sock"),
            );
        std::fs::write(s.config(), text).unwrap();
        s.add_to_inbox(&ITEMS.replace("CONTENT_KEY", &p("content_key")));
        tweak(&s);
        s.start();

        let mut pep = Pep {
            s,
            pdp_port,
            key_id: String::new(),
            key_text: String::new(),
        };
        pep.make_owner_and_key(agents);
        pep
    }

    /// The owner, with the grants on the service `db`, and its agent key.
    fn make_owner_and_key(&mut self, agents: Option<&str>) {
        let s = &self.s;
        users_form(
            s,
            "/auth/users/add",
            &format!("login={OWNER}&password={ACCOUNT_PASSWORD}"),
        );
        let grants = [
            "access on service:db",
            "data.write on service:db/host:prod",
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
        // The portal reads the document of the service after the first read of the catalogue.
        let deadline = Instant::now() + Duration::from_secs(60);
        loop {
            let location = users_form(s, "/auth/users/update", &fields);
            if location.contains("s=user-updated") || location.contains("s=unchanged") {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "the service db did not show up: {location} {}",
                s.log()
            );
            std::thread::sleep(Duration::from_secs(1));
        }
        // The session is registered: `account(OWNER)` must not make the account again with other permissions.
        let session = std::sync::Arc::new(sign_in_as(s.port, OWNER, ACCOUNT_PASSWORD));
        let cookie = session.cookie.clone();
        s.accounts
            .lock()
            .unwrap()
            .insert(OWNER.to_string(), session);
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
            "csrf={csrf}&kind=mcp&name=agent-laptop&expires_at=&interaction=conversational{}\
             &grant0={}&grant1={}&confirm1=1",
            agents.map_or(String::new(), |m| format!("&agents={m}")),
            form_value("access on service:db"),
            form_value(&format!("data.write on service:db/{RESOURCE}")),
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
        self.key_text = page[at..].split('"').next().unwrap().to_string();
        // The id of the key is in the title of its row on the page Keys.
        let (_, _, list) = http_full(
            s.port,
            &host,
            "GET",
            "/auth/settings",
            &[("Cookie", cookie)],
            "",
        );
        let at = list.find("agent-laptop").expect("the key is listed");
        let head = &list[..at];
        let id = head.rfind("title=\"Id ").expect("the id of the key") + "title=\"Id ".len();
        self.key_id = list[id..].split(['.', '"']).next().unwrap().to_string();
    }

    /// A second key of the owner, made through the form of the keys, in the given mode of the agents (lot G6). The result is
    /// the id of the key and its text. The key has the same grants as the key of the rig.
    pub fn make_key_in_mode(&self, name: &str, agents: &str) -> (String, String) {
        let s = &self.s;
        let cookie = self.cookie(OWNER);
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
            "csrf={csrf}&kind=mcp&name={name}&expires_at=&interaction=conversational&agents={agents}\
             &grant0={}&grant1={}&confirm1=1",
            form_value("access on service:db"),
            form_value(&format!("data.write on service:db/{RESOURCE}")),
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
        let at = list.find(name).expect("the key is listed");
        let head = &list[..at];
        let id = head.rfind("title=\"Id ").expect("the id of the key") + "title=\"Id ".len();
        (
            list[id..].split(['.', '"']).next().unwrap().to_string(),
            text,
        )
    }

    pub fn subject(&self) -> String {
        format!("key:{}", self.key_id)
    }

    /// A call to the PDP listener.
    pub fn pdp(&self, method: &str, path: &str, body: &str) -> Reply {
        let host = format!("127.0.0.1:{}", self.pdp_port);
        let (status, _, body) = http_full(
            self.pdp_port,
            &host,
            method,
            path,
            &[("Authorization", format!("Bearer {PDP_TOKEN}"))],
            body,
        );
        Reply { status, body }
    }

    /// The question of the service to the PDP: may the key do the action with this effect. `usage` takes a unit.
    pub fn evaluation(&self, effect: &str, usage: bool) -> Value {
        let mut context = serde_json::Map::new();
        if usage {
            context.insert("usage".into(), json!({"units": 1}));
        }
        context.insert("approval".into(), json!({"effect": effect}));
        let body = json!({
            "subject": {"type": "user", "id": self.subject()},
            "action": {"name": "data.write"},
            "resource": {"type": "database", "id": "analytics", "properties": {"ancestors": [
                {"type": "service", "id": "db"}, {"type": "host", "id": "prod"}]}},
            "context": context
        })
        .to_string();
        let reply = self.pdp("POST", "/access/v1/evaluation", &body);
        assert_eq!(reply.status, 200, "{}", reply.body);
        reply.json()
    }

    /// The submission of the service: a question to the PDP for the `evaluation_id`, then the request. The answer is
    /// the status and the body.
    pub fn submit_with(&self, c: &Command, control: Value) -> Reply {
        let answer = self.evaluation(&c.effect, false);
        let evaluation_id = answer["context"]["evaluation_id"]
            .as_str()
            .unwrap_or_else(|| panic!("no evaluation_id in {answer}"))
            .to_string();
        let mut body = json!({
            "evaluation_id": evaluation_id,
            "effect": c.effect, "op": c.op, "view": c.view,
        });
        for (k, v) in control.as_object().unwrap() {
            body[k] = v.clone();
        }
        self.pdp("POST", "/approval/v1/requests", &body.to_string())
    }

    /// A request with a lease of `lease_s` seconds. The result is the id and the code.
    pub fn submit(&self, c: &Command, lease_s: u64) -> (String, String) {
        let reply = self.submit_with(c, json!({"lease_s": lease_s}));
        assert!(
            reply.status == 201 || reply.status == 200,
            "{}: {}",
            reply.status,
            reply.body
        );
        let v = reply.json();
        (
            v["request_id"].as_str().unwrap().to_string(),
            v["code"].as_str().unwrap().to_string(),
        )
    }

    /// A tool of the agent channel (`/mcp`), with the key of the owner. The result is the structured content.
    pub fn tool(&self, name: &str, args: Value) -> Value {
        let body = json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call",
                          "params": {"name": name, "arguments": args}})
        .to_string();
        let host = format!("127.0.0.1:{}", self.s.port);
        let (status, _, text) = http_full(
            self.s.port,
            &host,
            "POST",
            "/mcp",
            &[
                ("Authorization", format!("Bearer {}", self.key_text)),
                ("Content-Type", "application/json".into()),
            ],
            &body,
        );
        assert_eq!(status, 200, "{text}");
        let reply: Value = serde_json::from_str(&text).expect("JSON");
        assert_ne!(reply["result"]["isError"], true, "{text}");
        reply["result"]["structuredContent"].clone()
    }

    /// A tool of the agent channel with the agent token and the headers of a real client, as an agent calls it. `token`
    /// goes in the header `Agent-Token`. The result is the flag `isError` and the structured content.
    pub fn tool_as(
        &self,
        token: Option<&str>,
        user_agent: &str,
        name: &str,
        args: Value,
    ) -> (bool, Value) {
        let body = json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call",
                          "params": {"name": name, "arguments": args}})
        .to_string();
        let host = format!("127.0.0.1:{}", self.s.port);
        let mut headers = vec![
            ("Authorization", format!("Bearer {}", self.key_text)),
            ("Content-Type", "application/json".to_string()),
            ("User-Agent", user_agent.to_string()),
        ];
        if let Some(t) = token {
            headers.push(("Agent-Token", t.to_string()));
        }
        let (status, _, text) = http_full(self.s.port, &host, "POST", "/mcp", &headers, &body);
        assert_eq!(status, 200, "{text}");
        let reply: Value = serde_json::from_str(&text).expect("JSON");
        (
            reply["result"]["isError"] == true,
            reply["result"]["structuredContent"].clone(),
        )
    }

    /// A request of a real agent through the real channel: the call of `approval_intent` (with the token, or with none:
    /// then the portal makes the agent), then the submission of the service with the `approval_ref`. `extra` holds the
    /// more arguments of the intent, such as `agent_label`. No SQL: the agent is what the channel made.
    pub fn agent_request(
        &self,
        token: Option<&str>,
        user_agent: &str,
        c: &Command,
        extra: Value,
    ) -> AgentRequest {
        let mut args = json!({"lease_s": 600});
        for (k, v) in extra.as_object().unwrap() {
            args[k] = v.clone();
        }
        let (is_error, intent) = self.tool_as(token, user_agent, "approval_intent", args);
        assert!(!is_error, "{intent}");
        let reference = intent["approval_ref"].as_str().expect("an approval_ref");
        let reply = self.submit_with(c, json!({"approval_ref": reference}));
        assert!(
            reply.status == 201 || reply.status == 200,
            "{}: {}",
            reply.status,
            reply.body
        );
        let v = reply.json();
        AgentRequest {
            request_id: v["request_id"].as_str().unwrap().to_string(),
            code: v["code"].as_str().unwrap().to_string(),
            token: intent["agent_token"].as_str().map(str::to_string),
            status: intent["agent_status"]
                .as_str()
                .unwrap_or_default()
                .to_string(),
        }
    }

    /// A request with an intent: the agent gives the texts to the portal, the service gives the command. The result is
    /// the id and the code.
    pub fn submit_intent(&self, c: &Command, intent: Value, lease_s: u64) -> (String, String) {
        let mut args = json!({"lease_s": lease_s});
        for (k, v) in intent.as_object().unwrap() {
            args[k] = v.clone();
        }
        let reference = self.tool("approval_intent", args)["approval_ref"]
            .as_str()
            .expect("an approval_ref")
            .to_string();
        let reply = self.submit_with(c, json!({"approval_ref": reference}));
        assert!(
            reply.status == 201 || reply.status == 200,
            "{}: {}",
            reply.status,
            reply.body
        );
        let v = reply.json();
        (
            v["request_id"].as_str().unwrap().to_string(),
            v["code"].as_str().unwrap().to_string(),
        )
    }

    /// What the service reads: the verdict of a request.
    pub fn verdict(&self, id: &str) -> Value {
        let subject = self.subject().replace(':', "%3A");
        let reply = self.pdp(
            "GET",
            &format!("/approval/v1/requests/{id}?subject={subject}"),
            "",
        );
        assert_eq!(reply.status, 200, "{}", reply.body);
        reply.json()
    }

    /// Waits until the verdict is not `pending`.
    pub fn verdict_settles(&self, id: &str, within: Duration) -> Value {
        let deadline = Instant::now() + within;
        loop {
            let v = self.verdict(id);
            if v["verdict"] != "pending" {
                return v;
            }
            assert!(Instant::now() < deadline, "the verdict stays pending");
            std::thread::sleep(Duration::from_millis(300));
        }
    }

    /// The service resumes the action: the same question with `usage`. The PDP consumes the approval once.
    pub fn resume(&self, c: &Command) -> Value {
        self.evaluation(&c.effect, true)
    }

    /// A phone of the owner.
    pub async fn phone(&self, name: &str) -> Phone {
        enroll_as(&self.s, name, OWNER)
            .await
            .expect("the phone enrolls")
    }
}

// ---------------------------------------------------------------- what a phone reads

/// The next frame of a kind. The other frames that the phone may get on the way are skipped.
pub async fn next_request(c: &mut Conn, wait: Duration) -> RequestMsg {
    let deadline = Instant::now() + wait;
    loop {
        match c
            .recv(deadline.saturating_duration_since(Instant::now()))
            .await
        {
            Recv::Msg(ServerMsg::Request(r)) => {
                c.seen.insert(r.id.clone());
                return *r;
            }
            Recv::Msg(_) => continue,
            Recv::Closed(code) => panic!("closed while the phone waits for a request: {code:?}"),
            Recv::Timeout => panic!("no request within {wait:?}"),
        }
    }
}

pub async fn next_settled(c: &mut Conn, wait: Duration) -> SettledMsg {
    let deadline = Instant::now() + wait;
    loop {
        match c
            .recv(deadline.saturating_duration_since(Instant::now()))
            .await
        {
            Recv::Msg(ServerMsg::Settled(s)) => {
                c.seen.insert(s.id.clone());
                return s;
            }
            Recv::Msg(_) => continue,
            Recv::Closed(code) => panic!("closed while the phone waits for settled: {code:?}"),
            Recv::Timeout => panic!("no settled within {wait:?}"),
        }
    }
}

pub async fn next_message(c: &mut Conn, wait: Duration) -> MessageMsg {
    let deadline = Instant::now() + wait;
    loop {
        match c
            .recv(deadline.saturating_duration_since(Instant::now()))
            .await
        {
            Recv::Msg(ServerMsg::Message(m)) => {
                c.seen.insert(m.id.clone());
                return m;
            }
            Recv::Msg(_) => continue,
            Recv::Closed(code) => panic!("closed while the phone waits for a message: {code:?}"),
            Recv::Timeout => panic!("no message within {wait:?}"),
        }
    }
}

/// The next `answer_result`.
pub async fn next_result(c: &mut Conn, wait: Duration) -> inbox_core::proto::AnswerResultMsg {
    let deadline = Instant::now() + wait;
    loop {
        match c
            .recv(deadline.saturating_duration_since(Instant::now()))
            .await
        {
            Recv::Msg(ServerMsg::AnswerResult(r)) => return r,
            Recv::Msg(_) => continue,
            Recv::Closed(code) => panic!("closed while the phone waits for a result: {code:?}"),
            Recv::Timeout => panic!("no answer_result within {wait:?}"),
        }
    }
}

/// Nothing of this kind arrives within `wait`. A frame of another kind is skipped. This is strict: a copy of a frame that
/// the test read is a failure. A test of the receipts uses it (a frame must not come again after its receipt).
pub async fn nothing_arrives(c: &mut Conn, wait: Duration) {
    let deadline = Instant::now() + wait;
    while Instant::now() < deadline {
        match c
            .recv(deadline.saturating_duration_since(Instant::now()))
            .await
        {
            Recv::Msg(ServerMsg::Request(r)) => panic!("unexpected request {r:?}"),
            Recv::Msg(ServerMsg::Settled(s)) => panic!("unexpected settled {s:?}"),
            Recv::Msg(ServerMsg::Message(m)) => panic!("unexpected message {m:?}"),
            Recv::Closed(code) => panic!("closed: {code:?}"),
            _ => {}
        }
    }
}

/// Nothing new arrives within `wait`. A frame that the test read already (`Conn::seen`) is not new: it is a copy that the
/// server sent again because the receipt of the last frame of a dropped session was lost. The copy gets its receipt. A
/// frame with another delivery id is a failure.
pub async fn nothing_new_arrives(c: &mut Conn, wait: Duration) {
    let deadline = Instant::now() + wait;
    while Instant::now() < deadline {
        match c
            .recv(deadline.saturating_duration_since(Instant::now()))
            .await
        {
            Recv::Msg(ServerMsg::Request(r)) if c.seen.contains(&r.id) => c.receipt(&r.id).await,
            Recv::Msg(ServerMsg::Settled(s)) if c.seen.contains(&s.id) => c.receipt(&s.id).await,
            Recv::Msg(ServerMsg::Message(m)) if c.seen.contains(&m.id) => c.receipt(&m.id).await,
            Recv::Msg(ServerMsg::Request(r)) => panic!("unexpected request {r:?}"),
            Recv::Msg(ServerMsg::Settled(s)) => panic!("unexpected settled {s:?}"),
            Recv::Msg(ServerMsg::Message(m)) => panic!("unexpected message {m:?}"),
            Recv::Closed(code) => panic!("closed: {code:?}"),
            _ => {}
        }
    }
}

impl Conn {
    /// Takes the delivery ids that the old session of the same phone read (see `Conn::seen`).
    pub fn adopt_seen(&mut self, old: &std::collections::HashSet<String>) {
        self.seen.extend(old.iter().cloned());
    }

    /// Sends the receipt of a frame by its delivery id.
    pub async fn receipt(&mut self, id: &str) {
        self.send(&ClientMsg::Ack {
            id: id.into(),
            stage: inbox_core::proto::AckStage::Received,
        })
        .await;
    }
}

// ---------------------------------------------------------------- the database, read and changed directly

impl PgAdmin {
    /// The first column of the first row of a query, cast to text. `None` for no row or a NULL.
    pub fn scalar(&self, database: &str, sql: &str) -> Option<String> {
        let (host, port, user, password, database) = (
            self.host.clone(),
            self.port,
            self.user.clone(),
            self.password.clone(),
            database.to_string(),
        );
        let sql = format!("SELECT ({sql})::text");
        std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            rt.block_on(async {
                let mut config = tokio_postgres::Config::new();
                config
                    .host(&host)
                    .port(port)
                    .user(&user)
                    .password(&password)
                    .dbname(&database);
                let (client, connection) = config.connect(tokio_postgres::NoTls).await.unwrap();
                tokio::spawn(connection);
                let rows = client.query(&sql, &[]).await.ok()?;
                rows.first()?.get::<_, Option<String>>(0)
            })
        })
        .join()
        .unwrap()
    }

    /// Every row of the tables as JSON text. The tests read the database directly: they check what the server keeps.
    pub fn dump(&self, database: &str, tables: &[&str]) -> String {
        let (host, port, user, password, database) = (
            self.host.clone(),
            self.port,
            self.user.clone(),
            self.password.clone(),
            database.to_string(),
        );
        let tables: Vec<String> = tables.iter().map(|t| t.to_string()).collect();
        std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            rt.block_on(async {
                let mut config = tokio_postgres::Config::new();
                config
                    .host(&host)
                    .port(port)
                    .user(&user)
                    .password(&password)
                    .dbname(&database);
                let (client, connection) = config.connect(tokio_postgres::NoTls).await.unwrap();
                tokio::spawn(connection);
                let mut out = String::new();
                for table in &tables {
                    let rows = client
                        .query(&format!("SELECT row_to_json(t)::text FROM {table} t"), &[])
                        .await
                        .unwrap();
                    for r in rows {
                        out.push_str(&r.get::<_, String>(0));
                        out.push('\n');
                    }
                }
                out
            })
        })
        .join()
        .unwrap()
    }
}

impl Server {
    /// The first column of the first row of a query, as text. `None` for no row or a NULL. The tests read what the server
    /// keeps: they never write the rows of the agents (the agents come from the channel).
    pub fn scalar(&self, sql: &str) -> Option<String> {
        match &self.pg {
            Some((admin, database)) => admin.scalar(database, sql),
            None => {
                let c = rusqlite::Connection::open(self.dir.join("portal.db")).unwrap();
                c.busy_timeout(Duration::from_secs(10)).unwrap();
                c.query_row(sql, [], |row| {
                    row.get::<_, rusqlite::types::Value>(0).map(|v| match v {
                        rusqlite::types::Value::Null => None,
                        rusqlite::types::Value::Integer(i) => Some(i.to_string()),
                        rusqlite::types::Value::Text(t) => Some(t),
                        other => Some(format!("{other:?}")),
                    })
                })
                .ok()
                .flatten()
            }
        }
    }

    /// Everything that the database of the server holds, as text that a plain value would show in: the file of
    /// SQLite with its log, or the rows of the tables of PostgreSQL (a `bytea` column shows its hexadecimal text).
    pub fn database_text(&self) -> String {
        match &self.pg {
            Some((admin, database)) => admin.dump(
                database,
                &[
                    "alerts",
                    "deliveries",
                    "items",
                    "turns",
                    "approvals",
                    "item_events",
                    "approval_intents",
                    "devices",
                    "agents",
                    "agent_events",
                ],
            ),
            None => {
                let mut bytes = Vec::new();
                for suffix in ["", "-wal"] {
                    if let Ok(more) = std::fs::read(self.dir.join(format!("portal.db{suffix}"))) {
                        bytes.extend(more);
                    }
                }
                String::from_utf8_lossy(&bytes).into_owned()
            }
        }
    }

    /// Revokes a phone in the database, as another instance would: the session of this instance gets no signal.
    pub fn revoke_in_database(&self, device_id: &str) {
        match &self.pg {
            Some((admin, database)) => admin.exec(
                database,
                format!("UPDATE devices SET revoked_at = 1 WHERE id = '{device_id}'"),
            ),
            None => {
                let c = rusqlite::Connection::open(self.dir.join("portal.db")).unwrap();
                c.busy_timeout(Duration::from_secs(10)).unwrap();
                let n = c
                    .execute(
                        "UPDATE devices SET revoked_at = 1 WHERE id = ?1",
                        [device_id],
                    )
                    .unwrap();
                assert_eq!(n, 1);
            }
        }
    }
}

// ---------------------------------------------------------------- the web pages of the items

/// The value of the header `name` in the head of an answer.
pub fn header_of(head: &str, name: &str) -> Option<String> {
    head.lines().find_map(|l| {
        let (n, v) = l.split_once(':')?;
        n.eq_ignore_ascii_case(name).then(|| v.trim().to_string())
    })
}

/// The value of the hidden field `name` in the form whose action is `action`.
pub fn hidden_in(page: &str, action: &str, name: &str) -> String {
    let form = page
        .find(&format!("action=\"{action}\""))
        .unwrap_or_else(|| panic!("no form {action} in {page}"));
    let end = page[form..]
        .find("</form>")
        .map_or(page.len(), |e| form + e);
    let marker = format!("name=\"{name}\" value=\"");
    let at = page[form..end]
        .find(&marker)
        .unwrap_or_else(|| panic!("no field {name} in the form {action}"))
        + form
        + marker.len();
    super::unescape(page[at..].split('"').next().unwrap())
}

pub const INBOX: &str = "/auth/settings/inbox";

pub fn item_path(id: &str) -> String {
    format!("{INBOX}/items/{id}")
}

impl Pep {
    /// The session cookie of an account that the rig made.
    pub fn cookie(&self, login: &str) -> String {
        self.s.account(login).cookie.clone()
    }

    /// A page with the session of an account: the status, the head and the body.
    pub fn page(&self, cookie: &str, path: &str) -> (u16, String, String) {
        let host = format!("127.0.0.1:{}", self.s.port);
        http_full(
            self.s.port,
            &host,
            "GET",
            path,
            &[("Cookie", cookie.to_string())],
            "",
        )
    }

    /// A form with the session of an account. `fields` has no `csrf`: the call adds `csrf`. `origin` is the header
    /// `Origin`: `None` sends the right one, `Some("")` sends none.
    pub fn post(
        &self,
        cookie: &str,
        path: &str,
        csrf: &str,
        fields: &str,
        origin: Option<&str>,
    ) -> (u16, String, String) {
        let host = format!("127.0.0.1:{}", self.s.port);
        let body = if csrf.is_empty() {
            fields.to_string()
        } else {
            format!("csrf={}&{fields}", form_value(csrf))
        };
        let mut headers = vec![
            (
                "Content-Type",
                "application/x-www-form-urlencoded".to_string(),
            ),
            ("Cookie", cookie.to_string()),
        ];
        match origin {
            None => headers.push(("Origin", format!("http://{host}"))),
            Some("") => {}
            Some(o) => headers.push(("Origin", o.to_string())),
        }
        http_full(self.s.port, &host, "POST", path, &headers, &body)
    }

    /// The answer form of a request, as the owner uses it: the page gives the token and the effect, the call sends
    /// `action` and `extra`. The result is the status, the head and the body.
    pub fn answer_form(
        &self,
        cookie: &str,
        id: &str,
        action: &str,
        extra: &str,
    ) -> (u16, String, String) {
        let (st, _, page) = self.page(cookie, &item_path(id));
        assert_eq!(st, 200, "{page}");
        let answer = format!("{INBOX}/items/answer");
        let csrf = hidden_in(&page, &answer, "csrf");
        let shown = hidden_in(&page, &answer, "shown_effect");
        let mut fields = format!(
            "request_id={id}&action={action}&shown_effect={}",
            form_value(&shown)
        );
        if !extra.is_empty() {
            fields.push('&');
            fields.push_str(extra);
        }
        self.post(cookie, &answer, &csrf, &fields, None)
    }

    /// The token `csrf` of any form of the page of the Inbox, for a session.
    pub fn inbox_csrf(&self, cookie: &str) -> String {
        let (_, _, page) = self.page(cookie, INBOX);
        let at = page
            .find("action=\"/auth/settings/inbox/")
            .expect("a form of the page");
        let marker = "name=\"csrf\" value=\"";
        let at = at + page[at..].find(marker).unwrap() + marker.len();
        super::unescape(page[at..].split('"').next().unwrap())
    }

    /// The owner makes an agent for the key of the rig with the form "Add agent" (lot G6; the key is in the mode `enrolled`).
    pub fn enroll_via_web(&self, cookie: &str, name: &str) -> (u16, String, String) {
        let csrf = self.inbox_csrf(cookie);
        self.post(
            cookie,
            "/auth/settings/inbox/agents/enroll",
            &csrf,
            &format!("key={}&name={}", self.key_id, form_value(name)),
            None,
        )
    }

    /// The owner resets the token of an agent with the form of its page.
    pub fn reset_via_web(&self, cookie: &str, agent_id: &str) -> (u16, String, String) {
        let csrf = self.inbox_csrf(cookie);
        self.post(
            cookie,
            "/auth/settings/inbox/agents/reset",
            &csrf,
            &format!("agent_id={agent_id}"),
            None,
        )
    }

    /// The token that the page of the token shows.
    pub fn token_of(page: &str) -> String {
        let marker = "id=\"agent-token\" type=\"text\" readonly value=\"";
        let at = page.find(marker).expect("the page shows a token") + marker.len();
        page[at..].split('"').next().unwrap().to_string()
    }

    /// The text of the page that a person reads, in lower case: the part of `main`.
    pub fn text_of(page: &str) -> String {
        let start = page.find("<main").unwrap_or(0);
        page[start..].to_lowercase()
    }
}

/// A command of the service with the text of the SQL and the title that the test chooses.
pub fn command_with(sql: &str, title: &str) -> Command {
    let op =
        json!({"tool": "run_write_query", "host": "prod", "database": "analytics", "sql": sql});
    let view = json!({"title": title,
                      "fields": [["Host", "/host"], ["Database", "/database"]],
                      "body": "/sql", "body_label": "SQL"});
    let effect = effect_hash("db", "data.write", RESOURCE, &op).unwrap();
    Command { op, view, effect }
}
