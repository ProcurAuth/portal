//! Black-box end-to-end test of the inbox: it runs the real `portal` binary with a combined
//! configuration and drives it with the real protocol 4 of the phones. A phone enrolls as the app
//! does: an account of the portal makes a code on the page "My phones" of the tab Inbox
//! (`POST /auth/settings/inbox/code`), and the phone uses it on `/v1/enroll` (Noise IK, no OPAQUE).
//! Then it opens Noise sessions. The admin checks read the pages of the tab Inbox with the session
//! of the superadmin. The rows of these pages carry `data-*` attributes for the tests.

mod common;

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use futures_util::{SinkExt, StreamExt};
use inbox_core::enroll::EnrollCode;
use inbox_core::noise::Initiator;
use inbox_core::proto::{AckStage, AlertMsg, ClientMsg, ServerMsg, SessionHello};
use serde_json::{json, Value};
use tokio_tungstenite::tungstenite::Message;

use common::*;

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
        // The phones have no list of proxies of their own: they use `server.trusted_proxies`.
        (
            "    max_devices = 10",
            "    max_devices = 10\n    trusted_proxies = []",
            "unknown configuration key: inbox.devices.trusted_proxies",
        ),
        // The phones use the listener of the portal: the key of the old phone port is gone, with no alias.
        (
            "    max_devices = 10",
            "    max_devices = 10\n    listen = \"127.0.0.1:1\"",
            "unknown configuration key: inbox.devices.listen",
        ),
        (
            "resend_after = \"1s\"",
            "resend_after = 1",
            "inbox.devices.resend_after",
        ),
        (
            "inbox {\n",
            "inbox {\n  server_key = \"abc\"",
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
    // The public address of the portal, as the phones see it, is mandatory and checked.
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
}

/// One port serves the pages, the API of the producers and the phones. A phone enrolls and gets an
/// alert on the same port as `/auth/login`. The routes of the inbox need no session of the portal and
/// set no cookie.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn one_port_serves_the_pages_the_producers_and_the_phones() {
    let mut s = Server::setup();
    s.start();
    let (pages, phones) = (s.port, s.port);
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
    assert_eq!(got.kind, inbox_core::proto::Kind::Alert);
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

    // The routes of the phones are on the port of the pages. A plain GET is not an upgrade.
    let (st, body) = http(phones, "GET", "/v1/session", &[], "");
    assert_eq!(st, 400, "a plain GET is not an upgrade: {body}");
    let (st, body) = http(phones, "GET", "/v1/enroll", &[], "");
    assert_eq!(st, 400, "a plain GET is not an upgrade: {body}");
    let (st, _) = http(phones, "POST", "/v1/session", &[], "");
    assert_eq!(st, 405);
    let (st, _) = http(phones, "POST", "/v1/enroll", &[], "");
    assert_eq!(st, 405);
    // The inbox owns the prefix `/v1/`: an unknown route is 404 and never a page of the portal.
    for path in ["/v1/", "/v1/nothing", "/v1/session/x"] {
        let (st, _) = http(phones, "GET", path, &[], "");
        assert_eq!(st, 404, "{path}");
    }
    // The upgrade sets no cookie and needs no session. The answer is a plain 101.
    let (mut ws, response) =
        tokio_tungstenite::connect_async(format!("ws://127.0.0.1:{phones}/v1/session"))
            .await
            .expect("the upgrade works on the port of the pages");
    assert_eq!(response.status(), 101);
    assert!(
        response.headers().get("set-cookie").is_none(),
        "a route of the phones sets no cookie: {:?}",
        response.headers()
    );
    // A bad first frame ends the session: the server closes the connection.
    let _ = ws.send(Message::Binary(vec![1u8; 10].into())).await;
    let mut closed = false;
    while let Ok(Some(frame)) = tokio::time::timeout(Duration::from_secs(5), ws.next()).await {
        if frame.is_err() || matches!(frame, Ok(Message::Close(_))) {
            closed = true;
            break;
        }
    }
    assert!(
        closed,
        "the inbox closes a session that starts with a bad frame"
    );
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
    assert!(!home.contains(">Inbox<"), "no tile: {home}");
    s.stop();
}

/// The phone sessions have their own budget on the one listener: `server.max_connections` plus two times
/// `inbox.devices.max_devices`. A connection that upgrades gives its permit of the pages back. With
/// `max_connections = 3` and `max_devices = 1` the budget is 5 sessions: the sixth gets 503 before the
/// upgrade, the pages still answer, and a closed session gives its permit back.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn phone_sessions_have_a_budget_that_the_pages_do_not_share() {
    let mut s = Server::setup();
    let text = std::fs::read_to_string(s.config()).unwrap();
    assert!(text.contains("  trusted_proxies = []\n"));
    assert!(text.contains("    max_devices = 10\n"));
    std::fs::write(
        s.config(),
        text.replacen(
            "  trusted_proxies = []\n",
            "  trusted_proxies = []\n  max_connections = 3\n  max_login_body_reads = 2\n",
            1,
        )
        .replacen(
            "    max_devices = 10\n",
            "    max_devices = 1\n    max_per_account = 1\n",
            1,
        ),
    )
    .unwrap();
    s.start();
    let port = s.port;
    let open = |path: &'static str| async move {
        tokio_tungstenite::connect_async(format!("ws://127.0.0.1:{port}{path}")).await
    };
    // Five sessions fill the budget. Two of them use the enrollment route.
    let mut held = Vec::new();
    for path in [
        "/v1/session",
        "/v1/session",
        "/v1/session",
        "/v1/enroll",
        "/v1/enroll",
    ] {
        let (ws, response) = open(path).await.expect("a free permit");
        assert_eq!(response.status(), 101);
        held.push(ws);
    }
    // The sixth attempt gets 503 before the upgrade, on both routes. No WebSocket exists for it.
    for path in ["/v1/session", "/v1/enroll"] {
        match open(path).await {
            Err(tokio_tungstenite::tungstenite::Error::Http(response)) => {
                assert_eq!(response.status(), 503, "{path}");
                assert!(response.headers().get("retry-after").is_some());
            }
            other => panic!("{path}: the budget is full, got {:?}", other.map(|_| ())),
        }
    }
    let (st, body) = http(port, "GET", "/v1/session", &[], "");
    assert_eq!((st, body.trim()), (503, "no free session"));
    // The pages and the API keep their own budget: the sessions hold none of it.
    for _ in 0..3 {
        let (st, body) = http(port, "GET", "/auth/login", &[], "");
        assert_eq!(st, 200, "{body}");
    }
    let (st, body) = http(port, "GET", "/readyz", &[], "");
    assert_eq!(st, 200, "{body}");
    let auth = ("Authorization", format!("Bearer {}", s.token));
    let (st, body) = http(
        port,
        "POST",
        "/api/v1/alerts",
        std::slice::from_ref(&auth),
        r#"{"group":"infra","title":"while the session budget is full"}"#,
    );
    assert_eq!(st, 201, "{body}");
    // A session that ends gives its permit back.
    let mut first = held.remove(0);
    let _ = first.close(None).await;
    drop(first);
    wait_for("a free session permit", Duration::from_secs(10), || {
        let (st, _) = http(port, "GET", "/v1/session", &[], "");
        st == 400
    });
    let (again, response) = open("/v1/session").await.expect("the permit came back");
    assert_eq!(response.status(), 101);
    drop((again, held));
    s.stop();
}

/// The audit lead portal/inbox-api/body-read-before-token-no-gate: the API of the senders checks
/// the key before it reads the body. Slow bodies without a valid key get `401` at once, on each
/// route that reads a body. They do not hold the connections of `server.max_connections` that
/// ForwardAuth and the probes need for `server.body_read_timeout`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn slow_bodies_without_a_key_get_401_at_once_and_hold_no_connection() {
    let mut s = Server::setup();
    let text = std::fs::read_to_string(s.config()).unwrap();
    assert!(text.contains("  trusted_proxies = []\n"));
    std::fs::write(
        s.config(),
        text.replacen(
            "  trusted_proxies = []\n",
            "  trusted_proxies = []\n  max_connections = 4\n  max_login_body_reads = 2\n  body_read_timeout = \"30s\"\n",
            1,
        ),
    )
    .unwrap();
    s.start();
    let port = s.port;
    let begun = Instant::now();
    let mut held = Vec::new();
    for path in [
        "/api/v1/alerts",
        "/api/v1/notify/infra",
        "/api/v1/webhooks/alertmanager/infra",
        "/api/v1/alerts?token=api_not_a_key",
    ] {
        let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        // The head is complete, the body has one byte of 1000.
        let head = format!(
            "POST {path} HTTP/1.1\r\nHost: localhost:{port}\r\nContent-Type: application/json\r\nContent-Length: 1000\r\n\r\nx"
        );
        stream.write_all(head.as_bytes()).unwrap();
        held.push((path, stream));
    }
    for (path, stream) in &mut held {
        let mut status = [0u8; 12];
        stream.read_exact(&mut status).unwrap_or_else(|e| {
            panic!("{path}: no answer before the body: {e}");
        });
        assert_eq!(&status, b"HTTP/1.1 401", "{path}");
    }
    assert!(
        begun.elapsed() < Duration::from_secs(10),
        "{:?}",
        begun.elapsed()
    );
    // The connections of the slow bodies hold no permit of the four: the pages and the probes
    // answer while the clients keep them open.
    for path in ["/readyz", "/auth/login", "/readyz", "/auth/login"] {
        let (st, body) = http(port, "GET", path, &[], "");
        assert_eq!(st, 200, "{path}: {body}");
    }
    drop(held);
    s.stop();
}

/// With the block `inbox`, the listener of the portal opens before the catalogue is complete and serves
/// the routes of the inbox only: the API of the producers and the routes of the phones. An alert and a
/// phone never wait for the catalogue. Every other route of the portal fails closed with 503. A stop in
/// this time ends the process.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_inbox_answers_before_the_catalogue_is_complete() {
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
    let (pages, phones) = (s.port, s.port);
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
    // The phones reach the inbox at once on the same port: a plain GET is not an upgrade.
    for path in ["/v1/session", "/v1/enroll"] {
        let (st, body) = http(phones, "GET", path, &[], "");
        assert_eq!(st, 400, "{path}: the phones reach the inbox: {body}");
    }
    let (mut ws, response) =
        tokio_tungstenite::connect_async(format!("ws://127.0.0.1:{phones}/v1/session"))
            .await
            .expect("a phone connects before the catalogue is complete");
    assert_eq!(response.status(), 101);
    let _ = ws.close(None).await;
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
        s.port,
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
        s.port,
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
        s.port,
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
        s.port,
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
    let ap = s.port;
    let host = format!("127.0.0.1:{ap}");
    for path in ["/auth/settings/inbox", "/auth/settings/inbox?open=none"] {
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
    let send = format!("csrf={csrf}&to=group:other&title=manual&key=m1");
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
            "to=group:other&title=manual&key=m1"
        ),
        403,
        "POST without the anti-CSRF token"
    );
    assert_eq!(
        post(
            &[form_type.clone(), origin.clone()],
            &format!("csrf={}&to=group:other&title=manual&key=m1", "x".repeat(43))
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
            "/auth/settings/inbox?open={}",
            v["alert_id"].as_str().unwrap()
        ),
    );
    assert_eq!(st, 200);
    assert!(
        page.contains("Received</b> 0/0"),
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
    // The row of an alert opens in place and shows its deliveries. There is no page of one alert.
    let (st, page) = inbox_page(
        &s,
        &format!(
            "/auth/settings/inbox?open={}",
            all[0]["id"].as_str().unwrap()
        ),
    );
    assert_eq!(st, 200, "{page}");
    assert!(page.contains("data-detail="), "{page}");
    let (st, _) = inbox_page(
        &s,
        &format!(
            "/auth/settings/inbox/alerts/{}",
            all[0]["id"].as_str().unwrap()
        ),
    );
    assert_eq!(st, 404, "the page of one alert is gone");
    let (st, _) = admin(&s, "POST", "/auth/inbox/api/login/start", "{}");
    assert_eq!(st, 404, "no login endpoint any more");

    // The metrics of the inbox are in the Prometheus text of the portal, with their old names.
    let (st, metrics) = http_host(
        s.port,
        &format!("127.0.0.1:{}", s.port),
        "GET",
        "/metrics",
        &[],
        "",
    );
    assert_eq!(st, 200);
    for expected in [
        "portal_inbox_alerts_accepted_total{source=\"webhook\",kind=\"alert\",target=\"group\"} 1",
        "portal_inbox_alerts_accepted_total{source=\"admin\",kind=\"alert\",target=\"group\"} 1",
        "portal_inbox_alerts_duplicate_total{source=\"webhook\",kind=\"alert\",target=\"group\"} 1",
        "portal_inbox_delivery_receipts_total ",
        "portal_inbox_delivery_latency_seconds_count ",
        "portal_inbox_deliveries_pending ",
        "portal_inbox_oldest_pending_delivery_seconds ",
        "portal_inbox_devices_unreachable ",
        "portal_inbox_devices_enrolled ",
        "portal_inbox_storage_degraded 0",
        "portal_inbox_spool_alerts 0",
    ] {
        assert!(metrics.contains(expected), "{expected}\n{metrics}");
    }
    assert!(
        !metrics.contains("portal_inbox_replica_epoch"),
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

/// Lets the proxy 127.0.0.1 give the client address in `X-Forwarded-For`, so one test plays
/// several client addresses. `more` is added to the block `inbox.devices`.
fn behind_proxy(s: &Server, more: &str) {
    let text = std::fs::read_to_string(s.config()).unwrap();
    assert!(text.contains("  trusted_proxies = []\n") && text.contains("    max_devices = 10\n"));
    std::fs::write(
        s.config(),
        text.replacen(
            "  trusted_proxies = []\n",
            "  trusted_proxies = [\"127.0.0.1/32\"]\n",
            1,
        )
        .replacen(
            "    max_devices = 10\n",
            &format!("    max_devices = 10\n{more}"),
            1,
        ),
    )
    .unwrap();
}

/// A WebSocket to `path` that the trusted proxy 127.0.0.1 forwards for the client address `from`.
async fn ws_from(port: u16, path: &str, from: &str) -> Ws {
    use tokio_tungstenite::tungstenite::client::IntoClientRequest;
    let mut request = format!("ws://127.0.0.1:{port}{path}")
        .into_client_request()
        .unwrap();
    request
        .headers_mut()
        .insert("x-forwarded-for", from.parse().unwrap());
    tokio_tungstenite::connect_async(request).await.unwrap().0
}

/// Noise message 1 and message 2 of a session of `dev`, on a WebSocket of the client address
/// `from`. The frame `ready` is not sent. `Err` is the close code of a refusal.
async fn half_open_from(port: u16, dev: &Phone, from: &str) -> Result<Conn, Option<u16>> {
    let mut ws = ws_from(port, "/v1/session", from).await;
    let hello = serde_json::to_vec(&SessionHello {
        protocol: inbox_core::PROTOCOL_VERSION,
        device_id: dev.device_id.clone(),
        app_version: "e2e".into(),
    })
    .unwrap();
    let (init, m1) =
        Initiator::start(&dev.device_private_key, &dev.server_public_key, &hello).unwrap();
    ws.send(Message::Binary(m1.into())).await.unwrap();
    let m2 = match next_frame(&mut ws).await {
        Some(Message::Binary(b)) => b,
        Some(Message::Close(f)) => return Err(f.map(|f| f.code.into())),
        other => panic!("unexpected handshake reply: {other:?}"),
    };
    let (session, _accept) = init.finish(&m2).unwrap();
    Ok(Conn {
        ws,
        session,
        seen: Default::default(),
    })
}

/// Sends `ready` on a half-open handshake. The result is the welcome, or the close code.
async fn ready(conn: &mut Conn) -> Result<ServerMsg, Option<u16>> {
    conn.send(&ClientMsg::Ready).await;
    match conn.recv(Duration::from_secs(5)).await {
        Recv::Msg(m @ ServerMsg::Welcome { .. }) => Ok(m),
        Recv::Msg(other) => panic!("expected welcome, got {other:?}"),
        Recv::Closed(code) => Err(code),
        Recv::Timeout => panic!("no answer to ready"),
    }
}

/// The full session of an enrolled phone from the client address `from`.
async fn session_from(port: u16, dev: &Phone, from: &str) -> Result<Conn, Option<u16>> {
    let mut conn = half_open_from(port, dev, from).await?;
    ready(&mut conn).await.map(|_| conn)
}

/// A client with no key from `from`: one binary frame that is not a Noise message 1.
async fn garbage_from(port: u16, from: &str) -> Option<u16> {
    let mut ws = ws_from(port, "/v1/session", from).await;
    let _ = ws.send(Message::Binary(vec![0x41u8; 48].into())).await;
    close_code(&mut ws).await
}

/// Sends a WebSocket ping every second until the server closes the socket. The result is the time
/// from `opened` and the close code.
async fn ping_until_closed(ws: &mut Ws, opened: Instant) -> (Duration, Option<u16>) {
    loop {
        if ws.send(Message::Ping(vec![1u8].into())).await.is_err() {
            return (opened.elapsed(), None);
        }
        match tokio::time::timeout(Duration::from_secs(1), ws.next()).await {
            Err(_) | Ok(Some(Ok(Message::Pong(_) | Message::Ping(_)))) => {
                assert!(opened.elapsed() < Duration::from_secs(40), "never closed");
            }
            Ok(Some(Ok(Message::Close(f)))) => return (opened.elapsed(), f.map(|f| f.code.into())),
            Ok(Some(Ok(other))) => panic!("unexpected frame {other:?}"),
            Ok(Some(Err(_)) | None) => return (opened.elapsed(), None),
        }
    }
}

/// Finding inbox-session/handshake-timeout-reset-by-ping-holds-budget, part one. The handshake of a
/// session has one deadline of 15 seconds from the upgrade. A ping or a pong does not extend it:
/// a socket that only sends pings, before message 1 or after message 2, gives its permit of the
/// session budget back. Each such timeout counts as a failure of the client address.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_session_handshake_has_one_deadline_that_pings_do_not_extend() {
    let mut s = Server::setup();
    s.start();
    let port = s.port;
    let phone = enroll_as(&s, "deadline-phone", SUPERADMIN).await.unwrap();
    let opened = Instant::now();
    let (mut silent, _) =
        tokio_tungstenite::connect_async(format!("ws://127.0.0.1:{port}/v1/session"))
            .await
            .unwrap();
    let mut half = half_open_from(port, &phone, "127.0.0.1").await.unwrap();
    let (before_m1, after_m2) = tokio::join!(
        ping_until_closed(&mut silent, opened),
        ping_until_closed(&mut half.ws, opened)
    );
    for (what, (elapsed, code)) in [
        ("before message 1", before_m1),
        ("after message 2", after_m2),
    ] {
        assert!(
            elapsed >= Duration::from_secs(13) && elapsed < Duration::from_secs(20),
            "{what}: closed after {elapsed:?}, the deadline is 15 s from the upgrade"
        );
        assert_eq!(code, Some(1001), "{what}");
    }
    let metrics = metrics_of(&s);
    assert!(
        metrics.contains("portal_inbox_auth_failures_total{kind=\"handshake_timeout\"} 2"),
        "{metrics}"
    );
    // The phone itself is not refused: it proves its key.
    session_from(port, &phone, "127.0.0.1").await.unwrap();
    s.stop();
}

/// Finding inbox-session/handshake-timeout-reset-by-ping-holds-budget, part two. One client address
/// holds at most `inbox.devices.max_handshakes_per_address` handshakes, so it cannot take the whole
/// session budget. A newer handshake of a full address ends the oldest one with 1013: a client
/// that holds silent sockets does not keep out an enrolled phone of its address.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn one_client_address_holds_a_limited_share_of_the_handshakes() {
    let mut s = Server::setup();
    behind_proxy(&s, "    max_handshakes_per_address = 2\n");
    s.start();
    let port = s.port;
    const SHARED: &str = "198.51.100.7";
    const OTHER: &str = "203.0.113.9";
    let phone = enroll_as(&s, "share-phone", SUPERADMIN).await.unwrap();
    let other_phone = enroll_as(&s, "share-other", SUPERADMIN).await.unwrap();

    // Five silent sockets of one address: only the two newest stay open.
    let mut silent = Vec::new();
    for _ in 0..5 {
        silent.push(ws_from(port, "/v1/session", SHARED).await);
    }
    let mut codes = Vec::new();
    for ws in silent.iter_mut().take(3) {
        codes.push(close_code(ws).await);
    }
    assert_eq!(codes, [Some(1013); 3], "the oldest handshakes end");
    for ws in silent.iter_mut().skip(3) {
        assert!(
            tokio::time::timeout(Duration::from_millis(300), next_frame(ws))
                .await
                .is_err(),
            "the two newest handshakes stay open"
        );
    }
    // The enrolled phone of the same address gets its session: its handshake ends the oldest.
    let conn = session_from(port, &phone, SHARED)
        .await
        .expect("the phone of the full address gets its session");
    assert_eq!(close_code(&mut silent[3]).await, Some(1013));
    // An established session does not count as a handshake: one more handshake fits next to the
    // last silent socket. The next one ends this socket.
    let first_enroll = ws_from(port, "/v1/enroll", SHARED).await;
    assert!(
        tokio::time::timeout(Duration::from_millis(300), next_frame(&mut silent[4]))
            .await
            .is_err(),
        "the session of the phone does not count as a handshake"
    );
    let second_enroll = ws_from(port, "/v1/enroll", SHARED).await;
    assert_eq!(close_code(&mut silent[4]).await, Some(1013));
    drop((first_enroll, second_enroll));
    // Another address has its own share.
    session_from(port, &other_phone, OTHER).await.unwrap();
    assert!(
        s.log().contains("\"event\":\"handshake_ended_by_newer\""),
        "{}",
        s.log()
    );
    drop((conn, silent));
    s.stop();
}

