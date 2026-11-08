//! End-to-end tests of the items of the inbox (approvals and questions). The helpers are in
//! `tests/common`. Each lot that adds a test of items adds it here.

mod common;

use std::time::{Duration, Instant};

use common::approvals::{
    command, next_request, next_result, next_settled, nothing_arrives, Pep, OWNER,
};
use common::{close_code, portal, portal_ok, Conn, Phone, Recv, Server};
use inbox_core::noise::Initiator;
use inbox_core::proto::{
    Action, AnswerMsg, AnswerStatus, ClientMsg, Decision, ItemState, OpenRef, ReplyMsg, RequestMsg,
    ServerMsg, SessionHello,
};
use tokio_tungstenite::tungstenite::Message;

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

// ---------------------------------------------------------------------------------------------
// The phone side of protocol 5: the frames of the items, the answers, the sync. The service asks for an approval through
// the approval API of the PDP listener (the PEP); a Rust phone speaks Noise to the session of the portal.

const WAIT: Duration = Duration::from_secs(10);

async fn connect(pep: &Pep, phone: &Phone) -> Conn {
    let (conn, _) = Conn::open(pep.s.port, phone)
        .await
        .expect("the session opens");
    conn
}

fn answer(
    req: &RequestMsg,
    client_msg_id: &str,
    action: Action,
    shown: Option<String>,
) -> ClientMsg {
    ClientMsg::Answer(AnswerMsg {
        request_id: req.request_id.clone(),
        client_msg_id: client_msg_id.into(),
        action,
        shown_effect: shown,
        choice: None,
        text: None,
        intent: None,
        answers: None,
    })
}

/// The effect that the phone computes from what it received.
fn shown(req: &RequestMsg) -> String {
    let approval = req.approval.as_ref().expect("an approval");
    assert!(
        approval.verify(),
        "the phone computes the effect of the command it got"
    );
    approval.computed_effect().unwrap()
}

/// The first request frame: it is stored and its receipt goes back.
async fn first_request(c: &mut Conn) -> RequestMsg {
    let req = next_request(c, WAIT).await;
    c.receipt(&req.id).await;
    req
}

/// The phone approves the first request and the service uses the approval one time. A second use is refused. The
/// service learns the verdict and nothing else.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn approve_on_phone_then_pdp_true_once() {
    let pep = Pep::new();
    let phone = pep.phone("pixel").await;
    let c = command(1);
    let (id, code) = pep.submit(&c, 60);
    let mut conn = connect(&pep, &phone).await;
    let req = first_request(&mut conn).await;
    assert_eq!(req.request_id, id);
    assert_eq!((req.seq, req.chain_id.as_str()), (0, id.as_str()));
    assert_eq!(req.from.owner, OWNER);
    assert_eq!(req.from.key_name, "agent-laptop");
    assert_eq!(
        req.from.interaction,
        inbox_core::proto::Interaction::Conversational
    );
    let approval = req.approval.clone().unwrap();
    assert_eq!(approval.code, code);
    assert_eq!(approval.subject, pep.subject());
    assert_eq!(approval.effect, c.effect);
    assert_eq!(
        approval.explanation, None,
        "no agent channel: no explanation"
    );
    assert_eq!(pep.verdict(&id)["verdict"], "pending");
    assert_eq!(pep.resume(&c)["decision"], false, "no approval yet");

    conn.send(&answer(&req, "m-0001", Action::Accept, Some(shown(&req))))
        .await;
    let result = next_result(&mut conn, WAIT).await;
    assert_eq!(
        (result.status, result.state, result.client_msg_id.as_str()),
        (AnswerStatus::Accepted, Some(ItemState::Answered), "m-0001")
    );
    assert_eq!(pep.verdict(&id)["verdict"], "approved");
    // The same answer again, as the outbox of the phone sends it: it never takes a second effect.
    conn.send(&answer(&req, "m-0001", Action::Accept, Some(shown(&req))))
        .await;
    assert_eq!(
        next_result(&mut conn, WAIT).await.status,
        AnswerStatus::AlreadyAnswered
    );

    assert_eq!(pep.resume(&c)["decision"], true, "the first use");
    let second = pep.resume(&c);
    assert_eq!(second["decision"], false, "an approval gives one execution");
    assert_eq!(second["context"]["reason"], "approval_required");
}

