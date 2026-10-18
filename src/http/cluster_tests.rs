//! The portal as a whole, driven over HTTP. Traefik and a browser drive it in the same way.
//! With PostgreSQL, two instances share one database and one signing key, as the configuration demands.
//! With SQLite, there is one database. Both "instances" of a test use it.
//! The tests start real servers on local ports. The blocking calls to the database therefore run on the async workers, as in production.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::sync::OnceLock;
use std::time::Duration;

use super::*;
use crate::grants::Grant;
use crate::store::testing::{each_backend, Backend};

const PASSWORD: &str = "correct-horse-battery-staple";
const SECRET: [u8; 32] = [b'k'; 32];
const AGENT: &str = "ClusterTest/1.0";
const PDP_DB_TOKEN: &str = "the PDP token of the server db, 32 bytes or more";
const PDP_CHAT_TOKEN: &str = "the PDP token of the server chat, 32 bytes or more";

struct Reply {
    status: u16,
    headers: Vec<(String, String)>,
    body: String,
}

impl Reply {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(header, _)| header == name)
            .map(|(_, value)| value.as_str())
    }

    fn json(&self) -> serde_json::Value {
        serde_json::from_str(&self.body).unwrap_or_else(|e| panic!("{e}: {}", self.body))
    }

    /// Gives the value of the cookie `name` in a `Set-Cookie` header.
    fn cookie(&self, name: &str) -> Option<String> {
        self.headers
            .iter()
            .filter(|(header, _)| header == "set-cookie")
            .find_map(|(_, value)| {
                let (cookie, _) = value.split_once(';').unwrap_or((value, ""));
                cookie.strip_prefix(&format!("{name}=")).map(str::to_string)
            })
            .filter(|value| !value.is_empty())
    }

    fn location(&self) -> Option<&str> {
        self.headers
            .iter()
            .find(|(header, _)| header == "location")
            .map(|(_, value)| value.as_str())
    }
}

fn send(port: u16, method: &str, path: &str, headers: &[(&str, &str)], body: &str) -> Reply {
    let stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
    send_on(stream, port, method, path, headers, body)
}

/// The same as [`send`], from the local address `source` (for example `127.0.0.2`). The server
/// then sees another TCP peer.
fn send_from(
    source: &str,
    port: u16,
    method: &str,
    path: &str,
    headers: &[(&str, &str)],
    body: &str,
) -> Reply {
    send_on(
        connect_from(source, port),
        port,
        method,
        path,
        headers,
        body,
    )
}

fn connect_from(source: &str, port: u16) -> TcpStream {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_io()
        .build()
        .unwrap();
    let source: std::net::IpAddr = source.parse().unwrap();
    runtime.block_on(async {
        let socket = tokio::net::TcpSocket::new_v4().unwrap();
        socket.bind((source, 0).into()).unwrap();
        let stream = socket.connect(([127, 0, 0, 1], port).into()).await.unwrap();
        let stream = stream.into_std().unwrap();
        stream.set_nonblocking(false).unwrap();
        stream
    })
}

fn send_on(
    mut stream: TcpStream,
    port: u16,
    method: &str,
    path: &str,
    headers: &[(&str, &str)],
    body: &str,
) -> Reply {
    let mut request = format!(
        "{method} {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\nContent-Length: {}\r\n",
        body.len()
    );
    if !headers
        .iter()
        .any(|(name, _)| name.eq_ignore_ascii_case("user-agent"))
    {
        request.push_str(&format!("User-Agent: {AGENT}\r\n"));
    }
    for (name, value) in headers {
        request.push_str(&format!("{name}: {value}\r\n"));
    }
    request.push_str("\r\n");
    request.push_str(body);
    stream
        .set_read_timeout(Some(Duration::from_secs(20)))
        .unwrap();
    stream.write_all(request.as_bytes()).unwrap();
    let mut raw = Vec::new();
    stream.read_to_end(&mut raw).unwrap();
    let text = String::from_utf8_lossy(&raw);
    let (head, body) = text.split_once("\r\n\r\n").expect("a complete response");
    let mut lines = head.lines();
    let status = lines
        .next()
        .unwrap()
        .split_whitespace()
        .nth(1)
        .unwrap()
        .parse()
        .unwrap();
    let headers = lines
        .filter_map(|line| line.split_once(':'))
        .map(|(name, value)| (name.trim().to_ascii_lowercase(), value.trim().to_string()))
        .collect();
    Reply {
        status,
        headers,
        body: body.to_string(),
    }
}

#[derive(Clone)]
struct Node {
    port: u16,
    state: Arc<AppState>,
}

/// The Argon2id hash of `password`. Each password is hashed once for all the tests.
fn password_hash(password: &str) -> String {
    static HASHES: OnceLock<std::sync::Mutex<std::collections::HashMap<String, String>>> =
        OnceLock::new();
    HASHES
        .get_or_init(Default::default)
        .lock()
        .unwrap()
        .entry(password.to_string())
        .or_insert_with(|| hash_password(password.as_bytes()).unwrap())
        .clone()
}

fn start(store: Arc<Store>, cache_ttl: Option<Duration>) -> Node {
    start_with(store, cache_ttl, "")
}

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

/// A node with more configuration. The `extra` text is added to the file.
/// The tests use these variables:
/// `TEST_IDENTITY_KEY` (a new Ed25519 key), `TEST_PDP_DB` and `TEST_PDP_CHAT` (PDP tokens).
fn start_with(store: Arc<Store>, cache_ttl: Option<Duration>, extra: &str) -> Node {
    start_custom(store, cache_ttl, Custom::default(), extra)
}

/// The parts of the harness configuration that a test can replace.
struct Custom {
    /// More keys of the `server` block.
    server: &'static str,
    /// The body of the `discovery` block.
    discovery: &'static str,
    /// The superadmin login, its password and the session secret.
    login: &'static str,
    password: &'static str,
    session_secret: &'static str,
    /// Wait for a complete catalogue before the node is given to the test.
    wait_complete: bool,
}

impl Default for Custom {
    fn default() -> Self {
        Self {
            server: r#"trusted_proxies = ["127.0.0.1/32"]"#,
            discovery: r#"interval = "1h""#,
            login: "root",
            password: PASSWORD,
            session_secret: "0123456789abcdef0123456789abcdef",
            wait_complete: true,
        }
    }
}

fn start_custom(
    store: Arc<Store>,
    cache_ttl: Option<Duration>,
    custom: Custom,
    extra: &str,
) -> Node {
    let port = free_port();
    let key = crate::identity::testing::new_key_pem();
    let Custom {
        server,
        discovery,
        login,
        password,
        session_secret,
        wait_complete,
    } = custom;
    let (config, mut secrets) = Config::parse(
        &format!(
            r#"
        server {{ listen_address = "127.0.0.1:{port}" workers = 2 {server} }}
        authentication {{
          superadmin_login_env    = "PORTAL_TEST_LOGIN"
          superadmin_password_env = "PORTAL_TEST_PASSWORD"
        }}
        users {{ max_users = 10 }}
        session {{ secret_mode = "provided" secret_env = "PORTAL_TEST_SESSION" }}
        discovery {{ {discovery} }}
        {extra}
        "#
        ),
        &|name| match name {
            "PORTAL_TEST_LOGIN" => Some(login.to_string()),
            "PORTAL_TEST_PASSWORD" => Some(password.to_string()),
            "PORTAL_TEST_SESSION" => Some(session_secret.to_string()),
            "TEST_IDENTITY_KEY" => Some(key.clone()),
            "TEST_PDP_DB" => Some(PDP_DB_TOKEN.to_string()),
            "TEST_PDP_CHAT" => Some(PDP_CHAT_TOKEN.to_string()),
            _ => None,
        },
    )
    .unwrap();
    let tracker = Tracker::start(Arc::clone(&store), Duration::from_millis(200), 90).unwrap();
    // The signing secret of the tests is SECRET. A test that rotates it gives another session secret.
    let signing_secret = if session_secret == Custom::default().session_secret {
        SECRET.to_vec()
    } else {
        session_secret.as_bytes().to_vec()
    };
    let superadmin = SuperAdmin {
        login: login.into(),
        password_hash: password_hash(password),
        generation: crate::crypto::superadmin_generation(
            &signing_secret,
            login,
            password.as_bytes(),
        )
        .unwrap(),
    };
    let access = AccessSecrets {
        identity_key: secrets.identity_key.take(),
        pdp_tokens: std::mem::take(&mut secrets.pdp_tokens),
    };
    let mut state =
        AppState::new(config, superadmin, signing_secret, store, tracker, access).unwrap();
    if let Some(ttl) = cache_ttl {
        state.sessions.set_cache_ttl(ttl);
    }
    let state = Arc::new(state);
    let serving = Arc::clone(&state);
    std::thread::spawn(move || {
        let _ = serve(serving);
    });
    let node = Node { port, state };
    wait_until(Duration::from_secs(10), || {
        TcpStream::connect(("127.0.0.1", port)).is_ok()
            && send(port, "GET", "/healthz", &[], "").status == 200
    });
    // The first discovery round publishes an empty catalogue. The next round starts an hour later.
    wait_until(Duration::from_secs(10), || {
        node.state.discovery.refreshes() >= 1
    });
    if wait_complete {
        assert!(
            current_catalog(&node.state).is_complete(),
            "the first round of a harness without a source is complete"
        );
    }
    node
}

