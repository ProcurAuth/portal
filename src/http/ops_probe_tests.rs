//! The probes (`/healthz`, `/livez`, `/readyz`), the security headers and the log event `request`, most of them driven
//! over HTTP.
//! The servers and the helpers come from `cluster_tests`.

use super::cluster_tests::*;
use super::pdp_rig::InboxRig;
use super::*;
use crate::store::testing::Backend;

fn node(extra: &str) -> Node {
    start_with(Arc::new(Backend::Sqlite.fresh()), None, extra)
}

/// Checks the answers of one probe to `GET` and `HEAD`: the same status and headers, and no body for `HEAD`.
fn assert_probe(port: u16, path: &str, status: u16, body: &str) {
    let get = send(port, "GET", path, &[], "");
    assert_eq!(get.status, status, "GET {path}");
    assert_eq!(get.body, body, "GET {path}");
    assert!(
        get.header("cache-control")
            .is_some_and(|v| v.contains("no-store")),
        "GET {path}: {:?}",
        get.header("cache-control")
    );
    let head = send(port, "HEAD", path, &[], "");
    assert_eq!(head.status, status, "HEAD {path}");
    assert_eq!(head.body, "", "HEAD {path}");
    assert_eq!(
        head.header("content-length"),
        Some(body.len().to_string().as_str()),
        "HEAD {path}"
    );
    assert_eq!(
        head.header("cache-control"),
        get.header("cache-control"),
        "HEAD {path}"
    );
    assert_eq!(
        head.header("content-type"),
        get.header("content-type"),
        "HEAD {path}"
    );
}

#[test]
fn the_probes_answer_get_and_head_and_are_not_kept_in_a_cache() {
    let node = node("");
    assert_probe(node.port, HEALTH_PATH, 200, "ok\n");
    assert_probe(node.port, LIVE_PATH, 200, "ok\n");
    assert_probe(node.port, READY_PATH, 200, "ready\n");
    // `/livez` gives exactly the answer of `/healthz`. Only the date can change between the two.
    let without_date = |reply: Reply| -> Vec<(String, String)> {
        reply
            .headers
            .into_iter()
            .filter(|(name, _)| name != "date")
            .collect()
    };
    let health = send(node.port, "GET", HEALTH_PATH, &[], "");
    let live = send(node.port, "GET", LIVE_PATH, &[], "");
    assert_eq!(live.status, health.status);
    assert_eq!(live.body, health.body);
    assert_eq!(without_date(live), without_date(health));
    // A probe takes `GET` and `HEAD` only: the other methods keep the answer of the released versions.
    assert_ne!(send(node.port, "POST", LIVE_PATH, &[], "").status, 200);
}

#[test]
fn a_metrics_path_equal_to_livez_keeps_serving_the_metrics() {
    let node = node(r#"metrics { enabled = true path = "/livez" }"#);
    let reply = send(node.port, "GET", LIVE_PATH, &[], "");
    assert_eq!(reply.status, 200);
    assert!(reply.body.contains("portal_build_info"), "{}", reply.body);
    assert_eq!(
        route_metric_label(&node.state, LIVE_PATH),
        "metrics",
        "the label of the metrics wins"
    );
    assert_ne!(send(node.port, "HEAD", LIVE_PATH, &[], "").status, 200);
}

#[test]
fn the_inbox_readiness_answers_head_too() {
    let backend = Backend::Sqlite;
    let inbox = InboxRig::without_approvals(&backend).expect("SQLite needs no database server");
    let port = free_port();
    let node = start_custom(
        Arc::new(backend.fresh()),
        None,
        Custom {
            env: inbox.env.clone(),
            boot_inbox: true,
            ..Custom::default()
        },
        &format!(
            r#"identity {{ issuer = "https://portal.test" signing_key_env = "TEST_IDENTITY_KEY" }}
            pdp {{ listen_address = "127.0.0.1:{port}" url = "http://pdp.test:{port}" plain_http = true }}
            keys {{}}
            {}"#,
            inbox.config
        ),
    );
    wait_until(Duration::from_secs(10), || {
        send(node.port, "GET", READY_PATH, &[], "").status == 200
    });
    assert_probe(node.port, READY_PATH, 200, "ready");
    // The other methods keep the answer of the inbox.
    assert_eq!(send(node.port, "POST", READY_PATH, &[], "").status, 405);
}

#[test]
fn the_pages_forbid_the_opener_and_the_unused_browser_features() {
    let node = node("");
    for path in [HEALTH_PATH, LOGIN_PATH, "/no-such-page"] {
        let reply = send(node.port, "GET", path, &[], "");
        assert_eq!(
            reply.header("cross-origin-opener-policy"),
            Some("same-origin"),
            "{path}"
        );
        assert_eq!(
            reply.header("permissions-policy"),
            Some("camera=(), microphone=(), geolocation=(), payment=(), usb=()"),
            "{path}"
        );
        assert_eq!(reply.header("cross-origin-resource-policy"), None, "{path}");
        assert_eq!(reply.header("x-frame-options"), Some("DENY"), "{path}");
    }
}

#[test]
fn the_request_event_adds_the_duration_and_the_route_after_the_released_fields() {
    let fields = request_log_fields(
        &Method::GET,
        "/healthz".into(),
        200,
        Some("192.0.2.1".parse().unwrap()),
        Duration::from_micros(12_900),
        "health",
    );
    let names: Vec<&str> = fields.iter().map(|(name, _)| *name).collect();
    assert_eq!(
        names,
        [
            "method",
            "path",
            "status",
            "client_ip",
            "duration_ms",
            "route"
        ]
    );
    assert_eq!(fields[3].1, "192.0.2.1");
    assert_eq!(fields[4].1, "12", "whole milliseconds");
    assert_eq!(fields[5].1, "health");
    let unknown = request_log_fields(
        &Method::POST,
        "/x".into(),
        404,
        None,
        Duration::ZERO,
        "not_found",
    );
    assert_eq!(unknown[3].1, "unknown");
    assert_eq!(unknown[4].1, "0");
}

#[test]
fn the_probe_routes_have_labels_and_the_metrics_give_the_start_time() {
    let node = node("metrics { enabled = true }");
    assert_eq!(route_metric_label(&node.state, LIVE_PATH), "live");
    assert!(is_probe_route("live") && is_probe_route("health") && is_probe_route("ready"));
    assert!(!is_probe_route("metrics"));
    send(node.port, "HEAD", LIVE_PATH, &[], "");
    let body = send(node.port, "GET", "/metrics", &[], "").body;
    assert!(
        body.contains(
            "portal_http_requests_total{method=\"HEAD\",route=\"live\",status=\"200\"} 1"
        ),
        "{body}"
    );
    assert!(
        body.contains("# TYPE portal_start_time_seconds gauge\n"),
        "{body}"
    );
    assert!(body.contains("\nportal_uptime_seconds "), "{body}");
}