/// A decline without a text: the service reads `denied`. A decline with a text is refused when the agent did not allow
/// it, and a `reply` is refused when the agent did not allow questions: the state does not change.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn custom_and_ask_back_are_refused_without_the_flags() {
    let pep = Pep::new();
    let phone = pep.phone("pixel").await;
    let (id, _) = pep.submit(&command(2), 60);
    let mut conn = connect(&pep, &phone).await;
    let req = first_request(&mut conn).await;

    conn.send(&ClientMsg::Answer(AnswerMsg {
        request_id: id.clone(),
        client_msg_id: "m-1".into(),
        action: Action::Decline,
        shown_effect: None,
        choice: None,
        text: Some("use staging".into()),
        intent: Some(inbox_core::proto::Intent::ChangeRequest),
        answers: None,
    }))
    .await;
    let result = next_result(&mut conn, WAIT).await;
    assert_eq!(
        (result.status, result.state),
        (AnswerStatus::CustomNotAllowed, Some(ItemState::Pending))
    );
    conn.send(&ClientMsg::Reply(ReplyMsg {
        request_id: id.clone(),
        client_msg_id: "m-2".into(),
        text: "why this database?".into(),
    }))
    .await;
    assert_eq!(
        next_result(&mut conn, WAIT).await.status,
        AnswerStatus::AskBackNotAllowed
    );
    assert_eq!(pep.verdict(&id)["verdict"], "pending", "nothing changed");

    conn.send(&answer(&req, "m-3", Action::Decline, None)).await;
    let result = next_result(&mut conn, WAIT).await;
    assert_eq!(result.status, AnswerStatus::Accepted);
    let verdict = pep.verdict(&id);
    assert_eq!(verdict["verdict"], "denied");
    assert_eq!(
        verdict.as_object().unwrap().len(),
        1,
        "the service reads the verdict only: {verdict}"
    );
}

/// The agent left: the lease ends, the phones get `settled`, and an answer that comes later gets `abandoned`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn answer_after_abandon_gets_abandoned_and_settled() {
    let pep = Pep::new();
    let phone = pep.phone("pixel").await;
    let c = command(3);
    // The shortest lease of the test file is 5 seconds: the effective lease is 15 seconds, and the grace is 5 seconds.
    let (id, _) = pep.submit(&c, 5);
    let mut conn = connect(&pep, &phone).await;
    let req = first_request(&mut conn).await;

    let settled = next_settled(&mut conn, Duration::from_secs(40)).await;
    conn.receipt(&settled.id).await;
    assert_eq!(
        (
            settled.request_id.as_str(),
            settled.state,
            settled.reason.as_str()
        ),
        (id.as_str(), ItemState::Abandoned, "lease_lapsed")
    );
    assert_eq!(settled.by, None);
    assert_eq!(pep.verdict(&id)["verdict"], "void");

    conn.send(&answer(&req, "m-late", Action::Accept, Some(shown(&req))))
        .await;
    let result = next_result(&mut conn, WAIT).await;
    assert_eq!(
        (result.status, result.state),
        (AnswerStatus::Abandoned, Some(ItemState::Abandoned))
    );
    assert_eq!(
        pep.verdict(&id)["verdict"],
        "void",
        "the late answer changed nothing"
    );
    assert_eq!(pep.resume(&c)["decision"], false);
}