/// Makes two nodes on one database, or the one SQLite node twice.
fn cluster(backend: &Backend, cache_ttl: Option<Duration>) -> (Node, Node) {
    match backend {
        Backend::Sqlite => {
            let node = start(Arc::new(backend.fresh()), cache_ttl);
            (node.clone(), node)
        }
        Backend::Postgres(_) => {
            let (first, second) = backend.fresh_pair();
            (
                start(Arc::new(first), cache_ttl),
                start(Arc::new(second), cache_ttl),
            )
        }
    }
}

fn wait_until(limit: Duration, mut condition: impl FnMut() -> bool) -> Duration {
    let begun = Instant::now();
    while !condition() {
        assert!(begun.elapsed() < limit, "still waiting after {limit:?}");
        std::thread::sleep(Duration::from_millis(100));
    }
    begun.elapsed()
}

fn session_cookie_name(node: &Node) -> String {
    transport::session_cookie_name(&node.state.config.session.cookie_name, false).to_string()
}

/// Signs in as the login form does. It returns the session cookie.
fn sign_in(node: &Node, login: &str, password: &str) -> String {
    let csrf = "A".repeat(32);
    let signature = sign_return_uri(&node.state.signing_secret, "/").unwrap();
    let origin = format!("http://127.0.0.1:{}", node.port);
    let cookie = format!("{}={csrf}", transport::csrf_cookie_name(false));
    let reply = send(
        node.port,
        "POST",
        LOGIN_PATH,
        &[
            ("Origin", &origin),
            ("Content-Type", "application/x-www-form-urlencoded"),
            ("Cookie", &cookie),
        ],
        &format!("login={login}&password={password}&csrf={csrf}&rd=%2F&sig={signature}"),
    );
    assert_eq!(reply.status, 303, "signing in as {login}");
    reply
        .cookie(&session_cookie_name(node))
        .expect("a session cookie")
}

/// The result that Traefik gets from ForwardAuth. 204 lets the request through. 302 sends it to the login page.
fn verify(node: &Node, session: &str, agent: &str) -> u16 {
    let cookie = format!("{}={session}", session_cookie_name(node));
    send(
        node.port,
        "GET",
        VERIFY_PATH,
        &[
            ("X-Forwarded-Uri", "/app/"),
            ("X-Forwarded-Method", "GET"),
            ("Cookie", &cookie),
            ("User-Agent", agent),
        ],
        "",
    )
    .status
}

/// Makes a POST to the settings or to the launcher. The request has the anti-CSRF token that those forms carry.
fn post(node: &Node, session: &str, path: &str, purpose: &str, fields: &str) -> Reply {
    let origin = format!("http://127.0.0.1:{}", node.port);
    let cookie = format!("{}={session}", session_cookie_name(node));
    let csrf = sign_action_token(&node.state.signing_secret, purpose, session).unwrap();
    send(
        node.port,
        "POST",
        path,
        &[
            ("Origin", &origin),
            ("Content-Type", "application/x-www-form-urlencoded"),
            ("Cookie", &cookie),
        ],
        &if fields.is_empty() {
            format!("csrf={csrf}")
        } else {
            format!("csrf={csrf}&{fields}")
        },
    )
}

#[test]
fn a_session_opened_on_one_instance_is_valid_on_the_other_and_a_logout_ends_it_on_both() {
    each_backend(|backend| {
        let (a, b) = cluster(backend, None);
        let session = sign_in(&a, "root", PASSWORD);
        assert_eq!(verify(&a, &session, AGENT), 204);
        assert_eq!(
            verify(&b, &session, AGENT),
            204,
            "the other instance accepts it"
        );
        assert_eq!(
            verify(&b, &session, "SomeoneElse/9"),
            302,
            "the cookie is tied to the browser on every instance"
        );
        assert_eq!(
            verify(&b, &session, AGENT),
            204,
            "and the refused try ended nothing"
        );

        let reply = post(&a, &session, LOGOUT_PATH, LOGOUT_TOKEN, "");
        assert_eq!(reply.status, 303);
        assert_eq!(
            verify(&a, &session, AGENT),
            302,
            "ended on the instance that did it"
        );
        // The other instance can keep the check result for a few seconds. It cannot keep it longer.
        let waited = wait_until(Duration::from_secs(10), || {
            verify(&b, &session, AGENT) == 302
        });
        assert!(
            waited < Duration::from_secs(6),
            "{} took {waited:?} to notice the logout",
            backend.name()
        );

        // The history shows how it ended, whoever wrote it.
        wait_until(Duration::from_secs(10), || {
            let snapshot = b
                .state
                .store
                .snapshot(&[("root".into(), true)], unix_now())
                .unwrap();
            snapshot.accounts[0]
                .recent
                .first()
                .and_then(|s| s.end_reason.clone())
                .as_deref()
                == Some("logout")
        });
    });
}

#[test]
fn without_a_copy_a_logout_is_seen_by_the_other_instance_at_once() {
    each_backend(|backend| {
        let (a, b) = cluster(backend, Some(Duration::ZERO));
        let session = sign_in(&a, "root", PASSWORD);
        assert_eq!(verify(&b, &session, AGENT), 204);
        assert_eq!(
            post(&a, &session, LOGOUT_PATH, LOGOUT_TOKEN, "").status,
            303
        );
        assert_eq!(verify(&b, &session, AGENT), 302);
    });
}

#[test]
fn an_account_created_on_one_instance_signs_in_and_is_known_on_the_other() {
    each_backend(|backend| {
        let (a, b) = cluster(backend, Some(Duration::ZERO));
        let root = sign_in(&a, "root", PASSWORD);
        let add = |login: &str| {
            post(
                &a,
                &root,
                USERS_ADD_PATH,
                USERS_TOKEN,
                &format!("login={login}&password=a-long-password-1"),
            )
        };
        assert_eq!(add("carol").status, 303);
        assert!(a.state.users.get("carol").is_some());

        // The periodic refresh of the other instance loads it.
        crate::sync::run_round(&b.state, 1);
        assert!(
            b.state.users.grants_of("carol").unwrap().is_empty(),
            "a new user has no grant"
        );
        let on_b = sign_in(&b, "carol", "a-long-password-1");
        assert_eq!(
            verify(&a, &on_b, AGENT),
            204,
            "and her session is good on the first one"
        );

        // Do not wait for a refresh. An unknown login makes the instance search for it.
        assert_eq!(add("dave").status, 303);
        if b.state.users.get("dave").is_none() {
            std::thread::sleep(Duration::from_millis(1_100));
        }
        let dave = sign_in(&b, "dave", "a-long-password-1");
        assert_eq!(verify(&b, &dave, AGENT), 204);
        assert_eq!(verify(&a, &dave, AGENT), 204);
    });
}

#[test]
fn a_session_opened_for_an_account_the_instance_has_not_heard_of_yet_is_not_ended() {
    each_backend(|backend| {
        let (a, b) = cluster(backend, Some(Duration::ZERO));
        let root = sign_in(&a, "root", PASSWORD);
        let reply = post(
            &a,
            &root,
            USERS_ADD_PATH,
            USERS_TOKEN,
            "login=erin&password=a-long-password-1",
        );
        assert_eq!(reply.status, 303);
        let erin = sign_in(&a, "erin", "a-long-password-1");
        // `b` never refreshed its accounts. The session must not be accepted as one of a deleted account.
        std::thread::sleep(Duration::from_millis(1_100));
        assert_eq!(verify(&b, &erin, AGENT), 204);
        assert_eq!(verify(&a, &erin, AGENT), 204);
    });
}

#[test]
fn deleting_an_account_ends_its_sessions_on_every_instance() {
    each_backend(|backend| {
        let (a, b) = cluster(backend, Some(Duration::ZERO));
        let root = sign_in(&a, "root", PASSWORD);
        let reply = post(
            &a,
            &root,
            USERS_ADD_PATH,
            USERS_TOKEN,
            "login=frank&password=a-long-password-1",
        );
        assert_eq!(reply.status, 303);
        crate::sync::run_round(&b.state, 1);
        let frank = sign_in(&b, "frank", "a-long-password-1");
        assert_eq!(verify(&b, &frank, AGENT), 204);

        let reply = post(&a, &root, USERS_DELETE_PATH, USERS_TOKEN, "login=frank");
        assert_eq!(reply.status, 303, "{:?}", reply.location());
        assert_eq!(
            verify(&b, &frank, AGENT),
            302,
            "the session is gone, wherever it was opened"
        );
        assert_eq!(verify(&a, &frank, AGENT), 302);
        assert_eq!(
            verify(&a, &root, AGENT),
            204,
            "the superadmin is not affected"
        );
        crate::sync::run_round(&b.state, 2);
        assert!(b.state.users.get("frank").is_none());
    });
}

