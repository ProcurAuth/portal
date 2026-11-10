//! Tests of `portal inbox rekey`, on SQLite and, when `INBOX_TEST_PG` names a server, on PostgreSQL.

use std::time::Duration;

use super::agents::{AgentCall, AgentMode, Hints};
use super::content_box::{self, ContentBox};
use super::items::Wait;
use super::items_ops::{RekeyCursor, RekeyTable};
use super::items_store::{start_checks, Items, NewIntent, Served};
use super::items_tests::{content, created, envs, question, Env, CANARY};
use super::rekey::{self, Options, Report, Target};
use super::store::Store;

/// Everything that the test seals: the plain text of each sealed value, read through the public side of the engine.
#[derive(Debug, PartialEq, Eq)]
struct Plains {
    bodies: Vec<String>,
    turns: Vec<String>,
    intents: Vec<String>,
    hints: Vec<String>,
}

struct Seeded {
    items: Vec<String>,
    with_turns: String,
    digests: Vec<String>,
    agent: String,
}

/// A phone for the owner, and limits that hold the rows of the test.
async fn roomy(env: &mut Env) {
    env.device("d1", "alice").await;
    env.rules.max_open_per_key = 1000;
    env.rules.max_open_per_owner = 1000;
    env.rules.max_open_total = 1000;
    env.rules.agents.max_per_key = 100;
    env.rules.agents.new_per_hour = 100;
}

/// The start of an instance with the keys of the box: it writes the check value of each key.
async fn bind(env: &Env) {
    let bound = env
        .store
        .bind_content_keys(env.content.check_values())
        .await
        .unwrap();
    assert_eq!(bound, super::db::KeyBind::Bound);
}

/// Writes the four kinds of sealed value with the key that the box makes active.
async fn seed(items: &Items<'_>, first: u32, count: u32) -> Seeded {
    let mut ids = Vec::new();
    for n in first..first + count {
        if n % 3 == 0 {
            let (item, body) = question(n);
            match items.create(item, &body).await.unwrap() {
                super::items_store::Created::New(l) => ids.push(l.item.request_id.clone()),
                other => panic!("not created: {other:?}"),
            }
        } else {
            ids.push(created(items, n).await);
        }
    }
    let with_turns = ids[0].clone();
    for i in 0..3 {
        items
            .say(
                &with_turns,
                "key1",
                format!("{CANARY} turn {first} {i}").as_bytes(),
            )
            .await
            .unwrap();
    }
    let mut digests = Vec::new();
    let agent = items
        .resolve_agent(
            "key1",
            "alice",
            &AgentCall {
                mode: AgentMode::Discover,
                token: None,
                hints: Hints {
                    says: Some(format!("{CANARY} agent {first}")),
                    ..Default::default()
                },
            },
            true,
        )
        .await
        .unwrap()
        .unwrap()
        .agent_id;
    for i in 0..2 {
        let digest = format!("digest-{first}-{i}");
        items
            .create_intent(NewIntent {
                digest: &digest,
                key_id: "key1",
                agent_id: &agent,
                wait: Wait::Lease { lease_s: 900 },
                allow_custom: false,
                allow_ask_back: false,
                previous_id: None,
                content: format!("{{\"explanation\":\"{CANARY} intent {first} {i}\"}}").as_bytes(),
                ttl_ms: 86_400_000,
            })
            .await
            .unwrap();
        digests.push(digest);
    }
    Seeded {
        items: ids,
        with_turns,
        digests,
        agent,
    }
}