/// Two phones answer at the same time: one answer wins, the other phone gets `already_answered` and `settled`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn two_phones_race_one_accepted_other_settled() {
    let pep = Pep::new();
    let (a, b) = (pep.phone("alpha").await, pep.phone("beta").await);
    let c = command(4);
    let (id, _) = pep.submit(&c, 60);
    let mut ca = connect(&pep, &a).await;
    let mut cb = connect(&pep, &b).await;
    let (ra, rb) = (first_request(&mut ca).await, first_request(&mut cb).await);
    assert_eq!(ra.request_id, rb.request_id);

    let (msg_a, msg_b) = (
        answer(&ra, "m-a", Action::Accept, Some(shown(&ra))),
        answer(&rb, "m-b", Action::Accept, Some(shown(&rb))),
    );
    tokio::join!(ca.send(&msg_a), cb.send(&msg_b));
    let (res_a, res_b) = (
        next_result(&mut ca, WAIT).await,
        next_result(&mut cb, WAIT).await,
    );
    let mut statuses = [res_a.status, res_b.status];
    statuses.sort_by_key(|s| format!("{s:?}"));
    assert_eq!(
        statuses,
        [AnswerStatus::Accepted, AnswerStatus::AlreadyAnswered]
    );
    let (winner, loser, winner_name) = if res_a.status == AnswerStatus::Accepted {
        (&mut ca, &mut cb, "alpha")
    } else {
        (&mut cb, &mut ca, "beta")
    };
    let settled = next_settled(loser, WAIT).await;
    assert_eq!(
        (settled.state, settled.decision, settled.reason.as_str()),
        (ItemState::Answered, Some(Decision::Approve), "answered")
    );
    let by = settled.by.expect("the phone that won");
    assert_eq!(by.via, inbox_core::proto::Channel::Phone);
    assert_eq!(by.device_name.as_deref(), Some(winner_name));
    nothing_arrives(winner, Duration::from_millis(1500)).await;
    assert_eq!(pep.verdict(&id)["verdict"], "approved");
    assert_eq!(pep.resume(&c)["decision"], true);
    assert_eq!(pep.resume(&c)["decision"], false);
}

/// A phone that is offline gets the request and then its `settled` when it connects. The receipts stop the sending.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn offline_phone_receives_request_and_settled_on_reconnect() {
    let pep = Pep::new();
    let (a, b) = (pep.phone("alpha").await, pep.phone("beta").await);
    let (id, _) = pep.submit(&command(5), 60);
    let mut ca = connect(&pep, &a).await;
    let req = first_request(&mut ca).await;
    ca.send(&answer(&req, "m-1", Action::Decline, None)).await;
    assert_eq!(
        next_result(&mut ca, WAIT).await.status,
        AnswerStatus::Accepted
    );
    assert_eq!(pep.verdict(&id)["verdict"], "denied");

    // The second phone never connected. Its queue holds the request and the settled, in this order.
    let mut cb = connect(&pep, &b).await;
    let mut kinds = Vec::new();
    let deadline = Instant::now() + WAIT;
    while kinds.len() < 2 && Instant::now() < deadline {
        match cb.recv(Duration::from_secs(2)).await {
            Recv::Msg(ServerMsg::Request(r)) => {
                assert_eq!(r.request_id, id);
                cb.receipt(&r.id).await;
                kinds.push("request");
            }
            Recv::Msg(ServerMsg::Settled(s)) => {
                assert_eq!(
                    (s.request_id.as_str(), s.state),
                    (id.as_str(), ItemState::Answered)
                );
                assert_eq!(s.decision, Some(Decision::Deny));
                cb.receipt(&s.id).await;
                kinds.push("settled");
            }
            _ => {}
        }
    }
    assert_eq!(kinds, ["request", "settled"]);
    // The receipts are stored: a new session gets nothing.
    drop(cb);
    let mut cb = connect(&pep, &b).await;
    nothing_arrives(&mut cb, Duration::from_secs(3)).await;
}