#[test]
fn a_restart_keeps_the_sessions_of_a_shared_database_and_drops_those_of_a_file() {
    each_backend(|backend| {
        let store = Arc::new(backend.fresh());
        let first = start(Arc::clone(&store), None);
        let session = sign_in(&first, "root", PASSWORD);
        assert_eq!(verify(&first, &session, AGENT), 204);
        // The same database is used by a new process. This is a restart.
        let second = start(Arc::clone(&store), None);
        let expected = if store.is_shared() { 204 } else { 302 };
        assert_eq!(
            verify(&second, &session, AGENT),
            expected,
            "{}",
            backend.name()
        );
    });
}

// Grants, identity token, launcher, refusal page and PDP ----------------------------------------

/// A service of the catalogue. Discovery makes the service from labels.
fn listed(labels: &[(&str, &str)]) -> crate::catalog::Service {
    let map = labels
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
    crate::catalog::parse_labels("test", &map).unwrap().unwrap()
}

/// The URL of the actions of the service `db` in the tests. The test fills the registry.
const DB_ACTIONS_URL: &str = "http://test/.well-known/access-actions";

/// The catalogue has two services. Grafana (`other-grafana`, only `access`) and Db (`db`, with actions).
fn publish_catalog(node: &Node, db_actions: &[&str]) {
    let catalog = Catalog::build(
        vec![
            listed(&[("portal.name", "Grafana"), ("portal.href", "/grafana/")]),
            listed(&[
                ("portal.name", "Db"),
                ("portal.id", "db"),
                ("portal.href", "/db/"),
                ("portal.actions", DB_ACTIONS_URL),
            ]),
        ],
        Vec::new(),
    );
    *node.state.catalog.write().unwrap() = Arc::new(catalog);
    node.state.actions.set(
        "db",
        db_actions
            .iter()
            .map(|name| crate::actions::Action {
                name: name.to_string(),
                resource_types: vec!["service".into(), "database".into()],
                description: format!("The action {name}."),
            })
            .collect(),
    );
}

/// The result that Traefik gets from ForwardAuth for a browser that opens `uri`.
fn open(node: &Node, session: &str, uri: &str) -> Reply {
    let cookie = format!("{}={session}", session_cookie_name(node));
    send(
        node.port,
        "GET",
        VERIFY_PATH,
        &[
            ("X-Forwarded-Uri", uri),
            ("X-Forwarded-Method", "GET"),
            ("Cookie", &cookie),
        ],
        "",
    )
}

fn page(node: &Node, session: &str, path: &str) -> Reply {
    let cookie = format!("{}={session}", session_cookie_name(node));
    send(node.port, "GET", path, &[("Cookie", &cookie)], "")
}

/// Gives the claims of a JWT. The function does not check the signature.
fn claims(token: &str) -> serde_json::Value {
    use base64ct::{Base64UrlUnpadded, Encoding};
    let part = token.split('.').nth(1).unwrap();
    serde_json::from_slice(&Base64UrlUnpadded::decode_vec(part).unwrap()).unwrap()
}

#[test]
fn grants_open_services_give_an_identity_token_and_the_pages_hide_the_other_services() {
    each_backend(|backend| {
        let node = start_with(
            Arc::new(backend.fresh()),
            Some(Duration::ZERO),
            r#"identity { issuer = "https://portal.test" signing_key_env = "TEST_IDENTITY_KEY" token_ttl = "5m" }"#,
        );
        publish_catalog(&node, &["query.run.read", "query.run.write"]);
        let root = sign_in(&node, "root", PASSWORD);
        let add = post(
            &node,
            &root,
            USERS_ADD_PATH,
            USERS_TOKEN,
            "login=alice&password=a-long-password-1",
        );
        assert_eq!(add.location(), Some("/auth/settings?s=user-added"));
        let refused = post(
            &node,
            &root,
            USERS_ADD_PATH,
            USERS_TOKEN,
            "login=bob&password=a-long-password-1&g.db.access=1",
        );
        assert_eq!(refused.status, 400, "a new user is added without grants");

        // A new user has no grant. The user has no service, and the refusal names none.
        let alice = sign_in(&node, "alice", "a-long-password-1");
        let denied = open(&node, &alice, "/grafana/d/1");
        assert_eq!(denied.status, 302);
        assert_eq!(
            denied.location(),
            Some(DENIED_PATH),
            "no service in the URL"
        );
        assert_eq!(open(&node, &alice, "/db/").status, 302);
        let launcher = page(&node, &alice, "/");
        assert!(launcher.body.contains("No service available"));
        assert!(!launcher.body.contains("Grafana") && !launcher.body.contains(">Db<"));
        let refusal = page(&node, &alice, DENIED_PATH);
        assert_eq!(refusal.status, 403);
        assert!(!refusal.body.contains("Grafana") && !refusal.body.contains("Db"));

        // The superadmin gives access and two actions on db.
        let update = |fields: &str| {
            post(
                &node,
                &root,
                USERS_UPDATE_PATH,
                USERS_TOKEN,
                &format!("login=alice{fields}"),
            )
        };
        let reply = update("&g.db.access=1&g.db.query.run.read=1&g.db.query.run.write=1");
        assert_eq!(reply.location(), Some("/auth/settings?s=user-updated"));
        let allowed = open(&node, &alice, "/db/query?x=1");
        assert_eq!(allowed.status, 204);
        let token = allowed
            .header("x-identity-token")
            .expect("an identity token")
            .to_string();
        let c = claims(&token);
        assert_eq!(c["iss"], "https://portal.test");
        assert_eq!(c["sub"], "alice");
        assert_eq!(c["aud"], "db");
        assert_eq!(c["exp"].as_i64().unwrap() - c["iat"].as_i64().unwrap(), 300);
        assert_eq!(
            open(&node, &alice, "/db/other").header("x-identity-token"),
            Some(token.as_str()),
            "one token for each session and audience"
        );
        assert_eq!(
            open(&node, &alice, "/grafana/").status,
            302,
            "still no access to Grafana"
        );
        let launcher = page(&node, &alice, "/");
        assert!(launcher.body.contains(">Db<") && !launcher.body.contains("Grafana"));
        // A path that no service owns gets no token.
        let free = open(&node, &alice, "/app.js");
        assert_eq!(free.status, 204);
        assert!(free.header("x-identity-token").is_none());

        // The server refuses a grant that the page did not offer.
        let reply = update("&g.other-grafana.dashboards.read=1");
        assert_eq!(
            reply.location(),
            Some("/auth/settings?s=edit-unknown-grant")
        );

        // An action that db no longer publishes keeps its grant. The page did not show the action.
        publish_catalog(&node, &["query.run.read"]);
        assert_eq!(
            update("&g.db.access=1").location(),
            Some("/auth/settings?s=user-updated")
        );
        let kept: Vec<String> = node
            .state
            .users
            .grants_of("alice")
            .unwrap()
            .iter()
            .map(ToString::to_string)
            .collect();
        assert_eq!(
            kept,
            ["access on service:db", "query.run.write on service:db"]
        );
        // The database holds the same grants.
        let stored = node.state.store.load_users().unwrap();
        assert_eq!(
            stored[0].grants,
            node.state.users.grants_of("alice").unwrap()
        );

        // Without access, the service is removed again.
        assert_eq!(update("").location(), Some("/auth/settings?s=user-updated"));
        assert_eq!(open(&node, &alice, "/db/").status, 302);

        // The superadmin opens every service. It gets a token with its own login.
        let as_root = open(&node, &root, "/grafana/");
        assert_eq!(as_root.status, 204);
        assert_eq!(
            claims(as_root.header("x-identity-token").unwrap())["sub"],
            "root"
        );
        assert_eq!(
            claims(as_root.header("x-identity-token").unwrap())["aud"],
            "other-grafana"
        );
    });
}