/// Reads all the sealed values back. A value that does not open gives a loud text, so a comparison fails.
async fn read_all(items: &Items<'_>, seeded: &Seeded) -> Plains {
    let mut bodies = Vec::new();
    for id in &seeded.items {
        let l = items
            .get(id, &super::items::Caller::System)
            .await
            .unwrap()
            .unwrap();
        bodies.push(match items.serve_body(&l).await.unwrap() {
            Served::Body(b) => String::from_utf8(b.to_vec()).unwrap(),
            other => format!("NOT READABLE: {}", variant(&other)),
        });
    }
    let turns = items
        .serve_turns(&seeded.with_turns)
        .await
        .unwrap()
        .into_iter()
        .map(|t| {
            t.text.map_or("NOT READABLE".to_string(), |x| {
                String::from_utf8(x.to_vec()).unwrap()
            })
        })
        .collect();
    let mut intents = Vec::new();
    for digest in &seeded.digests {
        intents.push(match items.read_intent(digest, "key1").await.unwrap() {
            Some(p) => String::from_utf8(p.content.to_vec()).unwrap(),
            None => "NOT READABLE".into(),
        });
    }
    let agent = items.agent(&seeded.agent).await.unwrap().unwrap();
    let hints = vec![items
        .observed(&agent)
        .map_or("NOT READABLE".to_string(), |o| format!("{o:?}"))];
    Plains {
        bodies,
        turns,
        intents,
        hints,
    }
}

fn variant(s: &Served) -> &'static str {
    match s {
        Served::NotFound => "not found",
        Served::Purged => "purged",
        Served::Mismatch => "mismatch",
        Served::Body(_) => "body",
    }
}