/// A phone that is revoked in the database while its session lives (another instance did it): its answer is refused,
/// the session ends with 4403, and the item does not change.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn revoked_device_answer_refused_4403_and_no_state_change() {
    let pep = Pep::new();
    let phone = pep.phone("lost").await;
    let c = command(6);
    let (id, _) = pep.submit(&c, 60);
    let mut conn = connect(&pep, &phone).await;
    let req = first_request(&mut conn).await;

    pep.s.revoke_in_database(&phone.device_id);
    conn.send(&answer(&req, "m-1", Action::Accept, Some(shown(&req))))
        .await;
    let result = next_result(&mut conn, WAIT).await;
    assert_eq!(result.status, AnswerStatus::DeviceRevoked);
    assert_eq!(
        conn.closed_code().await,
        Some(inbox_core::proto::close::REVOKED)
    );
    assert_eq!(
        pep.verdict(&id)["verdict"],
        "pending",
        "the answer of a revoked phone changed nothing"
    );
    assert_eq!(pep.resume(&c)["decision"], false);
    assert_eq!(
        Conn::open(pep.s.port, &phone).await.err(),
        Some(inbox_core::proto::close::REVOKED)
    );
    // Another phone of the owner can still answer.
    let other = pep.phone("spare").await;
    let mut co = connect(&pep, &other).await;
    let req = first_request(&mut co).await;
    co.send(&answer(&req, "m-2", Action::Accept, Some(shown(&req))))
        .await;
    assert_eq!(
        next_result(&mut co, WAIT).await.status,
        AnswerStatus::Accepted
    );
    assert_eq!(pep.verdict(&id)["verdict"], "approved");
}

/// A phone that shows another effect than the one of the item is refused: the state does not change.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn effect_mismatch_refused() {
    let pep = Pep::new();
    let phone = pep.phone("pixel").await;
    let (id, _) = pep.submit(&command(7), 60);
    let mut conn = connect(&pep, &phone).await;
    let req = first_request(&mut conn).await;
    for (n, wrong) in [Some(command(8).effect), None].into_iter().enumerate() {
        conn.send(&answer(&req, &format!("m-{n}"), Action::Accept, wrong))
            .await;
        let result = next_result(&mut conn, WAIT).await;
        assert_eq!(
            (result.status, result.state),
            (AnswerStatus::EffectMismatch, Some(ItemState::Pending))
        );
    }
    assert_eq!(pep.verdict(&id)["verdict"], "pending");
    conn.send(&answer(&req, "m-ok", Action::Accept, Some(shown(&req))))
        .await;
    assert_eq!(
        next_result(&mut conn, WAIT).await.status,
        AnswerStatus::Accepted
    );
}

