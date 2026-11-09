//! Test support for the approvals at the PDP: a harness server with the inbox, its content key and its items, and the
//! seeding of approvals in any state.
//!
//! The inbox keeps its tables in a database of its own in these tests: a file in a temporary directory for SQLite,
//! and a throwaway database for PostgreSQL (`INBOX_TEST_PG`, see `inbox::testing::TestDatabase`). The portal keeps its
//! accounts, keys and counters in the store that the test gives to `start_custom`.

use std::path::PathBuf;
use std::sync::Arc;

use serde_json::Value;

use super::cluster_tests::Node;
use crate::inbox::testing::TestDatabase;
use crate::inbox::SeedApproval;
use crate::store::testing::Backend;

/// The blocks of the configuration and the variables that start the inbox with its items, its content key and the
/// block `inbox.approvals`.
pub(super) struct InboxRig {
    /// Blocks of the file: `inbox { ... }` and the storage of the inbox. The test adds `identity`, `pdp`, `keys` and
    /// the blocks `pdp_client`.
    pub config: String,
    /// The variables that the blocks read.
    pub env: Vec<(String, String)>,
    /// Keeps the throwaway database of PostgreSQL until the rig is dropped.
    _pg: Option<TestDatabase>,
    dir: PathBuf,
}

impl Drop for InboxRig {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

impl InboxRig {
    /// `consume_within` is the text of the key of the block `inbox.approvals`. `None` when PostgreSQL is the backend
    /// and `INBOX_TEST_PG` is not set: the PostgreSQL side of the test is skipped.
    pub(super) fn new(backend: &Backend, consume_within: &str) -> Option<InboxRig> {
        Self::with_approvals(backend, &format!("consume_within = \"{consume_within}\""))
    }

