//! The portal as a whole, driven over HTTP the way Traefik and a browser drive it. With PostgreSQL two
//! instances share one database (and one signing key, as the configuration demands); with SQLite
//! there is one, and both "instances" of a test are it. Real servers on local ports, so the
//! blocking calls to the database run on the async workers exactly as in production.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::sync::OnceLock;
use std::time::Duration;

use super::*;
use crate::store::testing::{each_backend, Backend};

const PASSWORD: &str = "correct-horse-battery-staple";
const SECRET: [u8; 32] = [b'k'; 32];
const AGENT: &str = "ClusterTest/1.0";

struct Reply {
    status: u16,
    headers: Vec<(String, String)>,
}

impl Reply {
    /// The value a `Set-Cookie` gives to the cookie `name`.
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
    let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(20)))
        .unwrap();
    stream.write_all(request.as_bytes()).unwrap();
    let mut raw = Vec::new();
    stream.read_to_end(&mut raw).unwrap();
    let text = String::from_utf8_lossy(&raw);
    let (head, _) = text.split_once("\r\n\r\n").expect("a complete response");
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
    Reply { status, headers }
}

#[derive(Clone)]
struct Node {
    port: u16,
    state: Arc<AppState>,
}

fn password_hash() -> String {
    static HASH: OnceLock<String> = OnceLock::new();
    HASH.get_or_init(|| hash_password(PASSWORD.as_bytes()).unwrap())
        .clone()
}

fn start(store: Arc<Store>, cache_ttl: Option<Duration>) -> Node {
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let config = Config::parse(&format!(
        r#"
        server {{ listen_address = "127.0.0.1:{port}" workers = 2 }}
        authentication {{
          superadmin_login_file    = "/run/secrets/login"
          superadmin_password_file = "/run/secrets/password"
        }}
        roles {{ names = "ops, dev" }}
        users {{ max_users = 10 }}
        session {{ secret_mode = "file" secret_file = "/run/secrets/session" }}
        "#
    ))
    .unwrap();
    let tracker = Tracker::start(Arc::clone(&store), Duration::from_millis(200), 90).unwrap();
    let superadmin = SuperAdmin {
        login: "root".into(),
        password_hash: password_hash(),
    };
    let mut state = AppState::new(config, superadmin, SECRET.to_vec(), store, tracker).unwrap();
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
    node
}

/// Two nodes on one database, or the one SQLite node twice.
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

/// Signs in the way the login form does, and returns the session cookie.
fn sign_in(node: &Node, login: &str, password: &str) -> String {
    let csrf = "A".repeat(32);
    let signature = sign_return_uri(&SECRET, "/").unwrap();
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

/// What Traefik gets from ForwardAuth: 204 lets the request through, 302 sends it to the login page.
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

/// A POST of the settings or of the launcher, with the anti-CSRF token those forms carry.
fn post(node: &Node, session: &str, path: &str, purpose: &str, fields: &str) -> Reply {
    let origin = format!("http://127.0.0.1:{}", node.port);
    let cookie = format!("{}={session}", session_cookie_name(node));
    let csrf = sign_action_token(&SECRET, purpose, session).unwrap();
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
        // The other instance may keep what it checked for a few seconds, and no longer.
        let waited = wait_until(Duration::from_secs(10), || {
            verify(&b, &session, AGENT) == 302
        });
        assert!(
            waited < Duration::from_secs(6),
            "{} took {waited:?} to notice the logout",
            backend.name()
        );

        // The history says how it ended, whoever wrote it.
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
                &format!("login={login}&password=a-long-password-1&r_ops=1"),
            )
        };
        assert_eq!(add("carol").status, 303);
        assert!(a.state.users.get("carol").is_some());

        // The periodic refresh of the other instance brings it in.
        crate::sync::run_round(&b.state, 1);
        assert_eq!(b.state.users.roles_of("carol").unwrap(), ["ops"]);
        let on_b = sign_in(&b, "carol", "a-long-password-1");
        assert_eq!(
            verify(&a, &on_b, AGENT),
            204,
            "and her session is good on the first one"
        );

        // Without waiting for a refresh: the login that is not known yet makes the instance look.
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
            "login=erin&password=a-long-password-1&r_dev=1",
        );
        assert_eq!(reply.status, 303);
        let erin = sign_in(&a, "erin", "a-long-password-1");
        // `b` never refreshed its accounts: the session must not be taken for one of a deleted account.
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
            "login=frank&password=a-long-password-1&r_ops=1",
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
        // The same database under a new process: a restart.
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