/// `sync` brings back what the phone lacks: the open items that it does not hold, and the `settled` of the items that
/// are final or gone. An item that the phone holds in its newest state is not sent again.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sync_reconciles_open_and_closed() {
    let pep = Pep::new();
    let (a, b) = (pep.phone("alpha").await, pep.phone("beta").await);
    let (first, _) = pep.submit(&command(9), 60);
    let (second, _) = pep.submit(&command(10), 60);
    let mut ca = connect(&pep, &a).await;
    let mut cb = connect(&pep, &b).await;
    let mut held = Vec::new();
    for _ in 0..2 {
        held.push(first_request(&mut ca).await);
        let r = first_request(&mut cb).await;
        cb.receipt(&r.id).await;
    }
    // Beta declines the first one.
    let r1 = held.iter().find(|r| r.request_id == first).unwrap().clone();
    cb.send(&answer(&r1, "m-1", Action::Decline, None)).await;
    assert_eq!(
        next_result(&mut cb, WAIT).await.status,
        AnswerStatus::Accepted
    );
    let queued = next_settled(&mut ca, WAIT).await;
    assert_eq!(queued.request_id, first);

    // Alpha holds both items open and one that the server never had. Its sync learns that the first one is final, and
    // that the ghost is gone. The second one is in its newest state: it comes back as nothing.
    let ghost = "0".repeat(32);
    ca.send(&ClientMsg::Sync {
        open: vec![
            OpenRef {
                request_id: first.clone(),
                seq: 0,
            },
            OpenRef {
                request_id: second.clone(),
                seq: 0,
            },
            OpenRef {
                request_id: ghost.clone(),
                seq: 4,
            },
        ],
    })
    .await;
    let mut settled = Vec::new();
    let deadline = Instant::now() + Duration::from_secs(3);
    while Instant::now() < deadline {
        match ca.recv(Duration::from_millis(500)).await {
            Recv::Msg(ServerMsg::Settled(s)) => settled.push(s),
            Recv::Msg(ServerMsg::Request(r)) => {
                panic!("sync sent an item that the phone holds: {r:?}")
            }
            _ => {}
        }
    }
    let of_first = settled
        .iter()
        .find(|s| s.request_id == first && s.id != queued.id)
        .expect("settled by sync");
    assert_eq!(
        (of_first.state, of_first.decision),
        (ItemState::Answered, Some(Decision::Deny))
    );
    let of_ghost = settled
        .iter()
        .find(|s| s.request_id == ghost)
        .expect("the ghost is gone");
    assert_eq!(
        (of_ghost.state, of_ghost.reason.as_str()),
        (ItemState::Cancelled, "item_gone")
    );
    assert!(settled.iter().all(|s| s.request_id != second));

    // A phone that holds nothing gets the open item, with a new delivery id.
    ca.send(&ClientMsg::Sync { open: vec![] }).await;
    let again = next_request(&mut ca, WAIT).await;
    assert_eq!(again.request_id, second);
    assert!(
        held.iter().all(|h| h.id != again.id),
        "the frame has a new delivery id"
    );
    // Refresh gives the same item.
    ca.send(&ClientMsg::Refresh {
        request_id: second.clone(),
    })
    .await;
    let refreshed = next_request(&mut ca, WAIT).await;
    assert_eq!((refreshed.request_id, refreshed.seq), (second, 0));
}

/// The requests were made before the phone existed: the phone gets the open ones at its first session, and an item
/// that is final is not sent to it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn phone_enrolled_after_creation_receives_open_items() {
    let pep = Pep::new();
    // The owner has no phone: the web page can answer, so the creation works.
    let (first, _) = pep.submit(&command(11), 60);
    let (second, _) = pep.submit(&command(12), 60);
    let phone = pep.phone("late").await;
    let mut conn = connect(&pep, &phone).await;
    let r1 = first_request(&mut conn).await;
    let r2 = first_request(&mut conn).await;
    assert_eq!(
        [r1.request_id.as_str(), r2.request_id.as_str()],
        [first.as_str(), second.as_str()]
    );
    // A new session does not send them again: the receipts are stored.
    drop(conn);
    let mut conn = connect(&pep, &phone).await;
    nothing_arrives(&mut conn, Duration::from_secs(3)).await;
    // The second phone of the owner gets the same two items.
    let other = pep.phone("later").await;
    let mut co = connect(&pep, &other).await;
    let mut ids = vec![
        first_request(&mut co).await.request_id,
        first_request(&mut co).await.request_id,
    ];
    ids.sort();
    let mut want = vec![first, second];
    want.sort();
    assert_eq!(ids, want);
}

/// The old protocol gets 4426. A session of protocol 5 gets a welcome that says how long the server keeps a body.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn old_protocol_4426() {
    let pep = Pep::new();
    let phone = pep.phone("pixel").await;
    for protocol in [4u32, 6] {
        let (mut ws, _) =
            tokio_tungstenite::connect_async(format!("ws://127.0.0.1:{}/v1/session", pep.s.port))
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
        use futures_util::SinkExt;
        ws.send(Message::Binary(m1.into())).await.unwrap();
        assert_eq!(close_code(&mut ws).await, Some(4426), "protocol {protocol}");
    }
    let (_, welcome) = Conn::open(pep.s.port, &phone).await.unwrap();
    match welcome {
        ServerMsg::Welcome {
            prefs,
            body_purge_after_ms,
            ..
        } => {
            assert_eq!(
                body_purge_after_ms,
                30 * 24 * 3600 * 1000,
                "body_purge_after of the file"
            );
            assert_eq!(*prefs, Default::default());
        }
        other => panic!("{other:?}"),
    }
}

