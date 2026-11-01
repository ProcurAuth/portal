//! Black-box end-to-end test of the inbox: it runs the real `portal` binary with a combined
//! configuration and drives it with the real protocol 4 of the phones. A phone enrolls as the app
//! does: an account of the portal makes a code on the page "My phones" of the tab Inbox
//! (`POST /auth/settings/inbox/code`), and the phone uses it on `/v1/enroll` (Noise IK, no OPAQUE).
//! Then it opens Noise sessions. The admin checks read the pages of the tab Inbox with the session
//! of the superadmin. The rows of these pages carry `data-*` attributes for the tests.

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

const BIN: &str = env!("CARGO_BIN_EXE_portal");

type Ws =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

fn portal(args: &[&str], stdin: Option<&str>) -> std::process::Output {
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
    fn alerts(&self, database: &str) -> Vec<Value> {
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

    fn create_database(&self, name: &str) {
        self.exec("postgres", format!("CREATE DATABASE {name}"));
    }

    fn drop_database(&self, name: &str) {
        self.exec(
            "postgres",
            format!("DROP DATABASE IF EXISTS {name} WITH (FORCE)"),
        );
    }
}

/// The login and the password of the superadmin of the test servers.
const SUPERADMIN: &str = "root";
const SUPERADMIN_PASSWORD: &str = "a password for the end to end tests";

/// The session of an account: the cookie. The token of the forms comes from a page of the inbox.
struct AdminSession {
    cookie: String,
}

struct Server {
    dir: PathBuf,
    /// Name of this instance: its configuration and log files.
    name: String,
    /// The phone port (`inbox.devices.listen`): phones only.
    port: u16,
    /// The listener of the portal: the sign-in and the admin calls of the inbox.
    admin_port: u16,
    child: Option<Child>,
    token: String,
    /// The database to drop with the server that created it.
    pg: Option<(PgAdmin, String)>,
    /// The session of the superadmin. A start of the server clears it.
    session: std::sync::Mutex<Option<std::sync::Arc<AdminSession>>>,
    /// The sessions of the other accounts, by login. A start of the server clears them.
    accounts: std::sync::Mutex<std::collections::HashMap<String, std::sync::Arc<AdminSession>>>,
}

impl Server {
    fn setup() -> Server {
        match pg_admin().filter(|_| postgres_backend()) {
            Some(admin) => Server::setup_with(Some(admin)),
            None => Server::setup_with(None),
        }
    }

    fn setup_with(pg: Option<PgAdmin>) -> Server {
        let dir = std::env::temp_dir().join(format!(
            "portal-inbox-e2e-{}-{}",
            std::process::id(),
            free_port()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let p = |n: &str| dir.join(n).to_string_lossy().to_string();
        assert!(
            portal(&["inbox", "keygen", "--out", &p("server_key")], None)
                .status
                .success()
        );
        let key = p("server_key");
        assert!(portal(&["inbox", "token", "--out", &p("token")], None)
            .status
            .success());
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
        let admin_port = free_port();
        let database = format!("inbox_e2e_{}", free_port());
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
  listen_address = "127.0.0.1:{admin_port}"
  trusted_proxies = []
}}
authentication {{
  superadmin_login_file = "{login}"
  superadmin_password_file = "{password}"
}}
session {{
  secret_mode = "provided"
  secret_file = "{secret}"
}}
{storage_block}
logging {{
  level = "info"
}}
inbox {{
  name = "E2E"
  server_key_file = "{key}"
  devices {{
    listen = "127.0.0.1:{port}"
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
  producer "ci" {{
    token_file = "{tok}"
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
            admin_port,
            child: None,
            token,
            pg,
            session: Default::default(),
            accounts: Default::default(),
        }
    }

    /// Adds text inside the block `inbox` of the configuration: the blocks `inbox.replica` and
    /// `inbox.cluster`.
    fn add_to_inbox(&self, extra: &str) {
        let text = std::fs::read_to_string(self.config()).unwrap();
        assert!(text.contains("  # inbox-extra\n"));
        std::fs::write(
            self.config(),
            text.replace("  # inbox-extra\n", &format!("{extra}\n  # inbox-extra\n")),
        )
        .unwrap();
    }

    /// Adds a block at the top of the configuration, after the block `inbox`.
    fn add_block(&self, block: &str) {
        let text = std::fs::read_to_string(self.config()).unwrap();
        std::fs::write(self.config(), format!("{text}\n{block}\n")).unwrap();
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
            session: Default::default(),
            accounts: Default::default(),
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
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            if portal(&["healthcheck", "--config", &self.config()], None)
                .status
                .success()
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

    fn stop(&mut self) {
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

    fn log(&self) -> String {
        std::fs::read_to_string(self.dir.join(format!("{}.log", self.name))).unwrap_or_default()
    }

    /// The session of the superadmin. It signs in on the first call.
    fn session(&self) -> std::sync::Arc<AdminSession> {
        let mut slot = self.session.lock().unwrap();
        if let Some(session) = slot.as_ref() {
            return session.clone();
        }
        let session = std::sync::Arc::new(sign_in(self.admin_port));
        *slot = Some(session.clone());
        session
    }

    /// The session of an account: the superadmin, or a database account that the superadmin makes
    /// on the first call.
    fn account(&self, login: &str) -> std::sync::Arc<AdminSession> {
        self.account_with(login, &DEFAULT_GRANTS)
    }

    /// The same, with the permissions that the test chooses for a new account.
    fn account_with(&self, login: &str, grants: &[&str]) -> std::sync::Arc<AdminSession> {
        if login == SUPERADMIN {
            return self.session();
        }
        if let Some(session) = self.accounts.lock().unwrap().get(login) {
            return session.clone();
        }
        add_account_with(self, login, grants);
        let session = std::sync::Arc::new(sign_in_as(self.admin_port, login, ACCOUNT_PASSWORD));
        self.accounts
            .lock()
            .unwrap()
            .insert(login.to_string(), session.clone());
        session
    }
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

/// The password of the accounts that the tests make.
const ACCOUNT_PASSWORD: &str = "e2e-account-password-0123";

fn sign_in(port: u16) -> AdminSession {
    sign_in_as(port, SUPERADMIN, SUPERADMIN_PASSWORD)
}

/// One sign-in on the listener of the portal, with a fresh sign-in page and its tokens. The
/// answer is the status, the head and the body.
fn sign_in_request(port: u16, login: &str, password: &str) -> (u16, String, String) {
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
    let field = |name: &str| {
        let marker = format!("name=\"{name}\" value=\"");
        let start = page.find(&marker).expect("a field of the form") + marker.len();
        page[start..].split('"').next().unwrap().to_string()
    };
    let form = format!(
        "login={login}&password={}&csrf={}&rd={}&sig={}",
        password.replace(' ', "+"),
        field("csrf"),
        field("rd"),
        field("sig")
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

/// `count` sign-ins with a wrong password. The portal refuses each of them, or blocks it with a
/// limiter. Make the sessions that the test needs before: the limiter locks the address out.
fn refuse_sign_ins(port: u16, login: &str, count: usize) {
    for _ in 0..count {
        let (st, _, body) = sign_in_request(port, login, "a wrong password for the test");
        assert!(st == 401 || st == 429, "{st}: {body}");
    }
}

/// Signs in on the listener of the portal.
fn sign_in_as(port: u16, login: &str, password: &str) -> AdminSession {
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
fn http(
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

fn http_host(
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
fn http_full(
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
    let status = text.split_whitespace().nth(1).unwrap().parse().unwrap();
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
        s.admin_port,
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
struct Phone {
    device_id: String,
    device_private_key: Vec<u8>,
    server_public_key: Vec<u8>,
    server_name: String,
    user_name: String,
}

/// Why an enrollment did not end with message 2.
#[derive(Debug)]
enum EnrollFail {
    /// The server closed the WebSocket without message 2. The value is the close code.
    Closed(Option<u16>),
    /// The device side refused: the state machine of the core returned an error.
    #[allow(dead_code)] // the value shows in the failure message of a test
    Local(inbox_core::Error),
}

/// The users page gives the anti-CSRF token of the forms that manage the accounts.
fn users_csrf(s: &Server) -> String {
    let admin = s.session();
    let host = format!("127.0.0.1:{}", s.admin_port);
    let (st, _, page) = http_full(
        s.admin_port,
        &host,
        "GET",
        "/auth/settings?add=1",
        &[("Cookie", admin.cookie.clone())],
        "",
    );
    assert_eq!(st, 200, "{page}");
    // The token of the form that adds an account. The page has other forms with other tokens.
    let form = page
        .find("id=\"add-user\"")
        .expect("the users page has the add form");
    let marker = "name=\"csrf\" value=\"";
    let start = form + page[form..].find(marker).expect("the form has a token") + marker.len();
    page[start..].split('"').next().unwrap().to_string()
}

/// A form of the users pages, sent by the superadmin. The answer is a redirect with the outcome.
fn users_form(s: &Server, path: &str, fields: &str) -> String {
    let admin = s.session();
    let host = format!("127.0.0.1:{}", s.admin_port);
    let body = format!("csrf={}&{fields}", users_csrf(s));
    let (st, head, text) = http_full(
        s.admin_port,
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
const DEFAULT_GRANTS: [&str; 2] = [
    "group.subscribe on service:inbox/group:infra",
    "group.subscribe on service:inbox/group:other",
];

/// Encodes a value of a form.
fn form_value(text: &str) -> String {
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
fn add_account(s: &Server, login: &str) {
    add_account_with(s, login, &DEFAULT_GRANTS);
}

/// The same, with the permissions that the test chooses.
fn add_account_with(s: &Server, login: &str, grants: &[&str]) {
    users_form(
        s,
        "/auth/users/add",
        &format!("login={login}&password={ACCOUNT_PASSWORD}"),
    );
    set_grants(s, login, grants);
}

/// The superadmin sets the permissions of a database account. The form replaces the permissions of
/// the account, but those of the service `portal`.
fn set_grants(s: &Server, login: &str, grants: &[&str]) {
    let mut fields = format!("login={login}&grants=1");
    for (n, grant) in grants.iter().enumerate() {
        fields.push_str(&format!("&grant{n}={}", form_value(grant)));
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
fn delete_account(s: &Server, login: &str) {
    let location = users_form(s, "/auth/users/delete", &format!("login={login}"));
    assert!(location.contains("s=user-deleted"), "{location}");
    s.accounts.lock().unwrap().remove(login);
}

/// The token of the forms of the inbox, from a page of the tab. The overview has the form that sends
/// an alert, and the page "My phones" has the form that makes a code.
fn inbox_csrf(port: u16, cookie: &str) -> String {
    let host = format!("127.0.0.1:{port}");
    for path in [
        "/auth/settings/inbox",
        "/auth/settings/inbox/phones",
        "/auth/settings/inbox/devices",
    ] {
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
fn unescape(text: &str) -> String {
    text.replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&amp;", "&")
}

/// The value of the attribute `name` in the tag text.
fn attr(tag: &str, name: &str) -> Option<String> {
    let marker = format!(" {name}=\"");
    let start = tag.find(&marker)? + marker.len();
    Some(unescape(tag[start..].split('"').next()?))
}

/// The opening tags `<tr ...>` of a page that carry the attribute `key`.
fn rows(page: &str, key: &str) -> Vec<String> {
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
fn inbox_page(s: &Server, path: &str) -> (u16, String) {
    inbox_page_as(s, &s.session().cookie, path)
}

fn inbox_page_as(s: &Server, cookie: &str, path: &str) -> (u16, String) {
    let host = format!("127.0.0.1:{}", s.admin_port);
    let (st, _, page) = http_full(
        s.admin_port,
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
fn inbox_form(s: &Server, cookie: &str, path: &str, fields: &str) -> (u16, String, String) {
    let host = format!("127.0.0.1:{}", s.admin_port);
    let csrf = inbox_csrf(s.admin_port, cookie);
    let body = if fields.is_empty() {
        format!("csrf={csrf}")
    } else {
        format!("csrf={csrf}&{fields}")
    };
    http_full(
        s.admin_port,
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
fn location(head: &str) -> String {
    head.lines()
        .find_map(|l| {
            l.strip_prefix("location: ")
                .or(l.strip_prefix("Location: "))
        })
        .unwrap_or_default()
        .to_string()
}

/// An account makes an enrollment code on the page "My phones", as its owner does before he types it on
/// the phone. The answer is the status and a JSON object of the page: `code`, `fingerprint` and
/// `expires_at_ms` for a code (status 200), or `notice` for the refusal that the redirect names
/// (status 303).
fn make_code(s: &Server, login: &str) -> (u16, Value) {
    let session = s.account(login);
    let (st, head, page) = inbox_form(s, &session.cookie, "/auth/settings/inbox/code", "");
    if st != 200 {
        let loc = location(&head);
        let notice = loc.split("s=").nth(1).unwrap_or_default().to_string();
        return (st, json!({ "notice": notice, "body": page }));
    }
    let field = page
        .find("id=\"enroll-code\"")
        .expect("the code page has the field of the code");
    let tag_start = page[..field].rfind('<').unwrap();
    let tag = &page[tag_start..field + page[field..].find('>').unwrap()];
    let fingerprint = page
        .split("fingerprint <code>")
        .nth(1)
        .and_then(|t| t.split("</code>").next())
        .unwrap_or_default();
    (
        st,
        json!({
            "code": attr(tag, "value").unwrap(),
            "fingerprint": fingerprint,
            "expires_at_ms": attr(tag, "data-expires-ms").and_then(|v| v.parse::<i64>().ok()),
        }),
    )
}

/// The text of a new code of an account.
fn code_for(s: &Server, login: &str) -> String {
    let (st, v) = make_code(s, login);
    assert_eq!(st, 200, "{v}");
    v["code"].as_str().unwrap().to_string()
}

async fn next_frame(ws: &mut Ws) -> Option<Message> {
    loop {
        match ws.next().await {
            Some(Ok(Message::Ping(_) | Message::Pong(_))) => continue,
            Some(Ok(message)) => return Some(message),
            Some(Err(_)) | None => return None,
        }
    }
}

/// The close code of a connection that the server closes, or `None` when it ends without one.
async fn close_code(ws: &mut Ws) -> Option<u16> {
    match next_frame(ws).await {
        Some(Message::Close(frame)) => frame.map(|f| f.code.into()),
        Some(other) => panic!("expected a close frame, got {other:?}"),
        None => None,
    }
}

/// Enrolls a phone with a code, as the app does. The address of the code gives the server.
async fn enroll_code(code_text: &str, name: &str) -> Result<Phone, EnrollFail> {
    enroll_code_at(None, code_text, name).await
}

/// Like [`enroll_code`], on another port than the address of the code (the code of one instance
/// used on another instance).
async fn enroll_code_at(
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
async fn enroll_as(s: &Server, name: &str, login: &str) -> Result<Phone, EnrollFail> {
    enroll_code(&code_for(s, login), name).await
}

/// A code that has the right server key and address but a secret that no account made.
fn unknown_code(s: &Server) -> String {
    let mut code = EnrollCode::parse(&code_for(s, SUPERADMIN)).unwrap();
    code.secret = [0x5a; 16];
    code.encode()
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
    async fn open(port: u16, dev: &Phone) -> Result<(Conn, ServerMsg), u16> {
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
        let frame = self
            .session
            .encrypt(&serde_json::to_vec(msg).unwrap())
            .unwrap();
        self.ws.send(Message::Binary(frame.into())).await.unwrap();
    }

    /// Sends a text as one encrypted frame, even if the protocol has no such message.
    async fn send_raw(&mut self, json: &str) {
        let frame = self.session.encrypt(json.as_bytes()).unwrap();
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

    /// Like [`Conn::alert`], with a longer wait. The portal makes its alerts after a delay.
    async fn alert_within(&mut self, wait: Duration) -> AlertMsg {
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
        self.send(&ClientMsg::Ack {
            alert_id: id.into(),
            stage,
        })
        .await;
    }

    async fn subscribe(&mut self, group: &str) {
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

/// A read or a change of the inbox with the session of the superadmin, in the manner of the pages of
/// the tab Inbox. `path` names the kind of call as the first tests did. The answer has the shape of
/// the old JSON of the tests, built from the rows of the pages:
/// - `GET /auth/inbox/api/overview`: `stats` (enrolled, online, unreachable) and `devices`.
/// - `GET /auth/inbox/api/alerts`: `alerts`, newest first (the 20 of the overview).
/// - `GET /auth/inbox/api/connections?user=<login>`: `connections`.
/// - `POST /auth/inbox/api/alerts` with `{group, title, idempotency_key}`: sends an alert. The status
///   is 201, or 200 for a duplicate. The body has `alert_id`.
/// - `POST /auth/inbox/api/devices/<id>/revoke`: the status is 200 and the body has `revoked`.
fn admin(s: &Server, method: &str, path: &str, body: &str) -> (u16, Value) {
    let session = s.session();
    let (route, query) = path.split_once('?').unwrap_or((path, ""));
    match (method, route) {
        ("GET", "/auth/inbox/api/overview") => {
            let (st, page) = inbox_page(s, "/auth/settings/inbox/devices");
            if st != 200 {
                return (st, Value::Null);
            }
            let devices: Vec<Value> = rows(&page, "data-device")
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
            let (st, page) = inbox_page(
                s,
                &format!("/auth/settings/inbox/connections?login={login}"),
            );
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
            let fields = format!(
                "group={}&title={}&key={}",
                form_value(v["group"].as_str().unwrap()),
                form_value(v["title"].as_str().unwrap()),
                form_value(v["idempotency_key"].as_str().unwrap()),
            );
            let (st, head, text) =
                inbox_form(s, &session.cookie, "/auth/settings/inbox/send", &fields);
            let loc = location(&head);
            if st != 303 {
                return (st, json!({ "body": text }));
            }
            let id = loc
                .strip_prefix("/auth/settings/inbox/alerts/")
                .and_then(|r| r.split('?').next())
                .unwrap_or_default();
            let status = if loc.contains("s=sent") {
                201
            } else if loc.contains("s=duplicate") {
                200
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
                &format!("id={id}&back=%2Fauth%2Fsettings%2Finbox%2Fdevices"),
            );
            if st != 303 {
                return (st, json!({ "body": text }));
            }
            let revoked = location(&head).contains("s=revoked");
            (200, json!({ "revoked": revoked }))
        }
        _ => (404, Value::Null),
    }
}

#[test]
fn config_validation_is_strict() {
    let s = Server::setup();
    let ok = portal(&["check-config", "--config", &s.config()], None);
    assert!(
        ok.status.success(),
        "{}",
        String::from_utf8_lossy(&ok.stderr)
    );
    let text = std::fs::read_to_string(s.config()).unwrap();
    let broken = s.dir.join("broken.hcl");
    std::fs::write(&broken, text.replace("  resend_after = \"1s\"\n", "")).unwrap();
    let out = portal(
        &["check-config", "--config", broken.to_str().unwrap()],
        None,
    );
    assert_eq!(out.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&out.stderr).contains("resend_after"));
    // Missing secret file.
    std::fs::write(&broken, text.replace("token\"", "nope\"")).unwrap();
    assert!(!portal(
        &["check-config", "--config", broken.to_str().unwrap()],
        None
    )
    .status
    .success());
    // No implicit config path.
    assert_eq!(portal(&["run"], None).status.code(), Some(1));
    // A delivery value has no default.
    for key in [
        "heartbeat_interval",
        "session_timeout",
        "unreachable_after",
        "offline_warning_after",
        "max_inflight",
        "delivery_ttl",
        "alert_retention",
        "connection_log_retention",
    ] {
        let without: String = text
            .lines()
            .filter(|l| !l.trim_start().starts_with(&format!("{key} ")))
            .map(|l| format!("{l}\n"))
            .collect();
        std::fs::write(&broken, without).unwrap();
        let out = portal(
            &["check-config", "--config", broken.to_str().unwrap()],
            None,
        );
        assert_eq!(out.status.code(), Some(1), "{key} was optional");
        let error = String::from_utf8_lossy(&out.stderr).to_string();
        assert!(
            error.contains(key) && error.contains("missing required configuration key"),
            "{error}"
        );
    }
    // The closed schema: an unknown key, a removed key, a secret in clear text.
    for (from, to, expected) in [
        (
            "max_devices = 10",
            "max_devicez = 10",
            "unknown configuration key: inbox.devices.max_devicez",
        ),
        (
            "    max_devices = 10",
            "    max_devices = 10\n    admin_listen = \"127.0.0.1:1\"",
            "inbox.devices.admin_listen",
        ),
        // The phone port has no list of proxies of its own: it uses `server.trusted_proxies`.
        (
            "    max_devices = 10",
            "    max_devices = 10\n    trusted_proxies = []",
            "unknown configuration key: inbox.devices.trusted_proxies",
        ),
        (
            "resend_after = \"1s\"",
            "resend_after = 1",
            "inbox.devices.resend_after",
        ),
        (
            "  name = \"E2E\"",
            "  name = \"E2E\"\n  server_key = \"abc\"",
            "must not be written in the file",
        ),
        ("groups = [\"infra\"]", "groups = [\"nope\"]", "nope"),
        (
            "heartbeat_interval = \"5s\"",
            "heartbeat_interval = \"1s\"",
            "heartbeat_interval",
        ),
        (
            "session_timeout = \"10s\"",
            "session_timeout = \"9s\"",
            "session_timeout",
        ),
    ] {
        assert!(text.contains(from), "{from}");
        std::fs::write(&broken, text.replacen(from, to, 1)).unwrap();
        let out = portal(
            &["check-config", "--config", broken.to_str().unwrap()],
            None,
        );
        assert_eq!(out.status.code(), Some(1), "{to}");
        let error = String::from_utf8_lossy(&out.stderr).to_string();
        assert!(error.contains(expected), "{to}: {error}");
    }
    // The address of the listener, as the phones see it, is mandatory and checked.
    let public = format!("    public_address = \"127.0.0.1:{}\"\n", s.port);
    assert!(text.contains(&public), "{text}");
    std::fs::write(&broken, text.replacen(&public, "", 1)).unwrap();
    let out = portal(
        &["check-config", "--config", broken.to_str().unwrap()],
        None,
    );
    assert_eq!(out.status.code(), Some(1));
    let error = String::from_utf8_lossy(&out.stderr).to_string();
    assert!(
        error.contains("missing required configuration key")
            && error.contains("inbox.devices.public_address"),
        "{error}"
    );
    for bad in [
        "",
        "ftp://inbox.example.org",
        "inbox example",
        "inbox.example.org:0",
    ] {
        std::fs::write(
            &broken,
            text.replacen(&public, &format!("    public_address = \"{bad}\"\n"), 1),
        )
        .unwrap();
        let out = portal(
            &["check-config", "--config", broken.to_str().unwrap()],
            None,
        );
        assert_eq!(out.status.code(), Some(1), "{bad:?}");
        let error = String::from_utf8_lossy(&out.stderr).to_string();
        assert!(
            error.contains("inbox.devices.public_address"),
            "{bad:?}: {error}"
        );
    }
    // The shared password of protocol 3 does not exist any more, and its command either.
    std::fs::write(
        &broken,
        text.replacen(
            &public,
            &format!("{public}    credential_file = \"/x\"\n"),
            1,
        ),
    )
    .unwrap();
    let out = portal(
        &["check-config", "--config", broken.to_str().unwrap()],
        None,
    );
    assert_eq!(out.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&out.stderr).contains("inbox.devices.credential"));
    let out = portal(&["inbox", "credential", "--out", "/nonexistent/x"], None);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("unknown inbox command"));
    // The enrollment block has documented defaults, and its values have ranges.
    let shown = String::from_utf8_lossy(&ok.stdout).to_string();
    assert!(
        shown.contains("inbox.enrollment.code_ttl = \"10m\"  # default"),
        "{shown}"
    );
    assert!(
        shown.contains("inbox.enrollment.max_active_per_account = 5  # default"),
        "{shown}"
    );
    for (block, expected) in [
        (
            "enrollment { code_ttl = \"30s\" }",
            "inbox.enrollment.code_ttl",
        ),
        (
            "enrollment { max_active_per_account = 0 }",
            "inbox.enrollment.max_active_per_account",
        ),
        (
            "enrollment { max_active_per_account = 5 colour = 1 }",
            "inbox.enrollment.colour",
        ),
    ] {
        std::fs::write(
            &broken,
            text.replacen(
                "  # inbox-extra\n",
                &format!("  {block}\n  # inbox-extra\n"),
                1,
            ),
        )
        .unwrap();
        let out = portal(
            &["check-config", "--config", broken.to_str().unwrap()],
            None,
        );
        assert_eq!(out.status.code(), Some(1), "{block}");
        let error = String::from_utf8_lossy(&out.stderr).to_string();
        assert!(error.contains(expected), "{block}: {error}");
    }

    // The listener of the phones must not use the port of the portal.
    let same = text.replacen(
        &format!("listen = \"127.0.0.1:{}\"", s.port),
        &format!("listen = \"127.0.0.1:{}\"", s.admin_port),
        1,
    );
    std::fs::write(&broken, same).unwrap();
    let out = portal(
        &["check-config", "--config", broken.to_str().unwrap()],
        None,
    );
    assert_eq!(out.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&out.stderr).contains("inbox.devices.listen"));
}

/// The API of the producers is on the port of the pages. The phones have their own port, which has no
/// API and no page. The routes of the inbox need no session of the portal and set no cookie.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn producers_use_the_port_of_the_pages_and_phones_use_their_own() {
    let mut s = Server::setup();
    s.start();
    let (pages, phones) = (s.admin_port, s.port);
    let host = format!("127.0.0.1:{pages}");

    // The sign-in page and the API of the producers are on the same port.
    let (st, _, page) = http_full(pages, &host, "GET", "/auth/login", &[], "");
    assert_eq!(st, 200, "{page}");
    let phone = enroll_as(&s, "phone", SUPERADMIN).await.unwrap();
    let (mut c, _) = Conn::open(phones, &phone).await.unwrap();
    c.subscribe("infra").await;
    let auth = ("Authorization", format!("Bearer {}", s.token));
    let (st, head, body) = http_full(
        pages,
        &host,
        "POST",
        "/api/v1/alerts",
        std::slice::from_ref(&auth),
        r#"{"group":"infra","title":"on the port of the pages"}"#,
    );
    assert_eq!(st, 201, "{body}");
    assert!(
        !head.to_ascii_lowercase().contains("set-cookie"),
        "a route of the inbox sets no cookie: {head}"
    );
    let got = c.alert().await;
    assert_eq!(got.title, "on the port of the pages");
    c.ack(&got.alert_id, AckStage::Received).await;

    // The refusals are those of the inbox. A cookie of the portal changes nothing.
    let (st, body) = http(
        pages,
        "POST",
        "/api/v1/alerts",
        &[],
        r#"{"group":"infra","title":"x"}"#,
    );
    assert_eq!(st, 401, "{body}");
    let cookie = ("Cookie", s.session().cookie.clone());
    let (st, _) = http(
        pages,
        "POST",
        "/api/v1/alerts",
        &[cookie],
        r#"{"group":"infra","title":"x"}"#,
    );
    assert_eq!(st, 401);
    let (st, _) = http(
        pages,
        "GET",
        "/api/v1/alerts",
        std::slice::from_ref(&auth),
        "",
    );
    assert_eq!(st, 405);
    let (st, _) = http(pages, "GET", "/api/v1/nothing", &[], "");
    assert_eq!(st, 404);

    // Readiness and version are on the port of the pages.
    let (st, body) = http(pages, "GET", "/healthz", &[], "");
    assert_eq!((st, body.trim()), (200, "ok"));
    let (st, body) = http(pages, "GET", "/readyz", &[], "");
    assert_eq!(st, 200, "{body}");
    assert!(body.contains("ready"), "{body}");
    let (st, _) = http(pages, "POST", "/readyz", &[], "");
    assert_eq!(st, 405);
    let (st, body) = http(pages, "GET", "/api/version", &[], "");
    assert_eq!(st, 200, "{body}");
    assert!(body.contains("\"protocol\""), "{body}");

    // The routes of the phones are not on the port of the pages. The portal does not know them.
    let (st, body) = http(pages, "GET", "/v1/session", &[], "");
    assert_ne!(st, 400, "the pages port is not the phone port: {body}");
    assert!(!body.contains("WebSocket"), "{body}");

    // The phone port has the routes of the phones and the health routes only.
    let (st, body) = http(phones, "GET", "/v1/session", &[], "");
    assert_eq!(st, 400, "a plain GET is not an upgrade: {body}");
    let (st, _) = http(phones, "POST", "/v1/session", &[], "");
    assert_eq!(st, 405);
    let (st, _) = http(phones, "GET", "/healthz", &[], "");
    assert_eq!(st, 200);
    let (st, _) = http(phones, "GET", "/readyz", &[], "");
    assert_eq!(st, 200);
    for path in ["/api/v1/alerts", "/api/version", "/auth/login", "/"] {
        let (st, _) = http(phones, "GET", path, &[], "");
        assert_eq!(st, 404, "{path} is not on the phone port");
    }
    let (st, _) = http(
        phones,
        "POST",
        "/api/v1/alerts",
        std::slice::from_ref(&auth),
        "{}",
    );
    assert_eq!(st, 404, "the API of the producers is not on the phone port");

    // The pages work on their port. The launcher has no tile for the inbox: it is a feature of the portal.
    let (st, page) = inbox_page(&s, "/auth/settings/inbox");
    assert_eq!(st, 200, "{page}");
    let (st, _, home) = http_full(
        pages,
        &host,
        "GET",
        "/",
        &[("Cookie", s.session().cookie.clone())],
        "",
    );
    assert_eq!(st, 200);
    assert!(!home.contains("/auth/settings/inbox\""), "no tile: {home}");
    s.stop();
}

/// With the block `inbox`, the listener of the portal opens before the catalogue is complete and serves
/// the API of the inbox only. The phone port is open too. An alert and a phone never wait for the
/// catalogue. Every other route of the portal fails closed with 503. A stop in this time ends the process.
#[test]
fn the_inbox_answers_before_the_catalogue_is_complete() {
    let mut s = Server::setup();
    // A source that never answers: the catalogue is never complete.
    s.add_block("discovery {\n  docker_socket = \"/nonexistent/docker.sock\"\n}");
    let log = std::fs::File::create(s.dir.join("server.log")).unwrap();
    s.child = Some(
        Command::new(BIN)
            .args(["run", "--config", &s.config()])
            .stdout(Stdio::null())
            .stderr(log)
            .spawn()
            .unwrap(),
    );
    let (pages, phones) = (s.admin_port, s.port);
    let deadline = Instant::now() + Duration::from_secs(30);
    while TcpStream::connect(("127.0.0.1", pages)).is_err() {
        assert!(Instant::now() < deadline, "no port: {}", s.log());
        std::thread::sleep(Duration::from_millis(100));
    }
    let (st, body) = http(pages, "GET", "/readyz", &[], "");
    assert_eq!(st, 200, "{body}");
    let auth = ("Authorization", format!("Bearer {}", s.token));
    let (st, body) = http(
        pages,
        "POST",
        "/api/v1/alerts",
        std::slice::from_ref(&auth),
        r#"{"group":"infra","title":"before the catalogue"}"#,
    );
    assert_eq!(st, 201, "{body}");
    let (st, _) = http(phones, "GET", "/v1/session", &[], "");
    assert_eq!(st, 400, "the phones reach the inbox");
    for path in ["/auth/login", "/healthz", "/auth/verify", "/"] {
        let (st, body) = http(pages, "GET", path, &[], "");
        assert_eq!((st, body.trim()), (503, "starting"), "{path}");
    }
    assert!(!portal(&["healthcheck", "--config", &s.config()], None)
        .status
        .success());
    s.stop();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn delivery_guarantees_end_to_end() {
    let mut s = Server::setup();
    s.start();
    let port = s.port;

    // An unknown code is refused with the close code 4410 and without message 2. The accounts
    // enroll their phones with codes that they make themselves.
    assert!(matches!(
        enroll_code(&unknown_code(&s), "intruder").await,
        Err(EnrollFail::Closed(Some(4410)))
    ));
    let a = enroll_as(&s, "phone-a", "alice").await.unwrap();
    let b = enroll_as(&s, "phone-b", "bob").await.unwrap();
    assert_eq!(a.server_name, "E2E");
    assert_eq!(
        (a.user_name.as_str(), b.user_name.as_str()),
        ("alice", "bob")
    );

    let (mut ca, welcome) = Conn::open(port, &a).await.unwrap();
    let ServerMsg::Welcome {
        groups,
        subscriptions,
        heartbeat_interval_ms,
        user_name,
        ..
    } = welcome
    else {
        unreachable!()
    };
    assert_eq!(
        user_name, "alice",
        "the welcome carries the login of the account"
    );
    assert_eq!(groups.len(), 2);
    assert!(subscriptions.is_empty());
    assert_eq!(heartbeat_interval_ms, 5000);
    ca.subscribe("infra").await;
    {
        let (mut cb, _) = Conn::open(port, &b).await.unwrap();
        cb.subscribe("infra").await;
        // The login is read-only: the message that changed it does not exist in protocol 4.
        cb.send_raw(r#"{"t":"set_user_name","user_name":"x"}"#)
            .await;
        assert_eq!(
            cb.closed_code().await,
            Some(inbox_core::proto::close::PROTOCOL)
        );
    } // b goes offline

    // Producer API checks.
    let (st, _) = http(
        s.admin_port,
        "POST",
        "/api/v1/alerts",
        &[("Idempotency-Key", "x".into())],
        r#"{"group":"infra"}"#,
    );
    assert_eq!(st, 401);
    assert_eq!(
        post_alert(&s, "f", json!({"group": "other", "title": "t"})).0,
        403
    );
    assert_eq!(
        post_alert(&s, "u", json!({"group": "infra", "title": "t", "bogus": 1})).0,
        400
    );
    assert_eq!(
        post_alert(&s, "n", json!({"group": "infra"})).0,
        400,
        "the title is required"
    );
    assert_eq!(
        post_alert(&s, "b", json!({"group": "infra", "title": "  "})).0,
        422
    );

    // Accepted once, delivered once, deduplicated on retry.
    let (st, v) = post_alert(&s, "k1", json!({"group": "infra", "title": "Disk full"}));
    assert_eq!(st, 201, "{v}");
    assert_eq!(v["targets"], 2);
    let k1 = v["alert_id"].as_str().unwrap().to_string();
    let got = ca.alert().await;
    assert_eq!(got.alert_id, k1);
    assert_eq!(
        (got.title.as_str(), got.group.as_str()),
        ("Disk full", "infra")
    );
    ca.ack(&k1, AckStage::Received).await;
    let (st, v) = post_alert(&s, "k1", json!({"group": "infra", "title": "Disk full"}));
    assert_eq!((st, v["duplicate"].as_bool()), (200, Some(true)));
    assert_eq!(v["alert_id"], k1.as_str());
    ca.no_alert(Duration::from_millis(2500)).await;
    assert_eq!(
        post_alert(&s, "k1", json!({"group": "infra", "title": "changed"})).0,
        409
    );

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
    assert_eq!(
        (first.alert_id.as_str(), second.alert_id.as_str()),
        (k1.as_str(), k2.as_str())
    );
    cb.ack(&k1, AckStage::Received).await;
    cb.ack(&k2, AckStage::Received).await;
    cb.no_alert(Duration::from_millis(2500)).await;

    // A second session of the same device replaces the first.
    let (mut cb2, _) = Conn::open(port, &b).await.unwrap();
    assert_eq!(
        cb.closed_code().await,
        Some(inbox_core::proto::close::REPLACED)
    );
    drop(cb2.ws.close(None).await);

    // Pending deliveries survive a server restart.
    drop(ca);
    let (_, v) = post_alert(
        &s,
        "k3",
        json!({"group": "infra", "title": "while you were away"}),
    );
    let k3 = v["alert_id"].as_str().unwrap().to_string();
    s.stop();
    s.start();
    let (mut ca, _) = Conn::open(port, &a).await.unwrap();
    let got = ca.alert().await;
    assert_eq!(got.alert_id, k3);
    ca.ack(&k3, AckStage::Received).await;

    // Delivery status for producers.
    let (st, body) = http(
        s.admin_port,
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
    let (st, body) = http(
        s.admin_port,
        "POST",
        "/api/v1/webhooks/alertmanager/infra",
        &auth,
        &payload,
    );
    assert_eq!(st, 200, "{body}");
    let v: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(
        (v["created"].as_u64(), v["skipped"].as_u64()),
        (Some(1), Some(1))
    );
    let got = ca.alert().await;
    assert_eq!(got.title, "[FIRING] HighLatency: p99 > 2s");
    ca.ack(&got.alert_id, AckStage::Received).await;
    let (_, body) = http(
        s.admin_port,
        "POST",
        "/api/v1/webhooks/alertmanager/infra",
        &auth,
        &payload,
    );
    assert_eq!(
        serde_json::from_str::<Value>(&body).unwrap()["duplicates"],
        1
    );
    ca.no_alert(Duration::from_millis(2000)).await;

    // Admin: the pages of the tab Inbox need a session, the forms need the origin, the form type and
    // the anti-CSRF token. The JSON admin API of the first versions is gone.
    let ap = s.admin_port;
    let host = format!("127.0.0.1:{ap}");
    for path in ["/auth/settings/inbox", "/auth/settings/inbox/devices"] {
        let (st, head, _) = http_full(ap, &host, "GET", path, &[], "");
        assert_eq!(st, 303, "no session, no page");
        assert!(location(&head).starts_with("/auth/login"), "{head}");
    }
    let (st, _) = inbox_page(&s, "/auth/inbox/api/overview");
    assert_eq!(st, 404, "the JSON admin API is gone");
    let (st, v) = admin(&s, "GET", "/auth/inbox/api/overview", "");
    assert_eq!(st, 200, "{v}");
    assert_eq!(v["stats"]["enrolled"], 2);
    assert_eq!(v["stats"]["online"], 1);
    let devices = v["devices"].as_array().unwrap();
    let a_row = devices
        .iter()
        .find(|d| d["device"]["name"] == "phone-a")
        .unwrap();
    assert_eq!(a_row["status"], "online");
    assert_eq!(a_row["device"]["user_name"], "alice");
    let b_row = devices
        .iter()
        .find(|d| d["device"]["name"] == "phone-b")
        .unwrap();
    assert_eq!(b_row["device"]["user_name"], "bob");
    let session = s.session();
    let csrf = inbox_csrf(ap, &session.cookie);
    let send = format!("csrf={csrf}&group=other&title=manual&key=m1");
    let post = |extra: &[(&str, String)], body: &str| {
        let mut headers = vec![("Cookie", session.cookie.clone())];
        headers.extend_from_slice(extra);
        http_host(
            ap,
            &host,
            "POST",
            "/auth/settings/inbox/send",
            &headers,
            body,
        )
        .0
    };
    let origin = ("Origin", format!("http://{host}"));
    let form_type = (
        "Content-Type",
        "application/x-www-form-urlencoded".to_string(),
    );
    assert_eq!(
        post(std::slice::from_ref(&form_type), &send),
        403,
        "POST without Origin"
    );
    assert_eq!(
        post(
            &[form_type.clone(), ("Origin", "http://evil.example".into())],
            &send
        ),
        403,
        "POST from another origin"
    );
    assert_eq!(
        post(
            &[form_type.clone(), origin.clone()],
            "group=other&title=manual&key=m1"
        ),
        403,
        "POST without the anti-CSRF token"
    );
    assert_eq!(
        post(
            &[form_type.clone(), origin.clone()],
            &format!("csrf={}&group=other&title=manual&key=m1", "x".repeat(43))
        ),
        403,
        "POST with a wrong anti-CSRF token"
    );
    assert_eq!(
        post(
            &[("Content-Type", "text/plain".into()), origin.clone()],
            &send
        ),
        415,
        "POST without the form type"
    );
    let (st, head, _) = http_full(
        ap,
        &host,
        "POST",
        "/auth/settings/inbox/send",
        &[form_type.clone(), origin.clone()],
        &send,
    );
    assert_eq!(st, 303, "POST without session");
    assert!(location(&head).starts_with("/auth/login"), "{head}");
    let (st, v) = admin(
        &s,
        "POST",
        "/auth/inbox/api/alerts",
        &json!({"group": "other", "title": "manual", "idempotency_key": "m1"}).to_string(),
    );
    assert_eq!(st, 201, "{v}");
    let (st, page) = inbox_page(
        &s,
        &format!(
            "/auth/settings/inbox/alerts/{}",
            v["alert_id"].as_str().unwrap()
        ),
    );
    assert_eq!(st, 200);
    assert!(
        page.contains("0 phones"),
        "no phone subscribes to the group: {page}"
    );
    // The same key sends nothing a second time.
    let (st, _) = admin(
        &s,
        "POST",
        "/auth/inbox/api/alerts",
        &json!({"group": "other", "title": "manual", "idempotency_key": "m1"}).to_string(),
    );
    assert_eq!(st, 200, "the second send is a duplicate");
    let (st, v) = admin(
        &s,
        "POST",
        &format!("/auth/inbox/api/devices/{}/revoke", a.device_id),
        "{}",
    );
    assert_eq!((st, v["revoked"].as_bool()), (200, Some(true)));
    assert_eq!(
        ca.closed_code().await,
        Some(inbox_core::proto::close::REVOKED)
    );
    assert_eq!(
        Conn::open(port, &a).await.err(),
        Some(inbox_core::proto::close::REVOKED)
    );
    let (st, v) = admin(&s, "GET", "/auth/inbox/api/connections?limit=10", "");
    assert_eq!(st, 200);
    assert!(v["connections"].as_array().unwrap().len() >= 4);
    let (_, v) = admin(&s, "GET", "/auth/inbox/api/connections?user=alice", "");
    let mine = v["connections"].as_array().unwrap();
    assert!(
        !mine.is_empty() && mine.iter().all(|c| c["user_name"] == "alice"),
        "{v}"
    );
    let (_, v) = admin(&s, "GET", "/auth/inbox/api/connections?user=bob", "");
    assert!(v["connections"]
        .as_array()
        .unwrap()
        .iter()
        .all(|c| c["user_name"] == "bob"));
    assert!(
        !v["connections"].as_array().unwrap().is_empty(),
        "bob's later sessions are tracked"
    );
    let (_, v) = admin(&s, "GET", "/auth/inbox/api/alerts", "");
    let all = v["alerts"].as_array().unwrap().clone();
    assert!(all.len() >= 4);
    let (_, v) = admin(&s, "GET", "/auth/inbox/api/alerts?limit=2", "");
    assert_eq!(v["alerts"].as_array().unwrap().len(), 2);
    assert_eq!(v["alerts"][0]["id"], all[0]["id"], "newest first");
    // The page of one alert shows its deliveries.
    let (st, page) = inbox_page(
        &s,
        &format!(
            "/auth/settings/inbox/alerts/{}",
            all[0]["id"].as_str().unwrap()
        ),
    );
    assert_eq!(st, 200, "{page}");
    assert!(page.contains("Received"), "{page}");
    let (st, _) = admin(&s, "POST", "/auth/inbox/api/login/start", "{}");
    assert_eq!(st, 404, "no login endpoint any more");
    // The HTML front is gone: the listener of the phones serves no page.
    let (st, _) = http(s.port, "GET", "/", &[], "");
    assert_eq!(st, 404);

    // The metrics of the inbox are in the Prometheus text of the portal, with their old names.
    let (st, metrics) = http_host(
        s.admin_port,
        &format!("127.0.0.1:{}", s.admin_port),
        "GET",
        "/metrics",
        &[],
        "",
    );
    assert_eq!(st, 200);
    for expected in [
        "inbox_alerts_accepted_total{source=\"webhook\"} 1",
        "inbox_alerts_accepted_total{source=\"admin\"} 1",
        "inbox_alerts_duplicate_total{source=\"webhook\"} 1",
        "inbox_delivery_receipts_total ",
        "inbox_delivery_latency_seconds_count ",
        "inbox_deliveries_pending ",
        "inbox_oldest_pending_delivery_seconds ",
        "inbox_devices_unreachable ",
        "inbox_devices_enrolled ",
        "inbox_storage_degraded 0",
        "inbox_spool_alerts 0",
    ] {
        assert!(metrics.contains(expected), "{expected}\n{metrics}");
    }
    assert!(
        !metrics.contains("inbox_replica_epoch"),
        "no replica, no replica metrics"
    );

    // At SIGTERM the phones get the close code 1012 and reconnect elsewhere.
    let (mut last, _) = Conn::open(port, &b).await.unwrap();
    s.stop();
    assert_eq!(last.closed_code().await, Some(1012));
    assert!(!s.log().contains("panicked"), "{}", s.log());
    // SQLite is checkpointed and closed; with PostgreSQL the instance forgets itself and closes its sessions.
    let closed = if s.pg.is_some() {
        "\"event\":\"inbox_stopped\""
    } else {
        "\"event\":\"database_closed\""
    };
    assert!(s.log().contains(closed), "graceful shutdown: {}", s.log());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn enrollment_attempts_are_rate_limited() {
    let mut s = Server::setup();
    s.start();
    let unknown = unknown_code(&s);
    // max_auth_failures = 10: each started enrollment counts until it succeeds.
    for _ in 0..10 {
        assert!(matches!(
            enroll_code(&unknown, "x").await,
            Err(EnrollFail::Closed(Some(4410)))
        ));
    }
    // The next one is refused with 4429, also with a good code, and without message 2.
    let good = code_for(&s, SUPERADMIN);
    assert!(matches!(
        enroll_code(&good, "x").await,
        Err(EnrollFail::Closed(Some(4429)))
    ));
    assert!(
        s.log().contains("\"reason\":\"rate_limited\""),
        "the refusal is logged with its reason: {}",
        s.log()
    );
    s.stop();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn integrations_and_reverse_proxy_prefixes() {
    let mut s = Server::setup();
    // The proxy that sets the address of the client is a trusted proxy of the portal. The phone port uses the same list.
    let cfg = std::fs::read_to_string(s.config()).unwrap();
    std::fs::write(
        s.config(),
        cfg.replace(
            "  trusted_proxies = []",
            "  trusted_proxies = [\"127.0.0.1/32\"]",
        ),
    )
    .unwrap();
    s.start();
    let port = s.port;
    let dev = enroll_as(&s, "phone", SUPERADMIN).await.unwrap();
    let (mut c, _) = Conn::open(port, &dev).await.unwrap();
    c.subscribe("infra").await;

    // Generic notify: plain text body, token in the query string, no Idempotency-Key.
    let url = format!("/api/v1/notify/infra?token={}&title=Sauvegarde", s.token);
    let (st, body) = http(
        s.admin_port,
        "POST",
        &url,
        &[("Content-Type", "text/plain".into())],
        "backup KO\n",
    );
    assert_eq!(st, 201, "{body}");
    let got = c.alert().await;
    assert_eq!(got.title, "Sauvegarde");
    c.ack(&got.alert_id, AckStage::Received).await;
    // The same request retried within the minute is deduplicated.
    let (st, body) = http(
        s.admin_port,
        "POST",
        &url,
        &[("Content-Type", "text/plain".into())],
        "backup KO\n",
    );
    assert_eq!(st, 200, "{body}");
    assert!(body.contains("\"duplicate\":true"));

    // Without a title anywhere, the plain text body is the title, on one line.
    let auth = ("Authorization", format!("Bearer {}", s.token));
    let (st, _) = http(
        s.admin_port,
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
    let (st, _) = http(
        s.admin_port,
        "POST",
        "/api/v1/notify/infra",
        std::slice::from_ref(&auth),
        "",
    );
    assert_eq!(st, 422);

    // JSON with a title, bearer header.
    let (st, _) = http(
        s.admin_port,
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
        s.admin_port,
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
        s.admin_port,
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
    let (st, _) = http(
        s.admin_port,
        "POST",
        "/api/v1/notify/infra?token=nope",
        &[],
        "x",
    );
    assert_eq!(st, 401);
    let (st, _) = http(
        s.admin_port,
        "POST",
        &format!("/api/v1/notify/other?token={}", s.token),
        &[],
        "x",
    );
    assert_eq!(st, 403);

    // Titles are at most 128 characters: refused by the JSON API,
    // truncated by the generic webhook.
    let long = "x".repeat(129);
    let body = json!({"group": "infra", "title": long}).to_string();
    let (st, _) = http(
        s.admin_port,
        "POST",
        "/api/v1/alerts",
        std::slice::from_ref(&auth),
        &body,
    );
    assert_eq!(st, 413);
    let (st, _) = http(
        s.admin_port,
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
        s.admin_port,
        "POST",
        "/api/v1/alerts",
        std::slice::from_ref(&auth),
        r#"{"group":"infra","description":"old name"}"#,
    );
    assert_eq!(st, 400);

    // JSON API without Idempotency-Key: created, then deduplicated.
    let (st, _) = http(
        s.admin_port,
        "POST",
        "/api/v1/alerts",
        std::slice::from_ref(&auth),
        r#"{"group":"infra","title":"nokey"}"#,
    );
    assert_eq!(st, 201);
    let (st, _) = http(
        s.admin_port,
        "POST",
        "/api/v1/alerts",
        std::slice::from_ref(&auth),
        r#"{"group":"infra","title":"nokey"}"#,
    );
    assert_eq!(st, 200);

    // The pages of the inbox are not on the listener of the phones, and the portal needs a session for them.
    let (st, _) = http(port, "GET", "/auth/settings/inbox", &[], "");
    assert_eq!(st, 404);
    let (st, head, _) = http_full(
        s.admin_port,
        &format!("127.0.0.1:{}", s.admin_port),
        "GET",
        "/auth/settings/inbox",
        &[],
        "",
    );
    assert_eq!(st, 303);
    assert!(location(&head).starts_with("/auth/login"), "{head}");
    // The producer token does not open the pages.
    let (st, _, _) = http_full(
        s.admin_port,
        &format!("127.0.0.1:{}", s.admin_port),
        "GET",
        "/auth/settings/inbox",
        std::slice::from_ref(&auth),
        "",
    );
    assert_eq!(st, 303);
    // The alert of the call without a key is still waiting for its receipt.
    let got = c.alert().await;
    assert_eq!(got.title, "nokey");
    c.ack(&got.alert_id, AckStage::Received).await;
    // The form of the superadmin sends an alert to the group.
    let send = json!({"group": "infra", "title": "via admin", "idempotency_key": "p1"}).to_string();
    let (st, body) = admin(&s, "POST", "/auth/inbox/api/alerts", &send);
    assert_eq!(st, 201, "{body}");
    let got = c.alert().await;
    assert_eq!(got.title, "via admin");
    c.ack(&got.alert_id, AckStage::Received).await;
    s.stop();
}

/// The node running Inbox is lost (SIGKILL, local disk gone): a second instance with an empty local disk takes
/// over from the shared directory and knows every device and alert that was acknowledged.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_second_instance_takes_over_a_lost_node() {
    let mut s = Server::setup_with(None);
    let shared = s.dir.join("shared");
    let first_db = s.dir.join("portal.db");
    s.add_to_inbox(&format!(
        r#"  replica {{
    dir               = "{}"
    snapshot_interval = "1s"
    lease_ttl         = "5s"
    sync_timeout      = "5s"
    sync_alerts       = true
  }}"#,
        shared.display()
    ));
    s.start();
    let port = s.port;

    // Everything below is acknowledged, hence already in the shared directory.
    let used_code = code_for(&s, SUPERADMIN);
    let phone = enroll_code(&used_code, "phone-a").await.unwrap();
    // A code that nobody used yet: it is acknowledged too, so the standby must know it.
    let spare_code = code_for(&s, SUPERADMIN);
    let (st, accepted) = post_alert(
        &s,
        "failover-1",
        json!({"group": "infra", "title": "before the loss"}),
    );
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
        config.replace(
            &first_db.display().to_string(),
            &second_db.display().to_string(),
        ),
    )
    .unwrap();
    let began = Instant::now();
    s.start();
    assert!(
        began.elapsed() >= Duration::from_secs(3),
        "took over before the lease expired: {:?}",
        began.elapsed()
    );
    assert!(s.log().contains("replica_ready"), "{}", s.log());

    let (st, overview) = admin(&s, "GET", "/auth/inbox/api/overview", "");
    assert_eq!(st, 200, "{overview}");
    assert_eq!(
        overview["stats"]["enrolled"], 1,
        "the enrolled phone survived: {overview}"
    );
    let (_, alerts) = admin(&s, "GET", "/auth/inbox/api/alerts", "");
    assert!(
        alerts.to_string().contains(&alert_id),
        "the acknowledged alert survived: {alerts}"
    );
    // The code that the lost node saw used stays used. The code that it never saw used still works, once.
    assert!(matches!(
        enroll_code(&used_code, "phone-again").await,
        Err(EnrollFail::Closed(Some(4410)))
    ));
    let second = enroll_code(&spare_code, "phone-b").await.unwrap();
    assert!(matches!(
        enroll_code(&spare_code, "phone-c").await,
        Err(EnrollFail::Closed(Some(4410)))
    ));
    assert_ne!(second.device_id, phone.device_id);
    // The producer retries the alert it never got an answer for: deduplicated, not created twice.
    let (st, again) = post_alert(
        &s,
        "failover-1",
        json!({"group": "infra", "title": "before the loss"}),
    );
    assert_eq!(st, 200, "{again}");
    assert_eq!(again["alert_id"], alert_id.as_str());
    // The phone still enrolled on the first instance opens a session on the second one.
    let (_conn, welcome) = Conn::open(port, &phone).await.unwrap();
    assert!(matches!(welcome, ServerMsg::Welcome { .. }));
    assert!(
        shared.join("snapshot-2.db").exists(),
        "the new epoch publishes its own snapshot"
    );

    // A clean stop hands the lease over at once: no waiting for the ttl.
    s.stop();
    let began = Instant::now();
    s.start();
    assert!(
        began.elapsed() < Duration::from_secs(4),
        "a released lease was not taken at once: {:?}",
        began.elapsed()
    );
    let (_, overview) = admin(&s, "GET", "/auth/inbox/api/overview", "");
    assert_eq!(
        overview["stats"]["enrolled"], 2,
        "the phone that enrolled on the standby survived a clean restart: {overview}"
    );
    assert!(matches!(
        enroll_code(&spare_code, "phone-d").await,
        Err(EnrollFail::Closed(Some(4410)))
    ));
}

/// Without a `replica` block nothing changes: no shared directory is needed or touched.
#[test]
fn check_config_reports_the_replica_block() {
    // The block `replica` is for SQLite: with PostgreSQL the same file is an error.
    if postgres_backend() {
        return;
    }
    let s = Server::setup();
    let out = portal(&["check-config", "--config", &s.config()], None);
    let shown = String::from_utf8_lossy(&out.stdout).to_string();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(shown.contains("inbox.devices.listen"), "{shown}");
    assert!(
        shown.contains("inbox.producer.ci.token = <hidden>  # file:"),
        "{shown}"
    );
    assert!(
        !shown.contains("inbox.replica."),
        "no replica without the block: {shown}"
    );
    assert!(shown.contains("configuration ok"), "{shown}");
    let broken = s.dir.join("replica.hcl");
    let text = std::fs::read_to_string(s.config()).unwrap();
    let replica = "  replica {\n    dir = \"/x\"\n    snapshot_interval = \"1s\"\n    lease_ttl = \"1s\"\n    sync_timeout = \"1s\"\n    sync_alerts = true\n  }\n  # inbox-extra\n";
    std::fs::write(&broken, text.replace("  # inbox-extra\n", replica)).unwrap();
    let out = portal(
        &["check-config", "--config", broken.to_str().unwrap()],
        None,
    );
    assert_eq!(out.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&out.stderr).contains("lease_ttl"));
    // With a valid replica block the effective configuration shows the keys, with their defaults.
    let good = s.dir.join("replica-ok.hcl");
    let replica = "  replica {\n    dir = \"/x\"\n  }\n  # inbox-extra\n";
    std::fs::write(&good, text.replace("  # inbox-extra\n", replica)).unwrap();
    let out = portal(&["check-config", "--config", good.to_str().unwrap()], None);
    let shown = String::from_utf8_lossy(&out.stdout).to_string();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        shown.contains("inbox.replica.dir = \"/x\"  # file"),
        "{shown}"
    );
    assert!(
        shown.contains("inbox.replica.lease_ttl = \"30s\"  # default"),
        "{shown}"
    );
    assert!(
        shown.contains("inbox.replica.sync_alerts = true  # default"),
        "{shown}"
    );
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
        .replace(
            "unreachable_after = \"10s\"",
            "unreachable_after = \"120s\"",
        );
    std::fs::write(a.config(), text).unwrap();
    let mut b = a.sibling("instance-b");
    a.start();
    b.start();

    // A phone enrolled through A opens its session on B.
    let phone = enroll_as(&a, "phone-a", SUPERADMIN).await.unwrap();
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
        let (st, overview) = admin(s, "GET", "/auth/inbox/api/overview", "");
        assert_eq!(st, 200, "{overview}");
        assert_eq!(overview["stats"]["online"], 1, "{overview}");
        assert_eq!(overview["stats"]["enrolled"], 1);
    }
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let (_, alerts) = admin(&a, "GET", "/auth/inbox/api/alerts", "");
        if alerts["alerts"][0]["received"] == 1 {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the receipt recorded on B never showed on A: {alerts}"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    // A device has one session: opening another one through A closes the one held by B.
    let (mut on_a, _) = Conn::open(a.port, &phone).await.unwrap();
    assert_eq!(on_b.closed_code().await, Some(close::REPLACED));

    // Revoking through B closes the session held by A.
    let (st, _) = admin(
        &b,
        "POST",
        &format!("/auth/inbox/api/devices/{}/revoke", phone.device_id),
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
                    post_alert(
                        target,
                        "race-1",
                        json!({"group": "infra", "title": "same alert"}),
                    )
                })
            })
            .collect();
        tasks.into_iter().map(|t| t.join().unwrap()).collect()
    });
    assert_eq!(
        results.iter().filter(|(st, _)| *st == 201).count(),
        1,
        "{results:?}"
    );
    assert!(
        results.iter().all(|(st, _)| *st == 201 || *st == 200),
        "{results:?}"
    );
    let ids: std::collections::HashSet<_> = results
        .iter()
        .map(|(_, v)| v["alert_id"].to_string())
        .collect();
    assert_eq!(
        ids.len(),
        1,
        "one alert id for one idempotency key: {results:?}"
    );

    // A code made on A works one time, on either instance.
    let code = code_for(&a, SUPERADMIN);
    enroll_code_at(Some(b.port), &code, "phone-c")
        .await
        .unwrap();
    assert!(matches!(
        enroll_code_at(Some(a.port), &code, "phone-d").await,
        Err(EnrollFail::Closed(Some(4410)))
    ));
    // Two instances get the same code at the same moment: exactly one phone enrolls.
    let code = code_for(&b, SUPERADMIN);
    let (first, second) = tokio::join!(
        enroll_code_at(Some(a.port), &code, "race-a"),
        enroll_code_at(Some(b.port), &code, "race-b")
    );
    assert_eq!(
        [&first, &second].iter().filter(|r| r.is_ok()).count(),
        1,
        "{first:?} {second:?}"
    );
    // The account of a phone goes away on A: the session that B holds closes, and no phone of the account comes back.
    let carol = enroll_as(&a, "phone-carol", "carol").await.unwrap();
    let spare = code_for(&b, "carol");
    let (mut carol_on_b, _) = Conn::open(b.port, &carol).await.unwrap();
    delete_account(&a, "carol");
    assert_eq!(carol_on_b.closed_code().await, Some(close::REVOKED));
    assert_eq!(Conn::open(a.port, &carol).await.err(), Some(close::REVOKED));
    assert_eq!(Conn::open(b.port, &carol).await.err(), Some(close::REVOKED));
    assert!(matches!(
        enroll_code_at(Some(b.port), &spare, "phone-carol-2").await,
        Err(EnrollFail::Closed(Some(4410)))
    ));

    // The node of A is lost: B keeps accepting everything.
    let mut lost = a.child.take().unwrap();
    lost.kill().unwrap();
    lost.wait().unwrap();
    let other = enroll_as(&b, "phone-b", SUPERADMIN).await.unwrap();
    let (mut other_conn, _) = Conn::open(b.port, &other).await.unwrap();
    other_conn.subscribe("infra").await;
    let (st, second) = post_alert(
        &b,
        "shared-2",
        json!({"group": "infra", "title": "after the loss"}),
    );
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
        .replace(
            "unreachable_after = \"10s\"",
            "unreachable_after = \"120s\"",
        );
    std::fs::write(a.config(), text).unwrap();
    let mut b = a.sibling("instance-b");
    a.start();
    b.start();
    let phone = enroll_as(&a, "phone-a", SUPERADMIN).await.unwrap();
    let (_conn, _) = Conn::open(a.port, &phone).await.unwrap();
    let (_, overview) = admin(&b, "GET", "/auth/inbox/api/overview", "");
    assert_eq!(overview["stats"]["online"], 1, "{overview}");

    let mut lost = a.child.take().unwrap();
    lost.kill().unwrap();
    lost.wait().unwrap();
    // Nobody tells B: the heartbeat stops, then the janitor closes the sessions (30 s of silence).
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        let (_, overview) = admin(&b, "GET", "/auth/inbox/api/overview", "");
        if overview["stats"]["online"] == 0 {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the dead instance still holds a session: {overview}"
        );
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    // The sessions stop counting as live after 30 s, the janitor of B then writes their end within one heartbeat.
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        let (_, connections) = admin(&b, "GET", "/auth/inbox/api/connections", "");
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
    let used_code = code_for(&s, SUPERADMIN);
    let phone = enroll_code(&used_code, "phone-a").await.unwrap();
    // A code that nobody used yet moves with the data.
    let spare_code = code_for(&s, SUPERADMIN);
    {
        let (mut conn, _) = Conn::open(s.port, &phone).await.unwrap();
        conn.subscribe("infra").await;
        let (st, _) = post_alert(
            &s,
            "mig-1",
            json!({"group": "infra", "title": "received before"}),
        );
        assert_eq!(st, 201);
        let alert = conn.alert().await;
        conn.ack(&alert.alert_id, AckStage::Received).await;
        tokio::time::sleep(Duration::from_millis(300)).await;
    } // the phone goes offline
    let (st, _) = post_alert(
        &s,
        "mig-2",
        json!({"group": "infra", "title": "waiting for the phone"}),
    );
    assert_eq!(st, 201);
    s.stop();

    // The same configuration, on PostgreSQL.
    let database = format!("inbox_e2e_{}", free_port());
    admin_pg.create_database(&database);
    std::fs::write(s.dir.join("db_password"), &admin_pg.password).unwrap();
    let sqlite_file = s.dir.join("portal.db");
    let text = std::fs::read_to_string(s.config()).unwrap().replace(
        &format!("storage {{ database = \"{}\" }}", sqlite_file.display()),
        "",
    );
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
    let (cfg, from) = (
        pg_config.to_str().unwrap().to_string(),
        sqlite_file.to_str().unwrap().to_string(),
    );
    let out = portal(
        &["inbox", "import-sqlite", "--config", &cfg, "--from", &from],
        None,
    );
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let report = String::from_utf8_lossy(&out.stdout).to_string();
    assert!(
        report.contains("1 devices")
            && report.contains("2 alerts")
            && report.contains("2 enrollment codes"),
        "{report}"
    );
    // Importing twice would duplicate everything: refused.
    let again = portal(
        &["inbox", "import-sqlite", "--config", &cfg, "--from", &from],
        None,
    );
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
        session: Default::default(),
        accounts: Default::default(),
    };
    moved.start();
    let (_, overview) = admin(&moved, "GET", "/auth/inbox/api/overview", "");
    assert_eq!(overview["stats"]["enrolled"], 1, "{overview}");
    assert_eq!(
        overview["devices"][0]["device"]["subscriptions"][0],
        "infra"
    );
    let (_, alerts) = admin(&moved, "GET", "/auth/inbox/api/alerts", "");
    assert_eq!(alerts["alerts"].as_array().unwrap().len(), 2, "{alerts}");
    // The used code stays used. The other code works one time.
    assert!(matches!(
        enroll_code(&used_code, "phone-again").await,
        Err(EnrollFail::Closed(Some(4410)))
    ));
    enroll_code(&spare_code, "phone-b").await.unwrap();
    assert!(matches!(
        enroll_code(&spare_code, "phone-c").await,
        Err(EnrollFail::Closed(Some(4410)))
    ));
    // The phone did not enroll again: the same keys open a session, and the alert it never received arrives.
    let (mut conn, _) = Conn::open(moved.port, &phone).await.unwrap();
    let waiting = conn.alert().await;
    assert_eq!(waiting.title, "waiting for the phone");
    // New alerts keep working after the imported sequences.
    let (st, _) = post_alert(
        &moved,
        "mig-3",
        json!({"group": "infra", "title": "after the move"}),
    );
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

// ---------------------------------------------------------------- degraded mode (PostgreSQL away)

/// A TCP relay between Inbox and PostgreSQL that can play a lost node: `blackhole` keeps accepting and swallows every byte
/// (the primary of a lost node answers nothing), `pass` forwards again, for new connections only: the ones that were open
/// during the outage stay dead, as they would be after a failover.
#[derive(Clone)]
struct PgProxy {
    port: u16,
    state: std::sync::Arc<ProxyState>,
}

struct ProxyState {
    /// Bumped at every blackhole: a connection made before it never carries data again.
    epoch: std::sync::atomic::AtomicU64,
    pass: std::sync::atomic::AtomicBool,
}

impl PgProxy {
    fn start(target_host: &str, target_port: u16) -> PgProxy {
        use std::sync::atomic::Ordering::SeqCst;
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let state = std::sync::Arc::new(ProxyState {
            epoch: std::sync::atomic::AtomicU64::new(0),
            pass: std::sync::atomic::AtomicBool::new(true),
        });
        let (host, shared) = (target_host.to_string(), state.clone());
        std::thread::spawn(move || {
            for client in listener.incoming().flatten() {
                let epoch = shared.epoch.load(SeqCst);
                let Ok(server) = TcpStream::connect((host.as_str(), target_port)) else {
                    continue;
                };
                for (from, to) in [
                    (client.try_clone().unwrap(), server.try_clone().unwrap()),
                    (server, client),
                ] {
                    let shared = shared.clone();
                    std::thread::spawn(move || {
                        let (mut from, mut to) = (from, to);
                        let mut buf = [0u8; 8192];
                        while let Ok(n) = from.read(&mut buf) {
                            if n == 0 {
                                break;
                            }
                            let open =
                                shared.pass.load(SeqCst) && shared.epoch.load(SeqCst) == epoch;
                            if open && to.write_all(&buf[..n]).is_err() {
                                break;
                            }
                        }
                        let _ = to.shutdown(std::net::Shutdown::Both);
                    });
                }
            }
        });
        PgProxy { port, state }
    }

    fn blackhole(&self) {
        use std::sync::atomic::Ordering::SeqCst;
        self.state.epoch.fetch_add(1, SeqCst);
        self.state.pass.store(false, SeqCst);
    }

    fn pass(&self) {
        self.state
            .pass
            .store(true, std::sync::atomic::Ordering::SeqCst);
    }
}

/// Points the PostgreSQL block of a server at `to` instead of the database.
fn through(s: &Server, admin_pg: &PgAdmin, to: u16) {
    let text = std::fs::read_to_string(s.config()).unwrap();
    let from = format!("port = {}\n", admin_pg.port);
    assert!(
        text.contains(&from),
        "no postgres port in the configuration"
    );
    // The proxy listens on this machine, whatever the host of the real database is.
    let host = format!("host = \"{}\"\n", admin_pg.host);
    let text = text.replace(&host, "host = \"127.0.0.1\"\n");
    std::fs::write(s.config(), text.replace(&from, &format!("port = {to}\n"))).unwrap();
}

/// Adds the `cluster` block: this instance listens on `listen` and looks for peers at `peers`.
fn with_cluster(s: &Server, listen: u16, peers: &[u16]) {
    let list: Vec<String> = peers.iter().map(|p| format!("\"127.0.0.1:{p}\"")).collect();
    s.add_to_inbox(&format!(
        "  cluster {{\n    listen = \"127.0.0.1:{listen}\"\n    peers = [{}]\n  }}",
        list.join(", ")
    ));
}

/// A long session timeout: the test phones send no pings.
fn quiet_phones(s: &Server) {
    let text = std::fs::read_to_string(s.config())
        .unwrap()
        .replace("resend_after = \"1s\"", "resend_after = \"30s\"")
        .replace("session_timeout = \"10s\"", "session_timeout = \"120s\"")
        .replace(
            "unreachable_after = \"10s\"",
            "unreachable_after = \"120s\"",
        );
    std::fs::write(s.config(), text).unwrap();
}

fn wait_for(what: &str, limit: Duration, mut done: impl FnMut() -> bool) {
    let deadline = Instant::now() + limit;
    while !done() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn stored_alerts(s: &Server) -> Vec<Value> {
    let (admin, database) = s.pg.as_ref().expect("a server on PostgreSQL");
    admin.alerts(database)
}

/// PostgreSQL stops answering (a lost node): the instance stays ready, accepts alerts at once, delivers them to the phone
/// that is connected, keeps the session, and writes everything to the database when it is back, once.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn alerts_are_accepted_and_delivered_while_postgresql_is_away() {
    let Some(admin_pg) = pg_admin() else {
        eprintln!("INBOX_TEST_PG is not set: skipped");
        return;
    };
    let proxy = PgProxy::start(&admin_pg.host, admin_pg.port);
    let mut a = Server::setup_with(Some(admin_pg.clone()));
    through(&a, &admin_pg, proxy.port);
    quiet_phones(&a);
    a.start();
    let phone = enroll_as(&a, "phone", SUPERADMIN).await.unwrap();
    let (mut conn, _) = Conn::open(a.port, &phone).await.unwrap();
    conn.subscribe("infra").await;
    let (st, ready) = http(a.port, "GET", "/readyz", &[], "");
    assert_eq!((st, ready.as_str()), (200, "ready"));

    // The node of the primary is lost. The first alert may wait for the database to give up; the next ones do not.
    proxy.blackhole();
    let began = Instant::now();
    let (st, first) = post_alert(
        &a,
        "away-1",
        json!({"group": "infra", "title": "first, database away"}),
    );
    assert_eq!(st, 201, "{first}");
    assert!(
        began.elapsed() < Duration::from_secs(4),
        "first alert took {:?}",
        began.elapsed()
    );
    let began = Instant::now();
    let (st, second) = post_alert(
        &a,
        "away-2",
        json!({"group": "infra", "title": "second, database away"}),
    );
    assert_eq!(st, 201, "{second}");
    assert!(
        began.elapsed() < Duration::from_secs(1),
        "second alert took {:?}",
        began.elapsed()
    );
    assert_eq!(first["targets"], 1, "{first}");
    assert_ne!(first["alert_id"], second["alert_id"]);

    // A producer's retry finds the alert it sent: same id, a duplicate.
    let (st, again) = post_alert(
        &a,
        "away-1",
        json!({"group": "infra", "title": "first, database away"}),
    );
    assert_eq!(st, 200, "{again}");
    assert_eq!(again["alert_id"], first["alert_id"]);
    let (st, clash) = post_alert(
        &a,
        "away-1",
        json!({"group": "infra", "title": "another alert"}),
    );
    assert_eq!(st, 409, "{clash}");

    // The instance stays in the service, and the phone, which never lost its session, has both alerts.
    let (st, ready) = http(a.port, "GET", "/readyz", &[], "");
    assert_eq!((st, ready.as_str()), (200, "degraded"));
    let one = conn.alert().await;
    let two = conn.alert().await;
    let mut titles = vec![one.title.clone(), two.title.clone()];
    titles.sort();
    assert_eq!(titles, ["first, database away", "second, database away"]);
    conn.ack(&one.alert_id, AckStage::Received).await;
    conn.ack(&two.alert_id, AckStage::Received).await;
    // What needs the database is refused, and the session goes on.
    conn.send(&ClientMsg::Unsubscribe {
        group: "infra".into(),
    })
    .await;
    loop {
        match conn.recv(Duration::from_secs(5)).await {
            Recv::Msg(ServerMsg::Error { code, .. }) => {
                assert_eq!(code, "unavailable");
                break;
            }
            Recv::Msg(_) => continue,
            other => panic!(
                "expected an error, got {:?}",
                matches!(other, Recv::Timeout)
            ),
        }
    }

    // The database is back: both alerts are written, once, with their receipts, and nothing is sent again.
    proxy.pass();
    wait_for(
        "the alerts to reach the database",
        Duration::from_secs(70),
        || stored_alerts(&a).len() == 2,
    );
    wait_for(
        "the receipts to reach the database",
        Duration::from_secs(40),
        || stored_alerts(&a).iter().all(|x| x["received"] == 1),
    );
    let stored = stored_alerts(&a);
    let mut ids: Vec<String> = stored
        .iter()
        .map(|x| x["id"].as_str().unwrap().to_string())
        .collect();
    ids.sort();
    let mut sent = vec![
        first["alert_id"].as_str().unwrap().to_string(),
        second["alert_id"].as_str().unwrap().to_string(),
    ];
    sent.sort();
    assert_eq!(ids, sent, "the database holds the ids the phone was given");
    conn.no_alert(Duration::from_secs(2)).await;
    let (st, ready) = http(a.port, "GET", "/readyz", &[], "");
    assert_eq!((st, ready.as_str()), (200, "ready"));
    // Back to normal: an alert goes through the database again.
    let (st, normal) = post_alert(&a, "back-1", json!({"group": "infra", "title": "back"}));
    assert_eq!(st, 201, "{normal}");
    assert_eq!(conn.alert().await.title, "back");
    let log = a.log();
    for event in [
        "storage_degraded",
        "alert_spooled",
        "storage_recovered",
        "spool_replayed",
    ] {
        assert!(log.contains(event), "no {event} in the log:\n{log}");
    }
}

/// Two instances, the database away: an alert accepted by one reaches the phone held by the other before the producer is
/// answered, the same alert sent to both is one alert, and the database ends with one row per alert.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn instances_copy_alerts_to_each_other_while_postgresql_is_away() {
    let Some(admin_pg) = pg_admin() else {
        eprintln!("INBOX_TEST_PG is not set: skipped");
        return;
    };
    let proxy = PgProxy::start(&admin_pg.host, admin_pg.port);
    let mut a = Server::setup_with(Some(admin_pg.clone()));
    through(&a, &admin_pg, proxy.port);
    quiet_phones(&a);
    let mut b = a.sibling("instance-b");
    let (cluster_a, cluster_b) = (free_port(), free_port());
    with_cluster(&a, cluster_a, &[cluster_a, cluster_b]);
    with_cluster(&b, cluster_b, &[cluster_a, cluster_b]);
    a.start();
    b.start();
    wait_for(
        "the instances to find each other",
        Duration::from_secs(10),
        || a.log().contains("peers_changed") && b.log().contains("peers_changed"),
    );

    // The phone is enrolled through A and holds its session on B; A learns of it from the database, every 5 s.
    let phone = enroll_as(&a, "phone", SUPERADMIN).await.unwrap();
    let (mut on_b, _) = Conn::open(b.port, &phone).await.unwrap();
    on_b.subscribe("infra").await;
    tokio::time::sleep(Duration::from_secs(6)).await;

    proxy.blackhole();
    // The first call of each instance waits for the database to give up, the others do not.
    let (st, first) = post_alert(
        &a,
        "pair-1",
        json!({"group": "infra", "title": "accepted by a"}),
    );
    assert_eq!(st, 201, "{first}");
    let alert = on_b.alert().await;
    assert_eq!(alert.title, "accepted by a");
    assert_eq!(alert.alert_id, first["alert_id"].as_str().unwrap());
    on_b.ack(&alert.alert_id, AckStage::Received).await;

    // The same alert, sent to the other instance, is the same alert.
    let (st, same) = post_alert(
        &b,
        "pair-1",
        json!({"group": "infra", "title": "accepted by a"}),
    );
    assert_eq!(st, 200, "{same}");
    assert_eq!(same["alert_id"], first["alert_id"]);
    // And an alert both accept at the same time is one alert for the phone.
    let results: Vec<(u16, Value)> = std::thread::scope(|scope| {
        let tasks: Vec<_> = (0..6)
            .map(|i| {
                let target = if i % 2 == 0 { &a } else { &b };
                scope.spawn(move || {
                    post_alert(
                        target,
                        "pair-2",
                        json!({"group": "infra", "title": "accepted by both"}),
                    )
                })
            })
            .collect();
        tasks.into_iter().map(|t| t.join().unwrap()).collect()
    });
    assert!(
        results.iter().all(|(st, _)| *st == 201 || *st == 200),
        "{results:?}"
    );
    let ids: std::collections::HashSet<_> = results
        .iter()
        .map(|(_, v)| v["alert_id"].to_string())
        .collect();
    assert_eq!(ids.len(), 1, "one id for one idempotency key: {results:?}");
    let both = on_b.alert().await;
    assert_eq!(both.title, "accepted by both");
    on_b.ack(&both.alert_id, AckStage::Received).await;
    on_b.no_alert(Duration::from_secs(1)).await;

    // The database is back: both instances write what they hold, the database keeps one row per alert and the receipts.
    proxy.pass();
    wait_for(
        "the alerts to reach the database",
        Duration::from_secs(70),
        || stored_alerts(&a).len() == 2,
    );
    wait_for(
        "the receipts to reach the database",
        Duration::from_secs(40),
        || {
            let alerts = stored_alerts(&a);
            alerts.len() == 2 && alerts.iter().all(|x| x["received"] == 1)
        },
    );
    on_b.no_alert(Duration::from_secs(3)).await;
    assert_eq!(stored_alerts(&a).len(), 2);
    // The copy to the other instance worked every time: the alerts were held by two instances before the 201.
    let (log_a, log_b) = (a.log(), b.log());
    assert!(log_a.contains("alert_spooled"), "{log_a}");
    assert!(!log_a.contains("spool_unreplicated") && !log_b.contains("spool_unreplicated"));
    assert!(log_a.contains("spool_replayed") && log_b.contains("spool_replayed"));
}

/// The peer listener answers only requests signed with the key derived from the server key, and not old ones.
#[test]
fn the_peer_port_refuses_requests_that_are_not_signed() {
    use hmac::{Hmac, Mac};
    use sha2::{Digest, Sha256};
    let Some(admin_pg) = pg_admin() else {
        eprintln!("INBOX_TEST_PG is not set: skipped");
        return;
    };
    let mut a = Server::setup_with(Some(admin_pg));
    let cluster = free_port();
    with_cluster(&a, cluster, &[cluster]);
    a.start();
    let key = inbox_core::server_key::ServerKey::decode(
        std::fs::read_to_string(a.dir.join("server_key"))
            .unwrap()
            .trim(),
    )
    .unwrap();
    let mut h = Sha256::new();
    h.update(b"inbox-peer-v1\0");
    h.update(&*key.noise_private);
    let peer_key: [u8; 32] = h.finalize().into();
    let sign = |ts: i64, path: &str, body: &str| {
        let mut m = <Hmac<Sha256> as Mac>::new_from_slice(&peer_key).unwrap();
        m.update(format!("{ts}\n{path}\n{body}").as_bytes());
        m.finalize()
            .into_bytes()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>()
    };
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64;
    let hello = "/internal/v1/hello";
    let call = |ts: i64, sig: String| {
        http(
            cluster,
            "GET",
            hello,
            &[("X-Peer-Time", ts.to_string()), ("X-Peer-Auth", sig)],
            "",
        )
    };
    assert_eq!(http(cluster, "GET", hello, &[], "").0, 401, "no signature");
    assert_eq!(call(now, "00".repeat(32)).0, 401, "wrong signature");
    assert_eq!(
        call(now, sign(now, "/internal/v1/spool", "")).0,
        401,
        "signed for another path"
    );
    let old = now - 120_000;
    assert_eq!(call(old, sign(old, hello, "")).0, 401, "an old request");
    let (st, body) = call(now, sign(now, hello, ""));
    assert_eq!(st, 200, "{body}");
    assert!(body.contains("\"instance\""), "{body}");
    // The device port does not serve the peer API.
    assert_eq!(http(a.port, "GET", hello, &[], "").0, 404);
}

// ---------------------------------------------------------------- protocol 4: enrollment codes

/// Sets the end of every code that is not used to the past: the time travel of the tests. The
/// SQLite file is the file of the server. With PostgreSQL the call goes through the admin connection.
fn expire_codes(s: &Server) {
    let sql = "UPDATE enrollment_codes SET expires_at_ms = 1 WHERE used_at_ms IS NULL";
    match &s.pg {
        Some((admin, database)) => admin.exec(database, sql.to_string()),
        None => {
            let conn = rusqlite::Connection::open(s.dir.join("portal.db")).unwrap();
            conn.busy_timeout(Duration::from_secs(10)).unwrap();
            conn.execute(sql, []).unwrap();
        }
    }
}

/// The first message of an enrollment with a chosen protocol number and a fresh device key.
fn enroll_message(code: &EnrollCode, protocol: u32) -> Vec<u8> {
    let keys = inbox_core::noise::generate_keypair().unwrap();
    let hello = serde_json::to_vec(&inbox_core::proto::EnrollHello {
        protocol,
        secret: code.secret.to_vec(),
        device_name: "e2e".into(),
        app_version: "e2e".into(),
    })
    .unwrap();
    Initiator::start_with_prologue(
        inbox_core::noise::ENROLL_PROLOGUE,
        &keys.private,
        &code.server_public_key,
        &hello,
    )
    .unwrap()
    .1
}

/// Opens `/v1/enroll`, sends one frame and returns the close code. The server sends no message 2.
async fn enroll_with_frame(port: u16, frame: Message) -> Option<u16> {
    let (mut ws, _) = tokio_tungstenite::connect_async(format!("ws://127.0.0.1:{port}/v1/enroll"))
        .await
        .unwrap();
    ws.send(frame).await.unwrap();
    close_code(&mut ws).await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_code_enrolls_one_phone_for_the_account_that_made_it() {
    let mut s = Server::setup();
    s.start();
    let (admin_port, host) = (s.admin_port, format!("127.0.0.1:{}", s.admin_port));

    // The rules of the form: a session, a POST with the origin, the form type and the token, no other field.
    let session = s.session();
    let origin = ("Origin", format!("http://{host}"));
    let form_type = (
        "Content-Type",
        "application/x-www-form-urlencoded".to_string(),
    );
    let csrf = inbox_csrf(admin_port, &session.cookie);
    let call = |method: &str, headers: &[(&str, String)], body: &str| {
        http_host(
            admin_port,
            &host,
            method,
            "/auth/settings/inbox/code",
            headers,
            body,
        )
        .0
    };
    let cookie = ("Cookie", session.cookie.clone());
    let token_body = format!("csrf={csrf}");
    assert_eq!(
        call("POST", &[form_type.clone(), origin.clone()], &token_body),
        303,
        "no session: the page goes to the sign-in"
    );
    assert_eq!(call("GET", std::slice::from_ref(&cookie), ""), 405);
    assert_eq!(
        call("POST", &[cookie.clone(), form_type.clone()], &token_body),
        403,
        "no origin"
    );
    assert_eq!(
        call(
            "POST",
            &[cookie.clone(), form_type.clone(), origin.clone()],
            ""
        ),
        403,
        "no token"
    );
    assert_eq!(
        call(
            "POST",
            &[cookie.clone(), form_type.clone(), origin.clone()],
            &format!("csrf={}", "x".repeat(43))
        ),
        403,
        "wrong token"
    );
    assert_eq!(
        call(
            "POST",
            &[
                cookie.clone(),
                ("Content-Type", "text/plain".into()),
                origin.clone()
            ],
            &token_body
        ),
        415
    );
    assert_eq!(
        call(
            "POST",
            &[cookie.clone(), form_type.clone(), origin.clone()],
            &format!("{token_body}&login=bob")
        ),
        400,
        "a code is for the login of the session only"
    );

    // The answer: the text of the code, its end and the fingerprint of the key.
    let before = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64;
    let (st, v) = make_code(&s, SUPERADMIN);
    assert_eq!(st, 200, "{v}");
    let text = v["code"].as_str().unwrap().to_string();
    let parsed = EnrollCode::parse(&text).unwrap();
    assert_eq!(parsed.address, format!("127.0.0.1:{}", s.port));
    assert_eq!(v["fingerprint"].as_str().unwrap(), parsed.fingerprint());
    assert_eq!(v["fingerprint"].as_str().unwrap().len(), 8);
    let ends = v["expires_at_ms"].as_i64().unwrap();
    assert!(
        ends >= before + 9 * 60_000 && ends <= before + 11 * 60_000,
        "the code lives 10 minutes: {v}"
    );

    // Wrong protocol: 4426, and the code is not used. A text frame: 4426 as well.
    assert_eq!(
        enroll_with_frame(s.port, Message::Binary(enroll_message(&parsed, 3).into())).await,
        Some(4426)
    );
    assert_eq!(
        enroll_with_frame(s.port, Message::Text("hello".into())).await,
        Some(4426)
    );
    // Junk that is not a Noise message: 4410.
    assert_eq!(
        enroll_with_frame(s.port, Message::Binary(vec![7u8; 100].into())).await,
        Some(4410)
    );
    // A pinned key that is not the key of the server: the server cannot read message 1, 4410.
    let mut wrong = EnrollCode::parse(&text).unwrap();
    wrong.server_public_key = inbox_core::noise::generate_keypair()
        .unwrap()
        .public
        .try_into()
        .unwrap();
    assert!(matches!(
        enroll_code(&wrong.encode(), "pinned").await,
        Err(EnrollFail::Closed(Some(4410)))
    ));

    // The code still works: the refusals above did not use it. It works one time.
    let phone = enroll_code(&text, "phone-root").await.unwrap();
    assert_eq!(phone.user_name, SUPERADMIN);
    assert_eq!(phone.server_name, "E2E");
    assert_eq!(phone.server_public_key, parsed.server_public_key);
    assert_eq!(phone.device_id.len(), 32);
    let (_, welcome) = Conn::open(s.port, &phone).await.unwrap();
    assert!(matches!(welcome, ServerMsg::Welcome { ref user_name, .. } if user_name == SUPERADMIN));
    assert!(matches!(
        enroll_code(&text, "phone-twice").await,
        Err(EnrollFail::Closed(Some(4410)))
    ));

    // A code is for the account that made it: the login is the user name of the device.
    let bob = enroll_as(&s, "phone-bob", "bob").await.unwrap();
    assert_eq!(bob.user_name, "bob");
    let (_, welcome) = Conn::open(s.port, &bob).await.unwrap();
    assert!(matches!(welcome, ServerMsg::Welcome { ref user_name, .. } if user_name == "bob"));
    let (_, overview) = admin(&s, "GET", "/auth/inbox/api/overview", "");
    let users: Vec<&str> = overview["devices"]
        .as_array()
        .unwrap()
        .iter()
        .map(|d| d["device"]["user_name"].as_str().unwrap())
        .collect();
    assert_eq!(users, ["bob", "root"]);
    // A plain account with a right to subscribe has the page of its phones and no admin page.
    let bob_session = s.account("bob");
    let (st, _) = inbox_page_as(&s, &bob_session.cookie, "/auth/settings/inbox/phones");
    assert_eq!(st, 200);
    for path in [
        "/auth/settings/inbox/devices",
        "/auth/settings/inbox/connections",
    ] {
        let (st, _) = inbox_page_as(&s, &bob_session.cookie, path);
        assert_eq!(st, 403, "{path}");
    }

    // The secret is not in the log. The database keeps its SHA-256 digest only.
    let secret = inbox_core::b64::encode_url(&parsed.secret);
    assert!(!s.log().contains(&secret), "the secret is in the log");
    assert!(
        s.log().contains("\"event\":\"enroll_refused\""),
        "{}",
        s.log()
    );
    assert!(s.log().contains("\"reason\":\"used_code\""), "{}", s.log());
    assert!(
        s.log().contains("\"reason\":\"unsupported_protocol\""),
        "{}",
        s.log()
    );
    // The unit tests of the PostgreSQL store look at the digest there.
    if s.pg.is_none() {
        let conn = rusqlite::Connection::open(s.dir.join("portal.db")).unwrap();
        let digests: Vec<Vec<u8>> = conn
            .prepare("SELECT digest FROM enrollment_codes WHERE used_at_ms IS NOT NULL")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert_eq!(digests.len(), 2, "two codes were used");
        assert!(digests
            .iter()
            .all(|d| d.len() == 32 && d.as_slice() != parsed.secret.as_slice()));
    }
    s.stop();
}

/// The server closes an enrollment connection that sends no message 1 within 10 seconds.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_silent_enrollment_connection_is_closed_after_ten_seconds() {
    let mut s = Server::setup();
    s.start();
    let (mut ws, _) =
        tokio_tungstenite::connect_async(format!("ws://127.0.0.1:{}/v1/enroll", s.port))
            .await
            .unwrap();
    let began = Instant::now();
    assert_eq!(close_code(&mut ws).await, Some(4410));
    let waited = began.elapsed();
    assert!(
        waited >= Duration::from_secs(9) && waited < Duration::from_secs(14),
        "{waited:?}"
    );
    s.stop();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_expired_code_is_refused() {
    let mut s = Server::setup();
    s.start();
    let expired = code_for(&s, SUPERADMIN);
    expire_codes(&s);
    assert!(matches!(
        enroll_code(&expired, "late").await,
        Err(EnrollFail::Closed(Some(4410)))
    ));
    assert!(
        s.log().contains("\"reason\":\"expired_code\""),
        "{}",
        s.log()
    );
    // The refusal changed nothing: no phone, and a new code works.
    let (_, overview) = admin(&s, "GET", "/auth/inbox/api/overview", "");
    assert_eq!(overview["stats"]["enrolled"], 0, "{overview}");
    enroll_as(&s, "on time", SUPERADMIN).await.unwrap();
    s.stop();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_account_has_a_limited_number_of_active_codes() {
    let mut s = Server::setup();
    s.start();
    // inbox.enrollment.max_active_per_account has its default: 5.
    let mut codes = Vec::new();
    for _ in 0..5 {
        codes.push(code_for(&s, "dave"));
    }
    let (st, v) = make_code(&s, "dave");
    assert_eq!(st, 303, "{v}");
    assert_eq!(v["notice"], "too_many_codes", "{v}");
    // Another account has its own count.
    assert_eq!(make_code(&s, SUPERADMIN).0, 200);
    // A used code frees a place. An expired code does too.
    enroll_code(&codes[0], "phone-dave").await.unwrap();
    assert_eq!(make_code(&s, "dave").0, 200);
    assert_eq!(make_code(&s, "dave").0, 303);
    expire_codes(&s);
    assert_eq!(make_code(&s, "dave").0, 200);
    s.stop();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_deleted_account_loses_its_phones_and_its_codes() {
    let mut s = Server::setup();
    s.start();
    let port = s.port;
    let carol = enroll_as(&s, "phone-carol", "carol").await.unwrap();
    let dave = enroll_as(&s, "phone-dave", "dave").await.unwrap();
    let spare = code_for(&s, "carol");
    let (mut on_carol, _) = Conn::open(port, &carol).await.unwrap();
    on_carol.subscribe("infra").await;
    let (mut on_dave, _) = Conn::open(port, &dave).await.unwrap();
    on_dave.subscribe("infra").await;

    delete_account(&s, "carol");
    assert_eq!(
        on_carol.closed_code().await,
        Some(inbox_core::proto::close::REVOKED)
    );
    assert_eq!(
        Conn::open(port, &carol).await.err(),
        Some(inbox_core::proto::close::REVOKED)
    );
    // The unused code of the account is gone too.
    assert!(matches!(
        enroll_code(&spare, "phone-carol-2").await,
        Err(EnrollFail::Closed(Some(4410)))
    ));
    // The overview shows the revoked phone. The phone of another account is not touched.
    let (_, overview) = admin(&s, "GET", "/auth/inbox/api/overview", "");
    let status = |name: &str| {
        overview["devices"]
            .as_array()
            .unwrap()
            .iter()
            .find(|d| d["device"]["name"] == name)
            .map(|d| d["status"].as_str().unwrap().to_string())
            .unwrap()
    };
    assert_eq!(status("phone-carol"), "revoked");
    assert_eq!(status("phone-dave"), "online");
    let (_, v) = admin(&s, "GET", "/auth/inbox/api/connections?user=carol", "");
    assert!(!v["connections"].as_array().unwrap().is_empty(), "{v}");
    // The alerts go to the phone that remains.
    let (st, _) = post_alert(
        &s,
        "after-delete",
        json!({"group": "infra", "title": "still here"}),
    );
    assert_eq!(st, 201);
    assert_eq!(on_dave.alert().await.title, "still here");
    // An account that has the same login again does not bring the phone back.
    add_account(&s, "carol");
    assert_eq!(
        Conn::open(port, &carol).await.err(),
        Some(inbox_core::proto::close::REVOKED)
    );
    s.stop();
}

/// Without the block `inbox` the pages do not exist, and an account can be deleted as before.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn without_the_inbox_block_the_pages_are_absent() {
    let mut s = Server::setup();
    let text = std::fs::read_to_string(s.config()).unwrap();
    // The block `inbox` goes. The block `postgres`, if any, stays.
    let cut = text.find("inbox {").unwrap();
    let end = text.find("\npostgres {").unwrap_or(text.len());
    std::fs::write(s.config(), format!("{}{}", &text[..cut], &text[end..])).unwrap();
    s.start();
    let session = s.session();
    for path in [
        "/auth/settings/inbox",
        "/auth/settings/inbox/phones",
        "/auth/settings/inbox/doc",
    ] {
        let (st, _) = inbox_page_as(&s, &session.cookie, path);
        assert_eq!(st, 404, "{path}");
    }
    let (st, page) = inbox_page_as(&s, &session.cookie, "/auth/settings");
    assert_eq!(st, 200);
    assert!(
        !page.contains("/auth/settings/inbox"),
        "no tab Inbox without the block"
    );
    let (st, home) = inbox_page_as(&s, &session.cookie, "/");
    assert_eq!(st, 200);
    assert!(!home.contains("Inbox"), "no tile Inbox without the block");
    users_form(
        &s,
        "/auth/users/add",
        &format!("login=erin&password={ACCOUNT_PASSWORD}"),
    );
    delete_account(&s, "erin");
    s.stop();
}

// The alerts of the portal about itself.

/// A fake Docker socket. It lists one container with the labels of a service of the launcher. The
/// test sets the result of its `HEALTHCHECK`.
struct FakeDocker {
    path: PathBuf,
    health: std::sync::Arc<std::sync::Mutex<&'static str>>,
}

impl FakeDocker {
    fn start(dir: &std::path::Path, health: &'static str) -> FakeDocker {
        let path = dir.join("docker.sock");
        let listener = std::os::unix::net::UnixListener::bind(&path).unwrap();
        let health = std::sync::Arc::new(std::sync::Mutex::new(health));
        let shared = health.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { return };
                let mut seen = Vec::new();
                let mut chunk = [0u8; 1024];
                while !seen.windows(4).any(|w| w == b"\r\n\r\n") {
                    match stream.read(&mut chunk) {
                        Ok(0) | Err(_) => break,
                        Ok(n) => seen.extend_from_slice(&chunk[..n]),
                    }
                }
                let status = *shared.lock().unwrap();
                let body = format!(
                    r#"[{{"Id":"a","Names":["/notes"],"State":"running","Status":"Up 1 hour","Labels":{{"portal.name":"Notes","portal.id":"notes","portal.href":"/notes/"}},"Health":{{"Status":"{status}"}}}}]"#
                );
                let _ = write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
            }
        });
        FakeDocker { path, health }
    }

    fn set(&self, health: &'static str) {
        *self.health.lock().unwrap() = health;
    }
}

/// The block `portal_alerts` that the tests use. The values are the smallest that the ranges allow.
fn portal_alerts_block(extra: &str) -> String {
    format!(
        "  portal_alerts {{\n    group = \"infra\"\n    unhealthy_after = \"10s\"\n    sign_in_failures = 6\n    sign_in_window = \"1m\"\n    repeat_after = \"7d\"\n{extra}\n  }}"
    )
}

/// The service `notes` comes from the fake Docker socket, which a probe of 2 s reads again.
fn with_notes(s: &Server, health: &'static str) -> FakeDocker {
    let docker = FakeDocker::start(&s.dir, health);
    s.add_block(&format!(
        "discovery {{\n  interval = \"2s\"\n  docker_socket = \"{}\"\n}}",
        docker.path.display()
    ));
    docker
}

fn portal_alert_list(s: &Server) -> Vec<Value> {
    let (st, list) = admin(s, "GET", "/auth/inbox/api/alerts", "");
    assert_eq!(st, 200, "{list}");
    list["alerts"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|a| a["source"] == "portal")
        .cloned()
        .collect()
}

fn metrics_of(s: &Server) -> String {
    http_host(
        s.admin_port,
        &format!("127.0.0.1:{}", s.admin_port),
        "GET",
        "/metrics",
        &[],
        "",
    )
    .1
}

/// A burst of refused sign-ins makes one alert for the phone. A second burst inside `repeat_after` makes none.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_burst_of_refused_sign_ins_makes_one_alert() {
    let mut s = Server::setup();
    quiet_phones(&s);
    s.add_to_inbox(&portal_alerts_block("    service_unhealthy = false"));
    s.start();
    // The sessions come before the burst: the limiter locks the address out afterwards.
    let phone = enroll_as(&s, "phone", SUPERADMIN).await.unwrap();
    let _ = s.session();
    let (mut conn, _) = Conn::open(s.port, &phone).await.unwrap();
    conn.subscribe("infra").await;

    // Five refused sign-ins are fewer than the threshold: nothing.
    refuse_sign_ins(s.admin_port, "mallory", 5);
    conn.no_alert(Duration::from_secs(3)).await;
    // Four more reach the threshold of 6.
    refuse_sign_ins(s.admin_port, "mallory", 4);
    let alert = conn.alert_within(Duration::from_secs(10)).await;
    assert_eq!(alert.title, "6 refused sign-ins in 1 minute");
    assert!(!alert.title.contains("mallory") && !alert.title.contains("127.0.0.1"));
    assert_eq!(alert.group, "infra");
    conn.ack(&alert.alert_id, AckStage::Received).await;

    // A second burst inside `repeat_after` makes no second alert, and the burst goes on.
    refuse_sign_ins(s.admin_port, "mallory", 8);
    conn.no_alert(Duration::from_secs(4)).await;
    let alerts = portal_alert_list(&s);
    assert_eq!(alerts.len(), 1, "{alerts:?}");
    assert_eq!(alerts[0]["title"], alert.title);
    let metrics = metrics_of(&s);
    for expected in [
        "portal_alerts_sent_total{kind=\"sign_in_burst\"} 1",
        "portal_alerts_sent_total{kind=\"service_unhealthy\"} 0",
        "portal_alerts_failed_total{kind=\"sign_in_burst\"} 0",
    ] {
        assert!(metrics.contains(expected), "{expected}\n{metrics}");
    }
    s.stop();
}

// ---------------------------------------------------------------------------------------------
// Rights: keys of the portal as producers, subscriptions that follow the permissions.

/// The text of a key of the portal: `mcp_`, 43 characters and the checksum. The tests use fixed text.
fn key_text(seed: u8) -> String {
    const ALPHABET: &[u8; 62] = b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz";
    let random: Vec<u8> = (0..43)
        .map(|i| ALPHABET[(usize::from(seed) * 7 + i * 5) % 62])
        .collect();
    let mut crc = 0xFFFF_FFFFu32;
    for byte in &random {
        crc ^= u32::from(*byte);
        for _ in 0..8 {
            let mask = (crc & 1).wrapping_neg();
            crc = (crc >> 1) ^ (0xEDB8_8320 & mask);
        }
    }
    let mut value = !crc;
    let mut check = [b'0'; 6];
    for digit in check.iter_mut().rev() {
        *digit = ALPHABET[(value % 62) as usize];
        value /= 62;
    }
    format!(
        "mcp_{}{}",
        String::from_utf8(random).unwrap(),
        String::from_utf8(check.to_vec()).unwrap()
    )
}

fn key_digest(text: &str) -> String {
    use sha2::{Digest, Sha256};
    Sha256::digest(text.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// The configuration of the keys: the blocks `access`, `keys` and `identity`, and the accounts of
/// the file in `users`.
fn enable_keys(s: &Server, users: &str) {
    std::fs::write(
        s.dir.join("identity.pem"),
        "-----BEGIN PRIVATE KEY-----\nMC4CAQAwBQYDK2VwBCIEIAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8g\n-----END PRIVATE KEY-----\n",
    )
    .unwrap();
    let mut text = std::fs::read_to_string(s.config()).unwrap();
    let pdp = free_port();
    text.push_str(&format!(
        "\naccess {{\n  grants_mode = \"hybrid\"\n}}\nkeys {{\n}}\nidentity {{\n  issuer = \"https://portal.e2e\"\n  signing_key_file = \"{}\"\n}}\npdp {{\n  listen_address = \"127.0.0.1:{pdp}\"\n  url = \"http://127.0.0.1:{pdp}\"\n  plain_http = true\n}}\n{users}\n",
        s.dir.join("identity.pem").display()
    ));
    std::fs::write(s.config(), text).unwrap();
}

/// A request of a producer with the text of a key or a token. The answer is the status, the head and
/// the JSON body.
fn producer_call(
    s: &Server,
    method: &str,
    path: &str,
    token: &str,
    idempotency_key: &str,
    body: &str,
) -> (u16, String, Value) {
    let host = format!("localhost:{}", s.admin_port);
    let (status, head, text) = http_full(
        s.admin_port,
        &host,
        method,
        path,
        &[
            ("Authorization", format!("Bearer {token}")),
            ("Idempotency-Key", idempotency_key.to_string()),
        ],
        body,
    );
    (
        status,
        head,
        serde_json::from_str(&text).unwrap_or(Value::Null),
    )
}

fn send_with(s: &Server, token: &str, key: &str, group: &str, title: &str) -> (u16, String, Value) {
    producer_call(
        s,
        "POST",
        "/api/v1/alerts",
        token,
        key,
        &json!({ "group": group, "title": title }).to_string(),
    )
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_key_of_the_portal_sends_alerts_with_the_permissions_and_the_limit_of_its_owner() {
    let mut s = Server::setup();
    s.add_to_inbox("  group \"crit\" {\n    name = \"Critical\"\n    critical = true\n  }");
    let (ci, narrow, ghost) = (key_text(1), key_text(2), key_text(3));
    std::fs::write(s.dir.join("ci.sha256"), key_digest(&ci)).unwrap();
    std::fs::write(s.dir.join("narrow.sha256"), key_digest(&narrow)).unwrap();
    let users = format!(
        r#"
user "svc" {{
  grants = [
    "keys.own.use on service:portal",
    {{ grant = "access on service:inbox", limits = {{ alerts = [ {{ amount = 3, window = "minute" }} ] }} }},
    "alert.send on service:inbox/group:infra",
    "alert.send on service:inbox/group:crit",
    "alert.read on service:inbox/group:infra",
  ]
  key "ci" {{
    key_sha256_file = "{}"
    grants = [
      "alert.send on service:inbox/group:infra",
      "alert.send on service:inbox/group:crit",
      "alert.read on service:inbox/group:infra",
    ]
  }}
}}
user "partial" {{
  grants = [
    "keys.own.use on service:portal",
    "alert.send on service:inbox/group:infra",
  ]
  key "narrow" {{
    key_sha256_file = "{}"
    grants = [
      "alert.send on service:inbox/group:infra",
      "alert.send on service:inbox/group:other",
    ]
  }}
}}
"#,
        s.dir.join("ci.sha256").display(),
        s.dir.join("narrow.sha256").display()
    );
    enable_keys(&s, &users);
    s.start();
    let phone = enroll_as(&s, "phone", SUPERADMIN).await.unwrap();
    let (mut conn, _) = Conn::open(s.port, &phone).await.unwrap();
    conn.subscribe("infra").await;
    conn.subscribe("crit").await;

    // An allowed group: the alert is made and the phone gets it exactly one time.
    let (st, _, v) = send_with(&s, &ci, "k1", "infra", "from a key");
    assert_eq!(st, 201, "{v}");
    assert_eq!(v["targets"], 1);
    let first = conn.alert().await;
    assert_eq!(first.title, "from a key");
    conn.ack(&first.alert_id, AckStage::Received).await;
    conn.no_alert(Duration::from_millis(2500)).await;
    // The same Idempotency-Key makes no second alert. The repeated request is a use of the limit.
    let (st, _, v) = send_with(&s, &ci, "k1", "infra", "from a key");
    assert_eq!(st, 200, "{v}");
    conn.no_alert(Duration::from_millis(1500)).await;

    // A group without the permission: 403, and nothing is made. The reason is in the answer.
    let (st, _, v) = send_with(&s, &ci, "k2", "other", "no");
    assert_eq!(st, 403, "{v}");
    assert_eq!(v["error"]["code"], "forbidden_group", "{v}");
    assert!(
        v["error"]["message"].as_str().unwrap().contains("other"),
        "{v}"
    );
    // The key has the permission, its owner has not: the decision is no.
    let (st, _, _) = send_with(&s, &narrow, "n1", "other", "no");
    assert_eq!(st, 403);
    let (st, _, _) = send_with(&s, &narrow, "n2", "infra", "ok");
    assert_eq!(st, 201);
    assert_eq!(take(&mut conn).await.title, "ok");

    // The limit of the owner: 3 each minute. Two units are used (the first send and its repeat). This send takes the last.
    let (st, _, v) = send_with(&s, &ci, "k3", "infra", "second");
    assert_eq!(st, 201, "{v}");
    assert_eq!(take(&mut conn).await.title, "second");
    // The third send hits the limit: 429 with Retry-After, and nothing is made.
    let (st, head, v) = send_with(&s, &ci, "k4", "infra", "third");
    assert_eq!(st, 429, "{v}");
    assert_eq!(v["error"]["code"], "rate_limited", "{v}");
    let retry: u64 = head
        .lines()
        .find_map(|l| {
            l.to_ascii_lowercase()
                .strip_prefix("retry-after: ")
                .map(str::to_string)
        })
        .expect("the answer 429 has Retry-After")
        .trim()
        .parse()
        .unwrap();
    assert!((1..=60).contains(&retry), "{retry}");
    // The webhook and the generic endpoint count as the same use.
    let (st, _, _) = producer_call(&s, "POST", "/api/v1/notify/infra", &ci, "k5", "text");
    assert_eq!(st, 429);
    let (st, _, _) = producer_call(
        &s,
        "POST",
        "/api/v1/webhooks/alertmanager/infra",
        &ci,
        "k6",
        r#"{"alerts":[{"status":"firing","labels":{"alertname":"X"}}]}"#,
    );
    assert_eq!(st, 429);

    // A critical group never has a limit: more sends than the limit all work.
    for n in 0..5 {
        let (st, _, v) = send_with(&s, &ci, &format!("c{n}"), "crit", &format!("critical {n}"));
        assert_eq!(st, 201, "critical send {n}: {v}");
    }
    for n in 0..5 {
        assert_eq!(take(&mut conn).await.title, format!("critical {n}"));
    }
    let (st, _, _) = producer_call(
        &s,
        "POST",
        "/api/v1/notify/crit",
        &ci,
        "c9",
        "a critical notification",
    );
    assert_eq!(st, 201);
    assert_eq!(take(&mut conn).await.title, "a critical notification");
    let (st, _, _) = producer_call(
        &s,
        "POST",
        "/api/v1/webhooks/alertmanager/crit",
        &ci,
        "c10",
        r#"{"alerts":[{"status":"firing","labels":{"alertname":"Disk"},"fingerprint":"f1"}]}"#,
    );
    assert_eq!(st, 200);
    assert!(take(&mut conn).await.title.starts_with("[FIRING] Disk"));

    // The token of the file still works as before, and the key in the query works too.
    let (st, v) = post_alert(
        &s,
        "hcl1",
        json!({"group": "infra", "title": "from the file"}),
    );
    assert_eq!(st, 201, "{v}");
    assert_eq!(take(&mut conn).await.title, "from the file");
    let (st, _) = http(
        s.admin_port,
        "POST",
        &format!("/api/v1/notify/crit?token={ci}&title=query"),
        &[],
        "",
    );
    assert_eq!(st, 201);
    assert_eq!(take(&mut conn).await.title, "query");

    // The key of the owner can read the delivery status of an alert of a group that it may read.
    let id = first.alert_id.clone();
    let (st, _, v) = producer_call(&s, "GET", &format!("/api/v1/alerts/{id}"), &ci, "", "");
    assert_eq!(st, 200, "{v}");
    let (st, _, v) = send_with(&s, &ci, "crit-status", "crit", "status");
    assert_eq!(st, 201, "{v}");
    let crit_id = v["alert_id"].as_str().unwrap().to_string();
    let (st, _, _) = producer_call(&s, "GET", &format!("/api/v1/alerts/{crit_id}"), &ci, "", "");
    assert_eq!(st, 404, "the key has no alert.read on the group crit");
    let _ = take(&mut conn).await;

    // A text that is not a key, and a key that nobody has: 401.
    for token in ["not a key at all", ghost.as_str(), "mcp_short"] {
        let (st, _, _) = send_with(&s, token, "g1", "infra", "no");
        assert_eq!(st, 401, "{token}");
    }
    s.stop();
}

/// A service that goes unhealthy makes one alert after `unhealthy_after`. When it is healthy again, the
/// phone gets one short alert. A service that stays healthy makes no alert.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_unhealthy_service_makes_one_alert_and_its_recovery_one_more() {
    let mut s = Server::setup();
    quiet_phones(&s);
    s.add_to_inbox(&portal_alerts_block("    sign_in_burst = false"));
    let docker = with_notes(&s, "healthy");
    s.start();
    let phone = enroll_as(&s, "phone", SUPERADMIN).await.unwrap();
    let (mut conn, _) = Conn::open(s.port, &phone).await.unwrap();
    conn.subscribe("infra").await;

    // Healthy and starting give nothing, however long.
    conn.no_alert(Duration::from_secs(3)).await;
    docker.set("starting");
    conn.no_alert(Duration::from_secs(12)).await;

    docker.set("unhealthy");
    let alert = conn.alert_within(Duration::from_secs(25)).await;
    assert_eq!(alert.title, "Service Notes is unhealthy for 10 seconds");
    conn.ack(&alert.alert_id, AckStage::Received).await;
    // One alert, not one for each reading.
    conn.no_alert(Duration::from_secs(5)).await;

    docker.set("healthy");
    let back = conn.alert_within(Duration::from_secs(15)).await;
    assert_eq!(back.title, "Service Notes is healthy again");
    conn.ack(&back.alert_id, AckStage::Received).await;
    conn.no_alert(Duration::from_secs(4)).await;

    // The unhealthy alert is not repeated inside `repeat_after`, whatever the service does.
    docker.set("unhealthy");
    conn.no_alert(Duration::from_secs(13)).await;
    let titles: Vec<_> = portal_alert_list(&s)
        .iter()
        .map(|a| a["title"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(titles.len(), 2, "{titles:?}");
    let metrics = metrics_of(&s);
    for expected in [
        "portal_alerts_sent_total{kind=\"service_unhealthy\"} 1",
        "portal_alerts_sent_total{kind=\"service_recovered\"} 1",
        "portal_alerts_sent_total{kind=\"sign_in_burst\"} 0",
    ] {
        assert!(metrics.contains(expected), "{expected}\n{metrics}");
    }
    s.stop();
}

/// Two active instances detect the same events. The shared key makes the inbox accept one alert for each event.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn two_instances_send_one_alert_for_one_event() {
    let Some(admin_pg) = pg_admin() else {
        eprintln!("INBOX_TEST_PG is not set: skipped");
        return;
    };
    let mut a = Server::setup_with(Some(admin_pg));
    quiet_phones(&a);
    a.add_to_inbox(&portal_alerts_block(""));
    let docker = with_notes(&a, "healthy");
    let mut b = a.sibling("instance-b");
    a.start();
    b.start();
    let phone = enroll_as(&a, "phone", SUPERADMIN).await.unwrap();
    let _ = (a.session(), b.session());
    let (mut conn, _) = Conn::open(b.port, &phone).await.unwrap();
    conn.subscribe("infra").await;

    // Both instances see the service go down.
    docker.set("unhealthy");
    let alert = conn.alert_within(Duration::from_secs(25)).await;
    assert_eq!(alert.title, "Service Notes is unhealthy for 10 seconds");
    conn.ack(&alert.alert_id, AckStage::Received).await;
    // Both instances see a burst of refusals.
    refuse_sign_ins(a.admin_port, "mallory", 7);
    refuse_sign_ins(b.admin_port, "mallory", 7);
    let burst = conn.alert_within(Duration::from_secs(10)).await;
    assert_eq!(burst.title, "6 refused sign-ins in 1 minute");
    conn.ack(&burst.alert_id, AckStage::Received).await;
    conn.no_alert(Duration::from_secs(5)).await;

    // The database holds two alerts, and both come from the portal.
    let stored = stored_alerts(&a);
    assert_eq!(stored.len(), 2, "one alert for each event: {stored:?}");
    assert_eq!(portal_alert_list(&a).len(), 2);
    assert_eq!(portal_alert_list(&b).len(), 2);
    // The two instances both tried: one of them found the key already in use.
    wait_for(
        "an instance that finds the key in use",
        Duration::from_secs(10),
        || format!("{}{}", a.log(), b.log()).contains("\"duplicate\":\"true\""),
    );
    let logs = format!("{}{}", a.log(), b.log());
    assert!(!logs.contains("portal_alert_failed"), "{logs}");
    b.stop();
    a.stop();
}

#[test]
fn check_config_reports_the_block_portal_alerts() {
    let s = Server::setup();
    let out = portal(&["check-config", "--config", &s.config()], None);
    let shown = String::from_utf8_lossy(&out.stdout).to_string();
    assert!(out.status.success());
    assert!(
        !shown.contains("inbox.portal_alerts."),
        "nothing without the block: {shown}"
    );
    let text = std::fs::read_to_string(s.config()).unwrap();
    let good = s.dir.join("alerts-ok.hcl");
    std::fs::write(
        &good,
        text.replace(
            "  # inbox-extra\n",
            "  portal_alerts {\n    group = \"other\"\n    sign_in_failures = 3\n  }\n  # inbox-extra\n",
        ),
    )
    .unwrap();
    let out = portal(&["check-config", "--config", good.to_str().unwrap()], None);
    let shown = String::from_utf8_lossy(&out.stdout).to_string();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    for expected in [
        "inbox.portal_alerts.group = \"other\"  # file",
        "inbox.portal_alerts.sign_in_failures = 3  # file",
        "inbox.portal_alerts.unhealthy_after = \"2m\"  # default",
        "inbox.portal_alerts.repeat_after = \"1h\"  # default",
        "inbox.portal_alerts.recovered = true  # default",
    ] {
        assert!(shown.contains(expected), "{expected}\n{shown}");
    }
    // A group that does not exist stops the start, and the message names the key and the value.
    let bad = s.dir.join("alerts-bad.hcl");
    std::fs::write(
        &bad,
        text.replace(
            "  # inbox-extra\n",
            "  portal_alerts {\n    group = \"nowhere\"\n  }\n  # inbox-extra\n",
        ),
    )
    .unwrap();
    let out = portal(&["check-config", "--config", bad.to_str().unwrap()], None);
    assert_eq!(out.status.code(), Some(1));
    let error = String::from_utf8_lossy(&out.stderr);
    assert!(
        error.contains("inbox.portal_alerts.group") && error.contains("\"nowhere\""),
        "{error}"
    );
}

/// The next alert of the connection, confirmed.
async fn take(conn: &mut Conn) -> AlertMsg {
    let alert = conn.alert().await;
    conn.ack(&alert.alert_id, AckStage::Received).await;
    alert
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_phone_follows_the_permission_to_subscribe() {
    let mut s = Server::setup();
    s.start();
    let port = s.port;
    // The account has the permission for the group other only.
    s.account("nina");
    set_grants(
        &s,
        "nina",
        &["group.subscribe on service:inbox/group:other"],
    );
    let nina = enroll_as(&s, "phone-nina", "nina").await.unwrap();
    let (mut conn, welcome) = Conn::open(port, &nina).await.unwrap();
    let ServerMsg::Welcome { groups, .. } = welcome else {
        panic!("expected welcome")
    };
    let ids: Vec<&str> = groups.iter().map(|g| g.id.as_str()).collect();
    assert_eq!(ids, ["other"], "the welcome lists the allowed groups only");
    // A subscription to a group without the permission is refused with its own error.
    conn.send(&ClientMsg::Subscribe {
        group: "infra".into(),
    })
    .await;
    loop {
        match conn.recv(Duration::from_secs(5)).await {
            Recv::Msg(ServerMsg::Error { code, message }) => {
                assert_eq!(
                    (code.as_str(), message.as_str()),
                    ("forbidden_group", "infra")
                );
                break;
            }
            Recv::Msg(_) => continue,
            other => panic!("expected the error, got {}", describe(&other)),
        }
    }
    // A group that does not exist keeps its own error.
    conn.send(&ClientMsg::Subscribe {
        group: "nothing".into(),
    })
    .await;
    loop {
        match conn.recv(Duration::from_secs(5)).await {
            Recv::Msg(ServerMsg::Error { code, .. }) => {
                assert_eq!(code, "unknown_group");
                break;
            }
            Recv::Msg(_) => continue,
            other => panic!("expected the error, got {}", describe(&other)),
        }
    }
    conn.subscribe("other").await;
    // The group infra makes no alert for this phone.
    let (st, v) = post_alert(
        &s,
        "no-nina",
        json!({"group": "infra", "title": "not for nina"}),
    );
    assert_eq!(st, 201, "{v}");
    assert_eq!(v["targets"], 0);
    conn.no_alert(Duration::from_millis(1500)).await;
    s.stop();
}

fn describe(recv: &Recv) -> String {
    match recv {
        Recv::Msg(m) => format!("{m:?}"),
        Recv::Closed(c) => format!("closed {c:?}"),
        Recv::Timeout => "timeout".into(),
    }
}

/// The overview names the groups of a phone.
fn groups_of(s: &Server, device_id: &str) -> Vec<String> {
    let (_, v) = admin(s, "GET", "/auth/inbox/api/overview", "");
    v["devices"]
        .as_array()
        .unwrap()
        .iter()
        .find(|d| d["device"]["id"] == device_id)
        .map(|d| {
            d["device"]["subscriptions"]
                .as_array()
                .unwrap()
                .iter()
                .map(|g| g.as_str().unwrap().to_string())
                .collect()
        })
        .unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_account_that_loses_the_permission_loses_the_subscription_and_keeps_its_pending_alerts()
{
    let mut s = Server::setup();
    s.start();
    let port = s.port;
    s.account("omar");
    let omar = enroll_as(&s, "phone-omar", "omar").await.unwrap();
    let stay = enroll_as(&s, "phone-stay", "alice").await.unwrap();
    {
        let (mut c, _) = Conn::open(port, &omar).await.unwrap();
        c.subscribe("infra").await;
        c.subscribe("other").await;
        let (mut c2, _) = Conn::open(port, &stay).await.unwrap();
        c2.subscribe("infra").await;
    }
    // The phone of omar is offline. An alert waits for it.
    let (st, v) = post_alert(&s, "p1", json!({"group": "infra", "title": "pending"}));
    assert_eq!(st, 201, "{v}");
    assert_eq!(v["targets"], 2);
    // Omar loses the permission for infra.
    set_grants(
        &s,
        "omar",
        &["group.subscribe on service:inbox/group:other"],
    );
    wait_for(
        "the removal of the subscription",
        Duration::from_secs(20),
        || groups_of(&s, &omar.device_id) == ["other"],
    );
    assert_eq!(
        groups_of(&s, &stay.device_id),
        ["infra"],
        "another account is not touched"
    );
    // The pending alert stays deliverable. The welcome shows no subscription to infra.
    let (mut c, welcome) = Conn::open(port, &omar).await.unwrap();
    let ServerMsg::Welcome {
        subscriptions,
        groups,
        ..
    } = welcome
    else {
        panic!("expected welcome")
    };
    assert_eq!(subscriptions, ["other"]);
    assert_eq!(groups.len(), 1);
    let got = c.alert().await;
    assert_eq!(got.title, "pending");
    c.ack(&got.alert_id, AckStage::Received).await;
    // The next alerts of the group are not made for this phone.
    let (st, v) = post_alert(&s, "p2", json!({"group": "infra", "title": "after"}));
    assert_eq!(st, 201, "{v}");
    assert_eq!(v["targets"], 1);
    c.no_alert(Duration::from_millis(1500)).await;
    // A phone that is online is told with the message `subscriptions`.
    set_grants(
        &s,
        "omar",
        &[
            "group.subscribe on service:inbox/group:other",
            "group.subscribe on service:inbox/group:infra",
        ],
    );
    c.subscribe("infra").await;
    set_grants(
        &s,
        "omar",
        &["group.subscribe on service:inbox/group:infra"],
    );
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        match c.recv(Duration::from_secs(1)).await {
            Recv::Msg(ServerMsg::Subscriptions { subscriptions }) if subscriptions == ["infra"] => {
                break
            }
            Recv::Closed(code) => panic!("closed: {code:?}"),
            _ => assert!(Instant::now() < deadline, "the phone was never told"),
        }
    }
    s.stop();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_superadmin_and_the_accounts_of_the_file_follow_their_permissions() {
    let mut s = Server::setup();
    let hash = portal(&["hash-password"], Some(ACCOUNT_PASSWORD));
    assert!(hash.status.success());
    std::fs::write(
        s.dir.join("fred.hash"),
        String::from_utf8(hash.stdout).unwrap().trim(),
    )
    .unwrap();
    let users = format!(
        r#"
user "fred" {{
  password_hash_file = "{}"
  grants = [ "group.subscribe on service:inbox/group:infra" ]
}}
"#,
        s.dir.join("fred.hash").display()
    );
    enable_keys(&s, &users);
    s.start();
    // The superadmin has every permission: it subscribes to each group.
    let root = enroll_as(&s, "phone-root", SUPERADMIN).await.unwrap();
    let (mut c, welcome) = Conn::open(s.port, &root).await.unwrap();
    let ServerMsg::Welcome { groups, .. } = welcome else {
        panic!("expected welcome")
    };
    assert_eq!(groups.len(), 2);
    c.subscribe("infra").await;
    c.subscribe("other").await;
    // An account of the file has its permissions from the file: it opens the page of its phones,
    // makes a code, and its phone sees the group of its permission only.
    let fred = sign_in_as(s.admin_port, "fred", ACCOUNT_PASSWORD);
    let (st, page) = inbox_page_as(&s, &fred.cookie, "/auth/settings/inbox/phones");
    assert_eq!(st, 200, "{page}");
    let (st, _) = inbox_page_as(&s, &fred.cookie, "/auth/settings/inbox/devices");
    assert_eq!(st, 403);
    let (st, _, page) = inbox_form(&s, &fred.cookie, "/auth/settings/inbox/code", "");
    assert_eq!(st, 200, "{page}");
    let marker = "id=\"enroll-code\" type=\"text\" readonly value=\"";
    let start = page.find(marker).unwrap() + marker.len();
    let code = unescape(page[start..].split('"').next().unwrap());
    let phone = enroll_code(&code, "phone-fred").await.unwrap();
    assert_eq!(phone.user_name, "fred");
    let (mut cf, welcome) = Conn::open(s.port, &phone).await.unwrap();
    let ServerMsg::Welcome { groups, .. } = welcome else {
        panic!("expected welcome")
    };
    assert_eq!(
        groups.iter().map(|g| g.id.as_str()).collect::<Vec<_>>(),
        ["infra"]
    );
    cf.subscribe("infra").await;
    cf.send(&ClientMsg::Subscribe {
        group: "other".into(),
    })
    .await;
    loop {
        match cf.recv(Duration::from_secs(5)).await {
            Recv::Msg(ServerMsg::Error { code, .. }) => {
                assert_eq!(code, "forbidden_group");
                break;
            }
            Recv::Msg(_) => continue,
            other => panic!("expected the error, got {}", describe(&other)),
        }
    }
    s.stop();
}

/// Whether a page has the text.
fn has(page: &str, text: &str) -> bool {
    page.contains(text)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_pages_of_the_inbox_follow_the_permissions_of_the_account() {
    let mut s = Server::setup();
    s.start();
    let port = s.port;
    // A sender: it sends to the group infra and reads its alerts. It has no phone.
    let pia = s.account_with(
        "pia",
        &[
            "alert.send on service:inbox/group:infra",
            "alert.read on service:inbox/group:infra",
        ],
    );
    let (st, page) = inbox_page_as(&s, &pia.cookie, "/auth/settings/inbox");
    assert_eq!(st, 200, "{page}");
    assert!(has(&page, "Send an alert"));
    assert!(has(&page, r#"<option value="infra">Infra</option>"#));
    assert!(
        !has(&page, r#"<option value="other">"#),
        "only the groups that the account may send to"
    );
    assert!(has(&page, "Last alerts"));
    assert!(has(
        &page,
        r#"class="contentTabs__tab is-active" aria-current="page" href="/auth/settings/inbox">Inbox"#
    ));
    assert!(
        !has(&page, "/auth/settings/services"),
        "a plain account has not the tabs of the superadmin"
    );
    for path in [
        "/auth/settings/inbox/devices",
        "/auth/settings/inbox/connections",
        "/auth/settings/inbox/phones",
    ] {
        let (st, _) = inbox_page_as(&s, &pia.cookie, path);
        assert_eq!(st, 403, "{path}");
    }
    let (st, doc) = inbox_page_as(&s, &pia.cookie, "/auth/settings/inbox/doc");
    assert_eq!(st, 200);
    assert!(
        has(
            &doc,
            &format!("http://127.0.0.1:{}/api/v1/alerts", s.admin_port)
        ),
        "{doc}"
    );
    // The form refuses a group that the account may not send to, and sends to the others.
    let (st, head, _) = inbox_form(
        &s,
        &pia.cookie,
        "/auth/settings/inbox/send",
        "group=other&title=no&key=pia1",
    );
    assert_eq!(st, 303);
    assert!(location(&head).contains("s=bad_group"), "{head}");
    let (st, head, _) = inbox_form(
        &s,
        &pia.cookie,
        "/auth/settings/inbox/send",
        "group=infra&title=+&key=pia2",
    );
    assert_eq!(st, 303);
    assert!(location(&head).contains("s=no_title"), "{head}");
    let long = "x".repeat(200);
    let (_, head, _) = inbox_form(
        &s,
        &pia.cookie,
        "/auth/settings/inbox/send",
        &format!("group=infra&title={long}&key=pia3"),
    );
    assert!(location(&head).contains("s=bad_title"), "{head}");
    let (st, head, _) = inbox_form(
        &s,
        &pia.cookie,
        "/auth/settings/inbox/send",
        "group=infra&title=Disk+full&key=pia4",
    );
    assert_eq!(st, 303);
    let sent = location(&head);
    assert!(
        sent.starts_with("/auth/settings/inbox/alerts/") && sent.contains("s=sent"),
        "{sent}"
    );
    let (st, page) = inbox_page_as(&s, &pia.cookie, &sent);
    assert_eq!(st, 200, "{page}");
    assert!(has(&page, "Disk full") && has(&page, "The alert is sent."));
    // The alert of a group that the account may not read is not there for it.
    let (st, _) = admin(
        &s,
        "POST",
        "/auth/inbox/api/alerts",
        &json!({"group": "other", "title": "secret", "idempotency_key": "o1"}).to_string(),
    );
    assert_eq!(st, 201);
    let (_, all) = admin(&s, "GET", "/auth/inbox/api/alerts", "");
    let other_id = all["alerts"][0]["id"].as_str().unwrap().to_string();
    let (st, _) = inbox_page_as(
        &s,
        &pia.cookie,
        &format!("/auth/settings/inbox/alerts/{other_id}"),
    );
    assert_eq!(st, 404);
    let (_, page) = inbox_page_as(&s, &pia.cookie, "/auth/settings/inbox");
    assert!(
        !has(&page, "secret"),
        "the list shows the alerts of the readable groups only"
    );

    // An operator: it reads the phones and the connections and revokes a phone.
    let owen = s.account_with(
        "owen",
        &[
            "device.read on service:inbox",
            "device.revoke on service:inbox",
            "connection.read on service:inbox",
        ],
    );
    let alice = enroll_as(&s, "phone-alice", "alice").await.unwrap();
    let (mut ca, _) = Conn::open(port, &alice).await.unwrap();
    ca.subscribe("infra").await;
    let (st, page) = inbox_page_as(&s, &owen.cookie, "/auth/settings/inbox");
    assert_eq!(st, 200, "{page}");
    assert!(!has(&page, "Send an alert"));
    assert!(
        !has(&page, "Last alerts"),
        "no alert.read: no list of alerts"
    );
    assert!(
        has(&page, "Alerts waiting"),
        "device.read gives the figures"
    );
    let (st, page) = inbox_page_as(&s, &owen.cookie, "/auth/settings/inbox/devices");
    assert_eq!(st, 200);
    assert!(has(&page, "phone-alice") && has(&page, "Confirm: revoke phone-alice"));
    assert!(has(&page, r#"data-state="online""#));
    let (st, page) = inbox_page_as(
        &s,
        &owen.cookie,
        "/auth/settings/inbox/connections?login=alice",
    );
    assert_eq!(st, 200);
    assert!(has(&page, r#"data-login="alice""#) && has(&page, r#"value="alice""#));
    let (st, page) = inbox_page_as(
        &s,
        &owen.cookie,
        "/auth/settings/inbox/connections?login=nobody",
    );
    assert_eq!(st, 200);
    assert!(has(&page, "No connection in the log."));
    // The operator revokes the phone of another account.
    let (st, head, text) = inbox_form(
        &s,
        &owen.cookie,
        "/auth/settings/inbox/revoke",
        &format!(
            "id={}&back=%2Fauth%2Fsettings%2Finbox%2Fdevices",
            alice.device_id
        ),
    );
    assert_eq!(st, 303, "{text}");
    assert!(location(&head).contains("s=revoked"), "{head}");
    assert_eq!(
        ca.closed_code().await,
        Some(inbox_core::proto::close::REVOKED)
    );
    // A reader without device.revoke sees no button, and a forced request changes nothing.
    let rita = s.account_with("rita", &["device.read on service:inbox"]);
    let bob = enroll_as(&s, "phone-bob", "bob").await.unwrap();
    let (_, page) = inbox_page_as(&s, &rita.cookie, "/auth/settings/inbox/devices");
    assert!(has(&page, "phone-bob") && !has(&page, "Confirm: revoke"));
    // The page has no form, so the account has no token: the request fails on the token.
    let (st, _, _) = inbox_form(
        &s,
        &rita.cookie,
        "/auth/settings/inbox/revoke",
        &format!(
            "id={}&back=%2Fauth%2Fsettings%2Finbox%2Fdevices",
            bob.device_id
        ),
    );
    assert_eq!(st, 403);
    assert!(
        Conn::open(port, &bob).await.is_ok(),
        "the phone is not revoked"
    );

    // My phones: an account revokes its own phone and no other.
    let bob_session = s.account("bob");
    let (st, page) = inbox_page_as(&s, &bob_session.cookie, "/auth/settings/inbox/phones");
    assert_eq!(st, 200);
    assert!(has(&page, "phone-bob") && !has(&page, "phone-alice"));
    assert!(has(&page, "Confirm: revoke phone-bob") && has(&page, "Make a code"));
    let carol = enroll_as(&s, "phone-carol", "carol").await.unwrap();
    let (_, head, _) = inbox_form(
        &s,
        &bob_session.cookie,
        "/auth/settings/inbox/revoke",
        &format!(
            "id={}&back=%2Fauth%2Fsettings%2Finbox%2Fphones",
            carol.device_id
        ),
    );
    assert!(location(&head).contains("s=unknown_device"), "{head}");
    assert!(Conn::open(port, &carol).await.is_ok());
    let (_, head, _) = inbox_form(
        &s,
        &bob_session.cookie,
        "/auth/settings/inbox/revoke",
        &format!(
            "id={}&back=%2Fauth%2Fsettings%2Finbox%2Fphones",
            bob.device_id
        ),
    );
    assert!(location(&head).contains("s=revoked"), "{head}");
    assert_eq!(
        Conn::open(port, &bob).await.err(),
        Some(inbox_core::proto::close::REVOKED)
    );
    // The code page is shown one time, and no store keeps it.
    let host = format!("127.0.0.1:{}", s.admin_port);
    let csrf = inbox_csrf(s.admin_port, &bob_session.cookie);
    let (st, head, page) = http_full(
        s.admin_port,
        &host,
        "POST",
        "/auth/settings/inbox/code",
        &[
            ("Content-Type", "application/x-www-form-urlencoded".into()),
            ("Origin", format!("http://{host}")),
            ("Cookie", bob_session.cookie.clone()),
        ],
        &format!("csrf={csrf}"),
    );
    assert_eq!(st, 200);
    assert!(
        head.to_ascii_lowercase()
            .contains("cache-control: no-store"),
        "{head}"
    );
    assert!(has(&page, "inbox://enroll?s=") && has(&page, "data-copy=\"enroll-code\""));

    // The inbox is a feature of the portal: the launcher has no tile for it. An account with no permission of
    // the inbox has no gear and no tab. An account that has one gets the gear, which opens the tab.
    let none = s.account_with("nora", &[]);
    let (_, home) = inbox_page_as(&s, &none.cookie, "/");
    assert!(!has(&home, "/auth/settings/inbox") && !has(&home, "Settings"));
    let (st, _) = inbox_page_as(&s, &none.cookie, "/auth/settings/inbox");
    assert_eq!(st, 403);
    let (_, home) = inbox_page_as(&s, &pia.cookie, "/");
    assert!(
        has(
            &home,
            r#"href="/auth/settings/inbox" aria-label="Settings""#
        ),
        "{home}"
    );
    assert!(
        !has(&home, "inbox-tile") && !has(&home, "<h2"),
        "no tile of the inbox: {home}"
    );
    let (_, home) = inbox_page_as(&s, &s.session().cookie, "/");
    assert!(has(&home, r#"href="/auth/settings" aria-label="Settings""#));
    assert!(
        !has(&home, r#"href="/auth/settings/inbox""#),
        "no tile of the inbox"
    );
    // The superadmin sees the five tabs at most and the permission editor shows the groups by name.
    let (_, page) = inbox_page(&s, "/auth/settings/inbox");
    for tab in ["Users", "Services", "Activity", "Inbox"] {
        assert!(has(&page, &format!(">{tab}</a>")), "{tab}");
    }
    assert!(!has(&page, ">Keys</a>"), "no block keys, no tab Keys");
    // The inbox is a feature of the portal, as the keys are: the Services page does not list it, and
    // it has no page of a service. The editor of permissions keeps it as "Inbox (portal)".
    let (st, services) = inbox_page(&s, "/auth/settings/services");
    assert_eq!(st, 200);
    assert!(
        !has(&services, "Inbox (portal)") && !has(&services, "alert.send"),
        "{services}"
    );
    let (st, _) = inbox_page(&s, "/auth/settings/services/inbox");
    assert_eq!(st, 404);
    let (st, editor) = inbox_page(&s, "/auth/settings/users/pia?service=inbox");
    assert_eq!(st, 200);
    assert!(
        has(&editor, "Inbox (portal)")
            && has(&editor, "group:infra")
            && has(&editor, "Infra")
            && has(&editor, "device.revoke"),
        "{editor}"
    );
    s.stop();
}

/// Starts a server with accounts, phones and alerts, and waits until the file `stop` appears in the
/// directory that `INBOX_DEMO_OUT` names. The test is for a look at the pages with a browser. It
/// writes `demo.json` with the address of the portal. Run it with
/// `cargo test --test inbox_e2e demo_for_the_eyes -- --ignored`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "starts a server for a look at the pages"]
async fn demo_for_the_eyes() {
    let out = PathBuf::from(std::env::var("INBOX_DEMO_OUT").expect("INBOX_DEMO_OUT"));
    let mut s = Server::setup();
    s.add_to_inbox(
        "  group \"crit\" {\n    name = \"Critical outages of the production cluster\"\n    description = \"Pages the on-call team\"\n    critical = true\n  }",
    );
    s.start();
    s.account_with(
        "pia",
        &[
            "alert.send on service:inbox/group:infra",
            "alert.read on service:inbox/group:infra",
        ],
    );
    s.account_with(
        "owen",
        &[
            "device.read on service:inbox",
            "device.revoke on service:inbox",
            "connection.read on service:inbox",
            "alert.read on service:inbox/group:infra",
            "alert.read on service:inbox/group:other",
            "alert.read on service:inbox/group:crit",
            "alert.send on service:inbox/group:infra",
        ],
    );
    s.account_with(
        "alice",
        &[
            "group.subscribe on service:inbox/group:infra",
            "group.subscribe on service:inbox/group:other",
        ],
    );
    let alice = enroll_as(&s, "Pixel 8 of Alice", "alice").await.unwrap();
    let (mut live, _) = Conn::open(s.port, &alice).await.unwrap();
    live.subscribe("infra").await;
    live.subscribe("other").await;
    let alice2 = enroll_as(&s, "Old phone with a long name of the owner", "alice")
        .await
        .unwrap();
    {
        let (mut c, _) = Conn::open(s.port, &alice2).await.unwrap();
        c.subscribe("infra").await;
    }
    let root = enroll_as(&s, "Galaxy of root", SUPERADMIN).await.unwrap();
    {
        let (mut c, _) = Conn::open(s.port, &root).await.unwrap();
        c.subscribe("crit").await;
    }
    let gone = enroll_as(&s, "Lost phone", "alice").await.unwrap();
    admin(
        &s,
        "POST",
        &format!("/auth/inbox/api/devices/{}/revoke", gone.device_id),
        "{}",
    );
    for (n, title) in [
        "Disk full on db-1",
        "Backup done",
        "A very long title of an alert that goes on and on to see how the table handles it in the cell",
    ]
    .iter()
    .enumerate()
    {
        let (st, _) = post_alert(&s, &format!("d{n}"), json!({"group": "infra", "title": title}));
        assert_eq!(st, 201);
        let got = take(&mut live).await;
        assert_eq!(&got.title, title);
    }
    let (st, _) = post_alert(
        &s,
        "pending",
        json!({"group": "infra", "title": "Waits for the old phone"}),
    );
    assert_eq!(st, 201);
    let _ = take(&mut live).await;
    std::fs::write(
        out.join("demo.json"),
        json!({"port": s.admin_port, "password": ACCOUNT_PASSWORD, "root": SUPERADMIN, "root_password": SUPERADMIN_PASSWORD}).to_string(),
    )
    .unwrap();
    let deadline = Instant::now() + Duration::from_secs(1800);
    while !out.join("stop").exists() && Instant::now() < deadline {
        // The live phone answers the heartbeat of the server.
        let _ = live.recv(Duration::from_millis(500)).await;
    }
    s.stop();
}
