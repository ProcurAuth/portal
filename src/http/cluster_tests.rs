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

/// A free local port. A port that this function gave once is never given again, so two servers of
/// a test never get the same port when the first one has not bound it yet.
fn free_port() -> u16 {
    static GIVEN: std::sync::Mutex<Vec<u16>> = std::sync::Mutex::new(Vec::new());
    loop {
        let port = std::net::TcpListener::bind("127.0.0.1:0")
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
    /// Wait for the open port and a complete catalogue before the node is given to the test.
    /// A node that does not wait has no open port until its catalogue is complete (portal 7.3b).
    wait_complete: bool,
    /// `users.max_users`.
    max_users: usize,
    /// `users.allow_creation`. The mode `static` needs `false`.
    allow_creation: bool,
    /// More environment variables of the configuration.
    env: Vec<(String, String)>,
    /// The stop of the server, in place of SIGTERM. The server stops when it gets a value.
    stop: Option<tokio::sync::oneshot::Receiver<()>>,
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
            max_users: 10,
            allow_creation: true,
            env: Vec::new(),
            stop: None,
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
        max_users,
        allow_creation,
        env,
        stop,
    } = custom;
    let (config, mut secrets) = Config::parse(
        &format!(
            r#"
        server {{ listen_address = "127.0.0.1:{port}" workers = 2 {server} }}
        authentication {{
          superadmin_login_env    = "PORTAL_TEST_LOGIN"
          superadmin_password_env = "PORTAL_TEST_PASSWORD"
        }}
        users {{ max_users = {max_users} allow_creation = {allow_creation} }}
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
            other => env
                .iter()
                .find(|(key, _)| key == other)
                .map(|(_, value)| value.clone()),
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
        let _ = match stop {
            Some(stop) => super::serve_with(serving, async move {
                let _ = stop.await;
            }),
            None => super::serve_with(serving, std::future::pending()),
        };
    });
    let node = Node { port, state };
    if !wait_complete {
        return node;
    }
    wait_until(Duration::from_secs(10), || {
        TcpStream::connect(("127.0.0.1", port)).is_ok()
            && send(port, "GET", "/healthz", &[], "").status == 200
    });
    // The first discovery round publishes an empty catalogue. The next round starts an hour later.
    wait_until(Duration::from_secs(10), || {
        node.state.discovery.refreshes() >= 1
    });
    assert!(
        current_catalog(&node.state).is_complete(),
        "the first round of a harness without a source is complete"
    );
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

#[test]
fn a_sign_in_page_that_stayed_open_too_long_gives_the_page_again_with_a_new_token() {
    let node = start_with(Arc::new(Backend::Sqlite.fresh()), None, "");
    let origin = format!("http://127.0.0.1:{}", node.port);
    let csrf = "A".repeat(32);
    let signature = sign_return_uri(&node.state.signing_secret, "/").unwrap();
    // The browser dropped the cookie of the token: the form has a token and the request has none.
    let reply = send(
        node.port,
        "POST",
        LOGIN_PATH,
        &[
            ("Origin", &origin),
            ("Content-Type", "application/x-www-form-urlencoded"),
        ],
        &format!("login=root&password={PASSWORD}&csrf={csrf}&rd=%2F&sig={signature}"),
    );
    assert_eq!(reply.status, 403);
    assert!(
        reply.body.contains("The page was open too long"),
        "{}",
        reply.body
    );
    assert!(
        reply.body.contains(r#"name="csrf""#) && !reply.body.contains(&csrf),
        "a new token"
    );
    let cookie = reply
        .header("set-cookie")
        .expect("the cookie of the new token");
    assert!(cookie.contains("Max-Age=43200"), "{cookie}");
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
const DB_ACTIONS_URL: &str = "http://test/.well-known/procurauth/actions";

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
                limits: Vec::new(),
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
        assert_eq!(
            add.location(),
            Some("/auth/settings/users/alice?s=user-added")
        );
        // The add form has the login and the password only. A grant, a copy or any other field is
        // a bad request. The grants have their own page.
        for bad in [
            "login=bob&password=a-long-password-1&grant0=access+on+service%3Adb",
            "login=bob&password=a-long-password-1&g.db.access=1",
            "login=bob&password=a-long-password-1&copy=alice",
            "login=bob&password=a-long-password-1&other=1",
        ] {
            assert_eq!(
                post(&node, &root, USERS_ADD_PATH, USERS_TOKEN, bad).status,
                400,
                "{bad}"
            );
        }
        assert!(
            node.state.users.get("bob").is_none(),
            "a refused add makes no account"
        );

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
        // The page has a random CSRF token: match the service name as an element, not as a substring.
        assert!(!refusal.body.contains("Grafana") && !refusal.body.contains(">Db<"));

        // The superadmin gives access and two actions on db.
        let update = |fields: &str| {
            post(
                &node,
                &root,
                USERS_UPDATE_PATH,
                USERS_TOKEN,
                &format!("grants=1&login=alice{fields}"),
            )
        };
        let reply = update("&grant0=access+on+service%3Adb&grant1=query.run.read+on+service%3Adb&grant2=query.run.write+on+service%3Adb");
        assert_eq!(
            reply.location(),
            Some("/auth/settings/users/alice?s=user-updated")
        );
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
        let reply = update("&grant0=dashboards.read+on+service%3Aother-grafana");
        assert_eq!(
            reply.location(),
            Some("/auth/settings/users/alice?s=edit-unknown-grant")
        );

        // An action that db no longer publishes keeps its grant when the form sends it as it was.
        // The editor shows it, so the person can remove it.
        publish_catalog(&node, &["query.run.read"]);
        assert_eq!(
            update("&grant0=access+on+service%3Adb&grant1=query.run.write+on+service%3Adb")
                .location(),
            Some("/auth/settings/users/alice?s=user-updated")
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
        assert_eq!(
            update("").location(),
            Some("/auth/settings/users/alice?s=user-updated")
        );
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

/// An account of the database that exists before the mode is `static`: the create route does not
/// exist in this mode, so the test puts the row in the database.
fn insert_account(node: &Node, login: &str, password: &str) {
    let hash = password_hash(password);
    node.state
        .store
        .insert_user(&crate::store::UserRow {
            login: login.to_string(),
            password_hash: hash.clone(),
            grants: Vec::new(),
            created_at: 1,
        })
        .unwrap();
    node.state.users.insert(
        login.to_string(),
        UserRecord {
            password_hash: hash,
            grants: Vec::new(),
            created_at: 1,
        },
    );
}

#[test]
fn in_the_static_mode_only_the_configuration_gives_grants() {
    let node = start_custom(
        Arc::new(Backend::Sqlite.fresh()),
        None,
        Custom {
            env: vec![("FRED_HASH".into(), password_hash("a-long-password-2"))],
            allow_creation: false,
            ..Custom::default()
        },
        r#"access { grants_mode = "static" }
        user "fred" { password_hash_env = "FRED_HASH" grants = ["access on service:other-grafana"] }"#,
    );
    publish_catalog(&node, &[]);
    let root = sign_in(&node, "root", PASSWORD);
    insert_account(&node, "alice", "a-long-password-1");
    let reply = post(
        &node,
        &root,
        USERS_UPDATE_PATH,
        USERS_TOKEN,
        "grants=1&login=alice&grant0=access+on+service%3Adb",
    );
    assert_eq!(
        reply.location(),
        Some("/auth/settings/users/alice?s=grants-static")
    );
    // A grant in the database does not count in this mode (portal 3.4b).
    node.state
        .store
        .update_user("alice", None, Some(&[Grant::new("access", "db").unwrap()]))
        .unwrap();
    crate::sync::run_round(&node.state, 1);
    let alice = sign_in(&node, "alice", "a-long-password-1");
    assert_eq!(
        open(&node, &alice, "/db/").status,
        302,
        "the database does not give access in this mode"
    );
    assert_eq!(open(&node, &alice, "/grafana/").status, 302);
    // The account of the file gets its rights from the file.
    let fred = sign_in(&node, "fred", "a-long-password-2");
    assert_eq!(
        open(&node, &fred, "/grafana/").status,
        204,
        "the configuration gives it"
    );
    assert_eq!(open(&node, &fred, "/db/").status, 302);
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
            pdp {{ listen_address = "127.0.0.1:{pdp_port}" url = "http://pdp.test:{pdp_port}" plain_http = true max_evaluations = 4 }}
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
        .contains("portal_pdp_decisions_total{decision=\"true\",reason=\"none\"}"));
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
            pdp {{ listen_address = "127.0.0.1:{pdp_port}" url = "http://pdp.test:{pdp_port}" plain_http = true max_evaluations = 4 }}
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
fn the_pdp_gives_a_reason_only_for_an_unknown_subject_or_an_invalid_resource() {
    let pdp_port = free_port();
    let node = start_with(
        Arc::new(Backend::Sqlite.fresh()),
        None,
        &format!(
            r#"access {{ grants_mode = "hybrid" }}
            user "carol" {{ grants = ["query.run.read on service:db"] }}
            pdp {{ listen_address = "127.0.0.1:{pdp_port}" url = "http://pdp.test:{pdp_port}" plain_http = true max_evaluations = 4 }}
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
    // An id with a colon names a key. Nothing makes one yet: it is an unknown subject.
    for id in ["key:acc1", "key:", "other:k1", "carol:x"] {
        assert_eq!(
            single("user", id, "access", "service", "db"),
            unknown,
            "{id}"
        );
    }
    // The other denials have no reason: the answer does not show the grants.
    assert_eq!(
        single("user", "carol", "query.run.write", "service", "db"),
        plain_false,
        "an account without the grant"
    );
    assert_eq!(
        single("user", "carol", "access", "service", "chat"),
        serde_json::json!({"decision": false, "context": {"reason": "invalid_resource"}}),
        "an account, the service of another server: the resource is not valid"
    );
    // HOM-PTL-1: the invalid resource asked for an early read of the document of db. A second
    // request in the same interval does nothing.
    assert!(
        !node.state.actions.read_soon(
            "db",
            node.state.config.access.actions_refresh,
            std::time::Instant::now()
        ),
        "the PDP already asked for an early read in this interval"
    );
    assert_eq!(
        single("user", "carol", "Query.Run", "service", "db"),
        plain_false,
        "an account, an action name that is not valid"
    );
    // A subject of another type is an unknown subject (protocol 3.3, 11.1).
    assert_eq!(
        single("group", "mallory", "access", "service", "db"),
        unknown,
        "a subject type that is not user"
    );
    assert_eq!(
        single("group", "carol", "access", "service", "db"),
        unknown,
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
            {"decision": false, "context": {"reason": "unknown_subject"}}
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
            pdp {{ listen_address = "127.0.0.1:{pdp_port}" url = "http://pdp.test:{pdp_port}" plain_http = true max_evaluations = 4 }}
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

    // The service was never read: the PDP cannot check the chain of a node below the service.
    // The grant on the whole service is not affected.
    assert!(
        !write(),
        "never read: the grant on a sub-resource does not apply"
    );
    assert!(decide("system.read", "service", "db"));
    assert!(
        !decide("system.read", "database", "analytics"),
        "never read: no decision below the service"
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
            limits: Vec::new(),
        }],
    );
    assert!(!write(), "the new document removes the type");
    node.state.actions.fail("db");
    assert!(!write(), "the removal stays after a failed read");
    assert!(
        decide("system.read", "service", "db"),
        "the grant on the whole service stays"
    );
    assert!(
        !decide("system.read", "database", "analytics"),
        "the document does not declare database any more"
    );
}

/// A resource below the service with its ancestors (contract 11.5).
fn node_of(kind: &str, id: &str, ancestors: &[(&str, &str)]) -> serde_json::Value {
    let ancestors: Vec<serde_json::Value> = ancestors
        .iter()
        .map(|(kind, id)| serde_json::json!({"type": kind, "id": id}))
        .collect();
    serde_json::json!({"type": kind, "id": id, "properties": {"ancestors": ancestors}})
}

#[test]
fn the_pdp_checks_the_ancestors_and_applies_a_grant_to_the_nodes_below_it() {
    let pdp_port = free_port();
    let node = start_with(
        Arc::new(Backend::Sqlite.fresh()),
        None,
        &format!(
            r#"access {{ grants_mode = "hybrid" }}
            user "carol" {{ grants = [
              "query.run.read on service:db/host:local",
              "query.run.write on service:db/host:local/database:a%2Fb",
              "system.read on service:db",
              "query.run.write on service:db/database:old"
            ] }}
            pdp {{ listen_address = "127.0.0.1:{pdp_port}" url = "http://pdp.test:{pdp_port}" plain_http = true max_evaluations = 8 }}
            pdp_client "db" {{ token_env = "TEST_PDP_DB" }}
            pdp_client "chat" {{ token_env = "TEST_PDP_CHAT" }}"#
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
    // The catalogue holds db: an early read of its document keeps it in the registry.
    publish_catalog(&node, &[]);
    publish_tree(&node);
    let ask = |token: &str, path: &str, body: serde_json::Value| -> serde_json::Value {
        let reply = pdp(pdp_port, path, Some(token), &body.to_string(), None);
        assert_eq!(reply.status, 200, "{}", reply.body);
        reply.json()
    };
    let single = |subject: &str, action: &str, resource: serde_json::Value| {
        ask(
            PDP_DB_TOKEN,
            "/access/v1/evaluation",
            serde_json::json!({
                "subject": {"type": "user", "id": subject},
                "action": {"name": action},
                "resource": resource
            }),
        )
    };
    let yes = serde_json::json!({"decision": true});
    let no = serde_json::json!({"decision": false});
    let invalid = serde_json::json!({"decision": false, "context": {"reason": "invalid_resource"}});
    let service = [("service", "db")];
    let local = [("service", "db"), ("host", "local")];
    let other = [("service", "db"), ("host", "other")];

    // Prefix match: the grant on the host covers the host and its databases.
    assert_eq!(
        single(
            "carol",
            "query.run.read",
            node_of("host", "local", &service)
        ),
        yes
    );
    assert_eq!(
        single("carol", "query.run.read", node_of("database", "x", &local)),
        yes
    );
    assert_eq!(
        single("carol", "query.run.read", node_of("database", "x", &other)),
        no,
        "another host"
    );
    assert_eq!(
        single(
            "carol",
            "query.run.read",
            node_of("host", "other", &service)
        ),
        no
    );
    assert_eq!(
        single(
            "carol",
            "query.run.read",
            serde_json::json!({"type": "service", "id": "db"})
        ),
        no,
        "not the whole service"
    );
    // The ids are decoded in the request: the grant on a%2Fb covers the database a/b.
    assert_eq!(
        single(
            "carol",
            "query.run.write",
            node_of("database", "a/b", &local)
        ),
        yes
    );
    assert_eq!(
        single(
            "carol",
            "query.run.write",
            node_of("database", "a%2Fb", &local)
        ),
        no
    );
    assert_eq!(
        single(
            "carol",
            "query.run.write",
            node_of("host", "local", &service)
        ),
        no,
        "not the parent"
    );
    // The grant on the service covers each node.
    assert_eq!(
        single("carol", "system.read", node_of("host", "other", &service)),
        yes
    );
    // The superadmin, also on a node that no grant names.
    assert_eq!(
        single("root", "query.run.write", node_of("database", "z", &other)),
        yes
    );

    // The chain must start with the service of the server that asks, and follow the parents.
    for (resource, why) in [
        (
            node_of("database", "x", &service),
            "a database without its host",
        ),
        (
            serde_json::json!({"type": "database", "id": "x"}),
            "no ancestors: depth 1, but database is at depth 2",
        ),
        (
            node_of("database", "x", &[("service", "chat"), ("host", "local")]),
            "the service of another server",
        ),
        (
            node_of("database", "x", &[("host", "local")]),
            "no service first",
        ),
        (node_of("database", "x", &[]), "empty ancestors"),
        (
            node_of("table", "x", &local),
            "a type that the document does not declare",
        ),
        (node_of("host", "local", &local), "host below host"),
        (
            node_of("host", "local", &[("service", "db"), ("service", "db")]),
            "service twice",
        ),
        (
            serde_json::json!({"type": "service", "id": "db", "properties": {"ancestors": [{"type": "service", "id": "db"}]}}),
            "the service has no ancestors",
        ),
        (
            serde_json::json!({"type": "service", "id": "chat"}),
            "the service of another server",
        ),
        (
            node_of("database", "a\nb", &local),
            "a control character in an id",
        ),
    ] {
        assert_eq!(
            single("carol", "query.run.read", resource),
            invalid,
            "{why}"
        );
    }
    // An invalid resource comes before an unknown subject, and the superadmin gets it too.
    assert_eq!(
        single(
            "mallory",
            "query.run.read",
            node_of("database", "x", &service)
        ),
        invalid
    );
    assert_eq!(
        single("root", "query.run.read", node_of("database", "x", &service)),
        invalid
    );
    // A valid chain and an unknown subject: unknown_subject, as before.
    assert_eq!(
        single(
            "mallory",
            "query.run.read",
            node_of("database", "x", &local)
        ),
        serde_json::json!({"decision": false, "context": {"reason": "unknown_subject"}})
    );
    // A server without a known document: no decision below the service, and no reason.
    assert_eq!(
        ask(
            PDP_CHAT_TOKEN,
            "/access/v1/evaluation",
            serde_json::json!({
                "subject": {"type": "user", "id": "root"},
                "action": {"name": "x.read"},
                "resource": node_of("room", "r", &[("service", "chat")])
            })
        ),
        no
    );
    // A request with ancestors of a bad shape is refused with status 400.
    for bad in [
        serde_json::json!({"type": "database", "id": "x", "properties": {"ancestors": "service:db"}}),
        serde_json::json!({"type": "database", "id": "x", "properties": {"ancestors": [{"type": "service"}]}}),
        serde_json::json!({"type": "database", "id": "x", "properties": {"ancestors": ["service"]}}),
    ] {
        let body = serde_json::json!({
            "subject": {"type": "user", "id": "carol"},
            "action": {"name": "query.run.read"},
            "resource": bad
        });
        let reply = pdp(
            pdp_port,
            "/access/v1/evaluation",
            Some(PDP_DB_TOKEN),
            &body.to_string(),
            None,
        );
        assert_eq!(reply.status, 400, "{}", reply.body);
    }

    // The grant of the document of one level does not follow the tree: it does not apply, also
    // with the old form of the request.
    assert_eq!(
        single(
            "carol",
            "query.run.write",
            node_of("database", "old", &local)
        ),
        no
    );
    assert_eq!(
        single(
            "carol",
            "query.run.write",
            serde_json::json!({"type": "database", "id": "old"})
        ),
        invalid
    );

    // The batch: the same rules for each item, with the resource of the request as the default.
    let batch = ask(
        PDP_DB_TOKEN,
        "/access/v1/evaluations",
        serde_json::json!({
            "subject": {"type": "user", "id": "carol"},
            "action": {"name": "query.run.read"},
            "resource": node_of("database", "x", &local),
            "evaluations": [
                {},
                {"resource": node_of("database", "x", &other)},
                {"resource": node_of("database", "x", &service)},
                {"resource": node_of("host", "local", &service)},
                {"action": {"name": "query.run.write"}, "resource": node_of("database", "a/b", &local)}
            ]
        }),
    );
    assert_eq!(
        batch["evaluations"],
        serde_json::json!([yes, no, invalid, yes, yes])
    );

    // The action search on a node with its ancestors.
    let search = |resource: serde_json::Value| -> Vec<String> {
        ask(
            PDP_DB_TOKEN,
            "/access/v1/search/action",
            serde_json::json!({"subject": {"type": "user", "id": "carol"}, "resource": resource}),
        )["results"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| r["name"].as_str().unwrap().to_string())
            .collect()
    };
    assert_eq!(
        search(node_of("database", "a/b", &local)),
        ["query.run.read", "query.run.write", "system.read"]
    );
    assert_eq!(search(node_of("database", "x", &other)), ["system.read"]);
    assert!(
        search(node_of("database", "x", &service)).is_empty(),
        "an invalid resource"
    );

    // context.match = "subtree": a grant on a node below the resource also gives true.
    let with_context =
        |subject: &str, action: &str, resource: serde_json::Value, context: serde_json::Value| {
            ask(
                PDP_DB_TOKEN,
                "/access/v1/evaluation",
                serde_json::json!({
                    "subject": {"type": "user", "id": subject},
                    "action": {"name": action},
                    "resource": resource,
                    "context": context
                }),
            )
        };
    let subtree = serde_json::json!({"match": "subtree"});
    let node_match = serde_json::json!({"match": "node"});
    let invalid_request =
        serde_json::json!({"decision": false, "context": {"reason": "invalid_request"}});
    let db_service = serde_json::json!({"type": "service", "id": "db"});
    assert_eq!(
        with_context(
            "carol",
            "query.run.write",
            node_of("host", "local", &service),
            subtree.clone()
        ),
        yes,
        "the database a/b is below the host local"
    );
    assert_eq!(
        with_context(
            "carol",
            "query.run.write",
            node_of("host", "local", &service),
            node_match.clone()
        ),
        no,
        "node is the default"
    );
    assert_eq!(
        with_context(
            "carol",
            "query.run.write",
            db_service.clone(),
            subtree.clone()
        ),
        yes,
        "the service shows"
    );
    assert_eq!(
        with_context(
            "carol",
            "query.run.write",
            node_of("host", "other", &service),
            subtree.clone()
        ),
        no,
        "nothing below the host other"
    );
    assert_eq!(
        with_context(
            "carol",
            "query.run.write",
            node_of("database", "x", &local),
            subtree.clone()
        ),
        no,
        "a sibling of the database a/b"
    );
    assert_eq!(
        with_context(
            "carol",
            "query.run.read",
            node_of("database", "x", &local),
            subtree.clone()
        ),
        yes,
        "a grant above the resource still counts"
    );
    assert_eq!(
        with_context(
            "carol",
            "query.run.write",
            node_of("database", "old", &local),
            subtree.clone()
        ),
        no
    );
    assert_eq!(
        with_context(
            "root",
            "query.run.write",
            node_of("host", "other", &service),
            subtree.clone()
        ),
        yes,
        "the superadmin"
    );
    assert_eq!(
        with_context(
            "mallory",
            "query.run.write",
            node_of("host", "local", &service),
            subtree.clone()
        ),
        serde_json::json!({"decision": false, "context": {"reason": "unknown_subject"}})
    );
    assert_eq!(
        with_context(
            "carol",
            "query.run.write",
            node_of("database", "x", &service),
            subtree.clone()
        ),
        invalid
    );
    // In a request with one evaluation, a bad `context.match` gets HTTP 400 (protocol 11.1).
    for bad in [
        serde_json::json!({"match": "tree"}),
        serde_json::json!({"match": "Subtree"}),
        serde_json::json!({"match": 1}),
        serde_json::json!({"match": null}),
    ] {
        for subject in ["carol", "root"] {
            let reply = pdp(
                pdp_port,
                "/access/v1/evaluation",
                Some(PDP_DB_TOKEN),
                &serde_json::json!({
                    "subject": {"type": "user", "id": subject},
                    "action": {"name": "query.run.write"},
                    "resource": {"type": "host", "id": "local", "properties": {"ancestors": [{"type": "service", "id": "db"}]}},
                    "context": bad.clone()
                })
                .to_string(),
                None,
            );
            assert_eq!(reply.status, 400, "{bad}: {}", reply.body);
        }
    }
    // The batch: the default context of the request, replaced by the context of an item.
    let batch = ask(
        PDP_DB_TOKEN,
        "/access/v1/evaluations",
        serde_json::json!({
            "subject": {"type": "user", "id": "carol"},
            "action": {"name": "query.run.write"},
            "resource": node_of("host", "local", &service),
            "context": {"match": "subtree"},
            "evaluations": [
                {},
                {"context": {}},
                {"context": {"match": "node"}},
                {"resource": node_of("host", "other", &service)},
                {"context": {"match": "x"}}
            ]
        }),
    );
    assert_eq!(
        batch["evaluations"],
        serde_json::json!([yes, no, no, no, invalid_request])
    );
    let batch = ask(
        PDP_DB_TOKEN,
        "/access/v1/evaluations",
        serde_json::json!({
            "subject": {"type": "user", "id": "carol"},
            "action": {"name": "query.run.write"},
            "resource": node_of("host", "local", &service),
            "evaluations": [{}, {"context": {"match": "subtree"}}]
        }),
    );
    assert_eq!(batch["evaluations"], serde_json::json!([no, yes]));

    // The grants that do not follow the tree, for the warning of the log.
    let to_check = crate::http::grants_to_check(
        &node.state,
        "db",
        &node.state.actions.declared("db").unwrap(),
    );
    assert_eq!(
        to_check
            .iter()
            .map(|(login, grant, _)| format!("{login} {grant}"))
            .collect::<Vec<_>>(),
        ["carol query.run.write on service:db/database:old"]
    );

    // Compatibility: a document of one level reads a node without ancestors as depth 1.
    publish_catalog(&node, &["query.run.read", "query.run.write"]);
    assert_eq!(
        single(
            "carol",
            "query.run.write",
            serde_json::json!({"type": "database", "id": "old"})
        ),
        yes
    );
    assert_eq!(
        single(
            "carol",
            "query.run.write",
            node_of("database", "old", &service)
        ),
        yes
    );
    assert_eq!(
        single("carol", "query.run.read", node_of("database", "x", &local)),
        invalid,
        "no host in this document"
    );
}

#[test]
fn the_add_form_makes_an_account_without_grants_and_a_save_sets_them() {
    each_backend(|backend| {
        let node = start_with(Arc::new(backend.fresh()), Some(Duration::ZERO), "");
        publish_catalog(&node, &["query.run.read", "query.run.write"]);
        let root = sign_in(&node, "root", PASSWORD);
        let add = |fields: &str| {
            post(
                &node,
                &root,
                USERS_ADD_PATH,
                USERS_TOKEN,
                &format!("password=a-long-password-1&{fields}"),
            )
            .location()
            .map(str::to_string)
        };
        let grants_of = |login: &str| -> Vec<String> {
            node.state
                .users
                .grants_of(login)
                .unwrap()
                .iter()
                .map(ToString::to_string)
                .collect()
        };
        let give = |login: &str, fields: &str| {
            post(
                &node,
                &root,
                USERS_UPDATE_PATH,
                USERS_TOKEN,
                &format!("grants=1&login={login}&{fields}"),
            )
            .location()
            .map(str::to_string)
        };
        // The add form makes the account. The page of the account sets its grants.
        assert_eq!(
            add("login=alice"),
            Some("/auth/settings/users/alice?s=user-added".into())
        );
        assert_eq!(
            give("alice", "grant0=access+on+service%3Adb&grant1=query.run.read+on+service%3Adb&grant2=access+on+service%3Aother-grafana"),
            Some("/auth/settings/users/alice?s=user-updated".into())
        );
        assert_eq!(
            grants_of("alice"),
            [
                "access on service:db",
                "access on service:other-grafana",
                "query.run.read on service:db"
            ]
        );
        assert_eq!(
            node.state.store.load_users().unwrap()[0].grants,
            node.state.users.grants_of("alice").unwrap()
        );
        // The create page sends the grants with the account. A right on a service with no grant
        // `access` on it does nothing: the account is not made.
        // The page comes back with the login and the grants, names the service, and never sends
        // the password back.
        let refused = post(
            &node,
            &root,
            USERS_ADD_PATH,
            USERS_TOKEN,
            "password=a-long-password-1&login=uma&grants=1&grant0=query.run.read+on+service%3Adb",
        );
        assert_eq!(refused.status, 422, "{}", refused.body);
        assert!(refused
            .body
            .contains(r#"id="new-login" name="login" type="text" value="uma""#));
        assert!(refused
            .body
            .contains("a permission needs the switch Can open"));
        assert!(refused.body.contains("query.run.read on service:db"));
        assert!(!refused.body.contains("a-long-password-1"));
        assert_eq!(refused.header("cache-control"), Some("no-store"));
        assert!(node.state.users.get("uma").is_none());
        // An account without a grant has none.
        assert_eq!(
            add("login=carol"),
            Some("/auth/settings/users/carol?s=user-added".into())
        );
        assert!(grants_of("carol").is_empty());

        // The document of db names two hosts. A grant on a host is a grant like another: its path
        // follows the types of the document.
        publish_hosts(&node);
        assert_eq!(
            add("login=bob"),
            Some("/auth/settings/users/bob?s=user-added".into())
        );
        assert_eq!(
            give("bob", "grant0=access+on+service%3Adb&grant1=query.run.read+on+service%3Adb&grant2=access+on+service%3Aother-grafana&grant3=query.run.write+on+service%3Adb%2Fhost%3Alocal"),
            Some("/auth/settings/users/bob?s=user-updated".into())
        );
        assert_eq!(
            grants_of("bob"),
            [
                "access on service:db",
                "access on service:other-grafana",
                "query.run.read on service:db",
                "query.run.write on service:db/host:local",
            ]
        );
        // A path that does not follow the types of the document is refused: a database has a host
        // above it.
        assert_eq!(
            add("login=dave"),
            Some("/auth/settings/users/dave?s=user-added".into())
        );
        assert_eq!(
            give(
                "dave",
                "grant0=query.run.write+on+service%3Adb%2Fdatabase%3Ax"
            ),
            Some("/auth/settings/users/dave?s=edit-unknown-grant".into())
        );
        let bob = sign_in(&node, "bob", "a-long-password-1");
        assert_eq!(open(&node, &bob, "/db/").status, 204);

        // A Save sends every grant of the account. It replaces them.
        let reply = post(
            &node,
            &root,
            USERS_UPDATE_PATH,
            USERS_TOKEN,
            "grants=1&login=bob&grant0=query.run.write+on+service%3Adb",
        );
        assert_eq!(
            reply.location(),
            Some("/auth/settings/users/bob?s=needs-access"),
            "a right on a service needs the grant access on it"
        );
        assert_eq!(
            grants_of("bob").len(),
            4,
            "the refused form changes nothing"
        );
        let reply = post(
            &node,
            &root,
            USERS_UPDATE_PATH,
            USERS_TOKEN,
            "grants=1&login=bob&grant0=access+on+service%3Adb&grant1=query.run.write+on+service%3Adb",
        );
        assert_eq!(
            reply.location(),
            Some("/auth/settings/users/bob?s=user-updated")
        );
        assert_eq!(
            grants_of("bob"),
            ["access on service:db", "query.run.write on service:db"]
        );
        let reply = post(
            &node,
            &root,
            USERS_UPDATE_PATH,
            USERS_TOKEN,
            "grants=1&login=bob&grant0=access+on+service%3Aghost",
        );
        assert_eq!(
            reply.location(),
            Some("/auth/settings/users/bob?s=edit-unknown-grant"),
            "a service that the catalogue lacks is refused"
        );
        let reply = post(
            &node,
            &root,
            USERS_UPDATE_PATH,
            USERS_TOKEN,
            "grants=1&login=bob&grant0=access+on+service%3Adb&grant1=query.run.write+on+service%3Adb",
        );
        assert_eq!(
            reply.location(),
            Some("/auth/settings/users/bob?s=unchanged")
        );
        // A limit value follows the declaration of the document (portal 3.2).
        let reply = post(
            &node,
            &root,
            USERS_UPDATE_PATH,
            USERS_TOKEN,
            "grants=1&login=bob&grant0=access+on+service%3Adb&grant1=query.run.write+on+service%3Adb&limit1.max_rows=5",
        );
        assert_eq!(
            reply.location(),
            Some("/auth/settings/users/bob?s=bad-limits"),
            "the action lists no such limit"
        );
        // Only the superadmin adds.
        let alice = sign_in(&node, "alice", "a-long-password-1");
        assert_eq!(
            post(
                &node,
                &alice,
                USERS_ADD_PATH,
                USERS_TOKEN,
                "login=eve&password=a-long-password-1"
            )
            .status,
            403
        );
    });
}

#[test]
fn a_grant_that_the_whole_service_covers_is_dropped_and_a_host_stays_when_the_whole_service_goes() {
    each_backend(|backend| {
        let node = start_with(Arc::new(backend.fresh()), Some(Duration::ZERO), "");
        publish_catalog(&node, &["query.run.read", "query.run.write"]);
        publish_hosts(&node);
        let root = sign_in(&node, "root", PASSWORD);
        let grants_of = |login: &str| -> Vec<String> {
            node.state
                .users
                .grants_of(login)
                .unwrap()
                .iter()
                .map(ToString::to_string)
                .collect()
        };
        let access = "grant0=access+on+service%3Adb";
        let whole = "query.run.read+on+service%3Adb";
        let local = "query.run.read+on+service%3Adb%2Fhost%3Alocal";
        let replica = "query.run.read+on+service%3Adb%2Fhost%3Areplica";
        // The editor shows the hosts ticked when the whole service is ticked, so a form can send
        // all three grants. The account keeps the grant on the whole service only.
        let reply = post(
            &node,
            &root,
            USERS_ADD_PATH,
            USERS_TOKEN,
            "login=alice&password=a-long-password-1",
        );
        assert_eq!(
            reply.location(),
            Some("/auth/settings/users/alice?s=user-added")
        );
        let update = |fields: &str| {
            post(
                &node,
                &root,
                USERS_UPDATE_PATH,
                USERS_TOKEN,
                &format!("grants=1&login=alice&{fields}"),
            )
            .location()
            .map(str::to_string)
        };
        assert_eq!(
            update(&format!(
                "{access}&grant1={whole}&grant2={local}&grant3={replica}"
            )),
            Some("/auth/settings/users/alice?s=user-updated".into())
        );
        assert_eq!(
            grants_of("alice"),
            ["access on service:db", "query.run.read on service:db"]
        );
        // The whole service is unticked and one host stays ticked: the grant is now on that host.
        assert_eq!(
            update(&format!("{access}&grant1={replica}")),
            Some("/auth/settings/users/alice?s=user-updated".into())
        );
        assert_eq!(
            grants_of("alice"),
            [
                "access on service:db",
                "query.run.read on service:db/host:replica"
            ]
        );
        // The page holds the replica as a grant of its own, and not the local host.
        let page = page(&node, &root, "/auth/settings/users/alice");
        assert!(page.body.contains(r#"<input type="hidden" name="grant1" value="query.run.read on service:db/host:replica" form="grants">"#));
        assert!(!page.body.contains("host:local\" form="));
        // The whole service ticked again: one grant, and none for a host.
        assert_eq!(
            update(&format!("{access}&grant1={whole}")).as_deref(),
            Some("/auth/settings/users/alice?s=user-updated")
        );
        assert_eq!(
            grants_of("alice"),
            ["access on service:db", "query.run.read on service:db"]
        );
        let page = self::page(&node, &root, "/auth/settings/users/alice");
        assert!(page.body.contains(
            r#"<input type="hidden" name="grant1" value="query.run.read on service:db" form="grants">"#
        ));
        assert!(!page.body.contains(r#"host:replica" form="#));
    });
}

#[test]
fn in_the_static_mode_a_save_cannot_give_access() {
    let node = start_custom(
        Arc::new(Backend::Sqlite.fresh()),
        None,
        Custom {
            allow_creation: false,
            ..Custom::default()
        },
        r#"access { grants_mode = "static" }"#,
    );
    publish_catalog(&node, &[]);
    let root = sign_in(&node, "root", PASSWORD);
    let add = post(
        &node,
        &root,
        USERS_ADD_PATH,
        USERS_TOKEN,
        "login=alice&password=a-long-password-1",
    );
    assert_eq!(add.status, 404, "the create route does not exist");
    insert_account(&node, "alice", "a-long-password-1");
    assert_eq!(
        post(
            &node,
            &root,
            USERS_UPDATE_PATH,
            USERS_TOKEN,
            "grants=1&login=alice&grant0=access+on+service%3Adb",
        )
        .location(),
        Some("/auth/settings/users/alice?s=grants-static")
    );
    assert!(node.state.users.grants_of("alice").unwrap().is_empty());
}

/// The document of `db` with a tree, and with two hosts that it names: `local` takes the read and the
/// write, `replica` takes the read only.
fn publish_hosts(node: &Node) {
    let document = crate::actions::parse(
        "db",
        br#"{"server":"db","resource_types":[{"name":"host","parent":"service","description":"The type host."},{"name":"database","parent":"host","description":"The type database."}],"actions":[{"name":"query.run.read","resource_types":["service","host","database"],"description":"The action query.run.read."},{"name":"query.run.write","resource_types":["service","host","database"],"description":"The action query.run.write."},{"name":"system.read","resource_types":["service","host"],"description":"The action system.read."}],"resources":[{"path":"host:local","label":"Local","actions":["query.run.read","query.run.write"]},{"path":"host:replica","actions":["query.run.read"]}]}"#,
    )
    .unwrap();
    node.state.actions.set_document("db", document);
}

/// The document of `db` with a tree: `host` below the service, `database` below `host`.
fn publish_tree(node: &Node) {
    let document = crate::actions::parse(
        "db",
        br#"{"server":"db","resource_types":[{"name":"host","parent":"service","description":"The type host."},{"name":"database","parent":"host","description":"The type database."}],"actions":[{"name":"query.run.read","resource_types":["service","host","database"],"description":"The action query.run.read."},{"name":"query.run.write","resource_types":["service","host","database"],"description":"The action query.run.write."},{"name":"system.read","resource_types":["service","host"],"description":"The action system.read."}]}"#,
    )
    .unwrap();
    node.state.actions.set_document("db", document);
}

/// A service that lists options. It answers `GET /.well-known/procurauth/resources`: the databases
/// of the host `prod` and a 404 for the rest. It records each request line and its headers.
fn options_service() -> (u16, Arc<std::sync::Mutex<Vec<String>>>) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
    let log = Arc::clone(&seen);
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let mut buffer = [0u8; 4096];
            let n = stream.read(&mut buffer).unwrap_or(0);
            let request = String::from_utf8_lossy(&buffer[..n]).to_string();
            let first = request.lines().next().unwrap_or("").to_string();
            log.lock().unwrap().push(request.to_lowercase());
            let (status, body) = if first.contains("type=database")
                && first.contains("parent=host%3Aprod")
            {
                (
                    "200 OK",
                    r#"{"items":[{"id":"test1","label":"test1","has_children":true},{"id":"a/b","label":"a/b"}],"next":"p2"}"#,
                )
            } else {
                ("404 Not Found", "{}")
            };
            let _ = write!(
                stream,
                "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
        }
    });
    (port, seen)
}

#[test]
fn the_options_of_a_node_come_from_the_service_with_the_token_of_the_person() {
    each_backend(|backend| {
        let node = start_with(
            Arc::new(backend.fresh()),
            Some(Duration::ZERO),
            r#"identity { issuer = "https://portal.test" signing_key_env = "TEST_IDENTITY_KEY" token_ttl = "5m" }"#,
        );
        let (port, seen) = options_service();
        let catalog = Catalog::build(
            vec![listed(&[
                ("portal.name", "Db"),
                ("portal.id", "db"),
                ("portal.href", "/db/"),
                (
                    "portal.actions",
                    &format!("http://127.0.0.1:{port}/.well-known/procurauth/actions"),
                ),
            ])],
            Vec::new(),
        );
        *node.state.catalog.write().unwrap() = Arc::new(catalog);
        publish_tree(&node);
        let root = sign_in(&node, "root", PASSWORD);
        let get = |session: &str, query: &str| {
            let cookie = format!("{}={session}", session_cookie_name(&node));
            send(
                node.port,
                "GET",
                &format!("/auth/options{query}"),
                &[("Cookie", &cookie)],
                "",
            )
        };
        let reply = get(&root, "?service=db&type=database&parent=host%3Aprod");
        assert_eq!(reply.status, 200, "{}", reply.body);
        let json: serde_json::Value = serde_json::from_str(&reply.body).unwrap();
        assert_eq!(json["items"][0]["id"], "test1");
        assert_eq!(json["items"][1]["id"], "a/b");
        assert_eq!(json["next"], "p2");
        assert_eq!(json["unavailable"], false);
        {
            let seen = seen.lock().unwrap();
            assert_eq!(seen.len(), 1);
            assert!(
                seen[0].contains("get /.well-known/procurauth/resources?parent=host%3aprod&type=database&limit=50"),
                "{}",
                seen[0]
            );
            assert!(
                seen[0].contains("x-identity-token: "),
                "the token of the person"
            );
        }
        // The portal keeps the answer: the same question does not reach the service again.
        assert_eq!(
            get(&root, "?service=db&type=database&parent=host%3Aprod").status,
            200
        );
        assert_eq!(seen.lock().unwrap().len(), 1);
        // A page and a search are other questions.
        get(
            &root,
            "?service=db&type=database&parent=host%3Aprod&after=p2&q=t",
        );
        assert_eq!(seen.lock().unwrap().len(), 2);
        // A service that has no list gives no option, and the form says it.
        let none = get(&root, "?service=db&type=host");
        assert_eq!(none.status, 200);
        let json: serde_json::Value = serde_json::from_str(&none.body).unwrap();
        assert_eq!(json["unavailable"], true);
        assert_eq!(json["items"], serde_json::json!([]));
        // A type below another type, an unknown type, a missing parameter and a repeated one.
        for bad in [
            "?service=db&type=database",
            "?service=db&type=nothing",
            "?service=db",
            "?type=host",
            "?service=db&type=host&type=host",
            "?service=db&type=host&other=1",
            "?service=db&type=database&parent=bad",
        ] {
            assert_eq!(get(&root, bad).status, 400, "{bad}");
        }
        // A service that the catalogue lacks gives no option.
        let ghost = get(&root, "?service=ghost&type=host");
        assert!(
            ghost.body.contains(r#""unavailable":false"#),
            "{}",
            ghost.body
        );
        // No session: 401. A user without a right on the keys: 403.
        let cookie_less = send(
            node.port,
            "GET",
            "/auth/options?service=db&type=host",
            &[],
            "",
        );
        assert_eq!(cookie_less.status, 401);
        let alice = user_without_grant(&node, &root, "alice");
        assert_eq!(get(&alice, "?service=db&type=host").status, 403);
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

/// A source that never answered gives a snapshot without its services. The instance does not
/// open its ports until the catalogue is complete (portal 7.3b), so no user reaches the gate.
#[test]
fn a_source_that_never_answered_keeps_the_ports_closed() {
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
    wait_until(Duration::from_secs(10), || {
        node.state.discovery.refreshes() >= 1
    });
    assert!(!current_catalog(&node.state).is_complete());
    std::thread::sleep(Duration::from_millis(500));
    assert!(
        TcpStream::connect(("127.0.0.1", node.port)).is_err(),
        "the port stays closed"
    );
}

/// On SIGTERM the instance serves 5 more seconds, then it closes its listener and its idle
/// connections (portal 7.3a).
#[test]
fn the_instance_serves_five_seconds_after_the_stop_and_then_closes() {
    let (tx, rx) = tokio::sync::oneshot::channel();
    let node = start_custom(
        Arc::new(Store::open_in_memory()),
        None,
        Custom {
            stop: Some(rx),
            ..Custom::default()
        },
        "",
    );
    // An idle connection that the client keeps open.
    let mut idle = TcpStream::connect(("127.0.0.1", node.port)).unwrap();
    idle.write_all(b"GET /healthz HTTP/1.1\r\nHost: x\r\n\r\n")
        .unwrap();
    let mut first = [0_u8; 512];
    assert!(idle.read(&mut first).unwrap() > 0);
    assert_eq!(node.state.stop.live(), 1);
    let stopped = Instant::now();
    tx.send(()).unwrap();
    std::thread::sleep(Duration::from_millis(1500));
    assert_eq!(
        send(node.port, "GET", "/healthz", &[], "").status,
        200,
        "the instance still serves in the first seconds"
    );
    wait_until(Duration::from_secs(10), || {
        TcpStream::connect(("127.0.0.1", node.port)).is_err()
    });
    let closed_after = stopped.elapsed();
    assert!(
        closed_after >= Duration::from_millis(4800),
        "the listener closed too early: {closed_after:?}"
    );
    idle.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    let mut rest = [0_u8; 16];
    assert_eq!(
        idle.read(&mut rest).unwrap(),
        0,
        "the idle connection closed"
    );
    wait_until(Duration::from_secs(5), || node.state.stop.live() == 0);
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
        assert!(settings.body.contains(r#"aria-label="End all sessions""#));
        assert!(settings
            .body
            .contains(r#"name="confirm" value="end-all-sessions""#));
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
            page(&first, &again, "/auth/settings/activity?v=changes")
                .body
                .contains("All sessions ended")
        });
    });
}

// Keys at the PDP --------------------------------------------------------------------------------

/// The service `db` of the tests of the keys: hosts and databases, a budget and two ceilings.
const KEY_TEST_DOCUMENT: &str = r#"{"server":"db",
  "resource_types":[{"name":"host","parent":"service","description":"A host."},
                    {"name":"database","parent":"host","description":"A database."}],
  "actions":[
    {"name":"data.read","resource_types":["host","database"],"description":"Read.",
     "limits":["requests","max_rows","max_duration"]},
    {"name":"data.write","resource_types":["host","database"],"description":"Write.",
     "limits":["max_duration"]}],
  "limits":[
    {"name":"requests","label":"Requests","description":"d","unit":"request","kind":"budget"},
    {"name":"max_rows","label":"Rows","description":"d","unit":"row","kind":"ceiling","min":1,"max":100000},
    {"name":"max_duration","label":"Duration","description":"d","unit":"second","kind":"ceiling","min":1,"max":3600}]}"#;

fn limits(text: &str) -> crate::limits::Limits {
    crate::limits::Limits::from_json(text).unwrap()
}

fn key_caps() -> crate::store::KeyCaps {
    crate::store::KeyCaps {
        max_per_user: 100,
        max_counter_rows: 1_000_000,
        max_directory_grants: 1_000_000,
        now: unix_now(),
        created_not_before: None,
    }
}

/// A node with the blocks of the keys and the service `db` with its document.
fn key_node(backend: &Backend) -> (Node, u16) {
    let pdp_port = free_port();
    let node = start_with(
        Arc::new(backend.fresh()),
        None,
        &format!(
            r#"identity {{ issuer = "https://portal.test" signing_key_env = "TEST_IDENTITY_KEY" }}
            pdp {{ listen_address = "127.0.0.1:{pdp_port}" url = "http://pdp.test:{pdp_port}" plain_http = true max_evaluations = 4 }}
            keys {{}}
            pdp_client "db" {{ token_env = "TEST_PDP_DB" }}"#
        ),
    );
    publish_catalog(&node, &[]);
    node.state.actions.set_document(
        "db",
        crate::actions::parse("db", KEY_TEST_DOCUMENT.as_bytes()).unwrap(),
    );
    wait_until(Duration::from_secs(10), || {
        TcpStream::connect(("127.0.0.1", pdp_port)).is_ok()
    });
    (node, pdp_port)
}

/// Alice reads `host:prod` with `max_rows` 5000 and a budget of 3 each minute. She can use keys.
fn add_alice(node: &Node, extra: &[Grant]) {
    let mut grants = vec![
        Grant::new("access", "db")
            .unwrap()
            .with_limits(limits(r#"{"requests":[{"amount":3,"window":"minute"}]}"#)),
        Grant::from_path_text("data.read", "db", "host:prod")
            .unwrap()
            .with_limits(limits(r#"{"max_rows":5000}"#)),
        Grant::parse("keys.own.use on service:portal/key_kind:mcp").unwrap(),
        Grant::parse("keys.own.read on service:portal").unwrap(),
    ];
    grants.extend(extra.iter().cloned());
    grants.sort();
    node.state
        .store
        .insert_user(&crate::store::UserRow {
            login: "alice".into(),
            password_hash: password_hash(PASSWORD),
            grants,
            created_at: 1,
        })
        .unwrap();
    node.state.users.refresh(&node.state.store).unwrap();
}

fn add_key(node: &Node, id: &str, owner: Option<&str>, byte: u8, grants: Vec<Grant>) {
    let mut grants = grants;
    grants.sort();
    node.state
        .store
        .insert_key(
            &crate::store::KeyRow {
                kind: crate::keys::KeyKind::Mcp,
                id: id.into(),
                owner: owner.map(Into::into),
                name: format!("key {id}"),
                digest: [byte; 32],
                hint: "ab12".into(),
                created_at: unix_now() - 10,
                expires_at: None,
                last_check_at: None,
                check_count: 0,
                grants,
            },
            &key_caps(),
        )
        .unwrap();
    node.state.users.refresh(&node.state.store).unwrap();
}

fn read_body(subject: &str, database: &str, context: serde_json::Value) -> String {
    serde_json::json!({
        "subject": {"type": "user", "id": subject},
        "action": {"name": "data.read"},
        "resource": {"type": "database", "id": database, "properties": {"ancestors": [
            {"type": "service", "id": "db"}, {"type": "host", "id": "prod"}]}},
        "context": context
    })
    .to_string()
}

#[test]
fn the_pdp_decides_for_a_key_with_its_owner_its_ceilings_and_its_counters() {
    each_backend(|backend| {
        let (node, port) = key_node(backend);
        add_alice(&node, &[]);
        add_key(
            &node,
            "9b1c04e2aa71f0c3",
            Some("alice"),
            1,
            vec![
                Grant::new("access", "db")
                    .unwrap()
                    .with_limits(limits(r#"{"requests":[{"amount":2,"window":"minute"}]}"#)),
                Grant::from_path_text("data.read", "db", "host:prod/database:test2")
                    .unwrap()
                    .with_limits(limits(r#"{"max_rows":100}"#)),
            ],
        );
        let ask = |body: String| -> serde_json::Value {
            let reply = pdp(
                port,
                "/access/v1/evaluation",
                Some(PDP_DB_TOKEN),
                &body,
                None,
            );
            assert_eq!(reply.status, 200, "{}", reply.body);
            reply.json()
        };
        let key = "key:9b1c04e2aa71f0c3";
        let usage = serde_json::json!({"usage": {"units": 1}});
        // No usage: the answer has the ceilings, the lowest of the key and of its owner, and no
        // counter moves.
        assert_eq!(
            ask(read_body(key, "test2", serde_json::json!({}))),
            serde_json::json!({"decision": true, "context": {"limits": {"max_rows": 100}}})
        );
        // The key has no grant on test1, although its owner reads it.
        assert_eq!(
            ask(read_body(key, "test1", serde_json::json!({}))),
            serde_json::json!({"decision": false})
        );
        assert_eq!(
            ask(read_body("alice", "test1", serde_json::json!({}))),
            serde_json::json!({"decision": true, "context": {"limits": {"max_rows": 5000}}})
        );
        // Two uses fit the budget of the key. Each use takes one unit from the counter of the key
        // and one from the counter of Alice.
        for _ in 0..2 {
            assert_eq!(
                ask(read_body(key, "test2", usage.clone())),
                serde_json::json!({"decision": true, "context": {"limits": {"max_rows": 100}, "usage": {"units": 1}}})
            );
        }
        let refused = ask(read_body(key, "test2", usage.clone()));
        assert_eq!(refused["decision"], false);
        assert_eq!(refused["context"]["reason"], "budget_exceeded");
        let retry = refused["context"]["retry_after"].as_u64().unwrap();
        assert!((1..=60).contains(&retry), "{retry}");
        assert!(refused["context"].get("limits").is_none());
        // The budget of Alice has one unit left: the refusal of the key took nothing from it.
        assert_eq!(
            ask(read_body("alice", "test1", usage.clone()))["context"]["usage"]["units"],
            1
        );
        let refused = ask(read_body("alice", "test1", usage.clone()));
        assert_eq!(refused["context"]["reason"], "budget_exceeded");
        // The superadmin takes no unit.
        assert_eq!(
            ask(read_body("root", "test1", usage.clone())),
            serde_json::json!({"decision": true, "context": {"usage": {"units": 0}}})
        );
        // A person without a budget value counts no unit either.
        node.state
            .store
            .insert_user(&crate::store::UserRow {
                login: "bob".into(),
                password_hash: password_hash(PASSWORD),
                grants: vec![Grant::from_path_text("data.read", "db", "host:prod").unwrap()],
                created_at: 1,
            })
            .unwrap();
        node.state.users.refresh(&node.state.store).unwrap();
        assert_eq!(
            ask(read_body("bob", "test1", usage.clone())),
            serde_json::json!({"decision": true, "context": {"usage": {"units": 0}}})
        );
        // A false answer takes no unit and has no limit.
        assert_eq!(
            ask(read_body("bob", "test1", serde_json::json!({}))),
            serde_json::json!({"decision": true})
        );
    });
}

#[test]
fn a_key_that_is_unknown_revoked_expired_too_old_or_without_an_owner_right_is_an_unknown_subject() {
    each_backend(|backend| {
        let (node, port) = key_node(backend);
        add_alice(&node, &[]);
        let read = vec![Grant::from_path_text("data.read", "db", "host:prod").unwrap()];
        for (id, byte) in [("a1", 1u8), ("a2", 2), ("a3", 3)] {
            add_key(&node, id, Some("alice"), byte, read.clone());
        }
        let ask = |subject: &str| -> serde_json::Value {
            let reply = pdp(
                port,
                "/access/v1/evaluation",
                Some(PDP_DB_TOKEN),
                &read_body(subject, "test1", serde_json::json!({})),
                None,
            );
            assert_eq!(reply.status, 200, "{}", reply.body);
            reply.json()
        };
        let unknown =
            serde_json::json!({"decision": false, "context": {"reason": "unknown_subject"}});
        let yes = serde_json::json!({"decision": true, "context": {"limits": {"max_rows": 5000}}});
        assert_eq!(ask("key:a1"), yes);
        assert_eq!(ask("key:nope"), unknown);
        assert_eq!(ask("key:UPPER"), unknown, "a bad local part");
        assert_eq!(
            ask("other:a1"),
            unknown,
            "a prefix that the protocol does not define"
        );
        // A revoked key is an unknown subject after the next round of the directory.
        node.state.store.revoke_key("a1", None, unix_now()).unwrap();
        node.state.users.refresh(&node.state.store).unwrap();
        assert_eq!(ask("key:a1"), unknown);
        // An expired key.
        node.state.store.execute_raw(&format!(
            "UPDATE keys SET expires_at = {} WHERE id = 'a2';
             UPDATE directory_version SET version = version + 1",
            unix_now() - 1
        ));
        node.state.users.refresh(&node.state.store).unwrap();
        assert_eq!(ask("key:a2"), unknown);
        // The owner loses the right `keys.own.use`: the key gets the same answer (portal 9.3).
        assert_eq!(ask("key:a3"), yes);
        let without_use: Vec<Grant> = node
            .state
            .users
            .grants_of("alice")
            .unwrap()
            .into_iter()
            .filter(|g| g.action != "keys.own.use")
            .collect();
        node.state
            .store
            .update_user("alice", None, Some(&without_use))
            .unwrap();
        node.state.users.refresh(&node.state.store).unwrap();
        assert_eq!(ask("key:a3"), unknown);
        assert_eq!(
            ask("alice")["decision"],
            true,
            "the person keeps the other rights"
        );
        // The value 0 of `max_key_lifetime` refuses each key (portal 5.3a).
        let zero: Vec<Grant> = without_use
            .into_iter()
            .chain([Grant::parse("keys.own.use on service:portal/key_kind:mcp")
                .unwrap()
                .with_limits(limits(r#"{"max_key_lifetime":0}"#))])
            .collect();
        node.state
            .store
            .update_user("alice", None, Some(&zero))
            .unwrap();
        node.state.users.refresh(&node.state.store).unwrap();
        assert_eq!(ask("key:a3"), unknown, "the key is older than 0 seconds");
        // A high value lets the key work again.
        let high: Vec<Grant> = zero
            .into_iter()
            .filter(|g| g.action != "keys.own.use")
            .chain([Grant::parse("keys.own.use on service:portal/key_kind:mcp")
                .unwrap()
                .with_limits(limits(r#"{"max_key_lifetime":3600}"#))])
            .collect();
        node.state
            .store
            .update_user("alice", None, Some(&high))
            .unwrap();
        node.state.users.refresh(&node.state.store).unwrap();
        assert_eq!(ask("key:a3"), yes);
    });
}

#[test]
fn a_key_of_the_superadmin_has_only_its_own_grants_and_its_own_counters() {
    each_backend(|backend| {
        let (node, port) = key_node(backend);
        add_key(
            &node,
            "5e0000000000000a",
            None,
            9,
            vec![
                Grant::new("access", "db")
                    .unwrap()
                    .with_limits(limits(r#"{"requests":[{"amount":1,"window":"minute"}]}"#)),
                Grant::from_path_text("data.read", "db", "host:prod")
                    .unwrap()
                    .with_limits(limits(r#"{"max_rows":7}"#)),
            ],
        );
        let ask = |body: String| -> serde_json::Value {
            let reply = pdp(
                port,
                "/access/v1/evaluation",
                Some(PDP_DB_TOKEN),
                &body,
                None,
            );
            assert_eq!(reply.status, 200, "{}", reply.body);
            reply.json()
        };
        let usage = serde_json::json!({"usage": {"units": 1}});
        let key = "key:5e0000000000000a";
        assert_eq!(
            ask(read_body(key, "x", usage.clone())),
            serde_json::json!({"decision": true, "context": {"limits": {"max_rows": 7}, "usage": {"units": 1}}})
        );
        assert_eq!(
            ask(read_body(key, "x", usage))["context"]["reason"],
            "budget_exceeded"
        );
        // A key of the superadmin never gets more than its own grants.
        let write = serde_json::json!({
            "subject": {"type": "user", "id": key},
            "action": {"name": "data.write"},
            "resource": {"type": "host", "id": "prod", "properties": {"ancestors": [{"type": "service", "id": "db"}]}}
        });
        assert_eq!(
            ask(write.to_string()),
            serde_json::json!({"decision": false})
        );
    });
}

#[test]
fn a_request_with_context_usage_follows_the_rules_of_protocol_6() {
    each_backend(|backend| {
        let (node, port) = key_node(backend);
        add_alice(&node, &[]);
        let call = |path: &str, body: serde_json::Value| {
            pdp(port, path, Some(PDP_DB_TOKEN), &body.to_string(), None)
        };
        let item = |context: serde_json::Value| {
            serde_json::json!({
                "subject": {"type": "user", "id": "alice"},
                "action": {"name": "data.read"},
                "resource": {"type": "host", "id": "prod", "properties": {"ancestors": [{"type": "service", "id": "db"}]}},
                "context": context
            })
        };
        // Any other value than {"units": 1} gets 400.
        for bad in [
            serde_json::json!({"usage": {"units": 2}}),
            serde_json::json!({"usage": {"units": 0}}),
            serde_json::json!({"usage": {"units": "1"}}),
            serde_json::json!({"usage": {}}),
            serde_json::json!({"usage": {"units": 1, "other": 1}}),
            serde_json::json!({"usage": 1}),
            serde_json::json!({"usage": {"units": 1}, "match": "subtree"}),
        ] {
            let reply = call("/access/v1/evaluation", item(bad.clone()));
            assert_eq!(reply.status, 400, "{bad}: {}", reply.body);
        }
        assert_eq!(
            call(
                "/access/v1/evaluation",
                item(serde_json::json!({"usage": {"units": 1}}))
            )
            .status,
            200
        );
        // An Access Evaluations request with items takes no usage, at any level.
        let mut batch = item(serde_json::json!({}));
        batch["evaluations"] = serde_json::json!([{"context": {"usage": {"units": 1}}}]);
        assert_eq!(call("/access/v1/evaluations", batch.clone()).status, 400);
        let mut batch = item(serde_json::json!({"usage": {"units": 1}}));
        batch["evaluations"] = serde_json::json!([{}]);
        assert_eq!(call("/access/v1/evaluations", batch).status, 400);
        // A request without items is one evaluation: usage is permitted.
        let mut single = item(serde_json::json!({"usage": {"units": 1}}));
        single["evaluations"] = serde_json::json!([]);
        assert_eq!(call("/access/v1/evaluations", single).status, 200);
        // A search takes no usage.
        let search = serde_json::json!({
            "subject": {"type": "user", "id": "alice"},
            "resource": {"type": "host", "id": "prod", "properties": {"ancestors": [{"type": "service", "id": "db"}]}},
            "context": {"usage": {"units": 1}}
        });
        assert_eq!(call("/access/v1/search/action", search).status, 400);
        // The superadmin: the checks of the resource come first.
        let root = serde_json::json!({
            "subject": {"type": "user", "id": "root"},
            "action": {"name": "data.read"},
            "resource": {"type": "service", "id": "other"},
            "context": {"usage": {"units": 1}}
        });
        let reply = call("/access/v1/evaluation", root);
        assert_eq!(
            reply.json(),
            serde_json::json!({"decision": false, "context": {"reason": "invalid_resource"}})
        );
        // Bad ancestors: HTTP 400 for one evaluation, `invalid_request` in a batch item.
        let mut bad = item(serde_json::json!({}));
        bad["resource"]["properties"]["ancestors"] = serde_json::json!("service");
        assert_eq!(call("/access/v1/evaluation", bad.clone()).status, 400);
        let mut batch = item(serde_json::json!({}));
        batch["evaluations"] = serde_json::json!([
            {},
            {"resource": bad["resource"].clone()},
            {"context": {"match": "bad"}}
        ]);
        let reply = call("/access/v1/evaluations", batch);
        assert_eq!(reply.status, 200, "{}", reply.body);
        let answers = reply.json()["evaluations"].clone();
        assert_eq!(answers[0]["decision"], true);
        assert_eq!(
            answers[1],
            serde_json::json!({"decision": false, "context": {"reason": "invalid_request"}})
        );
        assert_eq!(
            answers[2],
            serde_json::json!({"decision": false, "context": {"reason": "invalid_request"}})
        );
    });
}

#[test]
fn a_counter_that_cannot_be_counted_gives_the_status_503_and_no_decision() {
    each_backend(|backend| {
        let (node, port) = key_node(backend);
        add_alice(&node, &[]);
        let usage = serde_json::json!({"usage": {"units": 1}});
        let reply = pdp(
            port,
            "/access/v1/evaluation",
            Some(PDP_DB_TOKEN),
            &read_body("alice", "test1", usage.clone()),
            None,
        );
        assert_eq!(reply.status, 200);
        // A counter row that the table does not hold (portal 7.2b).
        node.state.store.execute_raw("DELETE FROM budget_counters");
        let reply = pdp(
            port,
            "/access/v1/evaluation",
            Some(PDP_DB_TOKEN),
            &read_body("alice", "test1", usage.clone()),
            None,
        );
        assert_eq!(reply.status, 503, "{}", reply.body);
        assert!(reply.json().get("decision").is_none(), "never a decision");
        assert_eq!(
            node.state
                .counting
                .stats
                .row_missing
                .load(std::sync::atomic::Ordering::Relaxed),
            1
        );
        // A question without usage counts nothing, so it still gets its decision.
        let reply = pdp(
            port,
            "/access/v1/evaluation",
            Some(PDP_DB_TOKEN),
            &read_body("alice", "test1", serde_json::json!({})),
            None,
        );
        assert_eq!(reply.status, 200);
    });
}

#[test]
fn the_pdp_refuses_each_key_when_the_directory_is_too_old_and_without_the_block_keys() {
    each_backend(|backend| {
        let (node, port) = key_node(backend);
        add_alice(&node, &[]);
        add_key(
            &node,
            "a1",
            Some("alice"),
            1,
            vec![Grant::from_path_text("data.read", "db", "host:prod").unwrap()],
        );
        let ask = |subject: &str| -> serde_json::Value {
            pdp(
                port,
                "/access/v1/evaluation",
                Some(PDP_DB_TOKEN),
                &read_body(subject, "test1", serde_json::json!({})),
                None,
            )
            .json()
        };
        assert_eq!(ask("key:a1")["decision"], true);
        if node.state.store.is_shared() {
            // The last good read is older than `keys.directory_max_age` (protocol 3.9).
            node.state.users.set_last_success(0);
            assert_eq!(ask("key:a1"), serde_json::json!({"decision": false}));
            assert_eq!(
                ask("alice")["decision"],
                true,
                "a person keeps the old decisions"
            );
            node.state.users.refresh(&node.state.store).unwrap();
            assert_eq!(ask("key:a1")["decision"], true);
        }
    });
    // Without the block `keys`, each key is an unknown subject (portal 10.1).
    let pdp_port = free_port();
    let node = start_with(
        Arc::new(Backend::Sqlite.fresh()),
        None,
        &format!(
            r#"pdp {{ listen_address = "127.0.0.1:{pdp_port}" url = "http://pdp.test:{pdp_port}" plain_http = true }}
            pdp_client "db" {{ token_env = "TEST_PDP_DB" }}"#
        ),
    );
    publish_catalog(&node, &[]);
    node.state.actions.set_document(
        "db",
        crate::actions::parse("db", KEY_TEST_DOCUMENT.as_bytes()).unwrap(),
    );
    wait_until(Duration::from_secs(10), || {
        TcpStream::connect(("127.0.0.1", pdp_port)).is_ok()
    });
    let reply = pdp(
        pdp_port,
        "/access/v1/evaluation",
        Some(PDP_DB_TOKEN),
        &read_body("key:a1", "test1", serde_json::json!({})),
        None,
    );
    assert_eq!(
        reply.json(),
        serde_json::json!({"decision": false, "context": {"reason": "unknown_subject"}})
    );
}

// The conformance cases of the series D (a PDP) --------------------------------------------------

/// The cases of the public repository of the protocol (`conformance/`). Each case loads its
/// directory in the portal, sends its steps to the PDP and checks the answers. The test reads the
/// path of the directory `conformance/` in `PROCURAUTH_CONFORMANCE`. Without this variable, the test
/// does nothing.
mod conformance_d {
    use super::*;
    use serde_json::Value;
    use sha2::{Digest, Sha256};

    fn conformance_dir() -> Option<std::path::PathBuf> {
        std::env::var_os("PROCURAUTH_CONFORMANCE").map(std::path::PathBuf::from)
    }

    fn read_json(path: &std::path::Path) -> Value {
        let text = std::fs::read_to_string(path).unwrap_or_else(|e| panic!("{path:?}: {e}"));
        serde_json::from_str(&text).unwrap_or_else(|e| panic!("{path:?}: {e}"))
    }

    /// The service ids of a directory, with their documents and the credential of each.
    struct Service {
        id: String,
        document: Option<Value>,
        credential: String,
    }

    /// The text of a credential: the one that a step of the case gives for this digest, or one that
    /// the harness makes.
    fn credential_of(id: &str, digest: Option<&str>, case: &Value) -> String {
        let made = format!("{id}-credential-harness-0123456789abcdef");
        let Some(digest) = digest else {
            return made;
        };
        let mut found = None;
        let mut visit = |value: &Value| {
            if let Some(text) = value
                .get("request")
                .and_then(|r| r.get("headers"))
                .and_then(|h| h.as_object())
                .and_then(|h| {
                    h.iter()
                        .find(|(name, _)| name.eq_ignore_ascii_case("authorization"))
                })
                .and_then(|(_, v)| v.as_str())
                .and_then(|v| v.strip_prefix("Bearer "))
            {
                let hex: String = Sha256::digest(text.as_bytes())
                    .iter()
                    .map(|b| format!("{b:02x}"))
                    .collect();
                if hex == digest {
                    found = Some(text.to_string());
                }
            }
        };
        if let Some(steps) = case.get("steps").and_then(Value::as_array) {
            steps.iter().for_each(&mut visit);
        }
        found.unwrap_or(made)
    }

    fn limits_text(limits: Option<&Value>) -> String {
        limits.map_or_else(|| "{}".to_string(), Value::to_string)
    }

    fn grants_of(subject: &Value) -> Vec<Grant> {
        let mut grants: Vec<Grant> = subject["grants"]
            .as_array()
            .map(Vec::as_slice)
            .unwrap_or(&[])
            .iter()
            .map(|g| {
                let text = format!(
                    "{} on {}",
                    g["action"].as_str().unwrap(),
                    g["resource"].as_str().unwrap()
                );
                Grant::parse(&text)
                    .unwrap_or_else(|e| panic!("the grant {text:?} of the directory: {e}"))
                    .with_limits(
                        crate::limits::Limits::from_json(&limits_text(g.get("limits")))
                            .unwrap_or_else(|e| panic!("the limits of {text:?}: {e}")),
                    )
            })
            .collect();
        grants.sort();
        grants
    }

    /// The default service of a request: the service of its resource.
    fn service_of(body: &Value) -> Option<String> {
        let resource = body
            .get("resource")
            .or_else(|| body.get("evaluations")?.get(0)?.get("resource"))?;
        if resource["type"] == "service" {
            return resource["id"].as_str().map(str::to_string);
        }
        resource["properties"]["ancestors"][0]["id"]
            .as_str()
            .map(str::to_string)
    }

    /// Loads the directory in the node. `now` is the fixed clock.
    fn load(node: &Node, directory: &Value, counters: &[Value], now: i64) {
        let store = &node.state.store;
        let subjects = directory["subjects"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        // The persons first: each key needs its owner.
        for subject in subjects
            .iter()
            .filter(|s| !s["id"].as_str().unwrap().contains(':'))
        {
            let mut grants = grants_of(subject);
            // The portal lets a person use keys only with this right (portal 5.3b).
            grants.push(Grant::parse("keys.own.use on service:portal/key_kind:mcp").unwrap());
            grants.sort();
            store
                .insert_user(&crate::store::UserRow {
                    login: subject["id"].as_str().unwrap().to_string(),
                    password_hash: "h".into(),
                    grants,
                    created_at: 1,
                })
                .unwrap_or_else(|e| panic!("the person {}: {e:?}", subject["id"]));
        }
        for subject in subjects
            .iter()
            .filter(|s| s["id"].as_str().unwrap().contains(':'))
        {
            let id = subject["id"].as_str().unwrap();
            let local = id.strip_prefix("key:").unwrap();
            let owner = subject["owner"].as_str().unwrap();
            let state = subject["state"].as_str().unwrap_or("active");
            let owner_exists = subjects.iter().any(|s| s["id"] == owner);
            // A revoked key is deleted, and a key whose owner does not exist cannot be stored: the
            // PDP has no such key, so it is an unknown subject (protocol 11.1).
            if state == "revoked" || !owner_exists {
                continue;
            }
            store
                .insert_key(
                    &crate::store::KeyRow {
                        kind: crate::keys::KeyKind::Mcp,
                        id: local.to_string(),
                        owner: Some(owner.to_string()),
                        name: local.to_string(),
                        digest: Sha256::digest(id.as_bytes()).into(),
                        hint: "ab12".into(),
                        created_at: unix_now() - 100,
                        expires_at: (state == "expired").then(|| unix_now() - 50),
                        last_check_at: None,
                        check_count: 0,
                        grants: grants_of(subject),
                    },
                    &key_caps(),
                )
                .unwrap_or_else(|e| panic!("the key {id}: {e:?}"));
        }
        node.state.users.refresh(&node.state.store).unwrap();
        for counter in counters {
            let subject = counter["subject"].as_str().unwrap();
            let window = counter["window"].as_str().unwrap();
            let length = crate::limits::Window::parse(window).unwrap().seconds() as i64;
            let escape = |text: &str| text.replace('\'', "''");
            let owner = match subject.strip_prefix("key:") {
                Some(local) => format!("key_id = '{}'", escape(local)),
                None => format!(
                    "user_id = (SELECT id FROM users WHERE login = '{}')",
                    escape(subject)
                ),
            };
            store.execute_raw(&format!(
                "UPDATE budget_counters SET units = {}, window_start = {}
                  WHERE {owner} AND budget = '{}' AND \"window\" = '{window}'",
                counter["used"].as_i64().unwrap(),
                now - now.rem_euclid(length),
                escape(counter["limit"].as_str().unwrap()),
            ));
        }
    }

    /// Runs one case against a fresh node. Gives the failures, as text.
    fn run_case(backend: &Backend, dir: &std::path::Path, case: &Value) -> Vec<String> {
        let id = case["id"].as_str().unwrap();
        let given = &case["given"];
        let directory = match &given["directory"] {
            Value::String(name) => read_json(&dir.join("directories").join(name)),
            inline => inline.clone(),
        };
        let now = given
            .get("clock")
            .or_else(|| directory.get("clock"))
            .and_then(|c| c["now"].as_i64())
            .unwrap_or(0);
        let services: Vec<Service> = directory["services"]
            .as_array()
            .unwrap()
            .iter()
            .map(|s| Service {
                id: s["id"].as_str().unwrap().to_string(),
                document: s.get("document").cloned(),
                credential: credential_of(
                    s["id"].as_str().unwrap(),
                    s.get("credential_sha256").and_then(Value::as_str),
                    case,
                ),
            })
            .collect();
        let persons = directory["subjects"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|s| !s["id"].as_str().unwrap().contains(':'))
            .count();
        let pdp_port = free_port();
        let mut clients = String::new();
        let mut env = Vec::new();
        for (index, service) in services.iter().enumerate() {
            clients.push_str(&format!(
                "pdp_client \"{}\" {{ token_env = \"CONFORMANCE_TOKEN_{index}\" }}\n",
                service.id
            ));
            env.push((
                format!("CONFORMANCE_TOKEN_{index}"),
                service.credential.clone(),
            ));
        }
        let node = start_custom(
            Arc::new(backend.fresh()),
            None,
            Custom {
                max_users: persons.max(1),
                env,
                ..Custom::default()
            },
            &format!(
                r#"identity {{ issuer = "https://portal.test" signing_key_env = "TEST_IDENTITY_KEY" }}
                pdp {{ listen_address = "127.0.0.1:{pdp_port}" url = "http://pdp.test:{pdp_port}" plain_http = true max_evaluations = 64 max_body_bytes = "1MiB" }}
                keys {{}}
                {clients}"#
            ),
        );
        // The catalogue holds each service, so the registry keeps its document.
        let listed_services: Vec<crate::catalog::Service> = services
            .iter()
            .map(|s| {
                let href = format!("/{}/", s.id);
                let mut labels = vec![
                    ("portal.name", s.id.as_str()),
                    ("portal.id", s.id.as_str()),
                    ("portal.href", href.as_str()),
                ];
                if s.document.is_some() {
                    labels.push(("portal.actions", DB_ACTIONS_URL));
                }
                listed(&labels)
            })
            .collect();
        *node.state.catalog.write().unwrap() =
            Arc::new(Catalog::build(listed_services, Vec::new()));
        let mut failures = Vec::new();
        for service in &services {
            if let Some(document) = &service.document {
                match crate::actions::parse(&service.id, document.to_string().as_bytes()) {
                    Ok(document) => node.state.actions.set_document(&service.id, document),
                    Err(e) => failures.push(format!(
                        "{id}: the reader of the portal refuses the document of {}: {e}",
                        service.id
                    )),
                }
            }
        }
        if now != 0 {
            node.state.counting.set_clock(Some(now));
        }
        let counters: Vec<Value> = given
            .get("counters")
            .and_then(Value::as_array)
            .cloned()
            .or_else(|| directory.get("counters").and_then(Value::as_array).cloned())
            .unwrap_or_default();
        load(&node, &directory, &counters, now);
        wait_until(Duration::from_secs(10), || {
            TcpStream::connect(("127.0.0.1", pdp_port)).is_ok()
        });
        for (number, step) in case["steps"].as_array().unwrap().iter().enumerate() {
            let name = step.get("name").and_then(Value::as_str).unwrap_or("");
            let label = format!("{id} step {} ({name})", number + 1);
            let request = &step["request"];
            let (method, path) = match request.get("endpoint").and_then(Value::as_str) {
                Some("evaluation") => ("POST", "/access/v1/evaluation".to_string()),
                Some("evaluations") => ("POST", "/access/v1/evaluations".to_string()),
                Some("metadata") => ("GET", "/.well-known/authzen-configuration".to_string()),
                _ => (
                    request["method"].as_str().unwrap_or("POST"),
                    request["path"].as_str().unwrap_or("/").to_string(),
                ),
            };
            let body = match (request.get("body"), request.get("body_text")) {
                (Some(body), _) => body.to_string(),
                (None, Some(text)) => text.as_str().unwrap().to_string(),
                _ => String::new(),
            };
            let default_service = request
                .get("body")
                .and_then(service_of)
                .and_then(|id| services.iter().find(|s| s.id == id))
                .or_else(|| services.first());
            let mut headers: Vec<(String, String)> =
                vec![("Content-Type".into(), "application/json".into())];
            let mut given_headers = request
                .get("headers")
                .and_then(Value::as_object)
                .cloned()
                .unwrap_or_default();
            if !given_headers
                .keys()
                .any(|k| k.eq_ignore_ascii_case("authorization"))
                && method == "POST"
            {
                if let Some(service) = default_service {
                    given_headers.insert(
                        "Authorization".into(),
                        Value::String(format!("Bearer {}", service.credential)),
                    );
                }
            }
            for (name, value) in &given_headers {
                headers.push((name.clone(), value.as_str().unwrap_or("").to_string()));
            }
            let header_refs: Vec<(&str, &str)> = headers
                .iter()
                .map(|(k, v)| (k.as_str(), v.as_str()))
                .collect();
            let reply = send(pdp_port, method, &path, &header_refs, &body);
            let expect = &step["expect"];
            if let Some(matcher) = expect.get("status") {
                check(
                    &mut failures,
                    &label,
                    "status",
                    &Value::from(reply.status),
                    matcher,
                );
            }
            if let Some(pointers) = expect.get("json").and_then(Value::as_object) {
                let parsed: Option<Value> = serde_json::from_str(&reply.body).ok();
                for (pointer, matcher) in pointers {
                    let found = parsed.as_ref().and_then(|v| v.pointer(pointer));
                    match found {
                        Some(value) => check(&mut failures, &label, pointer, value, matcher),
                        None => {
                            if matcher.get("absent").is_none() {
                                failures.push(format!(
                                    "{label}: {pointer} is absent; expected {matcher}; body {}",
                                    reply.body
                                ));
                            }
                        }
                    }
                }
            }
            if let Some(matcher) = expect.get("body_text") {
                check(
                    &mut failures,
                    &label,
                    "body_text",
                    &Value::from(reply.body.clone()),
                    matcher,
                );
            }
        }
        failures
    }

    fn check(failures: &mut Vec<String>, label: &str, what: &str, value: &Value, matcher: &Value) {
        if let Some(expected) = matcher.get("equals") {
            if value != expected {
                failures.push(format!("{label}: {what} is {value}, expected {expected}"));
            }
        } else if matcher.get("absent").is_some() {
            failures.push(format!("{label}: {what} is {value}, expected it absent"));
        } else if matcher.get("present").is_some() {
            // The value is there.
        } else {
            failures.push(format!(
                "{label}: the matcher {matcher} is not read by the harness"
            ));
        }
    }

    #[test]
    fn the_cases_of_the_series_d_pass() {
        let Some(dir) = conformance_dir() else {
            eprintln!("PROCURAUTH_CONFORMANCE is not set: the cases of the series D are skipped");
            return;
        };
        let mut paths: Vec<_> = std::fs::read_dir(dir.join("cases/D"))
            .unwrap()
            .map(|e| e.unwrap().path())
            .filter(|p| p.extension().is_some_and(|e| e == "json"))
            .collect();
        paths.sort();
        let only = std::env::var("PROCURAUTH_CASES").ok();
        let mut failures = Vec::new();
        let mut run = 0;
        each_backend(|backend| {
            let _ = backend;
        });
        for backend in crate::store::testing::backends() {
            for path in &paths {
                let case = read_json(path);
                let id = case["id"].as_str().unwrap().to_string();
                if only
                    .as_ref()
                    .is_some_and(|only| !only.split(',').any(|o| o == id))
                {
                    continue;
                }
                run += 1;
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    run_case(&backend, &dir, &case)
                }));
                match result {
                    Ok(found) => failures.extend(
                        found
                            .into_iter()
                            .map(|f| format!("[{}] {f}", backend.name())),
                    ),
                    Err(_) => failures.push(format!(
                        "[{}] {id}: the harness or the portal panicked",
                        backend.name()
                    )),
                }
            }
        }
        eprintln!("{run} cases run, {} failures", failures.len());
        assert!(failures.is_empty(), "\n{}", failures.join("\n"));
    }
}

// The conformance cases of the series E (an edge) -------------------------------------------------

/// The cases of the series E drive an edge: Traefik with its ForwardAuth middleware for the keys,
/// and the portal behind it. The test acts as Traefik: it sends the headers of the request to
/// `/auth/verify-key`, it returns a refusal as it is, and it forwards an accepted request to the
/// service with the header of the identity token from the answer and without `Authorization`
/// (portal 8.1, 8.1a). The test also acts as the service: it records the request that arrives.
mod conformance_e {
    use super::*;
    use serde_json::Value;

    /// A small matcher of regular expressions for the patterns of the cases: anchors, literals,
    /// escapes, classes, groups, `?`, `*`, `+` and `{n,m}`.
    mod regex {
        #[derive(Debug, Clone)]
        enum Node {
            Char(char),
            Any,
            Class(bool, Vec<(char, char)>),
            Start,
            End,
            Group(Vec<Node>),
            Repeat(Box<Node>, usize, usize),
        }

        fn escape_class(c: char) -> Option<(bool, Vec<(char, char)>)> {
            match c {
                's' => Some((
                    false,
                    vec![(' ', ' '), ('\t', '\t'), ('\n', '\n'), ('\r', '\r')],
                )),
                'S' => Some((
                    true,
                    vec![(' ', ' '), ('\t', '\t'), ('\n', '\n'), ('\r', '\r')],
                )),
                'd' => Some((false, vec![('0', '9')])),
                'w' => Some((false, vec![('a', 'z'), ('A', 'Z'), ('0', '9'), ('_', '_')])),
                _ => None,
            }
        }

        fn parse_seq(chars: &[char], i: &mut usize, in_group: bool) -> Vec<Node> {
            let mut out: Vec<Node> = Vec::new();
            while *i < chars.len() {
                let c = chars[*i];
                if c == ')' && in_group {
                    return out;
                }
                *i += 1;
                let node = match c {
                    '^' => Node::Start,
                    '$' => Node::End,
                    '.' => Node::Any,
                    '(' => {
                        let inner = parse_seq(chars, i, true);
                        *i += 1; // the closing parenthesis
                        Node::Group(inner)
                    }
                    '[' => {
                        let negate = chars.get(*i) == Some(&'^');
                        if negate {
                            *i += 1;
                        }
                        let mut items = Vec::new();
                        while chars[*i] != ']' {
                            let mut lo = chars[*i];
                            if lo == '\\' {
                                *i += 1;
                                lo = chars[*i];
                            }
                            if chars.get(*i + 1) == Some(&'-') && chars.get(*i + 2) != Some(&']') {
                                items.push((lo, chars[*i + 2]));
                                *i += 3;
                            } else {
                                items.push((lo, lo));
                                *i += 1;
                            }
                        }
                        *i += 1;
                        Node::Class(negate, items)
                    }
                    '\\' => {
                        let e = chars[*i];
                        *i += 1;
                        match escape_class(e) {
                            Some((negate, items)) => Node::Class(negate, items),
                            None => Node::Char(e),
                        }
                    }
                    other => Node::Char(other),
                };
                // A quantifier applies to the node before it.
                let (min, max) = match chars.get(*i) {
                    Some('?') => (0, 1),
                    Some('*') => (0, usize::MAX),
                    Some('+') => (1, usize::MAX),
                    Some('{') => {
                        let close = chars[*i..].iter().position(|c| *c == '}').unwrap() + *i;
                        let body: String = chars[*i + 1..close].iter().collect();
                        *i = close;
                        match body.split_once(',') {
                            Some((a, b)) => (
                                a.parse().unwrap(),
                                if b.is_empty() {
                                    usize::MAX
                                } else {
                                    b.parse().unwrap()
                                },
                            ),
                            None => (body.parse().unwrap(), body.parse().unwrap()),
                        }
                    }
                    _ => {
                        out.push(node);
                        continue;
                    }
                };
                *i += 1;
                out.push(Node::Repeat(Box::new(node), min, max));
            }
            out
        }

        /// The end positions after the node, from `pos`.
        fn run(node: &Node, text: &[char], pos: usize) -> Vec<usize> {
            match node {
                Node::Char(c) => (text.get(pos) == Some(c))
                    .then_some(pos + 1)
                    .into_iter()
                    .collect(),
                Node::Any => (pos < text.len()).then_some(pos + 1).into_iter().collect(),
                Node::Class(negate, items) => match text.get(pos) {
                    Some(c) => {
                        let inside = items.iter().any(|(lo, hi)| lo <= c && c <= hi);
                        (inside != *negate).then_some(pos + 1).into_iter().collect()
                    }
                    None => Vec::new(),
                },
                Node::Start => (pos == 0).then_some(pos).into_iter().collect(),
                Node::End => (pos == text.len()).then_some(pos).into_iter().collect(),
                Node::Group(nodes) => run_seq(nodes, text, pos),
                Node::Repeat(inner, min, max) => {
                    let mut results = Vec::new();
                    let mut frontier = vec![pos];
                    let mut count = 0;
                    if *min == 0 {
                        results.push(pos);
                    }
                    while count < *max && !frontier.is_empty() && count < text.len() + 2 {
                        let mut next = Vec::new();
                        for p in &frontier {
                            for end in run(inner, text, *p) {
                                if !next.contains(&end) {
                                    next.push(end);
                                }
                            }
                        }
                        count += 1;
                        if count >= *min {
                            for p in &next {
                                if !results.contains(p) {
                                    results.push(*p);
                                }
                            }
                        }
                        frontier = next;
                    }
                    results
                }
            }
        }

        fn run_seq(nodes: &[Node], text: &[char], pos: usize) -> Vec<usize> {
            let mut positions = vec![pos];
            for node in nodes {
                let mut next = Vec::new();
                for p in positions {
                    for end in run(node, text, p) {
                        if !next.contains(&end) {
                            next.push(end);
                        }
                    }
                }
                positions = next;
            }
            positions
        }

        /// Whether the pattern matches somewhere in the text. `^` and `$` anchor it.
        pub fn is_match(pattern: &str, text: &str) -> bool {
            let chars: Vec<char> = pattern.chars().collect();
            let nodes = parse_seq(&chars, &mut 0, false);
            let text: Vec<char> = text.chars().collect();
            (0..=text.len()).any(|start| !run_seq(&nodes, &text, start).is_empty())
        }

        #[test]
        fn the_matcher_reads_the_patterns_of_the_cases() {
            assert!(is_match("^2[0-9]{2}$", "204"));
            assert!(!is_match("^2[0-9]{2}$", "2044"));
            assert!(!is_match("^2[0-9]{2}$", "404"));
            let invalid = r#"^Bearer\s+(realm="[^"]*",\s*)?error="invalid_token"(,\s*error_description="[^"]*")?$"#;
            assert!(is_match(invalid, r#"Bearer error="invalid_token""#));
            assert!(is_match(
                invalid,
                r#"Bearer realm="x", error="invalid_token", error_description="y z""#
            ));
            assert!(!is_match(invalid, "Bearer"));
            assert!(!is_match(invalid, r#"Bearer error="invalid_request""#));
            let none = r#"^Bearer( realm="[^"]*")?$"#;
            assert!(is_match(none, "Bearer"));
            assert!(is_match(none, r#"Bearer realm="r""#));
            assert!(!is_match(none, r#"Bearer error="invalid_token""#));
            let jwt = r"^[A-Za-z0-9_-]{16,}\.[A-Za-z0-9_-]{16,}\.[A-Za-z0-9_-]{16,}$";
            assert!(is_match(
                jwt,
                "aaaaaaaaaaaaaaaaaa.bbbbbbbbbbbbbbbbbb.cccccccccccccccccc"
            ));
            assert!(!is_match(jwt, "forged.forged.forged"));
            assert!(is_match("mcp_", "xxmcp_yy") && !is_match("^mcp_", "xxmcp_"));
        }
    }

    fn conformance_dir() -> Option<std::path::PathBuf> {
        std::env::var_os("PROCURAUTH_CONFORMANCE").map(std::path::PathBuf::from)
    }

    /// What the service behind the edge got.
    #[derive(Default)]
    struct Upstream {
        requests: Vec<Vec<(String, String)>>,
    }

    struct Edge<'a> {
        node: &'a Node,
        upstream: std::sync::Mutex<Upstream>,
        token_header: String,
    }

    struct Answer {
        status: u16,
        headers: Vec<(String, String)>,
        body: String,
    }

    impl Edge<'_> {
        /// One request through the edge: the ForwardAuth call, then the service.
        fn request(
            &self,
            method: &str,
            path: &str,
            headers: &[(String, String)],
            body: &str,
        ) -> Answer {
            let mut auth: Vec<(&str, &str)> = vec![
                ("X-Forwarded-Method", method),
                ("X-Forwarded-Proto", "https"),
                ("X-Forwarded-Host", "portal.test"),
                ("X-Forwarded-For", "203.0.113.9"),
            ];
            auth.push(("X-Forwarded-Uri", path));
            for (name, value) in headers {
                if name.eq_ignore_ascii_case("authorization") {
                    auth.push(("Authorization", value));
                }
            }
            let reply = send(self.node.port, "GET", "/auth/verify-key", &auth, "");
            if !(200..300).contains(&reply.status) {
                // A refusal: Traefik returns the answer of the portal as it is.
                return Answer {
                    status: reply.status,
                    headers: reply.headers.clone(),
                    body: reply.body.clone(),
                };
            }
            let mut forwarded: Vec<(String, String)> = headers
                .iter()
                .filter(|(name, _)| {
                    !name.eq_ignore_ascii_case("authorization")
                        && !name.eq_ignore_ascii_case(&self.token_header)
                })
                .cloned()
                .collect();
            if let Some(token) = reply.header(&self.token_header) {
                forwarded.push((self.token_header.clone(), token.to_string()));
            }
            let _ = body;
            self.upstream.lock().unwrap().requests.push(forwarded);
            Answer {
                status: 200,
                headers: Vec::new(),
                body: String::new(),
            }
        }
    }

    fn replace_variables(text: &str, variables: &[(String, String)]) -> String {
        let mut out = text.to_string();
        for (name, value) in variables {
            out = out.replace(&format!("{{{{{name}}}}}"), value);
        }
        out
    }

    fn variables_in(value: &Value, variables: &[(String, String)]) -> Value {
        match value {
            Value::String(text) => Value::String(replace_variables(text, variables)),
            Value::Array(items) => {
                Value::Array(items.iter().map(|v| variables_in(v, variables)).collect())
            }
            Value::Object(map) => Value::Object(
                map.iter()
                    .map(|(k, v)| (replace_variables(k, variables), variables_in(v, variables)))
                    .collect(),
            ),
            other => other.clone(),
        }
    }

    fn matches(value: Option<&Value>, matcher: &Value) -> Result<(), String> {
        if let Some(expected) = matcher.get("equals") {
            return if value == Some(expected) {
                Ok(())
            } else {
                Err(format!("is {value:?}, expected {expected}"))
            };
        }
        if matcher.get("absent").is_some() {
            return if value.is_none() {
                Ok(())
            } else {
                Err(format!("is {value:?}, expected it absent"))
            };
        }
        if matcher.get("present").is_some() {
            return if value.is_some() {
                Ok(())
            } else {
                Err("is absent, expected it present".into())
            };
        }
        let text = value.map(|v| v.as_str().map_or_else(|| v.to_string(), str::to_string));
        if let Some(pattern) = matcher.get("regex").and_then(Value::as_str) {
            return match &text {
                Some(text) if regex::is_match(pattern, text) => Ok(()),
                other => Err(format!("is {other:?}, expected a match of {pattern}")),
            };
        }
        if let Some(pattern) = matcher.get("not_regex").and_then(Value::as_str) {
            return match &text {
                Some(text) if regex::is_match(pattern, text) => {
                    Err(format!("matches {pattern}: {text}"))
                }
                _ => Ok(()),
            };
        }
        if let Some(options) = matcher.get("one_of").and_then(Value::as_array) {
            return if value.is_some_and(|v| options.contains(v)) {
                Ok(())
            } else {
                Err(format!("is {value:?}, expected one of {options:?}"))
            };
        }
        Err(format!("the matcher {matcher} is not read by the harness"))
    }

    fn decode_part(token: &str, index: usize) -> Value {
        use base64ct::{Base64UrlUnpadded, Encoding};
        let part = token.split('.').nth(index).unwrap_or("");
        Base64UrlUnpadded::decode_vec(part)
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or(Value::Null)
    }

    fn run_case(backend: &Backend, case: &Value) -> Vec<String> {
        let id = case["id"].as_str().unwrap();
        let pdp_port = free_port();
        let node = start_with(
            Arc::new(backend.fresh()),
            None,
            &format!(
                r#"identity {{ issuer = "https://portal.test" signing_key_env = "TEST_IDENTITY_KEY" token_ttl = "5m" }}
                pdp {{ listen_address = "127.0.0.1:{pdp_port}" url = "http://pdp.test:{pdp_port}" plain_http = true }}
                keys {{}}
                pdp_client "probe" {{ token_env = "TEST_PDP_DB" }}"#
            ),
        );
        let catalog = Catalog::build(
            vec![listed(&[
                ("portal.name", "Probe"),
                ("portal.id", "probe"),
                ("portal.href", "/probe/"),
                ("portal.actions", DB_ACTIONS_URL),
            ])],
            Vec::new(),
        );
        *node.state.catalog.write().unwrap() = Arc::new(catalog);
        node.state.actions.set_document(
            "probe",
            crate::actions::parse(
                "probe",
                br#"{"server":"probe","resource_types":[],"actions":[{"name":"probe.read","resource_types":["service"],"description":"Read."}]}"#,
            )
            .unwrap(),
        );
        node.state
            .store
            .insert_user(&crate::store::UserRow {
                login: "ana".into(),
                password_hash: "h".into(),
                grants: vec![
                    Grant::parse("access on service:probe").unwrap(),
                    Grant::parse("keys.own.use on service:portal/key_kind:mcp").unwrap(),
                ],
                created_at: 1,
            })
            .unwrap();
        let make = |key_id: &str, byte: u8, grants: Vec<Grant>| -> String {
            let text = crate::keys::generate(crate::keys::KeyKind::Mcp)
                .unwrap()
                .to_string();
            let _ = byte;
            node.state
                .store
                .insert_key(
                    &crate::store::KeyRow {
                        kind: crate::keys::KeyKind::Mcp,
                        id: key_id.into(),
                        owner: Some("ana".into()),
                        name: key_id.into(),
                        digest: crate::keys::digest(&text),
                        hint: crate::keys::hint(&text),
                        created_at: unix_now() - 10,
                        expires_at: None,
                        last_check_at: None,
                        check_count: 0,
                        grants,
                    },
                    &key_caps(),
                )
                .unwrap();
            text
        };
        let valid = make(
            "9b1c04e2aa71f0c3",
            1,
            vec![Grant::parse("access on service:probe").unwrap()],
        );
        let no_access = make("4d2e8f6a10b37c95", 2, Vec::new());
        node.state.users.refresh(&node.state.store).unwrap();
        let unknown = crate::keys::generate(crate::keys::KeyKind::Mcp)
            .unwrap()
            .to_string();
        let token_header = node.state.config.identity.as_ref().unwrap().header.clone();
        let variables: Vec<(String, String)> = vec![
            ("valid_key_text".into(), valid),
            ("unknown_key_text".into(), unknown),
            ("no_access_key_text".into(), no_access),
            ("valid_key_subject".into(), "key:9b1c04e2aa71f0c3".into()),
            ("valid_key_owner".into(), "ana".into()),
            ("token_header".into(), token_header.clone()),
            ("service".into(), "probe".into()),
            ("mcp_path".into(), "/probe/mcp".into()),
        ];
        let edge = Edge {
            node: &node,
            upstream: std::sync::Mutex::new(Upstream::default()),
            token_header,
        };
        let mut failures = Vec::new();
        for (number, step) in case["steps"].as_array().unwrap().iter().enumerate() {
            let label = format!(
                "{id} step {} ({})",
                number + 1,
                step.get("name").and_then(Value::as_str).unwrap_or("")
            );
            let request = variables_in(&step["request"], &variables);
            let method = request["method"].as_str().unwrap_or("GET").to_string();
            let path = request["path"].as_str().unwrap().to_string();
            let headers: Vec<(String, String)> = request
                .get("headers")
                .and_then(Value::as_object)
                .map(|h| {
                    h.iter()
                        .map(|(k, v)| (k.clone(), v.as_str().unwrap_or("").to_string()))
                        .collect()
                })
                .unwrap_or_default();
            let body = match (request.get("body"), request.get("body_text")) {
                (Some(body), _) => body.to_string(),
                (None, Some(text)) => text.as_str().unwrap_or("").to_string(),
                _ => String::new(),
            };
            let before = edge.upstream.lock().unwrap().requests.len();
            let answer = edge.request(&method, &path, &headers, &body);
            let received: Vec<Vec<(String, String)>> =
                edge.upstream.lock().unwrap().requests[before..].to_vec();
            let expect = variables_in(&step["expect"], &variables);
            let mut record = |what: &str, result: Result<(), String>| {
                if let Err(reason) = result {
                    failures.push(format!("[{}] {label}: {what} {reason}", backend.name()));
                }
            };
            if let Some(matcher) = expect.get("status") {
                record(
                    "status",
                    matches(Some(&Value::from(answer.status)), matcher),
                );
            }
            if let Some(headers) = expect.get("headers").and_then(Value::as_object) {
                for (name, matcher) in headers {
                    let value = answer
                        .headers
                        .iter()
                        .find(|(n, _)| n.eq_ignore_ascii_case(name))
                        .map(|(_, v)| Value::String(v.clone()));
                    record(&format!("header {name}"), matches(value.as_ref(), matcher));
                }
            }
            if let Some(matcher) = expect.get("body_text") {
                record(
                    "body_text",
                    matches(Some(&Value::String(answer.body.clone())), matcher),
                );
            }
            if let Some(upstream) = expect.get("upstream") {
                if let Some(matcher) = upstream.get("count") {
                    record(
                        "upstream count",
                        matches(Some(&Value::from(received.len())), matcher),
                    );
                }
                let first = received.first();
                if let Some(headers) = upstream.get("headers").and_then(Value::as_object) {
                    for (name, matcher) in headers {
                        let value = first.and_then(|h| {
                            h.iter()
                                .find(|(n, _)| n.eq_ignore_ascii_case(name))
                                .map(|(_, v)| Value::String(v.clone()))
                        });
                        record(
                            &format!("upstream header {name}"),
                            matches(value.as_ref(), matcher),
                        );
                    }
                }
                let token = first.and_then(|h| {
                    h.iter()
                        .find(|(n, _)| n.eq_ignore_ascii_case(&edge.token_header))
                        .map(|(_, v)| v.clone())
                });
                for (member, part) in [("token_header", 0), ("token_claims", 1)] {
                    if let Some(pointers) = upstream.get(member).and_then(Value::as_object) {
                        let decoded = token.as_ref().map(|t| decode_part(t, part));
                        for (pointer, matcher) in pointers {
                            let value = decoded.as_ref().and_then(|d| d.pointer(pointer));
                            record(&format!("{member} {pointer}"), matches(value, matcher));
                        }
                    }
                }
            }
        }
        failures
    }

    #[test]
    fn the_cases_of_the_series_e_pass() {
        let Some(dir) = conformance_dir() else {
            eprintln!("PROCURAUTH_CONFORMANCE is not set: the cases of the series E are skipped");
            return;
        };
        let mut paths: Vec<_> = std::fs::read_dir(dir.join("cases/E"))
            .unwrap()
            .map(|e| e.unwrap().path())
            .filter(|p| p.extension().is_some_and(|e| e == "json"))
            .collect();
        paths.sort();
        let mut failures = Vec::new();
        let mut run = 0;
        for backend in crate::store::testing::backends() {
            for path in &paths {
                let case: Value =
                    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
                run += 1;
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    run_case(&backend, &case)
                }));
                match result {
                    Ok(found) => failures.extend(found),
                    Err(_) => failures.push(format!(
                        "[{}] {}: the harness or the portal panicked",
                        backend.name(),
                        case["id"]
                    )),
                }
            }
        }
        eprintln!("{run} cases run, {} failures", failures.len());
        assert!(failures.is_empty(), "\n{}", failures.join("\n"));
    }
}

// The work on a timer ---------------------------------------------------------------------------

#[test]
fn the_rounds_write_the_key_checks_prune_the_old_keys_and_make_the_rows_of_a_new_directory() {
    each_backend(|backend| {
        let (node, _port) = key_node(backend);
        add_alice(
            &node,
            &[Grant::new("access", "db")
                .unwrap()
                .with_limits(limits(r#"{"requests":[{"amount":3,"window":"minute"}]}"#))],
        );
        let text = crate::keys::generate(crate::keys::KeyKind::Mcp)
            .unwrap()
            .to_string();
        let grants = vec![Grant::parse("access on service:db").unwrap()];
        node.state
            .store
            .insert_key(
                &crate::store::KeyRow {
                    kind: crate::keys::KeyKind::Mcp,
                    id: "c0ffee0000000001".into(),
                    owner: Some("alice".into()),
                    name: "checked".into(),
                    digest: crate::keys::digest(&text),
                    hint: crate::keys::hint(&text),
                    created_at: unix_now() - 10,
                    expires_at: None,
                    last_check_at: None,
                    check_count: 0,
                    grants,
                },
                &key_caps(),
            )
            .unwrap();
        node.state.users.refresh(&node.state.store).unwrap();
        let check = || {
            send(
                node.port,
                "GET",
                "/auth/verify-key",
                &[
                    ("Authorization", &format!("Bearer {text}")),
                    ("X-Forwarded-Uri", "/db/x"),
                    ("X-Forwarded-Method", "GET"),
                ],
                "",
            )
        };
        for _ in 0..3 {
            let reply = check();
            assert_eq!(reply.status, 204, "{:?}", reply.headers);
            assert!(reply.header("x-identity-token").is_some());
        }
        // The checks stay in memory until the round of 15 seconds writes them.
        let stored = |node: &Node| {
            node.state
                .store
                .load_directory()
                .unwrap()
                .keys
                .into_iter()
                .find(|k| k.id == "c0ffee0000000001")
        };
        assert_eq!(stored(&node).unwrap().check_count, 0);
        crate::sync::run_round(&node.state, 1);
        assert_eq!(
            stored(&node).unwrap().check_count,
            0,
            "not a round of the checks"
        );
        crate::sync::run_round(&node.state, 3);
        let key = stored(&node).unwrap();
        assert_eq!(key.check_count, 3);
        assert!(key.last_check_at.is_some_and(|at| at >= unix_now() - 5));
        check();
        crate::sync::run_round(&node.state, 6);
        assert_eq!(stored(&node).unwrap().check_count, 4);
        // A key that expired more than 30 days ago is deleted, and a summary row that is old.
        node.state.store.execute_raw(&format!(
            "UPDATE keys SET expires_at = {} WHERE id = 'c0ffee0000000001';
             INSERT INTO key_history (id, owner_id, name, created_at, revoked_at, check_count)
               VALUES ('old', NULL, 'old', 1, 2, 0)",
            unix_now() - 31 * 86_400
        ));
        crate::sync::run_round(&node.state, 60);
        assert!(stored(&node).is_none());
        assert_eq!(node.state.store.count("key_history"), 0);
    });
}

#[test]
fn a_new_version_of_the_directory_makes_the_counter_rows_it_needs_and_clears_the_refusal_memo() {
    each_backend(|backend| {
        let (node, port) = key_node(backend);
        add_alice(&node, &[]);
        let usage = serde_json::json!({"usage": {"units": 1}});
        let ask = |context: &serde_json::Value| {
            pdp(
                port,
                "/access/v1/evaluation",
                Some(PDP_DB_TOKEN),
                &read_body("alice", "test1", context.clone()),
                None,
            )
            .json()
        };
        // Three uses fit, the fourth is refused and enters the memo.
        for _ in 0..3 {
            assert_eq!(ask(&usage)["decision"], true);
        }
        assert_eq!(ask(&usage)["context"]["reason"], "budget_exceeded");
        assert_eq!(node.state.counting.memo_entries(), 1);
        // The superadmin gives Alice a bigger budget. The new version clears the memo.
        let bigger: Vec<Grant> = node
            .state
            .users
            .grants_of("alice")
            .unwrap()
            .into_iter()
            .filter(|g| g.action != "access")
            .chain([Grant::new("access", "db").unwrap().with_limits(limits(
                r#"{"requests":[{"amount":50,"window":"minute"},{"amount":5,"window":"hour"}]}"#,
            ))])
            .collect();
        node.state
            .store
            .update_user("alice", None, Some(&bigger))
            .unwrap();
        crate::sync::run_round(&node.state, 1);
        assert_eq!(node.state.counting.memo_entries(), 0);
        // The rows of the new windows exist: the use is counted, not unavailable.
        let reply = ask(&usage);
        assert_eq!(reply["decision"], true, "{reply}");
        assert_eq!(
            node.state
                .counting
                .stats
                .row_missing
                .load(std::sync::atomic::Ordering::Relaxed),
            0
        );
    });
}

#[test]
fn the_measures_of_the_keys_the_counters_and_the_directory_are_given() {
    let pdp_port = free_port();
    let node = start_with(
        Arc::new(Backend::Sqlite.fresh()),
        None,
        &format!(
            r#"identity {{ issuer = "https://portal.test" signing_key_env = "TEST_IDENTITY_KEY" }}
            pdp {{ listen_address = "127.0.0.1:{pdp_port}" url = "http://pdp.test:{pdp_port}" plain_http = true }}
            keys {{}}
            metrics {{ enabled = true }}
            pdp_client "db" {{ token_env = "TEST_PDP_DB" }}"#
        ),
    );
    publish_catalog(&node, &[]);
    node.state.actions.set_document(
        "db",
        crate::actions::parse("db", KEY_TEST_DOCUMENT.as_bytes()).unwrap(),
    );
    wait_until(Duration::from_secs(10), || {
        TcpStream::connect(("127.0.0.1", pdp_port)).is_ok()
    });
    add_alice(&node, &[]);
    let usage = serde_json::json!({"usage": {"units": 1}});
    for _ in 0..4 {
        pdp(
            pdp_port,
            "/access/v1/evaluation",
            Some(PDP_DB_TOKEN),
            &read_body("alice", "test1", usage.clone()),
            None,
        );
    }
    pdp(
        pdp_port,
        "/access/v1/evaluation",
        Some(PDP_DB_TOKEN),
        &read_body("alice", "test1", serde_json::json!({"usage": {"units": 2}})),
        None,
    );
    let refused = send(
        node.port,
        "GET",
        "/auth/verify-key",
        &[("Authorization", "Bearer mcp_nothing")],
        "",
    );
    assert_eq!(refused.status, 401);
    let missing = send(node.port, "GET", "/auth/verify-key", &[], "");
    assert_eq!(missing.status, 401);
    let body = send(node.port, "GET", "/metrics", &[], "").body;
    let value = |line_start: &str| -> f64 {
        body.lines()
            .find(|l| l.starts_with(line_start))
            .unwrap_or_else(|| panic!("{line_start} is not in the measures:\n{body}"))
            .rsplit(' ')
            .next()
            .unwrap()
            .parse()
            .unwrap()
    };
    assert_eq!(value("portal_budget_units_total{result=\"counted\"}"), 3.0);
    assert_eq!(value("portal_budget_units_total{result=\"refused\"}"), 1.0);
    assert_eq!(
        value("portal_pdp_decisions_total{decision=\"true\",reason=\"none\"}"),
        3.0
    );
    assert_eq!(
        value("portal_pdp_decisions_total{decision=\"false\",reason=\"budget_exceeded\"}"),
        1.0
    );
    assert_eq!(value("portal_usage_rejected_total"), 1.0);
    assert_eq!(
        value("portal_pdp_refused_total{reason=\"bad_request\"}"),
        0.0
    );
    assert_eq!(value("portal_key_checks_total{result=\"bad_format\"}"), 1.0);
    assert_eq!(value("portal_key_checks_total{result=\"no_key\"}"), 1.0);
    assert_eq!(value("portal_key_checks_total{result=\"ok\"}"), 0.0);
    assert_eq!(value("portal_counter_statement_seconds_count"), 4.0);
    assert_eq!(value("portal_counter_rows "), 1.0);
    assert_eq!(value("portal_active_keys "), 0.0);
    assert_eq!(value("portal_directory_age_seconds"), 0.0);
    assert!(value("portal_directory_grants ") >= 4.0);
    assert!(value("portal_actions_age_seconds") < 60.0);
    assert_eq!(value("portal_limit_values_undeclared"), 0.0);
    // No label names a user or a key.
    assert!(!body.contains("alice"), "no measure names a user");
}

// The page "Keys" -------------------------------------------------------------------------------

fn get_keys(node: &Node, session: &str, query: &str) -> Reply {
    let cookie = format!("{}={session}", session_cookie_name(node));
    send(
        node.port,
        "GET",
        &format!("/auth/settings{query}"),
        &[("Cookie", &cookie)],
        "",
    )
}

fn get_new_key(node: &Node, session: &str, query: &str) -> Reply {
    let cookie = format!("{}={session}", session_cookie_name(node));
    send(
        node.port,
        "GET",
        &format!("/auth/keys/new{query}"),
        &[("Cookie", &cookie)],
        "",
    )
}

/// A page of the keys: `new` or `edit?id=<id>`.
fn get_key_page(node: &Node, session: &str, what: &str) -> Reply {
    let cookie = format!("{}={session}", session_cookie_name(node));
    send(
        node.port,
        "GET",
        &format!("/auth/keys/{what}"),
        &[("Cookie", &cookie)],
        "",
    )
}

/// The text of a key in a page: its prefix and 49 characters.
fn key_text_in(body: &str) -> Option<String> {
    let marker = r#"id="key-text" type="text" readonly value=""#;
    let start = body.find(marker)? + marker.len();
    let text: String = body[start..].chars().take(53).collect();
    (text.len() == 53 && crate::keys::check_format(&text).is_ok()).then_some(text)
}

/// A form of the page Keys. The form of a new key carries the kind: the helper gives `mcp` when the
/// fields name none (see `keys_post_raw`).
fn keys_post(node: &Node, session: &str, action: &str, fields: &str) -> Reply {
    let fields = if action == "create" && !fields.contains("kind=") {
        format!("kind=mcp&{fields}")
    } else {
        fields.to_string()
    };
    keys_post_raw(node, session, action, &fields)
}

/// A form of the page Keys, with exactly these fields.
fn keys_post_raw(node: &Node, session: &str, action: &str, fields: &str) -> Reply {
    post(
        node,
        session,
        &format!("/auth/keys/{action}"),
        super::keys_page::KEYS_TOKEN,
        fields,
    )
}

/// A user with the rights to manage keys, and a node with the service `db`.
fn keys_user(backend: &Backend, extra_keys_block: &str) -> (Node, u16, String) {
    let pdp_port = free_port();
    let node = start_with(
        Arc::new(backend.fresh()),
        None,
        &format!(
            r#"identity {{ issuer = "https://portal.test" signing_key_env = "TEST_IDENTITY_KEY" }}
            pdp {{ listen_address = "127.0.0.1:{pdp_port}" url = "http://pdp.test:{pdp_port}" plain_http = true }}
            keys {{ {extra_keys_block} }}
            pdp_client "db" {{ token_env = "TEST_PDP_DB" }}"#
        ),
    );
    publish_catalog(&node, &[]);
    node.state.actions.set_document(
        "db",
        crate::actions::parse("db", KEY_TEST_DOCUMENT.as_bytes()).unwrap(),
    );
    let rights: Vec<Grant> = [
        "keys.own.read on service:portal",
        "keys.own.create on service:portal/key_kind:mcp",
        "keys.own.edit on service:portal",
        "keys.own.revoke on service:portal",
        "keys.own.use on service:portal/key_kind:mcp",
    ]
    .iter()
    .map(|text| Grant::parse(text).unwrap())
    .collect();
    add_alice(&node, &rights);
    let session = sign_in(&node, "alice", PASSWORD);
    (node, pdp_port, session)
}

#[test]
fn a_user_makes_uses_changes_and_revokes_a_key_on_the_page() {
    each_backend(|backend| {
        let (node, _pdp, alice) = keys_user(backend, "");
        let page = get_keys(&node, &alice, "");
        assert_eq!(page.status, 200, "{}", page.body);
        assert!(page.body.contains("New key"), "the link to the new key");
        assert!(page.body.contains(r#"href="/auth/keys/new""#));
        assert!(page.body.contains("No key yet"));
        assert!(
            !page.body.contains(r#"action="/auth/keys/create""#),
            "the pane is closed: it has no form"
        );
        let form = get_new_key(&node, &alice, "");
        assert_eq!(form.status, 200, "{}", form.body);
        assert!(
            form.body.contains(r#""name":"data.read""#)
                && form.body.contains(r#""path":"host:prod""#),
            "the action and the node of her rights are in the editor"
        );
        assert!(
            !form.body.contains("keys.own.read"),
            "no box for an action of the portal"
        );
        assert!(form.body.contains(r#"action="/auth/keys/create""#));
        assert!(
            !form.body.contains("No key yet"),
            "the page of a key holds no list"
        );
        // One entry point: the page of a key has no "New key" link.
        assert!(
            !form.body.contains(r#"href="/auth/keys/new""#) && !form.body.contains("New key</a>"),
            "{}",
            form.body
        );
        assert!(form.body.contains(r#"<button class="button button--primary" type="submit" form="key-form">Create</button>"#));

        // Make a key: the answer shows the text one time.
        let created = keys_post(
            &node,
            &alice,
            "create",
            "name=nightly+report&expires_at=&grant0=access+on+service%3Adb&grant1=data.read+on+service%3Adb%2Fhost%3Aprod&limit1.max_rows=100&limit0.requests.minute=2",
        );
        assert_eq!(created.status, 200, "{}", created.body);
        assert_eq!(created.header("cache-control"), Some("no-store"));
        let text = key_text_in(&created.body).expect("the text of the key");
        assert!(
            created.body.contains("acts as you"),
            "the warning of portal 5.2"
        );
        // The text of the key is in the page, with Copy and Done.
        assert!(created.body.contains(r#"data-copy="key-text""#));
        assert!(created.body.contains(r#"href="/auth/settings">Done</a>"#));
        assert!(
            !created.body.contains("New key</a>") && !created.body.contains(">Create</button>"),
            "one entry point: the page shows the key"
        );
        assert!(
            created.body.contains("nightly report"),
            "the page names the new key"
        );
        // The key works at the edge, with the grants that the form gave.
        let edge = |text: &str| {
            send(
                node.port,
                "GET",
                "/auth/verify-key",
                &[
                    ("Authorization", &format!("Bearer {text}")),
                    ("X-Forwarded-Uri", "/db/x"),
                ],
                "",
            )
        };
        assert_eq!(edge(&text).status, 204);
        // The page lists the key with its hint and without its text.
        let page = get_keys(&node, &alice, "");
        assert!(page.body.contains("nightly report"));
        assert!(
            page.body.contains(&format!("mcp_{}", &text[4..8])),
            "the hint"
        );
        assert!(!page.body.contains(&text), "the text is shown one time");
        assert!(page.body.contains("db · 2 permissions"));
        let key = node
            .state
            .users
            .keys()
            .into_iter()
            .find(|k| k.name == "nightly report")
            .unwrap();
        assert_eq!(key.grants.len(), 2);
        assert_eq!(key.expires_at, None);
        assert_eq!(key.owner, crate::users::KeyOwner::User("alice".into()));
        // The page of a new key copies the name, the ticks and the limit values of a key.
        let copy = get_new_key(&node, &alice, &format!("?copy={}", key.id));
        assert_eq!(copy.status, 200, "{}", copy.body);
        assert!(copy.body.contains(r#"value="nightly report""#));
        assert!(copy.body.contains(
            r#"<input type="hidden" name="limit1.max_rows" value="100" form="key-form">"#
        ));
        assert_eq!(
            copy.body.matches(r#"type="hidden" name="grant"#).count(),
            2,
            "the two grants of the key"
        );
        // The page of the change holds the grants of the key, and no field `expires_at`.
        let change = get_key_page(&node, &alice, &format!("edit?id={}", key.id));
        assert_eq!(change.status, 200, "{}", change.body);
        assert!(change.body.contains(r#"action="/auth/keys/edit""#));
        assert!(change.body.contains(&format!(
            r#"<input type="hidden" name="id" value="{}">"#,
            key.id
        )));
        assert!(change.body.contains(r#"value="nightly report""#));
        assert!(!change.body.contains(r#"name="expires_at""#));
        assert_eq!(
            change.body.matches(r#"type="hidden" name="grant"#).count(),
            2
        );
        let other = get_key_page(&node, &alice, "edit?id=nonexistent");
        assert_eq!(other.location(), Some("/auth/settings?s=unknown_key"));
        assert!(page
            .body
            .contains(&format!("/auth/keys/edit?id={}", key.id)));

        // Refusals: each one is a redirect with a code.
        for (fields, code) in [
            ("name=&grant0=access+on+service%3Adb", "bad_name"),
            ("name=x&expires_at=nonsense", "bad_expiry"),
            ("name=x&grant0=bad+text", "bad_grant"),
            ("name=x&grant0=access+on+service%3Aghost", "bad_grant"),
            (
                "name=x&grant0=data.delete+on+service%3Adb%2Fhost%3Aprod",
                "bad_grant",
            ),
            (
                "name=x&grant0=data.write+on+service%3Adb%2Fhost%3Aprod&grant1=access+on+service%3Adb",
                "not_covered",
            ),
            (
                "name=x&grant0=keys.own.read+on+service%3Aportal",
                "bad_grant",
            ),
            (
                "name=x&grant0=data.read+on+service%3Adb%2Fhost%3Adev&grant1=access+on+service%3Adb",
                "not_covered",
            ),
            (
                "name=x&grant0=data.read+on+service%3Adb%2Fhost%3Aprod&limit0.max_rows=0",
                "bad_limits",
            ),
        ] {
            let reply = keys_post(&node, &alice, "create", fields);
            assert_eq!(reply.status, 303, "{fields}: {}", reply.body);
            assert_eq!(
                reply.location(),
                Some(format!("/auth/keys/new?kind=mcp&s={code}").as_str()),
                "{fields}"
            );
        }
        // A right on a service, and no grant `access` on it: the key would do nothing. The form
        // comes back with the name and the grants, and the message names the
        // service.
        let refused = keys_post(
            &node,
            &alice,
            "create",
            "name=my+agent&grant0=data.read+on+service%3Adb%2Fhost%3Aprod",
        );
        assert_eq!(refused.status, 422, "{}", refused.body);
        assert!(refused.body.contains(r#"value="my agent""#));
        assert!(refused
            .body
            .contains("a permission needs the switch Can open"));
        assert!(refused.body.contains("data.read on service:db/host:prod"));
        // The same for the change of a key.
        let refused = keys_post(
            &node,
            &alice,
            "edit",
            &format!(
                "id={}&name=renamed&grant0=data.read+on+service%3Adb%2Fhost%3Aprod",
                key.id
            ),
        );
        assert_eq!(refused.status, 422, "{}", refused.body);
        assert!(refused.body.contains(r#"value="renamed""#));
        assert!(refused
            .body
            .contains("a permission needs the switch Can open"));
        // A bad request: an unknown field, a bad token, a repeated field.
        assert_eq!(
            keys_post(&node, &alice, "create", "name=x&colour=red").status,
            400
        );
        assert_eq!(
            keys_post(&node, &alice, "create", "name=x&name=y").status,
            400
        );
        let cookie = format!("{}={alice}", session_cookie_name(&node));
        let origin = format!("http://127.0.0.1:{}", node.port);
        let no_token = send(
            node.port,
            "POST",
            "/auth/keys/create",
            &[
                ("Origin", &origin),
                ("Content-Type", "application/x-www-form-urlencoded"),
                ("Cookie", &cookie),
            ],
            "name=x&csrf=wrong",
        );
        assert_eq!(no_token.status, 403);

        // Change the key: a new name, one grant less. The change applies at the edge at once.
        let edited = keys_post(
            &node,
            &alice,
            "edit",
            &format!(
                "id={}&name=renamed&grant0=access+on+service%3Adb&limit0.requests.minute=1",
                key.id
            ),
        );
        assert_eq!(edited.status, 303);
        assert_eq!(edited.location(), Some("/auth/settings?s=edited"));
        let changed = node.state.users.key_by_id(&key.id).unwrap();
        assert_eq!(
            (changed.name.as_str(), changed.grants.len()),
            ("renamed", 1)
        );
        assert_eq!(edge(&text).status, 204);
        // A key of another user, and a key that does not exist, cannot be changed.
        let reply = keys_post(&node, &alice, "edit", "id=nonexistent&name=x");
        assert_eq!(reply.location(), Some("/auth/settings?s=unknown_key"));

        // Revoke it. The edge refuses the text, and the page shows the summary row.
        let revoked = keys_post(
            &node,
            &alice,
            "revoke",
            &format!("id={}&section=mine", key.id),
        );
        assert_eq!(revoked.status, 303);
        assert_eq!(revoked.location(), Some("/auth/settings?s=revoked"));
        assert_eq!(edge(&text).status, 401);
        let page = get_keys(&node, &alice, "");
        assert!(page.body.contains("revoked"));
        // A second revocation gives the same answer and changes nothing.
        let again = keys_post(
            &node,
            &alice,
            "revoke",
            &format!("id={}&section=mine", key.id),
        );
        assert_eq!(again.location(), Some("/auth/settings?s=revoked"));
        let missing = keys_post(&node, &alice, "revoke", "id=nonexistent&section=mine");
        assert_eq!(missing.location(), Some("/auth/settings?s=unknown_key"));

        // The bulk form revokes each ticked key: the fields are `id.<id>`.
        let mut ids = Vec::new();
        for name in ["bulk one", "bulk two"] {
            let made = keys_post(
                &node,
                &alice,
                "create",
                &format!(
                    "name={}&expires_at=&grant0=access+on+service%3Adb",
                    name.replace(' ', "+")
                ),
            );
            assert_eq!(made.status, 200, "{}", made.body);
            let id = node
                .state
                .users
                .keys()
                .into_iter()
                .find(|k| k.name == name)
                .unwrap()
                .id
                .clone();
            ids.push(id);
        }
        let list = get_keys(&node, &alice, "");
        for id in &ids {
            assert!(
                list.body.contains(&format!(r#"name="id.{id}" value="1""#)),
                "{}",
                list.body
            );
        }
        assert!(list.body.contains(r#"data-bulk="revoke-keys""#));
        let bulk = keys_post_raw(
            &node,
            &alice,
            "revoke",
            &format!("id.{}=1&id.{}=1&section=mine", ids[0], ids[1]),
        );
        assert_eq!(bulk.location(), Some("/auth/settings?s=revoked"));
        node.state.users.refresh(&node.state.store).unwrap();
        assert!(ids
            .iter()
            .all(|id| node.state.users.key_by_id(id).is_none()));
        // A bad field, and no id at all, are bad requests.
        assert_eq!(
            keys_post_raw(&node, &alice, "revoke", "section=mine").status,
            400
        );
        assert_eq!(
            keys_post_raw(&node, &alice, "revoke", "id.x=2&section=mine").status,
            400
        );
    });
}

#[test]
fn the_page_of_the_keys_refuses_what_the_rights_and_the_request_do_not_allow() {
    each_backend(|backend| {
        let (node, _pdp, alice) = keys_user(backend, "max_per_user = 1");
        // No session: a redirect to the sign-in page, on each address.
        for (method, path) in [
            ("GET", "/auth/keys/new"),
            ("POST", "/auth/keys/create"),
            ("POST", "/auth/keys/edit"),
            ("POST", "/auth/keys/revoke"),
        ] {
            let origin = format!("http://127.0.0.1:{}", node.port);
            let reply = send(
                node.port,
                method,
                path,
                &[
                    ("Origin", &origin),
                    ("Content-Type", "application/x-www-form-urlencoded"),
                ],
                "",
            );
            assert_eq!(reply.status, 303, "{path}");
            assert_eq!(reply.location(), Some("/auth/login"), "{path}");
        }
        // The list of the keys is a panel of the tab Access: the old address does not exist, and a
        // signed-in account gets 404 with no redirect.
        let old = send(
            node.port,
            "GET",
            "/auth/keys",
            &[("Cookie", &format!("{}={alice}", session_cookie_name(&node)))],
            "",
        );
        assert_eq!(old.status, 404);
        assert!(old.location().is_none());
        // Bad parameters of the page of a new key.
        for query in [
            "?colour=red",
            "?kind=nonsense",
            "?s=unknown",
            "?copy=BAD",
            "?kind=mcp&kind=mcp",
        ] {
            assert_eq!(get_new_key(&node, &alice, query).status, 400, "{query}");
        }
        assert_eq!(get_new_key(&node, &alice, "?s=bad_name").status, 200);
        // The tab Access ignores what it does not know, and a value that names nothing gives an
        // empty list, not an error.
        for query in [
            "?colour=red",
            "?user=nobody",
            "?group=%3Cx%3E&service=%3Cy%3E",
            "?k=other",
        ] {
            assert_eq!(get_keys(&node, &alice, query).status, 200, "{query}");
        }
        // The keys of other accounts need `keys.all.read`: alice sees no column Owner and no chip mine.
        let list = get_keys(&node, &alice, "");
        assert!(!list.body.contains(">Owner</th>") && !list.body.contains("My keys"));
        // The maximum number of active keys.
        let made = keys_post(
            &node,
            &alice,
            "create",
            "name=one&grant0=access+on+service%3Adb",
        );
        assert_eq!(made.status, 200);
        let second = keys_post(
            &node,
            &alice,
            "create",
            "name=two&grant0=access+on+service%3Adb",
        );
        assert_eq!(
            second.location(),
            Some("/auth/keys/new?kind=mcp&s=too_many_keys")
        );
        // A user without the rights.
        node.state
            .store
            .insert_user(&crate::store::UserRow {
                login: "bob".into(),
                password_hash: password_hash(PASSWORD),
                grants: vec![Grant::parse("access on service:db").unwrap()],
                created_at: 1,
            })
            .unwrap();
        node.state.users.refresh(&node.state.store).unwrap();
        let bob = sign_in(&node, "bob", PASSWORD);
        assert_eq!(
            get_keys(&node, &bob, "").status,
            404,
            "no right of the keys: no page"
        );
        assert_eq!(keys_post(&node, &bob, "create", "name=x").status, 403);
        assert_eq!(keys_post(&node, &bob, "edit", "id=x&name=x").status, 403);
        assert_eq!(
            keys_post(&node, &bob, "revoke", "id=x&section=mine").status,
            403
        );
        // Bob cannot change or revoke a key of Alice, with any right.
        node.state
            .store
            .update_user(
                "bob",
                None,
                Some(&[
                    Grant::parse("keys.own.read on service:portal").unwrap(),
                    Grant::parse("keys.own.edit on service:portal").unwrap(),
                    Grant::parse("keys.own.revoke on service:portal").unwrap(),
                ]),
            )
            .unwrap();
        node.state.users.refresh(&node.state.store).unwrap();
        let key_of_alice = node.state.users.keys().into_iter().next().unwrap();
        let edit = keys_post(
            &node,
            &bob,
            "edit",
            &format!("id={}&name=stolen", key_of_alice.id),
        );
        assert_eq!(edit.location(), Some("/auth/settings?s=unknown_key"));
        let revoke = keys_post(
            &node,
            &bob,
            "revoke",
            &format!("id={}&section=mine", key_of_alice.id),
        );
        assert_eq!(revoke.location(), Some("/auth/settings?s=unknown_key"));
        assert!(node.state.users.key_by_id(&key_of_alice.id).is_some());
        // A session that "Log in as" opens changes no key, and revokes none (portal 3.7, 3.7a).
        let root = sign_in(&node, "root", PASSWORD);
        let reply = post(&node, &root, IMPERSONATE_PATH, USERS_TOKEN, "login=alice");
        assert_eq!(reply.status, 303);
        let as_alice = reply.cookie(&session_cookie_name(&node)).unwrap();
        assert_eq!(get_keys(&node, &as_alice, "").status, 200, "it can read");
        assert_eq!(keys_post(&node, &as_alice, "create", "name=x").status, 403);
        assert_eq!(
            keys_post(&node, &as_alice, "edit", "id=x&name=x").status,
            403
        );
        assert_eq!(
            keys_post(
                &node,
                &as_alice,
                "revoke",
                &format!("id={}&section=mine", key_of_alice.id)
            )
            .status,
            403
        );
    });
}

#[test]
fn the_section_all_shows_the_keys_of_the_users_and_revokes_any_key() {
    each_backend(|backend| {
        let (node, _pdp, alice) = keys_user(backend, "");
        let made = keys_post(
            &node,
            &alice,
            "create",
            "name=alices+key&grant0=access+on+service%3Adb",
        );
        assert_eq!(made.status, 200);
        let key = node.state.users.keys().into_iter().next().unwrap();
        let root = sign_in(&node, "root", PASSWORD);
        // The superadmin makes a key: it has no owner row.
        let root_key = keys_post(
            &node,
            &root,
            "create",
            "name=root+key&grant0=access+on+service%3Adb",
        );
        assert_eq!(root_key.status, 200, "{}", root_key.body);
        assert!(
            root_key.body.contains("every right"),
            "the warning of the superadmin (portal 5.2)"
        );
        let all = get_keys(&node, &root, "");
        assert_eq!(all.status, 200);
        assert!(all.body.contains("alices key") && all.body.contains("root key"));
        assert!(all.body.contains(">superadmin<"), "the owner of its key");
        assert!(all.body.contains(">Owner</th>"));
        // The chip "mine" keeps the keys of the viewer. The filter by user keeps the keys of an account.
        let mine = get_keys(&node, &root, "?mine=1");
        assert!(mine.body.contains("root key") && !mine.body.contains("alices key"));
        assert!(all.body.contains(r#"href="/auth/settings?mine=1""#));
        let alice_keys = get_keys(&node, &root, "?user=alice");
        assert!(alice_keys.body.contains("alices key") && !alice_keys.body.contains("root key"));
        // The superadmin revokes the key of Alice in the section "All".
        let revoked = keys_post(
            &node,
            &root,
            "revoke",
            &format!("id={}&section=all", key.id),
        );
        assert_eq!(revoked.location(), Some("/auth/settings?s=revoked"));
        assert!(node.state.users.key_by_id(&key.id).is_none());
        // An address that does not exist answers 404.
        let other = keys_post(&node, &root, "make_it_fail", "");
        assert_eq!(other.status, 404);
    });
}

#[test]
fn the_page_of_the_keys_does_not_exist_without_the_block_keys() {
    let node = start_with(Arc::new(Backend::Sqlite.fresh()), None, "");
    let root = sign_in(&node, "root", PASSWORD);
    assert_eq!(get_new_key(&node, &root, "").status, 404);
    // Without the block keys the tab Access has the accounts alone.
    let access = get_keys(&node, &root, "");
    assert_eq!(access.status, 200);
    assert!(!access.body.contains("keys-title") && !access.body.contains("/auth/keys/new"));
    assert_eq!(keys_post(&node, &root, "create", "name=x").status, 404);
    assert_eq!(keys_post(&node, &root, "edit", "id=x").status, 404);
    assert_eq!(
        keys_post(&node, &root, "revoke", "id=x&section=mine").status,
        404
    );
}

// The accounts and the keys of the file ------------------------------------------------------------

#[test]
fn an_account_of_the_file_signs_in_reads_its_keys_and_changes_none() {
    let pdp_port = free_port();
    let key_text = crate::keys::generate(crate::keys::KeyKind::Mcp)
        .unwrap()
        .to_string();
    let node = start_custom(
        Arc::new(Backend::Sqlite.fresh()),
        None,
        Custom {
            env: vec![
                ("CAROL_HASH".into(), password_hash(PASSWORD)),
                ("CAROL_KEY".into(), key_text.clone()),
            ],
            ..Custom::default()
        },
        &format!(
            r#"access {{ grants_mode = "hybrid" }}
            identity {{ issuer = "https://portal.test" signing_key_env = "TEST_IDENTITY_KEY" }}
            pdp {{ listen_address = "127.0.0.1:{pdp_port}" url = "http://pdp.test:{pdp_port}" plain_http = true }}
            keys {{}}
            pdp_client "db" {{ token_env = "TEST_PDP_DB" }}
            user "carol" {{
              password_hash_env = "CAROL_HASH"
              grants = [
                {{ grant = "access on service:db", limits = {{ requests = [ {{ amount = 300, window = "minute" }} ] }} }},
                "data.read on service:db/host:prod",
                "keys.own.read on service:portal",
                "keys.own.revoke on service:portal",
                "keys.own.use on service:portal/key_kind:mcp",
              ]
              key "nightly report" {{
                key_env = "CAROL_KEY"
                grants = [
                  {{ grant = "access on service:db", limits = {{ requests = [ {{ amount = 60, window = "minute" }} ] }} }},
                  "data.read on service:db/host:prod/database:test1",
                ]
              }}
            }}
            user "dave" {{ grants = ["access on service:db"] }}"#
        ),
    );
    publish_catalog(&node, &[]);
    node.state.actions.set_document(
        "db",
        crate::actions::parse("db", KEY_TEST_DOCUMENT.as_bytes()).unwrap(),
    );
    let id = crate::keys::file_id("carol", "nightly report");
    assert_eq!(node.state.users.key_count(), 1);
    // The account signs in with the digest of its block. An account without a digest cannot.
    let carol = sign_in(&node, "carol", PASSWORD);
    let csrf = "A".repeat(32);
    let signature = sign_return_uri(&node.state.signing_secret, "/").unwrap();
    let origin = format!("http://127.0.0.1:{}", node.port);
    let cookie = format!("{}={csrf}", transport::csrf_cookie_name(false));
    let dave = send(
        node.port,
        "POST",
        LOGIN_PATH,
        &[
            ("Origin", &origin),
            ("Content-Type", "application/x-www-form-urlencoded"),
            ("Cookie", &cookie),
        ],
        &format!("login=dave&password={PASSWORD}&csrf={csrf}&rd=%2F&sig={signature}"),
    );
    assert_eq!(dave.status, 401, "no digest: no session");
    // The key of the file works at the edge, and the page shows it as read-only.
    let edge = send(
        node.port,
        "GET",
        "/auth/verify-key",
        &[
            ("Authorization", &format!("Bearer {key_text}")),
            ("X-Forwarded-Uri", "/db/x"),
        ],
        "",
    );
    assert_eq!(edge.status, 204, "{:?}", edge.headers);
    let page = get_keys(&node, &carol, "");
    assert_eq!(page.status, 200, "{}", page.body);
    assert!(page.body.contains("nightly report") && page.body.contains(&id));
    assert!(page.body.contains("Defined in the configuration file"));
    assert!(
        !page.body.contains("/auth/keys/edit?id="),
        "no change of a key of the file"
    );
    assert!(
        !page.body.contains("/auth/keys/revoke"),
        "no button to revoke a key of the file"
    );
    assert!(
        !page.body.contains("New key"),
        "an account of the file makes no key"
    );
    assert!(
        page.body.contains("1 checks"),
        "the checks of the key of the file are in memory"
    );
    // No address changes a key of the file.
    let revoke = keys_post(&node, &carol, "revoke", &format!("id={id}&section=mine"));
    assert_eq!(revoke.location(), Some("/auth/settings?s=file_key"));
    assert_eq!(keys_post(&node, &carol, "create", "name=x").status, 403);
    assert_eq!(node.state.users.key_count(), 1);
    // The settings page refuses a login that the file holds.
    let root = sign_in(&node, "root", PASSWORD);
    let added = post(
        &node,
        &root,
        USERS_ADD_PATH,
        USERS_TOKEN,
        "login=carol&password=a-long-password-1",
    );
    assert_eq!(added.status, 303);
    assert!(
        added.location().unwrap().contains("exists"),
        "{:?}",
        added.location()
    );
    // The Users page shows the accounts of the file, read-only.
    let users = send(
        node.port,
        "GET",
        "/auth/settings",
        &[("Cookie", &format!("{}={root}", session_cookie_name(&node)))],
        "",
    );
    assert_eq!(users.status, 200);
    assert!(
        users.body.contains(
            r#"href="/auth/settings/users/carol">carol</a> <span class="badge">file</span>"#
        ) && users.body.contains(
            r#"href="/auth/settings/users/dave">dave</a> <span class="badge">file</span>"#
        ),
        "the accounts of the file stand in the table with their badge"
    );
    // The page of an account of the file shows its grants and changes nothing.
    let page_of = send(
        node.port,
        "GET",
        "/auth/settings/users/carol",
        &[("Cookie", &format!("{}={root}", session_cookie_name(&node)))],
        "",
    );
    assert_eq!(page_of.status, 200);
    assert!(
        !page_of.body.contains("/auth/users/update")
            && !page_of.body.contains("/auth/users/delete"),
        "{}",
        page_of.body
    );
    // The PDP knows the key of the file and its owner.
    let reply = pdp(
        pdp_port,
        "/access/v1/evaluation",
        Some(PDP_DB_TOKEN),
        &read_body(
            &format!("key:{id}"),
            "test1",
            serde_json::json!({"usage": {"units": 1}}),
        ),
        None,
    );
    assert_eq!(reply.status, 200, "{}", reply.body);
    assert_eq!(
        reply.json(),
        serde_json::json!({"decision": true, "context": {"usage": {"units": 1}}}),
        "the key of the file reads test1: the grant of the key covers it, and the owner holds host:prod"
    );
}

#[test]
fn the_start_refuses_a_login_a_key_id_or_a_digest_that_the_file_and_the_database_both_hold() {
    let key_text = crate::keys::generate(crate::keys::KeyKind::Mcp)
        .unwrap()
        .to_string();
    let vars = |extra: &[(&str, &str)]| {
        let mut all: Vec<(String, String)> = [
            ("PORTAL_TEST_LOGIN", "root"),
            ("PORTAL_TEST_PASSWORD", PASSWORD),
            ("PORTAL_TEST_SESSION", "0123456789abcdef0123456789abcdef"),
            ("CAROL_KEY", key_text.as_str()),
        ]
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
        all.extend(extra.iter().map(|(k, v)| (k.to_string(), v.to_string())));
        all
    };
    let identity = crate::identity::testing::new_key_pem();
    let text = format!(
        r#"server {{ listen_address = "127.0.0.1:{}" trusted_proxies = [] }}
        authentication {{ superadmin_login_env = "PORTAL_TEST_LOGIN" superadmin_password_env = "PORTAL_TEST_PASSWORD" }}
        session {{ secret_mode = "provided" secret_env = "PORTAL_TEST_SESSION" }}
        access {{ grants_mode = "hybrid" }}
        identity {{ issuer = "i" signing_key_env = "IDENTITY" }}
        pdp {{ listen_address = "127.0.0.1:{}" url = "https://pdp" }}
        keys {{}}
        user "carol" {{ grants = [] key "k" {{ key_env = "CAROL_KEY" grants = [] }} }}"#,
        free_port(),
        free_port()
    );
    let mut all = vars(&[("IDENTITY", &identity)]);
    all.retain(|(k, _)| !k.is_empty());
    let (config, _) = Config::parse(&text, &move |name| {
        all.iter().find(|(k, _)| k == name).map(|(_, v)| v.clone())
    })
    .unwrap();
    let store = Backend::Sqlite.fresh();
    let id = crate::keys::file_id("carol", "k");
    // Nothing in common: good.
    assert!(crate::bootstrap::check_file_against_database(
        &config,
        &store.load_directory().unwrap()
    )
    .is_ok());
    // The same login.
    store
        .insert_user(&crate::store::UserRow {
            login: "carol".into(),
            password_hash: "h".into(),
            grants: Vec::new(),
            created_at: 1,
        })
        .unwrap();
    let error =
        crate::bootstrap::check_file_against_database(&config, &store.load_directory().unwrap())
            .unwrap_err();
    assert!(
        error.contains("carol") && error.contains("file") && error.contains("database"),
        "{error}"
    );
    store.remove_user("carol").unwrap();
    // A user of the database with more grants than `users.max_grants`.
    let grants: Vec<Grant> = (0..=config.users.max_grants)
        .map(|n| Grant::parse(&format!("access on service:s{n}")).unwrap())
        .collect();
    store
        .insert_user(&crate::store::UserRow {
            login: "heavy".into(),
            password_hash: "h".into(),
            grants,
            created_at: 1,
        })
        .unwrap();
    let error =
        crate::bootstrap::check_user_grants(&config, &store.load_directory().unwrap()).unwrap_err();
    assert!(
        error.contains("heavy") && error.contains("users.max_grants"),
        "{error}"
    );
    store.remove_user("heavy").unwrap();
    // The same id of a key.
    store
        .insert_user(&crate::store::UserRow {
            login: "erin".into(),
            password_hash: "h".into(),
            grants: Vec::new(),
            created_at: 1,
        })
        .unwrap();
    let mut row = crate::store::KeyRow {
        kind: crate::keys::KeyKind::Mcp,
        id: id.clone(),
        owner: Some("erin".into()),
        name: "other".into(),
        digest: [5; 32],
        hint: String::new(),
        created_at: 1,
        expires_at: None,
        last_check_at: None,
        check_count: 0,
        grants: Vec::new(),
    };
    store.insert_key(&row, &key_caps()).unwrap();
    let error =
        crate::bootstrap::check_file_against_database(&config, &store.load_directory().unwrap())
            .unwrap_err();
    assert!(error.contains(&id) && error.contains("carol"), "{error}");
    store.revoke_key(&id, None, 1).unwrap();
    // The same digest.
    row.id = "e000000000000001".into();
    row.digest = crate::keys::digest(&key_text);
    store.insert_key(&row, &key_caps()).unwrap();
    let error =
        crate::bootstrap::check_file_against_database(&config, &store.load_directory().unwrap())
            .unwrap_err();
    assert!(
        error.contains("digest") && !error.contains(&key_text),
        "{error}"
    );
}

// The rights on the keys and the limit values, in the pages of the superadmin -----------------------

/// The JSON block of the editor in a page.
fn editor_json(body: &str) -> serde_json::Value {
    let start = body
        .find(r#"class="grantEditor__data">"#)
        .expect("the page has an editor")
        + r#"class="grantEditor__data">"#.len();
    let end = start + body[start..].find("</script>").unwrap();
    serde_json::from_str(&body[start..end]).unwrap()
}

#[test]
fn the_superadmin_gives_the_rights_on_keys_and_the_limit_values_of_a_grant() {
    each_backend(|backend| {
        let (node, _pdp) = key_node(backend);
        let root = sign_in(&node, "root", PASSWORD);
        let added = post(
            &node,
            &root,
            USERS_ADD_PATH,
            USERS_TOKEN,
            "login=alice&password=a-long-password-1",
        );
        assert_eq!(added.status, 303);
        let page = send(
            node.port,
            "GET",
            "/auth/settings/users/alice",
            &[("Cookie", &format!("{}={root}", session_cookie_name(&node)))],
            "",
        );
        assert_eq!(page.status, 200);
        // The editor has two parts: the services, and the portal features with the keys. There
        // is no pseudo service and no separate form for the rights on the keys.
        let editor = editor_json(&page.body);
        assert!(editor["services"].is_array() && editor["features"].is_object());
        let features = &editor["features"];
        assert_eq!(features["id"], "portal");
        let names: Vec<&str> = features["actions"]
            .as_array()
            .unwrap()
            .iter()
            .map(|a| a["name"].as_str().unwrap())
            .collect();
        for action in [
            "keys.own.read",
            "keys.own.create",
            "keys.own.use",
            "keys.all.revoke",
            "users.read",
        ] {
            assert!(names.contains(&action), "{action}");
        }
        assert!(
            !names.contains(&"alert.send") && !names.contains(&"inbox.receive"),
            "the inbox is off: its actions are not in the editor"
        );
        let nodes: Vec<&str> = features["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .map(|n| n["path"].as_str().unwrap())
            .collect();
        assert_eq!(nodes, ["", "key_kind:mcp"], "the kind api is off");
        assert!(editor["services"]
            .as_array()
            .unwrap()
            .iter()
            .all(|s| s["id"] != "portal"));
        assert!(!page.body.contains("name=\"p.keys.own.read\""));
        assert!(!page.body.contains("/auth/users/portal-rights"));
        let gone = post(
            &node,
            &root,
            "/auth/users/portal-rights",
            USERS_TOKEN,
            "login=alice&p.keys.own.read=1",
        );
        assert_eq!(
            gone.status, 404,
            "the form of the rights on the keys is gone"
        );
        let read = "grant0=keys.own.read+on+service%3Aportal";
        let create = "grant1=keys.own.create+on+service%3Aportal%2Fkey_kind%3Amcp";
        let usage = "grant2=keys.own.use+on+service%3Aportal%2Fkey_kind%3Amcp";
        let rights = |fields: &str| {
            post(
                &node,
                &root,
                USERS_UPDATE_PATH,
                USERS_TOKEN,
                &format!("login=alice&grants=1&{fields}"),
            )
        };
        let refused = rights(create);
        assert!(
            refused.location().unwrap().ends_with("?s=keys-needs"),
            "{:?}",
            refused.location()
        );
        assert!(rights("grant0=keys.own.nothing+on+service%3Aportal")
            .location()
            .unwrap()
            .ends_with("edit-unknown-grant"));
        // A service-level grant of an action of a node, a node that does not exist, a kind that is off.
        for bad in [
            "grant0=keys.own.use+on+service%3Aportal",
            "grant0=keys.own.use+on+service%3Aportal%2Fkey_kind%3Aapi",
            "grant0=keys.own.use+on+service%3Aportal%2Fkey_kind%3Awhk",
        ] {
            assert!(
                rights(bad)
                    .location()
                    .unwrap()
                    .ends_with("edit-unknown-grant"),
                "{bad}"
            );
        }
        for bad in ["abc", "31536001"] {
            assert!(
                rights(&format!(
                    "{read}&{create}&{usage}&limit2.max_key_lifetime={bad}"
                ))
                .location()
                .unwrap()
                .ends_with("bad-limits"),
                "{bad}"
            );
        }
        let ok = rights(&format!(
            "{read}&{create}&{usage}&limit2.max_key_lifetime=2592000"
        ));
        assert!(
            ok.location().unwrap().ends_with("?s=user-updated"),
            "{:?}",
            ok.location()
        );
        let alice = node.state.users.grants_of("alice").unwrap();
        assert_eq!(alice.len(), 3);
        let used = alice.iter().find(|g| g.action == "keys.own.use").unwrap();
        assert_eq!(used.limits.to_json(), r#"{"max_key_lifetime":2592000}"#);
        assert_eq!(used.path_text(), "key_kind:mcp");
        // The same rights again: nothing changes. Remove one that another one needs: refused.
        assert!(rights(&format!(
            "{read}&{create}&{usage}&limit2.max_key_lifetime=2592000"
        ))
        .location()
        .unwrap()
        .ends_with("unchanged"));
        assert!(rights(&format!(
            "grant0=keys.own.create+on+service%3Aportal%2Fkey_kind%3Amcp&{usage}"
        ))
        .location()
        .unwrap()
        .ends_with("keys-needs"));
        // Alice reads her page of the keys now, and the new key needs an expiry date.
        let alice_session = sign_in(&node, "alice", "a-long-password-1");
        assert_eq!(get_keys(&node, &alice_session, "").status, 200);
        let no_date = keys_post(&node, &alice_session, "create", "name=n");
        assert!(
            no_date.location().unwrap().ends_with("bad_expiry"),
            "the maximum lifetime needs a date"
        );
        let soon = crate::util::format_utc_instant(unix_now() + 86_400);
        let made = keys_post(
            &node,
            &alice_session,
            "create",
            &format!("name=n&expires_at={soon}"),
        );
        assert_eq!(made.status, 200, "{}", made.body);
        let too_far = crate::util::format_utc_instant(unix_now() + 40 * 86_400);
        let refused = keys_post(
            &node,
            &alice_session,
            "create",
            &format!("name=m&expires_at={too_far}"),
        );
        assert!(refused.location().unwrap().ends_with("bad_expiry"));

        // The limit values of a grant: give `access` on db, then a budget. The page of the account
        // sends the grants and their limit fields in one form.
        let access_text = "grant0=access+on+service%3Adb";
        let read_text = "grant1=data.read+on+service%3Adb%2Fhost%3Aprod";
        let save = |fields: &str| {
            post(
                &node,
                &root,
                USERS_UPDATE_PATH,
                USERS_TOKEN,
                &format!("login=alice&grants=1&{fields}"),
            )
        };
        let give = save(&format!("{access_text}&{read_text}"));
        assert_eq!(give.status, 303, "{:?}", give.location());
        // The action lists the types host and database, not the whole service.
        assert!(
            save(&format!("{access_text}&grant1=data.read+on+service%3Adb"))
                .location()
                .unwrap()
                .ends_with("edit-unknown-grant")
        );
        let saved = save(&format!(
            "{access_text}&limit0.requests.minute=5&limit0.requests.second=1&limit0.requests.hour=&{read_text}"
        ));
        assert!(
            saved.location().unwrap().ends_with("?s=user-updated"),
            "{:?}",
            saved.location()
        );
        let access = node
            .state
            .users
            .grants_of("alice")
            .unwrap()
            .into_iter()
            .find(|g| g.action == "access" && g.service == "db")
            .unwrap();
        assert_eq!(
            access.limits.to_json(),
            r#"{"requests":[{"amount":1,"window":"second"},{"amount":5,"window":"minute"}]}"#
        );
        // Bad values and bad names.
        for fields in [
            "limit0.requests.minute=0",
            "limit0.unknown=1",
            "limit0.max_rows=5",
            "limit0.requests=5",
            "limit0.requests.minute=x",
        ] {
            let reply = save(&format!("{access_text}&{fields}&{read_text}"));
            assert!(
                reply.location().unwrap().ends_with("bad-limits"),
                "{fields}: {:?}",
                reply.location()
            );
        }
        assert_eq!(save(&format!("{access_text}&colour=red")).status, 400);
        assert_eq!(
            save(&format!("{access_text}&limit0.requests.week=1")).status,
            400
        );
        // A ceiling of an action.
        let ceiling = save(&format!(
            "{access_text}&limit0.requests.minute=5&limit0.requests.second=1&{read_text}&limit1.max_rows=100&limit1.max_duration="
        ));
        assert!(ceiling.location().unwrap().ends_with("user-updated"));
        let held = node.state.users.grants_of("alice").unwrap();
        assert!(held
            .iter()
            .any(|g| g.action == "access" && !g.limits.is_empty()));
        assert!(held
            .iter()
            .any(|g| g.action == "data.read" && g.limits.to_json() == r#"{"max_rows":100}"#));
        // A save replaces every grant, the portal features included: the editor carries them.
        assert!(!held.iter().any(|g| g.action == "keys.own.use"));
        // The values reach the PDP at the next round of the directory.
        crate::sync::run_round(&node.state, 1);
        assert!(
            node.state.store.counter_rows().unwrap() >= 2,
            "the rows of the budget exist"
        );
        // Empty fields remove the values.
        let cleared = save(&format!("{access_text}&{read_text}&limit1.max_rows=100"));
        assert!(cleared.location().unwrap().ends_with("user-updated"));
        let held = node.state.users.grants_of("alice").unwrap();
        assert!(held
            .iter()
            .find(|g| g.action == "access" && g.service == "db")
            .unwrap()
            .limits
            .is_empty());
    });
}

// The authorizer that the portal gives to the inbox (`inbox_rights.rs`). The inbox does not run in
// these tests: the authorizer reads the state of the portal only.

mod inbox_rights_tests {
    use super::*;
    use crate::inbox::rights::{Authorizer, Decision, KeyCheck, Subscribable};
    use crate::keyauth::Cause;
    use crate::store::KeyRow;

    pub(super) fn inbox_node(backend: &Backend) -> (Node, std::sync::Arc<dyn Authorizer>) {
        let pdp_port = free_port();
        let server_key = inbox_core::server_key::ServerKey::generate()
            .unwrap()
            .encode()
            .to_string();
        let node = start_custom(
            Arc::new(backend.fresh()),
            None,
            Custom {
                env: vec![("INBOX_TEST_KEY".into(), server_key)],
                ..Custom::default()
            },
            &format!(
                r#"identity {{ issuer = "https://portal.test" signing_key_env = "TEST_IDENTITY_KEY" }}
                pdp {{ listen_address = "127.0.0.1:{pdp_port}" url = "http://pdp.test:{pdp_port}" plain_http = true }}
                keys {{ api = true }}
                inbox {{
                  server_key_env = "INBOX_TEST_KEY"
                  devices {{
                    public_address = "127.0.0.1:4181"
                    heartbeat_interval = "20s"
                    session_timeout = "60s"
                    resend_after = "15s"
                    max_inflight = 32
                    unreachable_after = "2m"
                    offline_warning_after = "3m"
                  }}
                  storage {{
                    delivery_ttl = "7d"
                    alert_retention = "30d"
                    connection_log_retention = "90d"
                  }}
                  group "infra" {{ name = "Infra" }}
                  group "oncall" {{ name = "On call" critical = true }}
                }}"#
            ),
        );
        let authorizer: std::sync::Arc<dyn Authorizer> = std::sync::Arc::new(
            super::super::inbox_rights::PortalAuthorizer::new(&node.state),
        );
        (node, authorizer)
    }

    pub(super) fn add_account(node: &Node, login: &str, grants: &[&str]) {
        let mut grants: Vec<Grant> = grants.iter().map(|g| Grant::parse(g).unwrap()).collect();
        grants.sort();
        node.state
            .store
            .insert_user(&crate::store::UserRow {
                login: login.into(),
                password_hash: password_hash(PASSWORD),
                grants,
                created_at: 1,
            })
            .unwrap();
        node.state.users.refresh(&node.state.store).unwrap();
    }

    /// A key with a real text. The result is the text and the id.
    pub(super) fn add_real_key(
        node: &Node,
        kind: crate::keys::KeyKind,
        owner: Option<&str>,
        grants: &[&str],
    ) -> (String, String) {
        let text = crate::keys::generate(kind).unwrap().to_string();
        let digest = crate::keys::digest(&text);
        let id: String = digest.iter().take(8).map(|b| format!("{b:02x}")).collect();
        let mut grants: Vec<Grant> = grants.iter().map(|g| Grant::parse(g).unwrap()).collect();
        grants.sort();
        node.state
            .store
            .insert_key(
                &KeyRow {
                    kind,
                    id: id.clone(),
                    owner: owner.map(Into::into),
                    name: format!("key {id}"),
                    digest,
                    hint: "ab12".into(),
                    created_at: unix_now() - 10,
                    expires_at: None,
                    last_check_at: None,
                    check_count: 0,
                    grants,
                },
                &key_caps(),
            )
            .unwrap();
        node.state.users.refresh(&node.state.store).unwrap();
        (text, id)
    }

    fn groups(ids: &[&str]) -> Subscribable {
        Subscribable::Groups(ids.iter().map(|g| g.to_string()).collect())
    }

    #[test]
    fn the_rights_to_subscribe_come_from_the_directory_and_say_unknown_when_it_cannot_tell() {
        each_backend(|backend| {
            let (node, authorizer) = inbox_node(backend);
            add_account(
                &node,
                "alice",
                &["inbox.receive on service:portal/inbox_group:infra"],
            );
            add_account(&node, "bob", &[]);
            add_account(
                &node,
                "carol",
                &[
                    "inbox.receive on service:portal/inbox_group:infra",
                    "inbox.receive on service:portal/inbox_group:oncall",
                    "alert.send on service:portal/inbox_group:oncall",
                ],
            );
            assert_eq!(authorizer.subscribable("alice"), groups(&["infra"]));
            assert_eq!(authorizer.subscribable("bob"), groups(&[]), "a positive no");
            assert_eq!(
                authorizer.subscribable("carol"),
                groups(&["infra", "oncall"])
            );
            // The superadmin has every permission.
            assert_eq!(
                authorizer.subscribable("root"),
                groups(&["infra", "oncall"])
            );
            // An account that the directory does not know is not a refusal.
            assert_eq!(authorizer.subscribable("nobody"), Subscribable::Unknown);
            // The stamp changes with the directory.
            let before = authorizer.directory_stamp().unwrap();
            node.state.users.update(
                "alice",
                None,
                Some(vec![Grant::parse(
                    "inbox.receive on service:portal/inbox_group:oncall",
                )
                .unwrap()]),
            );
            assert_ne!(authorizer.directory_stamp().unwrap(), before);
            assert_eq!(authorizer.subscribable("alice"), groups(&["oncall"]));
            // A directory that is old says nothing: no answer, no stamp, no removal.
            if node.state.store.is_shared() {
                node.state.users.set_last_success(0);
                assert_eq!(authorizer.subscribable("bob"), Subscribable::Unknown);
                assert_eq!(authorizer.subscribable("root"), Subscribable::Unknown);
                assert_eq!(authorizer.directory_stamp(), None);
                node.state.users.refresh(&node.state.store).unwrap();
                assert_eq!(authorizer.subscribable("bob"), groups(&[]));
                assert!(authorizer.directory_stamp().is_some());
            }
        });
    }

    #[test]
    fn a_key_is_checked_in_memory_and_a_text_that_is_not_a_key_is_not_a_key() {
        each_backend(|backend| {
            let (node, authorizer) = inbox_node(backend);
            add_account(
                &node,
                "alice",
                &[
                    "keys.own.use on service:portal/key_kind:api",
                    "alert.send on service:portal/inbox_group:infra",
                ],
            );
            let (text, id) = add_real_key(
                &node,
                crate::keys::KeyKind::Api,
                Some("alice"),
                &["alert.send on service:portal/inbox_group:infra"],
            );
            assert_eq!(authorizer.check_key(&text), KeyCheck::Valid { key_id: id });
            assert_eq!(
                authorizer.check_key("a token of the file"),
                KeyCheck::NotAKey
            );
            assert_eq!(authorizer.check_key("api_short"), KeyCheck::NotAKey);
            let ghost = crate::keys::generate(crate::keys::KeyKind::Api)
                .unwrap()
                .to_string();
            assert_eq!(authorizer.check_key(&ghost), KeyCheck::Invalid);
            // An owner without keys.own.use: the key is not valid.
            add_account(
                &node,
                "bob",
                &["alert.send on service:portal/inbox_group:infra"],
            );
            let (no_use, _) = add_real_key(
                &node,
                crate::keys::KeyKind::Api,
                Some("bob"),
                &["alert.send on service:portal/inbox_group:infra"],
            );
            assert_eq!(authorizer.check_key(&no_use), KeyCheck::Invalid);
            assert!(node
                .state
                .key_stats
                .counts()
                .iter()
                .any(|(r, n)| *r == Cause::OwnerCannotUseKeys.as_str() && *n >= 1));
            if node.state.store.is_shared() {
                // An old directory: not ready, never a wrong refusal.
                node.state.users.set_last_success(0);
                assert_eq!(authorizer.check_key(&text), KeyCheck::NotReady);
                node.state.users.refresh(&node.state.store).unwrap();
            }
        });
    }

    #[test]
    fn the_right_to_alert_an_account_is_flat_and_uses_the_same_budget_as_the_right_for_a_group() {
        each_backend(|backend| {
            let (node, authorizer) = inbox_node(backend);
            let mut owner = vec![
                Grant::new("access", "portal")
                    .unwrap()
                    .with_limits(limits(r#"{"alerts":[{"amount":2,"window":"minute"}]}"#)),
                Grant::parse("keys.own.use on service:portal/key_kind:api").unwrap(),
                Grant::parse("alert.send on service:portal/inbox_group:infra").unwrap(),
                Grant::parse("alert.send_user on service:portal").unwrap(),
            ];
            owner.sort();
            let row = |login: &str, grants: Vec<Grant>| crate::store::UserRow {
                login: login.into(),
                password_hash: password_hash(PASSWORD),
                grants,
                created_at: 1,
            };
            node.state.store.insert_user(&row("alice", owner)).unwrap();
            // The second owner has the right for a group only.
            node.state
                .store
                .insert_user(&row(
                    "bob",
                    vec![
                        Grant::parse("keys.own.use on service:portal/key_kind:api").unwrap(),
                        Grant::parse("alert.send on service:portal/inbox_group:infra").unwrap(),
                    ],
                ))
                .unwrap();
            node.state.users.refresh(&node.state.store).unwrap();
            let (_, id) = add_real_key(
                &node,
                crate::keys::KeyKind::Api,
                Some("alice"),
                &[
                    "alert.send on service:portal/inbox_group:infra",
                    "alert.send_user on service:portal",
                ],
            );
            // A key without the right: refused. A key whose owner lacks the right: refused too.
            let (_, group_only) = add_real_key(
                &node,
                crate::keys::KeyKind::Api,
                Some("alice"),
                &["alert.send on service:portal/inbox_group:infra"],
            );
            let (_, owner_lacks) = add_real_key(
                &node,
                crate::keys::KeyKind::Api,
                Some("bob"),
                &[
                    "alert.send on service:portal/inbox_group:infra",
                    "alert.send_user on service:portal",
                ],
            );
            let refused = |decision: Decision| matches!(decision, Decision::Refused { .. });
            assert!(refused(authorizer.decide_service(
                &group_only,
                "alert.send_user",
                true
            )));
            assert!(refused(authorizer.decide_service(
                &owner_lacks,
                "alert.send_user",
                true
            )));
            // One unit for an alert to an account, one for an alert to a group: the budget is
            // one count. The third use finds no room, for either action.
            assert_eq!(
                authorizer.decide_service(&id, "alert.send_user", true),
                Decision::Allowed
            );
            assert_eq!(
                authorizer.decide(&id, "alert.send", "infra", true),
                Decision::Allowed
            );
            assert!(matches!(
                authorizer.decide_service(&id, "alert.send_user", true),
                Decision::Limited { .. }
            ));
            assert!(matches!(
                authorizer.decide(&id, "alert.send", "infra", true),
                Decision::Limited { .. }
            ));
            // A decision without a use takes no unit and does not look at the budget.
            assert_eq!(
                authorizer.decide_service(&id, "alert.send_user", false),
                Decision::Allowed
            );
            // The accounts that exist: the accounts of the directory and the superadmin.
            assert_eq!(authorizer.user_exists("alice"), Some(true));
            assert_eq!(
                authorizer.user_exists(&node.state.superadmin_login),
                Some(true)
            );
            assert_eq!(authorizer.user_exists("ghost"), Some(false));
            if node.state.store.is_shared() {
                node.state.users.set_last_success(0);
                assert_eq!(authorizer.user_exists("alice"), None);
            }
        });
    }

    #[test]
    fn the_decision_for_a_key_follows_both_sides_the_limit_and_the_critical_group() {
        each_backend(|backend| {
            let (node, authorizer) = inbox_node(backend);
            // The owner has a budget of 2 each minute on the service inbox.
            let mut owner = vec![
                Grant::new("access", "portal")
                    .unwrap()
                    .with_limits(limits(r#"{"alerts":[{"amount":2,"window":"minute"}]}"#)),
                Grant::parse("keys.own.use on service:portal/key_kind:api").unwrap(),
                Grant::parse("alert.send on service:portal/inbox_group:infra").unwrap(),
                Grant::parse("alert.send on service:portal/inbox_group:oncall").unwrap(),
            ];
            owner.sort();
            node.state
                .store
                .insert_user(&crate::store::UserRow {
                    login: "alice".into(),
                    password_hash: password_hash(PASSWORD),
                    grants: owner,
                    created_at: 1,
                })
                .unwrap();
            node.state.users.refresh(&node.state.store).unwrap();
            let (_, id) = add_real_key(
                &node,
                crate::keys::KeyKind::Api,
                Some("alice"),
                &[
                    "alert.send on service:portal/inbox_group:infra",
                    "alert.send on service:portal/inbox_group:oncall",
                ],
            );
            let refused = |decision: Decision| matches!(decision, Decision::Refused { .. });
            // A group with no permission: refused, nothing counted.
            assert!(refused(authorizer.decide(
                &id,
                "alert.send",
                "missing",
                true
            )));
            assert!(refused(authorizer.decide(
                &id,
                "alert.read",
                "infra",
                false
            )));
            // Two uses fit, the third finds no room.
            assert_eq!(
                authorizer.decide(&id, "alert.send", "infra", true),
                Decision::Allowed
            );
            assert_eq!(
                authorizer.decide(&id, "alert.send", "infra", true),
                Decision::Allowed
            );
            let Decision::Limited { retry_after } =
                authorizer.decide(&id, "alert.send", "infra", true)
            else {
                panic!("the limit must refuse");
            };
            assert!((1..=60).contains(&retry_after));
            // The inbox does not count a critical group: the same key, never a limit.
            for _ in 0..10 {
                assert_eq!(
                    authorizer.decide(&id, "alert.send", "oncall", false),
                    Decision::Allowed
                );
            }
            // A decision without a use takes no unit, also when the budget is empty.
            assert_eq!(
                authorizer.decide(&id, "alert.send", "infra", false),
                Decision::Allowed
            );
            // A key that does not exist is not valid.
            assert_eq!(
                authorizer.decide("0000000000000000", "alert.send", "infra", true),
                Decision::Refused {
                    reason: "key_not_valid"
                }
            );
            // The superadmin as the owner: no limit, no counter.
            let (_, root_key) = add_real_key(
                &node,
                crate::keys::KeyKind::Api,
                None,
                &["alert.send on service:portal/inbox_group:infra"],
            );
            for _ in 0..5 {
                assert_eq!(
                    authorizer.decide(&root_key, "alert.send", "infra", true),
                    Decision::Allowed
                );
            }
        });
    }
}

// The kinds of key, the verifier of each kind, and the switches -----------------------------------

mod kinds_tests {
    use super::inbox_rights_tests::{add_account, add_real_key, inbox_node};
    use super::*;
    use crate::inbox::rights::{Decision, KeyCheck};
    use crate::keys::KeyKind;

    const EDGE_URI: &str = "/db/x";

    fn edge(node: &Node, text: &str) -> Reply {
        send(
            node.port,
            "GET",
            "/auth/verify-key",
            &[
                ("Authorization", &format!("Bearer {text}")),
                ("X-Forwarded-Uri", EDGE_URI),
            ],
            "",
        )
    }

    fn failed(node: &Node, cause: crate::keyauth::Cause) -> u64 {
        node.state
            .key_stats
            .counts()
            .iter()
            .find(|(name, _)| *name == cause.as_str())
            .map_or(0, |(_, n)| *n)
    }

    /// The text of the key with a prefix of the same length and the same other characters.
    fn with_prefix(text: &str, prefix: &str) -> String {
        format!("{prefix}{}", &text[4..])
    }

    #[test]
    fn an_api_key_at_the_key_edge_is_refused_and_an_mcp_key_works_there() {
        each_backend(|backend| {
            let (node, authorizer) = inbox_node(backend);
            publish_catalog(&node, &[]);
            add_account(
                &node,
                "alice",
                &[
                    "access on service:db",
                    "keys.own.use on service:portal/key_kind:mcp",
                    "keys.own.use on service:portal/key_kind:api",
                    "alert.send on service:portal/inbox_group:infra",
                ],
            );
            let (mcp, _) = add_real_key(
                &node,
                KeyKind::Mcp,
                Some("alice"),
                &["access on service:db"],
            );
            let (api, api_id) = add_real_key(
                &node,
                KeyKind::Api,
                Some("alice"),
                &["alert.send on service:portal/inbox_group:infra"],
            );
            assert_eq!(
                edge(&node, &mcp).status,
                204,
                "the kind that the edge serves"
            );
            let wrong_before = failed(&node, crate::keyauth::Cause::WrongKind);
            let reply = edge(&node, &api);
            assert_eq!(reply.status, 401);
            assert!(reply.body.is_empty(), "no detail: {}", reply.body);
            assert_eq!(
                failed(&node, crate::keyauth::Cause::WrongKind),
                wrong_before + 1
            );
            // The other verifier: the inbox serves the kind api, and an MCP text is no key for it.
            assert_eq!(authorizer.check_key(&mcp), KeyCheck::NotAKey);
            assert_eq!(
                authorizer.check_key(&api),
                KeyCheck::Valid {
                    key_id: api_id.clone()
                }
            );
            // The directory holds the kind: an api key never gets a token at the edge.
            assert_eq!(
                node.state.users.key_by_id(&api_id).unwrap().kind,
                KeyKind::Api
            );
        });
    }

    #[test]
    fn a_reserved_prefix_a_bad_checksum_and_a_changed_prefix_are_refused_before_any_lookup() {
        each_backend(|backend| {
            let (node, authorizer) = inbox_node(backend);
            publish_catalog(&node, &[]);
            add_account(
                &node,
                "alice",
                &[
                    "access on service:db",
                    "keys.own.use on service:portal/key_kind:mcp",
                    "keys.own.use on service:portal/key_kind:api",
                ],
            );
            let (mcp, _) = add_real_key(
                &node,
                KeyKind::Mcp,
                Some("alice"),
                &["access on service:db"],
            );
            let (api, _) = add_real_key(&node, KeyKind::Api, Some("alice"), &[]);
            let unknown_before = failed(&node, crate::keyauth::Cause::UnknownKey);
            let format_before = failed(&node, crate::keyauth::Cause::BadFormat);
            let mut bad_sum = mcp.clone();
            let last = bad_sum.pop().unwrap();
            bad_sum.push(if last == 'a' { 'b' } else { 'a' });
            let refused = [
                // The reserved prefix, whatever the checksum says.
                with_prefix(&mcp, "whk_"),
                // A changed prefix of a good key: the checksum covers the prefix.
                with_prefix(&mcp, "api_"),
                with_prefix(&api, "mcp_"),
                bad_sum,
                "mcp_short".to_string(),
                "Bearer".to_string(),
            ];
            for text in &refused {
                assert_eq!(edge(&node, text).status, 401, "{text}");
                assert_eq!(authorizer.check_key(text), KeyCheck::NotAKey, "{text}");
            }
            assert_eq!(
                failed(&node, crate::keyauth::Cause::UnknownKey),
                unknown_before,
                "no lookup happened"
            );
            assert_eq!(
                failed(&node, crate::keyauth::Cause::BadFormat),
                format_before + refused.len() as u64
            );
            // A key with a good form that nobody has is a lookup that finds nothing.
            let ghost = crate::keys::generate(KeyKind::Mcp).unwrap().to_string();
            assert_eq!(edge(&node, &ghost).status, 401);
            assert_eq!(
                failed(&node, crate::keyauth::Cause::UnknownKey),
                unknown_before + 1
            );
        });
    }

    #[test]
    fn the_owner_loses_a_right_and_the_key_loses_it_at_once() {
        each_backend(|backend| {
            let (node, authorizer) = inbox_node(backend);
            let send = "alert.send on service:portal/inbox_group:infra";
            let usage = "keys.own.use on service:portal/key_kind:api";
            add_account(&node, "alice", &[usage, send]);
            let (_, id) = add_real_key(&node, KeyKind::Api, Some("alice"), &[send]);
            assert_eq!(
                authorizer.decide(&id, "alert.send", "infra", false),
                Decision::Allowed
            );
            let grants = |texts: &[&str]| -> Option<Vec<Grant>> {
                Some(texts.iter().map(|t| Grant::parse(t).unwrap()).collect())
            };
            // The owner loses the permission on the group: the key loses it with no wait.
            node.state.users.update("alice", None, grants(&[usage]));
            assert_eq!(
                authorizer.decide(&id, "alert.send", "infra", false),
                Decision::Refused {
                    reason: "no_permission"
                }
            );
            // The owner gets it again: the key has it again.
            node.state
                .users
                .update("alice", None, grants(&[usage, send]));
            assert_eq!(
                authorizer.decide(&id, "alert.send", "infra", false),
                Decision::Allowed
            );
            // The owner loses the right to use the keys of the kind: the key is not valid.
            node.state.users.update("alice", None, grants(&[send]));
            assert_eq!(
                authorizer.decide(&id, "alert.send", "infra", false),
                Decision::Refused {
                    reason: "key_not_valid"
                }
            );
            // The right to use the keys of the other kind does not count.
            node.state.users.update(
                "alice",
                None,
                grants(&[send, "keys.own.use on service:portal/key_kind:mcp"]),
            );
            assert_eq!(
                authorizer.decide(&id, "alert.send", "infra", false),
                Decision::Refused {
                    reason: "key_not_valid"
                }
            );
        });
    }

    #[test]
    fn a_deleted_account_loses_its_keys_and_the_tab_no_longer_lists_them() {
        each_backend(|backend| {
            let (node, _) = inbox_node(backend);
            publish_catalog(&node, &[]);
            let usage = "keys.own.use on service:portal/key_kind:api";
            add_account(&node, "carol", &[usage]);
            add_account(&node, "dave", &[usage]);
            let mut carol_keys = Vec::new();
            for name in ["one", "two", "three"] {
                let (_, id) = add_real_key(&node, KeyKind::Api, Some("carol"), &[]);
                node.state.store.execute_raw(&format!(
                    "UPDATE keys SET name = '{name}' WHERE id = '{id}'"
                ));
                carol_keys.push(id);
            }
            let (_, dave_key) = add_real_key(&node, KeyKind::Api, Some("dave"), &[]);
            node.state.users.refresh(&node.state.store).unwrap();
            let root = sign_in(&node, "root", PASSWORD);
            let cookie = format!("{}={root}", session_cookie_name(&node));
            // The page of the account names the keys that go with it.
            let page = send(
                node.port,
                "GET",
                "/auth/settings/users/carol",
                &[("Cookie", &cookie)],
                "",
            );
            assert_eq!(page.status, 200);
            assert!(
                page.body.contains("Delete carol and 3 keys?"),
                "{}",
                &page.body[page.body.find("Delete carol").unwrap_or(0)..]
                    [..300.min(page.body.len())]
            );
            let page = send(
                node.port,
                "GET",
                "/auth/settings/users/dave",
                &[("Cookie", &cookie)],
                "",
            );
            assert!(page.body.contains("Delete dave and 1 key?"));
            let all = get_keys(&node, &root, "?section=all");
            assert!(all.body.contains(">one</td>") && all.body.contains(">three</td>"));
            let deleted = post(&node, &root, USERS_DELETE_PATH, USERS_TOKEN, "login=carol");
            assert_eq!(deleted.status, 303);
            node.state.users.refresh(&node.state.store).unwrap();
            let left = node.state.users.keys();
            assert_eq!(left.len(), 1, "only the key of dave stays");
            assert_eq!(left[0].id, dave_key);
            // The pane of a key of the deleted owner closes: the page of a change answers as for a
            // key that does not exist.
            for id in &carol_keys {
                let pane = get_key_page(&node, &root, &format!("edit?id={id}"));
                assert_eq!(pane.location(), Some("/auth/settings?s=unknown_key"));
            }
            let all = get_keys(&node, &root, "?section=all");
            assert_eq!(all.status, 200);
            for name in ["one", "two", "three"] {
                assert!(
                    !all.body.contains(&format!(">{name}</span>")),
                    "the key {name} is gone"
                );
            }
            assert!(!all.body.contains("<td data-label=\"Owner\">carol"));
        });
    }

    /// A node with both kinds on and an account with the rights that the test gives.
    fn kinds_node(backend: &Backend, rights: &[&str]) -> (Node, String) {
        let (node, _) = inbox_node(backend);
        publish_catalog(&node, &[]);
        let mut all = vec!["keys.own.read on service:portal"];
        all.extend_from_slice(rights);
        add_account(&node, "alice", &all);
        let session = sign_in(&node, "alice", PASSWORD);
        (node, session)
    }

    #[test]
    fn with_the_right_to_make_mcp_keys_only_there_is_no_selector_and_no_api_key() {
        each_backend(|backend| {
            let (node, alice) = kinds_node(
                backend,
                &[
                    "keys.own.create on service:portal/key_kind:mcp",
                    "keys.own.use on service:portal/key_kind:mcp",
                    "keys.own.use on service:portal/key_kind:api",
                    "access on service:db",
                ],
            );
            let list = get_keys(&node, &alice, "");
            assert!(list.body.contains(r#"href="/auth/keys/new""#), "the entry");
            let form = get_new_key(&node, &alice, "");
            assert_eq!(form.status, 200);
            assert!(
                !form.body.contains("kind-form")
                    && !form.body.contains(r#"name="kind" value="api""#),
                "one kind only: no selector"
            );
            assert!(form
                .body
                .contains(r#"<input type="hidden" name="kind" value="mcp">"#));
            // The form of the other kind is a bad request, and a forced create is refused.
            assert_eq!(get_new_key(&node, &alice, "?kind=api").status, 400);
            assert_eq!(get_new_key(&node, &alice, "?kind=whk").status, 400);
            let forced = keys_post_raw(&node, &alice, "create", "kind=api&name=sneaky");
            assert_eq!(forced.status, 403);
            let reserved = keys_post_raw(&node, &alice, "create", "kind=whk&name=sneaky");
            assert_eq!(reserved.status, 400);
            let missing = keys_post_raw(&node, &alice, "create", "name=sneaky");
            assert_eq!(missing.status, 400);
            assert!(node.state.users.keys().is_empty());
            let made = keys_post(
                &node,
                &alice,
                "create",
                "name=agent&grant0=access+on+service%3Adb",
            );
            assert_eq!(made.status, 200, "{}", made.body);
            let text = key_text_in(&made.body).unwrap();
            assert!(text.starts_with("mcp_"));
        });
    }

    #[test]
    fn with_both_kinds_the_form_has_a_selector_and_each_kind_has_its_own_form() {
        each_backend(|backend| {
            let (node, alice) = kinds_node(
                backend,
                &[
                    "keys.own.create on service:portal/key_kind:mcp",
                    "keys.own.create on service:portal/key_kind:api",
                    "keys.own.use on service:portal/key_kind:mcp",
                    "keys.own.use on service:portal/key_kind:api",
                    "alert.send on service:portal/inbox_group:infra",
                    "access on service:db",
                ],
            );
            // The selector is two chips that are links: each kind is an address. The API kind is
            // the first choice.
            let form = get_new_key(&node, &alice, "");
            assert!(form.body.contains(
                r#"<a class="segmented__option is-active" href="/auth/keys/new?kind=api""#
            ));
            assert!(form
                .body
                .contains(r#"<a class="segmented__option" href="/auth/keys/new?kind=mcp""#));
            let api = get_new_key(&node, &alice, "?kind=api");
            assert_eq!(api.status, 200);
            assert!(api.body.contains(
                r#"<a class="segmented__option is-active" href="/auth/keys/new?kind=api""#
            ));
            assert!(
                api.body.contains("Calls that these rights allow")
                    && !api.body.contains("This key can"),
                "the calls of an API key, and no list that repeats the boxes"
            );
            assert!(api
                .body
                .contains(r#"<input type="hidden" name="kind" value="api">"#));
            // The form of an API key holds the alert actions and no service.
            let editor = editor_json(&api.body);
            assert!(editor["services"].as_array().unwrap().is_empty());
            let names: Vec<&str> = editor["features"]["actions"]
                .as_array()
                .unwrap()
                .iter()
                .map(|a| a["name"].as_str().unwrap())
                .collect();
            assert_eq!(
                names,
                ["access", "alert.send", "alert.send_user", "alert.read"]
            );
            // The form of an MCP key holds the services and no portal feature.
            let mcp = get_new_key(&node, &alice, "?kind=mcp");
            let editor = editor_json(&mcp.body);
            assert!(editor["features"].is_null());
            assert!(!editor["services"].as_array().unwrap().is_empty());
            // An API key: the alert action is a grant of the portal. A service is refused.
            let made = keys_post_raw(
                &node,
                &alice,
                "create",
                "kind=api&name=grafana&grant0=alert.send+on+service%3Aportal%2Finbox_group%3Ainfra",
            );
            assert_eq!(made.status, 200, "{}", made.body);
            let text = key_text_in(&made.body).unwrap();
            assert!(text.starts_with("api_"));
            let refused = keys_post_raw(
                &node,
                &alice,
                "create",
                "kind=api&name=bad&grant0=access+on+service%3Adb",
            );
            assert_eq!(
                refused.location(),
                Some("/auth/keys/new?kind=api&s=bad_grant")
            );
            let refused = keys_post_raw(
                &node,
                &alice,
                "create",
                "kind=mcp&name=bad&grant0=alert.send+on+service%3Aportal%2Finbox_group%3Ainfra",
            );
            assert_eq!(
                refused.location(),
                Some("/auth/keys/new?kind=mcp&s=bad_grant")
            );
            let list = get_keys(&node, &alice, "");
            assert!(list.body.contains(r#"data-kind="api""#));
        });
    }

    #[test]
    fn without_a_right_to_make_a_key_there_is_no_new_key_entry() {
        each_backend(|backend| {
            let (node, alice) =
                kinds_node(backend, &["keys.own.use on service:portal/key_kind:mcp"]);
            let list = get_keys(&node, &alice, "");
            assert_eq!(list.status, 200);
            assert!(!list.body.contains("New key"));
            assert!(!list.body.contains("/auth/keys/new"));
            assert_eq!(get_new_key(&node, &alice, "").status, 403);
            // A kind that the file turns off is not a kind to make, whatever the grants say.
            let (node, alice) = kinds_node(
                backend,
                &[
                    "keys.own.create on service:portal/key_kind:api",
                    "keys.own.use on service:portal/key_kind:api",
                ],
            );
            assert!(get_keys(&node, &alice, "").body.contains("/auth/keys/new"));
        });
    }

    /// A node for the matrix of the switches.
    fn matrix_node(
        backend: &Backend,
        identity_pdp: bool,
        keys: Option<&str>,
        inbox: bool,
        allow_creation: bool,
    ) -> Node {
        let pdp_port = free_port();
        let server_key = inbox_core::server_key::ServerKey::generate()
            .unwrap()
            .encode()
            .to_string();
        let identity = if identity_pdp {
            format!(
                r#"identity {{ issuer = "https://portal.test" signing_key_env = "TEST_IDENTITY_KEY" }}
                pdp {{ listen_address = "127.0.0.1:{pdp_port}" url = "http://pdp.test:{pdp_port}" plain_http = true }}"#
            )
        } else {
            String::new()
        };
        let keys = keys.map(|k| format!("keys {{ {k} }}")).unwrap_or_default();
        let inbox = if inbox {
            r#"inbox {
                server_key_env = "INBOX_TEST_KEY"
                devices {
                  public_address = "127.0.0.1:4181"
                  heartbeat_interval = "20s"
                  session_timeout = "60s"
                  resend_after = "15s"
                  max_inflight = 32
                  unreachable_after = "2m"
                  offline_warning_after = "3m"
                }
                storage {
                  delivery_ttl = "7d"
                  alert_retention = "30d"
                  connection_log_retention = "90d"
                }
                group "infra" { name = "Infra" }
              }"#
        } else {
            ""
        };
        let node = start_custom(
            Arc::new(backend.fresh()),
            None,
            Custom {
                allow_creation,
                env: vec![("INBOX_TEST_KEY".into(), server_key)],
                ..Custom::default()
            },
            &format!("{identity}\n{keys}\n{inbox}"),
        );
        publish_catalog(&node, &[]);
        add_account(&node, "alice", &["access on service:db"]);
        node
    }

    /// What the page of the users and the page of an account show, and which routes answer.
    struct Seen {
        tabs: Vec<&'static str>,
        new_user: bool,
        features: Option<Vec<String>>,
        nodes: Vec<String>,
        create_route: u16,
        keys_routes: [u16; 2],
        /// The routes of the inbox answer as a path that nobody owns: the page of the tab, the
        /// version and the API of the senders.
        inbox_routes_absent: bool,
        inbox_page: u16,
        edge: u16,
    }

    fn look(node: &Node) -> Seen {
        let root = sign_in(node, "root", PASSWORD);
        let cookie = format!("{}={root}", session_cookie_name(node));
        let users = send(
            node.port,
            "GET",
            "/auth/settings",
            &[("Cookie", &cookie)],
            "",
        );
        assert_eq!(users.status, 200, "{}", users.body);
        let mut tabs = Vec::new();
        for (path, name) in [
            ("/auth/settings", "access"),
            ("/auth/settings/services", "services"),
            ("/auth/settings/activity", "activity"),
            ("/auth/settings/inbox", "inbox"),
        ] {
            if users.body.contains(&format!(r#"href="{path}""#)) {
                tabs.push(name);
            }
        }
        let with_add = send(
            node.port,
            "GET",
            "/auth/settings/new-user",
            &[("Cookie", &cookie)],
            "",
        );
        // The button is in the page, and the page of the creation exists. Both or none.
        let new_user = users.body.contains("New user") || with_add.status == 200;
        assert_eq!(
            users.body.contains("New user"),
            with_add.status == 200,
            "the button and the route agree"
        );
        assert!(
            users.status != 200 || !users.body.contains("id=\"add-user\""),
            "the form that made accounts is not in the Users page"
        );
        let account = send(
            node.port,
            "GET",
            "/auth/settings/users/alice",
            &[("Cookie", &cookie)],
            "",
        );
        assert_eq!(account.status, 200);
        let editor = editor_json(&account.body);
        let features = editor["features"].as_object().map(|f| {
            f["actions"]
                .as_array()
                .unwrap()
                .iter()
                .map(|a| a["name"].as_str().unwrap().to_string())
                .collect::<Vec<_>>()
        });
        let nodes = editor["features"]["nodes"]
            .as_array()
            .map(|nodes| {
                nodes
                    .iter()
                    .map(|n| n["path"].as_str().unwrap().to_string())
                    .collect()
            })
            .unwrap_or_default();
        let create = post(
            node,
            &root,
            USERS_ADD_PATH,
            USERS_TOKEN,
            "login=zed&password=a-long-password-9",
        );
        let get = |path: &str| send(node.port, "GET", path, &[("Cookie", &cookie)], "").status;
        let edge = send(
            node.port,
            "GET",
            "/auth/verify-key",
            &[
                (
                    "Authorization",
                    &format!(
                        "Bearer {}",
                        crate::keys::generate(KeyKind::Mcp).unwrap().as_str()
                    ),
                ),
                ("X-Forwarded-Uri", EDGE_URI),
            ],
            "",
        )
        .status;
        Seen {
            tabs,
            new_user,
            features,
            nodes,
            create_route: create.status,
            keys_routes: [get("/auth/keys"), get("/auth/keys/new")],
            inbox_routes_absent: send(node.port, "GET", "/api/version", &[], "").status
                == send(node.port, "GET", "/api/nothing-here", &[], "").status
                && send(node.port, "POST", "/api/v1/alerts", &[], "{}").status
                    == send(node.port, "POST", "/api/nothing-here", &[], "{}").status
                && send(node.port, "POST", "/v1/enroll", &[], "{}").status
                    == send(node.port, "POST", "/nothing-here", &[], "{}").status,
            inbox_page: get("/auth/settings/inbox"),
            edge,
        }
    }

    fn names(list: &Option<Vec<String>>) -> Vec<&str> {
        list.iter().flatten().map(String::as_str).collect()
    }

    #[test]
    fn panel_only_shows_three_tabs_no_new_user_and_no_feature_of_the_portal() {
        each_backend(|backend| {
            let node = matrix_node(backend, false, None, false, false);
            let seen = look(&node);
            assert_eq!(seen.tabs, ["access", "services", "activity"]);
            assert!(!seen.new_user, "no entry that makes an account");
            assert_eq!(seen.create_route, 404);
            assert!(seen.features.is_none(), "no part for the portal features");
            assert_eq!(seen.keys_routes, [404, 404]);
            assert!(seen.inbox_routes_absent);
            assert_eq!(seen.inbox_page, 404);
            assert_eq!(seen.edge, 401, "no key is accepted");
        });
    }

    #[test]
    fn panel_and_keys_has_three_tabs_one_kind_and_no_inbox() {
        each_backend(|backend| {
            let node = matrix_node(backend, true, Some(""), false, true);
            let seen = look(&node);
            assert_eq!(seen.tabs, ["access", "services", "activity"]);
            assert!(seen.new_user);
            let features = names(&seen.features);
            assert!(features.contains(&"keys.own.read") && features.contains(&"keys.own.create"));
            assert!(!features
                .iter()
                .any(|a| ["alert.send", "inbox.receive", "phone.read"].contains(a)));
            assert_eq!(seen.nodes, ["", "key_kind:mcp"], "one kind");
            assert_eq!(
                seen.keys_routes,
                [404, 200],
                "the list is in the tab Access"
            );
            assert!(seen.inbox_routes_absent);
            assert_eq!(seen.inbox_page, 404);
            // The new-key form has no selector: one kind is possible.
            let alice = {
                node.state.users.update(
                    "alice",
                    None,
                    Some(vec![
                        Grant::parse("keys.own.read on service:portal").unwrap(),
                        Grant::parse("keys.own.create on service:portal/key_kind:mcp").unwrap(),
                    ]),
                );
                sign_in(&node, "alice", PASSWORD)
            };
            let form = get_new_key(&node, &alice, "");
            assert_eq!(form.status, 200, "{}", form.body);
            assert!(!form.body.contains("<select"));
        });
    }

    #[test]
    fn panel_and_inbox_has_the_inbox_tab_and_no_key_of_any_kind() {
        each_backend(|backend| {
            let node = matrix_node(backend, false, None, true, true);
            let seen = look(&node);
            assert_eq!(seen.tabs, ["access", "services", "activity", "inbox"]);
            let features = names(&seen.features);
            assert!(features.contains(&"inbox.receive") && features.contains(&"alert.send"));
            assert!(
                !features
                    .iter()
                    .any(|a| a.starts_with("keys.") || *a == "users.read"),
                "{features:?}"
            );
            assert_eq!(seen.nodes, ["", "inbox_group:infra"], "no kind of key");
            assert_eq!(seen.keys_routes, [404, 404]);
            assert_eq!(seen.edge, 401);
        });
    }

    #[test]
    fn all_on_has_the_four_tabs_and_both_kinds() {
        each_backend(|backend| {
            let node = matrix_node(backend, true, Some("api = true"), true, true);
            let seen = look(&node);
            assert_eq!(seen.tabs, ["access", "services", "activity", "inbox"]);
            assert!(seen.new_user);
            assert_ne!(seen.create_route, 404);
            let features = names(&seen.features);
            for action in [
                "keys.own.create",
                "alert.send",
                "inbox.receive",
                "phone.read",
            ] {
                assert!(features.contains(&action), "{action}");
            }
            assert_eq!(
                seen.nodes,
                ["", "key_kind:mcp", "key_kind:api", "inbox_group:infra"]
            );
            assert_eq!(seen.keys_routes, [404, 200]);
        });
    }

    #[test]
    fn no_creation_leaves_all_the_tabs_and_removes_only_the_creation_of_users() {
        each_backend(|backend| {
            let node = matrix_node(backend, true, Some("api = true"), true, false);
            let seen = look(&node);
            assert_eq!(seen.tabs, ["access", "services", "activity", "inbox"]);
            assert!(!seen.new_user, "no button, no form, no entry in the page");
            assert_eq!(seen.create_route, 404);
            // The accounts that exist stay editable, as the mode of the grants says.
            let root = sign_in(&node, "root", PASSWORD);
            let saved = post(
                &node,
                &root,
                USERS_UPDATE_PATH,
                USERS_TOKEN,
                "login=alice&grants=1&grant0=access+on+service%3Adb&grant1=keys.own.read+on+service%3Aportal",
            );
            assert_eq!(saved.status, 303);
            assert!(
                saved.location().unwrap().ends_with("?s=user-updated"),
                "{:?}",
                saved.location()
            );
        });
    }

    #[test]
    fn the_users_page_of_a_panel_only_file_has_no_column_of_a_feature_and_no_way_to_create() {
        each_backend(|backend| {
            let node = matrix_node(backend, false, None, false, false);
            let root = sign_in(&node, "root", PASSWORD);
            let users = page(&node, &root, "/auth/settings");
            assert_eq!(users.status, 200, "{}", users.body);
            for gone in [
                "Features</th>",
                "Keys</th>",
                "New user",
                "/auth/settings/new-user",
                "Create user",
                r#"class="feat""#,
            ] {
                assert!(!users.body.contains(gone), "{gone}");
            }
            // The list of the accounts is there. The sign-ins and the changes are in the tab Activity.
            for part in ["Accounts", "Last sign-in"] {
                assert!(users.body.contains(part), "{part}");
            }
            let activity = page(&node, &root, "/auth/settings/activity");
            assert_eq!(activity.status, 200, "{}", activity.body);
            assert!(activity.body.contains("Sign-ins"));
            let nav = users.body.split("<main").next().unwrap();
            assert_eq!(nav.matches("contentTabs__tab").count(), 3, "three tabs");
            assert!(!nav.contains("/auth/keys") && !nav.contains("/auth/settings/inbox"));
            // The route of the creation answers 404, and the old routes are gone.
            for path in [
                "/auth/settings/new-user",
                "/auth/settings/logins",
                "/auth/settings/services/db",
            ] {
                assert_eq!(page(&node, &root, path).status, 404, "{path}");
            }
            assert_eq!(
                page(&node, &root, "/auth/settings?add=1")
                    .body
                    .matches("add-user")
                    .count(),
                0
            );
        });
    }

    #[test]
    fn with_everything_on_the_users_page_has_the_columns_the_button_and_the_create_page() {
        each_backend(|backend| {
            let node = matrix_node(backend, true, Some("api = true"), true, true);
            let root = sign_in(&node, "root", PASSWORD);
            let users = page(&node, &root, "/auth/settings");
            assert_eq!(users.status, 200);
            for part in [
                "Features</th>",
                "Keys</th>",
                "New user",
                "/auth/settings/new-user",
            ] {
                assert!(users.body.contains(part), "{part}");
            }
            let nav = users.body.split("<main").next().unwrap();
            assert_eq!(nav.matches("contentTabs__tab").count(), 4, "four tabs");
            let create = page(&node, &root, "/auth/settings/new-user");
            assert_eq!(create.status, 200, "{}", create.body);
            assert!(create.body.contains(r#"id="add-user""#));
            assert!(create.body.contains(r#"data-layout="groups""#));
            assert!(create.body.contains(">Create</button>"));
        });
    }

    #[test]
    fn the_create_page_makes_the_account_with_its_grants_in_one_request() {
        each_backend(|backend| {
            let node = matrix_node(backend, true, Some("api = true"), true, true);
            let root = sign_in(&node, "root", PASSWORD);
            let made = post(
                &node,
                &root,
                USERS_ADD_PATH,
                USERS_TOKEN,
                "login=zed&password=a-long-password-9&grants=1&grant0=keys.own.read+on+service%3Aportal&grant1=inbox.receive+on+service%3Aportal%2Finbox_group%3Ainfra",
            );
            assert_eq!(made.status, 303);
            assert_eq!(
                made.location(),
                Some("/auth/settings/users/zed?s=user-added")
            );
            let zed = node.state.users.get("zed").unwrap();
            assert_eq!(zed.grants.len(), 2, "{:?}", zed.grants);
            let stored = node.state.store.load_users().unwrap();
            assert!(stored
                .iter()
                .any(|u| u.login == "zed" && u.grants.len() == 2));
            // A grant that the documents do not know: the account is not made, and the page says so.
            let refused = post(
                &node,
                &root,
                USERS_ADD_PATH,
                USERS_TOKEN,
                "login=yan&password=a-long-password-9&grants=1&grant0=inbox.receive+on+service%3Aportal%2Finbox_group%3Anope",
            );
            assert_eq!(refused.status, 303);
            assert_eq!(
                refused.location(),
                Some("/auth/settings/new-user?s=unknown-grant")
            );
            assert!(node.state.users.get("yan").is_none());
            // An action that needs another one: refused too.
            let needs = post(
                &node,
                &root,
                USERS_ADD_PATH,
                USERS_TOKEN,
                "login=xav&password=a-long-password-9&grants=1&grant0=keys.own.edit+on+service%3Aportal",
            );
            assert_eq!(
                needs.location(),
                Some("/auth/settings/new-user?s=keys-needs")
            );
            // A refused login comes back to the create page, with the notice.
            let weak = post(
                &node,
                &root,
                USERS_ADD_PATH,
                USERS_TOKEN,
                "login=wil&password=short",
            );
            assert_eq!(
                weak.location(),
                Some("/auth/settings/new-user?s=weak-password")
            );
            // A field of the grants without the flag `grants` is a bad request.
            let bad = post(
                &node,
                &root,
                USERS_ADD_PATH,
                USERS_TOKEN,
                "login=vic&password=a-long-password-9&grant0=keys.own.read+on+service%3Aportal",
            );
            assert_eq!(bad.status, 400);
            assert!(node.state.users.get("vic").is_none());
            let notice = page(&node, &root, "/auth/settings/new-user?s=exists");
            assert!(notice.body.contains("That login already exists."));
        });
    }

    #[test]
    fn the_services_page_selects_a_service_with_a_query_parameter() {
        each_backend(|backend| {
            let node = matrix_node(backend, false, None, false, true);
            let root = sign_in(&node, "root", PASSWORD);
            let first = page(&node, &root, "/auth/settings/services");
            assert_eq!(first.status, 200, "{}", first.body);
            assert!(first.body.contains(r#"aria-labelledby="detail-title""#));
            let db = page(&node, &root, "/auth/settings/services?svc=db");
            assert!(
                db.body.contains(r#"id="detail-title">Db</h2>"#),
                "{}",
                db.body
            );
            assert!(db.body.contains(r#"aria-current="true">Db</a>"#));
            // Alice has `access` on the service db: she is in the panel, and the others are not.
            assert!(db
                .body
                .contains(r#"<a href="/auth/settings/users/alice">alice</a>"#));
            let other = page(&node, &root, "/auth/settings/services?svc=%3Cnope%3E");
            assert_eq!(other.status, 200);
            assert!(!other.body.contains("<nope>"));
            // The old page of one service is gone.
            assert_eq!(page(&node, &root, "/auth/settings/services/db").status, 404);
        });
    }
}
