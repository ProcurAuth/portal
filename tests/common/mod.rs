//! Helpers shared by the end-to-end tests of the inbox: the test server, the sessions of the
//! accounts, the HTTP calls, the forms and the phone side of protocol 4.
#![allow(dead_code)]

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use futures_util::{SinkExt, StreamExt};
use inbox_core::enroll::{EnrollCode, EnrollInitiator};
use inbox_core::noise::{Initiator, Session};
use inbox_core::proto::{AckStage, AlertMsg, ClientMsg, ServerMsg, SessionHello};
use serde_json::{json, Value};
use tokio_tungstenite::tungstenite::Message;

pub const BIN: &str = env!("CARGO_BIN_EXE_portal");

pub type Ws =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

/// A free TCP port that this process has not given before. The kernel gives a random port from
/// about 14 000 ports and takes it back at once. Parallel tests would sometimes get the same
/// port: two servers on one port, or a restarted server that finds its port taken.
pub fn free_port() -> u16 {
    static GIVEN: std::sync::Mutex<Vec<u16>> = std::sync::Mutex::new(Vec::new());
    loop {
        let port = TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        let mut given = GIVEN.lock().unwrap();
        if !given.contains(&port) {
            given.push(port);
            return port;
        }
    }
}

/// A name that no other test and no other run has: the tests of several runs share one
/// PostgreSQL server, and a database of another run must never be created or dropped here.
pub fn unique_name(prefix: &str) -> String {
    static COUNT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    format!(
        "{prefix}_{}_{}_{nanos:x}",
        std::process::id(),
        COUNT.fetch_add(1, std::sync::atomic::Ordering::SeqCst)
    )
}

/// Runs `portal` and makes sure that it succeeds. The failure shows its message.
pub fn portal_ok(args: &[&str]) -> std::process::Output {
    let out = portal(args, None);
    assert!(
        out.status.success(),
        "portal {args:?} failed with {}: {}",
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );
    out
}

pub fn portal(args: &[&str], stdin: Option<&str>) -> std::process::Output {
    let mut child = Command::new(BIN)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    if let Some(input) = stdin {
        child
            .stdin
            .take()
            .unwrap()
            .write_all(input.as_bytes())
            .unwrap();
    }
    child.wait_with_output().unwrap()
}

/// An administrative PostgreSQL connection, from `INBOX_TEST_PG=host:port:user:password`. The tests that need it are
/// skipped when it is absent; `INBOX_E2E_BACKEND=postgres` runs the whole suite against it instead of SQLite.
#[derive(Clone)]
pub struct PgAdmin {
    pub host: String,
    pub port: u16,
    pub user: String,
    pub password: String,
}

pub fn pg_admin() -> Option<PgAdmin> {
    let raw = std::env::var("INBOX_TEST_PG").ok()?;
    let mut parts = raw.splitn(4, ':');
    Some(PgAdmin {
        host: parts.next()?.to_string(),
        port: parts.next()?.parse().ok()?,
        user: parts.next()?.to_string(),
        password: parts.next()?.to_string(),
    })
}

pub fn postgres_backend() -> bool {
    std::env::var("INBOX_E2E_BACKEND").is_ok_and(|v| v == "postgres")
}

impl PgAdmin {
    /// Runs SQL on a fresh thread: the callers are asynchronous tests, where a runtime cannot be started.
    pub fn exec(&self, database: &str, sql: String) {
        let (host, port, user, password, database) = (
            self.host.clone(),
            self.port,
            self.user.clone(),
            self.password.clone(),
            database.to_string(),
        );
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
                client.batch_execute(&sql).await.unwrap();
            });
        })
        .join()
        .unwrap();
    }

    /// The alerts of a database with the number of phones that confirmed each one, newest first.
    /// The tests read the database directly: during an outage of PostgreSQL the portal itself
    /// waits for its own connections, and the inbox is what these tests check.
    pub fn alerts(&self, database: &str) -> Vec<Value> {
        let (host, port, user, password, database) = (
            self.host.clone(),
            self.port,
            self.user.clone(),
            self.password.clone(),
            database.to_string(),
        );
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
                let rows = client
                    .query(
                        "SELECT a.id, (SELECT COUNT(*) FROM deliveries d WHERE d.alert_id = a.id AND d.received_at IS NOT NULL)::bigint
                         FROM alerts a ORDER BY a.created_at DESC, a.id",
                        &[],
                    )
                    .await
                    .unwrap();
                rows.iter()
                    .map(|r| json!({ "id": r.get::<_, String>(0), "received": r.get::<_, i64>(1) }))
                    .collect()
            })
        })
        .join()
        .unwrap()
    }

    pub fn create_database(&self, name: &str) {
        self.exec("postgres", format!("CREATE DATABASE {name}"));
    }

    pub fn drop_database(&self, name: &str) {
        self.exec(
            "postgres",
            format!("DROP DATABASE IF EXISTS {name} WITH (FORCE)"),
        );
    }
}

/// The login and the password of the superadmin of the test servers.
pub const SUPERADMIN: &str = "root";
pub const SUPERADMIN_PASSWORD: &str = "a password for the end to end tests";

/// The session of an account: the cookie. The token of the forms comes from a page of the inbox.
pub struct AdminSession {
    pub cookie: String,
}