/// Finding inbox-limiter/ip-scope-blocks-coresident-phones. The failures of a client with no key
/// lock its address for enrollments and for the handshakes that prove no device. A phone of the
/// same address that proves its key still gets its session.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_failures_of_one_client_do_not_refuse_the_phones_of_its_address() {
    let mut s = Server::setup();
    behind_proxy(&s, "");
    s.start();
    let port = s.port;
    const SHARED: &str = "198.51.100.7";
    const OTHER: &str = "203.0.113.9";
    let victim = enroll_as(&s, "victim-phone", SUPERADMIN).await.unwrap();
    let other = enroll_as(&s, "other-phone", SUPERADMIN).await.unwrap();
    let mut early = session_from(port, &other, SHARED).await.unwrap();

    // max_auth_failures = 10 garbage first frames from the shared address.
    for _ in 0..10 {
        assert_eq!(garbage_from(port, SHARED).await, Some(4401));
    }
    // The address is locked for a client that proves nothing, and for an enrollment.
    assert_eq!(garbage_from(port, SHARED).await, Some(4429));
    let mut enroll = ws_from(port, "/v1/enroll", SHARED).await;
    assert_eq!(close_code(&mut enroll).await, Some(4429));
    // The enrolled phone of the same address proves its key: it gets its session, two times.
    for _ in 0..2 {
        let conn = session_from(port, &victim, SHARED).await;
        assert!(conn.is_ok(), "the phone is refused: {:?}", conn.err());
    }
    // A session that was open before the failures goes on.
    early.send(&ClientMsg::Ping { nonce: 21 }).await;
    loop {
        match early.recv(Duration::from_secs(5)).await {
            Recv::Msg(ServerMsg::Pong { nonce: 21 }) => break,
            Recv::Msg(_) => continue,
            other => panic!("the early session ended: {}", describe(&other)),
        }
    }
    // Another address is not locked.
    assert_eq!(garbage_from(port, OTHER).await, Some(4401));
    s.stop();
}

/// Finding inbox/revocation-misses-session-in-handshake. A phone that is revoked between message 2
/// and `ready` gets the close code 4403 after `ready`, no welcome and no alert.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_revocation_during_the_handshake_ends_the_session() {
    let mut s = Server::setup();
    s.start();
    let port = s.port;
    let phone = enroll_as(&s, "revoked-in-handshake", SUPERADMIN)
        .await
        .unwrap();
    let (mut live, _) = Conn::open(port, &phone).await.unwrap();
    live.subscribe("infra").await;
    let (st, _) = post_alert(
        &s,
        "pending-at-revocation",
        json!({"group": "infra", "title": "pending at the revocation"}),
    );
    assert_eq!(st, 201);
    live.alert().await;
    // A spare handshake of the same phone stops before `ready`.
    let mut spare = half_open_from(port, &phone, "127.0.0.1").await.unwrap();
    let (st, v) = admin(
        &s,
        "POST",
        &format!("/auth/inbox/api/devices/{}/revoke", phone.device_id),
        "{}",
    );
    assert_eq!((st, &v["revoked"]), (200, &json!(true)), "{v}");
    assert_eq!(
        live.closed_code().await,
        Some(inbox_core::proto::close::REVOKED)
    );
    assert_eq!(
        ready(&mut spare).await.err(),
        Some(Some(inbox_core::proto::close::REVOKED)),
        "the spare handshake must not get a session after the revocation"
    );
    let (_, overview) = admin(&s, "GET", "/auth/inbox/api/overview", "");
    assert_eq!(overview["stats"]["online"], 0, "{overview}");
    s.stop();
}