    /// `approvals` is the text inside the block `inbox.approvals`.
    pub(super) fn with_approvals(backend: &Backend, approvals: &str) -> Option<InboxRig> {
        let dir = std::env::temp_dir().join(format!(
            "portal-pdp-rig-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let (storage, pg) = match backend {
            Backend::Sqlite => (
                format!(
                    "storage {{ database = \"{}\" }}",
                    dir.join("portal.db").display()
                ),
                None,
            ),
            Backend::Postgres(_) => {
                let pg = TestDatabase::create_blocking()?;
                (pg.config_block(), Some(pg))
            }
        };
        let server_key = inbox_core::server_key::ServerKey::generate().unwrap();
        let content: Vec<u8> = (0..32u8)
            .map(|i| 17u8.wrapping_add(i.wrapping_mul(7)))
            .collect();
        let mut env = vec![
            (
                "TEST_INBOX_KEY".to_string(),
                server_key.encode().to_string(),
            ),
            (
                "TEST_CONTENT_KEY".to_string(),
                inbox_core::b64::encode(&content),
            ),
        ];
        if let Some(pg) = &pg {
            env.push(("TEST_PG_PASSWORD".to_string(), pg.password()));
        }
        let config = format!(
            r#"{storage}
            inbox {{
              server_key_env = "TEST_INBOX_KEY"
              devices {{
                public_address = "127.0.0.1:1"
                heartbeat_interval = "20s" session_timeout = "60s" resend_after = "15s"
                max_inflight = 32 unreachable_after = "2m" offline_warning_after = "3m"
              }}
              storage {{ delivery_ttl = "7d" alert_retention = "30d" connection_log_retention = "90d" }}
              items {{
                max_lifetime = "90d"
                max_open_per_key = 5
                max_open_per_owner = 20
                max_open_total = 1000
                retention = "180d"
                body_purge_after = "30d"
                audit_retention = "365d"
                lease {{ min = "30s" max = "7d" grace = "2m" revive_window = "7d" }}
                digest {{ mode = "off" }}
              }}
              at_rest {{
                active_key = "k1"
                key "k1" {{ secret_env = "TEST_CONTENT_KEY" }}
              }}
              approvals {{ {approvals} }}
              group "infra" {{ name = "Infra" }}
            }}"#
        );
        Some(InboxRig {
            config,
            env,
            _pg: pg,
            dir,
        })
    }
}

/// A runtime for the calls of the test thread to the engine of the items.
pub(super) fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
}

/// Waits until the inbox of the node runs, and stops its clock at `now_ms` when the value is given.
pub(super) fn app_of(node: &Node, now_ms: Option<i64>) -> Arc<crate::inbox::App> {
    let begun = std::time::Instant::now();
    loop {
        if let Some(app) = node.state.inbox.app() {
            if let Some(ms) = now_ms {
                app.clock.fix(Some(ms));
            }
            return app;
        }
        assert!(
            begun.elapsed() < std::time::Duration::from_secs(30),
            "the inbox of the harness does not run"
        );
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
}

/// An approval in the data of a case or of a test.
pub(super) struct Seed<'a> {
    pub service: &'a str,
    pub subject: &'a str,
    pub owner: &'a str,
    pub action: &'a str,
    /// The path of the resource without the service: `host:prod/database:analytics`.
    pub resource_path: &'a str,
    pub effect: &'a str,
    pub state: inbox_core::proto::ItemState,
    pub decision: Option<inbox_core::proto::Decision>,
    pub answered_at: Option<i64>,
    pub consume_before: Option<i64>,
    pub consumed_at: Option<i64>,
}

/// Writes one approval into the engine of the node and returns its request id.
pub(super) fn seed(app: &crate::inbox::App, s: &Seed<'_>) -> String {
    let items = app.items().expect("the inbox has items");
    let make = || SeedApproval {
        key_id: s
            .subject
            .strip_prefix("key:")
            .unwrap_or(s.subject)
            .to_string(),
        owner: s.owner.to_string(),
        service: s.service.to_string(),
        subject: s.subject.to_string(),
        action: s.action.to_string(),
        resource_path: s.resource_path.to_string(),
        effect: s.effect.to_string(),
        state: s.state,
        decision: s.decision,
        answered_at: s.answered_at,
        consume_before: s.consume_before,
        consumed_at: s.consumed_at,
        lease_s: 900,
    };
    // A loaded machine can make the shared PostgreSQL answer late: the engine then says `Degraded` at once, and it can stay
    // so for several seconds (another build of the machine fills the disk and the CPU). The seed tries again for about
    // twenty seconds before it fails the test.
    for attempt in 0..21 {
        match runtime().block_on(items.seed_approval(make())) {
            Ok(id) => return id,
            Err(crate::inbox::testing::ItemError::Degraded) if attempt < 20 => {
                std::thread::sleep(std::time::Duration::from_secs(1));
            }
            Err(e) => panic!("seed: {e:?}"),
        }
    }
    unreachable!("the loop returns or panics")
}

/// The approvals of the data of a case (`approvals.json`), written into the engine. The clock of the data is `now`
/// seconds; the deny time of a refusal is `deny_until` seconds, so the refusal was answered `deny_cooldown_s` before
/// it.
pub(super) fn seed_directory(app: &crate::inbox::App, directory: &Value, deny_cooldown_s: i64) {
    use inbox_core::proto::{Decision, ItemState};
    let subjects = directory["subjects"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    for approval in directory["approvals"]
        .as_array()
        .cloned()
        .unwrap_or_default()
    {
        let subject = approval["subject"].as_str().unwrap();
        let owner = subjects
            .iter()
            .find(|s| s["id"] == subject)
            .and_then(|s| s["owner"].as_str())
            .unwrap_or("alice");
        let resource = approval["resource"].as_str().unwrap();
        let (service, path) = match resource.strip_prefix("service:") {
            Some(rest) => match rest.split_once('/') {
                Some((service, path)) => (service, path),
                None => (rest, ""),
            },
            None => panic!("the resource {resource:?} of an approval has no service"),
        };
        let status = match approval["status"].as_str().unwrap() {
            "pending" => ItemState::Pending,
            "replied" => ItemState::Replied,
            "answered" => ItemState::Answered,
            "abandoned" => ItemState::Abandoned,
            "expired" => ItemState::Expired,
            "cancelled" => ItemState::Cancelled,
            "superseded" => ItemState::Superseded,
            other => panic!("the status {other:?} of an approval is not known"),
        };
        let decision = approval["decision"].as_str().map(|d| match d {
            "approve" => Decision::Approve,
            "decline" => Decision::Deny,
            other => panic!("the decision {other:?} is not known"),
        });
        let seconds = |name: &str| approval[name].as_i64().map(|s| s * 1000);
        let answered_at = seconds("deny_until").map(|until| until - deny_cooldown_s * 1000);
        seed(
            app,
            &Seed {
                service,
                subject,
                owner,
                action: approval["action"].as_str().unwrap(),
                resource_path: path,
                effect: approval["effect"].as_str().unwrap(),
                state: status,
                decision,
                answered_at,
                consume_before: seconds("consume_before"),
                consumed_at: None,
            },
        );
    }
}