pub struct Server {
    pub dir: PathBuf,
    /// Name of this instance: its configuration and log files.
    pub name: String,
    /// The listener of the portal: the pages, the producers and the phones use this one port.
    pub port: u16,
    pub child: Option<Child>,
    pub token: String,
    /// The database to drop with the server that created it.
    pub pg: Option<(PgAdmin, String)>,
    /// The session of the superadmin. A start of the server clears it.
    pub session: std::sync::Mutex<Option<std::sync::Arc<AdminSession>>>,
    /// The sessions of the other accounts, by login. A start of the server clears them.
    pub accounts: std::sync::Mutex<std::collections::HashMap<String, std::sync::Arc<AdminSession>>>,
}

impl Server {
    pub fn setup() -> Server {
        match pg_admin().filter(|_| postgres_backend()) {
            Some(admin) => Server::setup_with(Some(admin)),
            None => Server::setup_with(None),
        }
    }

    pub fn setup_with(pg: Option<PgAdmin>) -> Server {
        let dir = std::env::temp_dir().join(unique_name("portal-inbox-e2e"));
        // `create_dir` fails when the directory exists: two tests never share one.
        std::fs::create_dir(&dir).unwrap();
        let p = |n: &str| dir.join(n).to_string_lossy().to_string();
        portal_ok(&["inbox", "keygen", "--out", &p("server_key")]);
        let key = p("server_key");
        portal_ok(&["inbox", "token", "--out", &p("token")]);
        // Refuses to overwrite an existing secret.
        assert!(!portal(&["inbox", "token", "--out", &p("token")], None)
            .status
            .success());
        let token = std::fs::read_to_string(dir.join("token"))
            .unwrap()
            .trim()
            .to_string();
        std::fs::write(dir.join("superadmin_login"), SUPERADMIN).unwrap();
        std::fs::write(dir.join("superadmin_password"), SUPERADMIN_PASSWORD).unwrap();
        std::fs::write(
            dir.join("session_secret"),
            "an end to end signing secret of 40 bytes!!",
        )
        .unwrap();

        let port = free_port();
        let database = unique_name("inbox_e2e");
        let (storage_block, postgres_block) = match &pg {
            None => (
                format!("storage {{ database = \"{}\" }}", p("portal.db")),
                String::new(),
            ),
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
        let pool = if pg.is_some() {
            "  pool_size = 6\n"
        } else {
            ""
        };
        let config = format!(
            r#"
server {{
  listen_address = "127.0.0.1:{port}"
  name = "E2E"
  trusted_proxies = []
}}
authentication {{
  superadmin_login_file = "{login}"
  superadmin_password_file = "{password}"
  // The tests of the inbox do not use the second factor.
  mfa = "off"
}}
session {{
  secret_mode = "provided"
  secret_file = "{secret}"
}}
{storage_block}
logging {{
  level = "info"
}}
# access-begin
access {{
  grants_mode = "hybrid"
}}
keys {{
  mcp = false
  api = true
}}
user "ci" {{
  grants = [
    "keys.own.use on service:portal/key_kind:api",
    "alert.send on service:portal/inbox_group:infra",
    "alert.read on service:portal/inbox_group:infra",
  ]
}}
# access-end
inbox {{
  server_key_file = "{key}"
  devices {{
    public_address = "127.0.0.1:{port}"
    heartbeat_interval = "5s"
    session_timeout = "10s"
    resend_after = "1s"
    max_inflight = 8
    unreachable_after = "10s"
    offline_warning_after = "10s"
    max_devices = 10
    max_auth_failures = 10
    auth_failure_window = "1m"
    max_request_bytes = "64KiB"
  }}
  storage {{
    delivery_ttl = "1h"
    alert_retention = "1h"
    connection_log_retention = "1h"
{pool}  }}
  # inbox-extra
  group "infra" {{
    name = "Infra"
    description = "prod"
  }}
  group "other" {{
    name = "Other"
    description = ""
  }}
  api_key "ci" {{
    owner = "ci"
    secret_file = "{tok}"
    groups = ["infra"]
    webhook_send_resolved = false
  }}
}}
{postgres_block}"#,
            login = p("superadmin_login"),
            password = p("superadmin_password"),
            secret = p("session_secret"),
            tok = p("token"),
        );
        std::fs::write(dir.join("config.hcl"), config).unwrap();
        let pg = pg.map(|admin| (admin, database));
        Server {
            dir,
            name: "server".into(),
            port,
            child: None,
            token,
            pg,
            session: Default::default(),
            accounts: Default::default(),
        }
    }

    /// Adds text inside the block `inbox` of the configuration: the blocks `inbox.replica` and
    /// `inbox.cluster`.
    pub fn add_to_inbox(&self, extra: &str) {
        let text = std::fs::read_to_string(self.config()).unwrap();
        assert!(text.contains("  # inbox-extra\n"));
        std::fs::write(
            self.config(),
            text.replace("  # inbox-extra\n", &format!("{extra}\n  # inbox-extra\n")),
        )
        .unwrap();
    }

    /// Adds a block at the top of the configuration, after the block `inbox`.
    pub fn add_block(&self, block: &str) {
        let text = std::fs::read_to_string(self.config()).unwrap();
        std::fs::write(self.config(), format!("{text}\n{block}\n")).unwrap();
    }

    /// Another instance of the same service: same database, same secrets, its own ports, configuration and log.
    pub fn sibling(&self, name: &str) -> Server {
        let port = free_port();
        let text = std::fs::read_to_string(self.config())
            .unwrap()
            .replace(&self.port.to_string(), &port.to_string());
        std::fs::write(self.dir.join(format!("{name}.hcl")), text).unwrap();
        Server {
            dir: self.dir.clone(),
            name: name.into(),
            port,
            child: None,
            token: self.token.clone(),
            pg: None,
            session: Default::default(),
            accounts: Default::default(),
        }
    }

    pub fn config(&self) -> String {
        let file = if self.name == "server" {
            "config.hcl".to_string()
        } else {
            format!("{}.hcl", self.name)
        };
        self.dir.join(file).to_string_lossy().to_string()
    }

    pub fn start(&mut self) {
        self.spawn();
        self.wait_ready();
    }

    /// Starts the process. It does not wait: see [`Server::wait_ready`].
    pub fn spawn(&mut self) {
        *self.session.lock().unwrap() = None;
        self.accounts.lock().unwrap().clear();
        let log = std::fs::File::create(self.dir.join(format!("{}.log", self.name))).unwrap();
        let child = Command::new(BIN)
            .args(["run", "--config", &self.config()])
            .stdout(Stdio::null())
            .stderr(log)
            .spawn()
            .unwrap();
        self.child = Some(child);
    }

    /// Waits until the server is ready. Ready means two things. The inbox answers `/readyz` as
    /// soon as its listener is open. The pages answer `503 starting` until the first catalogue
    /// is complete: `/healthz` is `200` only after that. A test that makes its first request
    /// before that gets a 503.
    pub fn wait_ready(&mut self) {
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            // A server that cannot bind its port exits at once. Another server on this port
            // would answer in its place.
            if let Some(status) = self.child.as_mut().unwrap().try_wait().unwrap() {
                panic!("server exited with {status} at start: {}", self.log());
            }
            if portal(&["healthcheck", "--config", &self.config()], None)
                .status
                .success()
                && http(self.port, "GET", "/healthz", &[], "").0 == 200
            {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "server did not become ready: {}",
                self.log()
            );
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    pub fn stop(&mut self) {
        if let Some(mut c) = self.child.take() {
            let pid = c.id().to_string();
            Command::new("sh")
                .args(["-c", &format!("kill -TERM {pid}")])
                .status()
                .unwrap();
            let deadline = Instant::now() + Duration::from_secs(30);
            while c.try_wait().unwrap().is_none() {
                assert!(Instant::now() < deadline, "server did not stop on SIGTERM");
                std::thread::sleep(Duration::from_millis(50));
            }
        }
    }

    pub fn log(&self) -> String {
        std::fs::read_to_string(self.dir.join(format!("{}.log", self.name))).unwrap_or_default()
    }

    /// The session of the superadmin. It signs in on the first call.
    pub fn session(&self) -> std::sync::Arc<AdminSession> {
        let mut slot = self.session.lock().unwrap();
        if let Some(session) = slot.as_ref() {
            return session.clone();
        }
        let session = std::sync::Arc::new(sign_in(self.port));
        *slot = Some(session.clone());
        session
    }

    /// The session of an account: the superadmin, or a database account that the superadmin makes
    /// on the first call.
    pub fn account(&self, login: &str) -> std::sync::Arc<AdminSession> {
        self.account_with(login, &DEFAULT_GRANTS)
    }

    /// The session of an account of the configuration file (see `add_file_account`). The server
    /// is started. No database account is made.
    pub fn file_account(&self, login: &str) -> std::sync::Arc<AdminSession> {
        let mut accounts = self.accounts.lock().unwrap();
        accounts
            .entry(login.to_string())
            .or_insert_with(|| std::sync::Arc::new(sign_in_as(self.port, login, ACCOUNT_PASSWORD)))
            .clone()
    }

    /// The same, with the permissions that the test chooses for a new account.
    pub fn account_with(&self, login: &str, grants: &[&str]) -> std::sync::Arc<AdminSession> {
        if login == SUPERADMIN {
            return self.session();
        }
        if let Some(session) = self.accounts.lock().unwrap().get(login) {
            return session.clone();
        }
        add_account_with(self, login, grants);
        let session = std::sync::Arc::new(sign_in_as(self.port, login, ACCOUNT_PASSWORD));
        self.accounts
            .lock()
            .unwrap()
            .insert(login.to_string(), session.clone());
        session
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        // A failed test shows what its server logged: the log goes away with the directory.
        if std::thread::panicking() {
            let log = self.log();
            let lines: Vec<&str> = log.lines().collect();
            let tail = &lines[lines.len().saturating_sub(40)..];
            eprintln!(
                "--- log of the server {:?} (last lines) ---\n{}",
                self.name,
                tail.join("\n")
            );
        }
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

/// The password of the accounts that the tests make.
pub const ACCOUNT_PASSWORD: &str = "e2e-account-password-0123";

pub fn sign_in(port: u16) -> AdminSession {
    sign_in_as(port, SUPERADMIN, SUPERADMIN_PASSWORD)
}

/// One sign-in on the listener of the portal, with a fresh sign-in page and its tokens. The
/// answer is the status, the head and the body.
pub fn sign_in_request(port: u16, login: &str, password: &str) -> (u16, String, String) {
    let host = format!("127.0.0.1:{port}");
    let (st, head, page) = http_full(port, &host, "GET", "/auth/login", &[], "");
    assert_eq!(st, 200, "{page}");
    let csrf_cookie = head
        .lines()
        .filter(|l| l.to_ascii_lowercase().starts_with("set-cookie:"))
        .find_map(|l| {
            l.split_once(':')
                .map(|(_, v)| v.trim().split(';').next().unwrap().to_string())
        })
        .expect("the sign-in page sets the cookie of its token");
    // The page changes the order of the attributes at each render: read the tag, not a fixed text.
    let field = |name: &str| {
        let at = page
            .find(&format!("name=\"{name}\""))
            .expect("a field of the form");
        let tag = &page[page[..at].rfind('<').unwrap()..at + page[at..].find('>').unwrap()];
        let start = tag.find("value=\"").expect("a value") + "value=\"".len();
        tag[start..].split('"').next().unwrap().to_string()
    };
    // The form has a minimum time between its display and its submit.
    std::thread::sleep(std::time::Duration::from_millis(1_200));
    let form = format!(
        "login={login}&password={}&csrf={}&rd={}&sig={}&token={}&website=",
        password.replace(' ', "+"),
        field("csrf"),
        field("rd"),
        field("sig"),
        field("token")
    );
    http_full(
        port,
        &host,
        "POST",
        "/auth/login",
        &[
            ("Content-Type", "application/x-www-form-urlencoded".into()),
            ("Origin", format!("http://{host}")),
            ("Cookie", csrf_cookie),
        ],
        &form,
    )
}

/// The key of an alert of the portal holds the hour of the Unix clock (the bucket of `repeat_after`). Two instances
/// that detect one event on both sides of the end of the hour make two alerts. A test that counts the alerts of two
/// instances waits for the next hour when fewer than `margin` seconds are left in this one.
pub fn stay_in_an_hour(margin: u64) {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let left = 3600 - now % 3600;
    if left < margin {
        std::thread::sleep(Duration::from_secs(left + 1));
    }
}

/// `count` sign-ins with a wrong password. The portal refuses each of them, or blocks it with a
/// limiter. Make the sessions that the test needs before: the limiter locks the address out.
pub fn refuse_sign_ins(port: u16, login: &str, count: usize) {
    for _ in 0..count {
        let (st, _, body) = sign_in_request(port, login, "a wrong password for the test");
        assert!(st == 401 || st == 429, "{st}: {body}");
    }
}

/// Signs in on the listener of the portal.
pub fn sign_in_as(port: u16, login: &str, password: &str) -> AdminSession {
    let (st, head, body) = sign_in_request(port, login, password);
    assert_eq!(st, 303, "{body}");
    let cookie = head
        .lines()
        .filter(|l| l.to_ascii_lowercase().starts_with("set-cookie:"))
        .filter_map(|l| {
            l.split_once(':')
                .map(|(_, v)| v.trim().split(';').next().unwrap().to_string())
        })
        .find(|c| c.starts_with("portal="))
        .expect("the session cookie");
    AdminSession { cookie }
}

/// Minimal HTTP/1.1 client: returns (status, body).
pub fn http(
    port: u16,
    method: &str,
    path: &str,
    headers: &[(&str, String)],
    body: &str,
) -> (u16, String) {
    http_host(
        port,
        &format!("localhost:{port}"),
        method,
        path,
        headers,
        body,
    )
}

pub fn http_host(
    port: u16,
    host: &str,
    method: &str,
    path: &str,
    headers: &[(&str, String)],
    body: &str,
) -> (u16, String) {
    let (status, _, body) = http_full(port, host, method, path, headers, body);
    (status, body)
}

/// Like [`http_host`], with the head of the answer.
pub fn http_full(
    port: u16,
    host: &str,
    method: &str,
    path: &str,
    headers: &[(&str, String)],
    body: &str,
) -> (u16, String, String) {
    let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
    // The portal waits for a connection that the proxy swallowed: up to 20 s.
    s.set_read_timeout(Some(Duration::from_secs(45))).unwrap();
    let mut req = format!(
        "{method} {path} HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\nContent-Length: {}\r\n",
        body.len()
    );
    if !body.is_empty()
        && !headers
            .iter()
            .any(|(k, _)| k.eq_ignore_ascii_case("content-type"))
    {
        req.push_str("Content-Type: application/json\r\n");
    }
    for (k, v) in headers {
        req.push_str(&format!("{k}: {v}\r\n"));
    }
    req.push_str("\r\n");
    req.push_str(body);
    s.write_all(req.as_bytes()).unwrap();
    let mut raw = Vec::new();
    let began = Instant::now();
    if let Err(e) = s.read_to_end(&mut raw) {
        panic!("{method} {path} on {host}: {e} after {:?}", began.elapsed());
    }
    let text = String::from_utf8_lossy(&raw).to_string();
    let status = text
        .split_whitespace()
        .nth(1)
        .and_then(|status| status.parse().ok())
        .unwrap_or_else(|| {
            panic!(
                "{method} {path} on {host} (port {port}) got no HTTP answer after {:?}: {text:?}",
                began.elapsed()
            )
        });
    let (head, body) = text.split_once("\r\n\r\n").unwrap();
    let body = if head
        .to_ascii_lowercase()
        .contains("transfer-encoding: chunked")
    {
        dechunk(body)
    } else {
        body.to_string()
    };
    (status, head.to_string(), body)
}

pub fn dechunk(mut body: &str) -> String {
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

pub fn post_alert(s: &Server, key: &str, body: Value) -> (u16, Value) {
    let (status, text) = http(
        s.port,
        "POST",
        "/api/v1/alerts",
        &[
            ("Authorization", format!("Bearer {}", s.token)),
            ("Idempotency-Key", key.to_string()),
        ],
        &body.to_string(),
    );
    (status, serde_json::from_str(&text).unwrap_or(Value::Null))
}

/// A phone that enrolled: what the app stores.
#[derive(Debug)]
pub struct Phone {
    pub device_id: String,
    pub device_private_key: Vec<u8>,
    pub server_public_key: Vec<u8>,
    pub server_name: String,
    pub user_name: String,
}

/// Why an enrollment did not end with message 2.
#[derive(Debug)]
pub enum EnrollFail {
    /// The server closed the WebSocket without message 2. The value is the close code.
    Closed(Option<u16>),
    /// The device side refused: the state machine of the core returned an error.
    #[allow(dead_code)] // the value shows in the failure message of a test
    Local(inbox_core::Error),
}

/// The users page gives the anti-CSRF token of the forms that manage the accounts.
pub fn users_csrf(s: &Server) -> String {
    let admin = s.session();
    let host = format!("127.0.0.1:{}", s.port);
    let (st, _, page) = http_full(
        s.port,
        &host,
        "GET",
        "/auth/settings/new-user",
        &[("Cookie", admin.cookie.clone())],
        "",
    );
    assert_eq!(st, 200, "{page}");
    // The token of the form that adds an account. The page has other forms with other tokens.
    let form = page
        .find("id=\"add-user\"")
        .expect("the page New user has the form");
    let marker = "name=\"csrf\" value=\"";
    let start = form + page[form..].find(marker).expect("the form has a token") + marker.len();
    page[start..].split('"').next().unwrap().to_string()
}

/// A form of the users pages, sent by the superadmin. The answer is a redirect with the outcome.
pub fn users_form(s: &Server, path: &str, fields: &str) -> String {
    let admin = s.session();
    let host = format!("127.0.0.1:{}", s.port);
    let body = format!("csrf={}&{fields}", users_csrf(s));
    let (st, head, text) = http_full(
        s.port,
        &host,
        "POST",
        path,
        &[
            ("Content-Type", "application/x-www-form-urlencoded".into()),
            ("Origin", format!("http://{host}")),
            ("Cookie", admin.cookie.clone()),
        ],
        &body,
    );
    assert_eq!(st, 303, "{text}");
    head.lines()
        .find_map(|l| {
            l.strip_prefix("location: ")
                .or(l.strip_prefix("Location: "))
        })
        .unwrap_or_default()
        .to_string()
}

/// The permissions that a test account has when the test does not choose: it may subscribe to the
/// two groups of the test servers.
pub const DEFAULT_GRANTS: [&str; 2] = [
    "inbox.receive on service:portal/inbox_group:infra",
    "inbox.receive on service:portal/inbox_group:other",
];

/// Encodes a value of a form.
pub fn form_value(text: &str) -> String {
    text.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                (b as char).to_string()
            }
            b' ' => "+".to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}

/// The superadmin makes a database account, with the default permissions of the tests. An account
/// that exists already is not an error.
pub fn add_account(s: &Server, login: &str) {
    add_account_with(s, login, &DEFAULT_GRANTS);
}

/// The same, with the permissions that the test chooses.
pub fn add_account_with(s: &Server, login: &str, grants: &[&str]) {
    users_form(
        s,
        "/auth/users/add",
        &format!("login={login}&password={ACCOUNT_PASSWORD}"),
    );
    set_grants(s, login, grants);
}

/// The superadmin sets the permissions of a database account. The form replaces all the
/// permissions of the account, those of the portal features included.
pub fn set_grants(s: &Server, login: &str, grants: &[&str]) {
    set_grants_with(s, login, grants, "");
}

/// The same, with more fields of the form: the limit values of the grants.
pub fn set_grants_with(s: &Server, login: &str, grants: &[&str], more: &str) {
    let mut fields = format!("login={login}&grants=1");
    for (n, grant) in grants.iter().enumerate() {
        fields.push_str(&format!("&grant{n}={}", form_value(grant)));
    }
    if !more.is_empty() {
        fields.push_str(&format!("&{more}"));
    }
    // An account that another instance made shows up here after a round of the directory.
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let location = users_form(s, "/auth/users/update", &fields);
        if location.contains("s=user-updated") || location.contains("s=unchanged") {
            return;
        }
        assert!(
            location.contains("s=no-user") && Instant::now() < deadline,
            "{location}"
        );
        std::thread::sleep(Duration::from_millis(500));
    }
}

/// The superadmin deletes a database account.
pub fn delete_account(s: &Server, login: &str) {
    let location = users_form(s, "/auth/users/delete", &format!("login={login}"));
    assert!(location.contains("s=user-deleted"), "{location}");
    s.accounts.lock().unwrap().remove(login);
}

/// The token of the forms of the inbox, from the page of the tab. The page has the form that sends
/// an alert, or the form of the plus button that makes a code, by the rights of the account.
pub fn inbox_csrf(port: u16, cookie: &str) -> String {
    let host = format!("127.0.0.1:{port}");
    for path in ["/auth/settings/inbox"] {
        let (st, _, page) = http_full(
            port,
            &host,
            "GET",
            path,
            &[("Cookie", cookie.to_string())],
            "",
        );
        if st != 200 {
            continue;
        }
        // The header of the page has the sign-out form with its own token: take the token of a form of the inbox.
        let Some(form) = page.find("action=\"/auth/settings/inbox/") else {
            continue;
        };
        let marker = "name=\"csrf\" value=\"";
        if let Some(start) = page[form..].find(marker) {
            let start = form + start + marker.len();
            return page[start..].split('"').next().unwrap().to_string();
        }
    }
    String::new()
}

/// The text of an HTML attribute value, without the escapes of the portal.
pub fn unescape(text: &str) -> String {
    text.replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&amp;", "&")
}

/// The value of the attribute `name` in the tag text.
pub fn attr(tag: &str, name: &str) -> Option<String> {
    let marker = format!(" {name}=\"");
    let start = tag.find(&marker)? + marker.len();
    Some(unescape(tag[start..].split('"').next()?))
}

/// The opening tags `<tr ...>` of a page that carry the attribute `key`.
pub fn rows(page: &str, key: &str) -> Vec<String> {
    let marker = format!(" {key}=\"");
    page.match_indices("<tr ")
        .filter_map(|(at, _)| {
            let end = at + page[at..].find('>')?;
            let tag = &page[at..end];
            tag.contains(&marker).then(|| tag.to_string())
        })
        .collect()
}

/// A page of the tab Inbox with the session of the superadmin.
pub fn inbox_page(s: &Server, path: &str) -> (u16, String) {
    inbox_page_as(s, &s.session().cookie, path)
}

pub fn inbox_page_as(s: &Server, cookie: &str, path: &str) -> (u16, String) {
    let host = format!("127.0.0.1:{}", s.port);
    let (st, _, page) = http_full(
        s.port,
        &host,
        "GET",
        path,
        &[("Cookie", cookie.to_string())],
        "",
    );
    (st, page)
}

/// A form of the inbox, sent with the session of an account. The answer is the status, the head and
/// the body.
pub fn inbox_form(s: &Server, cookie: &str, path: &str, fields: &str) -> (u16, String, String) {
    let host = format!("127.0.0.1:{}", s.port);
    let csrf = inbox_csrf(s.port, cookie);
    let body = if fields.is_empty() {
        format!("csrf={csrf}")
    } else {
        format!("csrf={csrf}&{fields}")
    };
    http_full(
        s.port,
        &host,
        "POST",
        path,
        &[
            ("Content-Type", "application/x-www-form-urlencoded".into()),
            ("Origin", format!("http://{host}")),
            ("Cookie", cookie.to_string()),
        ],
        &body,
    )
}

/// The `Location` header of an answer.
pub fn location(head: &str) -> String {
    head.lines()
        .find_map(|l| {
            l.strip_prefix("location: ")
                .or(l.strip_prefix("Location: "))
        })
        .unwrap_or_default()
        .to_string()
}

/// An account makes an enrollment code with the plus button of the zone of its phones, as its owner
/// does before he types it on the phone. The answer is the status and a JSON object of the page:
/// `code`, `fingerprint` and `expires_at_ms` for a code (status 200), or `notice` for the refusal
/// that the redirect names (status 303).
pub fn make_code(s: &Server, login: &str) -> (u16, Value) {
    let session = s.account(login);
    let (st, head, page) = inbox_form(s, &session.cookie, "/auth/settings/inbox/code", "");
    if st != 200 {
        let loc = location(&head);
        let notice = loc.split("s=").nth(1).unwrap_or_default().to_string();
        return (st, json!({ "notice": notice, "body": page }));
    }
    // The answer is the page, with the code in a popover: a text area with the id `enroll-code`.
    let field = page
        .find("id=\"enroll-code\"")
        .expect("the answer has the field of the code");
    let tag_start = page[..field].rfind('<').unwrap();
    let tag_end = field + page[field..].find('>').unwrap();
    let tag = &page[tag_start..tag_end];
    let code = unescape(page[tag_end + 1..].split("</textarea>").next().unwrap());
    let fingerprint = page
        .split("<dt>Server key</dt><dd><code>")
        .nth(1)
        .and_then(|t| t.split("</code>").next())
        .unwrap_or_default();
    (
        st,
        json!({
            "code": code,
            "fingerprint": fingerprint,
            "expires_at_ms": attr(tag, "data-expires-ms").and_then(|v| v.parse::<i64>().ok()),
        }),
    )
}

/// The text of the enrollment code in the answer of the plus button: the page, with the code in the
/// text area `enroll-code`.
pub fn code_in(page: &str) -> String {
    let field = page
        .find("id=\"enroll-code\"")
        .expect("the field of the code");
    let tag_end = field + page[field..].find('>').unwrap();
    unescape(page[tag_end + 1..].split("</textarea>").next().unwrap())
}

/// The text of a new code of an account.
pub fn code_for(s: &Server, login: &str) -> String {
    let (st, v) = make_code(s, login);
    assert_eq!(st, 200, "{v}");
    v["code"].as_str().unwrap().to_string()
}

pub async fn next_frame(ws: &mut Ws) -> Option<Message> {
    loop {
        match ws.next().await {
            Some(Ok(Message::Ping(_) | Message::Pong(_))) => continue,
            Some(Ok(message)) => return Some(message),
            Some(Err(_)) | None => return None,
        }
    }
}

/// The close code of a connection that the server closes, or `None` when it ends without one.
pub async fn close_code(ws: &mut Ws) -> Option<u16> {
    match next_frame(ws).await {
        Some(Message::Close(frame)) => frame.map(|f| f.code.into()),
        Some(other) => panic!("expected a close frame, got {other:?}"),
        None => None,
    }
}

/// Enrolls a phone with a code, as the app does. The address of the code gives the server.
pub async fn enroll_code(code_text: &str, name: &str) -> Result<Phone, EnrollFail> {
    enroll_code_at(None, code_text, name).await
}

/// Like [`enroll_code`], on another port than the address of the code (the code of one instance
/// used on another instance).
pub async fn enroll_code_at(
    port: Option<u16>,
    code_text: &str,
    name: &str,
) -> Result<Phone, EnrollFail> {
    let code = EnrollCode::parse(code_text).unwrap();
    let keys = inbox_core::noise::generate_keypair().unwrap();
    let (initiator, first) =
        EnrollInitiator::start(&keys.private, &code, name, "e2e").map_err(EnrollFail::Local)?;
    let url = match port {
        Some(port) => format!("ws://127.0.0.1:{port}/v1/enroll"),
        None => format!("ws://{}/v1/enroll", code.address),
    };
    let (mut ws, _) = tokio_tungstenite::connect_async(url).await.unwrap();
    ws.send(Message::Binary(first.into())).await.unwrap();
    match next_frame(&mut ws).await {
        Some(Message::Binary(reply)) => {
            let accept = initiator.finish(&reply).map_err(EnrollFail::Local)?;
            Ok(Phone {
                device_id: accept.device_id,
                device_private_key: keys.private.to_vec(),
                server_public_key: code.server_public_key.to_vec(),
                server_name: accept.server_name,
                user_name: accept.user_name,
            })
        }
        Some(Message::Close(frame)) => Err(EnrollFail::Closed(frame.map(|f| f.code.into()))),
        Some(other) => panic!("unexpected frame {other:?}"),
        None => Err(EnrollFail::Closed(None)),
    }
}

/// A phone of `login` enrolls on the instance `s`.
pub async fn enroll_as(s: &Server, name: &str, login: &str) -> Result<Phone, EnrollFail> {
    enroll_code(&code_for(s, login), name).await
}

/// A code that has the right server key and address but a secret that no account made.
pub fn unknown_code(s: &Server) -> String {
    let mut code = EnrollCode::parse(&code_for(s, SUPERADMIN)).unwrap();
    code.secret = [0x5a; 16];
    code.encode()
}

pub struct Conn {
    pub ws: Ws,
    pub session: Session,
}

pub enum Recv {
    Msg(ServerMsg),
    Closed(Option<u16>),
    Timeout,
}

impl Conn {
    pub async fn open(port: u16, dev: &Phone) -> Result<(Conn, ServerMsg), u16> {
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

    pub async fn send(&mut self, msg: &ClientMsg) {
        let frame = self
            .session
            .encrypt(&serde_json::to_vec(msg).unwrap())
            .unwrap();
        self.ws.send(Message::Binary(frame.into())).await.unwrap();
    }

    /// Sends a text as one encrypted frame, even if the protocol has no such message.
    pub async fn send_raw(&mut self, json: &str) {
        let frame = self.session.encrypt(json.as_bytes()).unwrap();
        self.ws.send(Message::Binary(frame.into())).await.unwrap();
    }

    pub async fn recv(&mut self, wait: Duration) -> Recv {
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

    pub async fn alert(&mut self) -> AlertMsg {
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

    /// Like [`Conn::alert`], with a longer wait. The portal makes its alerts after a delay.
    pub async fn alert_within(&mut self, wait: Duration) -> AlertMsg {
        let deadline = Instant::now() + wait;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            match self.recv(left).await {
                Recv::Msg(ServerMsg::Alert(a)) => return a,
                Recv::Msg(_) => continue,
                Recv::Closed(c) => panic!("closed while waiting for alert: {c:?}"),
                Recv::Timeout => panic!("no alert within {wait:?}"),
            }
        }
    }

    pub async fn no_alert(&mut self, wait: Duration) {
        let deadline = Instant::now() + wait;
        while Instant::now() < deadline {
            match self.recv(deadline - Instant::now()).await {
                Recv::Msg(ServerMsg::Alert(a)) => panic!("unexpected alert {a:?}"),
                Recv::Closed(c) => panic!("closed: {c:?}"),
                _ => {}
            }
        }
    }

    pub async fn ack(&mut self, id: &str, stage: AckStage) {
        self.send(&ClientMsg::Ack {
            alert_id: id.into(),
            stage,
        })
        .await;
    }

    pub async fn subscribe(&mut self, group: &str) {
        self.send(&ClientMsg::Subscribe {
            group: group.into(),
        })
        .await;
        loop {
            if let Recv::Msg(ServerMsg::Subscriptions { subscriptions }) =
                self.recv(Duration::from_secs(5)).await
            {
                assert!(subscriptions.iter().any(|s| s == group));
                return;
            }
        }
    }

    pub async fn closed_code(&mut self) -> Option<u16> {
        loop {
            match self.recv(Duration::from_secs(5)).await {
                Recv::Closed(c) => return c,
                Recv::Timeout => panic!("not closed"),
                Recv::Msg(_) => continue,
            }
        }
    }
}

/// A read or a change of the inbox with the session of the superadmin, in the manner of the pages of
/// the tab Inbox. `path` names the kind of call as the first tests did. The answer has the shape of
/// the old JSON of the tests, built from the rows of the pages:
/// - `GET /auth/inbox/api/overview`: `stats` (enrolled, online, unreachable) and `devices`.
/// - `GET /auth/inbox/api/alerts`: `alerts`, newest first (the 20 of the overview).
/// - `GET /auth/inbox/api/connections?user=<login>`: `connections`.
/// - `POST /auth/inbox/api/alerts` with `{group, title, idempotency_key}`: sends an alert. The status
///   is 201, or 200 for a duplicate. The body has `alert_id`.
/// - `POST /auth/inbox/api/devices/<id>/revoke`: the status is 200 and the body has `revoked`.
pub fn admin(s: &Server, method: &str, path: &str, body: &str) -> (u16, Value) {
    let session = s.session();
    let (route, query) = path.split_once('?').unwrap_or((path, ""));
    match (method, route) {
        ("GET", "/auth/inbox/api/overview") => {
            let (st, page) = inbox_page(s, "/auth/settings/inbox");
            if st != 200 {
                return (st, Value::Null);
            }
            let devices: Vec<Value> = rows(&page, "data-groups")
                .iter()
                .map(|tag| {
                    let state = attr(tag, "data-state").unwrap();
                    let groups: Vec<Value> = attr(tag, "data-groups")
                        .unwrap()
                        .split(',')
                        .filter(|g| !g.is_empty())
                        .map(|g| json!(g))
                        .collect();
                    json!({
                        "device": {
                            "id": attr(tag, "data-device").unwrap(),
                            "name": attr(tag, "data-name").unwrap(),
                            "user_name": attr(tag, "data-login").unwrap(),
                            "pending": attr(tag, "data-pending").unwrap().parse::<i64>().unwrap(),
                            "subscriptions": groups,
                        },
                        "status": state,
                    })
                })
                .collect();
            let count = |f: &dyn Fn(&Value) -> bool| devices.iter().filter(|d| f(d)).count();
            let stats = json!({
                "enrolled": count(&|d| d["status"] != "revoked"),
                "online": count(&|d| d["status"] == "online"),
                "unreachable": count(&|d| d["status"] == "unreachable"),
            });
            (st, json!({ "stats": stats, "devices": devices }))
        }
        ("GET", "/auth/inbox/api/alerts") => {
            let (st, page) = inbox_page(s, "/auth/settings/inbox");
            if st != 200 {
                return (st, Value::Null);
            }
            let alerts: Vec<Value> = rows(&page, "data-alert")
                .iter()
                .map(|tag| {
                    let number = |name: &str| attr(tag, name).unwrap().parse::<i64>().unwrap();
                    json!({
                        "id": attr(tag, "data-alert").unwrap(),
                        "group": attr(tag, "data-group").unwrap(),
                        "to": attr(tag, "data-to").unwrap(),
                        "source": attr(tag, "data-source").unwrap(),
                        "title": attr(tag, "data-title").unwrap(),
                        "received": number("data-received"),
                        "targets": number("data-targets"),
                        "expired": number("data-expired"),
                        "created_at": number("data-created"),
                    })
                })
                .collect();
            let limit = query
                .split('&')
                .find_map(|p| p.strip_prefix("limit="))
                .and_then(|v| v.parse::<usize>().ok())
                .unwrap_or(usize::MAX);
            let alerts: Vec<Value> = alerts.into_iter().take(limit).collect();
            (st, json!({ "alerts": alerts }))
        }
        ("GET", "/auth/inbox/api/connections") => {
            let login = query
                .split('&')
                .find_map(|p| p.strip_prefix("user="))
                .unwrap_or_default();
            let (st, page) = inbox_page(s, &format!("/auth/settings/inbox?login={login}"));
            if st != 200 {
                return (st, Value::Null);
            }
            let connections: Vec<Value> = rows(&page, "data-reason")
                .iter()
                .map(|tag| {
                    json!({
                        "user_name": attr(tag, "data-login").unwrap(),
                        "device_id": attr(tag, "data-device").unwrap(),
                        "close_reason": attr(tag, "data-reason").unwrap(),
                    })
                })
                .collect();
            (st, json!({ "connections": connections }))
        }
        ("POST", "/auth/inbox/api/alerts") => {
            let v: Value = serde_json::from_str(body).unwrap();
            // The page form names the target: `group:<id>` or `user:<login>`.
            let to = match (v["group"].as_str(), v["user"].as_str()) {
                (Some(group), None) => format!("group:{group}"),
                (None, Some(user)) => format!("user:{user}"),
                _ => panic!("one target: group or user"),
            };
            let fields = format!(
                "to={}&title={}&key={}",
                form_value(&to),
                form_value(v["title"].as_str().unwrap()),
                form_value(v["idempotency_key"].as_str().unwrap()),
            );
            let (st, head, text) =
                inbox_form(s, &session.cookie, "/auth/settings/inbox/send", &fields);
            let loc = location(&head);
            // A refused form answers the page itself, with the status 422.
            if st == 422 {
                return (400, json!({ "body": text }));
            }
            if st != 303 {
                return (st, json!({ "body": text }));
            }
            let id = loc
                .split("open=")
                .nth(1)
                .and_then(|r| r.split(['&', '#']).next())
                .unwrap_or_default();
            // The row that opens is the answer to a new alert. A repeat says so.
            let status = if loc.contains("s=duplicate") {
                200
            } else if loc.contains("open=") {
                201
            } else {
                400
            };
            (status, json!({ "alert_id": id, "location": loc }))
        }
        ("POST", route) if route.starts_with("/auth/inbox/api/devices/") => {
            let id = route
                .trim_start_matches("/auth/inbox/api/devices/")
                .trim_end_matches("/revoke");
            let (st, head, text) = inbox_form(
                s,
                &session.cookie,
                "/auth/settings/inbox/revoke",
                &format!("id.{id}=1"),
            );
            if st != 303 {
                return (st, json!({ "body": text }));
            }
            let revoked = location(&head) == "/auth/settings/inbox";
            (200, json!({ "revoked": revoked }))
        }
        _ => (404, Value::Null),
    }
}