#[test]
fn in_the_static_mode_only_the_configuration_gives_grants() {
    let node = start_with(
        Arc::new(Backend::Sqlite.fresh()),
        None,
        r#"access { grants_mode = "static" }
        user "alice" { grants = ["access on service:other-grafana"] }"#,
    );
    publish_catalog(&node, &[]);
    let root = sign_in(&node, "root", PASSWORD);
    post(
        &node,
        &root,
        USERS_ADD_PATH,
        USERS_TOKEN,
        "login=alice&password=a-long-password-1",
    );
    let reply = post(
        &node,
        &root,
        USERS_UPDATE_PATH,
        USERS_TOKEN,
        "login=alice&g.db.access=1",
    );
    assert_eq!(reply.location(), Some("/auth/settings?s=grants-static"));
    // A grant in the database does not count in this mode.
    node.state
        .store
        .update_user("alice", None, Some(&[Grant::new("access", "db").unwrap()]))
        .unwrap();
    crate::sync::run_round(&node.state, 1);
    let alice = sign_in(&node, "alice", "a-long-password-1");
    assert_eq!(
        open(&node, &alice, "/grafana/").status,
        204,
        "the configuration gives it"
    );
    assert_eq!(
        open(&node, &alice, "/db/").status,
        302,
        "the database does not"
    );
}

/// A request to the PDP listener.
fn pdp(port: u16, path: &str, token: Option<&str>, body: &str, request_id: Option<&str>) -> Reply {
    let authorization = token.map(|t| format!("Bearer {t}"));
    let mut headers: Vec<(&str, &str)> = vec![("Content-Type", "application/json")];
    if let Some(value) = &authorization {
        headers.push(("Authorization", value));
    }
    if let Some(id) = request_id {
        headers.push(("X-Request-ID", id));
    }
    send(port, "POST", path, &headers, body)
}

fn evaluation(subject: &str, action: &str, kind: &str, id: &str) -> String {
    serde_json::json!({
        "subject": {"type": "user", "id": subject},
        "action": {"name": action},
        "resource": {"type": kind, "id": id},
        "context": {}
    })
    .to_string()
}

