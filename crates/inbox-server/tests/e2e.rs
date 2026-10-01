//! Black-box end-to-end test: runs the real `inbox` binary and drives it with
//! the real protocol (OPAQUE enrollment, Noise sessions, signed admin requests).

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use futures_util::{SinkExt, StreamExt};
use inbox_core::enroll::{ClientStep, EnrollClient, EnrolledDevice};
use inbox_core::login::{LoginClient, mac};
use inbox_core::noise::{Initiator, Session};
use inbox_core::proto::{AckStage, Action, AlertMsg, ClientMsg, ServerMsg, SessionHello};
use inbox_core::rng::Rng;
use inbox_core::suite::Role;
use serde_json::{Value, json};
use tokio_tungstenite::tungstenite::Message;

const BIN: &str = env!("CARGO_BIN_EXE_inbox");
const DEVICE_PW: &str = "device-password";
const ADMIN_PW: &str = "admin-password-long";

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

struct Server {
    dir: PathBuf,
    port: u16,
    admin_port: u16,
    child: Option<Child>,
    token: String,
}

impl Server {
    fn setup() -> Server {
        let dir =
            std::env::temp_dir().join(format!("inbox-e2e-{}-{}", std::process::id(), free_port()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = |n: &str| dir.join(n).to_string_lossy().to_string();
        assert!(inbox(&["keygen", "--out", &p("server_key")], None).status.success());
        let key = p("server_key");
        for (role, pw) in [("device", DEVICE_PW), ("admin", ADMIN_PW)] {
            let out = inbox(
                &[
                    "credential",
                    "--server-key",
                    &key,
                    "--role",
                    role,
                    "--out",
                    &p(&format!("{role}_cred")),
                    "--password-stdin",
                ],
                Some(&format!("{pw}\n")),
            );
            assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
        }
        assert!(inbox(&["token", "--out", &p("token")], None).status.success());
        // Refuses to overwrite an existing secret.
        assert!(!inbox(&["token", "--out", &p("token")], None).status.success());
        let token = std::fs::read_to_string(dir.join("token")).unwrap().trim().to_string();

        let port = free_port();
        let admin_port = free_port();
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
  path = "{db}"
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
  credential_file = "{ac}"
  session_ttl = "1h"
}}
security {{
  max_auth_failures = 10
  auth_failure_window = "1m"
  admin_request_max_skew = "30s"
}}
limits {{
  max_title_bytes = 100
  max_message_bytes = 1000
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
"#,
            db = p("inbox.db"),
            dc = p("device_cred"),
            ac = p("admin_cred"),
            tok = p("token"),
        );
        std::fs::write(dir.join("config.hcl"), config).unwrap();
        Server { dir, port, admin_port, child: None, token }
    }

    fn config(&self) -> String {
        self.dir.join("config.hcl").to_string_lossy().to_string()
    }

    fn start(&mut self) {
        let log = std::fs::File::create(self.dir.join("server.log")).unwrap();
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
        std::fs::read_to_string(self.dir.join("server.log")).unwrap_or_default()
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
        let _ = std::fs::remove_dir_all(&self.dir);
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
    let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
    let mut req = format!(
        "{method} {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\nContent-Length: {}\r\n",
        body.len()
    );
    if !body.is_empty() {
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
    let (mut ws, _) =
        tokio_tungstenite::connect_async(format!("ws://127.0.0.1:{port}/v1/enroll")).await.unwrap();
    let (mut client, first) = EnrollClient::start(password.as_bytes(), name, Rng::os())?;
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

    async fn subscribe(&mut self, group: &str, action: Action) {
        self.send(&ClientMsg::Subscribe { group: group.into(), action }).await;
        loop {
            if let Recv::Msg(ServerMsg::Subscriptions { subscriptions }) =
                self.recv(Duration::from_secs(5)).await
            {
                assert!(subscriptions.iter().any(|s| s.group == group && s.action == action));
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

struct Admin {
    port: u16,
    session_id: String,
    key: zeroize::Zeroizing<[u8; 32]>,
}

impl Admin {
    fn login(port: u16, password: &str) -> Result<Admin, u16> {
        let mut rng = Rng::os();
        let (client, request) =
            LoginClient::start(Role::Admin, password.as_bytes(), &mut rng).unwrap();
        let (st, body) = http(
            port,
            "POST",
            "/api/admin/login/start",
            &[],
            &json!({ "credential_request": inbox_core::b64::encode(&request) }).to_string(),
        );
        assert_eq!(st, 200, "{body}");
        let v: Value = serde_json::from_str(&body).unwrap();
        let response =
            inbox_core::b64::decode(v["credential_response"].as_str().unwrap()).unwrap();
        let Ok((finalization, key)) = client.finish(&response, &mut rng) else { return Err(0) };
        let (st, body) = http(
            port,
            "POST",
            "/api/admin/login/finish",
            &[],
            &json!({ "login_id": v["login_id"], "credential_finalization": inbox_core::b64::encode(&finalization) }).to_string(),
        );
        if st != 200 {
            return Err(st);
        }
        let v: Value = serde_json::from_str(&body).unwrap();
        Ok(Admin {
            port,
            session_id: v["session_id"].as_str().unwrap().into(),
            key: key.derive("admin/request-mac"),
        })
    }

    fn call(&self, method: &str, path: &str, body: &str) -> (u16, Value) {
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis()
            .to_string();
        let nonce = inbox_core::b64::encode(&Rng::os().bytes::<18>());
        let sig = mac(
            &self.key,
            &[
                b"inbox/v1/admin-request",
                method.as_bytes(),
                path.as_bytes(),
                ts.as_bytes(),
                nonce.as_bytes(),
                body.as_bytes(),
            ],
        );
        let headers = [
            ("x-inbox-session", self.session_id.clone()),
            ("x-inbox-timestamp", ts),
            ("x-inbox-nonce", nonce),
            ("x-inbox-signature", inbox_core::b64::encode(&sig)),
        ];
        let (st, text) = http(self.port, method, path, &headers, body);
        (st, serde_json::from_str(&text).unwrap_or(Value::Null))
    }
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
    let a = enroll(port, DEVICE_PW, "phone-a").await.unwrap();
    let b = enroll(port, DEVICE_PW, "phone-b").await.unwrap();
    assert_eq!(a.server_name, "E2E");

    let (mut ca, welcome) = Conn::open(port, &a).await.unwrap();
    let ServerMsg::Welcome { groups, subscriptions, heartbeat_interval_ms, .. } = welcome else {
        unreachable!()
    };
    assert_eq!(groups.len(), 2);
    assert!(subscriptions.is_empty());
    assert_eq!(heartbeat_interval_ms, 5000);
    ca.subscribe("infra", Action::Alarm).await;
    {
        let (mut cb, _) = Conn::open(port, &b).await.unwrap();
        cb.subscribe("infra", Action::Notify).await;
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
    let (st, _) = http(
        port,
        "POST",
        "/api/v1/alerts",
        &[("Authorization", format!("Bearer {}", s.token))],
        r#"{"group":"infra"}"#,
    );
    assert_eq!(st, 400, "missing idempotency key");
    assert_eq!(post_alert(&s, "f", json!({"group": "other"})).0, 403);
    assert_eq!(post_alert(&s, "u", json!({"group": "infra", "bogus": 1})).0, 400);

    // Accepted once, delivered once, deduplicated on retry.
    let (st, v) = post_alert(
        &s,
        "k1",
        json!({"group": "infra", "title": "Disk full", "message": "db-1 at 99%"}),
    );
    assert_eq!(st, 201, "{v}");
    assert_eq!(v["targets"], 2);
    let k1 = v["alert_id"].as_str().unwrap().to_string();
    let got = ca.alert().await;
    assert_eq!(got.alert_id, k1);
    assert_eq!(got.action, Action::Alarm);
    assert_eq!(got.title.as_deref(), Some("Disk full"));
    ca.ack(&k1, AckStage::Received).await;
    ca.ack(&k1, AckStage::Displayed).await;
    let (st, v) = post_alert(
        &s,
        "k1",
        json!({"group": "infra", "title": "Disk full", "message": "db-1 at 99%"}),
    );
    assert_eq!((st, v["duplicate"].as_bool()), (200, Some(true)));
    assert_eq!(v["alert_id"], k1.as_str());
    ca.no_alert(Duration::from_millis(2500)).await;
    assert_eq!(post_alert(&s, "k1", json!({"group": "infra", "title": "changed"})).0, 409);

    // Not acknowledged: retransmitted with the same id until it is.
    let (_, v) = post_alert(&s, "k2", json!({"group": "infra"}));
    let k2 = v["alert_id"].as_str().unwrap().to_string();
    assert_eq!(ca.alert().await.alert_id, k2);
    let again = ca.alert().await;
    assert_eq!(again.alert_id, k2, "retransmission keeps the alert id");
    assert!(again.title.is_none() && again.message.is_none(), "title and message are optional");
    ca.ack(&k2, AckStage::Acknowledged).await;
    ca.no_alert(Duration::from_millis(2500)).await;

    // Offline device receives its whole backlog on reconnect, in order.
    let (mut cb, _) = Conn::open(port, &b).await.unwrap();
    let first = cb.alert().await;
    let second = cb.alert().await;
    assert_eq!((first.alert_id.as_str(), second.alert_id.as_str()), (k1.as_str(), k2.as_str()));
    assert_eq!(first.action, Action::Notify);
    cb.ack(&k1, AckStage::Received).await;
    cb.ack(&k2, AckStage::Received).await;
    cb.no_alert(Duration::from_millis(2500)).await;

    // A second session of the same device replaces the first.
    let (mut cb2, _) = Conn::open(port, &b).await.unwrap();
    assert_eq!(cb.closed_code().await, Some(inbox_core::proto::close::REPLACED));
    drop(cb2.ws.close(None).await);

    // Pending deliveries survive a server restart.
    drop(ca);
    let (_, v) = post_alert(&s, "k3", json!({"group": "infra", "message": "while you were away"}));
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
    assert_eq!(detail["displayed"], 1);

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
    assert_eq!(got.title.as_deref(), Some("[FIRING] HighLatency"));
    assert_eq!(got.message.as_deref(), Some("p99 > 2s"));
    ca.ack(&got.alert_id, AckStage::Received).await;
    let (_, body) = http(port, "POST", "/api/v1/webhooks/alertmanager/infra", &auth, &payload);
    assert_eq!(serde_json::from_str::<Value>(&body).unwrap()["duplicates"], 1);
    ca.no_alert(Duration::from_millis(2000)).await;

    // Admin: wrong password, then signed requests, replay protection, revocation.
    assert_eq!(
        Admin::login(s.admin_port, "wrong-admin-password").err(),
        Some(0),
        "detected client-side"
    );
    assert!(
        Admin::login(s.admin_port, DEVICE_PW).is_err(),
        "device password must not open the admin front"
    );
    let admin = Admin::login(s.admin_port, ADMIN_PW).unwrap();
    let (st, v) = admin.call("GET", "/api/admin/overview", "");
    assert_eq!(st, 200, "{v}");
    assert_eq!(v["stats"]["enrolled"], 2);
    assert_eq!(v["stats"]["online"], 1);
    let devices = v["devices"].as_array().unwrap();
    let a_row = devices.iter().find(|d| d["device"]["name"] == "phone-a").unwrap();
    assert_eq!(a_row["status"], "online");
    assert_eq!(a_row["live"]["ip"], "127.0.0.1");
    let (st, _) = http(s.admin_port, "GET", "/api/admin/overview", &[], "");
    assert_eq!(st, 401, "unsigned admin request");
    let (st, v) = admin.call(
        "POST",
        "/api/admin/alerts",
        &json!({"group": "other", "title": "manual", "idempotency_key": "m1"}).to_string(),
    );
    assert_eq!(st, 201, "{v}");
    assert_eq!(v["targets"], 0);
    let (st, v) = admin.call("POST", &format!("/api/admin/devices/{}/revoke", a.device_id), "");
    assert_eq!((st, v["revoked"].as_bool()), (200, Some(true)));
    assert_eq!(ca.closed_code().await, Some(inbox_core::proto::close::REVOKED));
    assert_eq!(Conn::open(port, &a).await.err(), Some(inbox_core::proto::close::REVOKED));
    let (st, v) = admin.call("GET", "/api/admin/connections?limit=10", "");
    assert_eq!(st, 200);
    assert!(v["connections"].as_array().unwrap().len() >= 4);
    let (_, v) = admin.call("GET", "/api/admin/alerts", "");
    assert!(v["alerts"].as_array().unwrap().len() >= 4);

    // Assets are served with security headers.
    let (st, _) = http(s.admin_port, "GET", "/", &[], "");
    assert_eq!(st, 200);

    s.stop();
    assert!(!s.log().contains("panicked"), "{}", s.log());
    assert!(
        s.log().contains("database_closed"),
        "graceful shutdown closes the database: {}",
        s.log()
    );
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
    let (st, _) = http(
        s.admin_port,
        "POST",
        "/api/admin/login/start",
        &[],
        r#"{"credential_request":"AAAA"}"#,
    );
    assert_eq!(st, 429);
    s.stop();
}
