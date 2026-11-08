//! End-to-end tests of the items of the inbox (approvals and questions). The helpers are in
//! `tests/common`. Each lot that adds a test of items adds it here.

mod common;

use std::time::{Duration, Instant};

use common::{portal, portal_ok, Server};

const ITEMS_AND_AT_REST: &str = r#"
  items {
    max_lifetime = "90d"
    max_open_per_key = 5
    max_open_per_owner = 20
    max_open_total = 1000
    retention = "180d"
    body_purge_after = "30d"
    audit_retention = "365d"
    lease { min = "30s" max = "7d" grace = "2m" revive_window = "7d" }
    digest { mode = "off" }
  }
  at_rest {
    active_key = "k2"
    key "k1" { secret_file = "KEY1" }
    key "k2" { secret_file = "KEY2" }
  }
"#;

/// Waits until the process of `s` ends. The result is its exit status.
fn exit_status(s: &mut Server) -> std::process::ExitStatus {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        if let Some(status) = s.child.as_mut().unwrap().try_wait().unwrap() {
            s.child = None;
            return status;
        }
        assert!(
            Instant::now() < deadline,
            "the server did not stop: {}",
            s.log()
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// The binary reads the content keys at start. The database keeps the check value of each key: the same keys
/// start again, another key under the same id stops the start before the server serves, and a new id starts.
#[test]
fn wrong_key_same_id_stops_the_start_and_the_log_names_the_keys() {
    let mut s = Server::setup();
    let dir = s.dir.clone();
    let key = move |name: &str| dir.join(name).to_string_lossy().to_string();
    for name in ["content_k1", "content_k2", "content_k2_other", "content_k3"] {
        portal_ok(&["inbox", "content-key", "--out", &key(name)]);
    }
    // The command never overwrites.
    assert!(
        !portal(&["inbox", "content-key", "--out", &key("content_k1")], None)
            .status
            .success()
    );
    s.add_to_inbox(
        &ITEMS_AND_AT_REST
            .replace("KEY1", &key("content_k1"))
            .replace("KEY2", &key("content_k2")),
    );

    // `check-config` shows the id, the use and the source of each key, and never a secret.
    let shown = portal_ok(&["check-config", "--config", &s.config()]);
    let shown = String::from_utf8_lossy(&shown.stdout).to_string();
    assert!(
        shown.contains("inbox.at_rest.key.k2.use = \"active\""),
        "{shown}"
    );
    assert!(
        shown.contains("inbox.at_rest.key.k1.use = \"read-only\""),
        "{shown}"
    );
    assert!(
        shown.contains(&format!("# file:{}", key("content_k2"))),
        "{shown}"
    );
    for name in ["content_k1", "content_k2"] {
        let secret = std::fs::read_to_string(s.dir.join(name)).unwrap();
        assert!(!shown.contains(secret.trim()), "the secret is hidden");
    }

    s.start();
    let log = s.log();
    assert!(
        log.contains("\"event\":\"content_key_loaded\"") && log.contains("\"kid\":\"k2\""),
        "{log}"
    );
    assert!(!log.contains(
        &std::fs::read_to_string(s.dir.join("content_k2"))
            .unwrap()
            .trim()
            .to_string()
    ));
    s.stop();
    s.start();
    s.stop();

    // Another key under the id k2: the start stops, and the message names the id and not the key.
    let original = std::fs::read(s.dir.join("content_k2")).unwrap();
    std::fs::copy(s.dir.join("content_k2_other"), s.dir.join("content_k2")).unwrap();
    let other = std::fs::read_to_string(s.dir.join("content_k2")).unwrap();
    s.spawn();
    let status = exit_status(&mut s);
    assert!(!status.success(), "the start must stop");
    let log = s.log();
    assert!(
        log.contains("inbox.at_rest.key \"k2\"") && log.contains("another key under the same id"),
        "{log}"
    );
    assert!(!log.contains(other.trim()), "the message never shows a key");

    // The right key starts again. A key under a new id starts too, and the old keys stay.
    std::fs::write(s.dir.join("content_k2"), original).unwrap();
    s.start();
    s.stop();
    let text = std::fs::read_to_string(s.config())
        .unwrap()
        .replace("active_key = \"k2\"", "active_key = \"k3\"")
        .replace(
            "    key \"k2\" {",
            &format!(
                "    key \"k3\" {{ secret_file = \"{}\" }}\n    key \"k2\" {{",
                key("content_k3")
            ),
        );
    std::fs::write(s.config(), text).unwrap();
    s.start();
    assert!(s.log().contains("\"kid\":\"k3\""), "{}", s.log());
    s.stop();
}