#[test]
fn the_pdp_answers_each_server_for_its_own_resources_and_fails_closed() {
    let pdp_port = free_port();
    let node = start_with(
        Arc::new(Backend::Sqlite.fresh()),
        None,
        &format!(
            r#"access {{ grants_mode = "hybrid" }}
            user "carol" {{ grants = ["query.run.read on service:db"] }}
            pdp {{ listen_address = "127.0.0.1:{pdp_port}" url = "http://pdp.test:{pdp_port}" max_evaluations = 4 }}
            pdp_client "db" {{ token_env = "TEST_PDP_DB" }}
            pdp_client "chat" {{ token_env = "TEST_PDP_CHAT" }}"#
        ),
    );
    publish_catalog(&node, &["query.run.read", "query.run.write", "system.read"]);
    wait_until(Duration::from_secs(10), || {
        TcpStream::connect(("127.0.0.1", pdp_port)).is_ok()
    });
    let grants =
        |list: &[&str]| -> Vec<Grant> { list.iter().map(|g| Grant::parse(g).unwrap()).collect() };
    for (login, held) in [
        (
            "alice",
            grants(&["access on service:db", "query.run.write on service:db"]),
        ),
        ("carol", Vec::new()),
    ] {
        let row = crate::store::UserRow {
            login: login.into(),
            password_hash: password_hash(PASSWORD),
            grants: held,
            created_at: 1,
        };
        node.state.store.insert_user(&row).unwrap();
    }
    crate::sync::run_round(&node.state, 1);

    let ask =
        |token: Option<&str>, body: &str| pdp(pdp_port, "/access/v1/evaluation", token, body, None);
    let decide = |token: &str, body: &str| -> bool {
        let reply = ask(Some(token), body);
        assert_eq!(reply.status, 200, "{}", reply.body);
        reply.json()["decision"].as_bool().unwrap()
    };

    // The token: none, a wrong one.
    let body = evaluation("alice", "query.run.write", "service", "db");
    let unauthorized = ask(None, &body);
    assert_eq!(unauthorized.status, 401);
    assert_eq!(
        unauthorized.header("www-authenticate"),
        Some("Bearer realm=\"portal-pdp\"")
    );
    assert_eq!(
        ask(Some("not a token of a server at all, 32 bytes"), &body).status,
        401
    );

    // Decisions for db.
    assert!(decide(PDP_DB_TOKEN, &body));
    assert!(
        decide(
            PDP_DB_TOKEN,
            &evaluation("alice", "query.run.write", "database", "analytics")
        ),
        "a grant on the service covers its resources"
    );
    assert!(
        !decide(
            PDP_DB_TOKEN,
            &evaluation("alice", "query.run.read", "service", "db")
        ),
        "no grant"
    );
    assert!(
        !decide(
            PDP_DB_TOKEN,
            &evaluation("alice", "access", "service", "chat")
        ),
        "a resource of another server"
    );
    assert!(
        decide(
            PDP_DB_TOKEN,
            &evaluation("carol", "query.run.read", "service", "db")
        ),
        "the configuration (hybrid)"
    );
    assert!(!decide(
        PDP_DB_TOKEN,
        &evaluation("carol", "query.run.write", "service", "db")
    ));
    assert!(
        !decide(
            PDP_DB_TOKEN,
            &evaluation("mallory", "access", "service", "db")
        ),
        "no such user"
    );
    assert!(
        decide(
            PDP_DB_TOKEN,
            &evaluation("root", "system.read", "service", "db")
        ),
        "the superadmin has all grants"
    );
    assert!(!decide(
        PDP_DB_TOKEN,
        &evaluation("alice", "Query.Run", "service", "db")
    ));
    let agent = serde_json::json!({
        "subject": {"type": "agent", "id": "alice"},
        "action": {"name": "access"},
        "resource": {"type": "service", "id": "db"}
    });
    assert!(!decide(PDP_DB_TOKEN, &agent.to_string()), "only users");
    // The same user for chat: no grant there.
    assert!(!decide(
        PDP_CHAT_TOKEN,
        &evaluation("alice", "access", "service", "chat")
    ));
    assert!(decide(
        PDP_CHAT_TOKEN,
        &evaluation("root", "access", "service", "chat")
    ));

    // A request that the PDP cannot read gets no decision.
    assert_eq!(ask(Some(PDP_DB_TOKEN), "{").status, 400);
    assert_eq!(
        ask(Some(PDP_DB_TOKEN), r#"{"subject":{"type":"user"}}"#).status,
        400
    );

    // The response returns X-Request-ID.
    let reply = pdp(
        pdp_port,
        "/access/v1/evaluation",
        Some(PDP_DB_TOKEN),
        &body,
        Some("req-42"),
    );
    assert_eq!(reply.header("x-request-id"), Some("req-42"));

    // The batch: defaults at the top, three semantics.
    let batch = |semantic: Option<&str>| {
        let mut body = serde_json::json!({
            "subject": {"type": "user", "id": "alice"},
            "resource": {"type": "service", "id": "db"},
            "evaluations": [
                {"action": {"name": "access"}},
                {"action": {"name": "system.read"}},
                {"action": {"name": "query.run.write"}}
            ]
        });
        if let Some(semantic) = semantic {
            body["options"] = serde_json::json!({"evaluations_semantic": semantic});
        }
        let reply = pdp(
            pdp_port,
            "/access/v1/evaluations",
            Some(PDP_DB_TOKEN),
            &body.to_string(),
            None,
        );
        assert_eq!(reply.status, 200, "{}", reply.body);
        reply.json()["evaluations"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| e["decision"].as_bool().unwrap())
            .collect::<Vec<bool>>()
    };
    assert_eq!(batch(None), [true, false, true]);
    assert_eq!(batch(Some("execute_all")), [true, false, true]);
    assert_eq!(batch(Some("deny_on_first_deny")), [true, false]);
    assert_eq!(batch(Some("permit_on_first_permit")), [true]);
    let too_many = serde_json::json!({
        "subject": {"type": "user", "id": "alice"},
        "action": {"name": "access"},
        "resource": {"type": "service", "id": "db"},
        "evaluations": [{}, {}, {}, {}, {}]
    });
    assert_eq!(
        pdp(
            pdp_port,
            "/access/v1/evaluations",
            Some(PDP_DB_TOKEN),
            &too_many.to_string(),
            None
        )
        .status,
        400
    );
    let single = pdp(
        pdp_port,
        "/access/v1/evaluations",
        Some(PDP_DB_TOKEN),
        &body,
        None,
    );
    assert_eq!(
        single.json()["decision"],
        true,
        "without evaluations: one decision"
    );

    // The action search.
    let search = |subject: &str| {
        let body = serde_json::json!({
            "subject": {"type": "user", "id": subject},
            "resource": {"type": "service", "id": "db"}
        });
        let reply = pdp(
            pdp_port,
            "/access/v1/search/action",
            Some(PDP_DB_TOKEN),
            &body.to_string(),
            None,
        );
        assert_eq!(reply.status, 200, "{}", reply.body);
        reply.json()["results"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| r["name"].as_str().unwrap().to_string())
            .collect::<Vec<String>>()
    };
    assert_eq!(search("alice"), ["access", "query.run.write"]);
    assert_eq!(
        search("root"),
        ["access", "query.run.read", "query.run.write", "system.read"]
    );
    assert!(search("mallory").is_empty());

    // The metadata needs no token. The server refuses the other methods.
    let metadata = send(
        pdp_port,
        "GET",
        "/.well-known/authzen-configuration",
        &[],
        "",
    );
    assert_eq!(metadata.status, 200);
    let m = metadata.json();
    assert_eq!(
        m["policy_decision_point"],
        format!("http://pdp.test:{pdp_port}")
    );
    assert_eq!(
        m["access_evaluations_endpoint"],
        format!("http://pdp.test:{pdp_port}/access/v1/evaluations")
    );
    assert_eq!(
        send(pdp_port, "GET", "/access/v1/evaluation", &[], "").status,
        405
    );
    // The PDP is not on the public listener.
    assert_eq!(
        pdp(
            node.port,
            "/access/v1/evaluation",
            Some(PDP_DB_TOKEN),
            &body,
            None
        )
        .status,
        404
    );
    let metrics = send(node.port, "GET", "/metrics", &[], "");
    assert!(metrics
        .body
        .contains("portal_pdp_decisions_total{decision=\"true\"}"));
}

#[test]
fn the_pdp_decides_on_sub_resources_with_the_service_of_the_server_that_asks() {
    let pdp_port = free_port();
    let node = start_with(
        Arc::new(Backend::Sqlite.fresh()),
        None,
        &format!(
            r#"access {{ grants_mode = "hybrid" }}
            user "carol" {{ grants = ["query.run.read on service:db/database:logs"] }}
            pdp {{ listen_address = "127.0.0.1:{pdp_port}" url = "http://pdp.test:{pdp_port}" max_evaluations = 4 }}
            pdp_client "db" {{ token_env = "TEST_PDP_DB" }}
            pdp_client "chat" {{ token_env = "TEST_PDP_CHAT" }}"#
        ),
    );
    publish_catalog(&node, &["query.run.read", "query.run.write"]);
    wait_until(Duration::from_secs(10), || {
        TcpStream::connect(("127.0.0.1", pdp_port)).is_ok()
    });
    let grants =
        |list: &[&str]| -> Vec<Grant> { list.iter().map(|g| Grant::parse(g).unwrap()).collect() };
    for (login, held) in [
        (
            "alice",
            grants(&[
                "access on service:db",
                "query.run.read on service:db",
                "query.run.write on service:db/database:analytics",
            ]),
        ),
        ("carol", Vec::new()),
    ] {
        let row = crate::store::UserRow {
            login: login.into(),
            password_hash: password_hash(PASSWORD),
            grants: held,
            created_at: 1,
        };
        node.state.store.insert_user(&row).unwrap();
    }
    crate::sync::run_round(&node.state, 1);
    let decide = |token: &str, body: &str| -> bool {
        let reply = pdp(pdp_port, "/access/v1/evaluation", Some(token), body, None);
        assert_eq!(reply.status, 200, "{}", reply.body);
        reply.json()["decision"].as_bool().unwrap()
    };
    let db = |subject: &str, action: &str, kind: &str, id: &str| {
        decide(PDP_DB_TOKEN, &evaluation(subject, action, kind, id))
    };

    assert!(db("alice", "query.run.write", "database", "analytics"));
    assert!(
        !db("alice", "query.run.write", "database", "logs"),
        "another id"
    );
    assert!(
        !db("alice", "query.run.write", "database", "Analytics"),
        "the id is exact"
    );
    assert!(
        !db("alice", "query.run.write", "table", "analytics"),
        "another type"
    );
    assert!(
        !db("alice", "query.run.write", "service", "db"),
        "not the whole service"
    );
    assert!(
        db("alice", "query.run.read", "database", "logs"),
        "the grant on the service"
    );
    assert!(
        db("carol", "query.run.read", "database", "logs"),
        "the configuration"
    );
    assert!(!db("carol", "query.run.read", "database", "analytics"));
    assert!(!db("carol", "query.run.read", "service", "db"));
    assert!(
        !db("mallory", "query.run.read", "database", "logs"),
        "no such user"
    );
    assert!(
        db("root", "query.run.write", "database", "logs"),
        "the superadmin"
    );
    // The service of a sub-resource is the server that asks: chat has no grant of alice.
    assert!(!decide(
        PDP_CHAT_TOKEN,
        &evaluation("alice", "query.run.write", "database", "analytics")
    ));

    // The batch: one subject, a resource for each evaluation.
    let body = serde_json::json!({
        "subject": {"type": "user", "id": "alice"},
        "action": {"name": "query.run.write"},
        "evaluations": [
            {"resource": {"type": "database", "id": "analytics"}},
            {"resource": {"type": "database", "id": "logs"}},
            {"resource": {"type": "service", "id": "db"}},
            {"action": {"name": "query.run.read"}, "resource": {"type": "database", "id": "logs"}}
        ]
    });
    let reply = pdp(
        pdp_port,
        "/access/v1/evaluations",
        Some(PDP_DB_TOKEN),
        &body.to_string(),
        None,
    );
    assert_eq!(reply.status, 200, "{}", reply.body);
    let decisions: Vec<bool> = reply.json()["evaluations"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["decision"].as_bool().unwrap())
        .collect();
    assert_eq!(decisions, [true, false, false, true]);

    // The action search on a sub-resource.
    let search = |subject: &str, kind: &str, id: &str| {
        let body = serde_json::json!({
            "subject": {"type": "user", "id": subject},
            "resource": {"type": kind, "id": id}
        });
        let reply = pdp(
            pdp_port,
            "/access/v1/search/action",
            Some(PDP_DB_TOKEN),
            &body.to_string(),
            None,
        );
        assert_eq!(reply.status, 200, "{}", reply.body);
        reply.json()["results"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| r["name"].as_str().unwrap().to_string())
            .collect::<Vec<String>>()
    };
    assert_eq!(
        search("alice", "database", "analytics"),
        ["access", "query.run.read", "query.run.write"]
    );
    assert_eq!(
        search("alice", "database", "logs"),
        ["access", "query.run.read"]
    );
    assert_eq!(
        search("alice", "service", "db"),
        ["access", "query.run.read"]
    );
    assert_eq!(search("carol", "database", "logs"), ["query.run.read"]);
    assert!(search("carol", "database", "analytics").is_empty());
}

#[test]
fn the_pdp_gives_a_reason_only_for_a_subject_that_does_not_exist() {
    let pdp_port = free_port();
    let node = start_with(
        Arc::new(Backend::Sqlite.fresh()),
        None,
        &format!(
            r#"access {{ grants_mode = "hybrid" }}
            user "carol" {{ grants = ["query.run.read on service:db"] }}
            pdp {{ listen_address = "127.0.0.1:{pdp_port}" url = "http://pdp.test:{pdp_port}" max_evaluations = 4 }}
            pdp_client "db" {{ token_env = "TEST_PDP_DB" }}"#
        ),
    );
    publish_catalog(&node, &["query.run.read", "query.run.write", "system.read"]);
    wait_until(Duration::from_secs(10), || {
        TcpStream::connect(("127.0.0.1", pdp_port)).is_ok()
    });
    let row = crate::store::UserRow {
        login: "carol".into(),
        password_hash: password_hash(PASSWORD),
        grants: Vec::new(),
        created_at: 1,
    };
    node.state.store.insert_user(&row).unwrap();
    crate::sync::run_round(&node.state, 1);
    let unknown = serde_json::json!({"decision": false, "context": {"reason": "unknown_subject"}});
    let plain_false = serde_json::json!({"decision": false});
    let ask = |path: &str, body: String| -> serde_json::Value {
        let reply = pdp(pdp_port, path, Some(PDP_DB_TOKEN), &body, None);
        assert_eq!(reply.status, 200, "{}", reply.body);
        reply.json()
    };
    let single = |subject_kind: &str, subject: &str, action: &str, kind: &str, id: &str| {
        ask(
            "/access/v1/evaluation",
            serde_json::json!({
                "subject": {"type": subject_kind, "id": subject},
                "action": {"name": action},
                "resource": {"type": kind, "id": id}
            })
            .to_string(),
        )
    };

    // A user that does not exist: the reason is in the answer.
    assert_eq!(
        single("user", "mallory", "access", "service", "db"),
        unknown
    );
    assert_eq!(
        single("user", "mallory", "query.run.read", "database", "logs"),
        unknown,
        "also on a sub-resource"
    );
    // The other denials have no reason: the answer does not show the grants.
    assert_eq!(
        single("user", "carol", "query.run.write", "service", "db"),
        plain_false,
        "an account without the grant"
    );
    assert_eq!(
        single("user", "carol", "access", "service", "chat"),
        plain_false,
        "an account, the service of another server"
    );
    assert_eq!(
        single("user", "carol", "Query.Run", "service", "db"),
        plain_false,
        "an account, an action name that is not valid"
    );
    assert_eq!(
        single("group", "mallory", "access", "service", "db"),
        plain_false,
        "a subject type that is not user"
    );
    assert_eq!(
        single("group", "carol", "access", "service", "db"),
        plain_false,
        "a subject type that is not user, an existing login"
    );
    // A permit has no reason.
    assert_eq!(
        single("user", "carol", "query.run.read", "service", "db"),
        serde_json::json!({"decision": true})
    );
    assert_eq!(
        single("user", "root", "access", "service", "db"),
        serde_json::json!({"decision": true}),
        "the superadmin"
    );

    // The batch: the reason is in the item of the unknown subject only.
    let body = serde_json::json!({
        "action": {"name": "query.run.read"},
        "resource": {"type": "service", "id": "db"},
        "evaluations": [
            {"subject": {"type": "user", "id": "carol"}},
            {"subject": {"type": "user", "id": "mallory"}},
            {"subject": {"type": "user", "id": "carol"}, "action": {"name": "system.read"}},
            {"subject": {"type": "group", "id": "mallory"}}
        ]
    });
    let batch = ask("/access/v1/evaluations", body.to_string());
    assert_eq!(
        batch["evaluations"],
        serde_json::json!([
            {"decision": true},
            {"decision": false, "context": {"reason": "unknown_subject"}},
            {"decision": false},
            {"decision": false}
        ])
    );
    // A batch without `evaluations` is one decision, with the same reason.
    let one = ask(
        "/access/v1/evaluations",
        serde_json::json!({
            "subject": {"type": "user", "id": "mallory"},
            "action": {"name": "access"},
            "resource": {"type": "service", "id": "db"}
        })
        .to_string(),
    );
    assert_eq!(one, unknown);
}

#[test]
fn the_pdp_refuses_a_grant_on_a_sub_resource_when_the_service_does_not_declare_its_type() {
    let pdp_port = free_port();
    let node = start_with(
        Arc::new(Backend::Sqlite.fresh()),
        None,
        &format!(
            r#"access {{ grants_mode = "hybrid" }}
            user "carol" {{ grants = ["query.run.write on service:db/database:analytics", "query.run.read on service:db/table:t", "system.read on service:db"] }}
            pdp {{ listen_address = "127.0.0.1:{pdp_port}" url = "http://pdp.test:{pdp_port}" max_evaluations = 4 }}
            pdp_client "db" {{ token_env = "TEST_PDP_DB" }}"#
        ),
    );
    wait_until(Duration::from_secs(10), || {
        TcpStream::connect(("127.0.0.1", pdp_port)).is_ok()
    });
    let row = crate::store::UserRow {
        login: "carol".into(),
        password_hash: password_hash(PASSWORD),
        grants: Vec::new(),
        created_at: 1,
    };
    node.state.store.insert_user(&row).unwrap();
    crate::sync::run_round(&node.state, 1);
    let decide = |action: &str, kind: &str, id: &str| -> bool {
        let reply = pdp(
            pdp_port,
            "/access/v1/evaluation",
            Some(PDP_DB_TOKEN),
            &evaluation("carol", action, kind, id),
            None,
        );
        assert_eq!(reply.status, 200, "{}", reply.body);
        reply.json()["decision"].as_bool().unwrap()
    };
    let write = || decide("query.run.write", "database", "analytics");

    // The service was never read: no grant on a sub-resource applies. The grant on the whole
    // service is not affected.
    assert!(
        !write(),
        "never read: the grant on a sub-resource does not apply"
    );
    assert!(decide("system.read", "service", "db"));
    assert!(
        decide("system.read", "database", "analytics"),
        "the grant on the service covers its sub-resources without a document"
    );

    // The document declares the type: the grant applies. A type of another action does not help.
    publish_catalog(&node, &["query.run.read", "query.run.write", "system.read"]);
    assert!(write(), "the type is declared");
    assert!(!decide("query.run.write", "database", "logs"), "another id");

    // A failed read keeps the last document.
    node.state.actions.fail("db");
    assert!(write(), "a failed read keeps the last document");

    // The type of the grant is not declared for its action: the grant does not apply, and the
    // other grants do.
    assert!(
        !decide("query.run.read", "table", "t"),
        "the document declares database, not table"
    );

    // A new document without the type: the grant does not apply.
    node.state.actions.set(
        "db",
        vec![crate::actions::Action {
            name: "query.run.write".into(),
            resource_types: vec!["service".into()],
            description: String::new(),
        }],
    );
    assert!(!write(), "the new document removes the type");
    node.state.actions.fail("db");
    assert!(!write(), "the removal stays after a failed read");
    assert!(
        decide("system.read", "database", "analytics"),
        "the grant on the whole service stays"
    );
}

#[test]
fn the_settings_give_and_remove_a_grant_on_a_sub_resource() {
    each_backend(|backend| {
        let node = start_with(Arc::new(backend.fresh()), Some(Duration::ZERO), "");
        let root = sign_in(&node, "root", PASSWORD);
        assert_eq!(
            post(
                &node,
                &root,
                USERS_ADD_PATH,
                USERS_TOKEN,
                "login=alice&password=a-long-password-1",
            )
            .location(),
            Some("/auth/settings?s=user-added")
        );
        let add = |fields: &str| {
            post(&node, &root, GRANT_ADD_PATH, USERS_TOKEN, fields)
                .location()
                .map(str::to_string)
        };
        let notice = |code: &str| Some(format!("/auth/settings?s={code}"));
        let fields = "login=alice&service=db&action=query.run.write&type=database&id=analytics";
        // The service has not answered yet: no grant on a sub-resource.
        *node.state.catalog.write().unwrap() = Arc::new(Catalog::build(
            vec![listed(&[
                ("portal.name", "Db"),
                ("portal.id", "db"),
                ("portal.href", "/db/"),
                ("portal.actions", DB_ACTIONS_URL),
            ])],
            Vec::new(),
        ));
        assert_eq!(add(fields), notice("grant-not-known"));
        publish_catalog(&node, &["query.run.read", "query.run.write"]);
        assert_eq!(add(fields), notice("grant-added"));
        assert_eq!(add(fields), notice("unchanged"));
        for (bad, code) in [
            (
                "login=alice&service=db&action=query.run.write&type=table&id=a",
                "grant-undeclared",
            ),
            (
                "login=alice&service=db&action=system.read&type=database&id=a",
                "grant-undeclared",
            ),
            (
                "login=alice&service=db&action=access&type=database&id=a",
                "grant-undeclared",
            ),
            (
                "login=alice&service=db&action=query.run.write&type=service&id=db",
                "grant-undeclared",
            ),
            (
                "login=alice&service=chat&action=query.run.write&type=database&id=a",
                "grant-undeclared",
            ),
            (
                "login=alice&service=db&action=query.run.write&type=database&id=a+b",
                "grant-bad-id",
            ),
            (
                "login=alice&service=db&action=query.run.write&type=database&id=",
                "grant-bad-id",
            ),
            (
                "login=nobody&service=db&action=query.run.write&type=database&id=a",
                "no-user",
            ),
        ] {
            assert_eq!(add(bad), notice(code), "{bad}");
        }
        let extra = post(
            &node,
            &root,
            GRANT_ADD_PATH,
            USERS_TOKEN,
            &format!("{fields}&other=1"),
        );
        assert_eq!(extra.status, 400, "an unknown field");
        let expected =
            vec![Grant::parse("query.run.write on service:db/database:analytics").unwrap()];
        assert_eq!(node.state.users.grants_of("alice").unwrap(), expected);
        assert_eq!(node.state.store.load_users().unwrap()[0].grants, expected);
        let page = page(&node, &root, "/auth/settings");
        assert!(page.body.contains("<code>database:analytics</code>"));

        // The edit form keeps the grant on the sub-resource: it shows only the grants on services.
        assert_eq!(
            post(
                &node,
                &root,
                USERS_UPDATE_PATH,
                USERS_TOKEN,
                "login=alice&g.db.access=1"
            )
            .location(),
            Some("/auth/settings?s=user-updated")
        );
        assert_eq!(node.state.users.grants_of("alice").unwrap().len(), 2);

        // A user cannot give a grant.
        let alice = sign_in(&node, "alice", "a-long-password-1");
        assert_eq!(
            post(&node, &alice, GRANT_ADD_PATH, USERS_TOKEN, fields).status,
            403
        );

        let delete = |grant: &str| {
            post(
                &node,
                &root,
                GRANT_DELETE_PATH,
                USERS_TOKEN,
                &format!(
                    "login=alice&grant={}",
                    grant
                        .replace(' ', "+")
                        .replace('/', "%2F")
                        .replace(':', "%3A")
                ),
            )
            .location()
            .map(str::to_string)
        };
        assert_eq!(
            delete("access on service:db"),
            notice("no-grant"),
            "only a sub-resource"
        );
        assert_eq!(
            delete("query.run.write on service:db/database:logs"),
            notice("no-grant")
        );
        assert_eq!(
            delete("query.run.write on service:db/database:analytics"),
            notice("grant-deleted")
        );
        assert_eq!(
            node.state.users.grants_of("alice").unwrap(),
            vec![Grant::new("access", "db").unwrap()]
        );
        assert_eq!(
            node.state.store.load_users().unwrap()[0].grants,
            vec![Grant::new("access", "db").unwrap()]
        );
    });
}

// Security regressions -------------------------------------------------------------------------

/// Adds an account without a grant and signs it in.
fn user_without_grant(node: &Node, root: &str, login: &str) -> String {
    let reply = post(
        node,
        root,
        USERS_ADD_PATH,
        USERS_TOKEN,
        &format!("login={login}&password=a-long-password-1"),
    );
    assert_eq!(reply.status, 303, "{:?}", reply.location());
    sign_in(node, login, "a-long-password-1")
}

/// A source that never answered gives a snapshot without its services. The gate cannot tell an
/// unowned path from a path of a missing service: only the superadmin passes.
#[test]
fn a_source_that_never_answered_keeps_the_gate_closed_to_users() {
    let node = start_custom(
        Arc::new(Store::open_in_memory()),
        None,
        Custom {
            discovery: r#"interval = "1h" docker_socket = "/nonexistent/docker.sock""#,
            wait_complete: false,
            ..Custom::default()
        },
        "",
    );
    assert!(!current_catalog(&node.state).is_complete());
    let root = sign_in(&node, "root", PASSWORD);
    let alice = user_without_grant(&node, &root, "alice");
    for uri in ["/grafana/d/1", "/an/unowned/path"] {
        let reply = open(&node, &alice, uri);
        assert_eq!(reply.status, 302, "{uri}");
        assert_eq!(reply.location(), Some(DENIED_PATH), "{uri}");
        assert_eq!(open(&node, &root, uri).status, 204, "{uri}");
    }
    let metrics = send(node.port, "GET", "/metrics", &[], "").body;
    assert!(
        metrics.contains("portal_catalog_complete 0"),
        "the metric shows the state"
    );
}

/// Posts the login form from the local address `source`, with more headers. The reply is
/// returned as it is.
fn login_from(
    node: &Node,
    source: &str,
    login: &str,
    password: &str,
    headers: &[(&str, &str)],
) -> Reply {
    let csrf = "A".repeat(32);
    let signature = sign_return_uri(&node.state.signing_secret, "/").unwrap();
    let origin = format!("http://127.0.0.1:{}", node.port);
    let cookie = format!("{}={csrf}", transport::csrf_cookie_name(false));
    let mut all = vec![
        ("Origin", origin.as_str()),
        ("Content-Type", "application/x-www-form-urlencoded"),
        ("Cookie", cookie.as_str()),
    ];
    all.extend_from_slice(headers);
    send_from(
        source,
        node.port,
        "POST",
        LOGIN_PATH,
        &all,
        &format!("login={login}&password={password}&csrf={csrf}&rd=%2F&sig={signature}"),
    )
}

/// ForwardAuth from the local address `source`, with more headers.
fn verify_from(node: &Node, source: &str, session: &str, headers: &[(&str, &str)]) -> u16 {
    let cookie = format!("{}={session}", session_cookie_name(node));
    let mut all = vec![
        ("X-Forwarded-Uri", "/app/"),
        ("X-Forwarded-Method", "GET"),
        ("Cookie", cookie.as_str()),
    ];
    all.extend_from_slice(headers);
    send_from(source, node.port, "GET", VERIFY_PATH, &all, "").status
}

/// A URL parser of the URL Standard removes a TAB: `/<TAB>/evil.example/x` would be a link to
/// another host. The gate refuses to sign such a return target.
#[test]
fn the_gate_refuses_a_return_target_with_a_control_byte() {
    let node = start(Arc::new(Store::open_in_memory()), None);
    for uri in ["/\t/evil.example/x", "/%09/evil.example/x", "/x\x0b/y"] {
        let reply = send(
            node.port,
            "GET",
            VERIFY_PATH,
            &[("X-Forwarded-Uri", uri), ("X-Forwarded-Method", "GET")],
            "",
        );
        assert_eq!(reply.status, 400, "{uri:?}: {:?}", reply.location());
    }
    let signature = sign_return_uri(&node.state.signing_secret, "/\t/evil.example/x").unwrap();
    let root = sign_in(&node, "root", PASSWORD);
    let reply = page(
        &node,
        &root,
        &format!("{LOGIN_PATH}?rd=%2F%09%2Fevil.example%2Fx&sig={signature}"),
    );
    assert_eq!(reply.status, 303);
    assert_eq!(reply.location(), Some("/"), "a signed bad target goes home");
}

/// Only a trusted proxy chooses the client address. A direct peer of the port cannot use the
/// session binding of another address, and cannot use the login limiter of another address.
#[test]
fn x_forwarded_for_counts_only_from_a_trusted_proxy() {
    let node = start(Arc::new(Store::open_in_memory()), None);
    let victim = [("X-Forwarded-For", "2001:db8::7")];
    let reply = login_from(&node, "127.0.0.1", "root", PASSWORD, &victim);
    assert_eq!(reply.status, 303);
    let root = reply.cookie(&session_cookie_name(&node)).unwrap();
    assert_eq!(
        verify_from(&node, "127.0.0.1", &root, &victim),
        204,
        "through the proxy"
    );
    assert_eq!(
        verify_from(&node, "127.0.0.2", &root, &victim),
        302,
        "a direct peer cannot claim the address of the victim"
    );
    assert_eq!(
        verify_from(
            &node,
            "127.0.0.1",
            &root,
            &[("X-Forwarded-For", "2001:db8::7, not-an-ip")]
        ),
        302,
        "a broken chain gives the peer"
    );

    let target = [("X-Forwarded-For", "198.51.100.23")];
    for _ in 0..5 {
        let reply = login_from(&node, "127.0.0.2", "root", "wrong-password", &target);
        assert_eq!(reply.status, 401);
    }
    assert_eq!(
        login_from(&node, "127.0.0.2", "root", "wrong-password", &target).status,
        429,
        "the limiter keys the direct peer on its own address"
    );
    assert_eq!(
        login_from(&node, "127.0.0.1", "root", PASSWORD, &target).status,
        303,
        "the address behind the proxy is not locked out"
    );
}

/// Failures against other logins never use the budget of the superadmin. Failures against the
/// superadmin login lock only the superadmin.
#[test]
fn anonymous_failures_do_not_lock_out_the_superadmin() {
    let node = start_with(
        Arc::new(Store::open_in_memory()),
        None,
        r#"security { global_login_attempts = 3 login_attempts_per_ip = 1000 }"#,
    );
    let root = sign_in(&node, "root", PASSWORD);
    let reply = post(
        &node,
        &root,
        USERS_ADD_PATH,
        USERS_TOKEN,
        "login=alice&password=a-long-password-1",
    );
    assert_eq!(reply.status, 303);
    for login in ["nobody", "alice", "admin", "nobody"] {
        let reply = login_from(&node, "127.0.0.1", login, "wrong-password", &[]);
        assert!(
            matches!(reply.status, 401 | 429),
            "{login}: {}",
            reply.status
        );
    }
    assert_eq!(
        login_from(&node, "127.0.0.1", "nobody", "wrong-password", &[]).status,
        429,
        "the global budget of the other logins is used up"
    );
    assert_eq!(
        login_from(&node, "127.0.0.1", "ROOT", PASSWORD, &[]).status,
        303,
        "the superadmin still signs in"
    );
    for _ in 0..3 {
        assert_eq!(
            login_from(&node, "127.0.0.1", "root", "wrong-password", &[]).status,
            401
        );
    }
    assert_eq!(
        login_from(&node, "127.0.0.1", "root", PASSWORD, &[]).status,
        429,
        "failures against the superadmin lock the superadmin"
    );
}

/// Slow sign-in bodies hold at most `server.max_login_body_reads` connections. The others get
/// `503` at once, and ForwardAuth still gets a connection.
#[test]
fn slow_sign_in_bodies_leave_connections_for_forward_auth() {
    let node = start_custom(
        Arc::new(Store::open_in_memory()),
        None,
        Custom {
            server: r#"trusted_proxies = ["127.0.0.1/32"] max_connections = 4 max_login_body_reads = 2 body_read_timeout = "20s""#,
            ..Custom::default()
        },
        "",
    );
    let head = format!(
        "POST {LOGIN_PATH} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nOrigin: http://127.0.0.1:{port}\r\nContent-Type: application/x-www-form-urlencoded\r\nContent-Length: 64\r\nUser-Agent: {AGENT}\r\n\r\n",
        port = node.port
    );
    let mut slow = Vec::new();
    for _ in 0..4 {
        let mut stream = TcpStream::connect(("127.0.0.1", node.port)).unwrap();
        stream.write_all(head.as_bytes()).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_millis(2_000)))
            .unwrap();
        slow.push(stream);
        std::thread::sleep(Duration::from_millis(100));
    }
    let mut refused = 0;
    for stream in &mut slow {
        let mut buffer = [0u8; 64];
        if let Ok(n) = stream.read(&mut buffer) {
            if String::from_utf8_lossy(&buffer[..n]).starts_with("HTTP/1.1 503") {
                refused += 1;
            }
        }
    }
    assert_eq!(refused, 2, "two bodies are read, the other two are refused");
    let begun = Instant::now();
    let reply = send(
        node.port,
        "GET",
        VERIFY_PATH,
        &[("X-Forwarded-Uri", "/app/"), ("X-Forwarded-Method", "GET")],
        "",
    );
    assert_eq!(reply.status, 302, "ForwardAuth answers");
    assert!(
        begun.elapsed() < Duration::from_secs(3),
        "{:?}",
        begun.elapsed()
    );
    drop(slow);
}

