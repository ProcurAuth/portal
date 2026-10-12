//! Black-box end-to-end test: runs the real `inbox` binary and drives it with
//! the real protocol (OPAQUE enrollment, Noise sessions, signed admin requests).

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use futures_util::{SinkExt, StreamExt};
use inbox_core::enroll::{ClientStep, EnrollClient, EnrolledDevice};
use inbox_core::noise::{Initiator, Session};
use inbox_core::proto::{AckStage, AlertMsg, ClientMsg, ServerMsg, SessionHello};
use inbox_core::rng::Rng;
use serde_json::{Value, json};
use tokio_tungstenite::tungstenite::Message;

const BIN: &str = env!("CARGO_BIN_EXE_inbox");
const DEVICE_PW: &str = "device-password";

type Ws =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
}

fn inbox(args: &[&str], stdin: Option<&str>) -> std::process::Output {
    let mut child = Command::new(BIN)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    if let Some(input) = stdin {
        child.stdin.take().unwrap().write_all(input.as_bytes()).unwrap();
    }
    child.wait_with_output().unwrap()
}

/// An administrative PostgreSQL connection, from `INBOX_TEST_PG=host:port:user:password`. The tests that need it are
/// skipped when it is absent; `INBOX_E2E_BACKEND=postgres` runs the whole suite against it instead of SQLite.
#[derive(Clone)]
struct PgAdmin {
    host: String,
    port: u16,
    user: String,
    password: String,
}

fn pg_admin() -> Option<PgAdmin> {
    let raw = std::env::var("INBOX_TEST_PG").ok()?;
    let mut parts = raw.splitn(4, ':');
    Some(PgAdmin {
        host: parts.next()?.to_string(),
        port: parts.next()?.parse().ok()?,
        user: parts.next()?.to_string(),
        password: parts.next()?.to_string(),
    })
}

fn postgres_backend() -> bool {
    std::env::var("INBOX_E2E_BACKEND").is_ok_and(|v| v == "postgres")
}

impl PgAdmin {
    /// Runs SQL on a fresh thread: the callers are asynchronous tests, where a runtime cannot be started.
    fn exec(&self, database: &str, sql: String) {
        let (host, port, user, password, database) = (
            self.host.clone(),
            self.port,
            self.user.clone(),
            self.password.clone(),
            database.to_string(),
        );
        std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
            rt.block_on(async {
                let mut config = tokio_postgres::Config::new();
                config.host(&host).port(port).user(&user).password(&password).dbname(&database);
                let (client, connection) = config.connect(tokio_postgres::NoTls).await.unwrap();
                tokio::spawn(connection);
                client.batch_execute(&sql).await.unwrap();
            });
        })
        .join()
        .unwrap();
    }

    fn create_database(&self, name: &str) {
        self.exec("postgres", format!("CREATE DATABASE {name}"));
    }

    fn drop_database(&self, name: &str) {
        self.exec("postgres", format!("DROP DATABASE IF EXISTS {name} WITH (FORCE)"));
    }
}

struct Server {
    dir: PathBuf,
    /// Name of this instance: its configuration and log files.
    name: String,
    port: u16,
    admin_port: u16,
    child: Option<Child>,
    token: String,
    /// The database to drop with the server that created it.
    pg: Option<(PgAdmin, String)>,
}

impl Server {
    fn setup() -> Server {
        match pg_admin().filter(|_| postgres_backend()) {
            Some(admin) => Server::setup_with(Some(admin)),
            None => Server::setup_with(None),
        }
    }