/// Finding inbox/device-limit-global-not-per-account. One account has at most
/// `inbox.devices.max_per_account` phones that are not revoked. Its refused code stays unused, and
/// the other accounts still enroll their phones.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn one_account_cannot_take_all_the_places_of_the_phones() {
    let mut s = Server::setup();
    let text = std::fs::read_to_string(s.config()).unwrap();
    std::fs::write(
        s.config(),
        text.replacen(
            "    max_devices = 10\n",
            "    max_devices = 10\n    max_per_account = 3\n",
            1,
        ),
    )
    .unwrap();
    s.start();
    // mallory holds one grant only: inbox.receive on one group.
    let _ = s.account_with(
        "mallory",
        &["inbox.receive on service:portal/inbox_group:other"],
    );
    let mut phones = Vec::new();
    for n in 0..3 {
        phones.push(
            enroll_as(&s, &format!("mallory-{n}"), "mallory")
                .await
                .unwrap(),
        );
    }
    let spare = code_for(&s, "mallory");
    assert!(matches!(
        enroll_code(&spare, "mallory-3").await,
        Err(EnrollFail::Closed(Some(4410)))
    ));
    assert!(
        s.log().contains("\"reason\":\"too_many_account_devices\""),
        "{}",
        s.log()
    );
    // The other accounts still enroll: the server has free places.
    let victor = enroll_as(&s, "victor-phone", "victor").await.unwrap();
    assert_eq!(victor.user_name, "victor");
    enroll_as(&s, "root-phone", SUPERADMIN).await.unwrap();
    // A revoked phone gives its place back, and the refused code was not used.
    let (st, _) = admin(
        &s,
        "POST",
        &format!("/auth/inbox/api/devices/{}/revoke", phones[0].device_id),
        "{}",
    );
    assert_eq!(st, 200);
    assert_eq!(
        enroll_code(&spare, "mallory-3").await.unwrap().user_name,
        "mallory"
    );
    s.stop();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn integrations_and_reverse_proxy_prefixes() {
    let mut s = Server::setup();
    // The proxy that sets the address of the client is a trusted proxy of the portal. The phones use the same list.
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

    // Generic notify: plain text body, title in the query string, key in the header, no
    // Idempotency-Key.
    let auth = ("Authorization", format!("Bearer {}", s.token));
    let url = "/api/v1/notify/infra?title=Sauvegarde";
    let (st, body) = http(
        s.port,
        "POST",
        url,
        &[auth.clone(), ("Content-Type", "text/plain".into())],
        "backup KO\n",
    );
    assert_eq!(st, 201, "{body}");
    let got = c.alert().await;
    assert_eq!(got.title, "Sauvegarde");
    c.ack(&got.alert_id, AckStage::Received).await;
    // The same request retried within the minute is deduplicated.
    let (st, body) = http(
        s.port,
        "POST",
        url,
        &[auth.clone(), ("Content-Type", "text/plain".into())],
        "backup KO\n",
    );
    assert_eq!(st, 200, "{body}");
    assert!(body.contains("\"duplicate\":true"));

    // Without a title anywhere, the plain text body is the title, on one line.
    let (st, _) = http(
        s.port,
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
        s.port,
        "POST",
        "/api/v1/notify/infra",
        std::slice::from_ref(&auth),
        "",
    );
    assert_eq!(st, 422);

    // JSON with a title, bearer header.
    let (st, _) = http(
        s.port,
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
        s.port,
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
        s.port,
        "POST",
        "/api/v1/notify/infra",
        std::slice::from_ref(&auth),
        r#"{ "monitor": {"name": "web"}, "status": 0 }"#,
    );
    assert_eq!(st, 201);
    let got = c.alert().await;
    assert_eq!(got.title, r#"{"monitor":{"name":"web"},"status":0}"#);
    c.ack(&got.alert_id, AckStage::Received).await;
    // A wrong key is refused; forbidden group too.
    let (st, _) = http(
        s.port,
        "POST",
        "/api/v1/notify/infra",
        &[("Authorization", "Bearer nope".into())],
        "x",
    );
    assert_eq!(st, 401);
    let (st, _) = http(
        s.port,
        "POST",
        "/api/v1/notify/other",
        std::slice::from_ref(&auth),
        "x",
    );
    assert_eq!(st, 403);

    // Titles are at most 128 characters: refused by the JSON API,
    // truncated by the generic webhook.
    let long = "x".repeat(129);
    let body = json!({"group": "infra", "title": long}).to_string();
    let (st, _) = http(
        s.port,
        "POST",
        "/api/v1/alerts",
        std::slice::from_ref(&auth),
        &body,
    );
    assert_eq!(st, 413);
    let (st, _) = http(
        s.port,
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
        s.port,
        "POST",
        "/api/v1/alerts",
        std::slice::from_ref(&auth),
        r#"{"group":"infra","description":"old name"}"#,
    );
    assert_eq!(st, 400);

    // JSON API without Idempotency-Key: created, then deduplicated.
    let (st, _) = http(
        s.port,
        "POST",
        "/api/v1/alerts",
        std::slice::from_ref(&auth),
        r#"{"group":"infra","title":"nokey"}"#,
    );
    assert_eq!(st, 201);
    let (st, _) = http(
        s.port,
        "POST",
        "/api/v1/alerts",
        std::slice::from_ref(&auth),
        r#"{"group":"infra","title":"nokey"}"#,
    );
    assert_eq!(st, 200);

    // The portal needs a session for the pages of the inbox.
    let (st, head, _) = http_full(
        s.port,
        &format!("127.0.0.1:{}", s.port),
        "GET",
        "/auth/settings/inbox",
        &[],
        "",
    );
    assert_eq!(st, 303);
    assert!(location(&head).starts_with("/auth/login"), "{head}");
    // The producer token does not open the pages.
    let (st, _, _) = http_full(
        s.port,
        &format!("127.0.0.1:{}", s.port),
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
    // The start itself takes seconds on a loaded machine, as long as the lease: the time does not
    // tell. The log tells: an instance that finds a released lease never waits for it.
    s.stop();
    let began = Instant::now();
    s.start();
    assert!(
        !s.log().contains("replica_waiting"),
        "a released lease was not taken at once ({:?}): {}",
        began.elapsed(),
        s.log()
    );
    assert!(s.log().contains("replica_ready"), "{}", s.log());
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
    assert!(shown.contains("inbox.devices.public_address"), "{shown}");
    assert!(!shown.contains("inbox.devices.listen"), "{shown}");
    assert!(
        shown.contains("inbox.api_key.ci.secret = <hidden>  # file:"),
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
    let database = unique_name("inbox_e2e");
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
    // The database appears only after the server has said that it cannot reach it. A fixed
    // delay is not enough: on a loaded machine the server needs more than that to try.
    s.spawn();
    let deadline = Instant::now() + Duration::from_secs(60);
    while !s.log().contains("postgres_unreachable") {
        assert!(
            Instant::now() < deadline,
            "the server never said that PostgreSQL is away: {}",
            s.log()
        );
        std::thread::sleep(Duration::from_millis(50));
    }
    admin_pg.create_database(&database);
    s.wait_ready();
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

/// The audit case: a webhook alert whose fingerprint holds U+0000 is spooled between two alerts while PostgreSQL is
/// away. Its key is a digest, so PostgreSQL takes it, and the alert after it reaches the database too.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_webhook_alert_with_a_nul_byte_does_not_block_the_replay_of_the_spool() {
    let Some(admin_pg) = pg_admin() else {
        eprintln!("INBOX_TEST_PG is not set: skipped");
        return;
    };
    let proxy = PgProxy::start(&admin_pg.host, admin_pg.port);
    let mut a = Server::setup_with(Some(admin_pg.clone()));
    through(&a, &admin_pg, proxy.port);
    quiet_phones(&a);
    a.start();

    proxy.blackhole();
    let (st, before) = post_alert(
        &a,
        "ci-before",
        json!({"group": "infra", "title": "before the poison"}),
    );
    assert_eq!(st, 201, "{before}");
    let payload = json!({
        "alerts": [
            {"status": "firing", "labels": {"alertname": "Poison"}, "annotations": {},
             "startsAt": "t\u{0}", "fingerprint": "fp\u{0}nul"}
        ]
    })
    .to_string();
    let (st, body) = http(
        a.port,
        "POST",
        "/api/v1/webhooks/alertmanager/infra",
        &[("Authorization", format!("Bearer {}", a.token))],
        &payload,
    );
    assert_eq!(st, 200, "{body}");
    let (st, after) = post_alert(
        &a,
        "ci-after",
        json!({"group": "infra", "title": "after the poison"}),
    );
    assert_eq!(st, 201, "{after}");

    proxy.pass();
    let after_id = after["alert_id"].as_str().unwrap().to_string();
    wait_for(
        "the three spooled alerts to reach the database",
        Duration::from_secs(70),
        || stored_alerts(&a).len() == 3,
    );
    assert!(stored_alerts(&a)
        .iter()
        .any(|x| x["id"] == after_id.as_str()));
    let log = a.log();
    assert!(!log.contains("spool_alert_refused"), "{log}");
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
    h.update(b"portal-inbox-peer-v1\0");
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
    // The port of the portal does not serve the peer API: the portal sends a visitor without a session to the login.
    assert_eq!(http(a.port, "GET", hello, &[], "").0, 302);
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
    let (port, host) = (s.port, format!("127.0.0.1:{}", s.port));

    // The rules of the form: a session, a POST with the origin, the form type and the token, no other field.
    let session = s.session();
    let origin = ("Origin", format!("http://{host}"));
    let form_type = (
        "Content-Type",
        "application/x-www-form-urlencoded".to_string(),
    );
    let csrf = inbox_csrf(port, &session.cookie);
    let call = |method: &str, headers: &[(&str, String)], body: &str| {
        http_host(
            port,
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
    // 80 bits of the digest: 20 hexadecimal characters in five groups of 4.
    assert_eq!(v["fingerprint"].as_str().unwrap().len(), 24);
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
    let (st, page) = inbox_page_as(&s, &bob_session.cookie, "/auth/settings/inbox");
    assert_eq!(st, 200);
    assert!(page.contains(r#"id="phones-title">My phones<"#), "{page}");
    for absent in [
        r#"id="sent-title""#,
        r#"id="connections-title""#,
        "New alert",
    ] {
        assert!(!page.contains(absent), "{absent}");
    }
    for path in [
        "/auth/settings/inbox/devices",
        "/auth/settings/inbox/connections",
        "/auth/settings/inbox/phones",
    ] {
        let (st, _) = inbox_page_as(&s, &bob_session.cookie, path);
        assert_eq!(st, 404, "{path}: the page is gone");
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
    // The block `inbox` goes, and the blocks that need it: the kind api, the account of the file
    // and its key. The block `postgres`, if any, stays.
    let start = text.find("# access-begin").unwrap();
    let access_end = text.find("# access-end\n").unwrap() + "# access-end\n".len();
    let text = format!("{}{}", &text[..start], &text[access_end..]);
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
    // The routes of the inbox are not found without the block.
    for path in ["/v1/session", "/v1/enroll", "/api/version"] {
        let (st, _) = inbox_page_as(&s, &session.cookie, path);
        assert_eq!(st, 404, "{path}");
    }
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

/// The block `watch` that the tests use. The values are the smallest that the ranges allow.
fn watch_block(extra: &str) -> String {
    format!(
        "  watch {{\n    group = \"infra\"\n    unhealthy_after = \"10s\"\n    sign_in_failures = 6\n    sign_in_window = \"1m\"\n    repeat_after = \"7d\"\n{extra}\n  }}"
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

fn watch_list(s: &Server) -> Vec<Value> {
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
        s.port,
        &format!("127.0.0.1:{}", s.port),
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
    s.add_to_inbox(&watch_block("    service_unhealthy = false"));
    s.start();
    // The sessions come before the burst: the limiter locks the address out afterwards.
    let phone = enroll_as(&s, "phone", SUPERADMIN).await.unwrap();
    let _ = s.session();
    let (mut conn, _) = Conn::open(s.port, &phone).await.unwrap();
    conn.subscribe("infra").await;

    // Five refused sign-ins are fewer than the threshold: nothing.
    refuse_sign_ins(s.port, "mallory", 5);
    conn.no_alert(Duration::from_secs(3)).await;
    // Four more reach the threshold of 6.
    refuse_sign_ins(s.port, "mallory", 4);
    let alert = conn.alert_within(Duration::from_secs(10)).await;
    assert_eq!(alert.title, "6 refused sign-ins in 1 minute");
    assert!(!alert.title.contains("mallory") && !alert.title.contains("127.0.0.1"));
    assert_eq!(alert.group, "infra");
    conn.ack(&alert.alert_id, AckStage::Received).await;

    // A second burst inside `repeat_after` makes no second alert, and the burst goes on.
    refuse_sign_ins(s.port, "mallory", 8);
    conn.no_alert(Duration::from_secs(4)).await;
    let alerts = watch_list(&s);
    assert_eq!(alerts.len(), 1, "{alerts:?}");
    assert_eq!(alerts[0]["title"], alert.title);
    let metrics = metrics_of(&s);
    for expected in [
        "portal_watch_sent_total{kind=\"sign_in_burst\"} 1",
        "portal_watch_sent_total{kind=\"service_unhealthy\"} 0",
        "portal_watch_failed_total{kind=\"sign_in_burst\"} 0",
    ] {
        assert!(metrics.contains(expected), "{expected}\n{metrics}");
    }
    s.stop();
}

// ---------------------------------------------------------------------------------------------
// Rights: keys of the portal as producers, subscriptions that follow the permissions.

/// Turns on the kind `mcp`: it needs the blocks `identity` and `pdp`. The base configuration has
/// the kind `api` only.
fn enable_mcp(s: &Server) {
    std::fs::write(
        s.dir.join("identity.pem"),
        "-----BEGIN PRIVATE KEY-----\nMC4CAQAwBQYDK2VwBCIEIAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8g\n-----END PRIVATE KEY-----\n",
    )
    .unwrap();
    let text = std::fs::read_to_string(s.config()).unwrap();
    let from = "keys {\n  mcp = false\n  api = true\n}";
    assert!(text.contains(from));
    let mut text = text.replace(from, "keys {\n  api = true\n}");
    let pdp = free_port();
    text.push_str(&format!(
        "\nidentity {{\n  issuer = \"https://portal.e2e\"\n  signing_key_file = \"{}\"\n}}\npdp {{\n  listen_address = \"127.0.0.1:{pdp}\"\n  url = \"http://127.0.0.1:{pdp}\"\n  plain_http = true\n}}\n",
        s.dir.join("identity.pem").display()
    ));
    std::fs::write(s.config(), text).unwrap();
}

/// A key that an account makes on the page Keys, with the form of its kind. `grants` are the fields
/// `grant<n>` of the form. The answer is the text of the key, shown one time.
fn make_key(s: &Server, cookie: &str, kind: &str, name: &str, grants: &[&str]) -> String {
    let host = format!("127.0.0.1:{}", s.port);
    let (st, _, page) = http_full(
        s.port,
        &host,
        "GET",
        &format!("/auth/keys/new?kind={kind}"),
        &[("Cookie", cookie.to_string())],
        "",
    );
    assert_eq!(st, 200, "{page}");
    let form = page.find("id=\"key-form\"").expect("the form of a new key");
    let marker = "name=\"csrf\" value=\"";
    let start = form + page[form..].find(marker).unwrap() + marker.len();
    let csrf = page[start..].split('"').next().unwrap().to_string();
    let mut body = format!(
        "csrf={csrf}&kind={kind}&name={}&expires_at=",
        form_value(name)
    );
    for (n, grant) in grants.iter().enumerate() {
        body.push_str(&format!("&grant{n}={}", form_value(grant)));
    }
    let (st, _, page) = http_full(
        s.port,
        &host,
        "POST",
        "/auth/keys/create",
        &[
            ("Content-Type", "application/x-www-form-urlencoded".into()),
            ("Origin", format!("http://{host}")),
            ("Cookie", cookie.to_string()),
        ],
        &body,
    );
    assert_eq!(st, 200, "the key is not made: {page}");
    let marker = "id=\"key-text\" type=\"text\" readonly value=\"";
    let start = page.find(marker).expect("the text of the key") + marker.len();
    page[start..].split('"').next().unwrap().to_string()
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
    let host = format!("localhost:{}", s.port);
    let (status, head, text) = http_full(
        s.port,
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
async fn an_api_key_sends_alerts_with_the_permissions_and_the_limit_of_its_owner() {
    let mut s = Server::setup();
    s.add_to_inbox("  group \"crit\" {\n    name = \"Critical\"\n    critical = true\n  }");
    enable_mcp(&s);
    s.start();

    // The windows of a budget are fixed windows of the clock. Start the count early in a minute, so
    // that the window does not end before the test has used it. The test waits first, with an async
    // sleep, and opens the phone session after the wait: a session that nobody reads for more than
    // `session_timeout` closes with the code 1001.
    while std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs()
        % 60
        > 15
    {
        tokio::time::sleep(Duration::from_millis(250)).await;
    }

    // The owner of the first key: it may make keys of both kinds, send to two groups, read one, and
    // its budget is 3 alerts each minute.
    let rights = |kind_rights: &[&str]| -> Vec<String> {
        let mut grants = vec!["keys.own.read on service:portal".to_string()];
        grants.extend(kind_rights.iter().map(|g| g.to_string()));
        grants
    };
    // The budget is a limit value of the grant `access` on the service portal: it is the first grant.
    let svc_grants = rights(&[
        "keys.own.create on service:portal/key_kind:api",
        "keys.own.use on service:portal/key_kind:api",
        "keys.own.create on service:portal/key_kind:mcp",
        "keys.own.use on service:portal/key_kind:mcp",
        "alert.send on service:portal/inbox_group:infra",
        "alert.send on service:portal/inbox_group:crit",
        "alert.read on service:portal/inbox_group:infra",
    ]);
    add_account_with(&s, "svc", &[]);
    let svc_refs: Vec<&str> = std::iter::once("access on service:portal")
        .chain(svc_grants.iter().map(String::as_str))
        .collect();
    set_grants_with(&s, "svc", &svc_refs, "limit0.alerts.minute=3");
    let partial_grants = rights(&[
        "keys.own.create on service:portal/key_kind:api",
        "keys.own.use on service:portal/key_kind:api",
        "alert.send on service:portal/inbox_group:infra",
        "alert.send on service:portal/inbox_group:other",
    ]);
    let partial_refs: Vec<&str> = partial_grants.iter().map(String::as_str).collect();
    add_account_with(&s, "partial", &partial_refs);
    let svc = sign_in_as(s.port, "svc", ACCOUNT_PASSWORD).cookie;
    let partial = sign_in_as(s.port, "partial", ACCOUNT_PASSWORD).cookie;
    let ci = make_key(
        &s,
        &svc,
        "api",
        "ci",
        &[
            "alert.send on service:portal/inbox_group:infra",
            "alert.send on service:portal/inbox_group:crit",
            "alert.read on service:portal/inbox_group:infra",
        ],
    );
    assert!(ci.starts_with("api_") && ci.len() == 53, "{ci}");
    let narrow = make_key(
        &s,
        &partial,
        "api",
        "narrow",
        &[
            "alert.send on service:portal/inbox_group:infra",
            "alert.send on service:portal/inbox_group:other",
        ],
    );
    let mcp = make_key(&s, &svc, "mcp", "agent", &[]);
    assert!(mcp.starts_with("mcp_"));

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
    // The key has the permission, its owner has it too: the alert is made.
    let (st, _, _) = send_with(&s, &narrow, "n1", "other", "to other");
    assert_eq!(st, 201);
    // The owner loses the permission: the key loses it at once. The permission on the other group
    // stays.
    set_grants(
        &s,
        "partial",
        &[
            "keys.own.read on service:portal",
            "keys.own.create on service:portal/key_kind:api",
            "keys.own.use on service:portal/key_kind:api",
            "alert.send on service:portal/inbox_group:infra",
        ],
    );
    let (st, _, v) = send_with(&s, &narrow, "n2", "other", "no");
    assert_eq!(st, 403, "the owner has no permission now: {v}");
    let (st, _, _) = send_with(&s, &narrow, "n3", "infra", "ok");
    assert_eq!(st, 201);
    assert_eq!(take(&mut conn).await.title, "ok");

    // The limit of the owner: 3 each minute. Two units are used (the first send and its repeat),
    // and the send to the group other is a use of another owner. This send takes the last.
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

    // The key of the file works as before. It is an API key too: its owner is an account of the
    // file, and the start decided its groups. A title in the query works with the key in the header.
    let (st, v) = post_alert(
        &s,
        "file1",
        json!({"group": "infra", "title": "from the file"}),
    );
    assert_eq!(st, 201, "{v}");
    assert_eq!(take(&mut conn).await.title, "from the file");
    let (st, v) = post_alert(&s, "file2", json!({"group": "other", "title": "no"}));
    assert_eq!(st, 403, "the key of the file has the group infra only: {v}");
    let (st, _) = http(
        s.port,
        "POST",
        "/api/v1/notify/crit?title=query",
        &[("Authorization", format!("Bearer {ci}"))],
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

    // A text that is not a key, a key that nobody has, and an MCP key: 401 with no detail. The
    // form is checked before any lookup, and the kind next.
    let counted = |kind: &str| -> u64 {
        let line = format!("portal_inbox_auth_failures_total{{kind=\"{kind}\"}} ");
        metrics_of(&s)
            .lines()
            .find_map(|l| l.strip_prefix(&line).and_then(|n| n.trim().parse().ok()))
            .unwrap_or(0)
    };
    let token_before = counted("producer_token");
    let key_before = counted("producer_key");
    let kind_before = counted("producer_kind");
    let ghost = {
        // A good form that no key has: the lookup finds nothing.
        let mut text = ci.clone();
        text.replace_range(10..11, if &ci[10..11] == "A" { "B" } else { "A" });
        let (random, _) = text.split_at(47);
        let sum = crc_text(random);
        format!("{random}{sum}")
    };
    let mut bad_sum = ci.clone();
    let last = bad_sum.pop().unwrap();
    bad_sum.push(if last == 'a' { 'b' } else { 'a' });
    for token in [
        "not a key at all".to_string(),
        "api_short".to_string(),
        bad_sum,
        // A changed prefix fails the checksum, and `whk_` is reserved.
        format!("mcp_{}", &ci[4..]),
        format!("whk_{}", &ci[4..]),
    ] {
        let (st, _, v) = send_with(&s, &token, "g1", "infra", "no");
        assert_eq!(st, 401, "{token}");
        assert_eq!(v["error"]["code"], "unauthorized", "no detail: {v}");
    }
    assert_eq!(
        counted("producer_token") - token_before,
        5,
        "five bad forms"
    );
    assert_eq!(
        counted("producer_key"),
        key_before,
        "no lookup for a bad form"
    );
    let (st, _, _) = send_with(&s, &ghost, "g2", "infra", "no");
    assert_eq!(st, 401);
    assert_eq!(
        counted("producer_key") - key_before,
        1,
        "a good form is a lookup"
    );
    let (st, _, v) = send_with(&s, &mcp, "g3", "infra", "no");
    assert_eq!(st, 401, "{v}");
    assert_eq!(
        counted("producer_kind") - kind_before,
        1,
        "the kind is not served here"
    );
    for path in [
        "/api/v1/notify/infra",
        "/api/v1/webhooks/alertmanager/infra",
    ] {
        let (st, _, _) = producer_call(&s, "POST", path, &mcp, "g4", "x");
        assert_eq!(st, 401, "{path}");
    }

    // The other verifier: the key edge serves the kind mcp. An API key gets 401 there.
    let edge = |token: &str| {
        http(
            s.port,
            "GET",
            "/auth/verify-key",
            &[
                ("Authorization", format!("Bearer {token}")),
                ("X-Forwarded-Uri", "/anything/".into()),
            ],
            "",
        )
        .0
    };
    assert_eq!(edge(&ci), 401, "an API key at the key edge");
    assert_eq!(edge(&narrow), 401);
    assert_ne!(edge(&mcp), 204, "the agent has no service to open here");
    s.stop();
}

/// The checksum characters of a key text: the CRC-32 of zlib in base62, as the key format says.
fn crc_text(covered: &str) -> String {
    const ALPHABET: &[u8; 62] = b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz";
    let mut crc = 0xFFFF_FFFFu32;
    for byte in covered.bytes() {
        crc ^= u32::from(byte);
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
    String::from_utf8(check.to_vec()).unwrap()
}

/// A service that goes unhealthy makes one alert after `unhealthy_after`. When it is healthy again, the
/// phone gets one short alert. A service that stays healthy makes no alert.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_unhealthy_service_makes_one_alert_and_its_recovery_one_more() {
    let mut s = Server::setup();
    quiet_phones(&s);
    s.add_to_inbox(&watch_block("    sign_in_burst = false"));
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
    let titles: Vec<_> = watch_list(&s)
        .iter()
        .map(|a| a["title"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(titles.len(), 2, "{titles:?}");
    let metrics = metrics_of(&s);
    for expected in [
        "portal_watch_sent_total{kind=\"service_unhealthy\"} 1",
        "portal_watch_sent_total{kind=\"service_recovered\"} 1",
        "portal_watch_sent_total{kind=\"sign_in_burst\"} 0",
    ] {
        assert!(metrics.contains(expected), "{expected}\n{metrics}");
    }
    s.stop();
}

/// The audit finding portal/watch/group-alert-names-services-hidden-from-receivers (the test of the
/// audit, inverted). The members carol, dave and erin hold `inbox.receive` and `alert.read` on the
/// group of the watch. carol and erin may open the service `notes`, dave may not. The alert about
/// `notes` and its recovery reach the superadmin and the members that may open the service.
/// Nothing reaches dave: no phone alert, no notification, no unread count, no row of the page, no
/// answer to his key. When carol loses the access, the recovery that waits for her phone never
/// reaches it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_watch_alert_reaches_only_the_members_that_may_open_the_service() {
    let mut s = Server::setup();
    quiet_phones(&s);
    s.add_to_inbox(&watch_block("    sign_in_burst = false"));
    let docker = with_notes(&s, "healthy");
    s.start();
    let member = [
        "inbox.receive on service:portal/inbox_group:infra",
        "alert.read on service:portal/inbox_group:infra",
        "keys.own.read on service:portal",
        "keys.own.create on service:portal/key_kind:api",
        "keys.own.use on service:portal/key_kind:api",
    ];
    let opener: Vec<&str> = member
        .iter()
        .copied()
        .chain(["access on service:notes"])
        .collect();
    let carol = s.account_with("carol", &opener);
    let dave = s.account_with("dave", &member);
    let _ = s.account_with("erin", &opener);
    let (_, home) = inbox_page_as(&s, &dave.cookie, "/");
    assert!(
        !home.contains("Notes"),
        "the launcher of dave hides the service"
    );
    let (_, home) = inbox_page_as(&s, &carol.cookie, "/");
    assert!(
        home.contains("Notes"),
        "the launcher of carol shows the service"
    );
    let now_of = |cookie: &str| notifications(&s, cookie, "").1["now"].as_i64().unwrap();
    let (since_c, since_d) = (now_of(&carol.cookie), now_of(&dave.cookie));
    let key_c = make_key(&s, &carol.cookie, "api", "carol-key", &[member[1]]);
    let key_d = make_key(&s, &dave.cookie, "api", "dave-key", &[member[1]]);

    // The phones of carol, dave, the superadmin and erin, in this order.
    let mut conns = Vec::new();
    for (name, login) in [
        ("c", "carol"),
        ("d", "dave"),
        ("r", SUPERADMIN),
        ("e", "erin"),
    ] {
        let phone = enroll_as(&s, &format!("phone-{name}"), login)
            .await
            .unwrap();
        let (mut conn, _) = Conn::open(s.port, &phone).await.unwrap();
        conn.subscribe("infra").await;
        conns.push((phone, conn));
    }
    docker.set("unhealthy");
    let title = "Service Notes is unhealthy for 10 seconds";
    let alert = conns[0].1.alert_within(Duration::from_secs(30)).await;
    assert_eq!(alert.title, title);
    conns[0].1.ack(&alert.alert_id, AckStage::Received).await;
    for at in [2, 3] {
        let got = conns[at].1.alert_within(Duration::from_secs(10)).await;
        assert_eq!(got.alert_id, alert.alert_id);
        conns[at].1.ack(&got.alert_id, AckStage::Received).await;
    }
    conns[1].1.no_alert(Duration::from_secs(3)).await;
    // The fan-out made no delivery for the phone of dave.
    let stored = watch_list(&s);
    assert_eq!(stored[0]["targets"], 3, "{stored:?}");

    // The notifications and their count.
    let (st, seen) = notifications(&s, &dave.cookie, &format!("?after={since_d}"));
    assert_eq!(st, 200);
    assert!(titles(&seen).is_empty(), "{seen}");
    assert_eq!(seen["unread"], 0, "{seen}");
    let (_, seen) = notifications(&s, &carol.cookie, &format!("?after={since_c}"));
    assert_eq!(titles(&seen), [title]);
    assert_eq!(seen["unread"], 1);
    let (_, home) = inbox_page_as(&s, &dave.cookie, "/");
    assert!(!home.contains("Service Notes"), "the bell of the launcher");

    // The page of the Inbox: the rows, the opened alert and the graph.
    let open = format!("/auth/settings/inbox?open={}", alert.alert_id);
    for path in ["/auth/settings/inbox", open.as_str()] {
        let (st, page) = inbox_page_as(&s, &dave.cookie, path);
        assert_eq!(st, 200);
        assert!(!page.contains("Service Notes"), "{path}");
        let (_, page) = inbox_page_as(&s, &carol.cookie, path);
        assert!(page.contains(title), "{path}");
        let (_, page) = inbox_page(&s, path);
        assert!(page.contains(title), "{path}");
    }

    // The status of the alert for a key: the owner of the key must see the service. The key of the
    // file belongs to the account `ci`, which may not open the service.
    let status = format!("/api/v1/alerts/{}", alert.alert_id);
    let (st, _, v) = producer_call(&s, "GET", &status, &key_c, "", "");
    assert_eq!(st, 200, "{v}");
    assert_eq!(v["title"], title);
    for token in [&key_d, &s.token] {
        let (st, _, v) = producer_call(&s, "GET", &status, token, "", "");
        assert_eq!(st, 404, "{v}");
    }

    // The phone of carol is away when the service is healthy again. The recovery follows the same
    // rule: erin and the superadmin get it, dave does not, and one delivery waits for carol.
    let (phone_c, conn_c) = conns.remove(0);
    drop(conn_c);
    docker.set("healthy");
    let back = conns[2].1.alert_within(Duration::from_secs(20)).await;
    assert_eq!(back.title, "Service Notes is healthy again");
    conns[2].1.ack(&back.alert_id, AckStage::Received).await;
    let got = conns[1].1.alert_within(Duration::from_secs(10)).await;
    assert_eq!(got.alert_id, back.alert_id);
    conns[1].1.ack(&got.alert_id, AckStage::Received).await;
    conns[0].1.no_alert(Duration::from_secs(3)).await;
    let (_, seen) = notifications(&s, &dave.cookie, &format!("?after={since_d}"));
    assert!(titles(&seen).is_empty(), "{seen}");
    let stored = watch_list(&s);
    assert_eq!(stored[0]["title"], back.title);
    assert_eq!(
        stored[0]["targets"], 3,
        "carol could open the service: {stored:?}"
    );

    // carol loses the access: the recovery that waits for her phone never reaches it, and her
    // notifications no longer show the alerts about the service.
    set_grants(&s, "carol", &member);
    let (mut conn_c, _) = Conn::open(s.port, &phone_c).await.unwrap();
    conn_c.no_alert(Duration::from_secs(4)).await;
    wait_for("the withdrawn delivery", Duration::from_secs(10), || {
        watch_list(&s)[0]["targets"] == 2
    });
    let (st, seen) = notifications(&s, &carol.cookie, &format!("?after={since_c}"));
    assert_eq!(st, 200, "{seen}");
    assert!(titles(&seen).is_empty(), "{seen}");
    assert_eq!(seen["unread"], 0, "{seen}");
    s.stop();
}

/// Two active instances detect the same events. The shared key makes the inbox accept one alert for each event.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn two_instances_send_one_alert_for_one_event() {
    let Some(admin_pg) = pg_admin() else {
        eprintln!("INBOX_TEST_PG is not set: skipped");
        return;
    };
    stay_in_an_hour(90);
    let mut a = Server::setup_with(Some(admin_pg));
    quiet_phones(&a);
    a.add_to_inbox(&watch_block(""));
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
    refuse_sign_ins(a.port, "mallory", 7);
    refuse_sign_ins(b.port, "mallory", 7);
    let burst = conn.alert_within(Duration::from_secs(10)).await;
    assert_eq!(burst.title, "6 refused sign-ins in 1 minute");
    conn.ack(&burst.alert_id, AckStage::Received).await;
    conn.no_alert(Duration::from_secs(5)).await;

    // The database holds two alerts, and both come from the portal.
    let stored = stored_alerts(&a);
    assert_eq!(stored.len(), 2, "one alert for each event: {stored:?}");
    assert_eq!(watch_list(&a).len(), 2);
    assert_eq!(watch_list(&b).len(), 2);
    // The two instances both tried: one of them found the key already in use.
    wait_for(
        "an instance that finds the key in use",
        Duration::from_secs(10),
        || format!("{}{}", a.log(), b.log()).contains("\"duplicate\":\"true\""),
    );
    let logs = format!("{}{}", a.log(), b.log());
    assert!(!logs.contains("watch_failed"), "{logs}");
    b.stop();
    a.stop();
}

#[test]
fn check_config_reports_the_block_watch() {
    let s = Server::setup();
    let out = portal(&["check-config", "--config", &s.config()], None);
    let shown = String::from_utf8_lossy(&out.stdout).to_string();
    assert!(out.status.success());
    assert!(
        !shown.contains("inbox.watch."),
        "nothing without the block: {shown}"
    );
    let text = std::fs::read_to_string(s.config()).unwrap();
    let good = s.dir.join("alerts-ok.hcl");
    std::fs::write(
        &good,
        text.replace(
            "  # inbox-extra\n",
            "  watch {\n    group = \"other\"\n    sign_in_failures = 3\n  }\n  # inbox-extra\n",
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
        "inbox.watch.group = \"other\"  # file",
        "inbox.watch.sign_in_failures = 3  # file",
        "inbox.watch.unhealthy_after = \"2m\"  # default",
        "inbox.watch.repeat_after = \"1h\"  # default",
        "inbox.watch.recovered = true  # default",
    ] {
        assert!(shown.contains(expected), "{expected}\n{shown}");
    }
    // A group that does not exist stops the start, and the message names the key and the value.
    let bad = s.dir.join("alerts-bad.hcl");
    std::fs::write(
        &bad,
        text.replace(
            "  # inbox-extra\n",
            "  watch {\n    group = \"nowhere\"\n  }\n  # inbox-extra\n",
        ),
    )
    .unwrap();
    let out = portal(&["check-config", "--config", bad.to_str().unwrap()], None);
    assert_eq!(out.status.code(), Some(1));
    let error = String::from_utf8_lossy(&out.stderr);
    assert!(
        error.contains("inbox.watch.group") && error.contains("\"nowhere\""),
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
        &["inbox.receive on service:portal/inbox_group:other"],
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
        &["inbox.receive on service:portal/inbox_group:other"],
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
            "inbox.receive on service:portal/inbox_group:other",
            "inbox.receive on service:portal/inbox_group:infra",
        ],
    );
    c.subscribe("infra").await;
    set_grants(
        &s,
        "omar",
        &["inbox.receive on service:portal/inbox_group:infra"],
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
  grants = [ "inbox.receive on service:portal/inbox_group:infra" ]
}}
"#,
        s.dir.join("fred.hash").display()
    );
    s.add_block(&users);
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
    let fred = sign_in_as(s.port, "fred", ACCOUNT_PASSWORD);
    let (st, page) = inbox_page_as(&s, &fred.cookie, "/auth/settings/inbox");
    assert_eq!(st, 200, "{page}");
    assert!(has(&page, r#"id="phones-title">My phones<"#), "{page}");
    assert!(!has(&page, r#"id="sent-title""#) && !has(&page, r#"id="connections-title""#));
    let (st, _, page) = inbox_form(&s, &fred.cookie, "/auth/settings/inbox/code", "");
    assert_eq!(st, 200, "{page}");
    let code = code_in(&page);
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
            "alert.send on service:portal/inbox_group:infra",
            "alert.read on service:portal/inbox_group:infra",
        ],
    );
    let (st, page) = inbox_page_as(&s, &pia.cookie, "/auth/settings/inbox");
    assert_eq!(st, 200, "{page}");
    assert!(has(&page, "New alert"));
    // One group is no choice: the form has no list.
    assert!(has(
        &page,
        r#"<input type="hidden" name="to" value="group:infra">"#
    ));
    assert!(
        !has(&page, r#"<option value="other">"#) && !has(&page, "value=\"other\""),
        "only the groups that the account may send to"
    );
    assert!(has(&page, r#"id="sent-title""#));
    assert!(
        !has(&page, r#"id="phones-title""#) && !has(&page, r#"id="connections-title""#),
        "no phone right: no phone zone"
    );
    assert!(
        !has(&page, "contentTabs"),
        "an account with one tab has no tab strip"
    );
    assert!(
        !has(&page, "/auth/settings/services"),
        "a plain account has not the tabs of the superadmin"
    );
    // The old pages are gone: they answer 404 for each account.
    for path in [
        "/auth/settings/inbox/devices",
        "/auth/settings/inbox/connections",
        "/auth/settings/inbox/phones",
        "/auth/settings/inbox/doc",
    ] {
        let (st, _) = inbox_page_as(&s, &pia.cookie, path);
        assert_eq!(st, 404, "{path}");
    }
    // The documentation is a popover of the page. It shows the address that the request shows.
    assert!(has(&page, "POST /api/v1/alerts"));
    assert!(
        has(&page, &format!("http://127.0.0.1:{}", s.port)),
        "{page}"
    );
    // The form refuses a group that the account may not send to, and sends to the others.
    let (st, _, text) = inbox_form(
        &s,
        &pia.cookie,
        "/auth/settings/inbox/send",
        "to=group:other&title=no&key=pia1",
    );
    assert_eq!(st, 422, "{text}");
    // The page comes back with the popup open and the title that was typed.
    assert!(
        has(&text, "<details class=\"pop\" open>") && has(&text, r#"value="no""#),
        "{text}"
    );
    assert!(has(&text, r#"data-send-error="bad_group""#), "{text}");
    let (st, _, text) = inbox_form(
        &s,
        &pia.cookie,
        "/auth/settings/inbox/send",
        "to=group:infra&title=+&key=pia2",
    );
    assert_eq!(st, 422, "{text}");
    assert!(has(&text, r#"data-send-error="no_title""#), "{text}");
    let long = "x".repeat(200);
    let (st, _, text) = inbox_form(
        &s,
        &pia.cookie,
        "/auth/settings/inbox/send",
        &format!("to=group:infra&title={long}&key=pia3"),
    );
    assert_eq!(st, 422, "{text}");
    assert!(has(&text, r#"data-send-error="bad_title""#), "{text}");
    let (st, head, _) = inbox_form(
        &s,
        &pia.cookie,
        "/auth/settings/inbox/send",
        "to=group:infra&title=Disk+full&key=pia4",
    );
    assert_eq!(st, 303);
    let sent = location(&head);
    assert!(
        sent.starts_with("/auth/settings/inbox?open="),
        "the page opens the new alert in place, with no banner: {sent}"
    );
    let (st, page) = inbox_page_as(&s, &pia.cookie, sent.split('#').next().unwrap());
    assert_eq!(st, 200, "{page}");
    assert!(has(&page, "Disk full") && !has(&page, "The alert is sent."));
    assert!(has(&page, "data-detail="), "the row is open: {page}");
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
    let (st, page) = inbox_page_as(
        &s,
        &pia.cookie,
        &format!("/auth/settings/inbox?open={other_id}"),
    );
    assert_eq!(st, 200);
    assert!(
        !has(&page, "data-detail=") && !has(&page, "secret"),
        "the detail of an alert of an unreadable group does not open"
    );
    let (_, page) = inbox_page_as(&s, &pia.cookie, "/auth/settings/inbox");
    assert!(
        !has(&page, "secret"),
        "the list shows the alerts of the readable groups only"
    );

    // An operator: it reads the phones and the connections and revokes a phone.
    let owen = s.account_with(
        "owen",
        &[
            "phone.read on service:portal",
            "phone.revoke on service:portal",
            "phone.log on service:portal",
        ],
    );
    let alice = enroll_as(&s, "phone-alice", "alice").await.unwrap();
    let (mut ca, _) = Conn::open(port, &alice).await.unwrap();
    ca.subscribe("infra").await;
    let (st, page) = inbox_page_as(&s, &owen.cookie, "/auth/settings/inbox");
    assert_eq!(st, 200, "{page}");
    assert!(!has(&page, "New alert"));
    assert!(
        !has(&page, r#"id="sent-title""#),
        "no alert.read: no zone of alerts"
    );
    assert!(has(&page, r#"id="phones-title">Phones<"#) && has(&page, r#"id="connections-title""#));
    assert!(
        has(&page, "phone-alice")
            && has(
                &page,
                &format!(r#"name="id.{}" value="1""#, alice.device_id)
            )
    );
    assert!(has(&page, "data-bulk=\"revoke-phones\""));
    assert!(has(&page, r#"data-state="online""#));
    let (st, page) = inbox_page_as(&s, &owen.cookie, "/auth/settings/inbox?login=alice");
    assert_eq!(st, 200);
    assert!(has(&page, r#"data-login="alice""#) && has(&page, r#"value="alice""#));
    let (st, page) = inbox_page_as(&s, &owen.cookie, "/auth/settings/inbox?login=nobody");
    assert_eq!(st, 200);
    assert!(has(&page, "No connection for these filters."));
    // The operator revokes the phone of another account.
    let (st, head, text) = inbox_form(
        &s,
        &owen.cookie,
        "/auth/settings/inbox/revoke",
        &format!("id.{}=1", alice.device_id),
    );
    assert_eq!(st, 303, "{text}");
    assert_eq!(location(&head), "/auth/settings/inbox", "{head}");
    assert_eq!(
        ca.closed_code().await,
        Some(inbox_core::proto::close::REVOKED)
    );
    // A reader without phone.revoke sees no button, and a forced request changes nothing.
    let rita = s.account_with("rita", &["phone.read on service:portal"]);
    let bob = enroll_as(&s, "phone-bob", "bob").await.unwrap();
    let (_, page) = inbox_page_as(&s, &rita.cookie, "/auth/settings/inbox");
    assert!(has(&page, "phone-bob") && !has(&page, "bulk__box") && !has(&page, "revoke-phones"));
    // The page has the form of the level of the notifications, so the account has a token. The
    // request passes the token, and the missing right refuses it.
    let (st, head, _) = inbox_form(
        &s,
        &rita.cookie,
        "/auth/settings/inbox/revoke",
        &format!("id.{}=1", bob.device_id),
    );
    assert_eq!(st, 303);
    assert!(location(&head).ends_with("s=unknown_device"), "{head}");
    assert!(
        Conn::open(port, &bob).await.is_ok(),
        "the phone is not revoked"
    );

    // My phones: an account revokes its own phone and no other.
    let bob_session = s.account("bob");
    let (st, page) = inbox_page_as(&s, &bob_session.cookie, "/auth/settings/inbox");
    assert_eq!(st, 200);
    assert!(has(&page, "phone-bob") && !has(&page, "phone-alice"));
    assert!(
        has(&page, &format!(r#"name="id.{}" value="1""#, bob.device_id))
            && has(&page, r#"aria-label="Add a phone""#)
    );
    let carol = enroll_as(&s, "phone-carol", "carol").await.unwrap();
    let (_, head, _) = inbox_form(
        &s,
        &bob_session.cookie,
        "/auth/settings/inbox/revoke",
        &format!("id.{}=1", carol.device_id),
    );
    assert!(location(&head).contains("s=unknown_device"), "{head}");
    assert!(Conn::open(port, &carol).await.is_ok());
    let (_, head, _) = inbox_form(
        &s,
        &bob_session.cookie,
        "/auth/settings/inbox/revoke",
        &format!("id.{}=1", bob.device_id),
    );
    assert_eq!(location(&head), "/auth/settings/inbox", "{head}");
    assert_eq!(
        Conn::open(port, &bob).await.err(),
        Some(inbox_core::proto::close::REVOKED)
    );
    // The code page is shown one time, and no store keeps it.
    let host = format!("127.0.0.1:{}", s.port);
    let csrf = inbox_csrf(s.port, &bob_session.cookie);
    let (st, head, page) = http_full(
        s.port,
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
    assert!(has(&page, "portal-inbox://enroll?s=") && has(&page, "data-copy=\"enroll-code\""));

    // The inbox is a feature of the portal: the launcher has no tile for it. An account with no permission of
    // the inbox has no gear and no tab. An account that has one gets the gear, which opens the tab.
    let none = s.account_with("nora", &[]);
    let (_, home) = inbox_page_as(&s, &none.cookie, "/");
    assert!(!has(&home, "/auth/settings/inbox") && !has(&home, "Settings"));
    let (st, _) = inbox_page_as(&s, &none.cookie, "/auth/settings/inbox");
    assert_eq!(st, 403);
    let (_, home) = inbox_page_as(&s, &pia.cookie, "/");
    // The launcher changes the order of the attributes at each render: read the tag of the gear.
    // The inbox icon has a link to the same page: the gear is the tag that is named "Settings".
    let gear_of = |home: &str, href: &str| {
        home.match_indices(&format!(r#"href="{href}""#))
            .map(|(at, _)| {
                home[home[..at].rfind('<').unwrap()..at + home[at..].find('>').unwrap()].to_string()
            })
            .find(|tag| tag.contains(r#"aria-label="Settings""#))
            .unwrap_or_else(|| panic!("no gear for {href}: {home}"))
    };
    let gear = gear_of(&home, "/auth/settings/inbox");
    assert!(gear.contains(r#"aria-label="Settings""#), "{gear}");
    assert!(
        !has(&home, "inbox-tile") && !has(&home, "<h2"),
        "no tile of the inbox: {home}"
    );
    let (_, home) = inbox_page_as(&s, &s.session().cookie, "/");
    assert!(gear_of(&home, "/auth/settings").contains(r#"aria-label="Settings""#));
    assert!(!has(&home, ">Inbox<"), "no tile of the inbox");
    // The superadmin sees four tabs at most and the permission editor shows the groups by name.
    // The tab Keys does not exist: the list of the keys is in the tab Access.
    let (_, page) = inbox_page(&s, "/auth/settings/inbox");
    for tab in ["Access", "Services", "Activity", "Inbox", "Links"] {
        // The tab Inbox has its badge after the name.
        let name = if tab == "Inbox" { ">Inbox<span " } else { tab };
        assert!(
            has(&page, &format!(">{tab}</a>")) || has(&page, name),
            "{tab}"
        );
    }
    assert!(!has(&page, ">Keys</a>"), "the tab Keys is gone");
    assert_eq!(
        page.matches(r#"class="contentTabs__tab"#).count(),
        5,
        "four tabs and the tab Links"
    );
    // The inbox is a feature of the portal, as the keys are: the Services page does not list it, and
    // it has no page of a service. The editor of permissions shows it in the group "Server permissions".
    let (st, services) = inbox_page(&s, "/auth/settings/services");
    assert_eq!(st, 200);
    assert!(
        !has(&services, "Inbox (portal)") && !has(&services, "alert.send"),
        "{services}"
    );
    let (st, _) = inbox_page(&s, "/auth/settings/services/inbox");
    assert_eq!(st, 404);
    let (st, editor) = inbox_page(&s, "/auth/settings/users/pia");
    assert_eq!(st, 200);
    assert!(
        has(&editor, "Server permissions")
            && has(&editor, "inbox_group:infra")
            && has(&editor, "Infra")
            && has(&editor, "phone.revoke"),
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
    // Two accounts of the file for the browser check of an alert to such an account (see
    // tools/dev/ui-check-notify.js, section fileaccount): fiona has a phone, fran has none.
    for login in ["fiona", "fran"] {
        add_file_account(
            &s,
            login,
            &["inbox.receive on service:portal/inbox_group:infra"],
        );
    }
    s.add_to_inbox(
        "  group \"crit\" {\n    name = \"Critical outages of the production cluster\"\n    description = \"Pages the on-call team\"\n    critical = true\n  }",
    );
    s.start();
    s.account_with(
        "pia",
        &[
            "alert.send on service:portal/inbox_group:infra",
            "alert.read on service:portal/inbox_group:infra",
        ],
    );
    s.account_with(
        "owen",
        &[
            "phone.read on service:portal",
            "phone.revoke on service:portal",
            "phone.log on service:portal",
            "alert.read on service:portal/inbox_group:infra",
            "alert.read on service:portal/inbox_group:other",
            "alert.read on service:portal/inbox_group:crit",
            "alert.send on service:portal/inbox_group:infra",
        ],
    );
    s.account_with(
        "alice",
        &[
            "inbox.receive on service:portal/inbox_group:infra",
            "inbox.receive on service:portal/inbox_group:other",
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
    s.file_account("fiona");
    enroll_as(&s, "Phone of Fiona", "fiona").await.unwrap();
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
        json!({"port": s.port, "password": ACCOUNT_PASSWORD, "root": SUPERADMIN, "root_password": SUPERADMIN_PASSWORD}).to_string(),
    )
    .unwrap();
    let deadline = Instant::now() + Duration::from_secs(1800);
    while !out.join("stop").exists() && Instant::now() < deadline {
        // The live phone answers the heartbeat of the server.
        let _ = live.recv(Duration::from_millis(500)).await;
    }
    s.stop();
}

/// The old names of the feature are unknown. `check-config` refuses a file that still has one.
/// Two parts build each old name, so that the repository holds no copy of it.
#[test]
fn check_config_refuses_the_old_names_of_the_feature() {
    let s = Server::setup();
    let text = std::fs::read_to_string(s.config()).unwrap();
    let old_block = ["pa", "ger"].concat();
    let old_watch = ["portal", "_alerts"].concat();
    let renamed = s.dir.join("old-block.hcl");
    std::fs::write(
        &renamed,
        text.replacen("inbox {", &format!("{old_block} {{"), 1),
    )
    .unwrap();
    let out = portal(
        &["check-config", "--config", renamed.to_str().unwrap()],
        None,
    );
    assert_eq!(out.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();
    assert!(
        stderr.contains(&format!("unknown configuration block: {old_block}")),
        "{stderr}"
    );
    let watch = s.dir.join("old-watch.hcl");
    std::fs::write(
        &watch,
        text.replace(
            "  # inbox-extra\n",
            &format!("  {old_watch} {{\n    group = \"other\"\n  }}\n  # inbox-extra\n"),
        ),
    )
    .unwrap();
    let out = portal(&["check-config", "--config", watch.to_str().unwrap()], None);
    assert_eq!(out.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();
    assert!(stderr.contains(&old_watch), "{stderr}");
}

// ---------------------------------------------------------------------------------------------
// An alert to one account: the right `alert.send_user`, the phones of the account, the codes of
// the refusals.

/// `POST /api/v1/alerts` with a body that the test builds.
fn send_to(s: &Server, token: &str, key: &str, body: Value) -> (u16, Value) {
    let (status, _, v) = producer_call(s, "POST", "/api/v1/alerts", token, key, &body.to_string());
    (status, v)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_api_key_with_the_right_sends_an_alert_to_each_active_phone_of_an_account() {
    let mut s = Server::setup();
    s.start();
    // The windows of a budget are fixed windows of the clock: start the count early in a minute.
    while std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs()
        % 60
        > 15
    {
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    // The owner of the key may alert any account and the group infra. Its budget is 3 alerts each
    // minute, for the groups and the accounts together.
    let owner_grants = [
        "access on service:portal",
        "keys.own.read on service:portal",
        "keys.own.create on service:portal/key_kind:api",
        "keys.own.use on service:portal/key_kind:api",
        "alert.send_user on service:portal",
        "alert.send on service:portal/inbox_group:infra",
        "alert.read on service:portal/inbox_group:infra",
    ];
    add_account_with(&s, "svc", &[]);
    set_grants_with(&s, "svc", &owner_grants, "limit0.alerts.minute=3");
    let svc = sign_in_as(s.port, "svc", ACCOUNT_PASSWORD).cookie;
    let direct = make_key(
        &s,
        &svc,
        "api",
        "direct",
        &["alert.send_user on service:portal"],
    );
    let other = make_key(
        &s,
        &svc,
        "api",
        "other",
        &[
            "alert.send_user on service:portal",
            "alert.send on service:portal/inbox_group:infra",
        ],
    );
    let group_only = make_key(
        &s,
        &svc,
        "api",
        "group-only",
        &["alert.send on service:portal/inbox_group:infra"],
    );
    // The owner of a second key lacks the right: the key cannot carry it.
    add_account_with(
        &s,
        "plain",
        &[
            "keys.own.read on service:portal",
            "keys.own.create on service:portal/key_kind:api",
            "keys.own.use on service:portal/key_kind:api",
            "alert.send on service:portal/inbox_group:infra",
        ],
    );
    let plain = sign_in_as(s.port, "plain", ACCOUNT_PASSWORD).cookie;
    // The key form refuses the right: the owner does not hold it, so the key cannot carry it.
    let host = format!("127.0.0.1:{}", s.port);
    let (st, _, page) = http_full(
        s.port,
        &host,
        "GET",
        "/auth/keys/new?kind=api",
        &[("Cookie", plain.clone())],
        "",
    );
    assert_eq!(st, 200, "{page}");
    let form = page.find("id=\"key-form\"").unwrap();
    let marker = "name=\"csrf\" value=\"";
    let start = form + page[form..].find(marker).unwrap() + marker.len();
    let csrf = page[start..].split('"').next().unwrap().to_string();
    let (_, _, refused) = http_full(
        s.port,
        &host,
        "POST",
        "/auth/keys/create",
        &[
            ("Content-Type", "application/x-www-form-urlencoded".into()),
            ("Origin", format!("http://{host}")),
            ("Cookie", plain.clone()),
        ],
        &format!(
            "csrf={csrf}&kind=api&name=too-much&expires_at=&grant0={}",
            form_value("alert.send_user on service:portal")
        ),
    );
    assert!(
        !refused.contains("id=\"key-text\""),
        "an owner without the right cannot give it to a key"
    );

    // alice has three phones. The third is revoked. bob has one phone. dora has none.
    add_account(&s, "alice");
    add_account(&s, "bob");
    add_account(&s, "dora");
    let a1 = enroll_as(&s, "alice-1", "alice").await.unwrap();
    let a2 = enroll_as(&s, "alice-2", "alice").await.unwrap();
    let a3 = enroll_as(&s, "alice-3", "alice").await.unwrap();
    let b1 = enroll_as(&s, "bob-1", "bob").await.unwrap();
    let (st, _) = admin(
        &s,
        "POST",
        &format!("/auth/inbox/api/devices/{}/revoke", a3.device_id),
        "{}",
    );
    assert_eq!(st, 200);
    let (mut c1, _) = Conn::open(s.port, &a1).await.unwrap();
    let (mut c2, _) = Conn::open(s.port, &a2).await.unwrap();
    let (mut cb, _) = Conn::open(s.port, &b1).await.unwrap();
    // Not one of them subscribes to any group: a direct alert needs no subscription.

    let (st, v) = send_to(
        &s,
        &direct,
        "d1",
        json!({"user": "alice", "title": "wake up"}),
    );
    assert_eq!(st, 201, "{v}");
    assert_eq!(v["targets"], 2, "the two active phones: {v}");
    let first = take(&mut c1).await;
    let second = take(&mut c2).await;
    assert_eq!(first.alert_id, second.alert_id);
    assert_eq!(first.title, "wake up");
    // The frame is the frame of any alert. It names no group of the file.
    assert_eq!(first.group, "@direct");
    assert_eq!(first.group_name, "Direct");
    cb.no_alert(Duration::from_millis(1500)).await;
    assert_eq!(
        Conn::open(s.port, &a3).await.err(),
        Some(inbox_core::proto::close::REVOKED),
        "the revoked phone gets nothing"
    );
    // The same Idempotency-Key makes no second alert.
    let (st, v) = send_to(
        &s,
        &direct,
        "d1",
        json!({"user": "alice", "title": "wake up"}),
    );
    assert_eq!(st, 200, "{v}");
    c1.no_alert(Duration::from_millis(1200)).await;

    // The key that sent the alert reads it. Its target is an account.
    let id = first.alert_id.clone();
    let (st, _, v) = producer_call(&s, "GET", &format!("/api/v1/alerts/{id}"), &direct, "", "");
    assert_eq!(st, 200, "{v}");
    assert_eq!(v["target"], json!({"kind": "user", "id": "alice"}), "{v}");
    assert_eq!(v["kind"], "alert");
    assert_eq!(v["targets"], 2);
    // Another key of the same owner is not the sender: it reads nothing. A key that has the right
    // on a group reads nothing here either: the alert has no group.
    for token in [&other, &group_only] {
        let (st, _, v) = producer_call(&s, "GET", &format!("/api/v1/alerts/{id}"), token, "", "");
        assert_eq!(st, 404, "{v}");
    }

    // The target: exactly one of group and user.
    let (st, v) = send_to(
        &s,
        &direct,
        "t1",
        json!({"user": "alice", "group": "infra", "title": "both"}),
    );
    assert_eq!(st, 400, "{v}");
    assert_eq!(v["error"]["code"], "invalid_target", "{v}");
    let (st, v) = send_to(&s, &direct, "t2", json!({"title": "none"}));
    assert_eq!(st, 400, "{v}");
    assert_eq!(v["error"]["code"], "invalid_target", "{v}");
    // An account that does not exist: 404. An account with no phone: 409. Neither stores an alert,
    // and neither uses the budget.
    let (st, v) = send_to(&s, &direct, "u1", json!({"user": "ghost", "title": "x"}));
    assert_eq!(st, 404, "{v}");
    assert_eq!(v["error"]["code"], "unknown_user", "{v}");
    let (st, v) = send_to(&s, &direct, "p1", json!({"user": "dora", "title": "x"}));
    assert_eq!(st, 409, "{v}");
    assert_eq!(v["error"]["code"], "no_phone", "{v}");
    // The account of a revoked phone only has no phone to use either.
    add_account(&s, "gina");
    let g1 = enroll_as(&s, "gina-1", "gina").await.unwrap();
    admin(
        &s,
        "POST",
        &format!("/auth/inbox/api/devices/{}/revoke", g1.device_id),
        "{}",
    );
    let (st, v) = send_to(&s, &direct, "p2", json!({"user": "gina", "title": "x"}));
    assert_eq!((st, v["error"]["code"].as_str()), (409, Some("no_phone")));
    // The key without the right: 403, and the code names the user target.
    let (st, v) = send_to(
        &s,
        &group_only,
        "g1",
        json!({"user": "alice", "title": "x"}),
    );
    assert_eq!(st, 403, "{v}");
    assert_eq!(v["error"]["code"], "forbidden_user", "{v}");
    // Its right for a group works as before, and a key with the user right needs the group right
    // to send to a group.
    let (st, _) = send_to(
        &s,
        &group_only,
        "g2",
        json!({"group": "infra", "title": "g"}),
    );
    assert_eq!(st, 201);
    let (st, v) = send_to(&s, &direct, "g3", json!({"group": "infra", "title": "g"}));
    assert_eq!(st, 403, "{v}");
    assert_eq!(v["error"]["code"], "forbidden_group", "{v}");

    // The budget `alerts` counts the alerts to an account and the alerts to a group together: the
    // first alert (d1) and the repeat of it used two units, the group alert used the third.
    let (st, v) = send_to(
        &s,
        &direct,
        "d2",
        json!({"user": "alice", "title": "limit"}),
    );
    assert_eq!(st, 429, "{v}");
    assert_eq!(v["error"]["code"], "rate_limited", "{v}");
    c1.no_alert(Duration::from_millis(500)).await;

    // The owner loses the right: the key loses it at once.
    set_grants(
        &s,
        "svc",
        &[
            "keys.own.read on service:portal",
            "keys.own.create on service:portal/key_kind:api",
            "keys.own.use on service:portal/key_kind:api",
        ],
    );
    let (st, v) = send_to(&s, &other, "r1", json!({"user": "alice", "title": "x"}));
    assert_eq!(st, 403, "{v}");
    assert_eq!(v["error"]["code"], "forbidden_user", "{v}");

    // The metrics count the alert for each kind of target.
    let metrics = metrics_of(&s);
    assert!(
        metrics.contains("portal_inbox_alerts_accepted_total{source=\"producer\",kind=\"alert\",target=\"user\"} 1"),
        "{metrics}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_form_of_the_page_sends_an_alert_to_a_group_or_to_an_account() {
    let mut s = Server::setup();
    s.start();
    add_account(&s, "alice");
    add_account(&s, "dora");
    let a1 = enroll_as(&s, "alice-1", "alice").await.unwrap();
    let (mut c1, _) = Conn::open(s.port, &a1).await.unwrap();
    // An account that holds `alert.send` on a group and `alert.read`, but not `alert.send_user`.
    let group_only = s.account_with(
        "gwen",
        &[
            "alert.send on service:portal/inbox_group:infra",
            "alert.read on service:portal/inbox_group:infra",
        ],
    );
    // An account that holds both rights.
    let both = s.account_with(
        "boris",
        &[
            "alert.send on service:portal/inbox_group:infra",
            "alert.read on service:portal/inbox_group:infra",
            "alert.send_user on service:portal",
        ],
    );
    let root = s.session();
    let post = |cookie: &str, to: &str, key: &str| {
        let (st, head, text) = inbox_form(
            &s,
            cookie,
            "/auth/settings/inbox/send",
            &format!("to={}&title=hello&key={key}", form_value(to)),
        );
        // A form that works redirects. A form that the server refuses answers the page with 422.
        match st {
            303 => location(&head),
            422 => {
                let code = text.split("data-send-error=\"").nth(1).unwrap_or_default();
                format!("s={}", code.split('"').next().unwrap_or_default())
            }
            _ => panic!("{st}: {text}"),
        }
    };
    // The superadmin may do both.
    let loc = post(&root.cookie, "user:alice", "f1");
    assert!(loc.contains("open="), "{loc}");
    let msg = take(&mut c1).await;
    assert_eq!(
        (msg.title.as_str(), msg.group.as_str()),
        ("hello", "@direct")
    );
    // The page of the sender shows the alert, with its target.
    let (st, page) = inbox_page_as(&s, &root.cookie, "/auth/settings/inbox");
    assert_eq!(st, 200);
    assert!(
        page.contains("data-to=\"user:alice\"") && page.contains("@alice"),
        "{page}"
    );
    assert!(post(&root.cookie, "group:infra", "f2").contains("open="));
    // A forced form of an account without the right for an account is refused.
    assert!(post(&group_only.cookie, "user:alice", "f3").contains("s=bad_target"));
    assert!(post(&group_only.cookie, "group:infra", "f4").contains("open="));
    // An account with both rights sends to an account and reads its own alert (the sender).
    let loc = post(&both.cookie, "user:alice", "f5");
    assert!(loc.contains("open="), "{loc}");
    let id = loc
        .split("open=")
        .nth(1)
        .and_then(|r| r.split(['&', '#']).next())
        .unwrap()
        .to_string();
    let (st, page) = inbox_page_as(&s, &both.cookie, &format!("/auth/settings/inbox?open={id}"));
    assert_eq!(st, 200);
    assert!(
        page.contains(&format!("id=\"a-{id}\"")),
        "the sender reads the alert"
    );
    // The forms of the page refuse an unknown account, an account with no phone and a bad target.
    assert!(post(&both.cookie, "user:ghost", "f6").contains("s=bad_user"));
    assert!(post(&both.cookie, "user:dora", "f7").contains("s=no_phone"));
    assert!(post(&both.cookie, "alice", "f8").contains("s=bad_target"));
    assert!(post(&both.cookie, "team:infra", "f9").contains("s=bad_target"));
    assert!(post(&both.cookie, "group:nowhere", "f10").contains("s=bad_group"));
    // The old field `group` is gone: the form answers 400.
    let (st, _, _) = inbox_form(
        &s,
        &both.cookie,
        "/auth/settings/inbox/send",
        "group=infra&title=old&key=old1",
    );
    assert_eq!(st, 400);
    // The target account and an account that only reads groups: gwen reads infra, and the alert
    // for alice is not hers. The alert that boris sent is not in her list.
    let (st, page) = inbox_page_as(&s, &group_only.cookie, "/auth/settings/inbox");
    assert_eq!(st, 200);
    assert!(
        !page.contains("data-to=\"user:alice\""),
        "an alert for an account is not for a reader of a group"
    );
    let (st, page) = inbox_page_as(
        &s,
        &group_only.cookie,
        &format!("/auth/settings/inbox?open={id}"),
    );
    assert_eq!(st, 200);
    assert!(!page.contains(&format!("id=\"a-{id}\"")));
}

// An alert to an account that the configuration file declares (a block `user`).

/// Adds an account of the file before the start. Its password is `ACCOUNT_PASSWORD`.
fn add_file_account(s: &Server, login: &str, grants: &[&str]) {
    let hash = portal(&["hash-password"], Some(ACCOUNT_PASSWORD));
    assert!(hash.status.success());
    let path = s.dir.join(format!("{login}.hash"));
    std::fs::write(&path, String::from_utf8(hash.stdout).unwrap().trim()).unwrap();
    let list: Vec<String> = grants.iter().map(|g| format!("\"{g}\"")).collect();
    s.add_block(&format!(
        "user \"{login}\" {{\n  password_hash_file = \"{}\"\n  grants = [{}]\n}}\n",
        path.display(),
        list.join(", ")
    ));
}

/// Adds a key of the file before the start. The result is the text of its secret.
fn add_file_key(s: &Server, name: &str, owner: &str, direct_alerts: bool) -> String {
    let path = s.dir.join(format!("{name}.token"));
    portal_ok(&["inbox", "token", "--out", &path.to_string_lossy()]);
    s.add_to_inbox(&format!(
        "  api_key \"{name}\" {{\n    owner = \"{owner}\"\n    secret_file = \"{}\"\n    groups = [\"infra\"]\n    direct_alerts = {direct_alerts}\n  }}",
        path.display()
    ));
    std::fs::read_to_string(path).unwrap().trim().to_string()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_alert_to_an_account_of_the_file_goes_to_its_phones_and_its_notifications() {
    let mut s = Server::setup();
    // The sender, and the accounts of the file that get alerts. fred has a phone. frida has none.
    // Both can read the group infra, so the list of the alerts shows the alerts that they receive.
    let sender = [
        "access on service:portal",
        "keys.own.use on service:portal/key_kind:api",
        "alert.send on service:portal/inbox_group:infra",
        "alert.read on service:portal/inbox_group:infra",
        "alert.send_user on service:portal",
    ];
    let receiver = [
        "inbox.receive on service:portal/inbox_group:infra",
        "alert.read on service:portal/inbox_group:infra",
    ];
    add_file_account(&s, "gus", &sender);
    add_file_account(&s, "fred", &receiver);
    add_file_account(&s, "frida", &receiver);
    // The key of gus alerts accounts. The key of ci, the key of the group, does not.
    let direct = add_file_key(&s, "direct", "gus", true);
    s.start();
    add_account(&s, "alice");
    let gus = s.file_account("gus");
    let fred = s.file_account("fred");
    let frida = s.file_account("frida");
    let alice = s.account_with(
        "alice",
        &["inbox.receive on service:portal/inbox_group:infra"],
    );

    let fred_phone = enroll_as(&s, "fred-1", "fred").await.unwrap();
    assert_eq!(fred_phone.user_name, "fred", "the login is the key");
    let (mut conn, _) = Conn::open(s.port, &fred_phone).await.unwrap();
    let alice_phone = enroll_as(&s, "alice-1", "alice").await.unwrap();
    let (mut conn_alice, _) = Conn::open(s.port, &alice_phone).await.unwrap();
    let begin = notifications(&s, &fred.cookie, "").1["now"]
        .as_i64()
        .unwrap();
    std::thread::sleep(Duration::from_millis(20));

    // The key sends to the account of the file. The phone gets the frame of an alert to an account.
    let (st, v) = send_to(
        &s,
        &direct,
        "d1",
        json!({"user": "fred", "title": "wake fred"}),
    );
    assert_eq!(st, 201, "{v}");
    assert_eq!(v["targets"], 1, "{v}");
    let msg = take(&mut conn).await;
    assert_eq!(
        (msg.title.as_str(), msg.group.as_str()),
        ("wake fred", "@direct")
    );
    conn_alice.no_alert(Duration::from_millis(800)).await;

    // An account of the file with no phone: 409, as for an account of the database. The
    // superadmin and an account of the file that has no password exist too.
    for (key, login) in [("p1", "frida"), ("p2", "ci"), ("p3", SUPERADMIN)] {
        let (st, v) = send_to(&s, &direct, key, json!({"user": login, "title": "x"}));
        assert_eq!(
            (st, v["error"]["code"].as_str()),
            (409, Some("no_phone")),
            "{login}: {v}"
        );
    }
    // A login that does not exist gets the same answer, whatever its form. The message is the
    // same text with the login in it.
    let (st, ghost) = send_to(&s, &direct, "u1", json!({"user": "ghost", "title": "x"}));
    assert_eq!(
        (st, ghost["error"]["code"].as_str()),
        (404, Some("unknown_user"))
    );
    let (st, other) = send_to(&s, &direct, "u2", json!({"user": "Fred", "title": "x"}));
    assert_eq!(
        (st, other["error"]["code"].as_str()),
        (404, Some("unknown_user"))
    );
    assert_eq!(ghost["error"]["message"], "unknown user \"ghost\"");
    assert_eq!(other["error"]["message"], "unknown user \"Fred\"");
    // The key of the group has no `direct_alerts`: the owner may alert, the key may not.
    let (st, v) = send_to(&s, &s.token, "g1", json!({"user": "fred", "title": "x"}));
    assert_eq!(
        (st, v["error"]["code"].as_str()),
        (403, Some("forbidden_user"))
    );
    // The key that sent the alert reads it. The target is the login.
    let (st, _, v) = producer_call(
        &s,
        "GET",
        &format!("/api/v1/alerts/{}", msg.alert_id),
        &direct,
        "",
        "",
    );
    assert_eq!(st, 200, "{v}");
    assert_eq!(v["target"], json!({"kind": "user", "id": "fred"}), "{v}");

    // The notifications of fred have the alert, with the level Ring and a toast. The other
    // accounts, of the file or of the database, get nothing.
    let after = format!("?after={begin}");
    let (st, mine) = notifications(&s, &fred.cookie, &after);
    assert_eq!(st, 200, "{mine}");
    assert_eq!(titles(&mine), ["wake fred"], "{mine}");
    assert_eq!(mine["unread"], 1);
    assert_eq!(mine["items"][0]["level"], "ring");
    assert_eq!(mine["items"][0]["group"], "Direct");
    assert_eq!(mine["items"][0]["toast"], true);
    for (name, session) in [("frida", &frida), ("gus", &gus), ("alice", &alice)] {
        let (st, theirs) = notifications(&s, &session.cookie, &after);
        assert_eq!(st, 200, "{name}: {theirs}");
        assert_eq!(theirs["items"], json!([]), "{name}: {theirs}");
        assert_eq!(theirs["unread"], 0, "{name}");
    }
    // The superadmin has all the groups, and not the alert for an account.
    let root = s.session();
    assert_eq!(
        notifications(&s, &root.cookie, &after).1["items"],
        json!([])
    );

    // The list of the alerts: the target, the sender (the owner of the key) and the superadmin
    // read it. A reader of the group that is not the target does not.
    for (name, session, shown) in [
        ("fred", &fred, true),
        ("gus", &gus, true),
        ("root", &root, true),
        ("frida", &frida, false),
    ] {
        let (st, page) = inbox_page_as(&s, &session.cookie, "/auth/settings/inbox");
        assert_eq!(st, 200, "{name}");
        assert_eq!(page.contains("data-to=\"user:fred\""), shown, "{name}");
    }
    // The metrics count one alert to an account of the key.
    assert!(metrics_of(&s).contains(
        "portal_inbox_alerts_accepted_total{source=\"producer\",kind=\"alert\",target=\"user\"} 1"
    ));
    s.stop();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_popup_offers_the_accounts_of_the_file_and_sends_to_them() {
    let mut s = Server::setup();
    add_file_account(
        &s,
        "gus",
        &[
            "alert.send_user on service:portal",
            "alert.read on service:portal/inbox_group:infra",
        ],
    );
    add_file_account(
        &s,
        "fred",
        &["inbox.receive on service:portal/inbox_group:infra"],
    );
    add_file_account(&s, "frida", &[]);
    s.start();
    add_account(&s, "alice");
    let gus = s.file_account("gus");
    let fred = s.file_account("fred");
    enroll_as(&s, "fred-1", "fred").await.unwrap();
    let a1 = enroll_as(&s, "alice-1", "alice").await.unwrap();
    let (mut conn_alice, _) = Conn::open(s.port, &a1).await.unwrap();
    let fred_phone = enroll_as(&s, "fred-2", "fred").await.unwrap();
    let (mut conn, _) = Conn::open(s.port, &fred_phone).await.unwrap();

    // The list of the popup has the accounts of the database and of the file, each with its
    // phones. An account with no phone is disabled. The superadmin is not a row.
    let (st, page) = inbox_page_as(&s, &gus.cookie, "/auth/settings/inbox");
    assert_eq!(st, 200, "{page}");
    assert!(
        page.contains(r#"name="to" value="user:fred" data-phones="2""#),
        "{page}"
    );
    assert!(page.contains(r#"name="to" value="user:alice" data-phones="1""#));
    assert!(page.contains(r#"name="to" value="user:frida" disabled"#));
    assert!(page.contains(r#"name="to" value="user:gus" disabled"#));
    assert!(!page.contains(&format!("value=\"user:{SUPERADMIN}\"")));
    // An account without the right has no list.
    let (_, page) = inbox_page_as(&s, &fred.cookie, "/auth/settings/inbox");
    assert!(!page.contains("value=\"user:alice\""), "{page}");

    // The form sends to an account of the file, and the answers are the answers of any account.
    let post = |to: &str, key: &str| {
        let (st, head, text) = inbox_form(
            &s,
            &gus.cookie,
            "/auth/settings/inbox/send",
            &format!("to={}&title=hello&key={key}", form_value(to)),
        );
        match st {
            303 => location(&head),
            422 => {
                let code = text.split("data-send-error=\"").nth(1).unwrap_or_default();
                format!("s={}", code.split('"').next().unwrap_or_default())
            }
            _ => panic!("{st}: {text}"),
        }
    };
    let begin = notifications(&s, &fred.cookie, "").1["now"]
        .as_i64()
        .unwrap();
    std::thread::sleep(Duration::from_millis(20));
    let loc = post("user:fred", "f1");
    assert!(loc.contains("open="), "{loc}");
    let msg = take(&mut conn).await;
    assert_eq!(
        (msg.title.as_str(), msg.group.as_str()),
        ("hello", "@direct")
    );
    conn_alice.no_alert(Duration::from_millis(800)).await;
    assert!(post("user:frida", "f2").contains("s=no_phone"));
    assert!(post("user:ghost", "f3").contains("s=bad_user"));
    // The sender reads the alert that it sent, and fred sees it in its notifications.
    let (st, page) = inbox_page_as(&s, &gus.cookie, "/auth/settings/inbox");
    assert_eq!(st, 200);
    assert!(page.contains("data-to=\"user:fred\""), "{page}");
    let after = format!("?after={begin}");
    assert_eq!(
        titles(&notifications(&s, &fred.cookie, &after).1),
        ["hello"]
    );
    s.stop();
}

// The notifications of the signed-in person.

/// `GET /auth/settings/inbox/notifications` with the cookie of an account. `query` is the query
/// string with its `?`, or empty.
fn notifications(s: &Server, cookie: &str, query: &str) -> (u16, Value) {
    let host = format!("127.0.0.1:{}", s.port);
    let (st, _, body) = http_full(
        s.port,
        &host,
        "GET",
        &format!("/auth/settings/inbox/notifications{query}"),
        &[
            ("Cookie", cookie.to_string()),
            ("Accept", "application/json".into()),
        ],
        "",
    );
    (st, serde_json::from_str(&body).unwrap_or(Value::Null))
}

/// The token of the forms of the popover, from the launcher. A page of the tab Inbox would move the
/// marker "read up to" of the account.
fn home_csrf(s: &Server, cookie: &str) -> String {
    let (st, page) = inbox_page_as(s, cookie, "/");
    assert_eq!(st, 200);
    let at = page
        .find("/auth/settings/inbox/read")
        .expect("the popover has the form of the marker");
    let after = &page[at..];
    let name = after.find("name=\"csrf\"").expect("a token");
    let tag_start = at + after[..name].rfind('<').unwrap();
    let tag_end = at + name + after[name..].find('>').unwrap();
    attr(&page[tag_start..tag_end], "value").expect("the token has a value")
}

/// A form of the notifications. `json` asks for the answer of the script.
fn notify_form(
    s: &Server,
    cookie: &str,
    path: &str,
    fields: &str,
    json: bool,
) -> (u16, String, String) {
    let host = format!("127.0.0.1:{}", s.port);
    let csrf = home_csrf(s, cookie);
    let body = if fields.is_empty() {
        format!("csrf={csrf}")
    } else {
        format!("csrf={csrf}&{fields}")
    };
    let mut headers = vec![
        (
            "Content-Type",
            "application/x-www-form-urlencoded".to_string(),
        ),
        ("Origin", format!("http://{host}")),
        ("Cookie", cookie.to_string()),
    ];
    if json {
        headers.push(("Accept", "application/json".into()));
    }
    http_full(s.port, &host, "POST", path, &headers, &body)
}

fn titles(answer: &Value) -> Vec<String> {
    answer["items"]
        .as_array()
        .map(|items| {
            items
                .iter()
                .map(|i| i["title"].as_str().unwrap().to_string())
                .collect()
        })
        .unwrap_or_default()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_notifications_follow_the_rights_of_the_account() {
    let mut s = Server::setup();
    s.add_to_inbox("  group \"crit\" {\n    name = \"Critical\"\n    critical = true\n  }");
    s.start();
    let root = s.session();
    let mia = s.account_with(
        "mia",
        &[
            "inbox.receive on service:portal/inbox_group:infra",
            "inbox.receive on service:portal/inbox_group:crit",
        ],
    );
    let otto = s.account_with(
        "otto",
        &["inbox.receive on service:portal/inbox_group:other"],
    );
    let nina = s.account_with("nina", &["keys.own.read on service:portal"]);
    enroll_as(&s, "mia-phone", "mia").await.unwrap();

    // The first sight of an account: the level All, nothing unread, no alert, no cursor.
    let (st, first) = notifications(&s, &mia.cookie, "");
    assert_eq!(st, 200, "{first}");
    assert_eq!(first["level"], "all");
    assert_eq!(first["unread"], 0);
    assert_eq!(first["items"], json!([]));
    assert!(first["cursor"].is_null(), "{first}");
    let begin = first["now"].as_i64().unwrap();
    assert_eq!(notifications(&s, &otto.cookie, "").1["unread"], 0);
    std::thread::sleep(Duration::from_millis(20));

    // An alert for each group, and one for the account.
    assert_eq!(
        post_alert(&s, "k1", json!({"group": "infra", "title": "for infra"})).0,
        201
    );
    let send = |to: &str, title: &str, key: &str| {
        let (st, head, _) = inbox_form(
            &s,
            &root.cookie,
            "/auth/settings/inbox/send",
            &format!(
                "to={}&title={}&key={key}",
                form_value(to),
                form_value(title)
            ),
        );
        assert_eq!(st, 303, "{head}");
    };
    send("group:other", "for other", "f1");
    send("group:crit", "for crit", "f2");
    send("user:mia", "for mia", "f3");
    let after = format!("?after={begin}");

    // A member gets the alerts of its groups and its own, and no other.
    let (st, mine) = notifications(&s, &mia.cookie, &after);
    assert_eq!(st, 200, "{mine}");
    assert_eq!(
        titles(&mine),
        ["for infra", "for crit", "for mia"],
        "{mine}"
    );
    assert!(!mine.to_string().contains("for other"));
    assert_eq!(mine["unread"], 3);
    let levels: Vec<&str> = mine["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["level"].as_str().unwrap())
        .collect();
    assert_eq!(levels, ["alert", "critical", "ring"]);
    assert!(mine["items"]
        .as_array()
        .unwrap()
        .iter()
        .all(|i| i["toast"] == true));
    let groups: Vec<&str> = mine["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["group"].as_str().unwrap())
        .collect();
    assert_eq!(groups, ["Infra", "Critical", "Direct"]);
    // The account has no `alert.read`: the alert has no row to open for it.
    assert!(mine["items"][0]["href"].is_null(), "{mine}");
    assert!(mine["cursor"].as_i64().unwrap() >= begin);
    // Another account sees its own group, and not the alert for the account of another.
    let (_, others) = notifications(&s, &otto.cookie, &after);
    assert_eq!(titles(&others), ["for other"], "{others}");
    // An account with no right of the inbox gets the same answer as for any other page of it.
    assert_eq!(notifications(&s, &nina.cookie, &after).0, 403);
    // The superadmin has all the groups, and not the alert for another account. Its rows open.
    let (_, all) = notifications(&s, &root.cookie, &after);
    assert_eq!(
        titles(&all),
        ["for infra", "for other", "for crit"],
        "{all}"
    );
    let href = all["items"][0]["href"].as_str().unwrap();
    assert!(href.starts_with("/auth/settings/inbox?open="), "{href}");

    // The cursor: the window of the lookback is 5 seconds. Past it the answer has no alert.
    let cursor = mine["cursor"].as_i64().unwrap();
    let (_, later) = notifications(&s, &mia.cookie, &format!("?after={}", cursor + 6_000));
    assert_eq!(later["items"], json!([]), "{later}");
    assert_eq!(later["unread"], 3);
    assert_eq!(later["cursor"].as_i64().unwrap(), cursor + 6_000);
    // The query is strict.
    for bad in [
        "?after=abc",
        "?after=-1",
        "?x=1",
        "?after=1&after=2",
        "?after=",
    ] {
        assert_eq!(notifications(&s, &mia.cookie, bad).0, 400, "{bad}");
    }

    // Without a session the answer is the redirect of the other pages of the settings.
    let host = format!("127.0.0.1:{}", s.port);
    let (st, head, _) = http_full(
        s.port,
        &host,
        "GET",
        "/auth/settings/inbox/notifications?after=1",
        &[],
        "",
    );
    let (st_page, head_page, _) = http_full(s.port, &host, "GET", "/auth/settings/inbox", &[], "");
    assert_eq!((st, st_page), (303, 303));
    assert!(location(&head).starts_with("/auth/login"), "{head}");
    assert!(location(&head_page).starts_with("/auth/login"));

    // The level "Alert and Critical only" drops the toast of an alert for the account. The count
    // still has it.
    let (st, head, _) = notify_form(
        &s,
        &mia.cookie,
        "/auth/settings/inbox/notify",
        "level=important",
        false,
    );
    assert_eq!(
        (st, location(&head).as_str()),
        (303, "/auth/settings/inbox")
    );
    send("user:mia", "ring two", "f4");
    let (_, mine) = notifications(&s, &mia.cookie, &after);
    assert_eq!(mine["level"], "important");
    // The list has the alert for the account. The flag `toast` says it gives no toast.
    assert_eq!(
        titles(&mine),
        ["for infra", "for crit", "for mia", "ring two"],
        "{mine}"
    );
    let toasts: Vec<bool> = mine["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["toast"].as_bool().unwrap())
        .collect();
    assert_eq!(toasts, [true, true, false, false], "{mine}");
    assert_eq!(mine["unread"], 4);
    // The popover of the launcher lists the four, newest first.
    let (_, home) = inbox_page_as(&s, &mia.cookie, "/");
    for title in ["for infra", "for crit", "for mia", "ring two"] {
        assert!(home.contains(title), "{title}");
    }
    assert!(!home.contains("for other"));

    // "Mark all read" moves the marker. The script asks for JSON and gets the state.
    let (st, _, body) = notify_form(
        &s,
        &mia.cookie,
        "/auth/settings/inbox/read",
        "back=home",
        true,
    );
    let state: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(st, 200, "{body}");
    assert_eq!(
        (state["unread"].clone(), state["level"].clone()),
        (json!(0), json!("important"))
    );
    let (st, head, _) = notify_form(
        &s,
        &mia.cookie,
        "/auth/settings/inbox/read",
        "back=home",
        false,
    );
    assert_eq!((st, location(&head).as_str()), (303, "/"));
    assert_eq!(notifications(&s, &mia.cookie, "").1["unread"], 0);
    // The alerts that come after the marker are unread, and the page Inbox moves the marker too.
    send("group:crit", "for crit again", "f5");
    assert_eq!(notifications(&s, &mia.cookie, "").1["unread"], 1);
    let (st, _) = inbox_page_as(&s, &mia.cookie, "/auth/settings/inbox");
    assert_eq!(st, 200);
    assert_eq!(notifications(&s, &mia.cookie, "").1["unread"], 0);

    // The level Off: no count, no alert. The account of the other still has the default.
    let (st, _, body) = notify_form(
        &s,
        &mia.cookie,
        "/auth/settings/inbox/notify",
        "level=off",
        true,
    );
    assert_eq!(st, 200, "{body}");
    send("group:crit", "for crit three", "f6");
    let (_, off) = notifications(&s, &mia.cookie, &after);
    assert_eq!(
        (
            off["level"].clone(),
            off["unread"].clone(),
            off["items"].clone()
        ),
        (json!("off"), json!(0), json!([]))
    );
    assert_eq!(notifications(&s, &otto.cookie, "").1["level"], "all");
    // The popover of the launcher still lists what the account received, with no badge.
    let (_, home) = inbox_page_as(&s, &mia.cookie, "/");
    assert!(home.contains("for crit three"));
    // The forms are strict: a level, a page, a field that does not exist, the token.
    for (fields, expected) in [
        ("level=loud", 400),
        ("level=all&back=elsewhere", 400),
        ("level=all&extra=1", 400),
        ("", 400),
    ] {
        let (st, _, body) = notify_form(
            &s,
            &mia.cookie,
            "/auth/settings/inbox/notify",
            fields,
            false,
        );
        assert_eq!(st, expected, "{fields}: {body}");
    }
    let host = format!("127.0.0.1:{}", s.port);
    let (st, _, _) = http_full(
        s.port,
        &host,
        "POST",
        "/auth/settings/inbox/notify",
        &[
            ("Content-Type", "application/x-www-form-urlencoded".into()),
            ("Origin", format!("http://{host}")),
            ("Cookie", mia.cookie.clone()),
        ],
        "csrf=wrong&level=all",
    );
    assert_eq!(st, 403);
    // A GET of a form address is not allowed.
    let (st, _) = inbox_page_as(&s, &mia.cookie, "/auth/settings/inbox/notify");
    assert_eq!(st, 405);

    // The launcher: the icon is for an account that has a right of the inbox, and its names go
    // through the pass of the launcher.
    for (name, cookie, has_icon) in [
        ("mia", &mia.cookie, true),
        ("otto", &otto.cookie, true),
        ("root", &root.cookie, true),
        ("nina", &nina.cookie, false),
    ] {
        let (st, home) = inbox_page_as(&s, cookie, "/");
        assert_eq!(st, 200, "{name}");
        assert_eq!(home.contains("data-notify-bell"), has_icon, "{name}");
        assert_eq!(home.contains("data-notify-toast"), has_icon, "{name}");
        for plain in [
            "nbell", "nrow", "nlist", "nbadge", "nlevel", "toasts", "toast__",
        ] {
            assert!(
                !home.contains(&format!("class=\"{plain}"))
                    && !home.contains(&format!(" {plain}\"")),
                "{name}: the class {plain} is not mapped"
            );
        }
    }
    s.stop();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn two_instances_give_the_same_notifications() {
    let Some(admin_pg) = pg_admin() else {
        eprintln!("INBOX_TEST_PG is not set: skipped");
        return;
    };
    let mut a = Server::setup_with(Some(admin_pg));
    let mut b = a.sibling("instance-b");
    a.start();
    b.start();
    let grants = ["inbox.receive on service:portal/inbox_group:infra"];
    let on_a = a.account_with("mia", &grants);
    let on_b = b.account_with("mia", &grants);

    let (_, first) = notifications(&b, &on_b.cookie, "");
    assert_eq!(first["unread"], 0);
    let begin = first["now"].as_i64().unwrap();
    std::thread::sleep(Duration::from_millis(20));
    // An alert accepted by A is in the answer of B at once: the store is the only state.
    assert_eq!(
        post_alert(&a, "shared", json!({"group": "infra", "title": "from a"})).0,
        201
    );
    let (st, seen) = notifications(&b, &on_b.cookie, &format!("?after={begin}"));
    assert_eq!(st, 200, "{seen}");
    assert_eq!(titles(&seen), ["from a"]);
    assert_eq!(seen["unread"], 1);
    assert_eq!(notifications(&a, &on_a.cookie, "").1["unread"], 1);
    // The marker that A moves is the marker that B reads. So is the level.
    let (st, _, _) = notify_form(&a, &on_a.cookie, "/auth/settings/inbox/read", "", true);
    assert_eq!(st, 200);
    assert_eq!(notifications(&b, &on_b.cookie, "").1["unread"], 0);
    let (st, _, _) = notify_form(
        &b,
        &on_b.cookie,
        "/auth/settings/inbox/notify",
        "level=off",
        true,
    );
    assert_eq!(st, 200);
    assert_eq!(notifications(&a, &on_a.cookie, "").1["level"], "off");
    a.stop();
    b.stop();
}

/// One Alertmanager alert of a webhook payload, with the start time of the audit case.
fn am_alert(fingerprint: &str, name: &str, summary: &str) -> Value {
    json!({"status": "firing",
           "labels": {"alertname": name, "instance": fingerprint},
           "annotations": {"summary": summary},
           "startsAt": "2026-11-05T11:00:00Z", "fingerprint": fingerprint})
}

/// The audit case of a webhook payload: a new summary of a stored alert, the same alert to a second
/// group, and a control character in a label. Each alert of the payload is stored, and a repeat
/// keeps the first title.
#[test]
fn each_alert_of_a_webhook_payload_is_stored_and_a_new_summary_is_a_repeat() {
    let mut s = Server::setup();
    // One key of the file on two groups.
    let text = std::fs::read_to_string(s.config()).unwrap();
    let text = text
        .replace(
            "    \"alert.send on service:portal/inbox_group:infra\",\n",
            "    \"alert.send on service:portal/inbox_group:infra\",\n    \"alert.send on service:portal/inbox_group:other\",\n",
        )
        .replace("groups = [\"infra\"]", "groups = [\"infra\", \"other\"]");
    std::fs::write(s.config(), text).unwrap();
    s.start();
    let auth = [("Authorization", format!("Bearer {}", s.token))];
    let post = |group: &str, alerts: Vec<Value>| -> (u64, u64, u64) {
        let body = json!({"status": "firing", "alerts": alerts}).to_string();
        let (st, answer) = http(
            s.port,
            "POST",
            &format!("/api/v1/webhooks/alertmanager/{group}"),
            &auth,
            &body,
        );
        assert_eq!(st, 200, "{answer}");
        let v: Value = serde_json::from_str(&answer).unwrap();
        let n = |k: &str| v[k].as_u64().unwrap();
        (n("created"), n("duplicates"), n("refused"))
    };

    // A is stored. The next payload has B, A with a new summary, and C.
    assert_eq!(
        post("infra", vec![am_alert("fpA", "Load", "load 0.91")]),
        (1, 0, 0)
    );
    assert_eq!(
        post(
            "infra",
            vec![
                am_alert("fpB", "Load", "load 0.80"),
                am_alert("fpA", "Load", "load 0.97"),
                am_alert("fpC", "Load", "load 0.85"),
            ]
        ),
        (2, 1, 0)
    );
    assert_eq!(
        post("infra", vec![am_alert("fpC", "Load", "load 0.85")]),
        (0, 1, 0)
    );
    // The same alert to the second group makes an alert of that group.
    assert_eq!(
        post("infra", vec![am_alert("fpD", "Disk", "disk 95%")]),
        (1, 0, 0)
    );
    assert_eq!(
        post(
            "other",
            vec![
                am_alert("fpD", "Disk", "disk 95%"),
                am_alert("fpE", "Disk", "disk 96%"),
            ]
        ),
        (2, 0, 0)
    );
    assert_eq!(
        post("other", vec![am_alert("fpE", "Disk", "disk 96%")]),
        (0, 1, 0)
    );
    // A control character in the label alertname becomes a space.
    assert_eq!(
        post(
            "infra",
            vec![
                am_alert("fpF", "Bad\u{7}Name", "x"),
                am_alert("fpG", "Net", "link down"),
            ]
        ),
        (2, 0, 0)
    );
    assert_eq!(
        post("infra", vec![am_alert("fpG", "Net", "link down")]),
        (0, 1, 0)
    );

    let (st, v) = admin(&s, "GET", "/auth/inbox/api/alerts", "");
    assert_eq!(st, 200);
    let mut seen: Vec<(String, String)> = v["alerts"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| {
            (
                a["group"].as_str().unwrap().to_string(),
                a["title"].as_str().unwrap().to_string(),
            )
        })
        .collect();
    seen.sort();
    let want: Vec<(String, String)> = [
        ("infra", "[FIRING] Bad Name: x"),
        ("infra", "[FIRING] Disk: disk 95%"),
        ("infra", "[FIRING] Load: load 0.80"),
        ("infra", "[FIRING] Load: load 0.85"),
        ("infra", "[FIRING] Load: load 0.91"),
        ("infra", "[FIRING] Net: link down"),
        ("other", "[FIRING] Disk: disk 95%"),
        ("other", "[FIRING] Disk: disk 96%"),
    ]
    .iter()
    .map(|(g, t)| (g.to_string(), t.to_string()))
    .collect();
    assert_eq!(seen, want, "the first title of A stays");
}

/// A key of the file follows its owner for each action. The groups of its block give
/// `alert.send`. The delivery status of an alert of a group needs `alert.read` of the owner on the
/// group, as for a key of the database.
#[test]
fn a_key_of_the_file_reads_the_status_of_an_alert_only_with_alert_read_of_its_owner() {
    let mut s = Server::setup();
    let grant = "    \"alert.read on service:portal/inbox_group:infra\",\n";
    let with_read = std::fs::read_to_string(s.config()).unwrap();
    assert!(with_read.contains(grant));
    std::fs::write(s.config(), with_read.replace(grant, "")).unwrap();
    s.start();
    let (st, v) = post_alert(&s, "k1", json!({"group": "infra", "title": "status"}));
    assert_eq!(st, 201, "{v}");
    let id = v["alert_id"].as_str().unwrap().to_string();
    let by_header = |s: &Server| {
        http(
            s.port,
            "GET",
            &format!("/api/v1/alerts/{id}"),
            &[("Authorization", format!("Bearer {}", s.token))],
            "",
        )
    };
    // The key in the query is no key: 401, as a request without the header.
    let by_query = |s: &Server| {
        http(
            s.port,
            "GET",
            &format!("/api/v1/alerts/{id}?token={}", s.token),
            &[],
            "",
        )
    };
    // The owner may send to infra and may not read it: the same answer as for no alert.
    let (st, body) = by_header(&s);
    assert_eq!(st, 404, "{body}");
    assert!(!body.contains("deliveries"), "{body}");
    assert_eq!(by_query(&s).0, 401);
    // The owner gets alert.read on infra: the key reads the status.
    s.stop();
    std::fs::write(s.config(), &with_read).unwrap();
    s.start();
    let (st, body) = by_header(&s);
    assert_eq!(st, 200, "{body}");
    assert_eq!(by_query(&s).0, 401);
}

/// The API of the inbox reads the key only in the header `Authorization: Bearer` (ProcurAuth 1.4).
/// A valid key in the query `token` gets `401` on each route, as a request without the header.
/// The log of the server, the metrics and the answers hold nothing of the key.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_key_in_the_query_gets_401_and_no_log_holds_it() {
    let mut s = Server::setup();
    s.start();
    let key = s.token.clone();
    let auth = ("Authorization", format!("Bearer {key}"));
    let dev = enroll_as(&s, "phone", SUPERADMIN).await.unwrap();
    let (mut c, _) = Conn::open(s.port, &dev).await.unwrap();
    c.subscribe("infra").await;
    // The key is valid: with the header, it sends to infra and reads the status of the alert.
    let (st, v) = post_alert(&s, "q1", json!({"group": "infra", "title": "by header"}));
    assert_eq!(st, 201, "{v}");
    let id = v["alert_id"].as_str().unwrap().to_string();
    assert_eq!(take(&mut c).await.title, "by header");
    let (st, body) = http(
        s.port,
        "GET",
        &format!("/api/v1/alerts/{id}"),
        std::slice::from_ref(&auth),
        "",
    );
    assert_eq!(st, 200, "{body}");

    // The same key in the query only: 401 on the alerts, the notify route, the webhook and the
    // read of an alert. Also with a title, and with the key in the form of a header value.
    let webhook =
        r#"{"alerts":[{"status":"firing","labels":{"alertname":"Query"},"fingerprint":"q"}]}"#;
    let routes = [
        (
            "POST",
            "/api/v1/alerts".to_string(),
            r#"{"group":"infra","title":"by query"}"#,
        ),
        ("POST", "/api/v1/notify/infra".to_string(), "by query"),
        (
            "POST",
            "/api/v1/webhooks/alertmanager/infra".to_string(),
            webhook,
        ),
        ("GET", format!("/api/v1/alerts/{id}"), ""),
    ];
    let host = format!("localhost:{}", s.port);
    for (method, path, body) in &routes {
        for query in [
            format!("token={key}"),
            format!("title=by%20query&token={key}"),
            format!("token=Bearer%20{key}"),
        ] {
            let url = format!("{path}?{query}");
            let (st, head, answer) = http_full(s.port, &host, method, &url, &[], body);
            assert_eq!(st, 401, "{method} {url}: {answer}");
            assert!(answer.contains("\"unauthorized\""), "{answer}");
            assert!(
                !answer.contains(&key) && !head.contains(&key),
                "{head}{answer}"
            );
        }
    }
    // No request of the query made an alert.
    c.no_alert(Duration::from_millis(1500)).await;

    // The query `token` is no parameter of the API: with the header, a wrong key in the query
    // changes nothing.
    let (st, body) = http(
        s.port,
        "POST",
        "/api/v1/notify/infra?token=api_wrong&title=header%20wins",
        std::slice::from_ref(&auth),
        "",
    );
    assert_eq!(st, 201, "{body}");
    assert_eq!(take(&mut c).await.title, "header wins");

    // The log has a line for each request of the query, with the path and no query. No piece of
    // ten characters of the key is in the log or in the metrics.
    let metrics = metrics_of(&s);
    let log = s.log();
    for (method, path, _) in &routes {
        let line = format!("\"method\":\"{method}\",\"path\":\"{path}\",\"status\":\"401\"");
        assert_eq!(log.matches(&line).count(), 3, "{line}: {log}");
    }
    assert!(!log.contains("token="), "{log}");
    let random = &key["api_".len()..];
    for start in 0..=random.len() - 10 {
        let piece = &random[start..start + 10];
        assert!(
            !log.contains(piece),
            "the log holds {piece:?} of the key: {log}"
        );
        assert!(
            !metrics.contains(piece),
            "the metrics hold {piece:?} of the key"
        );
    }
    s.stop();
}

/// The total of the graph and the title of each bar, as the zone Sent draws them.
fn graph_of(page: &str) -> (String, Vec<String>) {
    let total = page
        .find("over the period: ")
        .map(|at| {
            let rest = &page[at + "over the period: ".len()..];
            rest.split('"').next().unwrap_or("").to_string()
        })
        .unwrap_or_else(|| "<no graph>".to_string());
    let bars: Vec<String> = page
        .match_indices("<title>")
        .filter_map(|(at, _)| {
            let rest = &page[at + 7..];
            let text = rest.split("</title>").next()?;
            text.contains("UTC:").then(|| text.to_string())
        })
        .collect();
    (total, bars)
}

/// The graph of the zone Sent counts the groups that the account may read, as the rows do. A
/// group where the account holds `alert.send` only adds nothing.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_graph_of_the_inbox_counts_only_the_groups_that_the_account_may_read() {
    let mut s = Server::setup();
    s.start();
    for n in 0..4 {
        let (st, v) = post_alert(
            &s,
            &format!("graph-{n}"),
            json!({"group": "infra", "title": format!("graph title {n}")}),
        );
        assert_eq!(st, 201, "{v}");
    }
    // vic: alert.send on infra and alert.send_user. sam: alert.send on infra, alert.read on other.
    let vic = s.account_with(
        "vic",
        &[
            "alert.send on service:portal/inbox_group:infra",
            "alert.send_user on service:portal",
        ],
    );
    let sam = s.account_with(
        "sam",
        &[
            "alert.send on service:portal/inbox_group:infra",
            "alert.read on service:portal/inbox_group:other",
        ],
    );
    let una = s.account_with("una", &["alert.read on service:portal/inbox_group:infra"]);
    for path in ["/auth/settings/inbox", "/auth/settings/inbox?g=infra"] {
        for (who, cookie) in [("vic", &vic.cookie), ("sam", &sam.cookie)] {
            let (st, page) = inbox_page_as(&s, cookie, path);
            assert_eq!(st, 200, "{who} {path}");
            assert!(
                page.contains("id=\"sent-title\""),
                "{who}: the zone Sent shows"
            );
            let (total, bars) = graph_of(&page);
            assert_eq!(total, "0 in total", "{who} {path}");
            assert!(bars.is_empty(), "{who} {path}: {bars:?}");
            assert_eq!(page.matches("graph title").count(), 0, "{who} {path}");
        }
        // A reader of infra still counts the four alerts.
        let (st, page) = inbox_page_as(&s, &una.cookie, path);
        assert_eq!(st, 200);
        assert_eq!(graph_of(&page).0, "4 in total", "una {path}");
    }
}

// ---------------------------------------------------------------- the end of an account

/// The rows of the zone Phones of the superadmin: the name and the login of each device.
fn phone_logins(s: &Server) -> Vec<(String, String, String)> {
    let (_, overview) = admin(s, "GET", "/auth/inbox/api/overview", "");
    overview["devices"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|d| {
            (
                d["device"]["name"].as_str().unwrap().to_string(),
                d["device"]["user_name"].as_str().unwrap().to_string(),
                d["status"].as_str().unwrap().to_string(),
            )
        })
        .collect()
}

/// The phones that the page "My phones" of an account lists as its own.
fn own_phone_rows(s: &Server, cookie: &str, login: &str) -> Vec<String> {
    let (st, page) = inbox_page_as(s, cookie, "/auth/settings/inbox");
    assert_eq!(st, 200, "{page}");
    rows(&page, "data-device")
        .into_iter()
        .filter(|r| r.contains(&format!("data-login=\"{login}\"")))
        .collect()
}

/// The audit finding portal/inbox/phones-of-removed-file-account-stay-active (vrf-34, inverted): the
/// operator removes the block `user` of an account of the file and starts the server again. The
/// phones of the account stop, its unused code stops, and a later database account with the same
/// login gets none of its phones or alerts.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_removed_account_of_the_file_loses_its_phones_and_its_codes() {
    let mut s = Server::setup_with(None);
    add_file_account(
        &s,
        "dave",
        &["inbox.receive on service:portal/inbox_group:infra"],
    );
    s.start();
    let port = s.port;
    let _dave = s.file_account("dave");
    let phone = enroll_as(&s, "dave-phone", "dave").await.unwrap();
    let spare = code_for(&s, "dave");
    {
        let (mut conn, _) = Conn::open(port, &phone).await.unwrap();
        conn.subscribe("infra").await;
    }
    let root = s.session();
    let (st, head, page) = inbox_form(
        &s,
        &root.cookie,
        "/auth/settings/inbox/send",
        &format!(
            "to={}&title={}&key=e2e-file-1",
            form_value("user:dave"),
            form_value("a note for the first dave")
        ),
    );
    assert_eq!(st, 303, "{head}{page}");
    s.stop();
    // The operator removes the block `user "dave"` from the file and starts the server again.
    let text = std::fs::read_to_string(s.config()).unwrap();
    let start = text.find("user \"dave\" {").expect("the block of dave");
    let end = start + text[start..].find("\n}\n").unwrap() + 3;
    std::fs::write(s.config(), format!("{}{}", &text[..start], &text[end..])).unwrap();
    s.start();
    assert_eq!(sign_in_request(port, "dave", ACCOUNT_PASSWORD).0, 401);

    // The phone gets no session, and a group alert finds no phone.
    assert_eq!(
        Conn::open(port, &phone).await.err(),
        Some(inbox_core::proto::close::REVOKED)
    );
    let (st, v) = post_alert(
        &s,
        "e2e-file-2",
        json!({"group": "infra", "title": "after the removal"}),
    );
    assert_eq!((st, v["targets"].as_u64()), (201, Some(0)), "{v}");
    // The unused code of the account is gone.
    assert!(matches!(
        enroll_code(&spare, "dave-spare").await,
        Err(EnrollFail::Closed(Some(4410)))
    ));
    // The zone Phones keeps the revoked row, without the login.
    let rows = phone_logins(&s);
    let (_, login, status) = rows.iter().find(|r| r.0 == "dave-phone").unwrap();
    assert_eq!(status, "revoked", "{rows:?}");
    assert_ne!(login, "dave", "{rows:?}");
    assert!(!rows.iter().any(|r| r.0 == "dave-spare"), "{rows:?}");

    // A later database account with the same login has no phone and no old alert.
    let new_dave = s.account_with(
        "dave",
        &["inbox.receive on service:portal/inbox_group:other"],
    );
    assert!(own_phone_rows(&s, &new_dave.cookie, "dave").is_empty());
    let (st, v) = notifications(&s, &new_dave.cookie, "?after=0");
    assert_eq!(st, 200);
    assert!(titles(&v).is_empty(), "{v}");
    assert_eq!(
        Conn::open(port, &phone).await.err(),
        Some(inbox_core::proto::close::REVOKED)
    );
    s.stop();
}

/// The same finding: the operator changes the login of the superadmin. The phones of the old login
/// stop.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_phones_of_an_old_login_of_the_superadmin_stop() {
    let mut s = Server::setup_with(None);
    s.start();
    let port = s.port;
    let phone = enroll_as(&s, "root-phone", SUPERADMIN).await.unwrap();
    {
        let (mut conn, _) = Conn::open(port, &phone).await.unwrap();
        conn.subscribe("infra").await;
    }
    s.stop();
    std::fs::write(s.dir.join("superadmin_login"), "newroot").unwrap();
    s.start();
    assert_eq!(
        sign_in_request(port, SUPERADMIN, SUPERADMIN_PASSWORD).0,
        401
    );
    assert_eq!(
        Conn::open(port, &phone).await.err(),
        Some(inbox_core::proto::close::REVOKED)
    );
    let (st, v) = post_alert(
        &s,
        "e2e-rename-1",
        json!({"group": "infra", "title": "after the rename"}),
    );
    assert_eq!((st, v["targets"].as_u64()), (201, Some(0)), "{v}");
    // The first look of the rights after the start ends the old login too, without a session.
    assert!(s.log().contains("account_inbox_ended"), "{}", s.log());
    s.stop();
}

/// The audit finding portal/inbox/login-reuse-inherits-login-keyed-data (vrf-33, inverted): a new
/// account that gets the login of a deleted account reads none of its records. A control login
/// that never existed reads none either.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_new_account_with_the_login_of_a_deleted_account_inherits_nothing() {
    let mut s = Server::setup();
    s.start();
    let root = s.session();
    let grants = [
        "inbox.receive on service:portal/inbox_group:infra",
        "alert.send_user on service:portal",
    ];
    s.account("bob");
    enroll_as(&s, "reuse-bob-phone", "bob").await.unwrap();
    let first = s.account_with("erin", &grants);
    enroll_as(&s, "reuse-first-erin-phone", "erin")
        .await
        .unwrap();
    let (st, head, _) = inbox_form(
        &s,
        &root.cookie,
        "/auth/settings/inbox/send",
        &format!(
            "to={}&title={}&key=reuse-a",
            form_value("user:erin"),
            form_value("a note for the first erin")
        ),
    );
    assert_eq!(st, 303, "{head}");
    let (st, head, _) = inbox_form(
        &s,
        &first.cookie,
        "/auth/settings/inbox/send",
        &format!(
            "to={}&title={}&key=reuse-b",
            form_value("user:bob"),
            form_value("sent by the first erin to bob")
        ),
    );
    assert_eq!(st, 303, "{head}");
    let (_, before) = notifications(&s, &first.cookie, "?after=0");
    assert!(titles(&before).contains(&"a note for the first erin".to_string()));

    delete_account(&s, "erin");
    let second = s.account_with("erin", &grants);
    let control = s.account_with("frank", &grants);
    for (who, cookie) in [("second erin", &second.cookie), ("frank", &control.cookie)] {
        let (st, v) = notifications(&s, cookie, "?after=0");
        assert_eq!(st, 200, "{who}");
        assert!(titles(&v).is_empty(), "{who}: {v}");
        let (st, page) = inbox_page_as(&s, cookie, "/auth/settings/inbox");
        assert_eq!(st, 200, "{who}");
        assert!(!page.contains("reuse-first-erin-phone"), "{who}");
        assert!(!page.contains("sent by the first erin to bob"), "{who}");
        let (st, home) = inbox_page_as(&s, cookie, "/");
        assert_eq!(st, 200, "{who}");
        assert!(!home.contains("a note for the first erin"), "{who}");
    }
    // The new erin may use the key of the old one again: the form takes it as a new alert.
    enroll_as(&s, "reuse-second-erin-phone", "erin")
        .await
        .unwrap();
    let (st, head, page) = inbox_form(
        &s,
        &second.cookie,
        "/auth/settings/inbox/send",
        &format!(
            "to={}&title={}&key=reuse-b",
            form_value("user:bob"),
            form_value("sent by the second erin to bob")
        ),
    );
    assert_eq!(st, 303, "{head}{page}");
    // Bob keeps the alert of the first erin, and the superadmin sees both.
    let bob = s.account("bob");
    let (_, v) = notifications(&s, &bob.cookie, "?after=0");
    let got = titles(&v);
    assert!(
        got.contains(&"sent by the first erin to bob".to_string()),
        "{v}"
    );
    assert!(
        got.contains(&"sent by the second erin to bob".to_string()),
        "{v}"
    );
    s.stop();
}

/// One POST of the plus button with a token that the test read before. The result is the code
/// text when the answer has one.
fn code_post(port: u16, cookie: &str, csrf: &str) -> Option<String> {
    let host = format!("127.0.0.1:{port}");
    let (st, _, page) = http_full(
        port,
        &host,
        "POST",
        "/auth/settings/inbox/code",
        &[
            ("Content-Type", "application/x-www-form-urlencoded".into()),
            ("Origin", format!("http://{host}")),
            ("Cookie", cookie.to_string()),
        ],
        &format!("csrf={csrf}"),
    );
    (st == 200 && page.contains("id=\"enroll-code\"")).then(|| code_in(&page))
}

/// The audit finding portal/inbox/enrollment-code-outlives-account-delete-race (hunter-14 check-04
/// and vrf-32, inverted): the account asks for codes again and again while the superadmin deletes
/// it. No code that the answers gave enrolls a phone after the delete.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_code_asked_while_the_account_is_deleted_enrolls_no_phone() {
    // SQLite: the test reads the codes in the file. A phone of a login without an account also
    // stops at its enrollment there, so the read of the file is the check of the codes.
    let mut s = Server::setup_with(None);
    // Each refused code counts as a failure of the address: the test tries many of them.
    let text = std::fs::read_to_string(s.config()).unwrap();
    assert!(text.contains("max_auth_failures = 10\n"));
    std::fs::write(
        s.config(),
        text.replace("max_auth_failures = 10\n", "max_auth_failures = 1000\n"),
    )
    .unwrap();
    s.start();
    let port = s.port;
    let mut given = 0;
    for round in 0..6 {
        let login = format!("racer{round}");
        let racer = s.account(&login);
        let csrf = inbox_csrf(port, &racer.cookie);
        assert!(!csrf.is_empty());
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let askers: Vec<_> = (0..3)
            .map(|_| {
                let (cookie, csrf, stop) = (racer.cookie.clone(), csrf.clone(), stop.clone());
                std::thread::spawn(move || {
                    let mut codes = Vec::new();
                    while !stop.load(std::sync::atomic::Ordering::SeqCst) {
                        codes.extend(code_post(port, &cookie, &csrf));
                    }
                    codes
                })
            })
            .collect();
        std::thread::sleep(Duration::from_millis(300));
        delete_account(&s, &login);
        std::thread::sleep(Duration::from_millis(300));
        stop.store(true, std::sync::atomic::Ordering::SeqCst);
        let codes: Vec<String> = askers.into_iter().flat_map(|t| t.join().unwrap()).collect();
        given += codes.len();
        // No code of the deleted account is left: the end of the account found every one.
        let conn = rusqlite::Connection::open(s.dir.join("portal.db")).unwrap();
        conn.busy_timeout(Duration::from_secs(5)).unwrap();
        let unused: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM enrollment_codes WHERE login = ?1 AND used_at_ms IS NULL",
                [&login],
                |r| r.get(0),
            )
            .unwrap();
        drop(conn);
        assert_eq!(
            unused, 0,
            "round {round}: codes of the deleted account {login} are left"
        );
        for code in &codes {
            let enrolled = enroll_code(code, &format!("{login}-after-delete")).await;
            assert!(
                matches!(enrolled, Err(EnrollFail::Closed(Some(4410)))),
                "round {round}: a code of the deleted account {login} enrolled: {enrolled:?}"
            );
        }
    }
    assert!(given > 0, "the test made no code at all");
    assert!(
        !phone_logins(&s)
            .iter()
            .any(|(name, _, status)| name.ends_with("-after-delete") && status != "revoked"),
        "{:?}",
        phone_logins(&s)
    );
    s.stop();
}

/// The same finding with PostgreSQL: an instance whose file lacks an account of the file gives the
/// phone of that account no session (1013) and revokes nothing. The instance whose file has the
/// account serves the phone.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_instance_that_does_not_know_an_account_of_the_file_revokes_nothing() {
    let Some(admin_pg) = pg_admin() else {
        eprintln!("INBOX_TEST_PG is not set: skipped");
        return;
    };
    let mut a = Server::setup_with(Some(admin_pg));
    // The file of the second instance has no block `user "fred"`: a change of the file that
    // reached only the first instance.
    let mut b = a.sibling("instance-b");
    add_file_account(
        &a,
        "fred",
        &["inbox.receive on service:portal/inbox_group:infra"],
    );
    a.start();
    b.start();
    let _fred = a.file_account("fred");
    let phone = enroll_as(&a, "fred-phone", "fred").await.unwrap();
    assert_eq!(Conn::open(b.port, &phone).await.err(), Some(1013));
    let (mut conn, _) = Conn::open(a.port, &phone).await.unwrap();
    conn.subscribe("infra").await;
    // After a look of the rights of the second instance, the phone still works on the first one.
    tokio::time::sleep(Duration::from_secs(5)).await;
    assert_eq!(Conn::open(b.port, &phone).await.err(), Some(1013));
    let (st, v) = post_alert(
        &a,
        "e2e-not-here-1",
        json!({"group": "infra", "title": "for fred"}),
    );
    assert_eq!(st, 201, "{v}");
    assert_eq!(conn.alert().await.title, "for fred");
}

/// The audit finding portal/inbox/degraded-cache-ignores-remote-revocation (vrf-30, inverted): a
/// phone that instance A revokes just before PostgreSQL goes away gets no session on instance B,
/// which got the notification of the revocation.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_revocation_on_one_instance_holds_on_the_other_while_postgresql_is_away() {
    use inbox_core::proto::close;
    let Some(admin_pg) = pg_admin() else {
        eprintln!("INBOX_TEST_PG is not set: skipped");
        return;
    };
    let proxy = PgProxy::start(&admin_pg.host, admin_pg.port);
    let mut a = Server::setup_with(Some(admin_pg.clone()));
    through(&a, &admin_pg, proxy.port);
    quiet_phones(&a);
    let mut b = a.sibling("instance-b");
    a.start();
    b.start();
    let phone = enroll_as(&a, "phone-stale", "alice").await.unwrap();
    let (mut on_b, _) = Conn::open(b.port, &phone).await.unwrap();
    on_b.subscribe("infra").await;
    // The copy of B holds the phone as not revoked, with its subscription.
    tokio::time::sleep(Duration::from_secs(7)).await;
    let (st, v) = admin(
        &a,
        "POST",
        &format!("/auth/inbox/api/devices/{}/revoke", phone.device_id),
        "{}",
    );
    assert_eq!(st, 200, "{v}");
    assert_eq!(on_b.closed_code().await, Some(close::REVOKED));
    // The database goes away before the next refresh of the copy of B.
    proxy.blackhole();
    for s in [&a, &b] {
        wait_for(
            "the instance to be degraded",
            Duration::from_secs(30),
            || http(s.port, "GET", "/readyz", &[], "").1 == "degraded",
        );
    }
    assert_eq!(Conn::open(a.port, &phone).await.err(), Some(close::REVOKED));
    assert_eq!(Conn::open(b.port, &phone).await.err(), Some(close::REVOKED));
    proxy.pass();
}

/// The rows of the account `login` in a SQLite file. The file is copied first: the replica can
/// rename a new snapshot over it at any time.
fn account_rows(dir: &std::path::Path, path: &std::path::Path, login: &str) -> i64 {
    let copy = dir.join(unique_name("rows-copy"));
    std::fs::copy(path, &copy).unwrap();
    let conn = rusqlite::Connection::open(&copy).unwrap();
    let n = conn
        .query_row(
            "SELECT COUNT(*) FROM users WHERE login = ?1",
            [login],
            |r| r.get(0),
        )
        .unwrap();
    drop(conn);
    let _ = std::fs::remove_file(&copy);
    n
}

/// The audit finding portal/inbox-replica/restore-reverts-unsynced-portal-revocations (vrf-24,
/// inverted): the delete of an account is in the shared directory when the superadmin sees it
/// done. An unclean stop, then a restart on the same disk or a takeover on an empty disk, keeps the
/// account deleted.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_deleted_account_stays_deleted_after_an_unclean_stop() {
    let mut s = Server::setup_with(None);
    let shared = s.dir.join("shared");
    let local = s.dir.join("portal.db");
    // Ten minutes between two periodic snapshots: after the first one, only the wait of a form can
    // make one in this test.
    s.add_to_inbox(&format!(
        r#"  replica {{
    dir               = "{}"
    snapshot_interval = "10m"
    lease_ttl         = "5s"
    sync_timeout      = "5s"
    sync_alerts       = true
  }}"#,
        shared.display()
    ));
    s.start();
    add_account(&s, "eve");
    assert_eq!(sign_in_request(s.port, "eve", ACCOUNT_PASSWORD).0, 303);
    let snapshot = shared.join("snapshot-1.db");
    assert_eq!(account_rows(&s.dir, &snapshot, "eve"), 1);
    delete_account(&s, "eve");
    assert_eq!(
        account_rows(&s.dir, &snapshot, "eve"),
        0,
        "the answer of the delete waited for the copy"
    );
    let kill = |s: &mut Server| {
        let mut child = s.child.take().unwrap();
        child.kill().unwrap();
        child.wait().unwrap();
    };
    // The same node starts again on its own disk, which it keeps.
    kill(&mut s);
    s.start();
    assert!(s.log().contains("\"kept_local\":\"true\""), "{}", s.log());
    assert_eq!(sign_in_request(s.port, "eve", ACCOUNT_PASSWORD).0, 401);
    // The node is lost with its disk: the next owner restores the shared directory.
    kill(&mut s);
    for suffix in ["", "-wal", "-shm"] {
        let _ = std::fs::remove_file(format!("{}{suffix}", local.display()));
    }
    s.start();
    assert!(s.log().contains("\"kept_local\":\"false\""), "{}", s.log());
    assert_eq!(sign_in_request(s.port, "eve", ACCOUNT_PASSWORD).0, 401);
    s.stop();
}

/// Protocol 5 breaks protocol 4. The hello of an app of version 4 ends with the close code 4426.
/// The hello of version 5 gets its session, and the new client frames get `not_enabled` for now.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn old_hello_protocol_4_gets_4426() {
    let mut s = Server::setup();
    s.start();
    let phone = enroll_as(&s, "phone", SUPERADMIN).await.unwrap();
    for protocol in [4u32, 6] {
        let (mut ws, _) =
            tokio_tungstenite::connect_async(format!("ws://127.0.0.1:{}/v1/session", s.port))
                .await
                .unwrap();
        let hello = serde_json::to_vec(&SessionHello {
            protocol,
            device_id: phone.device_id.clone(),
            app_version: "e2e".into(),
        })
        .unwrap();
        let (_, m1) =
            Initiator::start(&phone.device_private_key, &phone.server_public_key, &hello).unwrap();
        ws.send(Message::Binary(m1.into())).await.unwrap();
        assert_eq!(close_code(&mut ws).await, Some(4426), "protocol {protocol}");
    }
    let (mut c, welcome) = Conn::open(s.port, &phone).await.unwrap();
    assert!(
        matches!(welcome, ServerMsg::Welcome { ref prefs, body_purge_after_ms: 0, .. } if **prefs == Default::default())
    );
    c.send(&ClientMsg::Refresh {
        request_id: "a".repeat(32),
    })
    .await;
    match c.recv(Duration::from_secs(5)).await {
        Recv::Msg(ServerMsg::Error { code, .. }) => assert_eq!(code, "not_enabled"),
        other => panic!(
            "expected not_enabled, got {:?}",
            matches!(other, Recv::Timeout)
        ),
    }
    s.stop();
}