/// The preferences of the phone are stored and come back in the next welcome. The server applies none of them.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn set_prefs_is_stored_and_comes_back_in_the_welcome() {
    use inbox_core::proto::{DigestMode, DigestPrefs, Prefs, QuietPrefs};
    let pep = Pep::new();
    let phone = pep.phone("pixel").await;
    let mut conn = connect(&pep, &phone).await;
    let prefs = Prefs {
        quiet: Some(QuietPrefs {
            from: "22:00".into(),
            to: "07:00".into(),
            tz: "Europe/Paris".into(),
            high_rings: true,
        }),
        digest: Some(DigestPrefs {
            mode: DigestMode::Every,
            at: None,
            tz: None,
            every_h: Some(6),
        }),
    };
    conn.send(&ClientMsg::SetPrefs(prefs.clone())).await;
    // A later frame proves that the server handled the first one.
    conn.send(&ClientMsg::Ping { nonce: 7 }).await;
    loop {
        if let Recv::Msg(ServerMsg::Pong { .. }) = conn.recv(WAIT).await {
            break;
        }
    }
    drop(conn);
    let (_, welcome) = Conn::open(pep.s.port, &phone).await.unwrap();
    match welcome {
        ServerMsg::Welcome { prefs: stored, .. } => assert_eq!(*stored, prefs),
        other => panic!("{other:?}"),
    }
}

/// After a whole cycle the database holds no plain text of the command: the rows of `alerts` hold the metadata and the
/// fixed title, and the body is sealed.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn no_plaintext_in_alerts_table_after_cycle() {
    use serde_json::json;
    let pep = Pep::new();
    let phone = pep.phone("pixel").await;
    let canary = "canary-command-7c41e9d2-DROP-TABLE-secrets";
    let op =
        json!({"tool": "run_write_query", "host": "prod", "database": "analytics", "sql": canary});
    let effect =
        inbox_core::effect::effect_hash("db", "data.write", common::approvals::RESOURCE, &op)
            .unwrap();
    let c = common::approvals::Command {
        op,
        view: json!({"title": "Write to ClickHouse", "fields": [["Host", "/host"]], "body": "/sql"}),
        effect,
    };
    let (id, code) = pep.submit(&c, 60);
    let mut conn = connect(&pep, &phone).await;
    let req = first_request(&mut conn).await;
    assert!(
        req.approval
            .as_ref()
            .unwrap()
            .op
            .as_value()
            .to_string()
            .contains(canary),
        "the phone gets the command"
    );
    conn.send(&answer(&req, "m-1", Action::Accept, Some(shown(&req))))
        .await;
    assert_eq!(
        next_result(&mut conn, WAIT).await.status,
        AnswerStatus::Accepted
    );
    assert_eq!(pep.resume(&c)["decision"], true);
    assert_eq!(pep.verdict(&id)["verdict"], "approved");

    let rows = pep.s.database_text();
    assert!(
        !rows.contains(canary),
        "the database holds the command in clear"
    );
    assert!(
        !rows.contains(&hex_of(canary)),
        "the database holds the command in hexadecimal"
    );
    assert!(
        rows.contains(&format!("Approval {code}")),
        "the title of the row is the fixed text"
    );
    assert!(!pep.s.log().contains(canary), "the log holds the command");
}

fn hex_of(text: &str) -> String {
    text.bytes().map(|b| format!("{b:02x}")).collect()
}

