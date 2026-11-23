//! Tests of the engine of the items on SQLite and on PostgreSQL, with a clock that the test moves by hand.
//!
//! Each test runs on SQLite and, when `INBOX_TEST_PG` names a server, on PostgreSQL too.

use std::sync::Arc;
use std::time::Duration;

use inbox_core::effect::effect_hash;
use inbox_core::proto::{Action, AnswerStatus, Interaction, ItemState, Kind, Urgency, Via};
use serde_json::json;
use zeroize::Zeroizing;

use super::content_box::{ContentBox, KEY_BYTES};
use super::db::{Closed, Db, DbError};
use super::items::{tests::rules, Caller, Clock, ManualClock, Rules, Wait};
use super::items_ops::{
    AnswerVia, CancelBy, Cancelled, Consumed, ListFilter, NewApproval, NewItem, Renewed,
    SupersedeOut, TurnOut,
};
use super::items_store::{
    start_checks, Answer, Answered, Created, DigestMemo, ItemError, Items, Limit, Served,
};
use super::pg::tests::TestDatabase;
use super::store::Store;

pub(super) const T0: i64 = 1_800_000_000_000;
pub(super) const CANARY: &str = "canary-8f3a91c7-do-not-log";
pub(super) const DAY: Duration = Duration::from_secs(86_400);

pub(super) struct Env {
    pub(super) name: &'static str,
    pub(super) store: Store,
    pub(super) clock: Arc<ManualClock>,
    pub(super) content: ContentBox,
    pub(super) rules: Rules,
    pg: Option<TestDatabase>,
    path: Option<std::path::PathBuf>,
    _closed: Option<Closed>,
}

fn key(seed: u8) -> Zeroizing<[u8; KEY_BYTES]> {
    let mut k = Zeroizing::new([0u8; KEY_BYTES]);
    for (i, b) in k.iter_mut().enumerate() {
        *b = seed.wrapping_add((i as u8).wrapping_mul(7));
    }
    k
}

pub(super) fn content(ids: &[(&str, u8)], active: &str) -> ContentBox {
    let keys: Vec<_> = ids
        .iter()
        .map(|(id, s)| (id.to_string(), key(*s)))
        .collect();
    ContentBox::new(&keys, active).unwrap()
}