/// A rotation of the superadmin login, of its password or of the session secret ends the old
/// superadmin and impersonated sessions, also with a shared database that keeps the sessions
/// over a restart. The sessions of the database users stay with a shared database.
#[test]
fn a_superadmin_rotation_ends_the_old_superadmin_and_impersonated_sessions() {
    each_backend(|backend| {
        let store = Arc::new(backend.fresh());
        let old = start(Arc::clone(&store), Some(Duration::ZERO));
        let root = sign_in(&old, "root", PASSWORD);
        let carol = user_without_grant(&old, &root, "carol");
        let second_root = sign_in(&old, "root", PASSWORD);
        let reply = post(
            &old,
            &second_root,
            IMPERSONATE_PATH,
            USERS_TOKEN,
            "login=carol",
        );
        assert_eq!(reply.status, 303, "{:?}", reply.location());
        let as_carol = reply.cookie(&session_cookie_name(&old)).unwrap();
        assert_eq!(verify(&old, &as_carol, AGENT), 204);

        let shared = store.is_shared();
        for (name, custom) in [
            (
                "login",
                Custom {
                    login: "root2",
                    ..Custom::default()
                },
            ),
            (
                "password",
                Custom {
                    password: "another-password-for-root",
                    ..Custom::default()
                },
            ),
            (
                "session secret",
                Custom {
                    session_secret: "fedcba9876543210fedcba9876543210",
                    ..Custom::default()
                },
            ),
        ] {
            let new = start_custom(Arc::clone(&store), Some(Duration::ZERO), custom, "");
            assert_eq!(
                verify(&new, &root, AGENT),
                302,
                "{name}: the old superadmin session"
            );
            assert_eq!(
                page(&new, &root, "/auth/settings").status,
                302,
                "{name}: no settings page, the sign-in page"
            );
            assert_eq!(
                verify(&new, &as_carol, AGENT),
                302,
                "{name}: the old impersonated session"
            );
            assert_eq!(
                verify(&new, &carol, AGENT),
                if shared { 204 } else { 302 },
                "{name}: the session of a database user"
            );
        }
        // The rows of the old generation are gone: the old instance refuses them too.
        if shared {
            assert_eq!(verify(&old, &root, AGENT), 302);
        }
    });
}

