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

/// The server, the service, the owner and its agent key.
pub struct Pep {
    pub s: Server,
    pub pdp_port: u16,
    pub key_id: String,
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
        };
        pep.make_owner_and_key();
        pep
    }

    /// The owner, with the grants on the service `db`, and its agent key.
    fn make_owner_and_key(&mut self) {
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
            "csrf={csrf}&kind=mcp&name=agent-laptop&expires_at=&interaction=conversational\
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
            Recv::Msg(ServerMsg::Request(r)) => return *r,
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
            Recv::Msg(ServerMsg::Settled(s)) => return s,
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
            Recv::Msg(ServerMsg::Message(m)) => return m,
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

/// Nothing of this kind arrives within `wait`. A frame of another kind is skipped.
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

impl Conn {
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