/// The databases of the test: SQLite always, PostgreSQL when the server is there.
pub(super) async fn envs() -> Vec<Env> {
    let mut out = Vec::new();
    let dir = std::env::temp_dir().join(format!(
        "portal-items-{}-{}",
        std::process::id(),
        super::util::random_id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("inbox.db");
    let (db, closed) = Db::open(&path).unwrap();
    out.push(Env {
        name: "sqlite",
        store: Store::Sqlite(db),
        clock: Arc::new(ManualClock::new(T0)),
        content: content(&[("k1", 1)], "k1"),
        rules: rules(),
        pg: None,
        path: Some(path),
        _closed: Some(closed),
    });
    if let Some(pg) = TestDatabase::create().await {
        let store = Store::Pg(Arc::new(pg.store().await));
        out.push(Env {
            name: "postgres",
            store,
            clock: Arc::new(ManualClock::new(T0)),
            content: content(&[("k1", 1)], "k1"),
            rules: rules(),
            pg: Some(pg),
            path: None,
            _closed: None,
        });
    }
    out
}

impl Env {
    pub(super) fn items(&self) -> Items<'_> {
        Items {
            store: &self.store,
            content: &self.content,
            rules: self.rules,
            clock: &*self.clock,
            hub: None,
            replica: None,
            metrics: None,
            changed: None,
            admin_body_read: false,
        }
    }

    pub(super) fn advance(&self, by: Duration) {
        self.clock.advance(by);
    }

    pub(super) async fn sql(&self, sql: &str) {
        match (&self.store, &self.pg) {
            (Store::Sqlite(db), _) => {
                let sql = sql.to_string();
                db.call(move |c| Ok(c.execute_batch(&sql)?)).await.unwrap();
            }
            (_, Some(pg)) => pg.sql(sql).await,
            _ => unreachable!(),
        }
    }

    pub(super) async fn count(&self, sql: &str) -> i64 {
        match (&self.store, &self.pg) {
            (Store::Sqlite(db), _) => {
                let sql = sql.to_string();
                db.call(move |c| Ok(c.query_row(&sql, [], |r| r.get(0))?))
                    .await
                    .unwrap()
            }
            (_, Some(pg)) => pg
                .admin(&pg.name)
                .await
                .query_one(sql, &[])
                .await
                .unwrap()
                .get(0),
            _ => unreachable!(),
        }
    }

    /// A phone of `login`, not revoked.
    pub(super) async fn device(&self, id: &str, login: &str) {
        let hex: String = id.bytes().map(|b| format!("{b:02x}")).collect();
        let sql = match self.store {
            Store::Sqlite(_) => format!(
                "INSERT INTO devices (id, name, user_name, public_key, enrolled_at, enrolled_ip) VALUES ('{id}', 'phone', '{login}', x'{hex}', 1, 'ip')"
            ),
            Store::Pg(_) => format!(
                "INSERT INTO devices (id, name, user_name, public_key, enrolled_at, enrolled_ip) VALUES ('{id}', 'phone', '{login}', decode('{hex}', 'hex'), 1, 'ip')"
            ),
        };
        self.sql(&sql).await;
    }

    /// A pass of the sweeper, as if it had run every 2 seconds up to now: the heartbeat is one second old.
    pub(super) async fn sweep(&self) -> super::items_ops::SweepReport {
        let beat = self.clock.now_ms() - 1000;
        self.sql(&format!(
            "INSERT INTO items_heartbeat (id, swept_at) VALUES (1, {beat}) ON CONFLICT (id) DO UPDATE SET swept_at = {beat}"
        ))
        .await;
        self.items().sweep().await.unwrap()
    }

    pub(super) async fn column(&self, id: &str, column: &str) -> i64 {
        self.count(&format!(
            "SELECT {column} FROM items WHERE request_id = '{id}'"
        ))
        .await
    }

    /// The messages of the kind of the item for one request.
    pub(super) async fn notices(&self, id: &str) -> i64 {
        self.count(&format!(
            "SELECT COUNT(*) FROM alerts WHERE request_id = '{id}' AND kind = 'approval'"
        ))
        .await
    }

    pub(super) async fn state_of(&self, id: &str) -> ItemState {
        self.items()
            .get(id, &Caller::System)
            .await
            .unwrap()
            .unwrap()
            .item
            .state
    }
}

/// An approval of the key `key1` for `alice`. The effect is the real hash of the operation.
pub(super) fn approval(n: u32) -> (NewItem, Vec<u8>) {
    let op = json!({"sql": format!("DELETE FROM staging.events WHERE day = {n}")});
    let effect = effect_hash("db", "run_write_query", "/db/analytics", &op).unwrap();
    let body = serde_json::to_vec(&json!({
        "op": op, "view": {"title": "Write", "fields": [], "body": "/sql"}, "explanation": "clean up", "hint": null
    }))
    .unwrap();
    let item = NewItem {
        kind: Kind::Approval,
        requester_key_id: "key1".into(),
        owner: "alice".into(),
        via: Via::Mcp,
        interaction: Interaction::Conversational,
        urgency: Urgency::Normal,
        input_type: None,
        wait: Wait::Lease { lease_s: 900 },
        allow_custom: true,
        allow_ask_back: true,
        idempotency_key: format!("idem-{n}"),
        previous_id: None,
        agent_id: None,
        body_size: 0,
        approval: Some(NewApproval {
            service: "db".into(),
            subject: "key:key1".into(),
            action: "run_write_query".into(),
            resource_path: "/db/analytics".into(),
            effect,
            key_hash: format!("hash-{n}"),
            evaluation_id: None,
        }),
        require_device: false,
        guard: None,
    };
    (item, body)
}

pub(super) fn question(n: u32) -> (NewItem, Vec<u8>) {
    let (mut item, _) = approval(n);
    item.kind = Kind::Question;
    item.approval = None;
    item.input_type = Some(inbox_core::proto::InputType::Confirm);
    (item, format!("{{\"title\":\"question {n}\"}}").into_bytes())
}

pub(super) async fn created(items: &Items<'_>, n: u32) -> String {
    let (item, body) = approval(n);
    match items.create(item, &body).await.unwrap() {
        Created::New(l) => l.item.request_id.clone(),
        other => panic!("not created: {other:?}"),
    }
}

pub(super) fn answer(device: &str, effect: &str, action: Action) -> Answer {
    Answer {
        login: "alice".into(),
        via: AnswerVia::Device(device.into()),
        action,
        shown_effect: Some(effect.into()),
        client_msg_id: Some(format!("msg-{device}")),
        payload: None,
        consume_within: Duration::from_secs(300),
    }
}

pub(super) async fn effect_of(items: &Items<'_>, id: &str) -> String {
    items
        .get(id, &Caller::System)
        .await
        .unwrap()
        .unwrap()
        .approval
        .unwrap()
        .effect
}

// ---------------------------------------------------------------- creation

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn create_idempotent_same_request_same_id() {
    for env in envs().await {
        let items = env.items();
        let (item, body) = approval(1);
        let Created::New(first) = items.create(item.clone(), &body).await.unwrap() else {
            panic!()
        };
        // The same request again: the same id, no second row, no second delivery.
        let Created::Repeat(again) = items.create(item.clone(), &body).await.unwrap() else {
            panic!()
        };
        assert_eq!(again.item.request_id, first.item.request_id, "{}", env.name);
        // The same effect under another idempotency key is the same open approval.
        let mut other_key = item.clone();
        other_key.idempotency_key = "other".into();
        assert!(matches!(
            items.create(other_key, &body).await.unwrap(),
            Created::Repeat(_)
        ));
        // The same key with another effect is a conflict.
        let (mut conflict, conflict_body) = approval(2);
        conflict.idempotency_key = item.idempotency_key.clone();
        assert!(matches!(
            items.create(conflict, &conflict_body).await.unwrap(),
            Created::Conflict
        ));
        // A question compares its body.
        let (q, qbody) = question(3);
        assert!(matches!(
            items.create(q.clone(), &qbody).await.unwrap(),
            Created::New(_)
        ));
        assert!(matches!(
            items.create(q.clone(), &qbody).await.unwrap(),
            Created::Repeat(_)
        ));
        assert!(matches!(
            items.create(q, b"{\"title\":\"other\"}").await.unwrap(),
            Created::Conflict
        ));
        assert_eq!(env.count("SELECT COUNT(*) FROM items").await, 2);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn caps_per_key_owner_total() {
    for mut env in envs().await {
        env.rules.max_open_per_key = 2;
        env.rules.max_open_per_owner = 3;
        env.rules.max_open_total = 4;
        let items = env.items();
        let make = |n: u32, key: &str, owner: &str| {
            let (mut item, body) = approval(n);
            item.requester_key_id = key.into();
            item.owner = owner.into();
            (item, body)
        };
        for (n, key, owner, expect) in [
            (1, "k1", "alice", None),
            (2, "k1", "alice", None),
            (3, "k1", "alice", Some(Limit::PerKey)),
            (4, "k2", "alice", None),
            (5, "k3", "alice", Some(Limit::PerOwner)),
            (6, "k4", "bob", None),
            (7, "k5", "carol", Some(Limit::Total)),
        ] {
            let (item, body) = make(n, key, owner);
            match (items.create(item, &body).await.unwrap(), expect) {
                (Created::New(_), None) => {}
                (Created::TooMany(l), Some(want)) => assert_eq!(l, want, "{} {n}", env.name),
                (other, want) => panic!("{} {n}: {other:?} for {want:?}", env.name),
            }
        }
        // A closed item frees its place.
        let id = env
            .count("SELECT COUNT(*) FROM items WHERE requester_key_id = 'k1'")
            .await;
        assert_eq!(id, 2);
        let listed = items
            .list(ListFilter {
                key_id: Some("k1".into()),
                open: Some(true),
                limit: 10,
                agent_id: None,
                ..Default::default()
            })
            .await
            .unwrap();
        items
            .cancel(&listed[0].item.request_id, CancelBy::Agent("k1".into()))
            .await
            .unwrap();
        let (item, body) = make(8, "k1", "alice");
        assert!(matches!(
            items.create(item, &body).await.unwrap(),
            Created::New(_)
        ));
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_item_reaches_each_phone_of_its_owner_that_is_not_revoked() {
    for env in envs().await {
        let items = env.items();
        for (id, login) in [
            ("d1", "alice"),
            ("d2", "alice"),
            ("d3", "alice"),
            ("d4", "bob"),
        ] {
            env.device(id, login).await;
        }
        env.sql("UPDATE devices SET revoked_at = 5 WHERE id = 'd3'")
            .await;
        let id = created(&items, 1).await;
        let deliveries = "SELECT COUNT(*) FROM deliveries d JOIN alerts a ON a.id = d.alert_id WHERE a.request_id = ";
        assert_eq!(
            env.count(&format!("{deliveries}'{id}' AND a.kind = 'approval'"))
                .await,
            2,
            "{}",
            env.name
        );
        // The title is a fixed text. The body is not in the row.
        assert_eq!(
            env.count(&format!("SELECT COUNT(*) FROM alerts WHERE request_id = '{id}' AND title LIKE 'Approval ____'")).await,
            1
        );
        // The answer from d1 sends `settled` to the other phone only.
        let effect = effect_of(&items, &id).await;
        assert!(matches!(
            items
                .answer(&id, answer("d1", &effect, Action::Accept))
                .await
                .unwrap(),
            Answered::Accepted { .. }
        ));
        assert_eq!(
            env.count(&format!("{deliveries}'{id}' AND a.kind = 'settled'"))
                .await,
            1
        );
        assert_eq!(
            env.count(&format!(
                "{deliveries}'{id}' AND a.kind = 'settled' AND d.device_id = 'd2'"
            ))
            .await,
            1
        );
        // An owner without a phone: the creation can refuse, and then it writes nothing.
        let (mut lonely, body) = approval(2);
        lonely.owner = "erin".into();
        lonely.require_device = true;
        assert!(matches!(
            items.create(lonely, &body).await.unwrap(),
            Created::NoDevice
        ));
        assert_eq!(
            env.count("SELECT COUNT(*) FROM items WHERE requester_owner = 'erin'")
                .await,
            0
        );
    }
}

// ---------------------------------------------------------------- lease and clock

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn renewal_writes_at_most_once_per_lease_s_in_both_databases() {
    for env in envs().await {
        let items = env.items();
        let id = created(&items, 1).await;
        let first = env.column(&id, "lease_until").await;
        assert_eq!(first, T0 + 3 * 900_000);
        env.advance(Duration::from_secs(899));
        assert!(matches!(
            items.renew(&id, "key1").await.unwrap(),
            Renewed::Open { written: false, .. }
        ));
        assert_eq!(
            env.column(&id, "lease_until").await,
            first,
            "no write before lease_s"
        );
        env.advance(Duration::from_secs(1));
        assert!(matches!(
            items.renew(&id, "key1").await.unwrap(),
            Renewed::Open { written: true, .. }
        ));
        assert_eq!(
            env.column(&id, "lease_until").await,
            T0 + 900_000 + 3 * 900_000
        );
        // Another key reaches nothing: the answer is the answer for an unknown id.
        assert!(matches!(
            items.renew(&id, "key2").await.unwrap(),
            Renewed::NotFound
        ));
        assert!(items
            .get(&id, &Caller::Agent("key2".into()))
            .await
            .unwrap()
            .is_none());
        assert!(items
            .get(&id, &Caller::Owner("bob".into()))
            .await
            .unwrap()
            .is_none());
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_sweeper_abandons_after_lease_plus_grace_and_not_before() {
    for env in envs().await {
        let items = env.items();
        let id = created(&items, 1).await;
        let end = Duration::from_secs(3 * 900 + 120);
        env.advance(end - Duration::from_secs(1));
        assert_eq!(env.sweep().await.abandoned, 0);
        assert_eq!(env.state_of(&id).await, ItemState::Pending);
        env.advance(Duration::from_secs(1));
        let report = env.sweep().await;
        assert_eq!(report.abandoned, 1, "{}", env.name);
        assert_eq!(env.state_of(&id).await, ItemState::Abandoned);
        // A second pass does nothing more.
        assert_eq!(env.sweep().await, Default::default());
        assert_eq!(
            env.count("SELECT COUNT(*) FROM item_events WHERE event = 'abandoned'")
                .await,
            1
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn answer_after_abandon_returns_abandoned() {
    for env in envs().await {
        let items = env.items();
        env.device("d1", "alice").await;
        let id = created(&items, 1).await;
        let effect = effect_of(&items, &id).await;
        // No sweeper ran: the answer itself applies the clock.
        env.advance(Duration::from_secs(3 * 900 + 120));
        match items
            .answer(&id, answer("d1", &effect, Action::Accept))
            .await
            .unwrap()
        {
            Answered::Refused { status, loaded } => {
                assert_eq!(status, AnswerStatus::Abandoned, "{}", env.name);
                assert_eq!(loaded.item.state, ItemState::Abandoned);
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(env.state_of(&id).await, ItemState::Abandoned);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn outage_extends_open_leases_by_the_gap_no_false_abandon() {
    for env in envs().await {
        let items = env.items();
        let id = created(&items, 1).await;
        items.sweep().await.unwrap();
        let before = env.column(&id, "lease_until").await;
        // The portal was away for 3 hours: the lease of the item ended, and nobody could renew it.
        env.advance(Duration::from_secs(3 * 3600));
        let report = items.sweep().await.unwrap();
        assert_eq!((report.extended, report.abandoned), (1, 0), "{}", env.name);
        assert_eq!(env.state_of(&id).await, ItemState::Pending);
        assert_eq!(env.column(&id, "lease_until").await, before + 3 * 3_600_000);
        // A pass 2 seconds later is no gap.
        env.advance(Duration::from_secs(2));
        assert_eq!(items.sweep().await.unwrap().extended, 0);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn ninety_days_are_simulated_without_a_pause_and_supersede_does_not_extend() {
    for env in envs().await {
        let items = env.items();
        let (mut item, body) = approval(1);
        item.wait = Wait::Lease {
            lease_s: 7 * 86_400,
        };
        let Created::New(first) = items.create(item, &body).await.unwrap() else {
            panic!()
        };
        let id = first.item.request_id.clone();
        let mut newest = id.clone();
        for step in 1..=15 {
            env.advance(6 * DAY);
            env.sweep().await;
            if step < 15 {
                assert!(matches!(
                    items.renew(&newest, "key1").await.unwrap(),
                    Renewed::Open { .. }
                ));
            }
            if step == 14 {
                // Day 84: a successor keeps the clock of the chain.
                let (mut next, next_body) = approval(2);
                next.wait = Wait::Lease {
                    lease_s: 7 * 86_400,
                };
                let SupersedeOut::Created { old, new } = items
                    .supersede(&newest, "key1", next, &next_body)
                    .await
                    .unwrap()
                else {
                    panic!()
                };
                assert_eq!(old.item.state, ItemState::Superseded);
                assert_eq!(new.item.chain_created_at, T0);
                assert_eq!(new.item.chain_id, id);
                assert_eq!(
                    new.item.lease_until,
                    Some(T0 + 90 * 86_400_000),
                    "capped by the lifetime"
                );
                newest = new.item.request_id.clone();
            }
        }
        let last = items.get(&newest, &Caller::System).await.unwrap().unwrap();
        assert_eq!(last.item.state, ItemState::Expired, "{}", env.name);
        assert_eq!(last.item.settle_reason.as_deref(), Some("max_lifetime"));
        assert!(matches!(
            items.renew(&newest, "key1").await.unwrap(),
            Renewed::Final(_)
        ));
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn until_expires_and_never_abandons() {
    for env in envs().await {
        let items = env.items();
        let (mut item, body) = approval(1);
        item.wait = Wait::Until {
            at_ms: T0 + 5 * 3_600_000,
        };
        let Created::New(l) = items.create(item, &body).await.unwrap() else {
            panic!()
        };
        env.advance(Duration::from_secs(5 * 3600 - 1));
        env.sweep().await;
        assert_eq!(env.state_of(&l.item.request_id).await, ItemState::Pending);
        env.advance(Duration::from_secs(1));
        env.sweep().await;
        let l = items
            .get(&l.item.request_id, &Caller::System)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            (l.item.state, l.item.settle_reason.as_deref()),
            (ItemState::Expired, Some("until_reached"))
        );
        // A date that is past or beyond the lifetime is refused, and no row is written.
        let (mut bad, body) = approval(2);
        bad.wait = Wait::Until {
            at_ms: T0 + 91 * 86_400_000,
        };
        assert!(matches!(
            items.create(bad, &body).await.unwrap(),
            Created::BadWait(_)
        ));
    }
}

// ---------------------------------------------------------------- the answer

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cas_two_racers_one_answer() {
    for env in envs().await {
        let items = env.items();
        env.device("d1", "alice").await;
        env.device("d2", "alice").await;
        let id = created(&items, 1).await;
        let effect = effect_of(&items, &id).await;
        let (a, b) = tokio::join!(
            items.answer(&id, answer("d1", &effect, Action::Accept)),
            items.answer(&id, answer("d2", &effect, Action::Decline)),
        );
        let results = [a.unwrap(), b.unwrap()];
        let accepted = results
            .iter()
            .filter(|r| matches!(r, Answered::Accepted { .. }))
            .count();
        let late = results
            .iter()
            .filter(|r| {
                matches!(
                    r,
                    Answered::Refused {
                        status: AnswerStatus::AlreadyAnswered,
                        ..
                    }
                )
            })
            .count();
        assert_eq!((accepted, late), (1, 1), "{}: {results:?}", env.name);
        assert_eq!(
            env.count("SELECT COUNT(*) FROM turns WHERE kind IN ('answer', 'decline')")
                .await,
            1
        );
        assert_eq!(
            env.count("SELECT COUNT(*) FROM item_events WHERE event = 'answered'")
                .await,
            1
        );
        // The decision follows the winner.
        let l = items.get(&id, &Caller::System).await.unwrap().unwrap();
        let approve = l.item.answer_action == Some(Action::Accept);
        assert_eq!(
            l.approval.unwrap().decision == Some(inbox_core::proto::Decision::Approve),
            approve
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn answer_effect_mismatch_state_unchanged() {
    for env in envs().await {
        let items = env.items();
        env.device("d1", "alice").await;
        let id = created(&items, 1).await;
        let wrong = "sha256:".to_string() + &"0".repeat(64);
        match items
            .answer(&id, answer("d1", &wrong, Action::Accept))
            .await
            .unwrap()
        {
            Answered::Refused { status, loaded } => {
                assert_eq!(status, AnswerStatus::EffectMismatch);
                assert_eq!(loaded.item.state, ItemState::Pending);
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(env.state_of(&id).await, ItemState::Pending);
        assert_eq!(env.count("SELECT COUNT(*) FROM turns").await, 0);
        // A revoked device and an unknown owner change nothing either.
        env.sql("UPDATE devices SET revoked_at = 9 WHERE id = 'd1'")
            .await;
        let effect = effect_of(&items, &id).await;
        assert!(matches!(
            items
                .answer(&id, answer("d1", &effect, Action::Accept))
                .await
                .unwrap(),
            Answered::Refused {
                status: AnswerStatus::DeviceRevoked,
                ..
            }
        ));
        let mut stranger = answer("d1", &effect, Action::Accept);
        stranger.login = "bob".into();
        assert!(matches!(
            items.answer(&id, stranger).await.unwrap(),
            Answered::NotFound
        ));
        // The text goes with a decline only, and only when the agent allowed it.
        env.device("d2", "alice").await;
        let mut with_text = answer("d2", &effect, Action::Accept);
        with_text.payload = Some(Zeroizing::new(b"x".to_vec()));
        assert!(matches!(
            items.answer(&id, with_text).await.unwrap(),
            Answered::Refused {
                status: AnswerStatus::CustomNotAllowed,
                ..
            }
        ));
        assert_eq!(env.state_of(&id).await, ItemState::Pending);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn supersede_race_one_winner_old_approval_cancelled() {
    for env in envs().await {
        let items = env.items();
        let old = created(&items, 1).await;
        let (n2, b2) = approval(2);
        let (n3, b3) = approval(3);
        let (a, b) = tokio::join!(
            items.supersede(&old, "key1", n2, &b2),
            items.supersede(&old, "key1", n3, &b3)
        );
        let results = [a.unwrap(), b.unwrap()];
        let won = results
            .iter()
            .filter(|r| matches!(r, SupersedeOut::Created { .. }))
            .count();
        let lost = results
            .iter()
            .filter(|r| matches!(r, SupersedeOut::Closed(_)))
            .count();
        assert_eq!((won, lost), (1, 1), "{}: {results:?}", env.name);
        assert_eq!(env.state_of(&old).await, ItemState::Superseded);
        // The old approval cannot be consumed any more, and the new item asks its own approval.
        assert!(matches!(
            items.consume("hash-1").await.unwrap(),
            Consumed::Gone(_)
        ));
        assert_eq!(
            env.count("SELECT COUNT(*) FROM items WHERE chain_id = request_id")
                .await,
            1
        );
        assert_eq!(
            env.count("SELECT COUNT(*) FROM items WHERE chain_id <> request_id")
                .await,
            1
        );
        assert_eq!(
            env.count(&format!("SELECT COUNT(*) FROM items WHERE superseded_by IS NOT NULL AND request_id = '{old}'")).await,
            1
        );
        // The chain holds `max_chain` items at most.
    }
    for mut env in envs().await {
        env.rules.max_chain = 2;
        let items = env.items();
        let first = created(&items, 1).await;
        let (n, b) = approval(2);
        let SupersedeOut::Created { new, .. } =
            items.supersede(&first, "key1", n, &b).await.unwrap()
        else {
            panic!()
        };
        let (n, b) = approval(3);
        assert!(matches!(
            items
                .supersede(&new.item.request_id, "key1", n, &b)
                .await
                .unwrap(),
            SupersedeOut::ChainFull
        ));
    }
}

// ---------------------------------------------------------------- revive

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn revive_in_window_max_three_notifications_once() {
    for env in envs().await {
        let items = env.items();
        env.device("d1", "alice").await;
        let id = created(&items, 1).await;
        assert_eq!(env.notices(&id).await, 1);
        for round in 1..=3 {
            env.advance(Duration::from_secs(3 * 900 + 120));
            env.sweep().await;
            assert_eq!(env.state_of(&id).await, ItemState::Abandoned);
            assert!(
                matches!(items.renew(&id, "key1").await.unwrap(), Renewed::Revived(_)),
                "{} {round}",
                env.name
            );
            // Calls that come after the revival send nothing.
            env.advance(Duration::from_secs(1000));
            assert!(matches!(
                items.renew(&id, "key1").await.unwrap(),
                Renewed::Open { .. }
            ));
            assert_eq!(
                env.notices(&id).await,
                1 + round,
                "one message for each revival"
            );
        }
        env.advance(Duration::from_secs(3 * 900 + 120 + 1000));
        env.sweep().await;
        let Renewed::Final(l) = items.renew(&id, "key1").await.unwrap() else {
            panic!()
        };
        assert_eq!((l.item.state, l.item.revivals), (ItemState::Abandoned, 3));
        assert_eq!(l.item.revivable_until(&env.rules), None, "{}", env.name);
        assert_eq!(env.notices(&id).await, 4);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn revive_after_window_refused_new_request_needs_previous_id() {
    for env in envs().await {
        let items = env.items();
        let id = created(&items, 1).await;
        env.advance(Duration::from_secs(3 * 900 + 120));
        env.sweep().await;
        env.advance(7 * DAY);
        let Renewed::Final(l) = items.renew(&id, "key1").await.unwrap() else {
            panic!()
        };
        assert_eq!(l.item.state, ItemState::Abandoned, "{}", env.name);
        // A new request links to the old one with `previous_id`.
        let (mut again, body) = approval(1);
        again.idempotency_key = "idem-again".into();
        again.previous_id = Some(id.clone());
        let Created::New(l) = items.create(again, &body).await.unwrap() else {
            panic!()
        };
        assert_eq!(l.item.previous_id.as_deref(), Some(id.as_str()));
        assert_ne!(l.item.request_id, id);
    }
}

// ---------------------------------------------------------------- turns

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_thread_follows_the_flags_the_limits_and_the_pace_of_the_agent() {
    for mut env in envs().await {
        env.rules.max_turns = 4;
        let items = env.items();
        env.device("d1", "alice").await;
        let id = created(&items, 1).await;
        let reply = |cmid: &str| items.reply(&id, "alice", Some("d1".into()), cmid.into(), b"why?");
        let TurnOut::Stored { loaded, seq } = reply("m1").await.unwrap() else {
            panic!()
        };
        assert_eq!(
            (seq, loaded.item.state),
            (1, ItemState::Replied),
            "{}",
            env.name
        );
        // A repeat of the same message is not stored twice.
        assert!(matches!(
            reply("m1").await.unwrap(),
            TurnOut::Duplicate { seq: 1 }
        ));
        let TurnOut::Stored { loaded, seq } = items.say(&id, "key1", b"because").await.unwrap()
        else {
            panic!()
        };
        assert_eq!((seq, loaded.item.state), (2, ItemState::Pending));
        assert!(matches!(
            items.say(&id, "key1", b"again").await.unwrap(),
            TurnOut::TooFast
        ));
        env.advance(Duration::from_secs(3));
        assert!(matches!(
            items.say(&id, "key1", b"later").await.unwrap(),
            TurnOut::Stored { seq: 3, .. }
        ));
        assert!(matches!(
            reply("m2").await.unwrap(),
            TurnOut::Stored { seq: 4, .. }
        ));
        assert!(matches!(reply("m3").await.unwrap(), TurnOut::TooManyTurns));
        // Both texts are sealed in the database and come back through the unseal of the server.
        let turns = items.serve_turns(&id).await.unwrap();
        assert_eq!(turns.len(), 4);
        assert_eq!(
            turns[0].text.as_deref().map(|t| t.as_slice()),
            Some(&b"why?"[..])
        );
        assert_eq!(
            turns[1].text.as_deref().map(|t| t.as_slice()),
            Some(&b"because"[..])
        );
        // An item that does not allow the exchange, and a one-shot item, refuse.
        let (mut quiet, body) = approval(2);
        quiet.allow_ask_back = false;
        let Created::New(q) = items.create(quiet, &body).await.unwrap() else {
            panic!()
        };
        assert!(matches!(
            items.say(&q.item.request_id, "key1", b"x").await.unwrap(),
            TurnOut::AskBackNotAllowed
        ));
        let (mut shot, body) = approval(3);
        shot.interaction = Interaction::OneShot;
        let Created::New(s) = items.create(shot, &body).await.unwrap() else {
            panic!()
        };
        assert!(matches!(
            items
                .reply(&s.item.request_id, "alice", None, "w1".into(), b"x")
                .await
                .unwrap(),
            TurnOut::AskBackNotAllowed
        ));
    }
}

// ---------------------------------------------------------------- the approval of the PDP

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_approval_is_consumed_once_and_lapses() {
    for env in envs().await {
        let items = env.items();
        env.device("d1", "alice").await;
        assert!(matches!(
            items.consume("hash-1").await.unwrap(),
            Consumed::NoApproval
        ));
        let id = created(&items, 1).await;
        assert!(matches!(
            items.consume("hash-1").await.unwrap(),
            Consumed::Pending(_)
        ));
        let effect = effect_of(&items, &id).await;
        items
            .answer(&id, answer("d1", &effect, Action::Accept))
            .await
            .unwrap();
        let Consumed::Used(l) = items.consume("hash-1").await.unwrap() else {
            panic!()
        };
        assert!(l.approval.unwrap().consumed_at.is_some());
        // The window is the larger of `consume_within` (300 s) and `lease_s` (900 s).
        assert_eq!(l.item.answered_at.map(|_| ()), Some(()));
        assert!(
            matches!(items.consume("hash-1").await.unwrap(), Consumed::Gone(_)),
            "{}",
            env.name
        );
        // A consumed approval stays consumed: the PDP never gives it back (portal P7).
        assert!(matches!(
            items.consume("hash-1").await.unwrap(),
            Consumed::Gone(_)
        ));
        // An approval that nobody used lapses after its window.
        let (n2, b2) = approval(2);
        let Created::New(l2) = items.create(n2, &b2).await.unwrap() else {
            panic!()
        };
        let id2 = l2.item.request_id.clone();
        let effect2 = effect_of(&items, &id2).await;
        items
            .answer(&id2, answer("d1", &effect2, Action::Accept))
            .await
            .unwrap();
        env.advance(Duration::from_secs(900));
        assert_eq!(env.sweep().await.lapsed, 0, "still in the window");
        env.advance(Duration::from_secs(1));
        assert_eq!(env.sweep().await.lapsed, 1);
        assert!(matches!(
            items.consume("hash-2").await.unwrap(),
            Consumed::Gone(_)
        ));
        // A decline reads as a refusal.
        let (n3, b3) = approval(3);
        let Created::New(l3) = items.create(n3, &b3).await.unwrap() else {
            panic!()
        };
        items
            .answer(&l3.item.request_id, answer("d1", "", Action::Decline))
            .await
            .unwrap();
        assert!(matches!(
            items.consume("hash-3").await.unwrap(),
            Consumed::Denied(_)
        ));
    }
}

/// Many callers use one approval at the same time: one gets it. On PostgreSQL the lock of the item and the read of the
/// approval must not come in one statement: the read would see the approval of the snapshot of the start.
#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn concurrent_consumes_use_an_approval_once_and_status_consumes_nothing() {
    for env in envs().await {
        let items = env.items();
        env.device("d1", "alice").await;
        let id = created(&items, 1).await;
        let effect = effect_of(&items, &id).await;
        items
            .answer(&id, answer("d1", &effect, Action::Accept))
            .await
            .unwrap();
        // A read of the status never consumes.
        for _ in 0..3 {
            assert!(
                matches!(
                    items.status_for_pdp("hash-1").await.unwrap(),
                    Consumed::Usable(_)
                ),
                "{}",
                env.name
            );
        }
        let calls = (0..24).map(|_| items.consume("hash-1"));
        let results = futures_util::future::join_all(calls).await;
        let used = results
            .iter()
            .filter(|r| matches!(r, Ok(Consumed::Used(_))))
            .count();
        let gone = results
            .iter()
            .filter(|r| matches!(r, Ok(Consumed::Gone(l)) if l.approval.as_ref().is_some_and(|a| a.consumed_at.is_some())))
            .count();
        assert_eq!((used, gone), (1, 23), "{}", env.name);
        assert!(matches!(
            items.status_for_pdp("hash-1").await.unwrap(),
            Consumed::Gone(_)
        ));
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_cancel_of_a_key_and_of_an_owner_withdraws_the_open_items() {
    for env in envs().await {
        let items = env.items();
        let a = created(&items, 1).await;
        let b = created(&items, 2).await;
        let (mut other, body) = approval(3);
        other.requester_key_id = "key2".into();
        let Created::New(c) = items.create(other, &body).await.unwrap() else {
            panic!()
        };
        // The agent cancels its own item once.
        assert!(matches!(
            items
                .cancel(&a, CancelBy::Agent("key1".into()))
                .await
                .unwrap(),
            Cancelled::Done(_)
        ));
        assert!(matches!(
            items
                .cancel(&a, CancelBy::Agent("key1".into()))
                .await
                .unwrap(),
            Cancelled::Final(_)
        ));
        assert!(matches!(
            items
                .cancel(&b, CancelBy::Agent("key2".into()))
                .await
                .unwrap(),
            Cancelled::NotFound
        ));
        // A revoked key cancels what is open.
        assert_eq!(
            items
                .cancel_where(Some("key1"), None, "key_revoked")
                .await
                .unwrap(),
            1
        );
        let l = items.get(&b, &Caller::System).await.unwrap().unwrap();
        assert_eq!(
            (l.item.state, l.item.settle_reason.as_deref()),
            (ItemState::Cancelled, Some("key_revoked"))
        );
        assert_eq!(
            items
                .cancel_where(None, Some("alice"), "owner_deleted")
                .await
                .unwrap(),
            1
        );
        assert_eq!(
            env.state_of(&c.item.request_id).await,
            ItemState::Cancelled,
            "{}",
            env.name
        );
    }
}

// ---------------------------------------------------------------- events, retention, seal

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn events_one_row_per_transition_none_per_renewal() {
    for env in envs().await {
        let items = env.items();
        env.device("d1", "alice").await;
        let id = created(&items, 1).await;
        for _ in 0..4 {
            env.advance(Duration::from_secs(900));
            items.renew(&id, "key1").await.unwrap();
        }
        items
            .get(&id, &Caller::Owner("alice".into()))
            .await
            .unwrap();
        let effect = effect_of(&items, &id).await;
        items
            .answer(&id, answer("d1", &effect, Action::Accept))
            .await
            .unwrap();
        let rows = env
            .count(&format!(
                "SELECT COUNT(*) FROM item_events WHERE request_id = '{id}'"
            ))
            .await;
        assert_eq!(
            rows, 2,
            "requested and answered, nothing for the 4 renewals ({})",
            env.name
        );
        assert_eq!(
            env.count(&format!(
                "SELECT COUNT(*) FROM alerts WHERE request_id = '{id}'"
            ))
            .await,
            2,
            "the request and `settled`: a renewal makes no message"
        );
        // The events hold the key, the effect and the way of the answer, never a body.
        let detail = env.count("SELECT COUNT(*) FROM item_events WHERE detail LIKE '%effect=sha256:%' AND actor = 'device:d1'").await;
        assert_eq!(detail, 1);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn body_purge_keeps_metadata_effect_and_events() {
    for env in envs().await {
        let items = env.items();
        env.device("d1", "alice").await;
        let id = created(&items, 1).await;
        let effect = effect_of(&items, &id).await;
        items
            .answer(&id, answer("d1", &effect, Action::Decline))
            .await
            .unwrap();
        let l = items.get(&id, &Caller::System).await.unwrap().unwrap();
        assert!(matches!(
            items.serve_body(&l).await.unwrap(),
            Served::Body(_)
        ));
        env.advance(30 * DAY - Duration::from_secs(1));
        assert_eq!(items.maintain().await.unwrap().bodies_purged, 0);
        env.advance(Duration::from_secs(1));
        assert_eq!(
            items.maintain().await.unwrap().bodies_purged,
            1,
            "{}",
            env.name
        );
        let l = items.get(&id, &Caller::System).await.unwrap().unwrap();
        assert!(matches!(
            items.serve_body(&l).await.unwrap(),
            Served::Purged
        ));
        assert_eq!(
            l.approval.as_ref().unwrap().effect,
            effect,
            "the effect stays"
        );
        assert!(l.item.body_purged_at.is_some() && l.item.body_size > 0);
        assert_eq!(
            env.count("SELECT COUNT(*) FROM items WHERE body_ct IS NOT NULL")
                .await,
            0
        );
        assert_eq!(env.count("SELECT COUNT(*) FROM item_events").await, 2);
        // The metadata goes after `retention`, the events after `audit_retention`.
        env.advance(150 * DAY);
        let report = items.maintain().await.unwrap();
        assert_eq!((report.items_deleted, report.events_deleted), (1, 0));
        assert_eq!(env.count("SELECT COUNT(*) FROM approvals").await, 0);
        env.advance(185 * DAY);
        assert_eq!(items.maintain().await.unwrap().events_deleted, 2);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_sealed_value_that_moves_does_not_open() {
    for env in envs().await {
        let items = env.items();
        let a = created(&items, 1).await;
        let b = created(&items, 2).await;
        env.sql(&format!(
            "UPDATE items SET body_ct = (SELECT body_ct FROM items WHERE request_id = '{b}') WHERE request_id = '{a}'"
        ))
        .await;
        let la = items.get(&a, &Caller::System).await.unwrap().unwrap();
        assert!(
            matches!(items.serve_body(&la).await.unwrap(), Served::Mismatch),
            "{}",
            env.name
        );
        // A changed effect in the row breaks the seal of the body too.
        env.sql(&format!(
            "UPDATE approvals SET effect = 'sha256:{}' WHERE request_id = '{b}'",
            "1".repeat(64)
        ))
        .await;
        let lb = items.get(&b, &Caller::System).await.unwrap().unwrap();
        assert!(matches!(
            items.serve_body(&lb).await.unwrap(),
            Served::Mismatch
        ));
    }
}

/// Every text and blob that the database holds, one string for each value (a blob as lossy text and as hex).
pub(super) async fn dump(env: &Env) -> Vec<String> {
    match (&env.store, &env.pg) {
        (Store::Sqlite(db), _) => db
            .call(|c| {
                let tables: Vec<String> = c
                    .prepare("SELECT name FROM sqlite_master WHERE type = 'table'")?
                    .query_map([], |r| r.get(0))?
                    .collect::<Result<_, _>>()?;
                let mut out = Vec::new();
                for t in tables {
                    let mut stmt = c.prepare(&format!("SELECT * FROM \"{t}\""))?;
                    let n = stmt.column_count();
                    let mut rows = stmt.query([])?;
                    while let Some(r) = rows.next()? {
                        for i in 0..n {
                            match r.get_ref(i)? {
                                rusqlite::types::ValueRef::Text(t) => {
                                    out.push(String::from_utf8_lossy(t).into())
                                }
                                rusqlite::types::ValueRef::Blob(b) => {
                                    out.push(String::from_utf8_lossy(b).into());
                                    out.push(b.iter().map(|x| format!("{x:02x}")).collect());
                                }
                                _ => {}
                            }
                        }
                    }
                }
                Ok(out)
            })
            .await
            .unwrap(),
        (_, Some(pg)) => {
            let client = pg.admin(&pg.name).await;
            let tables = client
                .query("SELECT table_name FROM information_schema.tables WHERE table_schema = 'public'", &[])
                .await
                .unwrap();
            let mut out = Vec::new();
            for t in tables {
                let name: String = t.get(0);
                for row in client
                    .query(&format!("SELECT x::text FROM \"{name}\" x"), &[])
                    .await
                    .unwrap()
                {
                    out.push(row.get(0));
                }
            }
            out
        }
        _ => unreachable!(),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn canary_absent_in_every_column_and_in_replica_snapshot() {
    for env in envs().await {
        let items = env.items();
        env.device("d1", "alice").await;
        // One unique string in the command, the explanation, a turn of the agent, a turn of the owner and the comment.
        let op = json!({"sql": format!("SELECT '{CANARY}'")});
        let effect = effect_hash("db", "run_write_query", "/db/analytics", &op).unwrap();
        let (mut item, _) = approval(1);
        item.approval.as_mut().unwrap().effect = effect.clone();
        let body = serde_json::to_vec(
            &json!({"op": op, "view": {"title": "t"}, "explanation": CANARY, "hint": CANARY}),
        )
        .unwrap();
        let Created::New(l) = items.create(item, &body).await.unwrap() else {
            panic!()
        };
        let id = l.item.request_id.clone();
        items.say(&id, "key1", CANARY.as_bytes()).await.unwrap();
        items
            .reply(
                &id,
                "alice",
                Some("d1".into()),
                "m1".into(),
                CANARY.as_bytes(),
            )
            .await
            .unwrap();
        let mut decline = answer("d1", &effect, Action::Decline);
        decline.payload = Some(Zeroizing::new(CANARY.as_bytes().to_vec()));
        assert!(matches!(
            items.answer(&id, decline).await.unwrap(),
            Answered::Accepted { .. }
        ));
        let hex: String = CANARY.bytes().map(|b| format!("{b:02x}")).collect();
        for stage in ["before the purge", "after the purge"] {
            let values = dump(&env).await;
            assert!(
                values.len() > 10,
                "the dump reads the tables: {} values",
                values.len()
            );
            for v in &values {
                assert!(
                    !v.contains(CANARY) && !v.contains(&hex),
                    "{} {stage}: a column holds the canary",
                    env.name
                );
            }
            // The clear text is there in the answer of the server, for the owner.
            if stage == "before the purge" {
                let Served::Body(plain) = items
                    .serve_body(&items.get(&id, &Caller::System).await.unwrap().unwrap())
                    .await
                    .unwrap()
                else {
                    panic!()
                };
                assert!(String::from_utf8_lossy(&plain).contains(CANARY));
                let turns = items.serve_turns(&id).await.unwrap();
                assert!(turns
                    .iter()
                    .filter(|t| t.text.is_some())
                    .all(|t| String::from_utf8_lossy(t.text.as_ref().unwrap()).contains(CANARY)));
                assert_eq!(turns.iter().filter(|t| t.text.is_some()).count(), 3);
                env.advance(31 * DAY);
                items.maintain().await.unwrap();
            }
        }
        if let (Store::Sqlite(db), Some(path)) = (&env.store, &env.path) {
            // The copy of the replica is a snapshot of the file: it holds no clear text either.
            let copy = path.with_extension("snapshot");
            let target = copy.clone();
            db.call(move |c| {
                let mut dst = rusqlite::Connection::open(&target)?;
                let backup = rusqlite::backup::Backup::new(c, &mut dst)?;
                backup.run_to_completion(64, Duration::from_millis(0), None)?;
                Ok(())
            })
            .await
            .unwrap();
            let bytes = std::fs::read(&copy).unwrap();
            assert!(bytes.len() > 4096);
            assert!(
                !bytes.windows(CANARY.len()).any(|w| w == CANARY.as_bytes()),
                "the snapshot holds the canary"
            );
        }
    }
}

// ---------------------------------------------------------------- the start

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn orphan_body_kid_stops_start_with_count() {
    for env in envs().await {
        let items = env.items();
        let a = created(&items, 1).await;
        created(&items, 2).await;
        env.device("d1", "alice").await;
        items.say(&a, "key1", b"text").await.unwrap();
        assert!(start_checks(&env.store, &env.content).await.is_ok());
        // The file lost the key `k1` and holds `k2` only.
        let other = content(&[("k2", 2)], "k2");
        let error = start_checks(&env.store, &other).await.unwrap_err();
        assert!(
            error.contains("\"k1\"") && error.contains("3 sealed contents"),
            "{}: {error}",
            env.name
        );
        assert!(error.contains("content-purge"), "{error}");
        // The command erases the contents and keeps the metadata: the start goes on.
        let (items_n, turns_n, intents_n, _agents_n) =
            super::items_store::purge_key(&env.store, "k1")
                .await
                .unwrap();
        assert_eq!((items_n, turns_n, intents_n), (2, 1, 0));
        assert!(start_checks(&env.store, &other).await.is_ok());
        assert_eq!(
            env.count("SELECT COUNT(*) FROM items").await,
            2,
            "the metadata stays"
        );
        let l = items.get(&a, &Caller::System).await.unwrap().unwrap();
        assert!(matches!(
            items.serve_body(&l).await.unwrap(),
            Served::Purged
        ));
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn old_inbox_schema_stops_start() {
    for env in envs().await {
        assert!(start_checks(&env.store, &env.content).await.is_ok());
        // A database that version 5.0 made has no table of the items.
        let drop = match env.store {
            Store::Sqlite(_) => "DROP TABLE approvals; DROP TABLE turns; DROP TABLE items;",
            Store::Pg(_) => "DROP TABLE items CASCADE;",
        };
        env.sql(drop).await;
        let error = start_checks(&env.store, &env.content).await.unwrap_err();
        assert!(
            error.contains("table items") && error.contains("docs/operations.md, Upgrades"),
            "{}: {error}",
            env.name
        );
    }
}

// ---------------------------------------------------------------- several instances and the degraded mode

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn sweeper_two_instances_no_double_transition() {
    for mut env in envs().await {
        env.rules.max_open_per_key = 100;
        env.rules.max_open_per_owner = 100;
        let items = env.items();
        for n in 1..=20 {
            created(&items, n).await;
        }
        env.advance(Duration::from_secs(3 * 900 + 120));
        // A second instance on the same database, when there is one.
        let second = match (&env.store, &env.pg) {
            (Store::Pg(_), Some(pg)) => Some(Store::Pg(Arc::new(pg.store().await))),
            _ => None,
        };
        let other = Items {
            store: second.as_ref().unwrap_or(&env.store),
            ..env.items()
        };
        let (a, b) = tokio::join!(items.sweep(), other.sweep());
        let total = a.unwrap().abandoned + b.unwrap().abandoned;
        assert_eq!(total, 20, "{}", env.name);
        assert_eq!(
            env.count("SELECT COUNT(*) FROM item_events WHERE event = 'abandoned'")
                .await,
            20
        );
        assert_eq!(
            env.count("SELECT COUNT(*) FROM alerts WHERE kind = 'settled'")
                .await,
            20
        );
        assert_eq!(
            env.count("SELECT COUNT(*) FROM items WHERE state = 'abandoned'")
                .await,
            20
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn degraded_storage_returns_storage_degraded_and_writes_nothing() {
    for env in envs().await {
        let Store::Pg(pg) = &env.store else { continue };
        let items = env.items();
        pg.mark_down(&DbError::Unavailable("test".into()));
        let (item, body) = approval(1);
        assert!(matches!(
            items.create(item, &body).await,
            Err(ItemError::Degraded)
        ));
        assert!(matches!(
            items.renew("x", "key1").await,
            Err(ItemError::Degraded)
        ));
        assert!(matches!(
            items.get("x", &Caller::System).await,
            Err(ItemError::Degraded)
        ));
        assert!(matches!(items.sweep().await, Err(ItemError::Degraded)));
        pg.mark_up();
        assert_eq!(env.count("SELECT COUNT(*) FROM items").await, 0);
        assert_eq!(
            env.count("SELECT COUNT(*) FROM alerts").await,
            0,
            "no spool, no alert"
        );
        created(&items, 1).await;
    }
}

#[test]
fn the_journal_and_the_metrics_follow_the_names_of_the_specification() {
    use super::items_store::log_name;
    for event in [
        "requested",
        "answered",
        "consumed",
        "abandoned",
        "revived",
        "superseded",
        "expired",
    ] {
        assert_eq!(log_name(Kind::Approval, event), format!("approval_{event}"));
    }
    assert_eq!(log_name(Kind::Question, "answered"), "question_answered");
    let metrics = super::metrics::InboxMetrics::default();
    metrics.item_event("requested");
    metrics.item_answer("accepted");
    metrics.item_renewal_written();
    metrics.item_body_served();
    metrics.item_body_read();
    metrics.item_body_effect_mismatch();
    let text = metrics.render(None);
    for name in [
        "portal_item_events_total{event=\"requested\"} 1",
        "portal_item_answers_total{status=\"accepted\"} 1",
        "portal_item_lease_writes_total 1",
        "portal_item_body_served_total 1",
        "portal_item_body_reads_total 1",
        "portal_item_body_effect_mismatch_total 1",
    ] {
        assert!(text.contains(name), "{name}");
    }
}

// ---------------------------------------------------------------- preferences and digest

/// Unix time in milliseconds of an RFC 3339 text.
fn at(text: &str) -> i64 {
    text.parse::<jiff::Timestamp>().unwrap().as_millisecond()
}

fn paris_daily() -> crate::config::DigestConfig {
    crate::config::DigestConfig::Daily {
        hour: 9,
        minute: 0,
        timezone: "Europe/Paris".into(),
    }
}

fn digest_off() -> crate::config::DigestConfig {
    crate::config::DigestConfig::Off
}

fn every_prefs(hours: u16) -> inbox_core::proto::Prefs {
    inbox_core::proto::Prefs {
        quiet: None,
        digest: Some(inbox_core::proto::DigestPrefs {
            mode: inbox_core::proto::DigestMode::Every,
            at: None,
            tz: None,
            every_h: Some(hours),
        }),
    }
}

impl Env {
    /// The creation times of the rows of the digest of `owner`, oldest first.
    async fn digest_times(&self, owner: &str) -> Vec<i64> {
        let sql = format!(
            "SELECT created_at FROM alerts WHERE kind = 'digest' AND target_id = '{owner}' ORDER BY created_at"
        );
        match (&self.store, &self.pg) {
            (Store::Sqlite(db), _) => db
                .call(move |c| {
                    let mut stmt = c.prepare(&sql)?;
                    let rows = stmt.query_map([], |r| r.get::<_, i64>(0))?;
                    Ok(rows.collect::<Result<Vec<_>, _>>()?)
                })
                .await
                .unwrap(),
            (_, Some(pg)) => pg
                .admin(&pg.name)
                .await
                .query(&sql, &[])
                .await
                .unwrap()
                .iter()
                .map(|r| r.get::<_, i64>(0))
                .collect(),
            _ => unreachable!(),
        }
    }

    /// Runs the digest pass of one instance every `step` from `from` to `to` (both included). The result is the number
    /// of rows written.
    async fn digest_passes(
        &self,
        items: &Items<'_>,
        file: &crate::config::DigestConfig,
        memo: &mut DigestMemo,
        from: &str,
        to: &str,
        step: Duration,
    ) -> u32 {
        let (mut now, end) = (at(from), at(to));
        let mut written = 0;
        while now <= end {
            self.clock.set(now);
            written += items.digest_pass(file, memo).await.unwrap();
            now += step.as_millis() as i64;
        }
        written
    }
}

const FIVE_MIN: Duration = Duration::from_secs(300);

/// The digest of 09:00 in Paris is due once for each local day, at 08:00 UTC before the change to summer time and at 07:00
/// UTC after it (2026-03-29).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn daily_digest_once_at_local_time_across_dst_change() {
    for env in envs().await {
        env.device("phone-a", "alice").await;
        env.clock.set(at("2026-03-27T10:00:00Z"));
        let items = env.items();
        created(&items, 1).await;
        let mut memo = DigestMemo::default();
        let written = env
            .digest_passes(
                &items,
                &paris_daily(),
                &mut memo,
                "2026-03-28T00:00:00Z",
                "2026-03-30T12:00:00Z",
                FIVE_MIN,
            )
            .await;
        assert_eq!(written, 3, "{}", env.name);
        assert_eq!(
            env.digest_times("alice").await,
            vec![
                at("2026-03-28T08:00:00Z"),
                at("2026-03-29T07:00:00Z"),
                at("2026-03-30T07:00:00Z")
            ],
            "{}",
            env.name
        );
        assert_eq!(
            env.count("SELECT COUNT(*) FROM deliveries d JOIN alerts a ON a.id = d.alert_id WHERE a.kind = 'digest'")
                .await,
            3,
            "one delivery for the one phone, each day"
        );
        // An instance that has no memory (a restart) writes nothing more for a due time that has its row.
        let mut fresh = DigestMemo::default();
        env.clock.set(at("2026-03-30T07:05:00Z"));
        assert_eq!(
            items.digest_pass(&paris_daily(), &mut fresh).await.unwrap(),
            0
        );
        assert_eq!(env.digest_times("alice").await.len(), 3);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn digest_every_n_hours() {
    for env in envs().await {
        env.device("phone-a", "alice").await;
        env.clock.set(at("2026-06-09T23:00:00Z"));
        let items = env.items();
        created(&items, 1).await;
        items.set_prefs("alice", &every_prefs(6)).await.unwrap();
        let mut memo = DigestMemo::default();
        // The file says off: the account decides.
        let written = env
            .digest_passes(
                &items,
                &digest_off(),
                &mut memo,
                "2026-06-10T00:00:00Z",
                "2026-06-11T00:00:00Z",
                FIVE_MIN,
            )
            .await;
        assert_eq!(written, 5, "{}", env.name);
        assert_eq!(
            env.digest_times("alice").await,
            vec![
                at("2026-06-10T00:00:00Z"),
                at("2026-06-10T06:00:00Z"),
                at("2026-06-10T12:00:00Z"),
                at("2026-06-10T18:00:00Z"),
                at("2026-06-11T00:00:00Z")
            ],
            "{}",
            env.name
        );
    }
}

/// No row is written when no item is open at the due time. An item that comes after the due time waits for the next one.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn digest_empty_not_sent() {
    for mut env in envs().await {
        env.rules.max_open_per_key = 100;
        env.device("phone-a", "alice").await;
        let items = env.items();
        let mut memo = DigestMemo::default();
        let file = paris_daily();
        // Nothing is open: the due time of 2026-06-10 (07:00 UTC) passes without a row.
        env.clock.set(at("2026-06-09T12:00:00Z"));
        let n = env
            .digest_passes(
                &items,
                &file,
                &mut memo,
                "2026-06-10T06:00:00Z",
                "2026-06-10T08:00:00Z",
                FIVE_MIN,
            )
            .await;
        assert_eq!(n, 0, "{}", env.name);
        // An item that comes 5 minutes after the due time is not in this digest.
        env.clock.set(at("2026-06-10T07:05:00Z"));
        created(&items, 1).await;
        let n = env
            .digest_passes(
                &items,
                &file,
                &mut memo,
                "2026-06-10T07:05:00Z",
                "2026-06-10T07:15:00Z",
                FIVE_MIN,
            )
            .await;
        assert_eq!(n, 0, "{}", env.name);
        // The item is closed before the next due time: nothing is open, nothing is sent.
        assert_eq!(
            items
                .cancel_where(Some("key1"), None, "key_revoked")
                .await
                .unwrap(),
            1
        );
        let n = env
            .digest_passes(
                &items,
                &file,
                &mut memo,
                "2026-06-11T06:00:00Z",
                "2026-06-11T08:00:00Z",
                FIVE_MIN,
            )
            .await;
        assert_eq!(n, 0, "{}", env.name);
        assert!(env.digest_times("alice").await.is_empty());
        // With an open item, the same due time gives one row.
        env.clock.set(at("2026-06-12T05:00:00Z"));
        created(&items, 2).await;
        let n = env
            .digest_passes(
                &items,
                &file,
                &mut memo,
                "2026-06-12T06:00:00Z",
                "2026-06-12T08:00:00Z",
                FIVE_MIN,
            )
            .await;
        assert_eq!(n, 1, "{}", env.name);
        assert_eq!(
            env.digest_times("alice").await,
            vec![at("2026-06-12T07:00:00Z")]
        );
    }
}

/// Two instances, with their own memory, write one row for each owner and due time, also when they run together.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn digest_two_instances_one_frame() {
    for mut env in envs().await {
        env.rules.max_open_per_key = 100;
        env.device("phone-a", "alice").await;
        env.device("phone-b", "bob").await;
        env.clock.set(at("2026-06-10T00:00:00Z"));
        let items = env.items();
        created(&items, 1).await;
        let (mut item, body) = approval(2);
        item.owner = "bob".into();
        item.requester_key_id = "key2".into();
        assert!(matches!(
            items.create(item, &body).await.unwrap(),
            Created::New(_)
        ));
        let second = match (&env.store, &env.pg) {
            (Store::Pg(_), Some(pg)) => Some(Store::Pg(Arc::new(pg.store().await))),
            _ => None,
        };
        let other = Items {
            store: second.as_ref().unwrap_or(&env.store),
            ..env.items()
        };
        let file = paris_daily();
        for pass in 0..3 {
            env.clock.set(at("2026-06-10T07:00:00Z") + pass * 60_000);
            let (mut a, mut b) = (DigestMemo::default(), DigestMemo::default());
            let (x, y) = tokio::join!(
                items.digest_pass(&file, &mut a),
                other.digest_pass(&file, &mut b)
            );
            let written = x.unwrap() + y.unwrap();
            assert_eq!(
                written,
                if pass == 0 { 2 } else { 0 },
                "{} pass {pass}",
                env.name
            );
        }
        assert_eq!(env.digest_times("alice").await.len(), 1, "{}", env.name);
        assert_eq!(env.digest_times("bob").await.len(), 1, "{}", env.name);
        assert_eq!(
            env.count("SELECT COUNT(*) FROM deliveries d JOIN alerts a ON a.id = d.alert_id WHERE a.kind = 'digest'")
                .await,
            2,
            "one delivery for each phone"
        );
    }
}

/// The values of the account replace the default of the file, and no value goes past a limit of the file.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn prefs_override_file_defaults_never_a_cap() {
    use inbox_core::proto::{DigestMode, DigestPrefs, Prefs};
    for env in envs().await {
        env.device("phone-a", "alice").await;
        env.clock.set(at("2026-06-09T12:00:00Z"));
        let items = env.items();
        created(&items, 1).await;
        let file = paris_daily();
        let mut memo = DigestMemo::default();
        macro_rules! passes {
            ($from:expr, $to:expr) => {
                env.digest_passes(&items, &file, &mut memo, $from, $to, FIVE_MIN)
            };
        }
        // a) No preference: the file applies (09:00 Paris, 07:00 UTC in summer).
        assert_eq!(
            passes!("2026-06-10T00:00:00Z", "2026-06-10T12:00:00Z").await,
            1,
            "{}",
            env.name
        );
        // b) The account turns the digest off: the default of the file is replaced.
        let off = Prefs {
            quiet: None,
            digest: Some(DigestPrefs {
                mode: DigestMode::Off,
                at: None,
                tz: None,
                every_h: None,
            }),
        };
        items.set_prefs("alice", &off).await.unwrap();
        assert_eq!(
            passes!("2026-06-11T00:00:00Z", "2026-06-11T12:00:00Z").await,
            0,
            "{}",
            env.name
        );
        // c) The account asks for a digest every 2 hours: rows at even hours, none at the 07:00 of the file.
        items.set_prefs("alice", &every_prefs(2)).await.unwrap();
        assert_eq!(
            passes!("2026-06-12T00:00:00Z", "2026-06-12T08:00:00Z").await,
            5,
            "{}",
            env.name
        );
        let times = env.digest_times("alice").await;
        assert!(!times.contains(&at("2026-06-12T07:00:00Z")), "{times:?}");
        assert!(
            times.contains(&at("2026-06-12T06:00:00Z"))
                && times.contains(&at("2026-06-12T08:00:00Z"))
        );
        // d) The account removes its values: the file applies again.
        items.set_prefs("alice", &Prefs::default()).await.unwrap();
        assert_eq!(
            passes!("2026-06-13T00:00:00Z", "2026-06-13T12:00:00Z").await,
            1,
            "{}",
            env.name
        );
        assert!(env
            .digest_times("alice")
            .await
            .contains(&at("2026-06-13T07:00:00Z")));

        // The cap: the period of the account stays in the range of the key `inbox.items.digest.every` (1 hour to 7
        // days). A value outside the range is refused and nothing is stored.
        items.set_prefs("alice", &every_prefs(6)).await.unwrap();
        for bad in [0u16, 169] {
            match items.set_prefs("alice", &every_prefs(bad)).await {
                Err(ItemError::Prefs(e)) => assert_eq!(e.field, "digest.every_h", "{}", env.name),
                other => panic!("{}: {other:?}", env.name),
            }
        }
        assert_eq!(
            items.prefs("alice").await.unwrap(),
            every_prefs(6),
            "the refused value changed nothing"
        );
        items.set_prefs("alice", &every_prefs(1)).await.unwrap();
        items.set_prefs("alice", &every_prefs(168)).await.unwrap();
    }
}