/// How many sealed values each key id seals, in the four tables.
async fn counts(env: &Env) -> Vec<(String, u64)> {
    let mut out = Vec::new();
    for kid in ["k1", "k2", "k3"] {
        let n = env
            .count(&format!(
                "SELECT (SELECT COUNT(*) FROM items WHERE body_kid = '{kid}' AND body_ct IS NOT NULL)
                      + (SELECT COUNT(*) FROM turns WHERE body_kid = '{kid}' AND payload_ct IS NOT NULL)
                      + (SELECT COUNT(*) FROM approval_intents WHERE body_kid = '{kid}' AND body_ct IS NOT NULL)
                      + (SELECT COUNT(*) FROM agents WHERE hints_kid = '{kid}' AND hints_ct IS NOT NULL)"
            ))
            .await;
        if n > 0 {
            out.push((kid.to_string(), n as u64));
        }
    }
    out
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn rekey_moves_rows_to_active_key_resumes_after_kill() {
    for mut env in envs().await {
        roomy(&mut env).await;
        // Step 0: the rows of the key `k1`.
        env.content = content(&[("k1", 1)], "k1");
        bind(&env).await;
        let seeded = seed(&env.items(), 1, 8).await;
        let before = read_all(&env.items(), &seeded).await;
        assert!(
            !before.bodies.iter().any(|b| b.contains("NOT READABLE")),
            "{}: {before:?}",
            env.name
        );
        let all_k1 = counts(&env).await;
        assert_eq!(all_k1.len(), 1, "{}: {all_k1:?}", env.name);
        let total = all_k1[0].1;
        assert!(total >= 10, "{}: {total}", env.name);
        // Step 1: the new key, active. The new rows use `k2`, the old rows stay readable.
        env.content = content(&[("k1", 1), ("k2", 2)], "k2");
        bind(&env).await;
        let later = seed(&env.items(), 100, 3).await;
        let with_k2 = counts(&env).await;
        assert_eq!(with_k2.len(), 2, "{}: {with_k2:?}", env.name);
        let k2_before = with_k2[1].1;
        // Step 2: the command, stopped by a kill after 2 batches of 3 rows.
        let target = Target::Store(&env.store);
        let options = Options {
            batch: 3,
            stop_after_batches: Some(2),
            ..Default::default()
        };
        let first = rekey::run(&target, &env.content, &options).await.unwrap();
        assert!(first.stopped, "{}", env.name);
        assert!(
            first.total() > 0 && first.total() < total,
            "{}: {first:?}",
            env.name
        );
        let middle = counts(&env).await;
        let left: u64 = middle
            .iter()
            .filter(|(k, _)| k == "k1")
            .map(|(_, n)| *n)
            .sum();
        assert_eq!(
            left,
            total - first.total(),
            "{}: no row lost, none counted twice",
            env.name
        );
        assert_eq!(
            read_all(&env.items(), &seeded).await,
            before,
            "{}: the rows read the same in the middle of the run",
            env.name
        );
        // The start with the old key in the file works, and it tells that rows wait.
        assert!(start_checks(&env.store, &env.content).await.is_ok());
        // The command runs again and finishes.
        let rest = rekey::run(
            &target,
            &env.content,
            &Options {
                batch: 3,
                ..Default::default()
            },
        )
        .await
        .unwrap();
        assert!(
            !rest.stopped && rest.complete("k2", None),
            "{}: {rest:?}",
            env.name
        );
        assert_eq!(first.total() + rest.total(), total, "{}", env.name);
        for (i, table) in RekeyTable::ALL.iter().enumerate() {
            assert!(
                first.moved[i] + rest.moved[i] > 0,
                "{}: the table {} has rows to move",
                env.name,
                table.label()
            );
        }
        assert_eq!(rest.left_of("k1"), 0);
        assert_eq!(rest.check_dropped, ["k1"], "{}", env.name);
        assert_eq!(
            counts(&env).await,
            vec![("k2".to_string(), total + k2_before)],
            "{}: every row uses the active key",
            env.name
        );
        // The associated data did not change: each value opens, and the effect of each approval still agrees.
        assert_eq!(
            read_all(&env.items(), &seeded).await,
            before,
            "{}",
            env.name
        );
        let _ = read_all(&env.items(), &later).await;
        // The command again: nothing to do (idempotent).
        let again = rekey::run(&target, &env.content, &Options::default())
            .await
            .unwrap();
        assert_eq!(again.total(), 0, "{}", env.name);
        assert!(again.complete("k2", None));
        // The old key leaves the file.
        let only_new = content(&[("k2", 2)], "k2");
        assert!(
            start_checks(&env.store, &only_new).await.is_ok(),
            "{}",
            env.name
        );
        env.content = only_new;
        assert_eq!(
            read_all(&env.items(), &seeded).await,
            before,
            "{}",
            env.name
        );
        assert_eq!(
            env.count("SELECT COUNT(*) FROM content_key_check WHERE body_kid = 'k1'")
                .await,
            0,
            "{}: the check value of the old key is dropped",
            env.name
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn rekey_from_one_key_leaves_the_others_and_refuses_bad_ids() {
    for mut env in envs().await {
        roomy(&mut env).await;
        env.content = content(&[("k1", 1)], "k1");
        let seeded = seed(&env.items(), 1, 4).await;
        env.content = content(&[("k1", 1), ("k2", 2)], "k2");
        bind(&env).await;
        seed(&env.items(), 50, 4).await;
        env.content = content(&[("k1", 1), ("k2", 2), ("k3", 3)], "k3");
        bind(&env).await;
        let target = Target::Store(&env.store);
        for (from, why) in [("k3", "active key"), ("k9", "not in the file")] {
            let error = rekey::run(
                &target,
                &env.content,
                &Options {
                    from: Some(from.into()),
                    ..Default::default()
                },
            )
            .await
            .unwrap_err();
            assert!(error.contains(why) && error.contains(from), "{error}");
        }
        let report = rekey::run(
            &target,
            &env.content,
            &Options {
                from: Some("k2".into()),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        assert!(
            report.total() > 0 && report.complete("k3", Some("k2")),
            "{report:?}"
        );
        assert_eq!(report.check_dropped, ["k2"], "only the key that was named");
        let left = counts(&env).await;
        assert!(
            left.iter().any(|(k, _)| k == "k1") && left.iter().any(|(k, _)| k == "k3"),
            "{}: {left:?}",
            env.name
        );
        assert!(
            !left.iter().any(|(k, _)| k == "k2"),
            "{}: {left:?}",
            env.name
        );
        assert!(!read_all(&env.items(), &seeded).await.bodies[0].contains("NOT READABLE"));
        // Without `--from`, the command takes every key that is not active.
        let rest = rekey::run(&target, &env.content, &Options::default())
            .await
            .unwrap();
        assert!(rest.complete("k3", None), "{rest:?}");
        assert_eq!(counts(&env).await.len(), 1, "{}", env.name);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn rekey_does_not_overwrite_a_row_that_a_writer_changed() {
    for mut env in envs().await {
        roomy(&mut env).await;
        env.content = content(&[("k1", 1)], "k1");
        let seeded = seed(&env.items(), 1, 2).await;
        env.content = content(&[("k1", 1), ("k2", 2)], "k2");
        bind(&env).await;
        let target = Target::Store(&env.store);
        // The command reads a page of the items ...
        let rows = target
            .page(
                RekeyTable::Items,
                "k2",
                "",
                &RekeyCursor {
                    id: String::new(),
                    seq: -1,
                },
                10,
            )
            .await
            .unwrap();
        assert_eq!(rows.len(), 2, "{}", env.name);
        // ... a writer seals the body of the first row again with the active key ...
        let row = &rows[0];
        let writer = env
            .content
            .seal(
                b"the writer wins",
                &content_box::aad(&row.id, content_box::FIELD_BODY, 0, row.effect.as_deref()),
            )
            .unwrap();
        let hex: String = writer.iter().map(|b| format!("{b:02x}")).collect();
        let literal = match env.store {
            Store::Sqlite(_) => format!("x'{hex}'"),
            Store::Pg(_) => format!("decode('{hex}', 'hex')"),
        };
        env.sql(&format!(
            "UPDATE items SET body_kid = 'k2', body_ct = {literal} WHERE request_id = '{}'",
            row.id
        ))
        .await;
        // ... and the command writes its page: the changed row stays as the writer made it.
        let report = rekey::run(&target, &env.content, &Options::default())
            .await
            .unwrap();
        let stored = env
            .count(&format!(
                "SELECT COUNT(*) FROM items WHERE request_id = '{}' AND body_kid = 'k2'",
                row.id
            ))
            .await;
        assert_eq!(stored, 1, "{}", env.name);
        assert!(report.complete("k2", None), "{}: {report:?}", env.name);
        // The compare-and-set itself: the old sealed value of the first row is gone, so a swap with it fails.
        let new = super::items_ops::Sealed {
            kid: "k2".into(),
            ct: b"x".to_vec(),
        };
        let (done, lost) = target
            .swap(RekeyTable::Items, vec![(rows[0].clone(), new)])
            .await
            .unwrap();
        assert_eq!((done, lost), (0, 1), "{}", env.name);
        let _ = read_all(&env.items(), &seeded).await;
        env.advance(Duration::from_secs(1));
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn rekey_reports_a_row_that_does_not_open_and_leaves_it() {
    for mut env in envs().await {
        roomy(&mut env).await;
        env.content = content(&[("k1", 1)], "k1");
        let seeded = seed(&env.items(), 1, 3).await;
        env.content = content(&[("k1", 1), ("k2", 2)], "k2");
        bind(&env).await;
        // An attacker or a fault moves the sealed body of one item to another item.
        let (a, b) = (&seeded.items[1], &seeded.items[2]);
        env.sql(&format!(
            "UPDATE items SET body_ct = (SELECT body_ct FROM items WHERE request_id = '{a}') WHERE request_id = '{b}'"
        ))
        .await;
        let report = rekey::run(
            &Target::Store(&env.store),
            &env.content,
            &Options::default(),
        )
        .await
        .unwrap();
        assert_eq!(report.unreadable, 1, "{}: {report:?}", env.name);
        assert!(report.unreadable_ids[0].contains(b.as_str()), "{report:?}");
        assert!(!report.complete("k2", None));
        assert!(
            report.left_of("k1") >= 1,
            "the check value stays: {report:?}"
        );
        assert!(report.check_dropped.is_empty());
        let text = report.text("k2");
        assert!(!text.contains(CANARY), "the report holds no content");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn rekey_refuses_sqlite_while_running() {
    let dir = std::env::temp_dir().join(format!(
        "portal-rekey-{}-{}",
        std::process::id(),
        super::util::random_id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("inbox.db");
    let (db, closed) = super::db::Db::open(&path).unwrap();
    drop(db);
    drop(closed);
    // No mark: no instance runs. The command opens the file.
    assert!(rekey::open_sqlite(&path).is_ok());
    // The mark of this process is not another instance.
    crate::store::mark_sqlite_instance(&path).unwrap();
    assert!(rekey::open_sqlite(&path).is_ok());
    // The process 1 exists and is not this one: an instance runs.
    let mut pid = path.as_os_str().to_os_string();
    pid.push(".pid");
    std::fs::write(&pid, "1\n").unwrap();
    let error = rekey::open_sqlite(&path).err().expect("refused");
    assert!(
        error.contains("the portal runs") && error.contains("stop it first"),
        "{error}"
    );
    // The command line gives the same answer: the check comes before any write.
    std::fs::remove_dir_all(dir).unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn removed_key_with_rows_stops_start_until_rekey_complete() {
    for mut env in envs().await {
        roomy(&mut env).await;
        env.content = content(&[("k1", 1)], "k1");
        seed(&env.items(), 1, 6).await;
        let with_both = content(&[("k1", 1), ("k2", 2)], "k2");
        let only_new = content(&[("k2", 2)], "k2");
        env.content = with_both;
        bind(&env).await;
        // The old key is in the file and not active: the start works, and it names the rows that wait.
        assert!(start_checks(&env.store, &env.content).await.is_ok());
        let waiting = env.content.rows_to_rekey(&counts(&env).await);
        assert_eq!(waiting.len(), 1, "{}: {waiting:?}", env.name);
        assert_eq!(waiting[0].0, "k1");
        // The operator removes the old key too early: the start stops, with the id, the count and the way out.
        let error = start_checks(&env.store, &only_new).await.unwrap_err();
        let count = waiting[0].1;
        assert!(
            error.contains("\"k1\"") && error.contains(&format!("{count} sealed contents")),
            "{}: {error}",
            env.name
        );
        assert!(
            error.contains("portal inbox rekey") && error.contains("--from k1"),
            "{}: the way out is `rekey`: {error}",
            env.name
        );
        assert!(
            error.contains("content-purge"),
            "the way out for a lost key: {error}"
        );
        // A rekey that stopped in the middle does not open the way.
        let target = Target::Store(&env.store);
        let partial = rekey::run(
            &target,
            &env.content,
            &Options {
                batch: 2,
                stop_after_batches: Some(1),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        assert!(partial.stopped);
        let error = start_checks(&env.store, &only_new).await.unwrap_err();
        assert!(
            error.contains(&format!("{} sealed contents", count - partial.total())),
            "{}: the count follows the rows that wait: {error}",
            env.name
        );
        // The command ends: the start works without the old key.
        let done = rekey::run(&target, &env.content, &Options::default())
            .await
            .unwrap();
        assert!(done.complete("k2", None), "{done:?}");
        assert!(
            start_checks(&env.store, &only_new).await.is_ok(),
            "{}",
            env.name
        );
        assert!(only_new.rows_to_rekey(&counts(&env).await).is_empty());
    }
}

#[test]
fn the_report_says_what_moved_and_what_is_left() {
    let report = Report {
        moved: [4, 3, 2, 1],
        left: vec![("k1".into(), 0), ("k2".into(), 10)],
        check_dropped: vec!["k1".into()],
        ..Default::default()
    };
    let text = report.text("k2");
    for expected in [
        "items: 4 rows moved",
        "turns: 3 rows moved",
        "intents: 2 rows moved",
        "agent hints: 1 rows moved",
        "k1",
    ] {
        assert!(text.contains(expected), "{expected}: {text}");
    }
    assert_eq!(report.total(), 10);
    assert!(report.complete("k2", None));
    let stopped = Report {
        stopped: true,
        ..Default::default()
    };
    assert!(!stopped.complete("k2", None));
    let _ = ContentBox::kid_of(&[]);
}