/// "Log in as" and "Back to superadmin" give no fresh lifetime. The last session ends when the
/// sign-in with the password would have ended.
#[test]
fn impersonation_and_return_keep_the_end_of_the_password_session() {
    each_backend(|backend| {
        let node = start(Arc::new(backend.fresh()), Some(Duration::ZERO));
        let root = sign_in(&node, "root", PASSWORD);
        let _alice = user_without_grant(&node, &root, "alice");
        let binding = {
            let mut headers = HeaderMap::new();
            headers.insert(USER_AGENT, HeaderValue::from_static(AGENT));
            transport::binding(
                node.state.config.session.bind,
                &headers,
                Some("127.0.0.1".parse().unwrap()),
            )
        };
        let end = node
            .state
            .sessions
            .validate(&root, &binding)
            .unwrap()
            .expires_at;
        let ttl = node.state.config.session.ttl.as_secs();
        std::thread::sleep(Duration::from_millis(1_100));

        let reply = post(&node, &root, IMPERSONATE_PATH, USERS_TOKEN, "login=alice");
        assert_eq!(reply.status, 303);
        let as_alice = reply.cookie(&session_cookie_name(&node)).unwrap();
        let reply = post(&node, &as_alice, IMPERSONATE_END_PATH, USERS_TOKEN, "");
        assert_eq!(reply.status, 303);
        let back = reply.cookie(&session_cookie_name(&node)).unwrap();
        let max_age: u64 = reply
            .headers
            .iter()
            .filter(|(name, _)| name == "set-cookie")
            .find_map(|(_, value)| {
                value
                    .split("; ")
                    .find_map(|part| part.strip_prefix("Max-Age="))
                    .map(|n| n.parse().unwrap())
            })
            .unwrap();
        assert!(
            max_age < ttl,
            "Max-Age {max_age} must be less than the ttl {ttl}"
        );
        assert_eq!(
            node.state
                .sessions
                .validate(&back, &binding)
                .unwrap()
                .expires_at,
            end,
            "the session ends with the password session"
        );
    });
}