    fn setup_with(pg: Option<PgAdmin>) -> Server {
        let dir =
            std::env::temp_dir().join(format!("inbox-e2e-{}-{}", std::process::id(), free_port()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = |n: &str| dir.join(n).to_string_lossy().to_string();
        assert!(inbox(&["keygen", "--out", &p("server_key")], None).status.success());
        let key = p("server_key");
        let out = inbox(
            &["credential", "--server-key", &key, "--out", &p("device_cred"), "--password-stdin"],
            Some(&format!("{DEVICE_PW}\n")),
        );
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
        assert!(inbox(&["token", "--out", &p("token")], None).status.success());
        // Refuses to overwrite an existing secret.
        assert!(!inbox(&["token", "--out", &p("token")], None).status.success());
        let token = std::fs::read_to_string(dir.join("token")).unwrap().trim().to_string();

        let port = free_port();
        let admin_port = free_port();
        let database = format!("inbox_e2e_{}", free_port());
        let (storage_path, postgres_block) = match &pg {
            None => (format!("path = \"{}\"", p("inbox.db")), String::new()),
            Some(admin) => {
                admin.create_database(&database);
                std::fs::write(dir.join("db_password"), &admin.password).unwrap();
                (
                    String::new(),
                    format!(
                        r#"
postgres {{
  host = "{}"
  port = {}
  database = "{database}"
  user = "{}"
  password_file = "{}"
  pool_size = 6
  connect_timeout = "5s"
}}
"#,
                        admin.host,
                        admin.port,
                        admin.user,
                        p("db_password")
                    ),
                )
            }
        };
        let config = format!(
            r#"
server {{
  name = "E2E"
  listen = "127.0.0.1:{port}"
  admin_listen = "127.0.0.1:{admin_port}"
  trusted_proxies = []
  shutdown_grace = "2s"
  server_key_file = "{key}"
}}
storage {{
  {storage_path}
  delivery_ttl = "1h"
  alert_retention = "1h"
  connection_log_retention = "1h"
}}
devices {{
  credential_file = "{dc}"
  heartbeat_interval = "5s"
  session_timeout = "10s"
  resend_after = "1s"
  max_inflight = 8
  unreachable_after = "10s"
  offline_warning_after = "10s"
  max_devices = 10
}}
admin {{
  allowed_hosts = ["localhost:{admin_port}"]
}}
security {{
  max_auth_failures = 10
  auth_failure_window = "1m"
}}
limits {{
  max_request_bytes = 65536
}}
logging {{
  level = "info"
  format = "text"
}}
telemetry {{
  enabled = false
}}
group "infra" {{
  name = "Infra"
  description = "prod"
}}
group "other" {{
  name = "Other"
  description = ""
}}
producer "ci" {{
  token_file = "{tok}"
  groups = ["infra"]
  webhook_send_resolved = false
}}
{postgres_block}"#,
            dc = p("device_cred"),
            tok = p("token"),
        );
        std::fs::write(dir.join("config.hcl"), config).unwrap();
        let pg = pg.map(|admin| (admin, database));
        Server { dir, name: "server".into(), port, admin_port, child: None, token, pg }
    }

    /// Another instance of the same service: same database, same secrets, its own ports, configuration and log.
    fn sibling(&self, name: &str) -> Server {
        let (port, admin_port) = (free_port(), free_port());
        let text = std::fs::read_to_string(self.config())
            .unwrap()
            .replace(&self.port.to_string(), &port.to_string())
            .replace(&self.admin_port.to_string(), &admin_port.to_string());
        std::fs::write(self.dir.join(format!("{name}.hcl")), text).unwrap();
        Server {
            dir: self.dir.clone(),
            name: name.into(),
            port,
            admin_port,
            child: None,
            token: self.token.clone(),
            pg: None,
        }
    }

    fn config(&self) -> String {
        let file = if self.name == "server" {
            "config.hcl".to_string()
        } else {
            format!("{}.hcl", self.name)
        };
        self.dir.join(file).to_string_lossy().to_string()
    }

    fn start(&mut self) {
        let log = std::fs::File::create(self.dir.join(format!("{}.log", self.name))).unwrap();
        let child = Command::new(BIN)
            .args(["serve", "--config", &self.config()])
            .stdout(Stdio::null())
            .stderr(log)
            .spawn()
            .unwrap();
        self.child = Some(child);
        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            if inbox(&["healthcheck", "--config", &self.config()], None).status.success() {
                return;
            }
            assert!(Instant::now() < deadline, "server did not become ready: {}", self.log());
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    fn stop(&mut self) {
        if let Some(mut c) = self.child.take() {
            unsafe { libc_kill(c.id() as i32, 15) };
            let deadline = Instant::now() + Duration::from_secs(10);
            while c.try_wait().unwrap().is_none() {
                assert!(Instant::now() < deadline, "server did not stop on SIGTERM");
                std::thread::sleep(Duration::from_millis(50));
            }
        }
    }

    fn log(&self) -> String {
        std::fs::read_to_string(self.dir.join(format!("{}.log", self.name))).unwrap_or_default()
    }
}

unsafe extern "C" {
    #[link_name = "kill"]
    fn libc_kill(pid: i32, sig: i32) -> i32;
}

impl Drop for Server {
    fn drop(&mut self) {
        if let Some(mut c) = self.child.take() {
            let _ = c.kill();
            let _ = c.wait();
        }
        if let Some((admin, database)) = self.pg.take() {
            admin.drop_database(&database);
        }
        if self.name == "server" {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }
}

/// Minimal HTTP/1.1 client: returns (status, body).
fn http(
    port: u16,
    method: &str,
    path: &str,
    headers: &[(&str, String)],
    body: &str,
) -> (u16, String) {
    http_host(port, &format!("localhost:{port}"), method, path, headers, body)
}

fn http_host(
    port: u16,
    host: &str,
    method: &str,
    path: &str,
    headers: &[(&str, String)],
    body: &str,
) -> (u16, String) {
    let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
    let mut req = format!(
        "{method} {path} HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\nContent-Length: {}\r\n",
        body.len()
    );
    if !body.is_empty() && !headers.iter().any(|(k, _)| k.eq_ignore_ascii_case("content-type")) {
        req.push_str("Content-Type: application/json\r\n");
    }
    for (k, v) in headers {
        req.push_str(&format!("{k}: {v}\r\n"));
    }
    req.push_str("\r\n");
    req.push_str(body);
    s.write_all(req.as_bytes()).unwrap();
    let mut raw = Vec::new();
    s.read_to_end(&mut raw).unwrap();
    let text = String::from_utf8_lossy(&raw).to_string();
    let status = text.split_whitespace().nth(1).unwrap().parse().unwrap();
    let (head, body) = text.split_once("\r\n\r\n").unwrap();
    let body = if head.to_ascii_lowercase().contains("transfer-encoding: chunked") {
        dechunk(body)
    } else {
        body.to_string()
    };
    (status, body)
}

fn dechunk(mut body: &str) -> String {
    let mut out = String::new();
    while let Some((size, rest)) = body.split_once("\r\n") {
        let n = usize::from_str_radix(size.trim(), 16).unwrap_or(0);
        if n == 0 {
            break;
        }
        out.push_str(&rest[..n]);
        body = &rest[n + 2..];
    }
    out
}

fn post_alert(s: &Server, key: &str, body: Value) -> (u16, Value) {
    let (status, text) = http(
        s.port,
        "POST",
        "/api/v1/alerts",
        &[("Authorization", format!("Bearer {}", s.token)), ("Idempotency-Key", key.to_string())],
        &body.to_string(),
    );
    (status, serde_json::from_str(&text).unwrap_or(Value::Null))
}

async fn enroll(
    port: u16,
    password: &str,
    name: &str,
) -> Result<EnrolledDevice, inbox_core::Error> {
    enroll_as(port, password, name, "tester").await
}

async fn enroll_as(
    port: u16,
    password: &str,
    name: &str,
    user: &str,
) -> Result<EnrolledDevice, inbox_core::Error> {
    let (mut ws, _) =
        tokio_tungstenite::connect_async(format!("ws://127.0.0.1:{port}/v1/enroll")).await.unwrap();
    let (mut client, first) = EnrollClient::start(password.as_bytes(), name, user, Rng::os())?;
    ws.send(Message::Text(first.into())).await.unwrap();
    loop {
        let msg = ws.next().await.expect("enroll stream").unwrap();
        let Message::Text(text) = msg else { continue };
        match client.on_message(&text)? {
            ClientStep::Send(reply) => ws.send(Message::Text(reply.into())).await.unwrap(),
            ClientStep::Done(device) => return Ok(device),
        }
    }
}

struct Conn {
    ws: Ws,
    session: Session,
}

enum Recv {
    Msg(ServerMsg),
    Closed(Option<u16>),
    Timeout,
}

impl Conn {
    async fn open(port: u16, dev: &EnrolledDevice) -> Result<(Conn, ServerMsg), u16> {
        let (mut ws, _) =
            tokio_tungstenite::connect_async(format!("ws://127.0.0.1:{port}/v1/session"))
                .await
                .unwrap();
        let hello = serde_json::to_vec(&SessionHello {
            protocol: inbox_core::PROTOCOL_VERSION,
            device_id: dev.device_id.clone(),
            app_version: "e2e".into(),
        })
        .unwrap();
        let (init, m1) =
            Initiator::start(&dev.device_private_key, &dev.server_public_key, &hello).unwrap();
        ws.send(Message::Binary(m1.into())).await.unwrap();
        let m2 = match ws.next().await {
            Some(Ok(Message::Binary(b))) => b,
            Some(Ok(Message::Close(Some(f)))) => return Err(f.code.into()),
            other => panic!("unexpected handshake reply: {other:?}"),
        };
        let (session, _accept) = init.finish(&m2).unwrap();
        let mut conn = Conn { ws, session };
        conn.send(&ClientMsg::Ready).await;
        match conn.recv(Duration::from_secs(5)).await {
            Recv::Msg(m @ ServerMsg::Welcome { .. }) => Ok((conn, m)),
            Recv::Closed(code) => Err(code.unwrap_or(0)),
            _ => panic!("expected welcome"),
        }
    }

    async fn send(&mut self, msg: &ClientMsg) {
        let frame = self.session.encrypt(&serde_json::to_vec(msg).unwrap()).unwrap();
        self.ws.send(Message::Binary(frame.into())).await.unwrap();
    }

    async fn recv(&mut self, wait: Duration) -> Recv {
        loop {
            match tokio::time::timeout(wait, self.ws.next()).await {
                Err(_) => return Recv::Timeout,
                Ok(None) | Ok(Some(Err(_))) => return Recv::Closed(None),
                Ok(Some(Ok(Message::Close(f)))) => return Recv::Closed(f.map(|f| f.code.into())),
                Ok(Some(Ok(Message::Binary(b)))) => {
                    let plain = self.session.decrypt(&b).unwrap();
                    return Recv::Msg(serde_json::from_slice(&plain).unwrap());
                }
                Ok(Some(Ok(_))) => continue,
            }
        }
    }

    async fn alert(&mut self) -> AlertMsg {
        loop {
            match self.recv(Duration::from_secs(5)).await {
                Recv::Msg(ServerMsg::Alert(a)) => return a,
                Recv::Msg(ServerMsg::Pong { .. } | ServerMsg::Subscriptions { .. }) => continue,
                Recv::Msg(other) => panic!("expected alert, got {other:?}"),
                Recv::Closed(c) => panic!("closed while waiting for alert: {c:?}"),
                Recv::Timeout => panic!("no alert within 5s"),
            }
        }
    }

    async fn no_alert(&mut self, wait: Duration) {
        let deadline = Instant::now() + wait;
        while Instant::now() < deadline {
            match self.recv(deadline - Instant::now()).await {
                Recv::Msg(ServerMsg::Alert(a)) => panic!("unexpected alert {a:?}"),
                Recv::Closed(c) => panic!("closed: {c:?}"),
                _ => {}
            }
        }
    }

    async fn ack(&mut self, id: &str, stage: AckStage) {
        self.send(&ClientMsg::Ack { alert_id: id.into(), stage }).await;
    }

    async fn subscribe(&mut self, group: &str) {
        self.send(&ClientMsg::Subscribe { group: group.into() }).await;
        loop {
            if let Recv::Msg(ServerMsg::Subscriptions { subscriptions }) =
                self.recv(Duration::from_secs(5)).await
            {
                assert!(subscriptions.iter().any(|s| s == group));
                return;
            }
        }
    }

    async fn closed_code(&mut self) -> Option<u16> {
        loop {
            match self.recv(Duration::from_secs(5)).await {
                Recv::Closed(c) => return c,
                Recv::Timeout => panic!("not closed"),
                Recv::Msg(_) => continue,
            }
        }
    }
}

/// Admin API call as the front makes it: no credentials, anti-CSRF headers on POST.
fn admin(port: u16, method: &str, path: &str, body: &str) -> (u16, Value) {
    let headers: Vec<(&str, String)> = if method == "POST" {
        vec![("Content-Type", "application/json".into()), ("X-Inbox-Request", "1".into())]
    } else {
        Vec::new()
    };
    let (st, text) = http(port, method, path, &headers, body);
    (st, serde_json::from_str(&text).unwrap_or(Value::Null))
}

#[test]
fn config_validation_is_strict() {
    let s = Server::setup();
    let ok = inbox(&["check-config", "--config", &s.config()], None);
    assert!(ok.status.success(), "{}", String::from_utf8_lossy(&ok.stderr));
    let text = std::fs::read_to_string(s.config()).unwrap();
    let broken = s.dir.join("broken.hcl");
    std::fs::write(&broken, text.replace("  resend_after = \"1s\"\n", "")).unwrap();
    let out = inbox(&["check-config", "--config", broken.to_str().unwrap()], None);
    assert_eq!(out.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&out.stderr).contains("resend_after"));
    // Missing secret file.
    std::fs::write(&broken, text.replace("token\"", "nope\"")).unwrap();
    assert!(
        !inbox(&["check-config", "--config", broken.to_str().unwrap()], None).status.success()
    );
    // No implicit config path.
    assert_eq!(inbox(&["serve"], None).status.code(), Some(2));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn delivery_guarantees_end_to_end() {
    let mut s = Server::setup();
    s.start();
    let port = s.port;

    // Wrong password is rejected; the right one enrolls.
    // OPAQUE detects a wrong password on the client side.
    assert_eq!(
        enroll(port, "not-the-password", "intruder").await.err(),
        Some(inbox_core::Error::AuthenticationFailed)
    );
    let a = enroll_as(port, DEVICE_PW, "phone-a", "alice").await.unwrap();
    let b = enroll_as(port, DEVICE_PW, "phone-b", "bob-old").await.unwrap();
    assert_eq!(a.server_name, "E2E");

    let (mut ca, welcome) = Conn::open(port, &a).await.unwrap();
    let ServerMsg::Welcome { groups, subscriptions, heartbeat_interval_ms, .. } = welcome else {
        unreachable!()
    };
    assert_eq!(groups.len(), 2);
    assert!(subscriptions.is_empty());
    assert_eq!(heartbeat_interval_ms, 5000);
    ca.subscribe("infra").await;
    {
        let (mut cb, _) = Conn::open(port, &b).await.unwrap();
        cb.subscribe("infra").await;
        // The pseudonym can be changed per server at any time.
        cb.send(&ClientMsg::SetUserName { user_name: " bob ".into() }).await;
        loop {
            if let Recv::Msg(ServerMsg::UserName { user_name }) =
                cb.recv(Duration::from_secs(5)).await
            {
                assert_eq!(user_name, "bob");
                break;
            }
        }
    } // b goes offline

    // Producer API checks.
    let (st, _) = http(
        port,
        "POST",
        "/api/v1/alerts",
        &[("Idempotency-Key", "x".into())],
        r#"{"group":"infra"}"#,
    );
    assert_eq!(st, 401);
    assert_eq!(post_alert(&s, "f", json!({"group": "other", "title": "t"})).0, 403);
    assert_eq!(post_alert(&s, "u", json!({"group": "infra", "title": "t", "bogus": 1})).0, 400);
    assert_eq!(post_alert(&s, "n", json!({"group": "infra"})).0, 400, "the title is required");
    assert_eq!(post_alert(&s, "b", json!({"group": "infra", "title": "  "})).0, 422);

    // Accepted once, delivered once, deduplicated on retry.
    let (st, v) = post_alert(&s, "k1", json!({"group": "infra", "title": "Disk full"}));
    assert_eq!(st, 201, "{v}");
    assert_eq!(v["targets"], 2);
    let k1 = v["alert_id"].as_str().unwrap().to_string();
    let got = ca.alert().await;
    assert_eq!(got.alert_id, k1);
    assert_eq!((got.title.as_str(), got.group.as_str()), ("Disk full", "infra"));
    ca.ack(&k1, AckStage::Received).await;
    let (st, v) = post_alert(&s, "k1", json!({"group": "infra", "title": "Disk full"}));
    assert_eq!((st, v["duplicate"].as_bool()), (200, Some(true)));
    assert_eq!(v["alert_id"], k1.as_str());
    ca.no_alert(Duration::from_millis(2500)).await;
    assert_eq!(post_alert(&s, "k1", json!({"group": "infra", "title": "changed"})).0, 409);

    // No receipt: retransmitted with the same id until the device confirms.
    let (_, v) = post_alert(&s, "k2", json!({"group": "infra", "title": "Second"}));
    let k2 = v["alert_id"].as_str().unwrap().to_string();
    assert_eq!(ca.alert().await.alert_id, k2);
    let again = ca.alert().await;
    assert_eq!(again.alert_id, k2, "retransmission keeps the alert id");
    assert_eq!(again.title, "Second");
    ca.ack(&k2, AckStage::Received).await;
    ca.no_alert(Duration::from_millis(2500)).await;

    // Offline device receives its whole backlog on reconnect, in order.
    let (mut cb, _) = Conn::open(port, &b).await.unwrap();
    let first = cb.alert().await;
    let second = cb.alert().await;
    assert_eq!((first.alert_id.as_str(), second.alert_id.as_str()), (k1.as_str(), k2.as_str()));
    cb.ack(&k1, AckStage::Received).await;
    cb.ack(&k2, AckStage::Received).await;
    cb.no_alert(Duration::from_millis(2500)).await;

    // A second session of the same device replaces the first.
    let (mut cb2, _) = Conn::open(port, &b).await.unwrap();
    assert_eq!(cb.closed_code().await, Some(inbox_core::proto::close::REPLACED));
    drop(cb2.ws.close(None).await);

    // Pending deliveries survive a server restart.
    drop(ca);
    let (_, v) = post_alert(&s, "k3", json!({"group": "infra", "title": "while you were away"}));
    let k3 = v["alert_id"].as_str().unwrap().to_string();
    s.stop();
    s.start();
    let (mut ca, _) = Conn::open(port, &a).await.unwrap();
    let got = ca.alert().await;
    assert_eq!(got.alert_id, k3);
    ca.ack(&k3, AckStage::Received).await;

    // Delivery status for producers.
    let (st, body) = http(
        port,
        "GET",
        &format!("/api/v1/alerts/{k1}"),
        &[("Authorization", format!("Bearer {}", s.token))],
        "",
    );
    assert_eq!(st, 200);
    let detail: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(detail["targets"], 2);
    assert_eq!(detail["received"], 2);
    assert!(detail.get("acknowledged").is_none(), "alerts are one-way");
    assert_eq!(detail["missing"], json!([]), "everyone received k1");
    let users: Vec<&str> = detail["deliveries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|d| d["user_name"].as_str().unwrap())
        .collect();
    assert_eq!(users, ["alice", "bob"]);

    // Alertmanager / Grafana webhook: resolved skipped, repeats deduplicated.
    let payload = json!({
        "status": "firing",
        "alerts": [
            {"status": "firing", "labels": {"alertname": "HighLatency"}, "annotations": {"summary": "p99 > 2s"},
             "startsAt": "2026-10-01T10:00:00Z", "fingerprint": "abc123"},
            {"status": "resolved", "labels": {"alertname": "Old"}, "annotations": {},
             "startsAt": "2026-10-01T09:00:00Z", "fingerprint": "def456"}
        ]
    })
    .to_string();
    let auth = [("Authorization", format!("Bearer {}", s.token))];
    let (st, body) = http(port, "POST", "/api/v1/webhooks/alertmanager/infra", &auth, &payload);
    assert_eq!(st, 200, "{body}");
    let v: Value = serde_json::from_str(&body).unwrap();
    assert_eq!((v["created"].as_u64(), v["skipped"].as_u64()), (Some(1), Some(1)));
    let got = ca.alert().await;
    assert_eq!(got.title, "[FIRING] HighLatency: p99 > 2s");
    ca.ack(&got.alert_id, AckStage::Received).await;
    let (_, body) = http(port, "POST", "/api/v1/webhooks/alertmanager/infra", &auth, &payload);
    assert_eq!(serde_json::from_str::<Value>(&body).unwrap()["duplicates"], 1);
    ca.no_alert(Duration::from_millis(2000)).await;

    // Admin: no password; host allow-list and CSRF protections; revocation.
    let ap = s.admin_port;
    let (st, v) = admin(ap, "GET", "/api/admin/overview", "");
    assert_eq!(st, 200, "{v}");
    assert_eq!(v["stats"]["enrolled"], 2);
    assert_eq!(v["stats"]["online"], 1);
    let devices = v["devices"].as_array().unwrap();
    let a_row = devices.iter().find(|d| d["device"]["name"] == "phone-a").unwrap();
    assert_eq!(a_row["status"], "online");
    assert_eq!(a_row["live"]["ip"], "127.0.0.1");
    assert_eq!(a_row["device"]["user_name"], "alice");
    let b_row = devices.iter().find(|d| d["device"]["name"] == "phone-b").unwrap();
    assert_eq!(b_row["device"]["user_name"], "bob");
    let (st, body) = http_host(ap, "evil.example:8081", "GET", "/api/admin/overview", &[], "");
    assert_eq!(st, 403, "DNS rebinding: unknown Host refused");
    assert!(body.contains("host_not_allowed"));
    let (st, body) = http_host(ap, "evil.example:8081", "GET", "/", &[], "");
    assert_eq!(st, 403);
    assert!(body.contains("admin.allowed_hosts"), "readable page for humans: {body}");
    let send = json!({"group": "other", "title": "manual", "idempotency_key": "m1"}).to_string();
    let json_only = [("Content-Type", "application/json".to_string())];
    let (st, _) = http(ap, "POST", "/api/admin/alerts", &json_only, &send);
    assert_eq!(st, 403, "POST without X-Inbox-Request");
    let text_plain =
        [("X-Inbox-Request", "1".to_string()), ("Content-Type", "text/plain".to_string())];
    let (st, _) = http(ap, "POST", "/api/admin/alerts", &text_plain, &send);
    assert_eq!(st, 403, "POST without JSON content type");
    let cross = [
        ("Content-Type", "application/json".to_string()),
        ("X-Inbox-Request", "1".to_string()),
        ("Sec-Fetch-Site", "cross-site".to_string()),
    ];
    let (st, _) = http(ap, "POST", "/api/admin/alerts", &cross, &send);
    assert_eq!(st, 403, "cross-site request");
    let (st, v) = admin(ap, "POST", "/api/admin/alerts", &send);
    assert_eq!(st, 201, "{v}");
    assert_eq!(v["targets"], 0);
    let (st, v) = admin(ap, "POST", &format!("/api/admin/devices/{}/revoke", a.device_id), "{}");
    assert_eq!((st, v["revoked"].as_bool()), (200, Some(true)));
    assert_eq!(ca.closed_code().await, Some(inbox_core::proto::close::REVOKED));
    assert_eq!(Conn::open(port, &a).await.err(), Some(inbox_core::proto::close::REVOKED));
    let (st, v) = admin(ap, "GET", "/api/admin/connections?limit=10", "");
    assert_eq!(st, 200);
    assert!(v["connections"].as_array().unwrap().len() >= 4);
    let (_, v) = admin(ap, "GET", "/api/admin/connections?user=alice", "");
    let mine = v["connections"].as_array().unwrap();
    assert!(!mine.is_empty() && mine.iter().all(|c| c["user_name"] == "alice"), "{v}");
    let (_, v) = admin(ap, "GET", "/api/admin/connections?user=bob", "");
    assert!(v["connections"].as_array().unwrap().iter().all(|c| c["user_name"] == "bob"));
    assert!(!v["connections"].as_array().unwrap().is_empty(), "bob's later sessions are tracked");
    let (_, v) = admin(ap, "GET", "/api/admin/alerts", "");
    let all = v["alerts"].as_array().unwrap().clone();
    assert!(all.len() >= 4);
    let (_, v) = admin(ap, "GET", "/api/admin/alerts?limit=2", "");
    assert_eq!(v["alerts"].as_array().unwrap().len(), 2);
    assert_eq!(v["alerts"][0]["id"], all[0]["id"], "newest first");
    let newest = all[0]["created_at"].as_i64().unwrap();
    let (_, v) = admin(ap, "GET", &format!("/api/admin/alerts?since_ms={}", newest + 1), "");
    assert!(v["alerts"].as_array().unwrap().is_empty(), "since_ms filters older alerts");
    let (_, v) = admin(ap, "GET", &format!("/api/admin/alerts?since_ms={newest}"), "");
    assert!(!v["alerts"].as_array().unwrap().is_empty());
    let (st, _) = admin(ap, "POST", "/api/admin/login/start", "{}");
    assert_eq!(st, 405, "no login endpoint any more");

    // Assets are served with security headers.
    let (st, _) = http(s.admin_port, "GET", "/", &[], "");
    assert_eq!(st, 200);

    s.stop();
    assert!(!s.log().contains("panicked"), "{}", s.log());
    // SQLite is checkpointed and closed; with PostgreSQL the instance forgets itself and closes its sessions.
    let closed = if s.pg.is_some() { "event=\"stopped\"" } else { "database_closed" };
    assert!(s.log().contains(closed), "graceful shutdown: {}", s.log());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn password_guessing_is_rate_limited() {
    let mut s = Server::setup();
    s.start();
    // max_auth_failures = 10: each started OPAQUE attempt counts until it succeeds.
    for _ in 0..10 {
        assert_eq!(
            enroll(s.port, "guess-guess", "x").await.err(),
            Some(inbox_core::Error::AuthenticationFailed)
        );
    }
    match enroll(s.port, DEVICE_PW, "x").await {
        Err(inbox_core::Error::Rejected { code, .. }) => assert_eq!(code, "rate_limited"),
        other => panic!("expected rate_limited, got {:?}", other.err()),
    }
    s.stop();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn integrations_and_reverse_proxy_prefixes() {
    let mut s = Server::setup();
    // Even when loopback is declared a trusted proxy, loopback peers keep the Host
    // check (SSH tunnels); the exemption itself is unit-tested in admin.rs.
    let cfg = std::fs::read_to_string(s.config()).unwrap();
    std::fs::write(
        s.config(),
        cfg.replace("trusted_proxies = []", "trusted_proxies = [\"127.0.0.1/32\"]"),
    )
    .unwrap();
    s.start();
    let (port, ap) = (s.port, s.admin_port);
    let dev = enroll_as(port, DEVICE_PW, "phone", "ops").await.unwrap();
    let (mut c, _) = Conn::open(port, &dev).await.unwrap();
    c.subscribe("infra").await;

    // Generic notify: plain text body, token in the query string, no Idempotency-Key.
    let url = format!("/api/v1/notify/infra?token={}&title=Sauvegarde", s.token);
    let (st, body) =
        http(port, "POST", &url, &[("Content-Type", "text/plain".into())], "backup KO\n");
    assert_eq!(st, 201, "{body}");
    let got = c.alert().await;
    assert_eq!(got.title, "Sauvegarde");
    c.ack(&got.alert_id, AckStage::Received).await;
    // The same request retried within the minute is deduplicated.
    let (st, body) =
        http(port, "POST", &url, &[("Content-Type", "text/plain".into())], "backup KO\n");
    assert_eq!(st, 200, "{body}");
    assert!(body.contains("\"duplicate\":true"));

    // Without a title anywhere, the plain text body is the title, on one line.
    let auth = ("Authorization", format!("Bearer {}", s.token));
    let (st, _) = http(
        port,
        "POST",
        "/api/v1/notify/infra",
        std::slice::from_ref(&auth),
        "backup\n  failed\n",
    );
    assert_eq!(st, 201);
    let got = c.alert().await;
    assert_eq!(got.title, "backup failed");
    c.ack(&got.alert_id, AckStage::Received).await;
    // An empty request has no title: refused.
    let (st, _) = http(port, "POST", "/api/v1/notify/infra", std::slice::from_ref(&auth), "");
    assert_eq!(st, 422);

    // JSON with a title, bearer header.
    let (st, _) = http(
        port,
        "POST",
        "/api/v1/notify/infra",
        std::slice::from_ref(&auth),
        r#"{"title":"T","other":"ignored"}"#,
    );
    assert_eq!(st, 201);
    let got = c.alert().await;
    assert_eq!(got.title, "T");
    c.ack(&got.alert_id, AckStage::Received).await;
    // Any other JSON is the title as compact text; X-Title overrides it.
    let (st, _) = http(
        port,
        "POST",
        "/api/v1/notify/infra",
        &[auth.clone(), ("X-Title", "Uptime".into())],
        r#"{ "monitor": {"name": "web"}, "status": 0 }"#,
    );
    assert_eq!(st, 201);
    let got = c.alert().await;
    assert_eq!(got.title, "Uptime");
    c.ack(&got.alert_id, AckStage::Received).await;
    let (st, _) = http(
        port,
        "POST",
        "/api/v1/notify/infra",
        std::slice::from_ref(&auth),
        r#"{ "monitor": {"name": "web"}, "status": 0 }"#,
    );
    assert_eq!(st, 201);
    let got = c.alert().await;
    assert_eq!(got.title, r#"{"monitor":{"name":"web"},"status":0}"#);
    c.ack(&got.alert_id, AckStage::Received).await;
    // Wrong token in the query string is refused; forbidden group too.
    let (st, _) = http(port, "POST", "/api/v1/notify/infra?token=nope", &[], "x");
    assert_eq!(st, 401);
    let (st, _) = http(port, "POST", &format!("/api/v1/notify/other?token={}", s.token), &[], "x");
    assert_eq!(st, 403);

    // Titles are at most 128 characters: refused by the JSON API,
    // truncated by the generic webhook.
    let long = "x".repeat(129);
    let body = json!({"group": "infra", "title": long}).to_string();
    let (st, _) = http(port, "POST", "/api/v1/alerts", std::slice::from_ref(&auth), &body);
    assert_eq!(st, 413);
    let (st, _) = http(
        port,
        "POST",
        "/api/v1/notify/infra",
        std::slice::from_ref(&auth),
        &json!({"title": long}).to_string(),
    );
    assert_eq!(st, 201);
    let got = c.alert().await;
    assert_eq!(got.title.chars().count(), 128);
    c.ack(&got.alert_id, AckStage::Received).await;

    // Only the current field names are accepted.
    let (st, _) = http(
        port,
        "POST",
        "/api/v1/alerts",
        std::slice::from_ref(&auth),
        r#"{"group":"infra","description":"old name"}"#,
    );
    assert_eq!(st, 400);

    // JSON API without Idempotency-Key: created, then deduplicated.
    let (st, _) = http(
        port,
        "POST",
        "/api/v1/alerts",
        std::slice::from_ref(&auth),
        r#"{"group":"infra","title":"nokey"}"#,
    );
    assert_eq!(st, 201);
    let (st, _) = http(
        port,
        "POST",
        "/api/v1/alerts",
        std::slice::from_ref(&auth),
        r#"{"group":"infra","title":"nokey"}"#,
    );
    assert_eq!(st, 200);

    let (st, _) = http_host(ap, "evil.example", "GET", "/api/admin/overview", &[], "");
    assert_eq!(st, 403, "loopback is never exempt from the Host check");

    // Admin behind reverse proxies: any path prefix.
    let host = format!("localhost:{ap}");
    for prefix in ["", "/inbox", "/a/b"] {
        let (st, body) =
            http_host(ap, &host, "GET", &format!("{prefix}/api/admin/overview"), &[], "");
        assert_eq!(st, 200, "{prefix}: {body}");
        let (st, body) = http_host(ap, &host, "GET", &format!("{prefix}/"), &[], "");
        assert_eq!(st, 200);
        assert!(
            body.contains("<html") || body.contains("<!doctype") || body.contains("<!DOCTYPE"),
            "{prefix}"
        );
        let (st, _) = http_host(ap, &host, "GET", &format!("{prefix}/app.js"), &[], "");
        assert_eq!(st, 200, "{prefix}/app.js");
        let (st, body) = http_host(ap, &host, "GET", &format!("{prefix}/doc"), &[], "");
        assert_eq!(st, 200);
        assert!(body.contains("doc"), "{prefix}/doc");
    }
    let send = json!({"group": "infra", "title": "via proxy", "idempotency_key": "p1"}).to_string();
    let hdrs =
        [("Content-Type", "application/json".to_string()), ("X-Inbox-Request", "1".to_string())];
    let (st, body) = http_host(ap, &host, "POST", "/inbox/api/admin/alerts", &hdrs, &send);
    assert_eq!(st, 201, "{body}");
    s.stop();
}

/// The node running Inbox is lost (SIGKILL, local disk gone): a second instance with an empty local disk takes
/// over from the shared directory and knows every device and alert that was acknowledged.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_second_instance_takes_over_a_lost_node() {
    let mut s = Server::setup_with(None);
    let shared = s.dir.join("shared");
    let first_db = s.dir.join("inbox.db");
    let text = std::fs::read_to_string(s.config()).unwrap();
    let replica = format!(
        r#"
replica {{
  dir               = "{}"
  snapshot_interval = "1s"
  lease_ttl         = "5s"
  sync_timeout      = "5s"
  sync_alerts       = true
}}
"#,
        shared.display()
    );
    std::fs::write(s.config(), format!("{text}{replica}")).unwrap();
    s.start();
    let port = s.port;

    // Everything below is acknowledged, hence already in the shared directory.
    let phone = enroll_as(port, DEVICE_PW, "phone-a", "alice").await.unwrap();
    let (st, accepted) =
        post_alert(&s, "failover-1", json!({"group": "infra", "title": "before the loss"}));
    assert_eq!(st, 201, "{accepted}");
    let alert_id = accepted["alert_id"].as_str().unwrap().to_string();
    assert!(shared.join("lease.json").exists() && shared.join("snapshot-1.db").exists());

    // The node is lost: no signal handler runs, and its disk is gone with it.
    let mut child = s.child.take().unwrap();
    child.kill().unwrap();
    child.wait().unwrap();
    for suffix in ["", "-wal", "-shm"] {
        let _ = std::fs::remove_file(format!("{}{suffix}", first_db.display()));
    }

    // The standby has an empty local disk and waits for the dead owner's lease to expire.
    let second_db = s.dir.join("standby.db");
    let config = std::fs::read_to_string(s.config()).unwrap();
    std::fs::write(
        s.config(),
        config.replace(&first_db.display().to_string(), &second_db.display().to_string()),
    )
    .unwrap();
    let began = Instant::now();
    s.start();
    assert!(
        began.elapsed() >= Duration::from_secs(3),
        "took over before the lease expired: {:?}",
        began.elapsed()
    );
    assert!(s.log().contains("replica_ready") || s.log().contains("restored_epoch"), "{}", s.log());

    let (st, overview) = admin(s.admin_port, "GET", "/api/admin/overview", "");
    assert_eq!(st, 200, "{overview}");
    assert_eq!(overview["stats"]["enrolled"], 1, "the enrolled phone survived: {overview}");
    let (_, alerts) = admin(s.admin_port, "GET", "/api/admin/alerts", "");
    assert!(alerts.to_string().contains(&alert_id), "the acknowledged alert survived: {alerts}");
    // The producer retries the alert it never got an answer for: deduplicated, not created twice.
    let (st, again) =
        post_alert(&s, "failover-1", json!({"group": "infra", "title": "before the loss"}));
    assert_eq!(st, 200, "{again}");
    assert_eq!(again["alert_id"], alert_id.as_str());
    // The phone still enrolled on the first instance opens a session on the second one.
    let (_conn, welcome) = Conn::open(port, &phone).await.unwrap();
    assert!(matches!(welcome, ServerMsg::Welcome { .. }));
    assert!(shared.join("snapshot-2.db").exists(), "the new epoch publishes its own snapshot");

    // A clean stop hands the lease over at once: no waiting for the ttl.
    s.stop();
    let began = Instant::now();
    s.start();
    assert!(
        began.elapsed() < Duration::from_secs(4),
        "a released lease was not taken at once: {:?}",
        began.elapsed()
    );
    let (_, overview) = admin(s.admin_port, "GET", "/api/admin/overview", "");
    assert_eq!(overview["stats"]["enrolled"], 1);
}

/// Without a `replica` block nothing changes: no shared directory is needed or touched.
#[test]
fn check_config_reports_the_replica_block() {
    let s = Server::setup();
    let out = inbox(&["check-config", "--config", &s.config()], None);
    assert!(String::from_utf8_lossy(&out.stdout).contains("replica off"));
    let text = std::fs::read_to_string(s.config()).unwrap();
    let broken = s.dir.join("replica.hcl");
    std::fs::write(
        &broken,
        format!(
            "{text}\nreplica {{\n dir = \"/x\"\n snapshot_interval = \"1s\"\n lease_ttl = \"1s\"\n sync_timeout = \"1s\"\n sync_alerts = true\n}}\n"
        ),
    )
    .unwrap();
    let out = inbox(&["check-config", "--config", broken.to_str().unwrap()], None);
    assert_eq!(out.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&out.stderr).contains("lease_ttl"));
}

/// Two active instances on one PostgreSQL database: they share phones, alerts, receipts and sessions. Needs
/// `INBOX_TEST_PG=host:port:user:password` (a role that can create databases); skipped without it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn two_active_instances_share_phones_alerts_and_sessions() {
    use inbox_core::proto::close;
    let Some(admin_pg) = pg_admin() else {
        eprintln!("INBOX_TEST_PG is not set: skipped");
        return;
    };
    let mut a = Server::setup_with(Some(admin_pg));
    // Polling alone would only notice a new alert every 5 s: a fast delivery across instances proves the notifications.
    let text = std::fs::read_to_string(a.config())
        .unwrap()
        .replace("resend_after = \"1s\"", "resend_after = \"30s\"")
        .replace("session_timeout = \"10s\"", "session_timeout = \"120s\"")
        .replace("unreachable_after = \"10s\"", "unreachable_after = \"120s\"");
    std::fs::write(a.config(), text).unwrap();
    let mut b = a.sibling("instance-b");
    a.start();
    b.start();

    // A phone enrolled through A opens its session on B.
    let phone = enroll_as(a.port, DEVICE_PW, "phone-a", "alice").await.unwrap();
    let (mut on_b, _) = Conn::open(b.port, &phone).await.unwrap();
    on_b.subscribe("infra").await;

    // An alert accepted by A reaches the session held by B at once.
    let began = Instant::now();
    let body = json!({"group": "infra", "title": "from a"});
    let (st, first) = post_alert(&a, "shared-1", body.clone());
    assert_eq!(st, 201, "{first}");
    let alert = on_b.alert().await;
    assert_eq!(alert.title, "from a");
    assert!(
        began.elapsed() < Duration::from_secs(3),
        "woken by polling, not by the notification: {:?}",
        began.elapsed()
    );
    on_b.ack(&alert.alert_id, AckStage::Received).await;

    // The idempotency key holds across instances.
    let (st, again) = post_alert(&b, "shared-1", body);
    assert_eq!(st, 200, "{again}");
    assert_eq!(again["alert_id"], first["alert_id"]);

    // Both admin fronts see the phone online, although its session lives on B, and the receipt B recorded.
    for s in [&a, &b] {
        let (st, overview) = admin(s.admin_port, "GET", "/api/admin/overview", "");
        assert_eq!(st, 200, "{overview}");
        assert_eq!(overview["stats"]["online"], 1, "{overview}");
        assert_eq!(overview["stats"]["enrolled"], 1);
    }
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let (_, alerts) = admin(a.admin_port, "GET", "/api/admin/alerts", "");
        if alerts["alerts"][0]["received"] == 1 {
            break;
        }
        assert!(Instant::now() < deadline, "the receipt recorded on B never showed on A: {alerts}");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    // A device has one session: opening another one through A closes the one held by B.
    let (mut on_a, _) = Conn::open(a.port, &phone).await.unwrap();
    assert_eq!(on_b.closed_code().await, Some(close::REPLACED));

    // Revoking through B closes the session held by A.
    let (st, _) = admin(
        b.admin_port,
        "POST",
        &format!("/api/admin/devices/{}/revoke", phone.device_id),
        "{}",
    );
    assert_eq!(st, 200);
    assert_eq!(on_a.closed_code().await, Some(close::REVOKED));
    assert_eq!(Conn::open(b.port, &phone).await.err(), Some(close::REVOKED));

    // The same alert sent at the same time to both instances is stored once, whichever wins.
    let results: Vec<(u16, Value)> = std::thread::scope(|scope| {
        let tasks: Vec<_> = (0..8)
            .map(|i| {
                let target = if i % 2 == 0 { &a } else { &b };
                scope.spawn(move || {
                    post_alert(target, "race-1", json!({"group": "infra", "title": "same alert"}))
                })
            })
            .collect();
        tasks.into_iter().map(|t| t.join().unwrap()).collect()
    });
    assert_eq!(results.iter().filter(|(st, _)| *st == 201).count(), 1, "{results:?}");
    assert!(results.iter().all(|(st, _)| *st == 201 || *st == 200), "{results:?}");
    let ids: std::collections::HashSet<_> =
        results.iter().map(|(_, v)| v["alert_id"].to_string()).collect();
    assert_eq!(ids.len(), 1, "one alert id for one idempotency key: {results:?}");

    // The node of A is lost: B keeps accepting everything.
    let mut lost = a.child.take().unwrap();
    lost.kill().unwrap();
    lost.wait().unwrap();
    let other = enroll_as(b.port, DEVICE_PW, "phone-b", "bob").await.unwrap();
    let (mut other_conn, _) = Conn::open(b.port, &other).await.unwrap();
    other_conn.subscribe("infra").await;
    let (st, second) =
        post_alert(&b, "shared-2", json!({"group": "infra", "title": "after the loss"}));
    assert_eq!(st, 201, "{second}");
    assert_eq!(other_conn.alert().await.title, "after the loss");
}

/// The sessions of an instance that disappeared are closed by the survivors once its heartbeat stops.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_sessions_of_a_lost_instance_are_closed_by_the_others() {
    let Some(admin_pg) = pg_admin() else {
        eprintln!("INBOX_TEST_PG is not set: skipped");
        return;
    };
    let mut a = Server::setup_with(Some(admin_pg));
    let text = std::fs::read_to_string(a.config())
        .unwrap()
        .replace("session_timeout = \"10s\"", "session_timeout = \"120s\"")
        .replace("unreachable_after = \"10s\"", "unreachable_after = \"120s\"");
    std::fs::write(a.config(), text).unwrap();
    let mut b = a.sibling("instance-b");
    a.start();
    b.start();
    let phone = enroll_as(a.port, DEVICE_PW, "phone-a", "alice").await.unwrap();
    let (_conn, _) = Conn::open(a.port, &phone).await.unwrap();
    let (_, overview) = admin(b.admin_port, "GET", "/api/admin/overview", "");
    assert_eq!(overview["stats"]["online"], 1, "{overview}");

    let mut lost = a.child.take().unwrap();
    lost.kill().unwrap();
    lost.wait().unwrap();
    // Nobody tells B: the heartbeat stops, then the janitor closes the sessions (30 s of silence).
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        let (_, overview) = admin(b.admin_port, "GET", "/api/admin/overview", "");
        if overview["stats"]["online"] == 0 {
            break;
        }
        assert!(Instant::now() < deadline, "the dead instance still holds a session: {overview}");
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    // The sessions stop counting as live after 30 s, the janitor of B then writes their end within one heartbeat.
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        let (_, connections) = admin(b.admin_port, "GET", "/api/admin/connections", "");
        if connections.to_string().contains("instance_lost") {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the janitor never closed the connection: {connections}"
        );
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
}

/// An existing SQLite installation moves to PostgreSQL without enrolling the phones again and without losing the alerts
/// that are still waiting for a phone.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_sqlite_installation_moves_to_postgres() {
    let Some(admin_pg) = pg_admin() else {
        eprintln!("INBOX_TEST_PG is not set: skipped");
        return;
    };
    let mut s = Server::setup_with(None);
    s.start();
    let phone = enroll_as(s.port, DEVICE_PW, "phone-a", "alice").await.unwrap();
    {
        let (mut conn, _) = Conn::open(s.port, &phone).await.unwrap();
        conn.subscribe("infra").await;
        let (st, _) =
            post_alert(&s, "mig-1", json!({"group": "infra", "title": "received before"}));
        assert_eq!(st, 201);
        let alert = conn.alert().await;
        conn.ack(&alert.alert_id, AckStage::Received).await;
        tokio::time::sleep(Duration::from_millis(300)).await;
    } // the phone goes offline
    let (st, _) =
        post_alert(&s, "mig-2", json!({"group": "infra", "title": "waiting for the phone"}));
    assert_eq!(st, 201);
    s.stop();

    // The same configuration, on PostgreSQL.
    let database = format!("inbox_e2e_{}", free_port());
    admin_pg.create_database(&database);
    std::fs::write(s.dir.join("db_password"), &admin_pg.password).unwrap();
    let sqlite_file = s.dir.join("inbox.db");
    let text = std::fs::read_to_string(s.config())
        .unwrap()
        .replace(&format!("path = \"{}\"", sqlite_file.display()), "");
    let pg_config = s.dir.join("postgres.hcl");
    std::fs::write(
        &pg_config,
        format!(
            "{text}\npostgres {{\n host = \"{}\"\n port = {}\n database = \"{database}\"\n user = \"{}\"\n password_file = \"{}\"\n pool_size = 4\n connect_timeout = \"5s\"\n}}\n",
            admin_pg.host,
            admin_pg.port,
            admin_pg.user,
            s.dir.join("db_password").display()
        ),
    )
    .unwrap();
    let (cfg, from) =
        (pg_config.to_str().unwrap().to_string(), sqlite_file.to_str().unwrap().to_string());
    let out = inbox(&["import-sqlite", "--config", &cfg, "--from", &from], None);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let report = String::from_utf8_lossy(&out.stdout).to_string();
    assert!(report.contains("1 devices") && report.contains("2 alerts"), "{report}");
    // Importing twice would duplicate everything: refused.
    let again = inbox(&["import-sqlite", "--config", &cfg, "--from", &from], None);
    assert!(!again.status.success());
    assert!(String::from_utf8_lossy(&again.stderr).contains("refusing to import"));

    let mut moved = Server {
        dir: s.dir.clone(),
        name: "postgres".into(),
        port: s.port,
        admin_port: s.admin_port,
        child: None,
        token: s.token.clone(),
        pg: Some((admin_pg, database)),
    };
    moved.start();
    let (_, overview) = admin(moved.admin_port, "GET", "/api/admin/overview", "");
    assert_eq!(overview["stats"]["enrolled"], 1, "{overview}");
    assert_eq!(overview["devices"][0]["device"]["subscriptions"][0], "infra");
    let (_, alerts) = admin(moved.admin_port, "GET", "/api/admin/alerts", "");
    assert_eq!(alerts["alerts"].as_array().unwrap().len(), 2, "{alerts}");
    // The phone did not enroll again: the same keys open a session, and the alert it never received arrives.
    let (mut conn, _) = Conn::open(moved.port, &phone).await.unwrap();
    let waiting = conn.alert().await;
    assert_eq!(waiting.title, "waiting for the phone");
    // New alerts keep working after the imported sequences.
    let (st, _) = post_alert(&moved, "mig-3", json!({"group": "infra", "title": "after the move"}));
    assert_eq!(st, 201);
    assert_eq!(conn.alert().await.title, "after the move");
}

/// A pod starts before its database is reachable (network not ready, database failing over): Inbox waits and retries
/// instead of exiting.
#[test]
fn postgres_may_come_up_after_inbox() {
    let Some(admin_pg) = pg_admin() else {
        eprintln!("INBOX_TEST_PG is not set: skipped");
        return;
    };
    let mut s = Server::setup_with(Some(admin_pg.clone()));
    let database = s.pg.as_ref().map(|(_, d)| d.clone()).unwrap();
    admin_pg.drop_database(&database);
    let late = std::thread::spawn({
        let (admin_pg, database) = (admin_pg.clone(), database.clone());
        move || {
            std::thread::sleep(Duration::from_secs(3));
            admin_pg.create_database(&database);
        }
    });
    s.start();
    late.join().unwrap();
    assert!(s.log().contains("postgres_unreachable"), "{}", s.log());
    assert!(s.log().contains("postgres_ready"), "{}", s.log());
}