/// The receipt by id stops the sending: a request without receipt comes again with the same id, and with the receipt
/// it comes no more.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ack_by_id_stops_retransmission() {
    let pep = Pep::new();
    let phone = pep.phone("pixel").await;
    let (id, _) = pep.submit(&command(13), 60);
    let mut conn = connect(&pep, &phone).await;
    let first = next_request(&mut conn, WAIT).await;
    // No receipt: the file sets `resend_after = "1s"`.
    let again = next_request(&mut conn, WAIT).await;
    assert_eq!(
        (again.id.as_str(), again.request_id.as_str()),
        (first.id.as_str(), id.as_str())
    );
    conn.receipt(&first.id).await;
    // A frame that was in flight may still arrive. After it, nothing comes.
    tokio::time::sleep(Duration::from_millis(500)).await;
    while let Recv::Msg(_) = conn.recv(Duration::from_millis(100)).await {}
    nothing_arrives(&mut conn, Duration::from_millis(3500)).await;
}

/// Two instances on one database: the phone of the owner is on the second instance, the service talks to the first.
/// Each instance flushes the queue of its own phones. Needs PostgreSQL.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn two_instances_each_flush_their_own_phone() {
    if !common::postgres_backend() || common::pg_admin().is_none() {
        eprintln!("INBOX_E2E_BACKEND=postgres is not set: skipped");
        return;
    }
    let pep = Pep::with(|_| {});
    let mut b = pep.s.sibling("instance-b");
    // The second instance has its own listener of the PDP.
    let text = std::fs::read_to_string(b.config())
        .unwrap()
        .replace(&pep.pdp_port.to_string(), &common::free_port().to_string());
    std::fs::write(b.config(), text).unwrap();
    b.start();
    let phone = pep.phone("pixel").await;
    let (mut on_b, _) = Conn::open(b.port, &phone)
        .await
        .expect("the session on the second instance");
    let c = command(14);
    let began = Instant::now();
    let (id, _) = pep.submit(&c, 60);
    let req = first_request(&mut on_b).await;
    assert_eq!(req.request_id, id);
    assert!(
        began.elapsed() < Duration::from_secs(8),
        "{:?}",
        began.elapsed()
    );
    on_b.send(&answer(&req, "m-1", Action::Accept, Some(shown(&req))))
        .await;
    assert_eq!(
        next_result(&mut on_b, WAIT).await.status,
        AnswerStatus::Accepted
    );
    assert_eq!(
        pep.verdict(&id)["verdict"],
        "approved",
        "the first instance sees the answer taken on the second"
    );
    assert_eq!(pep.resume(&c)["decision"], true);
}

/// An item goes to the phones of its owner only. A phone of another account gets nothing, and its answer is refused:
/// the item stays as it was.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_phone_of_another_account_gets_nothing_and_cannot_answer() {
    let pep = Pep::new();
    let owner_phone = pep.phone("pixel").await;
    let c = command(15);
    let (id, _) = pep.submit(&c, 60);
    let mut owner = connect(&pep, &owner_phone).await;
    let req = first_request(&mut owner).await;

    let eve = common::enroll_as(&pep.s, "eve-phone", "eve")
        .await
        .expect("a phone of another account");
    let mut conn = connect(&pep, &eve).await;
    nothing_arrives(&mut conn, Duration::from_secs(2)).await;
    conn.send(&answer(&req, "m-eve", Action::Accept, Some(shown(&req))))
        .await;
    let result = next_result(&mut conn, WAIT).await;
    assert_eq!((result.status, result.state), (AnswerStatus::Closed, None));
    conn.send(&ClientMsg::Sync { open: vec![] }).await;
    nothing_arrives(&mut conn, Duration::from_secs(2)).await;
    assert_eq!(pep.verdict(&id)["verdict"], "pending");
    assert_eq!(pep.resume(&c)["decision"], false);
}