/// The incident action of the settings page. Only the superadmin in its own right ends every
/// session, with the confirmation field and the anti-CSRF token. The superadmin signs out too.
#[test]
fn the_superadmin_ends_all_sessions_after_a_confirmation_and_the_action_is_audited() {
    each_backend(|backend| {
        let (first, second) = cluster(backend, Some(Duration::ZERO));
        let root = sign_in(&first, "root", PASSWORD);
        let carol = user_without_grant(&first, &root, "carol");
        let dave = user_without_grant(&first, &root, "dave");
        let as_carol = {
            // "Log in as" replaces the session that makes it: use a second superadmin session.
            let second_root = sign_in(&first, "root", PASSWORD);
            let reply = post(
                &first,
                &second_root,
                IMPERSONATE_PATH,
                USERS_TOKEN,
                "login=carol",
            );
            assert_eq!(reply.status, 303);
            reply.cookie(&session_cookie_name(&first)).unwrap()
        };

        let settings = page(&first, &root, "/auth/settings");
        assert_eq!(settings.status, 200);
        assert!(settings.body.contains(END_SESSIONS_PATH), "the form");
        assert!(settings.body.contains("Confirm: end all sessions"));
        // A user does not see the page and cannot use the route.
        assert_eq!(
            post(
                &first,
                &dave,
                END_SESSIONS_PATH,
                USERS_TOKEN,
                "confirm=end-all-sessions"
            )
            .status,
            403
        );
        // The superadmin who browses as a user is not the superadmin in its own right.
        assert_eq!(
            post(
                &first,
                &as_carol,
                END_SESSIONS_PATH,
                USERS_TOKEN,
                "confirm=end-all-sessions"
            )
            .status,
            403
        );
        // No confirmation, a wrong value or an extra field: nothing happens.
        for fields in ["", "confirm=yes", "confirm=end-all-sessions&x=1"] {
            let reply = post(&first, &root, END_SESSIONS_PATH, USERS_TOKEN, fields);
            assert_eq!(reply.status, 400, "{fields:?}");
        }
        // A token of another purpose does not pass.
        let wrong = post(
            &first,
            &root,
            END_SESSIONS_PATH,
            LOGOUT_TOKEN,
            "confirm=end-all-sessions",
        );
        assert_eq!(wrong.status, 403, "a token of another form");
        for session in [&root, &carol, &dave, &as_carol] {
            assert_eq!(verify(&first, session, AGENT), 204, "nothing ended yet");
        }

        let reply = post(
            &first,
            &root,
            END_SESSIONS_PATH,
            USERS_TOKEN,
            "confirm=end-all-sessions",
        );
        assert_eq!(reply.status, 303);
        assert_eq!(reply.location(), Some(LOGIN_PATH));
        let cookies: Vec<&str> = reply
            .headers
            .iter()
            .filter(|(name, _)| name == "set-cookie")
            .map(|(_, value)| value.as_str())
            .collect();
        assert_eq!(cookies.len(), 2, "the session cookie and the CSRF cookie");
        assert!(
            cookies.iter().all(|c| c.contains("Max-Age=0")),
            "{cookies:?}"
        );
        for node in [&first, &second] {
            for session in [&root, &carol, &dave, &as_carol] {
                assert_eq!(verify(node, session, AGENT), 302, "every session ended");
            }
        }
        assert_eq!(first.state.sessions.active_count(), 0);

        // Accounts stay: the users sign in again. The history has one audit event.
        let again = sign_in(&first, "root", PASSWORD);
        let _ = sign_in(&first, "carol", "a-long-password-1");
        wait_until(Duration::from_secs(10), || {
            page(&first, &again, "/auth/settings")
                .body
                .contains("All sessions ended")
        });
    });
}
