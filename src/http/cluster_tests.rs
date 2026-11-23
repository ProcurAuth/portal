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
use crate::ui::mask::Mask;

const PASSWORD: &str = "correct-horse-battery-staple";
const SECRET: [u8; 32] = [b'k'; 32];
const AGENT: &str = "ClusterTest/1.0";
/// The key of the second factor of the harness: 32 bytes as 64 hexadecimal characters.
const TOTP_KEY: &str = "00112233445566778899aabbccddeeff00112233445566778899aabbccddeeff";
pub(super) const PDP_DB_TOKEN: &str = "the PDP token of the server db, 32 bytes or more";
const PDP_CHAT_TOKEN: &str = "the PDP token of the server chat, 32 bytes or more";

pub(super) struct Reply {
    pub(super) status: u16,
    pub(super) headers: Vec<(String, String)>,
    pub(super) body: String,
}

impl Reply {
    pub(super) fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(header, _)| header == name)
            .map(|(_, value)| value.as_str())
    }

    pub(super) fn json(&self) -> serde_json::Value {
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

pub(super) fn send(
    port: u16,
    method: &str,
    path: &str,
    headers: &[(&str, &str)],
    body: &str,
) -> Reply {
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
pub(super) struct Node {
    pub(super) port: u16,
    pub(super) state: Arc<AppState>,
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

/// A free local port. The ports of all the tests of the process come from one list: see `util::test_ports`.
pub(super) fn free_port() -> u16 {
    crate::util::test_ports::free()
}

/// A node with more configuration. The `extra` text is added to the file.
/// The tests use these variables:
/// `TEST_IDENTITY_KEY` (a new Ed25519 key), `TEST_PDP_DB` and `TEST_PDP_CHAT` (PDP tokens).
pub(super) fn start_with(store: Arc<Store>, cache_ttl: Option<Duration>, extra: &str) -> Node {
    start_custom(store, cache_ttl, Custom::default(), extra)
}

/// The parts of the harness configuration that a test can replace.
pub(super) struct Custom {
    /// More keys of the `server` block.
    pub(super) server: &'static str,
    /// More keys of the `session` block.
    pub(super) session: &'static str,
    /// The body of the `discovery` block.
    pub(super) discovery: &'static str,
    /// The superadmin login, its password and the session secret.
    pub(super) login: &'static str,
    pub(super) password: &'static str,
    pub(super) session_secret: &'static str,
    /// Wait for the open port and a complete catalogue before the node is given to the test.
    /// A node that does not wait has no open port until its catalogue is complete (portal 7.3b).
    pub(super) wait_complete: bool,
    /// `users.max_users`.
    pub(super) max_users: usize,
    /// `users.allow_creation`. The mode `static` needs `false`.
    pub(super) allow_creation: bool,
    /// The value of `authentication.mfa`. The key of the second factor is given unless the value is `off`.
    pub(super) mfa: &'static str,
    /// More environment variables of the configuration.
    pub(super) env: Vec<(String, String)>,
    /// The stop of the server, in place of SIGTERM. The server stops when it gets a value.
    pub(super) stop: Option<tokio::sync::oneshot::Receiver<()>>,
    /// Start the inbox of the configuration (the block `inbox`) in the process, on a database of its own. Most tests
    /// have a block `inbox` for its rights only and leave the inbox off.
    pub(super) boot_inbox: bool,
    /// The clock of the items of the inbox stops at this time (ms) from the start.
    pub(super) inbox_clock_ms: Option<i64>,
}

impl Default for Custom {
    fn default() -> Self {
        Self {
            server: r#"trusted_proxies = ["127.0.0.1/32"]"#,
            session: "",
            discovery: r#"interval = "1h""#,
            login: "root",
            password: PASSWORD,
            session_secret: "0123456789abcdef0123456789abcdef",
            wait_complete: true,
            max_users: 10,
            allow_creation: true,
            // The harness runs with the second factor off, so the tests of the other features see the pages that they
            // always saw. The tests of the second factor set `optional` or `required`.
            mfa: "off",
            env: Vec::new(),
            stop: None,
            boot_inbox: false,
            inbox_clock_ms: None,
        }
    }
}

pub(super) fn start_custom(
    store: Arc<Store>,
    cache_ttl: Option<Duration>,
    custom: Custom,
    extra: &str,
) -> Node {
    let port = free_port();
    let key = crate::identity::testing::new_key_pem();
    let Custom {
        server,
        session,
        discovery,
        login,
        password,
        session_secret,
        wait_complete,
        max_users,
        allow_creation,
        mfa,
        env,
        stop,
        boot_inbox,
        inbox_clock_ms,
    } = custom;
    let totp_key = if mfa == "off" {
        ""
    } else {
        r#"totp_key_env = "PORTAL_TEST_TOTP_KEY""#
    };
    let (config, mut secrets) = Config::parse(
        &format!(
            r#"
        server {{ listen_address = "127.0.0.1:{port}" workers = 2 {server} }}
        authentication {{
          superadmin_login_env    = "PORTAL_TEST_LOGIN"
          superadmin_password_env = "PORTAL_TEST_PASSWORD"
          mfa                     = "{mfa}"
          {totp_key}
        }}
        users {{ max_users = {max_users} allow_creation = {allow_creation} }}
        session {{ secret_mode = "provided" secret_env = "PORTAL_TEST_SESSION" {session} }}
        discovery {{ {discovery} }}
        {extra}
        "#
        ),
        &|name| match name {
            "PORTAL_TEST_LOGIN" => Some(login.to_string()),
            "PORTAL_TEST_PASSWORD" => Some(password.to_string()),
            "PORTAL_TEST_SESSION" => Some(session_secret.to_string()),
            "PORTAL_TEST_TOTP_KEY" => Some(TOTP_KEY.to_string()),
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
    // A configuration with the block `inbox` starts the inbox on a database of its own: the file of the block
    // `storage`, or the database of the block `postgres`. The tests of the approvals use it (see `pdp_rig`).
    let inbox_start = (boot_inbox && config.inbox.is_some()).then(|| {
        let booted = match crate::inbox::boot(&config, &mut secrets, &|| false).unwrap() {
            crate::inbox::Boot::Ready(booted) => booted,
            _ => panic!("the inbox of the harness must boot"),
        };
        let password = secrets
            .postgres_password
            .as_ref()
            .map(|p| Zeroizing::new(std::str::from_utf8(p.as_bytes()).unwrap().to_string()));
        let (init, lifecycle) = booted.start(&config, password).unwrap();
        let init = match inbox_clock_ms {
            Some(ms) => init.with_fixed_clock(ms),
            None => init,
        };
        (init, lifecycle)
    });
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
        totp_key: secrets.totp_key.take(),
    };
    let mut state =
        AppState::new(config, superadmin, signing_secret, store, tracker, access).unwrap();
    if let Some(ttl) = cache_ttl {
        state.sessions.set_cache_ttl(ttl);
    }
    if let Some((init, lifecycle)) = inbox_start {
        state.inbox.prepare(init);
        // The threads of the inbox database stay until the end of the process.
        std::mem::forget(lifecycle);
    }
    let state = Arc::new(state);
    let serving = Arc::clone(&state);
    // The thread of the server tells how it ended. A start that fails (an address in use, a panic) is a failure of
    // the test at once, with the reason, and not a wait of 60 seconds.
    let ended: Arc<std::sync::Mutex<Option<String>>> = Arc::default();
    let report = Arc::clone(&ended);
    std::thread::spawn(move || {
        let run = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| match stop {
            Some(stop) => super::serve_with(serving, async move {
                let _ = stop.await;
            }),
            None => super::serve_with(serving, std::future::pending()),
        }));
        let text = match run {
            Ok(Ok(())) => "the server ended without an error".to_string(),
            Ok(Err(error)) => format!("the server stopped with an error: {error}"),
            Err(_) => "the server thread panicked".to_string(),
        };
        *report.lock().unwrap_or_else(|p| p.into_inner()) = Some(text);
    });
    let node = Node { port, state };
    if !wait_complete {
        return node;
    }
    // A start has time to be late: the whole suite starts hundreds of servers at once on a busy machine.
    let begun = Instant::now();
    loop {
        if let Some(text) = ended.lock().unwrap_or_else(|p| p.into_inner()).clone() {
            panic!("the server on port {port} is gone before it answered /healthz: {text}");
        }
        let seen = match TcpStream::connect(("127.0.0.1", port)) {
            Err(error) => format!("connect: {error}"),
            Ok(stream) => {
                drop(stream);
                let status = send(port, "GET", "/healthz", &[], "").status;
                if status == 200 {
                    break;
                }
                format!("/healthz answered {status}")
            }
        };
        assert!(
            begun.elapsed() < START_LIMIT,
            "the server on port {port} did not answer /healthz 200 in {START_LIMIT:?}; last try: {seen}"
        );
        std::thread::sleep(Duration::from_millis(100));
    }
    // The first discovery round publishes an empty catalogue. The next round starts an hour later.
    let waited = Instant::now();
    while node.state.discovery.refreshes() < 1 {
        assert!(
            waited.elapsed() < START_LIMIT,
            "the server on port {port} answers /healthz but its first discovery round did not end in {START_LIMIT:?}"
        );
        std::thread::sleep(Duration::from_millis(100));
    }
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

/// The time that a harness server has to start. The checks of a delay (a few seconds) use their own limits.
const START_LIMIT: Duration = Duration::from_secs(60);

pub(super) fn wait_until(limit: Duration, mut condition: impl FnMut() -> bool) -> Duration {
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

/// The body of the sign-in form, as a browser posts it. The token of the form is as old as a
/// person needs to type: five seconds.
fn login_body(node: &Node, csrf: &str, login: &str, password: &str) -> String {
    let token = form_token_issued(node, csrf, 5_000);
    login_body_with(node, csrf, login, password, &token, "")
}

/// The token of the sign-in form for the anti-CSRF token `csrf`, issued `age_ms` milliseconds ago.
fn form_token_issued(node: &Node, csrf: &str, age_ms: u64) -> String {
    sign_form_token(
        &node.state.signing_secret,
        csrf,
        unix_millis().saturating_sub(age_ms),
    )
    .unwrap()
}

/// The same with the token of the form and the trap field that the test chooses.
fn login_body_with(
    node: &Node,
    csrf: &str,
    login: &str,
    password: &str,
    token: &str,
    trap: &str,
) -> String {
    let signature = sign_return_uri(&node.state.signing_secret, "/").unwrap();
    format!(
        "login={login}&password={password}&csrf={csrf}&rd=%2F&sig={signature}&{FORM_TOKEN_FIELD}={token}&{TRAP_FIELD}={trap}"
    )
}

/// Signs in as the login form does. It returns the session cookie.
fn sign_in(node: &Node, login: &str, password: &str) -> String {
    let csrf = "A".repeat(32);
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
        &login_body(node, &csrf, login, password),
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
    // The browser dropped the cookie of the token: the form has a token and the request has none.
    let reply = send(
        node.port,
        "POST",
        LOGIN_PATH,
        &[
            ("Origin", &origin),
            ("Content-Type", "application/x-www-form-urlencoded"),
        ],
        &login_body(&node, &csrf, "root", PASSWORD),
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
fn the_page_of_an_account_made_on_the_other_instance_opens_at_once() {
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
        // The redirect after "New user" can reach the other instance. It has not read the accounts yet.
        assert_eq!(
            page(&b, &root, "/auth/settings/users/frank").status,
            200,
            "the page of the new account must open"
        );
        // A login that nobody made stays unknown.
        assert_eq!(page(&b, &root, "/auth/settings/users/nobody").status, 404);
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
fn a_read_of_the_accounts_that_just_ran_does_not_end_the_session_of_a_new_account() {
    each_backend(|backend| {
        let (a, b) = cluster(backend, Some(Duration::ZERO));
        // The hash is made before the test starts its clock. The steps below take a few milliseconds.
        let hash = password_hash("a-long-password-1");
        // The instance `b` reads the accounts now: the next read for an unknown login is less than a second away.
        b.state.users.refresh(&b.state.store).unwrap();
        a.state
            .store
            .insert_user(&crate::store::UserRow {
                login: "gina".into(),
                password_hash: hash,
                grants: Vec::new(),
                created_at: 1,
            })
            .unwrap();
        a.state.users.refresh(&a.state.store).unwrap();
        let gina = a
            .state
            .sessions
            .issue(
                client_binding(&a, "127.0.0.1"),
                crate::session::NewSession {
                    principal: Principal::User("gina".into()),
                    credential: credential_stamp(&a.state, &Principal::User("gina".into()))
                        .unwrap(),
                    login: "gina".into(),
                    impersonated: false,
                    remember: false,
                    mfa: false,
                    ip: None,
                    user_agent: AGENT.into(),
                    not_after: None,
                },
            )
            .unwrap();
        // The account is new for `b`, and the last read of `b` is too old to show it. The session is good all the same.
        assert_eq!(verify(&b, &gina, AGENT), 204, "{}", backend.name());
        assert_eq!(
            verify(&a, &gina, AGENT),
            204,
            "{}: the check on the other instance ended nothing",
            backend.name()
        );
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
pub(super) fn listed(labels: &[(&str, &str)]) -> crate::catalog::Service {
    let map = labels
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
    crate::catalog::parse_labels("test", &map).unwrap().unwrap()
}

/// The URL of the actions of the service `db` in the tests. The test fills the registry.
pub(super) const DB_ACTIONS_URL: &str = "http://test/.well-known/procurauth/actions";

/// The catalogue has two services. Grafana (`other-grafana`, only `access`) and Db (`db`, with actions).
pub(super) fn publish_catalog(node: &Node, db_actions: &[&str]) {
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

/// Replaces the grants of an account as the portal does: the database first, then the directory of the instance.
/// A change of the directory alone is lost at the next read of the shared accounts, which a login starts when the last
/// read is older than one second.
fn change_grants(node: &Node, login: &str, mut grants: Vec<Grant>) {
    grants.sort();
    assert!(
        node.state
            .store
            .update_user(login, None, Some(&grants))
            .unwrap(),
        "the account {login} exists"
    );
    assert!(node.state.users.update(login, None, Some(grants)));
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
pub(super) fn pdp(
    port: u16,
    path: &str,
    token: Option<&str>,
    body: &str,
    request_id: Option<&str>,
) -> Reply {
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

/// A question about a service, or about a node of depth 1 of the service `db` with its chain
/// `[service:db]` (protocol 4.1).
fn evaluation(subject: &str, action: &str, kind: &str, id: &str) -> String {
    serde_json::json!({
        "subject": {"type": "user", "id": subject},
        "action": {"name": action},
        "resource": depth_one(kind, id),
        "context": {}
    })
    .to_string()
}

/// The service `{"type": "service", "id": id}`, or a node below the service `db` with the chain
/// `[service:db]`.
fn depth_one(kind: &str, id: &str) -> serde_json::Value {
    if kind == "service" {
        serde_json::json!({"type": kind, "id": id})
    } else {
        node_of(kind, id, &[("service", "db")])
    }
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
    let chat = serde_json::json!({
        "subject": {"type": "user", "id": "alice"},
        "action": {"name": "query.run.write"},
        "resource": node_of("database", "analytics", &[("service", "chat")])
    });
    assert!(!decide(PDP_CHAT_TOKEN, &chat.to_string()));

    // The batch: one subject, a resource for each evaluation.
    let body = serde_json::json!({
        "subject": {"type": "user", "id": "alice"},
        "action": {"name": "query.run.write"},
        "evaluations": [
            {"resource": depth_one("database", "analytics")},
            {"resource": depth_one("database", "logs")},
            {"resource": depth_one("service", "db")},
            {"action": {"name": "query.run.read"}, "resource": depth_one("database", "logs")}
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
            "resource": depth_one(kind, id)
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
                "resource": depth_one(kind, id)
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
    // An id with a colon names a key. Without the block `keys`, a key is an unknown subject.
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

    // The service was never read: the PDP cannot check the chain of a node below the service,
    // and it cannot apply the limits of a grant. Only `access` on the service stays (protocol 11.1).
    assert!(
        !write(),
        "never read: the grant on a sub-resource does not apply"
    );
    assert!(
        !decide("system.read", "service", "db"),
        "never read: no decision on the whole service"
    );
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
            "no ancestors",
        ),
        (
            serde_json::json!({"type": "host", "id": "local"}),
            "no ancestors, also for a type of depth 1",
        ),
        (
            serde_json::json!({"type": "host", "id": "local", "properties": {}}),
            "properties without ancestors",
        ),
        (
            node_of("host", "local", &[]),
            "empty ancestors of a type of depth 1",
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

    // The grant of the document of one level does not follow the tree: it does not apply. The
    // chain of the old tree and a request without ancestors are not valid.
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
            node_of("database", "old", &service)
        ),
        invalid
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

    // A document of one level: each node has the chain `[service]`. A node without ancestors is not
    // valid, also when its type has the parent `service` (protocol 4.1, 11.1).
    publish_catalog(&node, &["query.run.read", "query.run.write"]);
    assert_eq!(
        single(
            "carol",
            "query.run.write",
            serde_json::json!({"type": "database", "id": "old"})
        ),
        invalid
    );
    assert_eq!(
        single("carol", "query.run.write", node_of("database", "old", &[])),
        invalid
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

/// A node of depth 1 needs its chain `[service]` (protocol 4.1, 11.1). Without `ancestors`, or with
/// an empty array, it gets `invalid_resource` in the three endpoints and in a batch item.
#[test]
fn a_node_of_depth_1_without_ancestors_is_an_invalid_resource_in_each_endpoint() {
    let pdp_port = free_port();
    let node = start_with(
        Arc::new(Backend::Sqlite.fresh()),
        None,
        &format!(
            r#"access {{ grants_mode = "hybrid" }}
            user "carol" {{ grants = ["query.run.read on service:db/host:local"] }}
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
    let question = |subject: &str, resource: &serde_json::Value| {
        serde_json::json!({
            "subject": {"type": "user", "id": subject},
            "action": {"name": "query.run.read"},
            "resource": resource
        })
    };
    let yes = serde_json::json!({"decision": true});
    let no = serde_json::json!({"decision": false});
    let invalid = serde_json::json!({"decision": false, "context": {"reason": "invalid_resource"}});
    // The host is at depth 1 in the tree of db.
    let chained = node_of("host", "local", &[("service", "db")]);
    let without = [
        (
            serde_json::json!({"type": "host", "id": "local"}),
            "no properties",
        ),
        (
            serde_json::json!({"type": "host", "id": "local", "properties": {}}),
            "properties without ancestors",
        ),
        (node_of("host", "local", &[]), "an empty array"),
    ];

    // One evaluation, for a person with a grant on the node and for the superadmin.
    for subject in ["carol", "root"] {
        assert_eq!(
            ask(
                PDP_DB_TOKEN,
                "/access/v1/evaluation",
                question(subject, &chained)
            ),
            yes,
            "{subject}: the chain [service]"
        );
        for (resource, why) in &without {
            assert_eq!(
                ask(
                    PDP_DB_TOKEN,
                    "/access/v1/evaluation",
                    question(subject, resource)
                ),
                invalid,
                "{subject}: {why}"
            );
        }
    }

    // A batch: the default resource of the request and each item without ancestors.
    let mut body = question("carol", &without[0].0);
    body["evaluations"] = serde_json::json!([
        {},
        {"resource": chained},
        {"resource": without[0].0},
        {"resource": without[1].0},
        {"resource": without[2].0}
    ]);
    assert_eq!(
        ask(PDP_DB_TOKEN, "/access/v1/evaluations", body)["evaluations"],
        serde_json::json!([invalid, yes, invalid, invalid, invalid])
    );
    let mut body = question("carol", &chained);
    body["evaluations"] = serde_json::json!([{}, {"resource": without[0].0}]);
    assert_eq!(
        ask(PDP_DB_TOKEN, "/access/v1/evaluations", body)["evaluations"],
        serde_json::json!([yes, invalid])
    );

    // The action search: no action on a node without ancestors, also for the superadmin.
    let search = |subject: &str, resource: &serde_json::Value| -> Vec<String> {
        ask(
            PDP_DB_TOKEN,
            "/access/v1/search/action",
            serde_json::json!({"subject": {"type": "user", "id": subject}, "resource": resource}),
        )["results"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| r["name"].as_str().unwrap().to_string())
            .collect()
    };
    assert_eq!(search("carol", &chained), ["query.run.read"]);
    assert_eq!(
        search("root", &chained),
        ["access", "query.run.read", "query.run.write", "system.read"]
    );
    for (resource, why) in &without {
        assert!(search("carol", resource).is_empty(), "carol: {why}");
        assert!(search("root", resource).is_empty(), "root: {why}");
    }

    // Without a known document of the service, the start of the chain is still checked.
    assert_eq!(
        ask(
            PDP_CHAT_TOKEN,
            "/access/v1/evaluation",
            question("root", &serde_json::json!({"type": "room", "id": "r"}))
        ),
        invalid
    );
    assert_eq!(
        ask(
            PDP_CHAT_TOKEN,
            "/access/v1/evaluation",
            question("root", &node_of("room", "r", &[("service", "chat")]))
        ),
        no
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
        assert!(refused.body.contains("a permission needs the switch Open"));
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
/// The document of actions of the service `db` of the tests of the options.
const TREE_DOCUMENT: &str = r#"{"server":"db","resource_types":[{"name":"host","parent":"service","description":"The type host."},{"name":"database","parent":"host","description":"The type database."}],"actions":[{"name":"query.run.read","resource_types":["service","host","database"],"description":"The action query.run.read."},{"name":"query.run.write","resource_types":["service","host","database"],"description":"The action query.run.write."},{"name":"system.read","resource_types":["service","host"],"description":"The action system.read."}]}"#;

fn publish_tree(node: &Node) {
    let document = crate::actions::parse("db", TREE_DOCUMENT.as_bytes()).unwrap();
    node.state.actions.set_document("db", document);
}

/// A service that lists options. It answers `GET /.well-known/procurauth/resources`: the databases
/// of the host `prod` and a 404 for the rest. It records each request line and its headers.
fn options_service() -> (u16, Arc<std::sync::Mutex<Vec<String>>>) {
    let listener = crate::util::test_ports::listener();
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
            // The loop of the actions of the portal reads the document of the service at each tick of 5 seconds, also
            // after the test changed the catalogue. It gets the same document that the test published. The log keeps
            // the requests for the options only.
            if !first.contains("/.well-known/procurauth/actions") {
                log.lock().unwrap().push(request.to_lowercase());
            }
            let (status, body) = if first.contains("/.well-known/procurauth/actions") {
                ("200 OK", TREE_DOCUMENT)
            } else if first.contains("type=database") && first.contains("parent=host%3Aprod") {
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
    // The probe connections of the start end in the background: the server drops their marks
    // a little after the client sees the close. On a loaded machine this takes longer.
    wait_until(Duration::from_secs(10), || node.state.stop.live() == 1);
    let stopped = Instant::now();
    tx.send(()).unwrap();
    std::thread::sleep(Duration::from_millis(1500));
    assert_eq!(
        send(node.port, "GET", "/healthz", &[], "").status,
        200,
        "the instance still serves in the first seconds"
    );
    // Only a refused connection shows a closed listener. Another error, such as a lack of local
    // ports on a loaded machine, shows nothing.
    wait_until(Duration::from_secs(10), || {
        matches!(
            TcpStream::connect(("127.0.0.1", node.port)),
            Err(error) if error.kind() == std::io::ErrorKind::ConnectionRefused
        )
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
        &login_body(node, &csrf, login, password),
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

/// The audit lead portal/login/argon2-wait-holds-connection-permits: a sign-in that waits for the
/// Argon2 permit keeps its place in `server.max_login_body_reads`, and it waits only until
/// `server.body_read_timeout` ends. A queue of right-password sign-ins of one account thus
/// leaves connections for ForwardAuth, and each one ends at the deadline with `503`.
#[test]
fn sign_ins_that_wait_for_the_argon2_check_leave_connections_for_forward_auth() {
    let node = start_custom(
        Arc::new(Store::open_in_memory()),
        None,
        Custom {
            server: r#"trusted_proxies = ["127.0.0.1/32"] max_connections = 5 max_login_body_reads = 2 body_read_timeout = "3s""#,
            ..Custom::default()
        },
        "",
    );
    insert_account(&node, "mallory", "mallory-dummy-password");
    // The test holds the Argon2 permit: it stands for a long queue of checks.
    let held = Arc::clone(&node.state.password_gate)
        .try_acquire_owned()
        .unwrap();
    let mut waiting = Vec::new();
    for _ in 0..5 {
        let node = node.clone();
        waiting.push(std::thread::spawn(move || {
            let started = Instant::now();
            let status = login_from(
                &node,
                "127.0.0.20",
                "mallory",
                "mallory-dummy-password",
                &[],
            )
            .status;
            (status, started.elapsed())
        }));
        std::thread::sleep(Duration::from_millis(200));
    }
    std::thread::sleep(Duration::from_millis(300));
    // Without the fix, five waiting sign-ins hold the five connections, and this one is dropped.
    let begun = Instant::now();
    let reply = send(
        node.port,
        "GET",
        VERIFY_PATH,
        &[("X-Forwarded-Uri", "/app/"), ("X-Forwarded-Method", "GET")],
        "",
    );
    assert_eq!(reply.status, 302, "ForwardAuth gets a connection");
    assert!(
        begun.elapsed() < Duration::from_secs(2),
        "{:?}",
        begun.elapsed()
    );
    assert_eq!(
        node.state.login_body_gate.available_permits(),
        0,
        "the waiting sign-ins keep the body permits"
    );
    let results: Vec<(u16, Duration)> = waiting.into_iter().map(|t| t.join().unwrap()).collect();
    assert!(
        results.iter().all(|(status, _)| *status == 503),
        "{results:?}"
    );
    let at_deadline = results
        .iter()
        .filter(|(_, took)| *took >= Duration::from_millis(2_500))
        .count();
    assert_eq!(
        at_deadline, 2,
        "two sign-ins waited until the deadline: {results:?}"
    );
    drop(held);
    assert_eq!(
        login_from(
            &node,
            "127.0.0.20",
            "mallory",
            "mallory-dummy-password",
            &[]
        )
        .status,
        303,
        "a sign-in with a free permit succeeds"
    );
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
pub(super) const KEY_TEST_DOCUMENT: &str = r#"{"server":"db",
  "resource_types":[{"name":"host","parent":"service","description":"A host."},
                    {"name":"database","parent":"host","description":"A database."}],
  "actions":[
    {"name":"data.read","resource_types":["host","database"],"description":"Read.",
     "limits":["requests","max_rows","max_duration"]},
    {"name":"data.write","resource_types":["host","database"],"description":"Write.",
     "limits":["max_duration"]}],
  "limits":[
    {"name":"requests","label":"Requests","description":"d","unit":"request","kind":"budget"},
    {"name":"max_rows","label":"Rows","description":"d","unit":"rows","kind":"ceiling","min":1,"max":100000},
    {"name":"max_duration","label":"Duration","description":"d","unit":"seconds","kind":"ceiling","min":1,"max":3600}]}"#;

/// The budgets count in windows that start at a multiple of their length of the clock. A test that counts several
/// uses of a budget of one minute must not see the end of the window in the middle: the count starts again and a
/// use that must be refused is good. The test waits for the next window when the end is near.
pub(super) fn stay_in_a_minute() {
    let left = 60 - unix_now().rem_euclid(60);
    if left < 12 {
        std::thread::sleep(Duration::from_secs(left as u64 + 1));
    }
}

pub(super) fn limits(text: &str) -> crate::limits::Limits {
    crate::limits::Limits::from_json(text).unwrap()
}

pub(super) fn key_caps() -> crate::store::KeyCaps {
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

/// Spec 3.9 and 10.7b: with PostgreSQL, the metadata publishes `keys.directory_max_age` as a whole number of seconds, and
/// 30 without the block `keys`. With SQLite, the PDP has no copy of its directory, so it publishes 1.
#[test]
fn the_metadata_publishes_the_age_bound_of_the_directory() {
    each_backend(|backend| {
        for (keys, shared) in [(r#"keys { directory_max_age = "45s" }"#, 45), ("", 30)] {
            let pdp_port = free_port();
            let _node = start_with(
                Arc::new(backend.fresh()),
                None,
                &format!(
                    r#"identity {{ issuer = "https://portal.test" signing_key_env = "TEST_IDENTITY_KEY" }}
                    pdp {{ listen_address = "127.0.0.1:{pdp_port}" url = "http://pdp.test:{pdp_port}" plain_http = true }}
                    {keys}
                    pdp_client "db" {{ token_env = "TEST_PDP_DB" }}"#
                ),
            );
            wait_until(Duration::from_secs(10), || {
                TcpStream::connect(("127.0.0.1", pdp_port)).is_ok()
            });
            let reply = send(
                pdp_port,
                "GET",
                "/.well-known/authzen-configuration",
                &[],
                "",
            );
            assert_eq!(reply.status, 200, "{}", reply.body);
            let metadata = reply.json();
            let expected = match backend {
                Backend::Sqlite => 1,
                Backend::Postgres(_) => shared,
            };
            assert_eq!(
                metadata["procurauth_directory_max_age_seconds"],
                expected,
                "[{}] {keys:?}: {metadata}",
                backend.name()
            );
        }
    });
}

/// Protocol 6.9, 10.7 and 10.7a: the metadata publishes the real limits of the PDP. The idle time is real: the listener
/// closes a connection that sends no request after `server.header_read_timeout`.
#[test]
fn the_metadata_publishes_the_maxima_of_the_pdp_and_the_idle_time_is_real() {
    let pdp_port = free_port();
    let _node = start_custom(
        Arc::new(Store::open_in_memory()),
        None,
        Custom {
            server: r#"trusted_proxies = ["127.0.0.1/32"] header_read_timeout = "1s""#,
            ..Custom::default()
        },
        &format!(
            r#"identity {{ issuer = "https://portal.test" signing_key_env = "TEST_IDENTITY_KEY" }}
            pdp {{ listen_address = "127.0.0.1:{pdp_port}" url = "http://pdp.test:{pdp_port}" plain_http = true max_evaluations = 8 max_body_bytes = "2KiB" }}
            pdp_client "db" {{ token_env = "TEST_PDP_DB" }}"#
        ),
    );
    wait_until(Duration::from_secs(10), || {
        TcpStream::connect(("127.0.0.1", pdp_port)).is_ok()
    });
    let reply = send(
        pdp_port,
        "GET",
        "/.well-known/authzen-configuration",
        &[],
        "",
    );
    assert_eq!(reply.status, 200, "{}", reply.body);
    let metadata = reply.json();
    assert_eq!(metadata["procurauth_idle_max_seconds"], 1, "{metadata}");
    assert_eq!(
        metadata["procurauth_evaluations_max_bytes"], 2048,
        "{metadata}"
    );
    assert_eq!(
        metadata["procurauth_evaluations_max_items"], 8,
        "{metadata}"
    );
    assert_eq!(metadata["procurauth_usage_max_seconds"], 5, "{metadata}");
    // A SQLite file has no copy of the directory: the bound is 1 (spec 3.9, see the test of the age bound).
    assert_eq!(
        metadata["procurauth_directory_max_age_seconds"], 1,
        "{metadata}"
    );
    // The limits are real. A body above the maximum, and a batch above the maximum, get 400 (spec 10.7a).
    let big = format!(
        r#"{{"subject":{{"type":"user","id":"x"}},"pad":"{}"}}"#,
        "x".repeat(3000)
    );
    assert_eq!(
        pdp(
            pdp_port,
            "/access/v1/evaluation",
            Some(PDP_DB_TOKEN),
            &big,
            None
        )
        .status,
        400
    );
    // The idle time: one request, then no byte. The listener closes the connection at the published time.
    let mut stream = TcpStream::connect(("127.0.0.1", pdp_port)).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    stream
        .write_all(b"GET /.well-known/authzen-configuration HTTP/1.1\r\nHost: pdp.test\r\n\r\n")
        .unwrap();
    let mut buffer = [0u8; 8192];
    let first = stream.read(&mut buffer).unwrap();
    assert!(first > 0 && buffer[..first].starts_with(b"HTTP/1.1 200"));
    let waited = std::time::Instant::now();
    let closed = stream.read(&mut buffer);
    let elapsed = waited.elapsed();
    assert!(
        matches!(closed, Ok(0)),
        "the listener closes the idle connection: {closed:?}"
    );
    assert!(
        elapsed >= Duration::from_millis(500) && elapsed <= Duration::from_millis(2500),
        "closed after {elapsed:?}, and the published idle time is 1 s"
    );
}

/// Alice reads `host:prod` with `max_rows` 5000 and a budget of 3 each minute. She can use keys.
pub(super) fn add_alice(node: &Node, extra: &[Grant]) {
    stay_in_a_minute();
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

pub(super) fn add_key(node: &Node, id: &str, owner: Option<&str>, byte: u8, grants: Vec<Grant>) {
    let mut grants = grants;
    grants.sort();
    node.state
        .store
        .insert_key(
            &crate::store::KeyRow {
                kind: crate::keys::KeyKind::Mcp,
                interaction: crate::keys::Interaction::Conversational,
                agents: crate::keys::AgentsMode::Discover,
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

pub(super) fn read_body(subject: &str, database: &str, context: serde_json::Value) -> String {
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
    stay_in_a_minute();
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
fn an_orphan_key_is_an_unknown_subject_and_a_condition_of_the_owner_gives_false_with_no_reason() {
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
        // The owner loses the right `keys.own.use`: the key gets `false` with no reason. The condition can change
        // later, so the key is not an orphan (protocol 3.5, 11.1).
        let plain = serde_json::json!({"decision": false});
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
        assert_eq!(ask("key:a3"), plain);
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
        assert_eq!(ask("key:a3"), plain, "the key is older than 0 seconds");
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
fn the_home_page_without_a_session_goes_to_the_login_with_no_parameter() {
    each_backend(|backend| {
        let (node, _) = cluster(backend, None);
        let home = send(node.port, "GET", "/", &[], "");
        assert_eq!(home.status, 302, "{}", home.body);
        assert_eq!(home.location(), Some(LOGIN_PATH));
        // Any other address keeps its signed destination.
        let behind = send(
            node.port,
            "GET",
            VERIFY_PATH,
            &[("X-Forwarded-Uri", "/app/"), ("X-Forwarded-Method", "GET")],
            "",
        );
        let location = behind.location().expect("a redirect").to_string();
        assert!(
            location.starts_with(&format!("{LOGIN_PATH}?rd=%2Fapp%2F&sig=")),
            "{location}"
        );
        // The login page without a parameter returns to the home page, with its own signature.
        let page = send(node.port, "GET", LOGIN_PATH, &[], "");
        assert_eq!(page.status, 200, "{}", page.body);
        assert!(page.body.contains(r#"name="rd""#));
    });
}

#[test]
fn a_key_of_the_superadmin_has_only_its_own_grants_and_its_own_counters() {
    stay_in_a_minute();
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

// The work on a timer ---------------------------------------------------------------------------

#[test]
fn the_rounds_write_the_key_checks_prune_the_old_keys_and_make_the_rows_of_a_new_directory() {
    stay_in_a_minute();
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
                    interaction: crate::keys::Interaction::Conversational,
                    agents: crate::keys::AgentsMode::Discover,
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
    stay_in_a_minute();
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
        assert!(refused.body.contains("a permission needs the switch Open"));
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
        assert!(refused.body.contains("a permission needs the switch Open"));
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
        &login_body(&node, &csrf, "dave", PASSWORD),
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
    // The age of the action document never changes the answer of the edge: `access` stays `true`, also when the
    // last good document is older than `access.actions_max_age` (protocol 5.3a, 8.3, 11.1).
    node.state.actions.set_document_aged(
        "db",
        crate::actions::parse("db", KEY_TEST_DOCUMENT.as_bytes()).unwrap(),
        Duration::from_secs(48 * 3600),
    );
    assert!(node.state.actions.too_old("db"));
    let old = send(
        node.port,
        "GET",
        "/auth/verify-key",
        &[
            ("Authorization", &format!("Bearer {key_text}")),
            ("X-Forwarded-Uri", "/db/x"),
        ],
        "",
    );
    assert_eq!(
        old.status, 204,
        "a too old document does not refuse the key: {:?}",
        old.headers
    );
    // A fresh read brings the document back: the PDP answers the questions of the service again (protocol 5.3a).
    node.state.actions.set_document(
        "db",
        crate::actions::parse("db", KEY_TEST_DOCUMENT.as_bytes()).unwrap(),
    );
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
        page.body.contains("2 checks"),
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
        authentication {{ superadmin_login_env = "PORTAL_TEST_LOGIN" superadmin_password_env = "PORTAL_TEST_PASSWORD" mfa = "off" }}
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
        interaction: crate::keys::Interaction::Conversational,
        agents: crate::keys::AgentsMode::Discover,
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
    stay_in_a_minute();
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
    use crate::inbox::rights::{Authorizer, Decision, KeyCheck, Standing, Subscribable};
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
                    interaction: crate::keys::Interaction::default_for(kind),
                    agents: crate::keys::AgentsMode::default_for(kind),
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
            // An account that the directory does not know is not a refusal with a shared
            // database: the file of another instance can have it. One instance on SQLite knows
            // every account: the login is absent (portal/inbox/phones-of-removed-file-account-stay-active).
            for login in ["alice", "bob", "root"] {
                assert_eq!(authorizer.standing(login), Standing::Exists, "{login}");
            }
            if node.state.store.is_shared() {
                assert_eq!(authorizer.subscribable("nobody"), Subscribable::Unknown);
                assert_eq!(authorizer.standing("nobody"), Standing::NotHere);
            } else {
                assert_eq!(authorizer.subscribable("nobody"), Subscribable::Absent);
                assert_eq!(authorizer.standing("nobody"), Standing::Absent);
            }
            // The stamp changes with the directory.
            let before = authorizer.directory_stamp().unwrap();
            change_grants(
                &node,
                "alice",
                vec![Grant::parse("inbox.receive on service:portal/inbox_group:oncall").unwrap()],
            );
            assert_ne!(authorizer.directory_stamp().unwrap(), before);
            assert_eq!(authorizer.subscribable("alice"), groups(&["oncall"]));
            // A directory that is old says nothing: no answer, no stamp, no removal.
            if node.state.store.is_shared() {
                node.state.users.set_last_success(0);
                assert_eq!(authorizer.subscribable("bob"), Subscribable::Unknown);
                assert_eq!(authorizer.subscribable("root"), Subscribable::Unknown);
                assert_eq!(authorizer.standing("root"), Standing::Unknown);
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
        stay_in_a_minute();
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
        stay_in_a_minute();
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
    fn no_page_body_of_a_server_with_the_inbox_and_the_keys_names_the_product() {
        use crate::ui::body_words::{assert_clean, product_words};
        each_backend(|backend| {
            let (node, _) = inbox_node(backend);
            publish_catalog(&node, &[]);
            add_account(
                &node,
                "alice",
                &[
                    "access on service:db",
                    "keys.own.read on service:portal",
                    "keys.own.edit on service:portal",
                    "keys.own.create on service:portal/key_kind:api",
                    "keys.own.use on service:portal/key_kind:api",
                    "inbox.receive on service:portal/inbox_group:infra",
                    "alert.send on service:portal/inbox_group:infra",
                ],
            );
            let (_, key_id) = add_real_key(
                &node,
                KeyKind::Api,
                Some("alice"),
                &["alert.send on service:portal/inbox_group:infra"],
            );
            let root = sign_in(&node, "root", PASSWORD);
            let alice = sign_in(&node, "alice", PASSWORD);
            let edit = format!("/auth/keys/edit?id={key_id}");
            // Every main page, for the superadmin and for an account with rights.
            let paths = [
                "/",
                "/auth/settings",
                "/auth/settings/services",
                "/auth/settings/activity",
                "/auth/settings/inbox",
                "/auth/settings/new-user",
                "/auth/settings/users/alice",
                "/auth/keys/new?kind=api",
                edit.as_str(),
                "/auth/success",
                "/auth/denied",
                "/auth/nothing-here",
            ];
            let mut shown = 0;
            for (who, session) in [("root", &root), ("alice", &alice)] {
                for path in paths {
                    let reply = page(&node, session, path);
                    assert_ne!(reply.status, 500, "{who} {path}");
                    if reply.status == 200 || reply.status == 404 || reply.status == 403 {
                        assert_clean(&format!("{who} {path}"), &reply.body);
                    }
                    shown += usize::from(reply.status == 200);
                }
            }
            // The superadmin gets ten pages and the account gets at least three.
            assert!(shown >= 13, "only {shown} pages were shown");
            // The refusals and the notices that a page shows after an action.
            for code in ["error", "user-added", "exists"] {
                let path = format!("/auth/settings?s={code}");
                assert_clean(&path, &page(&node, &root, &path).body);
            }
            // The page of the sign-in.
            assert_clean("sign-in", &send(node.port, "GET", LOGIN_PATH, &[], "").body);
            // A server that has another name: its pages show no trace of it in the body.
            let launcher = page(&node, &root, "/");
            assert!(product_words(&launcher.body).is_empty());
        });
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
            let grants = |texts: &[&str]| -> Vec<Grant> {
                texts.iter().map(|t| Grant::parse(t).unwrap()).collect()
            };
            // The owner loses the permission on the group: the key loses it with no wait.
            change_grants(&node, "alice", grants(&[usage]));
            assert_eq!(
                authorizer.decide(&id, "alert.send", "infra", false),
                Decision::Refused {
                    reason: "no_permission"
                }
            );
            // The owner gets it again: the key has it again.
            change_grants(&node, "alice", grants(&[usage, send]));
            assert_eq!(
                authorizer.decide(&id, "alert.send", "infra", false),
                Decision::Allowed
            );
            // The owner loses the right to use the keys of the kind: the key is not valid.
            change_grants(&node, "alice", grants(&[send]));
            assert_eq!(
                authorizer.decide(&id, "alert.send", "infra", false),
                Decision::Refused {
                    reason: "key_not_valid"
                }
            );
            // The right to use the keys of the other kind does not count.
            change_grants(
                &node,
                "alice",
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
                api.body.contains("Calls that these permissions allow")
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
                [
                    "access",
                    "alert.send",
                    "alert.send_user",
                    "alert.read",
                    "question.ask",
                    "question.cancel"
                ]
            );
            // The form of an MCP key holds the services. It offers the two actions of the questions only to an owner that
            // holds one of them: this owner holds none.
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
                change_grants(
                    &node,
                    "alice",
                    vec![
                        Grant::parse("keys.own.read on service:portal").unwrap(),
                        Grant::parse("keys.own.create on service:portal/key_kind:mcp").unwrap(),
                    ],
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
                .contains(r#"<a href="/auth/settings/users/alice?service=db">alice</a>"#));
            let other = page(&node, &root, "/auth/settings/services?svc=%3Cnope%3E");
            assert_eq!(other.status, 200);
            assert!(!other.body.contains("<nope>"));
            // The old page of one service is gone.
            assert_eq!(page(&node, &root, "/auth/settings/services/db").status, 404);
        });
    }
}

// The sign-in form: anti-enumeration and anti-bot. See `docs/login-hardening.md`.

/// A node for the tests of the sign-in form. It has the account `alice`, and `metrics`.
fn sign_in_node(extra: &str) -> Node {
    let node = start_with(
        Arc::new(Backend::Sqlite.fresh()),
        None,
        &format!("metrics {{ enabled = true }} {extra}"),
    );
    insert_account(&node, "alice", "alice-correct-password");
    node
}

/// The same as [`sign_in_node`], with more keys in the block `session`.
fn sign_in_node_with_session(session: &'static str) -> Node {
    let node = start_custom(
        Arc::new(Backend::Sqlite.fresh()),
        None,
        Custom {
            session,
            ..Custom::default()
        },
        "metrics { enabled = true }",
    );
    insert_account(&node, "alice", "alice-correct-password");
    node
}

/// Posts one body to the sign-in route from the local address `source`.
fn post_login(node: &Node, source: &str, csrf: &str, body: &str) -> Reply {
    let origin = format!("http://127.0.0.1:{}", node.port);
    let cookie = format!("{}={csrf}", transport::csrf_cookie_name(false));
    send_from(
        source,
        node.port,
        "POST",
        LOGIN_PATH,
        &[
            ("Origin", &origin),
            ("Content-Type", "application/x-www-form-urlencoded"),
            ("Cookie", &cookie),
        ],
        body,
    )
}

/// The value of the metric line that starts with `start`.
fn metric(node: &Node, start: &str) -> u64 {
    let body = send(node.port, "GET", "/metrics", &[], "").body;
    body.lines()
        .find(|line| line.starts_with(start))
        .unwrap_or_else(|| panic!("{start} is not in the measures:\n{body}"))
        .rsplit(' ')
        .next()
        .unwrap()
        .parse()
        .unwrap()
}

/// The text of a refusal with the values that change at each render (the anti-CSRF token and the
/// token of the form) replaced. Two refusals must be the same text.
fn without_random(node: &Node, body: &str) -> String {
    // Two renders differ in the inert elements, in the order of the attributes and in the order
    // of the head. The canonical form has none of these differences.
    let mut out = node.state.mask.canonical(body);
    for name in ["csrf", FORM_TOKEN_FIELD] {
        // The canonical form sorts the attributes: `name`, `type`, `value`.
        let marker = format!(r#"name="{name}" type="hidden" value=""#);
        if let Some(start) = out.find(&marker) {
            let value_start = start + marker.len();
            let end = value_start + out[value_start..].find('"').unwrap();
            out.replace_range(value_start..end, "X");
        }
    }
    out
}

/// The headers of a reply as a sorted list. The values of `set-cookie` lose the token. The length
/// of the body is not in the list: the inert elements of the page change its size at each render,
/// and the change does not depend on the cause of the refusal.
fn shape_of(reply: &Reply) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = reply
        .headers
        .iter()
        .filter(|(name, _)| name != "date" && name != "content-length")
        .map(|(name, value)| {
            let value = if name == "set-cookie" {
                value.split('=').next().unwrap().to_string()
                    + "="
                    + value.split_once(';').map_or("", |(_, rest)| rest)
            } else {
                value.clone()
            };
            (name.clone(), value)
        })
        .collect();
    out.sort();
    out
}

#[test]
fn an_unknown_login_a_wrong_password_and_a_failed_form_check_get_the_same_refusal() {
    let node = sign_in_node("");
    let csrf = "B".repeat(32);
    // Each case has a login, a password, the age of the token of the form (none: no token) and
    // the value of the trap field.
    let too_long = "a".repeat(300);
    let cases: Vec<(&str, &str, &str, Option<u64>, &str)> = vec![
        (
            "unknown login",
            "nobody",
            "x-wrong-password",
            Some(5_000),
            "",
        ),
        (
            "wrong password of the superadmin",
            "root",
            "x-wrong-password",
            Some(5_000),
            "",
        ),
        (
            "wrong password of an account",
            "alice",
            "x-wrong-password",
            Some(5_000),
            "",
        ),
        (
            "an account with the right password and a filled trap",
            "alice",
            "alice-correct-password",
            Some(5_000),
            "x",
        ),
        (
            "the superadmin with the right password and a filled trap",
            "root",
            PASSWORD,
            Some(5_000),
            "x",
        ),
        (
            "a submit that is too fast",
            "alice",
            "alice-correct-password",
            Some(0),
            "",
        ),
        (
            "no token of the form",
            "alice",
            "alice-correct-password",
            None,
            "",
        ),
        (
            "a login that is far too long",
            &too_long,
            "x-wrong-password",
            Some(5_000),
            "",
        ),
    ];
    let checks_before = metric(&node, "portal_password_checks_total");
    let failures_before = metric(&node, r#"portal_login_attempts_total{result="failure"}"#);
    let mut replies = Vec::new();
    for (index, (name, login, password, age, trap)) in cases.iter().enumerate() {
        let token = age.map_or(String::new(), |age| form_token_issued(&node, &csrf, age));
        let body = login_body_with(&node, &csrf, login, password, &token, trap);
        let reply = post_login(&node, &format!("127.0.1.{}", index + 1), &csrf, &body);
        assert_eq!(reply.status, 401, "{name}: {}", reply.body);
        assert!(reply.body.contains("Invalid login or password"), "{name}");
        replies.push((name, reply));
    }
    // A damaged token and the token of another anti-CSRF token get the same refusal.
    for (index, token) in [
        "AAAA".to_string(),
        form_token_issued(&node, &"C".repeat(32), 5_000),
    ]
    .iter()
    .enumerate()
    {
        let body = login_body_with(&node, &csrf, "alice", "alice-correct-password", token, "");
        let reply = post_login(&node, &format!("127.0.2.{}", index + 1), &csrf, &body);
        assert_eq!(reply.status, 401, "{}", reply.body);
        replies.push((&"a damaged token or a token of another form", reply));
    }
    let first = &replies[0].1;
    for (name, reply) in &replies {
        assert_eq!(
            without_random(&node, &reply.body),
            without_random(&node, &first.body),
            "{name}: the page"
        );
        assert_eq!(shape_of(reply), shape_of(first), "{name}: the headers");
    }
    // Each refusal went through one Argon2 check and counted as a failed sign-in: the time and
    // the budgets are the same for each cause.
    assert_eq!(
        metric(&node, "portal_password_checks_total") - checks_before,
        replies.len() as u64
    );
    assert_eq!(
        metric(&node, r#"portal_login_attempts_total{result="failure"}"#) - failures_before,
        replies.len() as u64
    );
    // Only the failed checks of the form count in their own series: two traps, the fast submit,
    // the missing token, the damaged token and the token of another anti-CSRF token.
    assert_eq!(metric(&node, "portal_login_form_check_failures_total"), 6);
}

/// The body of a sign-in post that goes to `rd` (already signed, as the server signs it).
fn login_body_to(node: &Node, csrf: &str, rd: &str, login: &str, password: &str) -> String {
    let token = form_token_issued(node, csrf, 5_000);
    let signature = sign_return_uri(&node.state.signing_secret, rd).unwrap();
    format!(
        "login={login}&password={password}&csrf={csrf}&rd={}&sig={signature}&{FORM_TOKEN_FIELD}={token}&{TRAP_FIELD}=",
        percent_encode_query_component(rd)
    )
}

/// The page of `GET /auth/login` for the return target `rd` with the signature of the server. `headers` are
/// the headers that a proxy adds.
fn login_page_for(node: &Node, rd: &str, headers: &[(&str, &str)]) -> Reply {
    let signature = sign_return_uri(&node.state.signing_secret, rd).unwrap();
    let path = format!(
        "{LOGIN_PATH}?rd={}&sig={}",
        percent_encode_query_component(rd),
        percent_encode_query_component(&signature)
    );
    send(node.port, "GET", &path, headers, "")
}

#[test]
fn the_band_of_the_sign_in_page_shows_the_destination_only_when_there_is_one() {
    let node = sign_in_node("");
    // No destination: the page has no band, no "Launcher" and no placeholder.
    let plain = send(node.port, "GET", LOGIN_PATH, &[], "").body;
    assert!(Mask::has_element(&plain, "h1", &[]), "{plain}");
    assert!(!plain.contains("Launcher"), "{plain}");
    assert!(!plain.contains("Redirect</span>"), "no band: {plain}");
    let direct = login_page_for(&node, "/", &[]).body;
    assert!(!direct.contains("Redirect</span>"), "{direct}");
    // A destination with a good signature: the band shows the path, without the query and without a host. The
    // destination is on the host of the page, which the address bar shows.
    let page = login_page_for(
        &node,
        "/chdash/x?a=1",
        &[("X-Forwarded-Host", "app.example.com")],
    )
    .body;
    assert!(page.contains("Redirect</span>"), "{page}");
    assert!(
        !page.contains("app.example.com") && !page.contains("signin__host"),
        "{page}"
    );
    assert!(page.contains(">/chdash/x</b>"), "{page}");
    assert!(!page.contains(">/chdash/x?a=1</b>"), "{page}");
    let page = login_page_for(&node, "/chdash/", &[]).body;
    assert!(!page.contains("127.0.0.1"), "{page}");
    // A page of the portal shows its path, like any other page: the band never tells the pages of the product.
    let page = login_page_for(&node, "/auth/settings/services", &[]).body;
    assert!(page.contains(">/auth/settings/services</b>"), "{page}");
    assert!(!page.contains("Settings"), "{page}");
    // A destination with a bad signature is the default: no band.
    let path = format!("{LOGIN_PATH}?rd=%2Fchdash%2F&sig=AAAA");
    let forged = send(node.port, "GET", &path, &[], "").body;
    assert!(!forged.contains("Redirect</span>"), "{forged}");
    assert!(!forged.contains("chdash"), "{forged}");
    // A header that is not a host is not in the page.
    let page = login_page_for(&node, "/chdash/", &[("X-Forwarded-Host", "a\"><script>x")]).body;
    assert!(!page.contains("<script>x"), "{page}");
    assert!(page.contains(">/chdash/</b>"), "{page}");
}

#[test]
fn an_address_with_markup_stays_text_in_the_band() {
    let node = sign_in_node("");
    for rd in [
        "/\"><script>alert(1)</script>",
        "/javascript:alert(1)",
        "/a<img src=x onerror=alert(1)>",
    ] {
        let page = login_page_for(&node, rd, &[("X-Forwarded-Host", "app.example.com")]).body;
        assert!(page.contains("Redirect</span>"), "{rd}");
        assert!(!page.contains("<script>alert"), "{rd}: {page}");
        assert!(!page.contains("<img src=x"), "{rd}: {page}");
        assert!(!page.contains("<a "), "{rd}: {page}");
    }
    let page = login_page_for(&node, "/javascript:alert(1)", &[]).body;
    assert!(page.contains(">/javascript:alert(1)</b>"), "{page}");
}

#[test]
fn a_refusal_with_a_destination_has_the_same_band_for_each_cause() {
    let node = sign_in_node("");
    let csrf = "G".repeat(32);
    let rd = "/chdash/";
    let cases = [
        ("unknown login", "nobody", "x-wrong-password"),
        (
            "wrong password of the superadmin",
            "root",
            "x-wrong-password",
        ),
        ("wrong password of an account", "alice", "x-wrong-password"),
    ];
    let mut replies = Vec::new();
    for (index, (name, login, password)) in cases.iter().enumerate() {
        let body = login_body_to(&node, &csrf, rd, login, password);
        let reply = post_login(&node, &format!("127.0.5.{}", index + 1), &csrf, &body);
        assert_eq!(reply.status, 401, "{name}: {}", reply.body);
        assert!(
            reply.body.contains("Redirect</span>"),
            "{name}: the band stays"
        );
        assert!(reply.body.contains(">/chdash/</b>"), "{name}");
        replies.push((name, reply));
    }
    // The wrong code of a login that is not in the list changes nothing: the band depends on `rd`.
    let first = &replies[0].1;
    for (name, reply) in &replies {
        assert_eq!(
            without_random(&node, &reply.body),
            without_random(&node, &first.body),
            "{name}"
        );
        assert_eq!(shape_of(reply), shape_of(first), "{name}");
    }
}

#[test]
fn the_sign_in_page_has_the_hidden_heading_the_hidden_button_and_no_plain_class() {
    let node = sign_in_node("");
    for rd in ["/", "/chdash/"] {
        let page = login_page_for(&node, rd, &[]).body;
        assert!(page.contains("<title>Authentication</title>"), "{page}");
        assert_eq!(page.matches("<h1").count(), 1, "{page}");
        assert!(page.contains(">Authentication</h1>"), "{page}");
        // The button that shows the password is in the page with the attribute `hidden`.
        let hook = format!("data-{}", node.state.mask.name("data-reveal"));
        let at = page.find(&hook).expect("the button has the mapped hook");
        let tag = &page[page[..at].rfind('<').unwrap()..at + page[at..].find('>').unwrap()];
        assert!(tag.starts_with("<button"), "{tag}");
        assert!(tag.split_whitespace().any(|part| part == "hidden"), "{tag}");
        assert!(!page.contains("data-reveal"), "{page}");
        for plain in [
            "signin",
            "authShell",
            "card",
            "visually-hidden",
            "reveal",
            "keep",
        ] {
            assert!(
                !Mask::names_in_page(&page, false).iter().any(|n| n == plain),
                "{plain} is a class of the page"
            );
        }
        assert!(
            !page.contains("style=") && !page.contains("<script>"),
            "{page}"
        );
        for name in [
            "login",
            "password",
            "csrf",
            "rd",
            "sig",
            FORM_TOKEN_FIELD,
            TRAP_FIELD,
            KEEP_FIELD,
        ] {
            assert!(page.contains(&format!("name=\"{name}\"")), "{name}");
        }
    }
}

#[test]
fn a_refusal_shows_the_message_as_a_strip_after_the_button_and_the_normal_page_has_none() {
    let node = sign_in_node("");
    let alert = [("role", Some("alert"))];
    let normal = login_page_for(&node, "/chdash/", &[]).body;
    assert!(!Mask::has_element(&normal, "p", &alert), "{normal}");
    assert!(!normal.contains("aria-live"), "no empty place: {normal}");
    assert!(!normal.contains("Invalid login or password"), "{normal}");
    let csrf = "F".repeat(32);
    let body = login_body(&node, &csrf, "alice", "x-wrong-password");
    let refused = post_login(&node, "127.0.6.1", &csrf, &body);
    assert_eq!(refused.status, 401, "{}", refused.body);
    let page = refused.body;
    assert!(Mask::has_element(&page, "p", &alert), "{page}");
    assert_eq!(page.matches("role=\"alert\"").count(), 1, "{page}");
    // The strip is the last part of the card: after the button and after the form.
    let strip = page.find("role=\"alert\"").unwrap();
    let form_end = page.find("</form>").unwrap();
    assert!(page[..form_end].contains("type=\"submit\""), "{page}");
    assert!(form_end < strip, "{page}");
    // The variation of the page can wrap the span in an inert element: the canonical form has none.
    let canonical = node.state.mask.canonical(&page);
    let canonical_strip = canonical.find("role=\"alert\"").unwrap();
    assert!(
        canonical[canonical_strip..].contains("Invalid login or password</span></p>"),
        "{canonical}"
    );
    // Both fields point to the strip with the mapped identifier, and the identifier exists.
    let id = node.state.mask.name("signin-msg");
    assert!(Mask::names_in_page(&page, true).contains(&id), "{page}");
    assert_eq!(
        page.matches(&format!("aria-describedby=\"{id}\"")).count(),
        2
    );
    // The card of a page with a strip has its own mapped class, and no plain class.
    let room = node.state.mask.name("signin--strip");
    assert!(Mask::names_in_page(&page, false).contains(&room), "{page}");
    assert!(!Mask::names_in_page(&normal, false).contains(&room));
    assert!(!page.contains("signin"), "{page}");
}

#[test]
fn a_good_sign_in_still_works_with_the_form_token() {
    let node = sign_in_node("");
    let csrf = "D".repeat(32);
    let body = login_body(&node, &csrf, "alice", "alice-correct-password");
    let reply = post_login(&node, "127.0.1.1", &csrf, &body);
    assert_eq!(reply.status, 303, "{}", reply.body);
    assert!(reply.cookie(&session_cookie_name(&node)).is_some());
    assert_eq!(metric(&node, "portal_login_form_check_failures_total"), 0);
}

#[test]
fn a_form_that_stayed_open_for_days_gets_the_page_that_stayed_open_too_long() {
    let node = sign_in_node("");
    let csrf = "E".repeat(32);
    let old = form_token_issued(&node, &csrf, crate::crypto::FORM_MAX_AGE_MS + 1_000);
    let body = login_body_with(&node, &csrf, "alice", "alice-correct-password", &old, "");
    let reply = post_login(&node, "127.0.1.1", &csrf, &body);
    assert_eq!(reply.status, 403);
    assert!(reply.body.contains("The page was open too long"));
    assert!(reply.cookie(&session_cookie_name(&node)).is_none());
}

#[test]
fn a_failed_form_check_uses_the_budget_of_the_failed_sign_ins() {
    let node = sign_in_node(r#"security { login_attempts_per_ip = 3 }"#);
    let csrf = "F".repeat(32);
    let filled = |login: &str, password: &str| {
        let token = form_token_issued(&node, &csrf, 5_000);
        login_body_with(&node, &csrf, login, password, &token, "spam")
    };
    for _ in 0..3 {
        let reply = post_login(
            &node,
            "127.0.1.1",
            &csrf,
            &filled("alice", "alice-correct-password"),
        );
        assert_eq!(reply.status, 401);
    }
    // The address is locked out: even a good form with the good password is refused.
    let good = login_body(&node, &csrf, "alice", "alice-correct-password");
    let reply = post_login(&node, "127.0.1.1", &csrf, &good);
    assert_eq!(reply.status, 429);
    // The refusal of the limiter is the same for a known and an unknown login.
    let unknown = login_body(&node, &csrf, "nobody", "x-wrong-password");
    let other = post_login(&node, "127.0.1.1", &csrf, &unknown);
    assert_eq!(other.status, 429);
    assert_eq!(
        without_random(&node, &other.body),
        without_random(&node, &reply.body)
    );
    assert_eq!(shape_of(&other), shape_of(&reply));
    // Another address is free.
    assert_eq!(post_login(&node, "127.0.1.2", &csrf, &good).status, 303);
}

#[test]
fn the_sign_in_page_names_no_product_and_has_a_trap_that_a_person_cannot_reach() {
    let node = sign_in_node("");
    let page = send(node.port, "GET", LOGIN_PATH, &[], "");
    assert_eq!(page.status, 200);
    let lower = page.body.to_ascii_lowercase();
    for word in ["portal", "inbox", "generator", "<!--", "version"] {
        assert!(!lower.contains(word), "{word}: {}", page.body);
    }
    assert!(page.body.contains("<title>Authentication</title>"));
    for name in ["server", "x-powered-by", "via", "x-generator"] {
        assert!(page.header(name).is_none(), "{name}");
    }
    for (name, value) in &page.headers {
        assert!(
            !value.to_ascii_lowercase().contains("portal"),
            "{name}: {value}"
        );
        assert!(!name.contains("portal"), "{name}");
    }
    // The cookie of the form has a neutral name.
    let cookie = page
        .headers
        .iter()
        .find(|(n, _)| n == "set-cookie")
        .unwrap();
    assert!(cookie.1.starts_with("lc="), "{}", cookie.1);
    // The trap: hidden by a class, out of the tab order, hidden from a screen reader, and not
    // filled by a password manager. The class has a mapped name.
    let mask = &node.state.mask;
    assert!(Mask::has_element(
        &page.body,
        "div",
        &[
            ("class", Some(&mask.name("hp"))),
            ("aria-hidden", Some("true")),
            ("inert", None)
        ]
    ));
    assert!(Mask::has_element(
        &page.body,
        "input",
        &[
            ("type", Some("text")),
            ("name", Some(TRAP_FIELD)),
            ("tabindex", Some("-1")),
            ("autocomplete", Some("off")),
            ("data-lpignore", Some("true")),
            ("data-1p-ignore", Some("true")),
            ("data-bwignore", Some("true")),
            ("value", Some("")),
        ]
    ));
    // The fields of a person keep their names and their autocomplete tokens, for a password manager.
    assert!(Mask::has_element(
        &page.body,
        "input",
        &[
            ("id", Some(&mask.name("login"))),
            ("name", Some("login")),
            ("type", Some("text")),
            ("autocomplete", Some("username")),
        ]
    ));
    assert!(Mask::has_element(
        &page.body,
        "input",
        &[
            ("id", Some(&mask.name("password"))),
            ("name", Some("password")),
            ("type", Some("password")),
            ("autocomplete", Some("current-password")),
        ]
    ));
    // The page loads one stylesheet and one script, with neutral paths. No script is inline.
    let assets = mask.assets();
    assert!(page
        .body
        .contains(&format!(r#"href="{}""#, assets.css_path)));
    assert!(page.body.contains(&format!(r#"src="{}""#, assets.js_path)));
    assert_eq!(page.body.matches("<script").count(), 1);
    let plain = ui::assets();
    for path in [
        &assets.css_path,
        &assets.js_path,
        &plain.css_path,
        &plain.js_path,
        &STYLE_PATH.to_string(),
        &THEME_JS_PATH.to_string(),
    ] {
        let asset = send(node.port, "GET", path, &[], "");
        assert_eq!(asset.status, 200, "{path}");
        assert!(
            !asset.body.to_ascii_lowercase().contains("portal"),
            "{path}"
        );
        assert!(!asset.body.contains("/*"), "{path}");
        assert!(asset.header("server").is_none());
    }
    // ForwardAuth lets the neutral paths pass without a session.
    for path in [
        &assets.css_path,
        &assets.js_path,
        &plain.css_path,
        &plain.js_path,
    ] {
        let reply = send(
            node.port,
            "GET",
            "/auth/verify",
            &[("X-Forwarded-Uri", path), ("X-Forwarded-Method", "GET")],
            "",
        );
        assert_eq!(reply.status, 204, "{path}");
    }
}

/// The value of the attribute `attribute` of the first element `tag` of the page.
fn attribute_of(page: &str, tag: &str, attribute: &str) -> String {
    let start = page.find(&format!("<{tag} ")).unwrap();
    let element = &page[start..start + page[start..].find('>').unwrap()];
    let marker = format!(r#" {attribute}=""#);
    let value = element.find(&marker).unwrap() + marker.len();
    element[value..value + element[value..].find('"').unwrap()].to_string()
}

/// The `href` of the link of the stylesheet. A page after sign-in has a second link, the tab icon, and the pass
/// that masks the page changes the order of the elements of the head.
fn stylesheet_of(page: &str) -> String {
    let marker = r#" rel="stylesheet""#;
    let mut rest = page;
    while let Some(start) = rest.find("<link ") {
        let element = &rest[start..start + rest[start..].find('>').unwrap()];
        if element.contains(marker) {
            let value = element.find(r#" href=""#).unwrap() + r#" href=""#.len();
            return element[value..value + element[value..].find('"').unwrap()].to_string();
        }
        rest = &rest[start + 1..];
    }
    panic!("no stylesheet link in {page}");
}

#[test]
fn the_tab_icon_is_for_the_signed_in_pages_only_and_no_page_body_has_a_logo() {
    let node = sign_in_node("");
    let icon = ui::favicon_path();
    // The sign-in page, masked: no mark, no product word, and the empty icon as its only icon link.
    let login = send(node.port, "GET", LOGIN_PATH, &[], "");
    assert_eq!(login.status, 200);
    // The only icon of a page before sign-in is the empty one.
    assert_eq!(
        login.body.matches("rel=\"icon\"").count(),
        1,
        "{}",
        login.body
    );
    assert_eq!(login.body.matches("href=\"data:,\"").count(), 1);
    assert!(!login.body.contains(icon) && !login.body.contains("image/svg+xml"));
    assert_eq!(
        login.body.matches("<symbol").count(),
        4,
        "the four icons of the sign-in page"
    );
    assert!(!login.body.to_ascii_lowercase().contains("portal"));
    assert_eq!(login.body.matches("<link ").count(), 2);
    // The 404 page and the status pages: the same.
    let missing = send(
        node.port,
        "GET",
        "/auth/nothing-here",
        &[("Accept", "text/html")],
        "",
    );
    assert_eq!(missing.status, 302);
    let root = sign_in(&node, "root", PASSWORD);
    for path in ["/auth/nothing-here", SUCCESS_PATH, DENIED_PATH] {
        let reply = page(&node, &root, path);
        assert!(!reply.body.contains(icon), "{path}");
        assert_eq!(reply.body.matches("href=\"data:,\"").count(), 1, "{path}");
        assert!(!reply.body.contains("<symbol"), "{path}");
        assert_eq!(reply.body.matches("<link ").count(), 2, "{path}");
    }
    // The launcher: the icon link and no logo. The mask keeps the path of the icon, which has no class.
    let home = page(&node, &root, "/");
    assert_eq!(home.status, 200);
    assert!(
        home.body.contains(&format!(r#"href="{icon}""#)),
        "{}",
        home.body
    );
    assert!(home.body.contains("rel=\"icon\"") && home.body.contains("<symbol"));
    assert!(!home.body.contains("<symbol id=\"i-logo\"") && !home.body.contains("i-logo-"));
    assert_eq!(stylesheet_of(&home.body), stylesheet_of(&login.body));
    for name in [
        Mask::names_in_page(&home.body, false),
        Mask::names_in_page(&home.body, true),
    ]
    .concat()
    {
        assert!(![
            "appMark__glyph",
            "appMark__lockup",
            "logoDot",
            "appSep",
            "i-logo"
        ]
        .contains(&name.as_str()));
    }
    assert!(
        !home.body.contains("appMark")
            && !home.body.replace("i-logout", "").contains("i-logo")
            && !home.body.contains("logoDot")
    );
    // A settings page: the icon link too, and no logo.
    let settings = page(&node, &root, "/auth/settings/services");
    assert!(settings.body.contains(&format!(r#"href="{icon}""#)));
    assert!(
        !settings.body.replace("i-logout", "").contains("i-logo")
            && !settings.body.contains("appMark")
    );
    // The file: a signed-in browser gets it with a private cache and the image type. Nobody else does.
    let cookie = format!("{}={root}", session_cookie_name(&node));
    let file = send(node.port, "GET", icon, &[("Cookie", &cookie)], "");
    assert_eq!(file.status, 200);
    assert_eq!(file.header("content-type"), Some("image/svg+xml"));
    assert_eq!(
        file.header("cache-control"),
        Some("private, max-age=31536000, immutable")
    );
    assert_eq!(file.body, ui::FAVICON_SVG);
    // Nobody else gets it. The answer is the one of a path that never existed, with the same status and cache headers.
    let unknown = send(node.port, "GET", "/auth/a/0000000000000000.svg", &[], "");
    assert_eq!(unknown.status, 302);
    for anonymous in [
        send(node.port, "GET", icon, &[], ""),
        send(
            node.port,
            "GET",
            icon,
            &[("Cookie", &format!("{}=garbage", session_cookie_name(&node)))],
            "",
        ),
    ] {
        assert_eq!(anonymous.status, unknown.status);
        assert!(!anonymous.body.contains("svg"));
        assert_eq!(
            anonymous.header("content-type"),
            unknown.header("content-type")
        );
        assert_eq!(
            anonymous.header("cache-control"),
            unknown.header("cache-control")
        );
    }
    // ForwardAuth does not let the icon pass without a session: it is not a neutral asset.
    let verify = send(
        node.port,
        "GET",
        "/auth/verify",
        &[("X-Forwarded-Uri", icon), ("X-Forwarded-Method", "GET")],
        "",
    );
    assert_ne!(verify.status, 204);
}

#[test]
fn no_page_that_a_visitor_without_a_session_can_reach_shows_the_icon_of_the_product() {
    let node = sign_in_node("");
    let icon = ui::favicon_path();
    // The paths that a scraper or a browser asks for, and the pages of the sign-in. Each answer is a redirect, an
    // unknown path or a page. A page has no icon, or the empty one.
    let paths = [
        "/",
        LOGIN_PATH,
        "/auth/login?rd=%2Fchdash%2F",
        "/auth/login/code",
        "/auth/login/enrol",
        DENIED_PATH,
        SUCCESS_PATH,
        "/auth/nothing-here",
        "/favicon.ico",
        "/favicon.svg",
        "/apple-touch-icon.png",
        "/apple-touch-icon-precomposed.png",
        "/browserconfig.xml",
        "/site.webmanifest",
        "/manifest.json",
        icon,
        "/auth/a/0000000000000000.svg",
    ];
    let mut pages = 0;
    for path in paths {
        let reply = send(node.port, "GET", path, &[("Accept", "text/html")], "");
        let kind = reply.header("content-type").unwrap_or_default();
        assert!(!kind.contains("image/"), "{path}: {kind}");
        assert!(
            [200, 302, 303, 404].contains(&reply.status),
            "{path}: {}",
            reply.status
        );
        if reply.status == 302 {
            let to = reply.header("location").unwrap_or_default();
            assert!(to.starts_with(LOGIN_PATH), "{path}: {to}");
        }
        if reply.status == 200 {
            // A page that the visitor can read has the empty icon, and only that.
            pages += 1;
            assert_eq!(reply.body.matches("rel=\"icon\"").count(), 1, "{path}");
            assert_eq!(reply.body.matches("href=\"data:,\"").count(), 1, "{path}");
        }
        assert!(
            !reply.body.contains("image/svg+xml") && !reply.body.contains(icon),
            "{path}"
        );
        assert!(!reply.body.contains("apple-touch"), "{path}");
        let links = reply.body.matches("rel=\"icon\"").count();
        assert!(links <= 1, "{path}");
        assert_eq!(
            links,
            reply.body.matches("href=\"data:,\"").count(),
            "{path}"
        );
    }
    // The sign-in page and its variant with a destination are pages: the other paths are redirects.
    assert!(pages >= 2, "{pages} pages");
    // The icon file itself: the answer of an unknown path, the same for a path that never existed. Both are the
    // redirect to the sign-in page with the path in `rd`, with the same headers.
    let file = send(node.port, "GET", icon, &[], "");
    let none = send(node.port, "GET", "/auth/a/0000000000000000.svg", &[], "");
    assert_eq!(file.status, none.status);
    assert_eq!(file.status, 302);
    assert!(file.body.is_empty() && none.body.is_empty());
    let to = |reply: &Reply| reply.header("location").unwrap_or_default().to_string();
    assert!(
        to(&file).starts_with("/auth/login?rd=%2Fauth%2Fa%2F"),
        "{}",
        to(&file)
    );
    assert!(
        to(&none).starts_with("/auth/login?rd=%2Fauth%2Fa%2F"),
        "{}",
        to(&none)
    );
    assert_eq!(file.header("content-type"), none.header("content-type"));
    let names = |reply: &Reply| {
        let mut names: Vec<String> = reply.headers.iter().map(|(n, _)| n.clone()).collect();
        names.sort();
        names
    };
    assert_eq!(names(&file), names(&none));
    // The policy lets the page show the empty icon and nothing else from a `data:` URL.
    let page = send(node.port, "GET", LOGIN_PATH, &[], "");
    let policy = page.header("content-security-policy").unwrap_or_default();
    assert!(policy.contains("img-src 'self' data:"), "{policy}");
    assert!(policy.contains("default-src 'none';"), "{policy}");
    assert_eq!(policy.matches("data:").count(), 1, "{policy}");
}

/// The text of the `title` element of a page.
fn title_of(page: &str) -> &str {
    let start = page.find("<title>").expect("no title") + "<title>".len();
    &page[start..start + page[start..].find("</title>").unwrap()]
}

#[test]
fn with_favicon_none_no_page_links_an_icon_and_the_icon_route_is_an_unknown_path() {
    let node = sign_in_node(r#"ui { favicon = "none" }"#);
    let icon = ui::favicon_path();
    let root = sign_in(&node, "root", PASSWORD);
    let login = send(node.port, "GET", LOGIN_PATH, &[], "");
    assert_eq!(login.status, 200);
    let mut pages = vec![login];
    for path in ["/", "/auth/settings/services", "/auth/settings"] {
        let reply = page(&node, &root, path);
        assert_eq!(reply.status, 200, "{path}");
        pages.push(reply);
    }
    for (index, reply) in pages.iter().enumerate() {
        // The sign-in page has the empty icon. The pages after sign-in have no icon link at all.
        let blank = usize::from(index == 0);
        assert_eq!(
            reply.body.matches("rel=\"icon\"").count(),
            blank,
            "{}",
            reply.body
        );
        assert!(!reply.body.contains(icon), "{}", reply.body);
        assert_eq!(
            reply.body.matches("<link ").count(),
            blank
                + if reply.body.contains("settings.css") {
                    2
                } else {
                    1
                }
        );
    }
    // The route: a signed-in browser gets the 404 page of an unknown path, not the file.
    let signed = page(&node, &root, icon);
    let unknown = page(&node, &root, "/auth/a/0000000000000000.svg");
    assert_eq!(signed.status, 404);
    assert_eq!(signed.status, unknown.status);
    assert_eq!(title_of(&signed.body), title_of(&unknown.body));
    assert!(!signed.body.contains("1238F0"));
    // A browser that is not signed in gets what an unknown path gets.
    let anonymous = send(node.port, "GET", icon, &[], "");
    let anonymous_unknown = send(node.port, "GET", "/auth/a/0000000000000000.svg", &[], "");
    assert_eq!(anonymous.status, anonymous_unknown.status);
    assert_ne!(anonymous.status, 200);
}

#[test]
fn the_name_of_the_instance_ends_the_title_of_the_pages_after_sign_in_and_nowhere_else() {
    let node = sign_in_node("");
    let root = sign_in(&node, "root", PASSWORD);
    assert_eq!(title_of(&page(&node, &root, "/").body), "Portal");
    assert_eq!(
        title_of(&page(&node, &root, "/auth/settings/services").body),
        "Settings: Services \u{b7} Portal"
    );
    // The sign-in page keeps its neutral title.
    assert_eq!(
        title_of(&send(node.port, "GET", LOGIN_PATH, &[], "").body),
        "Authentication"
    );
    // Another name: the title shows it, escaped, and the body does not.
    let node = start_custom(
        Arc::new(Backend::Sqlite.fresh()),
        None,
        Custom {
            server: r#"name = "Prod <&> Zeta" trusted_proxies = ["127.0.0.1/32"]"#,
            ..Custom::default()
        },
        "",
    );
    let root = sign_in(&node, "root", PASSWORD);
    let home = page(&node, &root, "/");
    assert_eq!(title_of(&home.body), "Prod &lt;&amp;&gt; Zeta");
    let settings = page(&node, &root, "/auth/settings");
    assert_eq!(
        title_of(&settings.body),
        "Settings: Access \u{b7} Prod &lt;&amp;&gt; Zeta"
    );
    for reply in [&home, &settings] {
        let body = &reply.body[reply.body.find("<body").unwrap()..];
        assert!(!body.contains("Zeta") && !body.contains("Prod"), "{body}");
    }
    assert_eq!(
        title_of(&send(node.port, "GET", LOGIN_PATH, &[], "").body),
        "Authentication"
    );
}

#[test]
fn the_pages_of_the_sign_in_and_of_the_status_use_names_that_look_random() {
    let node = sign_in_node("");
    let other = sign_in_node("");
    let login = send(node.port, "GET", LOGIN_PATH, &[], "");
    let again = send(node.port, "GET", LOGIN_PATH, &[], "");
    assert_eq!(login.status, 200);
    // The page has none of the plain classes of the stylesheet, and no plain identifier.
    let plain = crate::ui::mask::names_in_stylesheet(&ui::assets().css);
    assert!(plain.len() > 30, "{}", plain.len());
    let classes = Mask::names_in_page(&login.body, false);
    let names: Vec<String> = [classes.clone(), Mask::names_in_page(&login.body, true)].concat();
    assert!(classes.len() >= 6, "{classes:?}");
    for name in &names {
        assert!(!plain.contains(name), "{name}");
        assert!(!["login", "password"].contains(&name.as_str()), "{name}");
    }
    // The mapped stylesheet that the page links has a rule for each of these names.
    let css_path = stylesheet_of(&login.body);
    let css = send(node.port, "GET", &css_path, &[], "");
    assert_eq!(css.status, 200);
    assert_eq!(
        css.header("cache-control"),
        Some("public, max-age=31536000, immutable")
    );
    let ruled = crate::ui::mask::names_in_stylesheet(&css.body);
    for name in &classes {
        assert!(ruled.contains(name), "{name} has no rule");
    }
    // The script uses the mapped data attributes: none of the plain ones remains.
    let js_path = attribute_of(&login.body, "script", "src");
    let js = send(node.port, "GET", &js_path, &[], "");
    assert_eq!(js.status, 200);
    assert!(!js.body.contains("data-theme"), "{}", js.body);
    // Two renders differ, and two instances with the same secret use the same files and names.
    assert_ne!(login.body, again.body);
    assert_eq!(stylesheet_of(&again.body), css_path);
    let twin = send(other.port, "GET", LOGIN_PATH, &[], "");
    assert_eq!(stylesheet_of(&twin.body), css_path);
    assert_eq!(attribute_of(&twin.body, "script", "src"), js_path);
    assert_eq!(send(other.port, "GET", &css_path, &[], "").body, css.body);
    // The 404 page and the 403 page too.
    let missing = send(
        node.port,
        "GET",
        "/nothing-here",
        &[("Accept", "text/html")],
        "",
    );
    assert_eq!(missing.status, 302);
    assert!(missing.location().unwrap().starts_with(LOGIN_PATH));
    let root = sign_in(&node, "root", PASSWORD);
    let missing = page(&node, &root, "/auth/nothing-here");
    assert_eq!(missing.status, 404);
    let home = page(&node, &root, "/");
    for reply in [&missing, &home] {
        for name in Mask::names_in_page(&reply.body, true) {
            assert!(!plain.contains(&name), "{name}");
        }
        for name in Mask::names_in_page(&reply.body, false) {
            assert!(!plain.contains(&name), "{name}");
            assert!(ruled.contains(&name), "{name} has no rule");
        }
        assert_eq!(stylesheet_of(&reply.body), css_path);
    }
    // The settings pages keep the plain names and the plain files.
    let settings = page(&node, &root, "/auth/settings/services");
    assert_eq!(settings.status, 200);
    assert!(settings
        .body
        .contains(&format!(r#"href="{}""#, ui::assets().css_path)));
    assert!(!settings.body.contains(&css_path));
}

// "Remember me": the box of the sign-in form, the cookie and the session.

/// The binding of a session that a test client opens from the address `source`, with the test User-Agent.
fn client_binding(node: &Node, source: &str) -> Binding {
    let mut headers = HeaderMap::new();
    headers.insert(USER_AGENT, HeaderValue::from_static(AGENT));
    transport::binding(
        node.state.config.session.bind,
        &headers,
        Some(source.parse().unwrap()),
    )
}

/// Posts the sign-in form from `source`. `keep` is the value of the field of the box: `None` sends no field.
fn sign_in_with_box(
    node: &Node,
    source: &str,
    login: &str,
    password: &str,
    keep: Option<&str>,
) -> Reply {
    let csrf = "K".repeat(32);
    let token = form_token_issued(node, &csrf, 5_000);
    let mut body = login_body_with(node, &csrf, login, password, &token, "");
    if let Some(value) = keep {
        body.push_str(&format!("&{KEEP_FIELD}={value}"));
    }
    post_login(node, source, &csrf, &body)
}

/// The `Set-Cookie` line of the session cookie of a reply.
fn session_set_cookie(node: &Node, reply: &Reply) -> String {
    let prefix = format!("{}=", session_cookie_name(node));
    reply
        .headers
        .iter()
        .filter(|(name, _)| name == "set-cookie")
        .map(|(_, value)| value.clone())
        .find(|value| value.starts_with(&prefix))
        .unwrap_or_else(|| panic!("no session cookie in {:?}", reply.headers))
}

#[test]
fn the_cookie_of_a_remembered_session_is_persistent_and_the_others_are_session_cookies() {
    each_backend(|backend| {
        let node = start(Arc::new(backend.fresh()), Some(Duration::ZERO));
        let binding = client_binding(&node, "127.0.0.1");
        let ttl = node.state.config.session.ttl.as_secs();
        let remember_ttl = node.state.config.session.remember.unwrap().ttl.as_secs();
        assert_eq!((ttl, remember_ttl), (86_400, 30 * 86_400), "the defaults");

        // Without the box, also with a value that is not the value of a ticked box.
        for keep in [None, Some("0"), Some("on"), Some("")] {
            let reply = sign_in_with_box(&node, "127.0.0.1", "root", PASSWORD, keep);
            assert_eq!(reply.status, 303, "{keep:?}");
            let cookie = session_set_cookie(&node, &reply);
            assert!(!cookie.contains("Max-Age"), "{keep:?}: {cookie}");
            assert!(!cookie.contains("Expires"), "{keep:?}: {cookie}");
            assert!(
                cookie.contains("HttpOnly") && cookie.contains("SameSite=Strict"),
                "{cookie}"
            );
            let session = node
                .state
                .sessions
                .validate(
                    &reply.cookie(&session_cookie_name(&node)).unwrap(),
                    &binding,
                )
                .unwrap();
            assert!(!session.remembered, "{keep:?}");
            let left = session.expires_at.saturating_sub(unix_now()) as u64;
            assert!((ttl - 2..=ttl + 2).contains(&left), "{keep:?}: {left}");
        }

        // With the box: Max-Age is the life of the session.
        let reply = sign_in_with_box(&node, "127.0.0.1", "root", PASSWORD, Some("1"));
        assert_eq!(reply.status, 303);
        let cookie = session_set_cookie(&node, &reply);
        assert!(
            cookie.contains(&format!("Max-Age={remember_ttl};")),
            "{cookie}"
        );
        assert!(
            cookie.contains("HttpOnly") && cookie.contains("SameSite=Strict"),
            "{cookie}"
        );
        let token = reply.cookie(&session_cookie_name(&node)).unwrap();
        let session = node.state.sessions.validate(&token, &binding).unwrap();
        assert!(session.remembered);
        let left = session.expires_at.saturating_sub(unix_now()) as u64;
        assert!(
            (remember_ttl - 2..=remember_ttl + 2).contains(&left),
            "{left}"
        );
        // The remembered session works on a page, and a logout ends it.
        assert_eq!(verify(&node, &token, AGENT), 204);
        assert_eq!(
            post(&node, &token, LOGOUT_PATH, LOGOUT_TOKEN, "").status,
            303
        );
        assert_eq!(verify(&node, &token, AGENT), 302);
    });
}

#[test]
fn the_box_works_for_an_account_and_follows_the_keys_of_the_file() {
    let node = sign_in_node_with_session(
        r#"remember_ttl = "90d" remember_idle_timeout = "2d" ttl = "12h""#,
    );
    let reply = sign_in_with_box(
        &node,
        "127.0.3.1",
        "alice",
        "alice-correct-password",
        Some("1"),
    );
    assert_eq!(reply.status, 303);
    let cookie = session_set_cookie(&node, &reply);
    assert!(
        cookie.contains(&format!("Max-Age={};", 90 * 86_400)),
        "{cookie}"
    );
    let token = reply.cookie(&session_cookie_name(&node)).unwrap();
    let session = node
        .state
        .sessions
        .validate(&token, &client_binding(&node, "127.0.3.1"))
        .unwrap();
    assert_eq!(session.principal, Principal::User("alice".into()));
    assert!(session.remembered);
    // The session of the same account without the box has the short life.
    let reply = sign_in_with_box(&node, "127.0.3.2", "alice", "alice-correct-password", None);
    let session = node
        .state
        .sessions
        .validate(
            &reply.cookie(&session_cookie_name(&node)).unwrap(),
            &client_binding(&node, "127.0.3.2"),
        )
        .unwrap();
    assert!(!session.remembered);
    let left = session.expires_at - unix_now();
    assert!((12 * 3600 - 2..=12 * 3600 + 2).contains(&left), "{left}");
}

#[test]
fn the_sign_in_page_shows_the_box_with_the_number_of_days_of_the_file() {
    let node = sign_in_node("");
    let page = send(node.port, "GET", LOGIN_PATH, &[], "").body;
    assert!(
        Mask::has_element(
            &page,
            "input",
            &[
                ("type", Some("checkbox")),
                ("name", Some("keep")),
                ("value", Some("1"))
            ]
        ),
        "{page}"
    );
    assert!(
        page.contains("Stay signed in for 30 days</label>"),
        "{page}"
    );
    assert!(
        !page.contains("Remember me") && !page.contains(r#"title="Stay signed in"#),
        "{page}"
    );
    assert!(!page.contains("Keep me"), "{page}");
    assert!(!page.contains("checked"), "the box is not ticked: {page}");
    // The box is a plain control in a label: no inline style, no identifier, no script of its own.
    assert!(!page.contains("style="), "{page}");
    // The class of the label is a mapped name, like all other classes of the page.
    let plain = crate::ui::mask::names_in_stylesheet(&ui::assets().css);
    assert!(plain.contains("keep"));
    assert!(!Mask::names_in_page(&page, false).contains(&"keep".to_string()));
    // Another life, another number.
    let node = sign_in_node_with_session(r#"remember_ttl = "90d" remember_idle_timeout = "7d""#);
    let page = send(node.port, "GET", LOGIN_PATH, &[], "").body;
    assert!(
        page.contains("Stay signed in for 90 days</label>"),
        "{page}"
    );
    assert!(!page.contains("Remember me"), "{page}");
    let node =
        sign_in_node_with_session(r#"ttl = "1h" remember_ttl = "1d" remember_idle_timeout = "1d""#);
    let page = send(node.port, "GET", LOGIN_PATH, &[], "").body;
    assert!(page.contains("Stay signed in for 1 day</label>"), "{page}");
}

#[test]
fn without_the_option_the_page_has_no_box_and_the_server_ignores_the_field() {
    let node = sign_in_node_with_session("remember = false");
    assert_eq!(node.state.config.session.remember, None);
    let page = send(node.port, "GET", LOGIN_PATH, &[], "").body;
    assert!(!page.contains("keep"), "{page}");
    assert!(!page.contains("checkbox"), "{page}");
    assert!(!page.contains("Remember me"), "{page}");
    assert!(!page.contains("Stay signed in"), "{page}");
    // A post with the field is not refused, and its session is a normal one.
    let reply = sign_in_with_box(&node, "127.0.4.1", "root", PASSWORD, Some("1"));
    assert_eq!(reply.status, 303);
    let cookie = session_set_cookie(&node, &reply);
    assert!(!cookie.contains("Max-Age"), "{cookie}");
    let session = node
        .state
        .sessions
        .validate(
            &reply.cookie(&session_cookie_name(&node)).unwrap(),
            &client_binding(&node, "127.0.4.1"),
        )
        .unwrap();
    assert!(!session.remembered);
    let left = session.expires_at - unix_now();
    assert!((86_400 - 2..=86_400 + 2).contains(&left), "{left}");
    // Even a value that is not "1" is not refused.
    assert_eq!(
        sign_in_with_box(&node, "127.0.4.2", "root", PASSWORD, Some("zzz")).status,
        303
    );
}

#[test]
fn a_refused_sign_in_gives_the_same_answer_with_the_box_ticked_or_not() {
    let node = sign_in_node("");
    let csrf = "B".repeat(32);
    let mut replies = Vec::new();
    let mut index = 0;
    // The same causes as the test of the refusals, each with and without the box.
    for (login, password) in [
        ("nobody", "x-wrong-password"),
        ("root", "x-wrong-password"),
        ("alice", "x-wrong-password"),
    ] {
        for keep in [None, Some("1")] {
            index += 1;
            let token = form_token_issued(&node, &csrf, 5_000);
            let mut body = login_body_with(&node, &csrf, login, password, &token, "");
            if let Some(value) = keep {
                body.push_str(&format!("&{KEEP_FIELD}={value}"));
            }
            let reply = post_login(&node, &format!("127.0.5.{index}"), &csrf, &body);
            assert_eq!(reply.status, 401, "{login} {keep:?}");
            assert!(reply.body.contains("Invalid login or password"));
            // The page after the refusal does not keep the box ticked.
            assert!(!reply.body.contains("checked"), "{login} {keep:?}");
            assert!(!reply
                .headers
                .iter()
                .any(|(name, value)| name == "set-cookie"
                    && value.starts_with(&format!("{}=", session_cookie_name(&node)))));
            replies.push(reply);
        }
    }
    // A failed check of the form with the box ticked is the same refusal.
    let token = form_token_issued(&node, &csrf, 0);
    let body = format!(
        "{}&{KEEP_FIELD}=1",
        login_body_with(&node, &csrf, "alice", "alice-correct-password", &token, "")
    );
    replies.push(post_login(&node, "127.0.5.99", &csrf, &body));
    assert_eq!(replies.last().unwrap().status, 401);
    let first = &replies[0];
    for reply in &replies {
        assert_eq!(reply.status, first.status);
        assert_eq!(shape_of(reply), shape_of(first));
        assert_eq!(
            without_random(&node, &reply.body),
            without_random(&node, &first.body)
        );
    }
}

#[test]
fn an_impersonated_session_never_gets_the_remembered_rule() {
    each_backend(|backend| {
        let node = start(Arc::new(backend.fresh()), Some(Duration::ZERO));
        let binding = client_binding(&node, "127.0.0.1");
        let reply = sign_in_with_box(&node, "127.0.0.1", "root", PASSWORD, Some("1"));
        let root = reply.cookie(&session_cookie_name(&node)).unwrap();
        assert!(
            node.state
                .sessions
                .validate(&root, &binding)
                .unwrap()
                .remembered
        );
        let _alice = user_without_grant(&node, &root, "alice");
        let reply = post(&node, &root, IMPERSONATE_PATH, USERS_TOKEN, "login=alice");
        assert_eq!(reply.status, 303);
        let as_alice = reply.cookie(&session_cookie_name(&node)).unwrap();
        let session = node.state.sessions.validate(&as_alice, &binding).unwrap();
        assert!(session.impersonated && !session.remembered);
        let left = session.expires_at - unix_now();
        assert!(left <= 86_400, "the short rule: {left}");
        let cookie = session_set_cookie(&node, &reply);
        let max_age: u64 = cookie
            .split("; ")
            .find_map(|part| part.strip_prefix("Max-Age="))
            .unwrap()
            .parse()
            .unwrap();
        assert!(max_age <= 86_400, "{cookie}");
        // The way back is not remembered either.
        let reply = post(&node, &as_alice, IMPERSONATE_END_PATH, USERS_TOKEN, "");
        let back = reply.cookie(&session_cookie_name(&node)).unwrap();
        assert!(
            !node
                .state
                .sessions
                .validate(&back, &binding)
                .unwrap()
                .remembered
        );
    });
}

/// The second factor over HTTP: the two steps of the sign-in, the forced enrolment, the section "Security", the reset by the
/// superadmin, and what the second factor leaves unchanged.
mod mfa_tests {
    use super::*;
    use crate::totp;

    const ALICE: &str = "alice-correct-password";
    /// The address of the requests. The sessions of the sign-ins are bound to it, and `verify` and `page` use the same one.
    const SOURCE: &str = "127.0.0.1";

    /// A node with the account `alice` (database) and the policy `policy`. `extra` is more configuration.
    fn mfa_node(backend: &Backend, policy: &'static str, extra: &str) -> Node {
        let store = Arc::new(backend.fresh());
        let node = start_custom(
            store,
            None,
            Custom {
                mfa: policy,
                env: vec![("FRED_HASH".into(), password_hash("fred-correct-password"))],
                ..Custom::default()
            },
            &format!("metrics {{ enabled = true }} {extra}"),
        );
        insert_account(&node, "alice", ALICE);
        node
    }

    fn file_account_extra() -> String {
        r#"access { grants_mode = "hybrid" } user "fred" { grants = [] password_hash_env = "FRED_HASH" }"#
            .to_string()
    }

    /// Turns the second factor of `login` on, as the page does. It returns the secret and the recovery codes.
    fn enrol(node: &Node, login: &str) -> (Vec<u8>, Vec<String>) {
        let mfa = node.state.mfa.as_ref().expect("the second factor is on");
        let now = unix_now();
        let page = mfa.begin(login, "Portal", now).unwrap();
        let secret = totp::base32_decode(&page.grouped.replace(' ', "")).unwrap();
        // The code of the confirmation is the code of the step before: the codes of the next steps stay free.
        let code = totp::code_for_step(&secret, base_step(&secret) - 1, totp::DIGITS);
        match mfa
            .confirm(&node.state.store, login, &page.token, &code, now)
            .unwrap()
        {
            crate::mfa::Enrolled::Done(codes) => (secret, codes),
            other => panic!("enrolment: {other:?}"),
        }
    }

    /// The code of the app at the current step plus `offset` steps. The window of the server holds -1, 0 and +1.
    fn code(secret: &[u8], offset: i64) -> String {
        totp::code_for_step(secret, base_step(secret) + offset, totp::DIGITS)
    }

    thread_local! {
        static BASE_STEPS: std::cell::RefCell<Vec<(Vec<u8>, i64)>> = const { std::cell::RefCell::new(Vec::new()) };
    }

    /// The step of the clock at the first use of `secret` in the test. The codes of an account stay at fixed offsets
    /// from it, so a step of 30 seconds that ends in the middle of the test does not turn a good code into a replay.
    /// Each secret has its own base. A test that runs on two backends enrols a new account on each of them, and the
    /// clock of the second run is later than the clock of the first.
    fn base_step(secret: &[u8]) -> i64 {
        BASE_STEPS.with(|bases| {
            let mut bases = bases.borrow_mut();
            if let Some((_, step)) = bases.iter().find(|(known, _)| known == secret) {
                return *step;
            }
            let step = totp::step_of(unix_now());
            bases.push((secret.to_vec(), step));
            step
        })
    }

    /// The value of the hidden field `name` in a page, whatever the order of the attributes.
    fn hidden_value(body: &str, name: &str) -> String {
        let marker = format!(r#"name="{name}""#);
        for (at, _) in body.match_indices(&marker) {
            let start = body[..at].rfind('<').unwrap();
            let end = at + body[at..].find('>').unwrap();
            let tag = &body[start..end];
            if tag.contains(r#"type="hidden""#) {
                let value = tag.split_once(r#"value=""#).expect("a value").1;
                return value.split('"').next().unwrap().replace("&amp;", "&");
            }
        }
        panic!("no hidden field {name} in {body}")
    }

    /// The first step, as a browser posts it. It returns the reply and the anti-CSRF token that the page uses.
    fn step_one(node: &Node, login: &str, password: &str, keep: bool) -> (Reply, String) {
        let csrf = "M".repeat(32);
        let mut body = login_body(node, &csrf, login, password);
        if keep {
            body.push_str("&keep=1");
        }
        (post_login(node, SOURCE, &csrf, &body), csrf)
    }

    /// A post of the second step with the body that the caller gives.
    fn post_step(node: &Node, path: &str, csrf: &str, body: &str, agent: &str) -> Reply {
        let origin = format!("http://127.0.0.1:{}", node.port);
        let cookie = format!("{}={csrf}", transport::csrf_cookie_name(false));
        send_from(
            SOURCE,
            node.port,
            "POST",
            path,
            &[
                ("Origin", &origin),
                ("Content-Type", "application/x-www-form-urlencoded"),
                ("Cookie", &cookie),
                ("User-Agent", agent),
            ],
            body,
        )
    }

    /// The body of the form of the page `page`, with the code `code`.
    fn code_body(page: &Reply, csrf: &str, code: &str) -> String {
        format!(
            "csrf={csrf}&rd={}&sig={}&pending={}&code={code}",
            percent_encode_query_component(&hidden_value(&page.body, "rd")),
            hidden_value(&page.body, "sig"),
            hidden_value(&page.body, "pending"),
        )
    }

    fn second(node: &Node, page: &Reply, csrf: &str, code: &str) -> Reply {
        post_step(
            node,
            ui::mfa::CODE_PATH,
            csrf,
            &code_body(page, csrf, code),
            AGENT,
        )
    }

    /// The history of the sessions of `login`: the flag `mfa` of each, newest first.
    fn mfa_flags(node: &Node, login: &str) -> Vec<bool> {
        let mut flags = Vec::new();
        wait_until(Duration::from_secs(10), || {
            flags = node
                .state
                .store
                .snapshot(&[(login.to_string(), false)], unix_now())
                .unwrap()
                .accounts[0]
                .recent
                .iter()
                .map(|s| s.mfa)
                .collect();
            !flags.is_empty()
        });
        flags
    }

    fn audit_kinds(node: &Node) -> Vec<String> {
        node.state
            .store
            .snapshot(&[], unix_now())
            .unwrap()
            .recent_events
            .into_iter()
            .map(|e| e.kind)
            .collect()
    }

    #[test]
    fn an_account_with_a_second_factor_signs_in_in_two_steps_on_any_instance() {
        each_backend(|backend| {
            let (a, b) = match backend {
                Backend::Sqlite => {
                    let node = mfa_node(backend, "optional", "");
                    (node.clone(), node)
                }
                Backend::Postgres(_) => {
                    let (first, second) = backend.fresh_pair();
                    let custom = || Custom {
                        mfa: "optional",
                        ..Custom::default()
                    };
                    let a = start_custom(Arc::new(first), None, custom(), "");
                    let b = start_custom(Arc::new(second), None, custom(), "");
                    insert_account(&a, "alice", ALICE);
                    (a, b)
                }
            };
            let (secret, _) = enrol(&a, "alice");
            let (page, csrf) = step_one(&a, "alice", ALICE, true);
            assert_eq!(page.status, 200, "{}", page.body);
            assert!(
                page.cookie(&session_cookie_name(&a)).is_none(),
                "no session yet"
            );
            assert!(page.body.contains(ui::mfa::CODE_PATH));
            assert!(page
                .header("cache-control")
                .is_some_and(|v| v.contains("no-store")));
            // The second step goes to the other instance: the state is in the token.
            let reply = second(&b, &page, &csrf, &code(&secret, 0));
            assert_eq!(reply.status, 303, "{}", reply.body);
            let session = reply
                .cookie(&session_cookie_name(&b))
                .expect("a session cookie");
            let cookie = reply.header("set-cookie").unwrap();
            assert!(
                cookie.contains("Max-Age"),
                "the box Remember me survives the second step"
            );
            assert_eq!(verify(&b, &session, AGENT), 204);
            assert_eq!(verify(&a, &session, AGENT), 204);
            // The history records the second factor.
            assert_eq!(mfa_flags(&a, "alice"), [true]);
            // The same code is refused at once on both instances: a code is good once.
            let (page, csrf) = step_one(&a, "alice", ALICE, false);
            for node in [&a, &b] {
                let replay = second(node, &page, &csrf, &code(&secret, 0));
                assert_eq!(replay.status, 401, "{}", replay.body);
                assert!(replay.body.contains("Invalid code"));
            }
            let reply = second(&b, &page, &csrf, &code(&secret, 1));
            assert_eq!(
                reply.status, 303,
                "the next step of the clock is good: {}",
                reply.body
            );
        });
    }

    #[test]
    fn the_superadmin_and_an_account_of_the_file_have_the_second_factor_too() {
        let node = mfa_node(&Backend::Sqlite, "optional", &file_account_extra());
        for (login, password) in [("root", PASSWORD), ("fred", "fred-correct-password")] {
            let (secret, _) = enrol(&node, login);
            let (page, csrf) = step_one(&node, login, password, false);
            assert_eq!(page.status, 200, "{login}: {}", page.body);
            let reply = second(&node, &page, &csrf, &code(&secret, 0));
            assert_eq!(reply.status, 303, "{login}: {}", reply.body);
            assert!(reply.cookie(&session_cookie_name(&node)).is_some());
        }
        // Alice has none: the sign-in is the sign-in of before.
        let session = sign_in(&node, "alice", ALICE);
        assert_eq!(verify(&node, &session, AGENT), 204);
        assert_eq!(mfa_flags(&node, "alice"), [false]);
    }

    #[test]
    fn a_wrong_code_a_used_code_and_a_recovery_code_are_answered_as_they_must() {
        let node = mfa_node(&Backend::Sqlite, "optional", "");
        let (secret, recovery) = enrol(&node, "alice");
        let failures = |n: &Node| metric(n, r#"portal_login_attempts_total{result="mfa_failure"}"#);
        let (page, csrf) = step_one(&node, "alice", ALICE, false);
        let wrong = second(&node, &page, &csrf, "000000");
        assert_eq!(wrong.status, 401);
        assert!(wrong.body.contains("Invalid code") && wrong.body.contains(r#"role="alert""#));
        assert!(wrong.cookie(&session_cookie_name(&node)).is_none());
        assert_eq!(failures(&node), 1);
        // A new token counts the try, and it works with a good code.
        let again = second(&node, &wrong, &csrf, &code(&secret, 0));
        assert_eq!(again.status, 303, "{}", again.body);
        // A recovery code, typed in capitals, works once.
        let (page, csrf) = step_one(&node, "alice", ALICE, false);
        let typed = recovery[0].to_uppercase();
        let ok = second(&node, &page, &csrf, &typed);
        assert_eq!(ok.status, 303, "{}", ok.body);
        let (page, csrf) = step_one(&node, "alice", ALICE, false);
        let used = second(&node, &page, &csrf, &recovery[0]);
        assert_eq!(used.status, 401);
        wait_until(Duration::from_secs(10), || {
            audit_kinds(&node).contains(&"recovery_used".to_string())
        });
        assert!(audit_kinds(&node).contains(&"mfa_failed".to_string()));
        // A wrong password never reaches the second step.
        let (page, _) = step_one(&node, "alice", "x-wrong-password", false);
        assert_eq!(page.status, 401);
        assert!(page.body.contains("Invalid login or password"));
    }

    #[test]
    fn five_wrong_codes_on_the_account_lock_it_for_a_while_and_the_lock_has_an_end() {
        let node = mfa_node(&Backend::Sqlite, "optional", "");
        let (secret, _) = enrol(&node, "alice");
        let (mut page, csrf) = step_one(&node, "alice", ALICE, false);
        for n in 1..crate::mfa::MAX_TRIES {
            page = second(&node, &page, &csrf, "000000");
            assert_eq!(page.status, 401, "try {n}: {}", page.body);
            assert!(page.body.contains(ui::mfa::CODE_PATH));
        }
        // The fifth wrong code is the fifth failure of the account: it locks the account, and the answer is 429.
        let last = second(&node, &page, &csrf, "000000");
        assert_eq!(last.status, 429, "{}", last.body);
        assert!(last.header("retry-after").is_some());
        // The account is locked, also for a good code. The lock is in the database, so every instance sees it.
        let locked = second(&node, &page, &csrf, &code(&secret, 0));
        assert_eq!(locked.status, 429);
        assert!(locked.cookie(&session_cookie_name(&node)).is_none());
        assert!(
            node.state
                .store
                .totp_get("alice")
                .unwrap()
                .unwrap()
                .locked_until
                > unix_now(),
            "the lock has an end"
        );
        assert!(
            metric(
                &node,
                r#"portal_login_attempts_total{result="mfa_failure"}"#
            ) >= 5
        );
    }

    #[test]
    fn a_pending_token_that_took_its_last_wrong_code_sends_the_person_back_to_the_password() {
        let node = mfa_node(&Backend::Sqlite, "optional", "");
        enrol(&node, "alice");
        let (page, csrf) = step_one(&node, "alice", ALICE, false);
        let mut headers = HeaderMap::new();
        headers.insert(USER_AGENT, HeaderValue::from_static(AGENT));
        let binding = transport::binding(
            node.state.config.session.bind,
            &headers,
            Some(SOURCE.parse().unwrap()),
        );
        let pending = hidden_value(&page.body, "pending");
        // The token of the page holds the proof of the password step: the forged tokens keep it.
        let opened = crate::mfa::open_pending(
            &node.state.signing_secret,
            &pending,
            &binding,
            &csrf,
            unix_now(),
        )
        .unwrap();
        let with_tries = |tries: u8| {
            crate::mfa::sign_pending(
                &node.state.signing_secret,
                &crate::mfa::Pending {
                    stage: crate::mfa::Stage::Code,
                    principal: Principal::User("alice".into()),
                    keep: false,
                    tries,
                    issued: unix_now(),
                    proof: opened.proof,
                },
                &binding,
                &csrf,
            )
            .unwrap()
        };
        let body =
            |tries: u8| code_body(&page, &csrf, "000000").replace(&pending, &with_tries(tries));
        let almost = post_step(
            &node,
            ui::mfa::CODE_PATH,
            &csrf,
            &body(crate::mfa::MAX_TRIES - 2),
            AGENT,
        );
        assert_eq!(almost.status, 401);
        assert!(almost.body.contains(ui::mfa::CODE_PATH), "one try is left");
        let last = post_step(
            &node,
            ui::mfa::CODE_PATH,
            &csrf,
            &body(crate::mfa::MAX_TRIES - 1),
            AGENT,
        );
        assert_eq!(last.status, 401);
        assert!(
            last.body.contains("Too many wrong codes") && last.body.contains("name=\"password\"")
        );
    }

    #[test]
    fn the_address_has_its_own_budget_of_wrong_codes() {
        let node = mfa_node(
            &Backend::Sqlite,
            "optional",
            r#"security { login_attempts_per_ip = 3 }"#,
        );
        enrol(&node, "alice");
        let (page, csrf) = step_one(&node, "alice", ALICE, false);
        let mut last = second(&node, &page, &csrf, "000000");
        for _ in 0..2 {
            last = second(&node, &page, &csrf, "000000");
        }
        assert_eq!(last.status, 401, "three failures count");
        let blocked = second(&node, &page, &csrf, "000000");
        assert_eq!(blocked.status, 429, "{}", blocked.body);
        assert!(blocked.header("retry-after").is_some());
    }

    #[test]
    fn a_pending_token_that_is_changed_old_or_used_by_another_client_gives_the_page_open_too_long()
    {
        let node = mfa_node(&Backend::Sqlite, "optional", "");
        let (secret, _) = enrol(&node, "alice");
        let (page, csrf) = step_one(&node, "alice", ALICE, false);
        let good = code(&secret, 0);
        let stale = |reply: &Reply, why: &str| {
            assert_eq!(reply.status, 403, "{why}: {}", reply.body);
            assert!(reply.body.contains("The page was open too long"), "{why}");
            assert!(reply.cookie(&session_cookie_name(&node)).is_none(), "{why}");
        };
        // Another browser: the token is bound to the client.
        let other = post_step(
            &node,
            ui::mfa::CODE_PATH,
            &csrf,
            &code_body(&page, &csrf, &good),
            "Other/1.0",
        );
        stale(&other, "another client");
        // Another page: the anti-CSRF token is part of the signature.
        let other_csrf = "N".repeat(32);
        let reply = post_step(
            &node,
            ui::mfa::CODE_PATH,
            &other_csrf,
            &code_body(&page, &other_csrf, &good),
            AGENT,
        );
        stale(&reply, "another page");
        // A changed token and a token of the other stage.
        let pending = hidden_value(&page.body, "pending");
        let broken = code_body(&page, &csrf, &good).replace(&pending, &format!("x{pending}"));
        stale(
            &post_step(&node, ui::mfa::CODE_PATH, &csrf, &broken, AGENT),
            "a changed token",
        );
        let to_enrol = post_step(
            &node,
            ui::mfa::ENROL_PATH,
            &csrf,
            &code_body(&page, &csrf, &good),
            AGENT,
        );
        stale(&to_enrol, "the token of the other stage");
        // An old token: the server signs a token that was issued 6 minutes ago, for this client and this page.
        let mut headers = HeaderMap::new();
        headers.insert(USER_AGENT, HeaderValue::from_static(AGENT));
        let binding = transport::binding(
            node.state.config.session.bind,
            &headers,
            Some(SOURCE.parse().unwrap()),
        );
        let proof = crate::mfa::open_pending(
            &node.state.signing_secret,
            &pending,
            &binding,
            &csrf,
            unix_now(),
        )
        .unwrap()
        .proof;
        let old = crate::mfa::sign_pending(
            &node.state.signing_secret,
            &crate::mfa::Pending {
                stage: crate::mfa::Stage::Code,
                principal: Principal::User("alice".into()),
                keep: false,
                tries: 0,
                issued: unix_now() - crate::mfa::PENDING_TTL_SECS - 60,
                proof,
            },
            &binding,
            &csrf,
        )
        .unwrap();
        let body = code_body(&page, &csrf, &good).replace(&pending, &old);
        stale(
            &post_step(&node, ui::mfa::CODE_PATH, &csrf, &body, AGENT),
            "an old token",
        );
        // The same body with the right token works: the checks above were the only obstacle.
        let reply = second(&node, &page, &csrf, &good);
        assert_eq!(reply.status, 303, "{}", reply.body);
    }

    #[test]
    fn a_wrong_password_looks_the_same_for_an_account_with_and_without_a_second_factor() {
        let node = mfa_node(&Backend::Sqlite, "optional", "");
        insert_account(&node, "bob", "bob-correct-password");
        enrol(&node, "alice");
        let csrf = "M".repeat(32);
        let mut replies = Vec::new();
        for (index, login) in ["alice", "bob", "nobody", "root"].iter().enumerate() {
            let body = login_body(&node, &csrf, login, "x-wrong-password");
            let reply = post_login(&node, &format!("127.0.3.{}", index + 1), &csrf, &body);
            assert_eq!(reply.status, 401, "{login}");
            replies.push(reply);
        }
        for reply in &replies[1..] {
            assert_eq!(
                without_random(&node, &reply.body),
                without_random(&node, &replies[0].body)
            );
            assert_eq!(shape_of(reply), shape_of(&replies[0]));
        }
    }

    #[test]
    fn the_pages_of_the_second_step_have_no_plain_class_name_and_no_inline_script_or_style() {
        let required = mfa_node(&Backend::Sqlite, "required", "");
        let (enrol_page, _) = step_one(&required, "alice", ALICE, false);
        assert_eq!(enrol_page.status, 200, "{}", enrol_page.body);
        let optional = mfa_node(&Backend::Sqlite, "optional", "");
        enrol(&optional, "alice");
        let (code_page, _) = step_one(&optional, "alice", ALICE, false);
        let plain = [
            "otp",
            "altCode",
            "mfaScan",
            "qr",
            "mfaSecret",
            "mfaBox",
            "mfaCopy",
            "copyBtn",
            "mfaLabel",
            "card",
            "authShell",
            "button",
            "error",
            "field--first",
        ];
        for (name, body) in [
            ("the enrolment", &enrol_page.body),
            ("the code", &code_page.body),
        ] {
            let mut classes = 0;
            for chunk in body.split("class=\"").skip(1) {
                for token in chunk.split('"').next().unwrap().split_whitespace() {
                    classes += 1;
                    assert!(
                        !plain.contains(&token),
                        "{name}: the plain class {token} is in the page"
                    );
                }
            }
            assert!(classes > 3, "{name}: the page has classes");
            assert!(
                !body.contains("<script>") && !body.contains("style="),
                "{name}"
            );
            assert!(!body.contains("<img"), "{name}: the QR code is markup");
        }
        assert!(enrol_page.body.contains("<svg") && !enrol_page.body.contains("otpauth"));
        assert_eq!(secret_of(&enrol_page.body).len(), 20);
    }

    /// The secret of an enrolment page, as the app takes it: the text of its only `code` element, without spaces.
    fn secret_of(body: &str) -> Vec<u8> {
        let start = body.find("<code").expect("no secret in the page");
        let start = start + body[start..].find('>').unwrap() + 1;
        let text = body[start..start + body[start..].find("</code>").unwrap()].replace(' ', "");
        totp::base32_decode(&text).unwrap()
    }

    #[test]
    fn the_pages_of_the_second_factor_never_name_the_product_and_never_print_the_link_of_the_app() {
        use crate::ui::body_words::{assert_clean, product_words};
        // The forced enrolment: a card before the session starts.
        let node = mfa_node(&Backend::Sqlite, "required", "");
        let (card, _) = step_one(&node, "alice", ALICE, false);
        assert_eq!(card.status, 200);
        assert_clean("forced enrolment", &card.body);
        assert!(!card.body.contains("otpauth") && !card.body.contains("issuer="));
        // The same page under another name: the body has no trace of the product or of the name.
        let named = start_custom(
            Arc::new(Backend::Sqlite.fresh()),
            None,
            Custom {
                mfa: "required",
                server: r#"name = "Prod" trusted_proxies = ["127.0.0.1/32"]"#,
                ..Custom::default()
            },
            "",
        );
        insert_account(&named, "alice", ALICE);
        let (card, _) = step_one(&named, "alice", ALICE, false);
        assert!(product_words(&card.body).is_empty(), "{}", card.body);
        assert!(!card.body.contains("Prod"), "{}", card.body);
        // The pages of the settings, with the policy optional.
        let node = mfa_node(&Backend::Sqlite, "optional", "");
        let session = sign_in(&node, "alice", ALICE);
        for path in [
            "/auth/settings/security",
            "/auth/settings/security?enable=1",
        ] {
            let reply = page(&node, &session, path);
            assert_eq!(reply.status, 200, "{path}");
            assert_clean(path, &reply.body);
        }
    }

    #[test]
    fn with_the_policy_required_an_account_without_a_second_factor_enrols_before_its_session_starts(
    ) {
        let node = mfa_node(&Backend::Sqlite, "required", "");
        let (page, csrf) = step_one(&node, "alice", ALICE, false);
        assert_eq!(page.status, 200, "{}", page.body);
        assert!(page.body.contains(ui::mfa::ENROL_PATH) && page.body.contains("<svg"));
        assert!(
            page.cookie(&session_cookie_name(&node)).is_none(),
            "no session before the enrolment"
        );
        let enrol_token = hidden_value(&page.body, "enrol");
        // The secret is in the page as text, in groups of four characters.
        let secret = secret_of(&page.body);
        let body = |code: &str| {
            format!(
                "csrf={csrf}&rd={}&sig={}&pending={}&enrol={enrol_token}&code={code}",
                percent_encode_query_component(&hidden_value(&page.body, "rd")),
                hidden_value(&page.body, "sig"),
                hidden_value(&page.body, "pending"),
            )
        };
        let wrong = post_step(&node, ui::mfa::ENROL_PATH, &csrf, &body("000000"), AGENT);
        assert_eq!(wrong.status, 401, "{}", wrong.body);
        assert!(wrong.body.contains("<svg"), "the same secret again");
        assert!(wrong.cookie(&session_cookie_name(&node)).is_none());
        let done = post_step(
            &node,
            ui::mfa::ENROL_PATH,
            &csrf,
            &body(&code(&secret, -1)),
            AGENT,
        );
        assert_eq!(done.status, 200, "{}", done.body);
        let session = done
            .cookie(&session_cookie_name(&node))
            .expect("the session starts");
        assert!(
            done.body.contains("textarea")
                && done.header("cache-control").unwrap().contains("no-store")
        );
        assert!(done.body.matches('-').count() >= 10, "ten recovery codes");
        assert_eq!(verify(&node, &session, AGENT), 204);
        assert_eq!(mfa_flags(&node, "alice"), [true]);
        assert!(node.state.store.totp_get("alice").unwrap().is_some());
        // The next sign-in asks for a code, and the code of the enrolment is used up.
        let (page, csrf) = step_one(&node, "alice", ALICE, false);
        assert!(page.body.contains(ui::mfa::CODE_PATH));
        assert_eq!(second(&node, &page, &csrf, &code(&secret, -1)).status, 401);
        assert_eq!(second(&node, &page, &csrf, &code(&secret, 1)).status, 303);
        // The superadmin is under the same rule.
        let (page, _) = step_one(&node, "root", PASSWORD, false);
        assert!(page.body.contains(ui::mfa::ENROL_PATH));
    }

    #[test]
    fn with_the_policy_off_nothing_changes_and_nothing_is_shown() {
        let node = mfa_node(&Backend::Sqlite, "off", "");
        assert!(node.state.mfa.is_none());
        let session = sign_in(&node, "alice", ALICE);
        let home = page(&node, &session, "/");
        assert!(
            !home.body.contains("/auth/settings"),
            "no gear for an account with nothing to set"
        );
        assert_eq!(page(&node, &session, ui::mfa::SECURITY_PATH).status, 404);
        let origin = format!("http://127.0.0.1:{}", node.port);
        let csrf = "M".repeat(32);
        let cookie = format!("{}={csrf}", transport::csrf_cookie_name(false));
        let reply = send(
            node.port,
            "POST",
            ui::mfa::CODE_PATH,
            &[
                ("Origin", &origin),
                ("Content-Type", "application/x-www-form-urlencoded"),
                ("Cookie", &cookie),
            ],
            "code=1",
        );
        assert_eq!(reply.status, 404);
        let root = sign_in(&node, "root", PASSWORD);
        let access = page(&node, &root, "/auth/settings");
        assert!(!access.body.contains(">Security<"), "no tab");
    }

    #[test]
    fn an_account_turns_the_second_factor_on_renews_the_recovery_codes_and_turns_it_off_on_the_page(
    ) {
        let node = mfa_node(&Backend::Sqlite, "optional", "");
        let session = sign_in(&node, "alice", ALICE);
        let home = page(&node, &session, "/");
        assert!(
            home.body.contains(ui::mfa::SECURITY_PATH),
            "the gear opens the page Security"
        );
        let off = page(&node, &session, ui::mfa::SECURITY_PATH);
        assert_eq!(off.status, 200);
        assert!(off.body.contains("Turn on") && !off.body.contains("Recovery codes left"));
        // The enrolment page: the QR code, the secret and the link as text.
        let enrolment = page(
            &node,
            &session,
            &format!("{}?enable=1", ui::mfa::SECURITY_PATH),
        );
        assert!(
            enrolment.body.contains("<svg class=\"qr\"") && !enrolment.body.contains("otpauth")
        );
        let token = hidden_value(&enrolment.body, "enrol");
        let secret = secret_of(&enrolment.body);
        // A wrong code shows the same secret again.
        let wrong = post(
            &node,
            &session,
            ui::mfa::SECURITY_ENABLE_PATH,
            "security",
            &format!("enrol={token}&code=000000"),
        );
        assert_eq!(wrong.status, 200, "{}", wrong.body);
        assert!(wrong.body.contains("The code is not right") && wrong.body.contains(&token));
        let done = post(
            &node,
            &session,
            ui::mfa::SECURITY_ENABLE_PATH,
            "security",
            &format!("enrol={token}&code={}", code(&secret, -1)),
        );
        assert_eq!(done.status, 200, "{}", done.body);
        assert!(
            done.body.contains("textarea")
                && done.header("cache-control").unwrap().contains("no-store")
        );
        let on = page(&node, &session, ui::mfa::SECURITY_PATH);
        assert!(
            on.body.contains("Recovery codes left") && on.body.contains(">10<"),
            "{}",
            on.body
        );
        // New recovery codes need the password and a code.
        let refused = post(
            &node,
            &session,
            ui::mfa::SECURITY_RECOVERY_PATH,
            "security",
            &format!("password=x-wrong&code={}", code(&secret, 0)),
        );
        assert_eq!(
            refused.location(),
            Some("/auth/settings/security?s=mfa-refused")
        );
        let renewed = post(
            &node,
            &session,
            ui::mfa::SECURITY_RECOVERY_PATH,
            "security",
            &format!("password={ALICE}&code={}", code(&secret, 0)),
        );
        assert_eq!(renewed.status, 200, "{}", renewed.body);
        assert!(renewed.body.contains("textarea"));
        // Turning it off needs the password and a code, and a code is good once.
        let again = post(
            &node,
            &session,
            ui::mfa::SECURITY_DISABLE_PATH,
            "security",
            &format!("password={ALICE}&code={}", code(&secret, 0)),
        );
        assert_eq!(
            again.location(),
            Some("/auth/settings/security?s=mfa-refused"),
            "the code was used"
        );
        let off = post(
            &node,
            &session,
            ui::mfa::SECURITY_DISABLE_PATH,
            "security",
            &format!("password={ALICE}&code={}", code(&secret, 1)),
        );
        assert_eq!(
            off.location(),
            Some("/auth/settings/security?s=mfa-disabled")
        );
        assert!(node.state.store.totp_get("alice").unwrap().is_none());
        wait_until(Duration::from_secs(10), || {
            audit_kinds(&node).contains(&"totp_disabled".to_string())
        });
        let kinds = audit_kinds(&node);
        for kind in ["totp_enabled", "recovery_regenerated", "totp_disabled"] {
            assert!(kinds.contains(&kind.to_string()), "{kind}: {kinds:?}");
        }
        // A form needs its own token and the same origin.
        let origin = format!("http://127.0.0.1:{}", node.port);
        let cookie = format!("{}={session}", session_cookie_name(&node));
        let forged = send(
            node.port,
            "POST",
            ui::mfa::SECURITY_DISABLE_PATH,
            &[
                ("Origin", &origin),
                ("Content-Type", "application/x-www-form-urlencoded"),
                ("Cookie", &cookie),
            ],
            "csrf=wrong&password=x&code=1",
        );
        assert_eq!(forged.status, 403);
    }

    #[test]
    fn with_the_policy_required_the_second_factor_cannot_be_turned_off() {
        let node = mfa_node(&Backend::Sqlite, "required", "");
        let (secret, _) = enrol(&node, "alice");
        let (page_one, csrf) = step_one(&node, "alice", ALICE, false);
        let reply = second(&node, &page_one, &csrf, &code(&secret, 0));
        let session = reply.cookie(&session_cookie_name(&node)).unwrap();
        let on = page(&node, &session, ui::mfa::SECURITY_PATH);
        assert!(!on.body.contains("Turn off"), "no button");
        let off = post(
            &node,
            &session,
            ui::mfa::SECURITY_DISABLE_PATH,
            "security",
            &format!("password={ALICE}&code={}", code(&secret, 1)),
        );
        assert_eq!(
            off.location(),
            Some("/auth/settings/security?s=mfa-required")
        );
        assert!(node.state.store.totp_get("alice").unwrap().is_some());
    }

    #[test]
    fn the_list_of_the_accounts_shows_who_has_a_second_factor() {
        let node = mfa_node(&Backend::Sqlite, "optional", "");
        insert_account(&node, "bob", "bob-correct-password");
        enrol(&node, "alice");
        let root = sign_in(&node, "root", PASSWORD);
        let access = page(&node, &root, "/auth/settings");
        assert_eq!(access.status, 200);
        // The column, and the count in the head of the list: one account of two.
        assert!(
            access.body.contains(">2FA</th>")
                && access
                    .body
                    .contains(r#"title="Accounts with two-factor sign-in">"#)
                && access.body.contains(r#"</svg>2FA 1/2</span>"#),
            "{}",
            access.body
        );
        // The row of alice has the mark. The row of bob has the dash.
        let row = |login: &str| {
            let start = access.body.find(&format!(r#"title="{login}""#)).unwrap();
            let end = start + access.body[start..].find("</tr>").unwrap();
            access.body[start..end].to_string()
        };
        assert!(
            row("alice").contains("Two-factor is on"),
            "{}",
            row("alice")
        );
        assert!(
            row("bob").contains("Two-factor is off") && !row("bob").contains("Two-factor is on")
        );
        // Without the second factor, the page has no column.
        let off = start_custom(
            Arc::new(Backend::Sqlite.fresh()),
            None,
            Custom::default(),
            "",
        );
        insert_account(&off, "alice", ALICE);
        let root = sign_in(&off, "root", PASSWORD);
        let access = page(&off, &root, "/auth/settings");
        assert_eq!(access.status, 200);
        assert!(
            access.body.contains(r#"title="alice""#),
            "the list shows alice"
        );
        assert!(
            !access.body.contains(">2FA</th>")
                && !access.body.contains("Accounts with two-factor sign-in"),
            "no column"
        );
    }

    #[test]
    fn the_superadmin_resets_the_second_factor_of_an_account_and_it_is_audited() {
        let node = mfa_node(&Backend::Sqlite, "optional", "");
        insert_account(&node, "bob", "bob-correct-password");
        let (secret, _) = enrol(&node, "alice");
        let (page_one, csrf) = step_one(&node, "alice", ALICE, false);
        let alice = second(&node, &page_one, &csrf, &code(&secret, 0))
            .cookie(&session_cookie_name(&node))
            .unwrap();
        let root = sign_in(&node, "root", PASSWORD);
        // The page of the account shows the state and a reset with a confirmation in place.
        let account = page(&node, &root, "/auth/settings/users/alice");
        assert!(
            account.body.contains("Reset the two-factor of alice")
                && account.body.contains(ui::mfa::RESET_PATH)
        );
        let bob = page(&node, &root, "/auth/settings/users/bob");
        assert!(
            !bob.body.contains(ui::mfa::RESET_PATH),
            "an account without it has nothing to reset"
        );
        // An account that is not the superadmin cannot reset.
        let denied = post(&node, &alice, ui::mfa::RESET_PATH, "users", "login=alice");
        assert_eq!(denied.status, 403);
        assert_eq!(
            post(&node, &root, ui::mfa::RESET_PATH, "users", "login=bob").location(),
            Some("/auth/settings/users/bob?s=mfa-none")
        );
        let reply = post(&node, &root, ui::mfa::RESET_PATH, "users", "login=alice");
        assert_eq!(
            reply.location(),
            Some("/auth/settings/users/alice?s=mfa-reset")
        );
        assert!(node.state.store.totp_get("alice").unwrap().is_none());
        assert_eq!(
            verify(&node, &alice, AGENT),
            302,
            "the sessions of the account end"
        );
        wait_until(Duration::from_secs(10), || {
            audit_kinds(&node).contains(&"totp_reset".to_string())
        });
        let changes = page(&node, &root, "/auth/settings/activity?v=changes");
        assert!(
            changes.body.contains("Two-factor reset"),
            "the event is in Activity"
        );
        // The account signs in with the password alone again.
        let session = sign_in(&node, "alice", ALICE);
        assert_eq!(verify(&node, &session, AGENT), 204);
    }

    #[test]
    fn a_session_of_log_in_as_needs_no_code_has_no_flag_and_has_no_page_security() {
        let node = mfa_node(&Backend::Sqlite, "optional", "");
        enrol(&node, "alice");
        let root = sign_in(&node, "root", PASSWORD);
        let reply = post(
            &node,
            &root,
            "/auth/users/impersonate",
            "users",
            "login=alice",
        );
        assert_eq!(reply.status, 303, "{}", reply.body);
        let browsing = reply
            .cookie(&session_cookie_name(&node))
            .expect("a session of Log in as");
        assert_eq!(verify(&node, &browsing, AGENT), 204);
        assert_eq!(page(&node, &browsing, ui::mfa::SECURITY_PATH).status, 404);
        assert!(!page(&node, &browsing, "/")
            .body
            .contains(ui::mfa::SECURITY_PATH));
        let flags = mfa_flags(&node, "alice");
        assert!(flags.iter().all(|f| !f), "{flags:?}");
    }

    #[test]
    fn the_pages_of_the_second_step_pass_forward_auth_without_a_session() {
        let node = mfa_node(&Backend::Sqlite, "required", "");
        assert_eq!(verify(&node, "nothing", AGENT), 302);
        for path in [ui::mfa::CODE_PATH, ui::mfa::ENROL_PATH] {
            let reply = send(
                node.port,
                "GET",
                VERIFY_PATH,
                &[("X-Forwarded-Uri", path), ("X-Forwarded-Method", "POST")],
                "",
            );
            assert_eq!(reply.status, 204, "{path}");
        }
    }

    /// The refusal of a stale pending token: the sign-in page with "open too long", and no session.
    fn assert_stale(node: &Node, reply: &Reply, why: &str) {
        assert_eq!(reply.status, 403, "{why}: {}", reply.body);
        assert!(
            reply.body.contains("The page was open too long")
                && reply.body.contains(r#"name="password""#),
            "{why}: {}",
            reply.body
        );
        assert!(
            !reply.body.contains(ui::mfa::ENROL_PATH),
            "{why}: no enrolment"
        );
        assert!(
            reply.cookie(&session_cookie_name(node)).is_none(),
            "{why}: no session"
        );
    }

    /// A pending token of the old password, used after the documented incident response (a new password, a reset
    /// of the second factor, End all sessions), opens no session and enrols no authenticator of the attacker
    /// (portal/session/sign-in-proof-outlives-credential-revocation, policy required).
    #[test]
    fn a_pending_token_from_before_the_incident_response_opens_no_session_and_enrols_nothing() {
        let node = mfa_node(&Backend::Sqlite, "required", "");
        enrol(&node, "alice");
        let (root_secret, _) = enrol(&node, "root");
        let (page, csrf) = step_one(&node, "root", PASSWORD, false);
        let root = second(&node, &page, &csrf, &code(&root_secret, 0))
            .cookie(&session_cookie_name(&node))
            .expect("a session of the superadmin");
        let (held, held_csrf) = step_one(&node, "alice", ALICE, true);
        assert_eq!(held.status, 200);
        let reset = post(
            &node,
            &root,
            USERS_UPDATE_PATH,
            USERS_TOKEN,
            "login=alice&password=a-new-alice-password-1",
        );
        assert_eq!(
            reset.location(),
            Some("/auth/settings/users/alice?s=password-reset")
        );
        let reset = post(
            &node,
            &root,
            ui::mfa::RESET_PATH,
            USERS_TOKEN,
            "login=alice",
        );
        assert_eq!(
            reset.location(),
            Some("/auth/settings/users/alice?s=mfa-reset")
        );
        let ended = post(
            &node,
            &root,
            END_SESSIONS_PATH,
            USERS_TOKEN,
            "confirm=end-all-sessions",
        );
        assert_eq!(ended.status, 303);
        assert_stale(
            &node,
            &second(&node, &held, &held_csrf, "000000"),
            "the old token",
        );
        assert!(
            node.state.store.totp_get("alice").unwrap().is_none(),
            "no authenticator of the attacker"
        );
    }

    /// With the policy optional, a pending token of the old password does not open a session without a code after a
    /// reset of the second factor (portal/session/sign-in-proof-outlives-credential-revocation, policy optional).
    #[test]
    fn a_pending_token_from_before_a_reset_of_the_second_factor_needs_the_password_again() {
        let node = mfa_node(&Backend::Sqlite, "optional", "");
        enrol(&node, "alice");
        let root = sign_in(&node, "root", PASSWORD);
        let (held, csrf) = step_one(&node, "alice", ALICE, true);
        assert_eq!(held.status, 200);
        let reset = post(
            &node,
            &root,
            ui::mfa::RESET_PATH,
            USERS_TOKEN,
            "login=alice",
        );
        assert_eq!(
            reset.location(),
            Some("/auth/settings/users/alice?s=mfa-reset")
        );
        assert_stale(
            &node,
            &second(&node, &held, &csrf, "000000"),
            "after the reset",
        );
        // The password step of now gives a session at once: the account has no second factor.
        let session = sign_in(&node, "alice", ALICE);
        assert_eq!(verify(&node, &session, AGENT), 204);
    }

    /// A pending token of the old password does not open a session after a new password, even with a good code of
    /// the second factor (portal/mfa/pending-token-not-bound-to-credential-state, case b).
    #[test]
    fn a_pending_token_of_an_old_password_is_refused_with_a_good_code() {
        let node = mfa_node(&Backend::Sqlite, "optional", "");
        let (secret, _) = enrol(&node, "alice");
        let root = sign_in(&node, "root", PASSWORD);
        let (held, csrf) = step_one(&node, "alice", ALICE, false);
        let reset = post(
            &node,
            &root,
            USERS_UPDATE_PATH,
            USERS_TOKEN,
            "login=alice&password=a-new-alice-password-1",
        );
        assert_eq!(
            reset.location(),
            Some("/auth/settings/users/alice?s=password-reset")
        );
        assert_stale(
            &node,
            &second(&node, &held, &csrf, &code(&secret, 0)),
            "an old password",
        );
        // The new password and the same factor sign in.
        let (page, csrf) = step_one(&node, "alice", "a-new-alice-password-1", false);
        let reply = second(&node, &page, &csrf, &code(&secret, 1));
        assert_eq!(reply.status, 303, "{}", reply.body);
    }

    /// With the policy required, the enrolment page of an old password enrols nothing and opens no session
    /// (portal/mfa/pending-token-not-bound-to-credential-state, case c).
    #[test]
    fn the_enrolment_page_of_an_old_password_enrols_nothing() {
        let node = mfa_node(&Backend::Sqlite, "required", "");
        let (root_secret, _) = enrol(&node, "root");
        let (page, csrf) = step_one(&node, "root", PASSWORD, false);
        let root = second(&node, &page, &csrf, &code(&root_secret, 0))
            .cookie(&session_cookie_name(&node))
            .expect("a session of the superadmin");
        let (held, held_csrf) = step_one(&node, "alice", ALICE, false);
        assert_eq!(held.status, 200);
        assert!(held.body.contains(ui::mfa::ENROL_PATH));
        let reset = post(
            &node,
            &root,
            USERS_UPDATE_PATH,
            USERS_TOKEN,
            "login=alice&password=a-new-alice-password-1",
        );
        assert_eq!(
            reset.location(),
            Some("/auth/settings/users/alice?s=password-reset")
        );
        let secret = secret_of(&held.body);
        let body = format!(
            "csrf={held_csrf}&rd={}&sig={}&pending={}&enrol={}&code={}",
            percent_encode_query_component(&hidden_value(&held.body, "rd")),
            hidden_value(&held.body, "sig"),
            hidden_value(&held.body, "pending"),
            hidden_value(&held.body, "enrol"),
            code(&secret, 0),
        );
        let reply = post_step(&node, ui::mfa::ENROL_PATH, &held_csrf, &body, AGENT);
        assert_stale(&node, &reply, "the enrolment of an old password");
        assert!(node.state.store.totp_get("alice").unwrap().is_none());
    }

    /// A pending token of the superadmin does not open a session on an instance with a new superadmin password, or
    /// with a new superadmin login, and the same session secret (portal/session/sign-in-proof-outlives-credential-
    /// revocation, check 04, and portal/mfa/pending-token-not-bound-to-credential-state, case d).
    #[test]
    fn a_pending_token_of_the_superadmin_is_refused_after_a_rotation_of_its_credential() {
        let store = Arc::new(Backend::Sqlite.fresh());
        let custom = |login: &'static str, password: &'static str| Custom {
            mfa: "optional",
            login,
            password,
            ..Custom::default()
        };
        let old = start_custom(Arc::clone(&store), None, custom("root", PASSWORD), "");
        let (secret, _) = enrol(&old, "root");
        let (held, csrf) = step_one(&old, "root", PASSWORD, false);
        assert_eq!(held.status, 200);
        let new_password = start_custom(
            Arc::clone(&store),
            None,
            custom("root", "another-password-for-root"),
            "",
        );
        assert_stale(
            &new_password,
            &second(&new_password, &held, &csrf, &code(&secret, 0)),
            "a new superadmin password",
        );
        let new_login = start_custom(
            Arc::clone(&store),
            None,
            custom("root2", "another-password-for-root"),
            "",
        );
        assert_stale(
            &new_login,
            &second(&new_login, &held, &csrf, "000000"),
            "a new superadmin login",
        );
        // The instance of the old credential still takes the token: nothing else refused it.
        let reply = second(&old, &held, &csrf, &code(&secret, 1));
        assert_eq!(reply.status, 303, "{}", reply.body);
    }

    /// A good second step of the own account clears the attempts of the second factor of the address, and not the
    /// failures of the password step that the address made against another login
    /// (portal/login/ip-limiter-cleared-by-own-sign-in, second-step variant).
    #[test]
    fn a_good_second_step_of_another_account_does_not_clear_the_password_failures_of_the_address() {
        let node = mfa_node(&Backend::Sqlite, "optional", "");
        let (secret, _) = enrol(&node, "alice");
        let csrf = "A".repeat(32);
        let guess = || {
            post_login(
                &node,
                SOURCE,
                &csrf,
                &login_body(&node, &csrf, "root", "a-wrong-guess-1"),
            )
            .status
        };
        for _ in 0..4 {
            assert_eq!(guess(), 401);
        }
        let (page, page_csrf) = step_one(&node, "alice", ALICE, false);
        assert_eq!(page.status, 200);
        let reply = second(&node, &page, &page_csrf, &code(&secret, 0));
        assert_eq!(reply.status, 303, "{}", reply.body);
        assert_eq!(guess(), 401, "the fifth failure locks");
        assert_eq!(guess(), 429);
    }
}

// Security fixes of the gate, the forms and the PDP --------------------------------------------

/// The real `/auth/verify` with the hrefs of the demo, an escaped href and the text prefixes of
/// the routers. bob holds `access` on wiki and on `myapp` only.
#[test]
fn the_gate_owns_an_escaped_href_and_each_path_that_a_router_prefix_routes() {
    let node = start_with(
        Arc::new(Store::open_in_memory()),
        Some(Duration::ZERO),
        r#"identity { issuer = "https://portal.test" signing_key_env = "TEST_IDENTITY_KEY" token_ttl = "5m" }"#,
    );
    let catalog = Catalog::build(
        vec![
            listed(&[
                ("portal.id", "files"),
                ("portal.name", "Files"),
                ("portal.href", "/files/"),
            ]),
            listed(&[
                ("portal.id", "wiki"),
                ("portal.name", "Wiki"),
                ("portal.href", "/wiki/"),
            ]),
            listed(&[
                ("portal.id", "grafana"),
                ("portal.name", "Grafana"),
                ("portal.href", "/grafana/"),
            ]),
            listed(&[
                ("portal.id", "myapp"),
                ("portal.name", "My app"),
                ("portal.href", "/my%20app/"),
            ]),
        ],
        Vec::new(),
    );
    *node.state.catalog.write().unwrap() = Arc::new(catalog);
    let root = sign_in(&node, "root", PASSWORD);
    let alice = user_without_grant(&node, &root, "alice");
    let _ = user_without_grant(&node, &root, "bob");
    change_grants(
        &node,
        "bob",
        vec![
            Grant::new(crate::grants::ACCESS, "wiki").unwrap(),
            Grant::new(crate::grants::ACCESS, "myapp").unwrap(),
        ],
    );
    let bob = sign_in(&node, "bob", "a-long-password-1");
    for uri in [
        "/files/secret",
        "/filesX/secret",
        "/files-private/secret",
        "/files.json",
        "/grafanaapi/admin/users",
    ] {
        let reply = open(&node, &bob, uri);
        assert_eq!(reply.status, 302, "{uri}");
        assert_eq!(reply.location(), Some("/auth/denied"), "{uri}");
    }
    for uri in ["/my%20app/", "/my%20app/admin/settings"] {
        assert_eq!(open(&node, &alice, uri).status, 302, "{uri}");
        let reply = open(&node, &bob, uri);
        assert_eq!(reply.status, 204, "{uri}");
        let token = reply
            .header("x-identity-token")
            .expect("the holder gets the token of the service");
        assert_eq!(claims(token)["aud"], "myapp");
    }
    // A text prefix: the holder passes, without a token, and the others are refused.
    let reply = open(&node, &bob, "/wiki-old/x");
    assert_eq!(reply.status, 204);
    assert!(reply.header("x-identity-token").is_none());
    assert_eq!(open(&node, &alice, "/wiki-old/x").status, 302);
    // A path that no service owns, and the pages of the portal, stay open.
    assert_eq!(open(&node, &alice, "/undeclared/x").status, 204);
    assert_eq!(open(&node, &alice, "/auth/denied").status, 204);
    // The superadmin passes each path. The token is only for a page of the service.
    let reply = open(&node, &root, "/files/secret");
    assert_eq!(reply.status, 204);
    assert!(reply.header("x-identity-token").is_some());
    assert_eq!(open(&node, &root, "/filesX/secret").status, 204);
}

/// `GET /auth/options` gives a key creator without `access` on a service the answer of a service
/// that does not exist, whatever the type and the parent. The holder still gets the type check.
#[test]
fn the_options_of_a_service_that_the_viewer_cannot_open_tell_nothing_of_it() {
    let (node, _) = key_node(&Backend::Sqlite);
    let root = sign_in(&node, "root", PASSWORD);
    let rights = vec![
        Grant::parse("keys.own.read on service:portal").unwrap(),
        Grant::parse("keys.own.create on service:portal/key_kind:mcp").unwrap(),
        Grant::parse("keys.own.use on service:portal/key_kind:mcp").unwrap(),
    ];
    let _ = user_without_grant(&node, &root, "alice");
    change_grants(&node, "alice", rights.clone());
    let _ = user_without_grant(&node, &root, "bob");
    let mut with_access = rights;
    with_access.push(Grant::new("access", "db").unwrap());
    change_grants(&node, "bob", with_access);
    let alice = sign_in(&node, "alice", "a-long-password-1");
    let bob = sign_in(&node, "bob", "a-long-password-1");
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
    let nothing = r#"{"items":[],"next":null,"unavailable":false}"#;
    for query in [
        "?service=db&type=zzz",
        "?service=db&type=host",
        "?service=db&type=database",
        "?service=db&type=database&parent=host%3Ax",
        "?service=db&type=host&parent=host%3Ax",
        "?service=db&type=table&parent=database%3Ax",
        "?service=ghost&type=zzz",
    ] {
        let reply = get(&alice, query);
        assert_eq!((reply.status, reply.body.trim()), (200, nothing), "{query}");
    }
    assert_eq!(get(&bob, "?service=db&type=zzz").status, 400);
    assert_eq!(get(&bob, "?service=db&type=database").status, 400);
    assert_eq!(get(&alice, "?service=db&type=host&parent=bad").status, 400);
}

/// The form of an account keeps a narrower grant with its own limit values: the PDP takes the
/// lowest ceiling of the grants that cover a node. A second save of the same form changes nothing.
#[test]
fn the_form_of_an_account_keeps_a_narrower_grant_with_a_tighter_ceiling() {
    let (node, port) = key_node(&Backend::Sqlite);
    let root = sign_in(&node, "root", PASSWORD);
    let ask = |database: &str| -> serde_json::Value {
        let body = read_body("dave", database, serde_json::json!({}));
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
    let add = post(
        &node,
        &root,
        USERS_ADD_PATH,
        USERS_TOKEN,
        "login=dave&password=a-long-password-1",
    );
    assert_eq!(add.status, 303);
    for (parent_limit, sibling) in [
        ("", serde_json::json!({"decision": true})),
        (
            "&limit1.max_rows=5000",
            serde_json::json!({"decision": true, "context": {"limits": {"max_rows": 5000}}}),
        ),
    ] {
        let fields = format!(
            "grants=1&login=dave&grant0=access+on+service%3Adb\
             &grant1=data.read+on+service%3Adb%2Fhost%3Aprod{parent_limit}\
             &grant2=data.read+on+service%3Adb%2Fhost%3Aprod%2Fdatabase%3Atest2&limit2.max_rows=100"
        );
        let saved = post(&node, &root, USERS_UPDATE_PATH, USERS_TOKEN, &fields);
        assert_eq!(
            saved.location(),
            Some("/auth/settings/users/dave?s=user-updated"),
            "{parent_limit}"
        );
        assert_eq!(node.state.users.grants_of("dave").unwrap().len(), 3);
        assert_eq!(
            ask("test2"),
            serde_json::json!({"decision": true, "context": {"limits": {"max_rows": 100}}}),
            "{parent_limit}"
        );
        assert_eq!(ask("test1"), sibling, "{parent_limit}");
        let again = post(&node, &root, USERS_UPDATE_PATH, USERS_TOKEN, &fields);
        assert_eq!(
            again.location(),
            Some("/auth/settings/users/dave?s=unchanged"),
            "{parent_limit}"
        );
    }
    // A narrower grant without a limit value adds nothing: the account keeps the broad one.
    let fields = "grants=1&login=dave&grant0=access+on+service%3Adb\
        &grant1=data.read+on+service%3Adb%2Fhost%3Aprod\
        &grant2=data.read+on+service%3Adb%2Fhost%3Aprod%2Fdatabase%3Atest2";
    post(&node, &root, USERS_UPDATE_PATH, USERS_TOKEN, fields);
    assert_eq!(node.state.users.grants_of("dave").unwrap().len(), 2);
}

/// With no good document of the service, the PDP answers `false` for an action on the whole
/// service: it cannot apply the ceilings and the budgets. Only `access` on the service stays.
#[test]
fn without_a_document_the_pdp_refuses_an_action_on_the_whole_service_and_keeps_access() {
    let pdp_port = free_port();
    let node = start_with(
        Arc::new(Backend::Sqlite.fresh()),
        None,
        &format!(
            r#"identity {{ issuer = "https://portal.test" signing_key_env = "TEST_IDENTITY_KEY" }}
            pdp {{ listen_address = "127.0.0.1:{pdp_port}" url = "http://pdp.test:{pdp_port}" plain_http = true max_evaluations = 4 }}
            keys {{}}
            pdp_client "db" {{ token_env = "TEST_PDP_DB" }}"#
        ),
    );
    // The catalogue holds db, but no read of its document has answered.
    let catalog = Catalog::build(
        vec![listed(&[
            ("portal.name", "Db"),
            ("portal.id", "db"),
            ("portal.href", "/db/"),
            ("portal.actions", DB_ACTIONS_URL),
        ])],
        Vec::new(),
    );
    *node.state.catalog.write().unwrap() = Arc::new(catalog);
    wait_until(Duration::from_secs(10), || {
        TcpStream::connect(("127.0.0.1", pdp_port)).is_ok()
    });
    stay_in_a_minute();
    let mut grants = vec![
        Grant::new("access", "db")
            .unwrap()
            .with_limits(limits(r#"{"requests":[{"amount":1,"window":"minute"}]}"#)),
        Grant::new("data.read", "db")
            .unwrap()
            .with_limits(limits(r#"{"max_rows":50}"#)),
    ];
    grants.sort();
    node.state
        .store
        .insert_user(&crate::store::UserRow {
            login: "erin".into(),
            password_hash: password_hash(PASSWORD),
            grants,
            created_at: 1,
        })
        .unwrap();
    node.state.users.refresh(&node.state.store).unwrap();
    let ask = |subject: &str, action: &str, context: serde_json::Value| -> serde_json::Value {
        let body = serde_json::json!({
            "subject": {"type": "user", "id": subject},
            "action": {"name": action},
            "resource": {"type": "service", "id": "db"},
            "context": context
        })
        .to_string();
        let reply = pdp(
            pdp_port,
            "/access/v1/evaluation",
            Some(PDP_DB_TOKEN),
            &body,
            None,
        );
        assert_eq!(reply.status, 200, "{}", reply.body);
        reply.json()
    };
    let refused = serde_json::json!({"decision": false});
    assert!(node.state.actions.declared("db").is_none());
    for context in [
        serde_json::json!({}),
        serde_json::json!({"usage": {"units": 1}}),
    ] {
        assert_eq!(
            ask("erin", "data.read", context.clone()),
            refused,
            "{context}"
        );
        assert_eq!(
            ask("root", "data.read", context.clone()),
            refused,
            "{context}"
        );
    }
    assert_eq!(
        ask("erin", "access", serde_json::json!({})),
        serde_json::json!({"decision": true})
    );
    // With the document, the ceiling and the budget of erin apply.
    node.state.actions.set_document(
        "db",
        crate::actions::parse("db", KEY_TEST_DOCUMENT.as_bytes()).unwrap(),
    );
    assert_eq!(
        ask("erin", "data.read", serde_json::json!({})),
        serde_json::json!({"decision": true, "context": {"limits": {"max_rows": 50}}})
    );
    let first = ask(
        "erin",
        "data.read",
        serde_json::json!({"usage": {"units": 1}}),
    );
    assert_eq!(first["context"]["usage"]["units"], 1, "{first}");
    let second = ask(
        "erin",
        "data.read",
        serde_json::json!({"usage": {"units": 1}}),
    );
    assert_eq!(second["decision"], false, "{second}");
    assert_eq!(second["context"]["reason"], "budget_exceeded", "{second}");
}

// The binding of the sign-in proofs and of the sessions to the credential, and the checks of a session after the body
// of a form (security fixes of the sessions) ---------------------------------------------------------------------

mod credential_binding_tests {
    use super::*;

    /// Writes the head of a form POST with the `Content-Length` of `body`, and keeps the body. `finish_form` sends it.
    fn hold_form(node: &Node, session: &str, path: &str, body: &str) -> TcpStream {
        let head = format!(
            "POST {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\nContent-Length: {len}\r\nUser-Agent: {AGENT}\r\nOrigin: http://127.0.0.1:{port}\r\nContent-Type: application/x-www-form-urlencoded\r\nCookie: {name}={session}\r\n\r\n",
            port = node.port,
            len = body.len(),
            name = session_cookie_name(node),
        );
        let mut stream = TcpStream::connect(("127.0.0.1", node.port)).unwrap();
        stream.write_all(head.as_bytes()).unwrap();
        stream.flush().unwrap();
        stream
    }

    fn finish_form(mut stream: TcpStream, body: &str) -> Reply {
        stream
            .set_read_timeout(Some(Duration::from_secs(20)))
            .unwrap();
        stream.write_all(body.as_bytes()).unwrap();
        let mut raw = Vec::new();
        stream.read_to_end(&mut raw).unwrap();
        let text = String::from_utf8_lossy(&raw).to_string();
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

    /// A new password reaches the account while a sign-in checks the old one, and the revocation of the sessions runs
    /// before the sign-in issues its session. The session carries the stamp of the old password: it ends at its first
    /// use, without any revocation (portal/session/sign-in-proof-outlives-credential-revocation, in-flight variant).
    #[test]
    fn a_session_whose_password_changed_ends_at_its_next_use_without_a_revocation() {
        each_backend(|backend| {
            let (a, b) = cluster(backend, Some(Duration::ZERO));
            let root = sign_in(&a, "root", PASSWORD);
            let carol = user_without_grant(&a, &root, "carol");
            assert_eq!(verify(&a, &carol, AGENT), 204);
            assert_eq!(verify(&b, &carol, AGENT), 204);
            // The write of `handle_users_update`, without its call of `revoke_user`.
            let hash = password_hash("another-long-password-2");
            assert!(a
                .state
                .store
                .update_user("carol", Some(&hash), None)
                .unwrap());
            assert!(a.state.users.update("carol", Some(hash), None));
            assert_eq!(verify(&a, &carol, AGENT), 302, "{}", backend.name());
            crate::sync::run_round(&b.state, 1);
            assert_eq!(verify(&b, &carol, AGENT), 302, "{}", backend.name());
            // The new password opens a session that works on both instances.
            let fresh = sign_in(&a, "carol", "another-long-password-2");
            assert_eq!(verify(&b, &fresh, AGENT), 204, "{}", backend.name());
            assert_eq!(verify(&a, &fresh, AGENT), 204, "{}", backend.name());
        });
    }

    /// The proof of the password step holds for the hash that the check used. When the account has another hash at
    /// the moment the session opens, `finish_sign_in` opens no session and gives the sign-in page.
    #[test]
    fn a_proof_of_an_old_password_opens_no_session() {
        let node = start(Arc::new(Backend::Sqlite.fresh()), None);
        insert_account(&node, "carol", "old-long-password-1");
        let old = credential_stamp(&node.state, &Principal::User("carol".into())).unwrap();
        assert!(node
            .state
            .users
            .update("carol", Some(password_hash("new-long-password-2")), None));
        let client = Client {
            https: false,
            ip: Some("127.0.0.1".parse().unwrap()),
            binding: client_binding(&node, "127.0.0.1"),
        };
        let mut headers = HeaderMap::new();
        headers.insert(USER_AGENT, HeaderValue::from_static(AGENT));
        let reply = finish_sign_in(
            &node.state,
            &client,
            &headers,
            "/",
            SignedIn {
                principal: Principal::User("carol".into()),
                credential: old,
                session_login: "carol".into(),
                typed: "carol".into(),
                known: true,
                keep: true,
                mfa: false,
            },
            Landing::Redirect,
        );
        assert_eq!(reply.status(), StatusCode::FORBIDDEN);
        assert!(reply.headers().get_all(SET_COOKIE).iter().all(|cookie| {
            !cookie
                .to_str()
                .unwrap()
                .starts_with(&format!("{}=", session_cookie_name(&node)))
        }));
        assert_eq!(node.state.sessions.active_count(), 0);
    }

    /// With PostgreSQL, a session of an account of the file survives a restart. A new or a removed `password_hash`
    /// of the block, and a new account of the same login in the database, end it on every instance
    /// (portal/store/pg-session-row-bound-to-login-only, and the merged
    /// portal/session/file-account-credential-change-keeps-shared-sessions).
    #[test]
    fn a_new_password_hash_of_a_file_account_or_a_new_account_of_its_login_ends_its_shared_sessions(
    ) {
        let Some(backend) = crate::store::testing::postgres() else {
            return;
        };
        let store = Arc::new(backend.fresh());
        let block = |with_hash: bool| {
            format!(
                r#"access {{ grants_mode = "hybrid" }} user "carol" {{ grants = [] {} }}"#,
                if with_hash {
                    r#"password_hash_env = "CAROL_HASH""#
                } else {
                    ""
                }
            )
        };
        let node = |hash: Option<&str>, block: &str| {
            start_custom(
                Arc::clone(&store),
                Some(Duration::ZERO),
                Custom {
                    env: hash
                        .map(|password| vec![("CAROL_HASH".to_string(), password_hash(password))])
                        .unwrap_or_default(),
                    ..Custom::default()
                },
                block,
            )
        };
        let first = node(Some("carol-old-password-1"), &block(true));
        let rotated = sign_in(&first, "carol", "carol-old-password-1");
        let removed = sign_in(&first, "carol", "carol-old-password-1");
        let reused = sign_in(&first, "carol", "carol-old-password-1");
        for cookie in [&rotated, &removed, &reused] {
            assert_eq!(verify(&first, cookie, AGENT), 204);
        }
        // A restart with a new digest: the old password fails, and the old session ends.
        let second = node(Some("carol-new-password-2"), &block(true));
        let csrf = "A".repeat(32);
        let refused = post_login(
            &second,
            "127.0.0.1",
            &csrf,
            &login_body(&second, &csrf, "carol", "carol-old-password-1"),
        );
        assert_eq!(refused.status, 401);
        assert_eq!(verify(&second, &rotated, AGENT), 302, "a new digest");
        assert_eq!(
            verify(&first, &rotated, AGENT),
            302,
            "ended on every instance"
        );
        // A restart without the digest: the account opens no session, and its sessions end.
        let third = node(None, &block(false));
        assert_eq!(verify(&third, &removed, AGENT), 302, "no digest");
        // The block goes, and the superadmin makes an account of the database with the same login. A session of the
        // old account, unused in the meantime, does not pass to the new account.
        let fourth = node(None, "");
        let root = sign_in(&fourth, "root", PASSWORD);
        let new_carol = user_without_grant(&fourth, &root, "carol");
        assert_eq!(verify(&fourth, &reused, AGENT), 302, "another account");
        assert_eq!(verify(&fourth, &new_carol, AGENT), 204);
    }

    /// A form whose head arrived before End all sessions, with the body after it, does nothing: "Log in as" gives no
    /// new session (portal/http/form-viewer-before-body-no-recheck).
    #[test]
    fn a_held_log_in_as_after_end_all_sessions_gives_no_session() {
        let node = start(Arc::new(Backend::Sqlite.fresh()), None);
        let root = sign_in(&node, "root", PASSWORD);
        let _bob = user_without_grant(&node, &root, "bob");
        let stolen = sign_in(&node, "root", PASSWORD);
        let token = sign_action_token(&node.state.signing_secret, USERS_TOKEN, &stolen).unwrap();
        let body = format!("csrf={token}&login=bob");
        let held = hold_form(&node, &stolen, IMPERSONATE_PATH, &body);
        std::thread::sleep(Duration::from_millis(300));
        let ended = post(
            &node,
            &root,
            END_SESSIONS_PATH,
            USERS_TOKEN,
            "confirm=end-all-sessions",
        );
        assert_eq!(ended.status, 303);
        assert_eq!(node.state.sessions.active_count(), 0);
        std::thread::sleep(Duration::from_millis(500));
        let reply = finish_form(held, &body);
        assert_eq!(reply.status, 303);
        assert_eq!(reply.location(), Some(LOGIN_PATH));
        assert!(reply.cookie(&session_cookie_name(&node)).is_none());
        assert_eq!(node.state.sessions.active_count(), 0);
    }

    /// A key form whose head arrived before a new password of the account, with the body after it, makes no key.
    #[test]
    fn a_held_key_creation_after_a_new_password_makes_no_key() {
        let (node, _pdp, alice) = keys_user(&Backend::Sqlite, "");
        let root = sign_in(&node, "root", PASSWORD);
        let token = sign_action_token(
            &node.state.signing_secret,
            super::super::keys_page::KEYS_TOKEN,
            &alice,
        )
        .unwrap();
        let body =
            format!("csrf={token}&kind=mcp&name=held&expires_at=&grant0=access+on+service%3Adb");
        let keys_before = node.state.users.key_count();
        let held = hold_form(&node, &alice, "/auth/keys/create", &body);
        std::thread::sleep(Duration::from_millis(300));
        let reset = post(
            &node,
            &root,
            USERS_UPDATE_PATH,
            USERS_TOKEN,
            "login=alice&password=another-long-password-9",
        );
        assert_eq!(reset.status, 303);
        let reply = finish_form(held, &body);
        assert_eq!(reply.location(), Some(LOGIN_PATH), "{}", reply.body);
        assert!(key_text_in(&reply.body).is_none());
        assert_eq!(node.state.users.key_count(), keys_before);
    }

    /// The configuration of a start whose superadmin login is the login of an account of the database.
    fn state_with_superadmin(store: Arc<Store>, login: &str) -> Result<AppState, String> {
        let login_value = login.to_string();
        let (config, _) = Config::parse(
            &format!(
                r#"server {{ listen_address = "127.0.0.1:{}" trusted_proxies = [] }}
                authentication {{ superadmin_login_env = "L" superadmin_password_env = "P" mfa = "off" }}
                session {{ secret_mode = "provided" secret_env = "S" }}"#,
                free_port()
            ),
            &move |name| match name {
                "L" => Some(login_value.clone()),
                "P" => Some(PASSWORD.to_string()),
                "S" => Some("0123456789abcdef0123456789abcdef".to_string()),
                _ => None,
            },
        )
        .unwrap();
        let superadmin = SuperAdmin {
            login: login.into(),
            password_hash: password_hash(PASSWORD),
            generation: crate::crypto::superadmin_generation(&SECRET, login, PASSWORD.as_bytes())
                .unwrap(),
        };
        AppState::new(
            config,
            superadmin,
            SECRET.to_vec(),
            store,
            Tracker::disabled(),
            AccessSecrets::default(),
        )
    }

    /// The start refuses a superadmin login that an account of the database holds, and the error hides the login
    /// (portal/bootstrap/superadmin-login-not-checked-against-database-accounts).
    #[test]
    fn the_start_refuses_a_superadmin_login_that_an_account_of_the_database_holds() {
        each_backend(|backend| {
            let store = Arc::new(backend.fresh());
            store
                .insert_user(&crate::store::UserRow {
                    login: "carol".into(),
                    password_hash: password_hash("carol-password-1"),
                    grants: Vec::new(),
                    created_at: 1,
                })
                .unwrap();
            let error = state_with_superadmin(Arc::clone(&store), "carol")
                .err()
                .expect("the start must stop");
            assert!(
                error.contains("authentication.superadmin_login") && !error.contains("carol"),
                "{error}"
            );
            assert!(state_with_superadmin(store, "root").is_ok());
        });
    }

    /// One account fills only its own places: with 5 places and 2 for each account, the account that signs in again
    /// and again ends its own oldest session, and the other accounts and the superadmin still sign in
    /// (portal/session/no-per-account-session-cap).
    #[test]
    fn one_account_that_signs_in_again_and_again_leaves_places_for_the_others() {
        let node = start_custom(
            Arc::new(Backend::Sqlite.fresh()),
            None,
            Custom {
                session: "max_active_sessions = 5 max_sessions_per_account = 2",
                ..Custom::default()
            },
            "",
        );
        let root = sign_in(&node, "root", PASSWORD);
        insert_account(&node, "mallory", "mallory-password-1");
        insert_account(&node, "bob", "bob-password-1");
        let mallory: Vec<String> = (0..5)
            .map(|_| sign_in(&node, "mallory", "mallory-password-1"))
            .collect();
        assert_eq!(node.state.sessions.active_count(), 3);
        assert_eq!(verify(&node, &mallory[0], AGENT), 302, "the oldest ended");
        assert_eq!(verify(&node, &mallory[4], AGENT), 204);
        let bob = sign_in(&node, "bob", "bob-password-1");
        let root_again = sign_in(&node, "root", PASSWORD);
        for session in [&root, &bob, &root_again] {
            assert_eq!(verify(&node, session, AGENT), 204);
        }
    }

    /// A good sign-in of the own account of a client does not clear the failures that the address made against
    /// another login (portal/login/ip-limiter-cleared-by-own-sign-in).
    #[test]
    fn a_good_sign_in_of_another_account_does_not_clear_the_failures_of_the_address() {
        let node = start(Arc::new(Backend::Sqlite.fresh()), None);
        insert_account(&node, "mallory", "mallory-password-1");
        let csrf = "A".repeat(32);
        let attempt = |login: &str, password: &str| {
            post_login(
                &node,
                "127.0.0.5",
                &csrf,
                &login_body(&node, &csrf, login, password),
            )
            .status
        };
        for _ in 0..4 {
            assert_eq!(attempt("root", "a-wrong-guess-1"), 401);
        }
        assert_eq!(attempt("mallory", "mallory-password-1"), 303);
        assert_eq!(
            attempt("root", "a-wrong-guess-1"),
            401,
            "the fifth failure locks"
        );
        assert_eq!(attempt("root", "a-wrong-guess-1"), 429);
        assert_eq!(attempt("mallory", "mallory-password-1"), 429);
    }
}

// External services -------------------------------------------------------------------------------

fn add_external(node: &Node, session: &str, name: &str, href: &str) -> Reply {
    post(
        node,
        session,
        EXTERNAL_ADD_PATH,
        USERS_TOKEN,
        &format!(
            "name={}&href={}",
            percent_encode_query_component(name),
            percent_encode_query_component(href)
        ),
    )
}

fn delete_external(node: &Node, session: &str, id: &str) -> Reply {
    post(
        node,
        session,
        EXTERNAL_DELETE_PATH,
        USERS_TOKEN,
        &format!("id={}", percent_encode_query_component(id)),
    )
}

const WIKI_LINK: &str = "https://wiki.example.org/start?a=1&b=2";

#[test]
fn the_superadmin_adds_an_external_service_and_its_tile_follows_the_grant_access() {
    each_backend(|backend| {
        let node = start_with(
            Arc::new(backend.fresh()),
            Some(Duration::ZERO),
            r#"identity { issuer = "https://portal.test" signing_key_env = "TEST_IDENTITY_KEY" token_ttl = "5m" }"#,
        );
        publish_catalog(&node, &["query.run.read"]);
        let root = sign_in(&node, "root", PASSWORD);
        assert_eq!(
            post(
                &node,
                &root,
                USERS_ADD_PATH,
                USERS_TOKEN,
                "login=alice&password=a-long-password-1"
            )
            .status,
            303
        );
        let added = add_external(&node, &root, "Team wiki", WIKI_LINK);
        assert_eq!(added.status, 303, "{}", added.body);
        assert_eq!(
            added.location(),
            Some("/auth/settings/services?s=external-added&svc=ext-team-wiki")
        );
        let stored = node.state.store.load_directory().unwrap().externals;
        assert_eq!(stored.len(), 1);
        assert_eq!(stored[0].id, "ext-team-wiki");
        assert_eq!(stored[0].href, WIKI_LINK);
        assert_eq!(node.state.users.externals(), stored);

        // The superadmin sees the tile with the badge. An account without a grant sees none.
        let launcher = page(&node, &root, "/");
        assert!(launcher.body.contains("Team wiki"), "{}", launcher.body);
        assert!(launcher
            .body
            .contains("External link. This server does not protect it."));
        assert!(launcher
            .body
            .contains(r#"href="https://wiki.example.org/start?a=1&amp;b=2""#));
        let alice = sign_in(&node, "alice", "a-long-password-1");
        assert!(page(&node, &alice, "/")
            .body
            .contains("No service available"));
        // The Services page lists it with the badge and the words that say that the portal does not protect it.
        let services = page(&node, &root, "/auth/settings/services?svc=ext-team-wiki");
        assert_eq!(services.status, 200);
        assert!(services.body.contains("badge badge--external"));
        assert!(services
            .body
            .contains("External link. This server does not protect it."));
        assert!(services.body.contains(r#"action="/auth/services/delete""#));
        assert!(services.body.contains("Add external service"));

        // The grant `access` shows the tile.
        let give = |fields: &str| {
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
            give("grant0=access+on+service%3Aext-team-wiki"),
            Some("/auth/settings/users/alice?s=user-updated".into())
        );
        let launcher = page(&node, &alice, "/");
        assert!(launcher.body.contains("Team wiki"), "{}", launcher.body);
        assert!(launcher
            .body
            .contains("External link. This server does not protect it."));
        assert!(!launcher.body.contains("No service available"));

        // The account page draws the service with its switch and no other action.
        let account = page(&node, &root, "/auth/settings/users/alice");
        let editor = editor_json(&account.body);
        let wiki = editor["services"]
            .as_array()
            .unwrap()
            .iter()
            .find(|s| s["id"] == "ext-team-wiki")
            .expect("the editor lists the external service");
        assert_eq!(wiki["external"], true);
        let actions: Vec<&str> = wiki["actions"]
            .as_array()
            .unwrap()
            .iter()
            .map(|a| a["name"].as_str().unwrap())
            .collect();
        assert_eq!(actions, ["access"]);
        assert_eq!(wiki["nodes"].as_array().unwrap().len(), 1);
        let others: Vec<&serde_json::Value> = editor["services"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|s| s["id"] != "ext-team-wiki")
            .collect();
        assert!(others.iter().all(|s| s["external"] == false));

        // A grant other than `access` on the whole service is refused, in the account editor and in the new user form.
        for refused in [
            "grant1=query.run.read+on+service%3Aext-team-wiki",
            "grant1=access+on+service%3Aext-team-wiki%2Fhost%3Aprod",
            "grant1=anything+on+service%3Aext-team-wiki",
        ] {
            assert_eq!(
                give(&format!(
                    "grant0=access+on+service%3Aext-team-wiki&{refused}"
                )),
                Some("/auth/settings/users/alice?s=edit-unknown-grant".into()),
                "{refused}"
            );
            let reply = post(
                &node,
                &root,
                USERS_ADD_PATH,
                USERS_TOKEN,
                &format!("password=a-long-password-1&login=bob&grants=1&grant0=access+on+service%3Aext-team-wiki&{refused}"),
            );
            assert_eq!(reply.status, 303, "{refused}");
            assert_eq!(
                reply.location(),
                Some("/auth/settings/new-user?s=unknown-grant")
            );
        }
        assert!(node.state.users.get("bob").is_none());
        assert_eq!(
            node.state.users.grants_of("alice").unwrap().len(),
            1,
            "a refused save changes no grant"
        );
        // The new user form still makes an account with `access` on the service and on another one.
        let ok = post(
            &node,
            &root,
            USERS_ADD_PATH,
            USERS_TOKEN,
            "password=a-long-password-1&login=carol&grants=1&grant0=access+on+service%3Aext-team-wiki&grant1=access+on+service%3Adb&grant2=query.run.read+on+service%3Adb",
        );
        assert_eq!(
            ok.location(),
            Some("/auth/settings/users/carol?s=user-added")
        );
        assert_eq!(node.state.users.grants_of("carol").unwrap().len(), 3);
        // The form of the other services works as before.
        assert_eq!(
            give("grant0=access+on+service%3Aext-team-wiki&grant1=access+on+service%3Adb&grant2=query.run.read+on+service%3Adb"),
            Some("/auth/settings/users/alice?s=user-updated".into())
        );

        // The service is never a client: no document, no action, no identity token and no path in the gate.
        assert!(node.state.actions.declared("ext-team-wiki").is_none());
        assert!(node.state.actions.actions("ext-team-wiki").is_empty());
        let allowed = open(&node, &alice, "/db/query");
        assert_eq!(allowed.status, 204);
        assert_eq!(
            claims(allowed.header("x-identity-token").unwrap())["aud"],
            "db"
        );
        assert_eq!(
            open(&node, &alice, "/grafana/d/1").status,
            302,
            "the grant on the link opens no path of another service"
        );
        for uri in ["/wiki/start", "/start", "/auth/verify"] {
            assert!(
                open(&node, &alice, uri)
                    .header("x-identity-token")
                    .is_none(),
                "{uri}: no identity token for a link"
            );
        }
        assert!(current_catalog(&node.state)
            .gate()
            .iter()
            .all(|entry| entry.services.iter().all(|s| !s.id.starts_with("ext-"))));
    });
}

#[test]
fn a_link_to_the_host_of_the_portal_owns_no_path_of_the_gate() {
    each_backend(|backend| {
        let node = start_with(Arc::new(backend.fresh()), Some(Duration::ZERO), "");
        publish_catalog(&node, &[]);
        let root = sign_in(&node, "root", PASSWORD);
        insert_account(&node, "alice", "a-long-password-1");
        insert_account(&node, "bob", "a-long-password-1");
        let host = format!("http://127.0.0.1:{}", node.port);
        for (name, path) in [
            ("Mirror", "/grafana/"),
            ("Verify", "/auth/verify"),
            ("Everything", "/"),
            ("Deep", "/db/admin/x"),
        ] {
            let reply = add_external(&node, &root, name, &format!("{host}{path}"));
            assert_eq!(reply.status, 303, "{name}: {}", reply.body);
            assert!(
                reply.location().unwrap().contains("external-added"),
                "{name}"
            );
        }
        // Alice holds `access` on every link and on nothing else. Bob holds `access` on Grafana.
        let links: Vec<Grant> = ["ext-mirror", "ext-verify", "ext-everything", "ext-deep"]
            .iter()
            .map(|id| Grant::new("access", id).unwrap())
            .collect();
        change_grants(&node, "alice", links);
        change_grants(
            &node,
            "bob",
            vec![Grant::new("access", "other-grafana").unwrap()],
        );
        let alice = sign_in(&node, "alice", "a-long-password-1");
        let bob = sign_in(&node, "bob", "a-long-password-1");
        // A link cannot open the path of another service...
        assert_eq!(open(&node, &alice, "/grafana/d/1").status, 302);
        assert_eq!(open(&node, &alice, "/db/query").status, 302);
        // ...and it cannot hide or shadow it: the owner of the path is the service of the workload.
        assert_eq!(open(&node, &bob, "/grafana/d/1").status, 204);
        assert_eq!(open(&node, &bob, "/db/query").status, 302);
        // The gate of the portal has no entry for a link.
        let catalog = current_catalog(&node.state);
        assert!(catalog
            .owner(&["grafana".to_string(), "d".to_string()])
            .is_some());
        assert!(catalog
            .owner(&["auth".to_string(), "verify".to_string()])
            .is_none());
        assert_eq!(catalog.gate().len(), 2, "the gate of the two workloads");
        // The pages of the portal answer as before.
        assert_eq!(page(&node, &alice, "/").status, 200);
        assert_eq!(page(&node, &root, "/auth/settings/services").status, 200);
    });
}

#[test]
fn deleting_an_external_service_removes_its_grants_in_one_step_and_is_audited() {
    each_backend(|backend| {
        let node = start_with(Arc::new(backend.fresh()), Some(Duration::ZERO), "");
        publish_catalog(&node, &[]);
        let root = sign_in(&node, "root", PASSWORD);
        insert_account(&node, "alice", "a-long-password-1");
        insert_account(&node, "bob", "a-long-password-1");
        assert_eq!(
            add_external(&node, &root, "Team wiki", WIKI_LINK).status,
            303
        );
        assert_eq!(
            add_external(&node, &root, "Docs", "https://docs.example.org/").status,
            303
        );
        let wiki = Grant::new("access", "ext-team-wiki").unwrap();
        let docs = Grant::new("access", "ext-docs").unwrap();
        let db = Grant::new("access", "db").unwrap();
        change_grants(&node, "alice", vec![wiki.clone(), docs.clone(), db.clone()]);
        change_grants(&node, "bob", vec![wiki.clone()]);
        let alice = sign_in(&node, "alice", "a-long-password-1");
        assert!(page(&node, &alice, "/").body.contains("Team wiki"));

        let gone = delete_external(&node, &root, "ext-team-wiki");
        assert_eq!(gone.status, 303);
        assert_eq!(
            gone.location(),
            Some("/auth/settings/services?s=external-deleted")
        );
        // The entry and every grant that names it: in the directory and in the database.
        assert_eq!(node.state.users.externals().len(), 1);
        let directory = node.state.store.load_directory().unwrap();
        assert_eq!(directory.externals.len(), 1);
        assert_eq!(directory.externals[0].id, "ext-docs");
        for user in &directory.users {
            assert!(
                user.grants.iter().all(|g| g.service != "ext-team-wiki"),
                "{}",
                user.login
            );
        }
        assert_eq!(
            node.state.users.grants_of("alice").unwrap(),
            {
                let mut kept = vec![docs.clone(), db.clone()];
                kept.sort();
                kept
            },
            "the other grants stay"
        );
        assert!(node.state.users.grants_of("bob").unwrap().is_empty());
        let launcher = page(&node, &alice, "/");
        assert!(!launcher.body.contains("Team wiki") && launcher.body.contains("Docs"));
        // The same name makes a new entry with no old grant.
        assert_eq!(
            add_external(&node, &root, "Team wiki", WIKI_LINK).status,
            303
        );
        assert!(!page(&node, &alice, "/").body.contains("Team wiki"));
        // A service that does not exist, or an id that is not an external id.
        for id in ["ext-nobody", "db", "other-grafana", "", "ext-"] {
            let reply = delete_external(&node, &root, id);
            assert_eq!(
                reply.location(),
                Some("/auth/settings/services?s=external-gone"),
                "{id:?}"
            );
        }
        assert_eq!(node.state.users.externals().len(), 2);
        // The history has one event for each add and each delete, and the measures count them.
        assert_eq!(
            metric(
                &node,
                r#"portal_user_admin_total{action="external_add",result="ok"}"#
            ),
            3
        );
        assert_eq!(
            metric(
                &node,
                r#"portal_user_admin_total{action="external_delete",result="ok"}"#
            ),
            1
        );
        wait_until(Duration::from_secs(10), || {
            let changes = page(&node, &root, "/auth/settings/activity?v=changes").body;
            changes.contains("External service deleted")
                && changes.matches("External service added").count() == 3
        });
    });
}

#[test]
fn the_add_form_refuses_a_bad_value_a_known_id_the_limit_and_a_request_that_is_not_the_superadmin()
{
    each_backend(|backend| {
        let node = start_with(Arc::new(backend.fresh()), Some(Duration::ZERO), "");
        // A service that discovery lists uses the id: it wins, and the form refuses the name.
        let mut found = listed(&[("portal.name", "Wiki"), ("portal.href", "/wiki/")]);
        found.id = "ext-wiki".into();
        *node.state.catalog.write().unwrap() = Arc::new(Catalog::build(vec![found], Vec::new()));
        let root = sign_in(&node, "root", PASSWORD);
        insert_account(&node, "alice", "a-long-password-1");
        let alice = sign_in(&node, "alice", "a-long-password-1");
        let code = |reply: Reply| reply.location().map(str::to_string);
        let services = |code: &str| Some(format!("/auth/settings/services?s={code}"));
        for (name, href, expected) in [
            ("", "https://a.example.org/", "external-bad-name"),
            ("---", "https://a.example.org/", "external-bad-name"),
            ("A\nB", "https://a.example.org/", "external-bad-name"),
            (
                &"a".repeat(33),
                "https://a.example.org/",
                "external-bad-name",
            ),
            ("A", "", "external-bad-link"),
            ("A", "/local/", "external-bad-link"),
            ("A", "javascript:alert(1)", "external-bad-link"),
            ("A", "https://user:pw@a.example.org/", "external-bad-link"),
            ("A", "https://a.example.org/\u{7}", "external-bad-link"),
            ("A", "ftp://a.example.org/", "external-bad-link"),
            ("Wiki", "https://wiki.example.org/", "external-taken"),
        ] {
            assert_eq!(
                code(add_external(&node, &root, name, href)),
                services(expected),
                "{name:?} {href:?}"
            );
        }
        assert!(node.state.users.externals().is_empty());
        assert_eq!(
            node.state.store.load_directory().unwrap().externals.len(),
            0
        );
        // A good value, then the same id again: the name "Team Wiki" and "team-wiki" have one id.
        assert_eq!(
            add_external(&node, &root, "Team Wiki", WIKI_LINK).status,
            303
        );
        assert_eq!(
            code(add_external(
                &node,
                &root,
                "team-wiki",
                "https://other.example.org/"
            )),
            services("external-exists")
        );
        // The limit: the store holds 256 entries at most.
        for n in node.state.users.externals().len()..crate::catalog::external::MAX_SERVICES {
            let entry = crate::catalog::external::ExternalEntry {
                id: format!("ext-bulk-{n}"),
                name: format!("Bulk {n}"),
                href: "https://bulk.example.org/".into(),
                created_at: 1,
                ..Default::default()
            };
            node.state
                .store
                .insert_external(&entry, crate::catalog::external::MAX_SERVICES)
                .unwrap();
            node.state.users.add_external(entry);
        }
        assert_eq!(
            code(add_external(&node, &root, "One more", WIKI_LINK)),
            services("external-limit")
        );
        // An account that is not the superadmin cannot add, delete or open the page.
        assert_eq!(add_external(&node, &alice, "Mine", WIKI_LINK).status, 403);
        assert_eq!(delete_external(&node, &alice, "ext-team-wiki").status, 403);
        assert_eq!(
            page(&node, &alice, "/auth/settings/services").status,
            404,
            "the page does not exist for an account"
        );
        assert_eq!(node.state.users.externals().len(), 256);
        // A field that the form does not have, a bad token, a bad origin and a bad type.
        assert_eq!(
            post(
                &node,
                &root,
                EXTERNAL_ADD_PATH,
                USERS_TOKEN,
                "name=A&href=https%3A%2F%2Fa.example.org%2F&grant0=x",
            )
            .status,
            400
        );
        assert_eq!(
            post(
                &node,
                &root,
                EXTERNAL_ADD_PATH,
                "other-purpose",
                "name=A&href=https%3A%2F%2Fa.example.org%2F",
            )
            .status,
            403,
            "the token of another form"
        );
        let cookie = format!("{}={root}", session_cookie_name(&node));
        let csrf = sign_action_token(&node.state.signing_secret, USERS_TOKEN, &root).unwrap();
        let body = format!("csrf={csrf}&name=A&href=https%3A%2F%2Fa.example.org%2F");
        assert_eq!(
            send(
                node.port,
                "POST",
                EXTERNAL_ADD_PATH,
                &[
                    ("Origin", "https://evil.example"),
                    ("Content-Type", "application/x-www-form-urlencoded"),
                    ("Cookie", &cookie),
                ],
                &body,
            )
            .status,
            403
        );
        assert_eq!(
            send(
                node.port,
                "POST",
                EXTERNAL_ADD_PATH,
                &[
                    ("Origin", &format!("http://127.0.0.1:{}", node.port)),
                    ("Content-Type", "text/plain"),
                    ("Cookie", &cookie),
                ],
                &body,
            )
            .status,
            415
        );
        assert_eq!(
            send(
                node.port,
                "POST",
                EXTERNAL_ADD_PATH,
                &[
                    ("Origin", &format!("http://127.0.0.1:{}", node.port)),
                    ("Content-Type", "application/x-www-form-urlencoded"),
                ],
                &body,
            )
            .location(),
            Some(LOGIN_PATH),
            "no session"
        );
        // A GET is not a route of the form.
        assert_eq!(
            send(
                node.port,
                "GET",
                EXTERNAL_ADD_PATH,
                &[("Cookie", &cookie)],
                ""
            )
            .status,
            404
        );
    });
}

#[test]
fn an_external_service_that_discovery_lists_later_wins_and_the_page_says_so() {
    let node = start_with(Arc::new(Backend::Sqlite.fresh()), Some(Duration::ZERO), "");
    publish_catalog(&node, &[]);
    let root = sign_in(&node, "root", PASSWORD);
    assert_eq!(add_external(&node, &root, "Wiki", WIKI_LINK).status, 303);
    assert!(page(&node, &root, "/")
        .body
        .contains("External link. This server does not protect it."));
    let mut found = listed(&[("portal.name", "Wiki"), ("portal.href", "/wiki/")]);
    found.id = "ext-wiki".into();
    *node.state.catalog.write().unwrap() = Arc::new(Catalog::build(vec![found], Vec::new()));
    let launcher = page(&node, &root, "/");
    assert!(!launcher
        .body
        .contains("External link. This server does not protect it."));
    assert!(launcher.body.contains(r#"href="/wiki/""#));
    assert!(
        launcher.body.contains("a discovered service has this id"),
        "the superadmin sees the problem: {}",
        launcher.body
    );
}

#[test]
fn the_other_instance_sees_an_external_service_that_was_added_or_deleted_here() {
    each_backend(|backend| {
        let (a, b) = cluster(backend, Some(Duration::ZERO));
        publish_catalog(&a, &[]);
        publish_catalog(&b, &[]);
        let root = sign_in(&a, "root", PASSWORD);
        insert_account(&a, "alice", "a-long-password-1");
        crate::sync::run_round(&b.state, 1);
        assert_eq!(add_external(&a, &root, "Team wiki", WIKI_LINK).status, 303);
        change_grants(
            &a,
            "alice",
            vec![Grant::new("access", "ext-team-wiki").unwrap()],
        );
        let alice = sign_in(&a, "alice", "a-long-password-1");
        // The periodic round of the other instance reads the directory. The catalogue merges the new entry at once.
        crate::sync::run_round(&b.state, 2);
        let launcher = page(&b, &alice, "/");
        assert!(launcher.body.contains("Team wiki"), "{}", launcher.body);
        assert!(launcher
            .body
            .contains("External link. This server does not protect it."));
        assert!(launcher
            .body
            .contains(r#"href="https://wiki.example.org/start?a=1&amp;b=2""#));
        let root_on_b = sign_in(&b, "root", PASSWORD);
        assert!(page(&b, &root_on_b, "/auth/settings/services")
            .body
            .contains("badge badge--external"));
        // The page that the redirect after "Add" selects opens at once on the other instance.
        let fresh = add_external(&a, &root, "Docs", "https://docs.example.org/");
        assert_eq!(fresh.status, 303);
        let selected = page(&b, &root_on_b, fresh.location().unwrap());
        assert_eq!(selected.status, 200);
        assert!(selected.body.contains(r#"id="detail-title">Docs</h2>"#));

        // A delete on the other instance reaches this one, with the grants.
        assert_eq!(delete_external(&b, &root_on_b, "ext-team-wiki").status, 303);
        crate::sync::run_round(&a.state, 3);
        assert!(page(&a, &alice, "/").body.contains("No service available"));
        assert!(a.state.users.grants_of("alice").unwrap().is_empty());
        assert_eq!(
            a.state.users.externals().len(),
            1,
            "only Docs is left on the first instance"
        );
        // A delete of an entry that this instance has not read yet finds it in the shared database.
        let later = add_external(&a, &root, "Later", "https://later.example.org/");
        assert_eq!(later.status, 303);
        let gone = delete_external(&b, &root_on_b, "ext-later");
        assert_eq!(
            gone.location(),
            Some("/auth/settings/services?s=external-deleted")
        );
    });
}

#[test]
fn a_node_that_cannot_change_grants_has_no_form_that_adds_an_external_service() {
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
    assert_eq!(
        add_external(&node, &root, "Team wiki", WIKI_LINK).status,
        404,
        "the route does not exist, as the route that adds an account"
    );
    assert!(node.state.users.externals().is_empty());
    let services = page(&node, &root, "/auth/settings/services");
    assert_eq!(services.status, 200);
    assert!(!services.body.contains("Add external service") && !services.body.contains("extForm"));
}

// External services of the file, the own icon and the form with the description ------------------------

const SVG_ICON: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 8 8"><rect width="8" height="8" fill="#336"/></svg>"##;
const PNG_BYTES: &[u8] =
    b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR\0\0\0\x01\0\0\0\x01\x08\x06\0\0\0\x1f\x15\xc4\x89";

fn data_uri(mime: &str, bytes: &[u8]) -> String {
    use base64ct::{Base64, Encoding};
    format!("data:{mime};base64,{}", Base64::encode_string(bytes))
}

fn file_services() -> String {
    format!(
        r#"external_service "Team wiki" {{
          href        = "https://file.example.org/wiki"
          description = "The wiki of the <team>"
          icon        = "{}"
        }}
        external_service "Plain" {{ href = "http://[::1]:8080/" }}"#,
        data_uri("image/svg+xml", SVG_ICON.as_bytes())
    )
}

fn add_external_full(
    node: &Node,
    session: &str,
    name: &str,
    href: &str,
    description: &str,
    icon: &str,
) -> Reply {
    post(
        node,
        session,
        EXTERNAL_ADD_PATH,
        USERS_TOKEN,
        &format!(
            "name={}&href={}&description={}&icon={}",
            percent_encode_query_component(name),
            percent_encode_query_component(href),
            percent_encode_query_component(description),
            percent_encode_query_component(icon)
        ),
    )
}

#[test]
fn a_service_of_the_file_has_its_tile_and_wins_over_the_row_of_the_database_that_stays() {
    each_backend(|backend| {
        let node = start_with(
            Arc::new(backend.fresh()),
            Some(Duration::ZERO),
            &file_services(),
        );
        assert_eq!(node.state.config.access.external_services.len(), 2);
        let root = sign_in(&node, "root", PASSWORD);
        // A row of the database with the id of a service of the file, and one with an id of its own.
        let row = |id: &str, name: &str, href: &str| crate::catalog::external::ExternalEntry {
            id: id.into(),
            name: name.into(),
            href: href.into(),
            created_at: 1,
            ..Default::default()
        };
        for entry in [
            row("ext-team-wiki", "Team wiki", "https://row.example.org/"),
            row("ext-docs", "Docs", "https://docs.example.org/"),
        ] {
            node.state.store.insert_external(&entry, 10).unwrap();
            node.state.users.add_external(entry);
        }
        let launcher = page(&node, &root, "/");
        assert_eq!(launcher.status, 200);
        // The file wins: its link, its description and its icon. The row of the database is ignored.
        assert!(
            launcher
                .body
                .contains(r#"href="https://file.example.org/wiki""#),
            "{}",
            launcher.body
        );
        assert!(!launcher.body.contains("row.example.org"));
        assert!(launcher
            .body
            .contains("The wiki of the &lt;team&gt;</span>"));
        assert!(!launcher.body.contains("<team>"));
        assert!(launcher
            .body
            .contains(r#"src="/auth/ext-icon/ext-team-wiki""#));
        // The other services show too, and the tile of the database service has no own icon.
        assert!(launcher.body.contains(r#"href="http://[::1]:8080/""#));
        assert!(launcher
            .body
            .contains(r#"href="https://docs.example.org/""#));
        assert!(!launcher.body.contains("/auth/ext-icon/ext-docs"));
        assert_eq!(
            launcher
                .body
                .matches("External link. This server does not protect it.")
                .count(),
            3
        );
        // The ignored row is not deleted, and it makes no problem.
        let rows = node.state.store.load_directory().unwrap().externals;
        assert!(rows
            .iter()
            .any(|e| e.id == "ext-team-wiki" && e.href == "https://row.example.org/"));
        assert!(current_catalog(&node.state).problems().is_empty());

        // The Services page: the badge `file` on the service of the file, read-only, and the trash icon on the other.
        let wiki = page(&node, &root, "/auth/settings/services?svc=ext-team-wiki");
        assert_eq!(wiki.status, 200);
        assert!(wiki
            .body
            .contains(r#"title="Defined in the configuration file">file</span>"#));
        assert!(wiki
            .body
            .contains(r#"<p class="extDesc">The wiki of the &lt;team&gt;</p>"#));
        assert!(!wiki.body.contains(r#"action="/auth/services/delete""#));
        let docs = page(&node, &root, "/auth/settings/services?svc=ext-docs");
        assert!(docs.body.contains(r#"action="/auth/services/delete""#));
        // The delete of the service of the file is refused, and so is a new name with the same id.
        let refused = delete_external(&node, &root, "ext-team-wiki");
        assert_eq!(
            refused.location(),
            Some("/auth/settings/services?s=external-in-file")
        );
        assert_eq!(
            add_external(&node, &root, "TEAM WIKI", "https://x.example.org/").location(),
            Some("/auth/settings/services?s=external-in-file")
        );
        assert_eq!(
            add_external(&node, &root, "Plain", "https://x.example.org/").location(),
            Some("/auth/settings/services?s=external-in-file")
        );
        assert!(node
            .state
            .store
            .load_directory()
            .unwrap()
            .externals
            .iter()
            .any(|e| e.id == "ext-team-wiki"));

        // The grant `access` on the id of the file shows the tile to an account.
        insert_account(&node, "alice", "a-long-password-1");
        let alice = sign_in(&node, "alice", "a-long-password-1");
        assert!(!page(&node, &alice, "/").body.contains("Team wiki"));
        assert_eq!(
            post(
                &node,
                &root,
                USERS_UPDATE_PATH,
                USERS_TOKEN,
                "grants=1&login=alice&grant0=access+on+service%3Aext-team-wiki"
            )
            .location(),
            Some("/auth/settings/users/alice?s=user-updated")
        );
        let tile = page(&node, &alice, "/");
        assert!(tile.body.contains("Team wiki") && !tile.body.contains("Plain"));
        // A grant other than `access` on it is refused, as for a service of the database.
        assert_eq!(
            post(
                &node,
                &root,
                USERS_UPDATE_PATH,
                USERS_TOKEN,
                "grants=1&login=alice&grant0=access+on+service%3Aext-team-wiki&grant1=query.run.read+on+service%3Aext-team-wiki"
            )
            .location(),
            Some("/auth/settings/users/alice?s=edit-unknown-grant")
        );
    });
}

#[test]
fn a_service_of_the_file_works_in_the_mode_static_without_the_form() {
    let node = start_custom(
        Arc::new(Backend::Sqlite.fresh()),
        None,
        Custom {
            allow_creation: false,
            ..Custom::default()
        },
        &format!(r#"access {{ grants_mode = "static" }} {}"#, file_services()),
    );
    let root = sign_in(&node, "root", PASSWORD);
    let launcher = page(&node, &root, "/");
    assert!(launcher.body.contains("Team wiki") && launcher.body.contains("Plain"));
    let services = page(&node, &root, "/auth/settings/services?svc=ext-plain");
    assert!(services.body.contains(">file</span>"));
    assert!(!services.body.contains("Add external service"));
    assert_eq!(
        delete_external(&node, &root, "ext-plain").location(),
        Some("/auth/settings/services?s=external-in-file")
    );
}

#[test]
fn the_route_of_the_own_icon_gives_the_image_to_a_session_that_may_open_the_service() {
    each_backend(|backend| {
        let node = start_with(
            Arc::new(backend.fresh()),
            Some(Duration::ZERO),
            &file_services(),
        );
        // A service of the database with a PNG icon, and one with no icon.
        let mut png = crate::catalog::external::ExternalEntry {
            id: "ext-chart".into(),
            name: "Chart".into(),
            href: "https://chart.example.org/".into(),
            created_at: 1,
            ..Default::default()
        };
        png.icon = Some(Arc::new(
            crate::catalog::image::Image::from_bytes(PNG_BYTES.to_vec(), None).unwrap(),
        ));
        let bare = crate::catalog::external::ExternalEntry {
            id: "ext-bare".into(),
            name: "Bare".into(),
            href: "https://bare.example.org/".into(),
            created_at: 1,
            ..Default::default()
        };
        for entry in [png, bare] {
            node.state.store.insert_external(&entry, 10).unwrap();
            node.state.users.add_external(entry);
        }
        let root = sign_in(&node, "root", PASSWORD);
        let get = |session: Option<&str>, path: &str, extra: &[(&str, &str)]| {
            let cookie = session.map(|s| format!("{}={s}", session_cookie_name(&node)));
            let mut headers: Vec<(&str, &str)> = extra.to_vec();
            if let Some(cookie) = &cookie {
                headers.push(("Cookie", cookie));
            }
            send(node.port, "GET", path, &headers, "")
        };

        // The superadmin gets the SVG as it is stored, with the headers of an inert image.
        let reply = get(Some(&root), "/auth/ext-icon/ext-team-wiki", &[]);
        assert_eq!(reply.status, 200, "{}", reply.body);
        assert_eq!(reply.body, SVG_ICON);
        assert_eq!(reply.header("content-type"), Some("image/svg+xml"));
        assert_eq!(reply.header("x-content-type-options"), Some("nosniff"));
        assert_eq!(
            reply.header("content-security-policy"),
            Some("default-src 'none'; style-src 'unsafe-inline'; sandbox")
        );
        assert_eq!(
            reply.header("cross-origin-resource-policy"),
            Some("same-origin")
        );
        assert_eq!(reply.header("cache-control"), Some("private, no-cache"));
        let etag = reply.header("etag").expect("a validator").to_string();
        assert!(etag.starts_with('"') && !etag.starts_with("W/"), "{etag}");
        // A request with the validator gets 304 and no body. A request with another validator gets the image.
        let same = get(
            Some(&root),
            "/auth/ext-icon/ext-team-wiki",
            &[("If-None-Match", &etag)],
        );
        assert_eq!((same.status, same.body.as_str()), (304, ""));
        assert_eq!(same.header("etag"), Some(etag.as_str()));
        assert!(same.header("content-security-policy").is_some());
        let other = get(
            Some(&root),
            "/auth/ext-icon/ext-team-wiki",
            &[("If-None-Match", "\"other\"")],
        );
        assert_eq!(other.status, 200);
        // The image of the database keeps its own type.
        let chart = get(Some(&root), "/auth/ext-icon/ext-chart", &[]);
        assert_eq!(chart.status, 200);
        assert_eq!(chart.header("content-type"), Some("image/png"));
        assert_ne!(chart.header("etag"), Some(etag.as_str()));

        // No session: the answer of any unknown path. The image never leaves.
        let visitor = get(None, "/auth/ext-icon/ext-team-wiki", &[]);
        assert_eq!(visitor.status, 302, "{}", visitor.body);
        assert!(visitor.location().unwrap().starts_with("/auth/login"));
        assert!(!visitor.body.contains("<svg"));
        assert_eq!(
            get(Some("not-a-session"), "/auth/ext-icon/ext-team-wiki", &[]).status,
            302
        );
        // An account that does not hold `access` gets 404, and gets the image when it holds the grant.
        insert_account(&node, "alice", "a-long-password-1");
        let alice = sign_in(&node, "alice", "a-long-password-1");
        let denied = get(Some(&alice), "/auth/ext-icon/ext-team-wiki", &[]);
        assert_eq!(denied.status, 404);
        assert!(!denied.body.contains("<svg xmlns") && denied.header("etag").is_none());
        assert_eq!(
            post(
                &node,
                &root,
                USERS_UPDATE_PATH,
                USERS_TOKEN,
                "grants=1&login=alice&grant0=access+on+service%3Aext-team-wiki"
            )
            .status,
            303
        );
        assert_eq!(
            get(Some(&alice), "/auth/ext-icon/ext-team-wiki", &[]).status,
            200
        );
        assert_eq!(
            get(Some(&alice), "/auth/ext-icon/ext-chart", &[]).status,
            404,
            "no grant on this one"
        );
        // Nothing else answers: no icon, an unknown id, an id that is not external, a bad encoding, a longer path.
        for path in [
            "/auth/ext-icon/ext-bare",
            "/auth/ext-icon/ext-plain",
            "/auth/ext-icon/ext-nothing",
            "/auth/ext-icon/",
            "/auth/ext-icon/portal",
            "/auth/ext-icon/%zz",
            "/auth/ext-icon/ext-team-wiki/extra",
            "/auth/ext-icon/..%2Fsettings",
            "/auth/ext-icon/ext-team-wiki.svg",
        ] {
            let reply = get(Some(&root), path, &[]);
            assert_eq!(reply.status, 404, "{path}: {}", reply.body);
            assert!(!reply.body.contains("<svg xmlns"), "{path}");
        }
        // Only GET.
        let cookie = format!("{}={root}", session_cookie_name(&node));
        let posted = send(
            node.port,
            "POST",
            "/auth/ext-icon/ext-team-wiki",
            &[("Cookie", &cookie)],
            "",
        );
        assert_ne!(posted.status, 200);
        // The deleted service has no icon any more.
        assert_eq!(delete_external(&node, &root, "ext-chart").status, 303);
        assert_eq!(
            get(Some(&root), "/auth/ext-icon/ext-chart", &[]).status,
            404
        );
    });
}

#[test]
fn the_form_stores_the_description_and_the_icon_and_refuses_what_the_rules_refuse() {
    each_backend(|backend| {
        let node = start_with(Arc::new(backend.fresh()), Some(Duration::ZERO), "");
        let root = sign_in(&node, "root", PASSWORD);
        let services = |code: &str| Some(format!("/auth/settings/services?s={code}"));
        let location = |reply: Reply| reply.location().map(str::to_string);
        let link = "https://wiki.example.org/";
        let svg = data_uri("image/svg+xml", SVG_ICON.as_bytes());
        // A good form: the description and the icon are stored, and the tile shows them.
        let added = add_external_full(
            &node,
            &root,
            "Team wiki",
            link,
            "  The wiki  ",
            &format!("{svg}\n"),
        );
        assert_eq!(added.status, 303, "{}", added.body);
        assert_eq!(
            added.location(),
            Some("/auth/settings/services?s=external-added&svc=ext-team-wiki")
        );
        let stored = node.state.store.load_directory().unwrap().externals;
        assert_eq!(stored.len(), 1);
        assert_eq!(stored[0].description, "The wiki");
        assert_eq!(
            stored[0].icon.as_ref().unwrap().bytes(),
            SVG_ICON.as_bytes()
        );
        assert_eq!(node.state.users.externals(), stored);
        let launcher = page(&node, &root, "/");
        assert!(launcher.body.contains("The wiki</span>"));
        assert!(launcher
            .body
            .contains(r#"src="/auth/ext-icon/ext-team-wiki""#));
        assert_eq!(
            page(&node, &root, "/auth/ext-icon/ext-team-wiki").body,
            SVG_ICON
        );
        // The fields are optional.
        assert_eq!(add_external(&node, &root, "Plain", link).status, 303);
        let plain = node
            .state
            .users
            .externals()
            .into_iter()
            .find(|e| e.id == "ext-plain")
            .unwrap();
        assert!(plain.description.is_empty() && plain.icon.is_none());

        // What the rules refuse.
        let jpeg_as_png = data_uri(
            "image/png",
            b"\xff\xd8\xff\xe0\0\x10JFIF\0\x01\x01\0\0\x01\0\x01\0\0",
        );
        let script = data_uri(
            "image/svg+xml",
            br#"<svg xmlns="http://www.w3.org/2000/svg"><script>alert(1)</script></svg>"#,
        );
        let mut big = PNG_BYTES.to_vec();
        big.resize(32 * 1024 + 1, 0);
        for (name, href, description, icon, expected) in [
            ("A", "http://1.2.3/", "", String::new(), "external-bad-link"),
            (
                "A",
                "https://caf\u{e9}.example/",
                "",
                String::new(),
                "external-bad-link",
            ),
            (
                "A",
                "https://a.example.org:0/",
                "",
                String::new(),
                "external-bad-link",
            ),
            (
                "A",
                link,
                &"x".repeat(201),
                String::new(),
                "external-bad-description",
            ),
            (
                "A",
                link,
                "two\nlines",
                String::new(),
                "external-bad-description",
            ),
            (
                "A",
                link,
                "",
                "https://example.org/icon.png".to_string(),
                "external-bad-icon",
            ),
            ("A", link, "", jpeg_as_png, "external-bad-icon"),
            ("A", link, "", script, "external-bad-icon"),
            (
                "A",
                link,
                "",
                data_uri("image/png", &big),
                "external-bad-icon",
            ),
            (
                "A",
                link,
                "",
                data_uri("text/html", b"<html>"),
                "external-bad-icon",
            ),
            (
                "A",
                link,
                "",
                "data:image/png;base64,@@@".to_string(),
                "external-bad-icon",
            ),
        ] {
            assert_eq!(
                location(add_external_full(
                    &node,
                    &root,
                    name,
                    href,
                    description,
                    &icon
                )),
                services(expected),
                "{href:?} {description:?} {}",
                &icon[..icon.len().min(40)]
            );
        }
        assert_eq!(
            node.state.users.externals().len(),
            2,
            "a refused form stores nothing"
        );
        // An unknown field is a bad request.
        let reply = post(
            &node,
            &root,
            EXTERNAL_ADD_PATH,
            USERS_TOKEN,
            "name=A&href=https%3A%2F%2Fa.example.org%2F&colour=red",
        );
        assert_eq!(reply.status, 400);
    });
}

#[test]
fn the_largest_icon_fits_the_form_for_the_superadmin_only() {
    let node = start_with(Arc::new(Backend::Sqlite.fresh()), Some(Duration::ZERO), "");
    let root = sign_in(&node, "root", PASSWORD);
    insert_account(&node, "alice", "a-long-password-1");
    let alice = sign_in(&node, "alice", "a-long-password-1");
    // The largest image, with the Base64 characters that the percent form triples: the body is about 131 KB.
    let mut bytes = PNG_BYTES.to_vec();
    while bytes.len() + 3 <= 32 * 1024 {
        bytes.extend_from_slice(&[0xfb, 0xef, 0xbe]);
    }
    bytes.resize(32 * 1024, 0xff);
    let uri = data_uri("image/png", &bytes);
    let added = add_external_full(&node, &root, "Big", "https://big.example.org/", "d", &uri);
    assert_eq!(added.status, 303, "{}", added.body);
    assert_eq!(
        added.location(),
        Some("/auth/settings/services?s=external-added&svc=ext-big")
    );
    let stored = node.state.users.externals();
    assert_eq!(stored[0].icon.as_ref().unwrap().bytes().len(), 32 * 1024);
    // An account that is not the superadmin gets the limit of the other forms: its body is refused before it is read.
    let refused = add_external_full(&node, &alice, "Big2", "https://big.example.org/", "d", &uri);
    assert_eq!(refused.status, 413);
    assert_eq!(node.state.users.externals().len(), 1);
    // A body above the addition for the icon is refused for the superadmin too.
    let huge = add_external_full(
        &node,
        &root,
        "Huge",
        "https://huge.example.org/",
        "d",
        &"A".repeat(200 * 1024),
    );
    assert_eq!(huge.status, 413);
}

#[test]
fn the_start_refuses_a_row_of_the_database_that_breaks_the_rules_and_names_the_row() {
    let build = |store: Arc<Store>, extra: &str| {
        let text = format!(
            r#"server {{ listen_address = "127.0.0.1:{}" trusted_proxies = [] }}
            authentication {{ superadmin_login_env = "L" superadmin_password_env = "P" mfa = "off" }}
            session {{ secret_mode = "provided" secret_env = "S" }}
            {extra}"#,
            free_port()
        );
        let (config, _) = Config::parse(&text, &|name| match name {
            "L" => Some("root".into()),
            "P" => Some(PASSWORD.into()),
            "S" => Some("0123456789abcdef0123456789abcdef".into()),
            _ => None,
        })
        .unwrap();
        let superadmin = SuperAdmin {
            login: "root".into(),
            password_hash: password_hash(PASSWORD),
            generation: crate::crypto::superadmin_generation(&SECRET, "root", PASSWORD.as_bytes())
                .unwrap(),
        };
        let tracker = Tracker::start(Arc::clone(&store), Duration::from_millis(200), 90).unwrap();
        let access = AccessSecrets {
            identity_key: None,
            pdp_tokens: Vec::new(),
            totp_key: None,
        };
        AppState::new(config, superadmin, SECRET.to_vec(), store, tracker, access).map(|_| ())
    };
    let store = Arc::new(Backend::Sqlite.fresh());
    store.execute_raw(
        "INSERT INTO external_services (id, name, href, created_at) VALUES ('ext-wiki', 'Wiki', 'https://wiki.example.org/', 1)",
    );
    assert_eq!(build(Arc::clone(&store), ""), Ok(()), "a good row starts");
    // A bad link, a bad description, and a bad icon each stop the start with the row and the reason.
    for (sql, words) in [
        (
            "INSERT INTO external_services (id, name, href, created_at) VALUES ('ext-bad', 'Bad', 'http://1.2.3/', 1)",
            vec!["ext-bad", "href", "http://1.2.3/", "IPv4", "fix or delete the row"],
        ),
        (
            "INSERT INTO external_services (id, name, href, created_at) VALUES ('ext-ftp', 'Ftp', 'ftp://example.org/', 1)",
            vec!["ext-ftp", "href", "ftp://example.org/"],
        ),
        (
            "INSERT INTO external_services (id, name, href, created_at, description) VALUES ('ext-ctl', 'Ctl', 'https://a.example.org/', 1, 'a' || char(10) || 'b')",
            vec!["ext-ctl", "description", "control character"],
        ),
        (
            "INSERT INTO external_services (id, name, href, created_at, icon_type, icon) VALUES ('ext-img', 'Img', 'https://a.example.org/', 1, 'image/png', X'68656c6c6f')",
            vec!["ext-img", "icon"],
        ),
    ] {
        let store = Arc::new(Backend::Sqlite.fresh());
        store.execute_raw(sql);
        let error = build(store, "").expect_err(sql);
        for word in words {
            assert!(error.contains(word), "{word}: {error}");
        }
    }
    // A row that a service of the file hides is checked too.
    let store = Arc::new(Backend::Sqlite.fresh());
    store.execute_raw(
        "INSERT INTO external_services (id, name, href, created_at) VALUES ('ext-wiki', 'Wiki', 'http://1.2.3/', 1)",
    );
    let file = r#"access { grants_mode = "hybrid" } external_service "Wiki" { href = "https://wiki.example.org/" }"#;
    let error = build(store, file).expect_err("the hidden row is still a bad row");
    assert!(
        error.contains("ext-wiki") && error.contains("IPv4"),
        "{error}"
    );
    // The grants of the file are checked against the ids of the file and of the database.
    let store = Arc::new(Backend::Sqlite.fresh());
    store.execute_raw(
        "INSERT INTO external_services (id, name, href, created_at) VALUES ('ext-docs', 'Docs', 'https://docs.example.org/', 1)",
    );
    let error = build(
        store,
        r#"access { grants_mode = "hybrid" } user "carol" { grants = ["data.read on service:ext-docs"] }"#,
    )
    .expect_err("a right other than access on a service of the database");
    assert!(
        error.contains("carol") && error.contains("ext-docs"),
        "{error}"
    );
}

#[test]
fn a_key_never_holds_a_right_on_an_external_service() {
    each_backend(|backend| {
        let (node, _pdp, alice) = keys_user(backend, "");
        let entry = crate::catalog::external::ExternalEntry {
            id: "ext-wiki".into(),
            name: "Wiki".into(),
            href: "https://wiki.example.org/".into(),
            created_at: 1,
            ..Default::default()
        };
        node.state.store.insert_external(&entry, 10).unwrap();
        node.state.users.add_external(entry);
        // Even an account that holds `access` on the service does not see it in a form of a key.
        let mut grants = node.state.users.grants_of("alice").unwrap();
        grants.push(Grant::new("access", "ext-wiki").unwrap());
        change_grants(&node, "alice", grants);
        let form = get_new_key(&node, &alice, "");
        assert_eq!(form.status, 200, "{}", form.body);
        assert!(!form.body.contains("ext-wiki"), "no editor line of a link");
        let created = keys_post(
            &node,
            &alice,
            "create",
            "name=link&expires_at=&grant0=access+on+service%3Aext-wiki",
        );
        assert!(
            key_text_in(&created.body).is_none(),
            "no key is made: {}",
            created.body
        );
        assert!(node.state.users.keys().is_empty());
        // The same for a form that mixes a good grant and a link.
        let mixed = keys_post(
            &node,
            &alice,
            "create",
            "name=link&expires_at=&grant0=access+on+service%3Adb&grant1=access+on+service%3Aext-wiki",
        );
        assert!(key_text_in(&mixed.body).is_none());
        assert!(node.state.users.keys().is_empty());
        // The same service as the account sees it: the editor of the account has it.
        let root = sign_in(&node, "root", PASSWORD);
        let account = page(&node, &root, "/auth/settings/users/alice");
        assert!(account.body.contains(r#""id":"ext-wiki""#));
    });
}

#[test]
fn the_start_refuses_a_file_that_gives_a_right_other_than_access_on_an_external_service() {
    let key_text = crate::keys::generate(crate::keys::KeyKind::Mcp)
        .unwrap()
        .to_string();
    let config_with = |user: &str, pdp: &str| {
        let identity = crate::identity::testing::new_key_pem();
        let text = format!(
            r#"server {{ listen_address = "127.0.0.1:{}" trusted_proxies = [] }}
            authentication {{ superadmin_login_env = "L" superadmin_password_env = "P" mfa = "off" }}
            session {{ secret_mode = "provided" secret_env = "S" }}
            access {{ grants_mode = "hybrid" }}
            identity {{ issuer = "i" signing_key_env = "IDENTITY" }}
            pdp {{ listen_address = "127.0.0.1:{}" url = "https://pdp" }}
            keys {{}}
            {pdp}
            {user}"#,
            free_port(),
            free_port()
        );
        let identity = identity.clone();
        let key_text = key_text.clone();
        Config::parse(&text, &move |name| match name {
            "L" => Some("root".into()),
            "P" => Some(PASSWORD.into()),
            "S" => Some("0123456789abcdef0123456789abcdef".into()),
            "IDENTITY" => Some(identity.clone()),
            "K" => Some(key_text.clone()),
            "PDP_TOKEN" => Some(PDP_DB_TOKEN.into()),
            _ => None,
        })
        .unwrap()
        .0
    };
    let store = Backend::Sqlite.fresh();
    let snapshot = |store: &Store| store.load_directory().unwrap();
    let access = config_with(
        r#"user "carol" { grants = ["access on service:ext-wiki"] }"#,
        "",
    );
    let data = config_with(
        r#"user "carol" { grants = ["access on service:ext-wiki", "data.read on service:ext-wiki"] }"#,
        "",
    );
    let path = config_with(
        r#"user "carol" { grants = ["access on service:ext-wiki", "data.read on service:ext-wiki/host:prod"] }"#,
        "",
    );
    let key = config_with(
        r#"user "carol" { grants = [] key "agent" { key_env = "K" grants = ["access on service:ext-wiki"] } }"#,
        "",
    );
    let client = config_with("", r#"pdp_client "ext-wiki" { token_env = "PDP_TOKEN" }"#);
    // No external service in the database: the start cannot know the id, and it accepts the files.
    for config in [&access, &data, &path, &key, &client] {
        assert!(crate::bootstrap::check_externals(config, &[], &["ext-wiki".to_string()]).is_ok());
    }
    let entry = crate::catalog::external::ExternalEntry {
        id: "ext-wiki".into(),
        name: "Wiki".into(),
        href: "https://wiki.example.org/".into(),
        created_at: 1,
        ..Default::default()
    };
    store.insert_external(&entry, 10).unwrap();
    let names: Vec<String> = Vec::new();
    let ids: Vec<String> = snapshot(&store)
        .externals
        .into_iter()
        .map(|entry| entry.id)
        .collect();
    // `access` on the whole service is the one right that a file account holds on a link.
    assert!(crate::bootstrap::check_externals(&access, &ids, &names).is_ok());
    for (config, words) in [
        (&data, ["carol", "grants", "data.read on service:ext-wiki"]),
        (
            &path,
            ["carol", "grants", "data.read on service:ext-wiki/host:prod"],
        ),
        (&key, ["carol", "agent", "no right on an external service"]),
    ] {
        let error = crate::bootstrap::check_externals(config, &ids, &names).unwrap_err();
        for word in words {
            assert!(error.contains(word), "{word}: {error}");
        }
        assert!(error.contains("ext-wiki"), "{error}");
    }
    let error =
        crate::bootstrap::check_externals(&client, &ids, &["ext-wiki".to_string()]).unwrap_err();
    assert!(
        error.contains("pdp_client") && error.contains("ext-wiki"),
        "{error}"
    );
}

// Latency of a plain evaluation ------------------------------------------------------------------

/// The time of a plain `evaluation` (no `confirm`, no `usage`) over HTTP, for a key and for a person. The test is
/// slow and gives no verdict: run it by hand to compare two versions.
/// `tools/dev/test.sh --release http::cluster_tests::pdp_latency_of_a_plain_evaluation -- --ignored --nocapture`
#[test]
#[ignore]
fn pdp_latency_of_a_plain_evaluation() {
    let (node, port) = key_node(&Backend::Sqlite);
    add_alice(&node, &[]);
    add_key(
        &node,
        "9b1c04e2aa71f0c3",
        Some("alice"),
        1,
        vec![Grant::from_path_text("data.read", "db", "host:prod/database:test2").unwrap()],
    );
    let samples = std::env::var("PDP_LATENCY_SAMPLES")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(20_000);
    for (label, subject) in [("key", "key:9b1c04e2aa71f0c3"), ("person", "alice")] {
        let body = read_body(subject, "test2", serde_json::json!({}));
        let ask = || {
            let begun = Instant::now();
            let reply = pdp(
                port,
                "/access/v1/evaluation",
                Some(PDP_DB_TOKEN),
                &body,
                None,
            );
            let took = begun.elapsed();
            assert_eq!(reply.status, 200, "{}", reply.body);
            took
        };
        for _ in 0..2_000 {
            ask();
        }
        let mut times: Vec<Duration> = (0..samples).map(|_| ask()).collect();
        times.sort();
        let at = |p: f64| times[((times.len() as f64 * p) as usize).min(times.len() - 1)];
        eprintln!(
            "PDP-LATENCY {label}: n={} p50={:?} p90={:?} p99={:?} p999={:?} max={:?}",
            times.len(),
            at(0.50),
            at(0.90),
            at(0.99),
            at(0.999),
            times[times.len() - 1]
        );
    }
}
