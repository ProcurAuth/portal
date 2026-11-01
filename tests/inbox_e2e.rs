//! Black-box end-to-end test of the inbox: it runs the real `portal` binary with a combined
//! configuration and drives it with the real protocol 4 of the phones. A phone enrolls as the app
//! does: an account of the portal makes a code (`POST /auth/inbox/api/enrollment-codes`), and the
//! phone uses it on `/v1/enroll` (Noise IK, no OPAQUE). Then it opens Noise sessions. The admin
//! calls use the session of the superadmin.

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

/// The session of the superadmin: the cookie and the token of the admin calls that change data.
struct AdminSession {
    cookie: String,
    csrf: String,
}

struct Server {
    dir: PathBuf,
    /// Name of this instance: its configuration and log files.
    name: String,
    /// The listener of the phones and the producers.
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
    trusted_proxies = []
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
        if login == SUPERADMIN {
            return self.session();
        }
        if let Some(session) = self.accounts.lock().unwrap().get(login) {
            return session.clone();
        }
        add_account(self, login);
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

/// Signs in on the listener of the portal, and asks for the anti-CSRF token of the calls of the
/// inbox.
fn sign_in_as(port: u16, login: &str, password: &str) -> AdminSession {
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
    let (st, head, body) = http_full(
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
    );
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
    let (st, _, body) = http_full(
        port,
        &host,
        "GET",
        "/auth/inbox/api/csrf",
        &[("Cookie", cookie.clone())],
        "",
    );
    // Without the block `inbox` the call does not exist: the session has no token for it.
    if st == 404 {
        return AdminSession {
            cookie,
            csrf: String::new(),
        };
    }
    assert_eq!(st, 200, "{body}");
    let csrf = serde_json::from_str::<Value>(&body).unwrap()["csrf"]
        .as_str()
        .unwrap()
        .to_string();
    AdminSession { cookie, csrf }
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

/// The superadmin makes a database account. An account that exists already is not an error.
fn add_account(s: &Server, login: &str) {
    users_form(
        s,
        "/auth/users/add",
        &format!("login={login}&password={ACCOUNT_PASSWORD}"),
    );
}

/// The superadmin deletes a database account.
fn delete_account(s: &Server, login: &str) {
    let location = users_form(s, "/auth/users/delete", &format!("login={login}"));
    assert!(location.contains("s=user-deleted"), "{location}");
    s.accounts.lock().unwrap().remove(login);
}

/// An account makes an enrollment code through the portal, as its owner does before he types it on
/// the phone. The answer is the status and the JSON body.
fn make_code(s: &Server, login: &str) -> (u16, Value) {
    let session = s.account(login);
    let host = format!("127.0.0.1:{}", s.admin_port);
    let (st, text) = http_host(
        s.admin_port,
        &host,
        "POST",
        "/auth/inbox/api/enrollment-codes",
        &[
            ("Cookie", session.cookie.clone()),
            ("Content-Type", "application/json".into()),
            ("Origin", format!("http://{host}")),
            ("X-CSRF-Token", session.csrf.clone()),
        ],
        "{}",
    );
    (st, serde_json::from_str(&text).unwrap_or(Value::Null))
}

/// The text of a new code of an account.
fn code_for(s: &Server, login: &str) -> String {
    let (st, v) = make_code(s, login);
    assert_eq!(st, 201, "{v}");
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

/// An admin call of the inbox, with the session of the superadmin. A POST carries the
/// anti-CSRF token, the origin and the JSON content type.
fn admin(s: &Server, method: &str, path: &str, body: &str) -> (u16, Value) {
    let session = s.session();
    let host = format!("127.0.0.1:{}", s.admin_port);
    let mut headers: Vec<(&str, String)> = vec![("Cookie", session.cookie.clone())];
    if method == "POST" {
        headers.push(("Content-Type", "application/json".into()));
        headers.push(("Origin", format!("http://{host}")));
        headers.push(("X-CSRF-Token", session.csrf.clone()));
    }
    let (st, text) = http_host(s.admin_port, &host, method, path, &headers, body);
    (st, serde_json::from_str(&text).unwrap_or(Value::Null))
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
        port,
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
    let (st, body) = http(
        port,
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
        port,
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

    // Admin: the session of the superadmin, the origin, the content type and the anti-CSRF token; revocation.
    let ap = s.admin_port;
    let host = format!("127.0.0.1:{ap}");
    let (st, body) = http_host(ap, &host, "GET", "/auth/inbox/api/overview", &[], "");
    assert_eq!(st, 401, "no session, no admin call: {body}");
    assert!(body.contains("unauthorized"), "{body}");
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
    assert_eq!(a_row["live"]["ip"], "127.0.0.1");
    assert_eq!(a_row["device"]["user_name"], "alice");
    let b_row = devices
        .iter()
        .find(|d| d["device"]["name"] == "phone-b")
        .unwrap();
    assert_eq!(b_row["device"]["user_name"], "bob");
    let session = s.session();
    let send = json!({"group": "other", "title": "manual", "idempotency_key": "m1"}).to_string();
    let post = |extra: &[(&str, String)]| {
        let mut headers = vec![("Cookie", session.cookie.clone())];
        headers.extend_from_slice(extra);
        http_host(ap, &host, "POST", "/auth/inbox/api/alerts", &headers, &send)
    };
    let origin = ("Origin", format!("http://{host}"));
    let json_type = ("Content-Type", "application/json".to_string());
    let token = ("X-CSRF-Token", session.csrf.clone());
    let (st, _) = post(&[json_type.clone(), token.clone()]);
    assert_eq!(st, 403, "POST without Origin");
    let (st, _) = post(&[
        json_type.clone(),
        token.clone(),
        ("Origin", "http://evil.example".into()),
    ]);
    assert_eq!(st, 403, "POST from another origin");
    let (st, _) = post(&[json_type.clone(), origin.clone()]);
    assert_eq!(st, 403, "POST without the anti-CSRF token");
    let (st, _) = post(&[
        json_type.clone(),
        origin.clone(),
        ("X-CSRF-Token", "x".repeat(43)),
    ]);
    assert_eq!(st, 403, "POST with a wrong anti-CSRF token");
    let (st, _) = post(&[
        ("Content-Type", "text/plain".into()),
        origin.clone(),
        token.clone(),
    ]);
    assert_eq!(st, 415, "POST without JSON content type");
    let (st, _) = http_host(
        ap,
        &host,
        "POST",
        "/auth/inbox/api/alerts",
        &[json_type.clone(), origin.clone(), token.clone()],
        &send,
    );
    assert_eq!(st, 401, "POST without session");
    let (st, v) = admin(&s, "POST", "/auth/inbox/api/alerts", &send);
    assert_eq!(st, 201, "{v}");
    assert_eq!(v["targets"], 0);
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
    let newest = all[0]["created_at"].as_i64().unwrap();
    let (_, v) = admin(
        &s,
        "GET",
        &format!("/auth/inbox/api/alerts?since_ms={}", newest + 1),
        "",
    );
    assert!(
        v["alerts"].as_array().unwrap().is_empty(),
        "since_ms filters older alerts"
    );
    let (_, v) = admin(
        &s,
        "GET",
        &format!("/auth/inbox/api/alerts?since_ms={newest}"),
        "",
    );
    assert!(!v["alerts"].as_array().unwrap().is_empty());
    let (st, _) = admin(&s, "POST", "/auth/inbox/api/login/start", "{}");
    assert_eq!(st, 404, "no login endpoint any more");
    let (st, _) = admin(&s, "GET", "/auth/inbox/api/devices", "");
    assert_eq!(st, 404);
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
    // The proxy that sets the address of the client is a trusted proxy of the listener of the phones.
    let cfg = std::fs::read_to_string(s.config()).unwrap();
    std::fs::write(
        s.config(),
        cfg.replace(
            "    trusted_proxies = []",
            "    trusted_proxies = [\"127.0.0.1/32\"]",
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
        port,
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
        port,
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
    let (st, _) = http(
        port,
        "POST",
        "/api/v1/notify/infra",
        std::slice::from_ref(&auth),
        "",
    );
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
    let (st, _) = http(
        port,
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
        port,
        "POST",
        "/api/v1/alerts",
        std::slice::from_ref(&auth),
        &body,
    );
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

    // The admin calls are not on the listener of the phones, and the portal needs a session for them.
    let (st, _) = http(port, "GET", "/auth/inbox/api/overview", &[], "");
    assert_eq!(st, 404);
    let (st, _) = http_host(
        s.admin_port,
        &format!("127.0.0.1:{}", s.admin_port),
        "GET",
        "/auth/inbox/api/overview",
        &[],
        "",
    );
    assert_eq!(st, 401);
    // The producer token does not open the admin calls.
    let (st, _) = http_host(
        s.admin_port,
        &format!("127.0.0.1:{}", s.admin_port),
        "GET",
        "/auth/inbox/api/overview",
        std::slice::from_ref(&auth),
        "",
    );
    assert_eq!(st, 401);
    // The alert of the call without a key is still waiting for its receipt.
    let got = c.alert().await;
    assert_eq!(got.title, "nokey");
    c.ack(&got.alert_id, AckStage::Received).await;
    // A call of the superadmin sends an alert to the group.
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

    // The rules of the call: a session, a POST with the origin, JSON and the token, an empty object.
    let session = s.session();
    let origin = ("Origin", format!("http://{host}"));
    let json_type = ("Content-Type", "application/json".to_string());
    let call = |method: &str, headers: &[(&str, String)], body: &str| {
        http_host(
            admin_port,
            &host,
            method,
            "/auth/inbox/api/enrollment-codes",
            headers,
            body,
        )
        .0
    };
    let cookie = ("Cookie", session.cookie.clone());
    let token = ("X-CSRF-Token", session.csrf.clone());
    assert_eq!(
        call(
            "POST",
            &[json_type.clone(), origin.clone(), token.clone()],
            "{}"
        ),
        401,
        "no session"
    );
    assert_eq!(call("GET", std::slice::from_ref(&cookie), ""), 405);
    assert_eq!(
        call(
            "POST",
            &[cookie.clone(), json_type.clone(), token.clone()],
            "{}"
        ),
        403,
        "no origin"
    );
    assert_eq!(
        call(
            "POST",
            &[cookie.clone(), json_type.clone(), origin.clone()],
            "{}"
        ),
        403,
        "no token"
    );
    assert_eq!(
        call(
            "POST",
            &[
                cookie.clone(),
                json_type.clone(),
                origin.clone(),
                ("X-CSRF-Token", "x".repeat(43))
            ],
            "{}"
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
                origin.clone(),
                token.clone()
            ],
            "{}"
        ),
        415
    );
    let full = [
        cookie.clone(),
        json_type.clone(),
        origin.clone(),
        token.clone(),
    ];
    assert_eq!(
        call("POST", &full, r#"{"login":"bob"}"#),
        400,
        "a code is for the login of the session only"
    );
    assert_eq!(call("POST", &full, "not json"), 400);

    // The answer: the text of the code, its end and the fingerprint of the key.
    let before = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64;
    let (st, v) = make_code(&s, SUPERADMIN);
    assert_eq!(st, 201, "{v}");
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
    // The inbox has no admin call of its own for a plain account, but the code and the token of the call.
    let bob_session = s.account("bob");
    let (st, _, _) = http_full(
        admin_port,
        &host,
        "GET",
        "/auth/inbox/api/overview",
        &[("Cookie", bob_session.cookie.clone())],
        "",
    );
    assert_eq!(st, 403, "the admin calls are for the superadmin");

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
    assert_eq!(st, 429, "{v}");
    assert_eq!(v["error"]["code"], "too_many_codes", "{v}");
    // Another account has its own count.
    assert_eq!(make_code(&s, SUPERADMIN).0, 201);
    // A used code frees a place. An expired code does too.
    enroll_code(&codes[0], "phone-dave").await.unwrap();
    assert_eq!(make_code(&s, "dave").0, 201);
    assert_eq!(make_code(&s, "dave").0, 429);
    expire_codes(&s);
    assert_eq!(make_code(&s, "dave").0, 201);
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

/// Without the block `inbox` the call does not exist, and an account can be deleted as before.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn without_the_inbox_block_the_call_is_absent() {
    let mut s = Server::setup();
    let text = std::fs::read_to_string(s.config()).unwrap();
    // The block `inbox` goes. The block `postgres`, if any, stays.
    let cut = text.find("inbox {").unwrap();
    let end = text.find("\npostgres {").unwrap_or(text.len());
    std::fs::write(s.config(), format!("{}{}", &text[..cut], &text[end..])).unwrap();
    s.start();
    let host = format!("127.0.0.1:{}", s.admin_port);
    let session = s.session();
    let (st, _, body) = http_full(
        s.admin_port,
        &host,
        "POST",
        "/auth/inbox/api/enrollment-codes",
        &[
            ("Cookie", session.cookie.clone()),
            ("Content-Type", "application/json".into()),
            ("Origin", format!("http://{host}")),
            ("X-CSRF-Token", session.csrf.clone()),
        ],
        "{}",
    );
    assert_eq!(st, 404, "{body}");
    add_account(&s, "erin");
    delete_account(&s, "erin");
    s.stop();
}
