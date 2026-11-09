//! End-to-end tests of the items of the inbox (approvals and questions). The helpers are in
//! `tests/common`. Each lot that adds a test of items adds it here.

mod common;

use std::collections::HashSet;
use std::time::{Duration, Instant};

use common::approvals::{
    command, next_request, next_result, next_settled, nothing_arrives, nothing_new_arrives, Pep,
    OWNER,
};
use common::{close_code, portal, portal_ok, Conn, Phone, Recv, Server};
use inbox_core::noise::Initiator;
use inbox_core::proto::{
    Action, AgentDoMsg, AgentKind, AgentMsg, AgentOpKind, AgentRef, AgentResultMsg,
    AgentResultStatus, AgentState, AnswerMsg, AnswerStatus, ClientMsg, Decision, ItemState,
    OpenRef, ReplyMsg, RequestMsg, ServerMsg, SessionHello, SettledMsg,
};
use serde_json::json;
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
                cb.seen.insert(r.id.clone());
                kinds.push("request");
            }
            Recv::Msg(ServerMsg::Settled(s)) => {
                assert_eq!(
                    (s.request_id.as_str(), s.state),
                    (id.as_str(), ItemState::Answered)
                );
                assert_eq!(s.decision, Some(Decision::Deny));
                cb.receipt(&s.id).await;
                cb.seen.insert(s.id.clone());
                kinds.push("settled");
            }
            _ => {}
        }
    }
    assert_eq!(kinds, ["request", "settled"]);
    // The receipts are stored: a new session gets nothing new. A copy of the last frame is possible when its receipt was
    // lost with the dropped session.
    let seen = cb.seen.clone();
    drop(cb);
    let mut cb = connect(&pep, &b).await;
    cb.adopt_seen(&seen);
    nothing_new_arrives(&mut cb, Duration::from_secs(3)).await;
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
        agents: None,
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
    ca.send(&ClientMsg::Sync {
        open: vec![],
        agents: None,
    })
    .await;
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
    // A new session does not send them again: the receipts are stored. The receipt of the last frame can be lost when the
    // session drops, so a copy of a frame that the first session read is not a failure.
    let seen = conn.seen.clone();
    drop(conn);
    let mut conn = connect(&pep, &phone).await;
    conn.adopt_seen(&seen);
    nothing_new_arrives(&mut conn, Duration::from_secs(3)).await;
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
    conn.send(&ClientMsg::Sync {
        open: vec![],
        agents: None,
    })
    .await;
    nothing_arrives(&mut conn, Duration::from_secs(2)).await;
    assert_eq!(pep.verdict(&id)["verdict"], "pending");
    assert_eq!(pep.resume(&c)["decision"], false);
}

// ---------------------------------------------------------------------------------------------
// The agents on the phone (lot G2). The agent channel makes the agents (lot G1b), so these tests write the rows of the
// agents in the database of the server, as another instance would, and let the server do the rest: the line "from" of a
// request, the frames `agent`, `sync`, `agent_do` and the lock of a new agent. The in-process tests of the portal run the
// whole life of an agent from its first call on both databases (`http/agent_frames_tests.rs`).

const A_NEW: &str = "d4d4d4d4d4d4d4d4d4d4d4d4d4d4d4d4";
const A_KNOWN: &str = "a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1";
const A_FOREIGN: &str = "f6f6f6f6f6f6f6f6f6f6f6f6f6f6f6f6";
const NOBODY: &str = "00000000000000000000000000000000";

/// A server with the block `inbox.agents`. `lock` is the value of `inbox.agents.lock_new`.
fn with_agents(lock: bool) -> Pep {
    Pep::with(|s| {
        s.add_to_inbox(&format!(
            "agents {{ max_per_key = 8 new_per_hour = 20 lock_new = {lock} }}"
        ))
    })
}

/// Runs SQL in the database of the server, as `revoke_in_database` does.
fn sql(s: &common::Server, statements: &str) {
    match &s.pg {
        Some((admin, database)) => admin.exec(database, statements.to_string()),
        None => {
            let c = rusqlite::Connection::open(s.dir.join("portal.db")).unwrap();
            c.busy_timeout(Duration::from_secs(10)).unwrap();
            c.execute_batch(statements).unwrap();
        }
    }
}

/// A row of `agents` for `key_id`: its state, the number that the key gave it and the name that the owner gave it.
fn seed_agent(pep: &Pep, id: &str, key_id: &str, state: &str, ordinal: i64, name: Option<&str>) {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis();
    let (name_sql, key_sql, named_at) = match name {
        Some(n) => (
            format!("'{n}'"),
            format!("'{}'", n.to_lowercase()),
            now.to_string(),
        ),
        None => ("NULL".into(), "NULL".into(), "NULL".into()),
    };
    sql(
        &pep.s,
        &format!(
            "INSERT INTO agents (agent_id, key_id, owner, kind, state, origin, ordinal, name, name_key, token_digest,
                                 merged_into, rev, created_at, named_at, last_seen_at, hints_kid, hints_ct)
             VALUES ('{id}', '{key_id}', '{OWNER}', 'agent', '{state}', 'call', {ordinal}, {name_sql}, {key_sql}, '{id}{id}',
                     NULL, 1, {now}, {named_at}, {now}, NULL, NULL)"
        ),
    );
}

/// The item of `request_id` belongs to `agent` now.
fn give_item(pep: &Pep, request_id: &str, agent: &str) {
    sql(
        &pep.s,
        &format!("UPDATE items SET agent_id = '{agent}' WHERE request_id = '{request_id}'"),
    );
}

/// What a phone reads in a time: the frames of the agents and the frames that come with them. The phone sends the receipt
/// of each frame that came from the queue.
#[derive(Default)]
struct Read {
    agents: Vec<AgentMsg>,
    results: Vec<AgentResultMsg>,
    requests: Vec<RequestMsg>,
    settled: Vec<SettledMsg>,
    answers: Vec<inbox_core::proto::AnswerResultMsg>,
}

/// Reads until nothing comes for `quiet`.
async fn gather(c: &mut Conn, quiet: Duration) -> Read {
    let mut read = Read::default();
    // The server sends a frame again until it gets the receipt. A copy has the id of the first delivery: it is one frame.
    let mut seen = HashSet::new();
    loop {
        match c.recv(quiet).await {
            Recv::Msg(ServerMsg::Agent(a)) => {
                c.receipt(&a.id).await;
                if seen.insert(a.id.clone()) {
                    read.agents.push(*a);
                }
            }
            Recv::Msg(ServerMsg::AgentResult(r)) => read.results.push(r),
            Recv::Msg(ServerMsg::Request(r)) => {
                c.receipt(&r.id).await;
                if seen.insert(r.id.clone()) {
                    read.requests.push(*r);
                }
            }
            Recv::Msg(ServerMsg::Settled(s)) => {
                c.receipt(&s.id).await;
                if seen.insert(s.id.clone()) {
                    read.settled.push(s);
                }
            }
            Recv::Msg(ServerMsg::AnswerResult(r)) => read.answers.push(r),
            Recv::Msg(_) => {}
            Recv::Closed(code) => panic!("closed while the phone reads: {code:?}"),
            Recv::Timeout => return read,
        }
    }
}

const QUIET: Duration = Duration::from_millis(1200);

fn agent_do(
    agent: &str,
    id: &str,
    op: AgentOpKind,
    name: Option<&str>,
    into: Option<&str>,
) -> ClientMsg {
    ClientMsg::AgentDo(AgentDoMsg {
        agent_id: agent.into(),
        client_msg_id: id.into(),
        op,
        name: name.map(str::to_string),
        into: into.map(str::to_string),
    })
}

/// The phone sends `agent_do` and reads the result. The other frames that come with it are the result of the change.
async fn act(
    c: &mut Conn,
    agent: &str,
    id: &str,
    op: AgentOpKind,
    name: Option<&str>,
    into: Option<&str>,
) -> (AgentResultStatus, Read) {
    c.send(&agent_do(agent, id, op, name, into)).await;
    let mut read = gather(c, QUIET).await;
    assert_eq!(read.results.len(), 1, "one result for one action");
    let result = read.results.remove(0);
    assert_eq!(
        (result.client_msg_id.as_str(), result.agent_id.as_str()),
        (id, agent)
    );
    (result.status, read)
}

fn by_id<'a>(agents: &'a [AgentMsg], id: &str) -> &'a AgentMsg {
    agents.iter().find(|a| a.agent_id == id).unwrap_or_else(|| {
        panic!(
            "no frame for the agent {id}: {:?}",
            agents.iter().map(|a| &a.agent_id).collect::<Vec<_>>()
        )
    })
}

/// The line "from" of a request names the agent in portal text, and `sync` brings the agents that the phone lacks.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn requests_name_their_agent_and_sync_brings_the_agents_the_phone_lacks() {
    let pep = with_agents(true);
    let phone = pep.phone("pixel").await;
    let (first, _) = pep.submit(&command(21), 60);
    let (second, _) = pep.submit(&command(22), 60);
    let (third, _) = pep.submit(&command(23), 60);
    seed_agent(&pep, A_NEW, &pep.key_id, "new", 1, None);
    seed_agent(&pep, A_KNOWN, &pep.key_id, "known", 2, Some("Night ETL"));
    give_item(&pep, &first, A_NEW);
    give_item(&pep, &second, A_KNOWN);

    let mut conn = connect(&pep, &phone).await;
    let read = gather(&mut conn, QUIET).await;
    assert!(
        read.agents.is_empty(),
        "the server sends no agent before the sync of the phone"
    );
    let from = |id: &str| {
        &read
            .requests
            .iter()
            .find(|r| r.request_id == id)
            .expect("a request")
            .from
    };
    let f = from(&first);
    assert_eq!(
        (
            f.label.as_str(),
            f.agent_id.as_str(),
            f.agent_kind,
            f.agent_state
        ),
        ("agent-laptop #1", A_NEW, AgentKind::Agent, AgentState::New)
    );
    assert_eq!(f.key_name, "agent-laptop");
    let f = from(&second);
    assert_eq!(
        (f.label.as_str(), f.agent_id.as_str(), f.agent_state),
        ("Night ETL", A_KNOWN, AgentState::Known)
    );
    let bucket = from(&third);
    assert_eq!(
        (bucket.label.as_str(), bucket.agent_kind, bucket.agent_state),
        (
            "Unattributed · agent-laptop",
            AgentKind::Bucket,
            AgentState::Known
        ),
        "the bucket of a key in the mode discover is named after the key"
    );
    let bucket_id = bucket.agent_id.clone();
    let open: Vec<OpenRef> = read
        .requests
        .iter()
        .map(|r| OpenRef {
            request_id: r.request_id.clone(),
            seq: r.seq,
        })
        .collect();

    // A sync from a phone that holds no agent brings the three, and no frame twice.
    conn.send(&ClientMsg::Sync {
        open: open.clone(),
        agents: Some(vec![]),
    })
    .await;
    let read = gather(&mut conn, QUIET).await;
    assert_eq!(read.agents.len(), 3, "{:?}", read.agents);
    let new = by_id(&read.agents, A_NEW);
    assert_eq!(
        (
            new.kind,
            new.state,
            new.label.as_str(),
            new.named,
            new.lock_accept,
            new.open,
            new.rev
        ),
        (
            AgentKind::Agent,
            AgentState::New,
            "agent-laptop #1",
            false,
            true,
            1,
            1
        )
    );
    assert_eq!(new.key_name, "agent-laptop");
    assert!(new.observed.is_none(), "the row holds no hint");
    let known = by_id(&read.agents, A_KNOWN);
    assert_eq!(
        (
            known.state,
            known.label.as_str(),
            known.named,
            known.lock_accept,
            known.open
        ),
        (AgentState::Known, "Night ETL", true, false, 1)
    );
    let b = by_id(&read.agents, &bucket_id);
    assert_eq!(
        (
            b.kind,
            b.state,
            b.label.as_str(),
            b.named,
            b.lock_accept,
            b.open
        ),
        (
            AgentKind::Bucket,
            AgentState::Known,
            "Unattributed · agent-laptop",
            false,
            false,
            1
        )
    );

    // The phone holds two of them in their newest rev: it gets the third one only. With the three: none.
    let held = |ids: &[&str]| -> Vec<AgentRef> {
        ids.iter()
            .map(|id| AgentRef {
                agent_id: id.to_string(),
                rev: 1,
            })
            .collect()
    };
    conn.send(&ClientMsg::Sync {
        open: open.clone(),
        agents: Some(held(&[A_NEW, A_KNOWN])),
    })
    .await;
    let read = gather(&mut conn, QUIET).await;
    assert_eq!(
        read.agents
            .iter()
            .map(|a| a.agent_id.clone())
            .collect::<Vec<_>>(),
        vec![bucket_id.clone()]
    );
    conn.send(&ClientMsg::Sync {
        open: open.clone(),
        agents: Some(held(&[A_NEW, A_KNOWN, &bucket_id])),
    })
    .await;
    assert!(gather(&mut conn, QUIET).await.agents.is_empty());
    // A phone that holds an older rev than the server's gets the agent: the row moves, the phone converges.
    sql(
        &pep.s,
        &format!("UPDATE agents SET rev = 4 WHERE agent_id = '{A_KNOWN}'"),
    );
    conn.send(&ClientMsg::Sync {
        open,
        agents: Some(held(&[A_NEW, A_KNOWN, &bucket_id])),
    })
    .await;
    let read = gather(&mut conn, QUIET).await;
    assert_eq!(read.agents.len(), 1);
    assert_eq!(
        (read.agents[0].agent_id.as_str(), read.agents[0].rev),
        (A_KNOWN, 4)
    );
}

/// A server without the block `inbox.agents`: every key is one agent. The request names the bucket, `sync` brings no
/// agent, and `agent_do` says `not_enabled`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn without_the_block_agents_the_request_names_the_bucket_and_agent_do_is_not_enabled() {
    let pep = Pep::new();
    let phone = pep.phone("pixel").await;
    let (id, _) = pep.submit(&command(24), 60);
    let mut conn = connect(&pep, &phone).await;
    let req = first_request(&mut conn).await;
    assert_eq!(req.request_id, id);
    assert_eq!(
        (
            req.from.label.as_str(),
            req.from.agent_kind,
            req.from.agent_state
        ),
        ("agent-laptop", AgentKind::Bucket, AgentState::Known)
    );
    assert_eq!(req.agent_label, None);
    conn.send(&ClientMsg::Sync {
        open: vec![],
        agents: Some(vec![]),
    })
    .await;
    assert!(gather(&mut conn, QUIET).await.agents.is_empty());
    let (status, read) = act(
        &mut conn,
        &req.from.agent_id,
        "m-1",
        AgentOpKind::Block,
        None,
        None,
    )
    .await;
    assert_eq!(status, AgentResultStatus::NotEnabled);
    assert!(read.agents.is_empty());
    assert_eq!(
        pep.verdict(&id)["verdict"],
        "pending",
        "nothing was cancelled"
    );
}

/// `agent_do`: each action changes the agent, every phone of the owner gets the new frame, a phone of another account
/// gets nothing and changes nothing, and each refusal is a status of the result that leaves the agent as it was.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn agent_do_changes_the_agent_for_every_phone_of_the_owner_and_each_refusal_changes_nothing()
{
    let pep = with_agents(true);
    let phone_a = pep.phone("pixel").await;
    let phone_b = pep.phone("second").await;
    let (first, _) = pep.submit(&command(31), 60);
    let (third, _) = pep.submit(&command(33), 60);
    seed_agent(&pep, A_NEW, &pep.key_id, "new", 1, None);
    seed_agent(&pep, A_KNOWN, &pep.key_id, "known", 2, Some("Night ETL"));
    seed_agent(&pep, A_FOREIGN, "another-key", "known", 1, None);
    give_item(&pep, &first, A_NEW);
    let mut a = connect(&pep, &phone_a).await;
    let mut b = connect(&pep, &phone_b).await;
    let eve_phone = common::enroll_as(&pep.s, "eve-phone", "eve")
        .await
        .expect("a phone of another account");
    let mut eve = connect(&pep, &eve_phone).await;
    let read = gather(&mut a, QUIET).await;
    let bucket_id = read
        .requests
        .iter()
        .find(|r| r.request_id == third)
        .unwrap()
        .from
        .agent_id
        .clone();
    gather(&mut b, QUIET).await;
    assert!(
        gather(&mut eve, QUIET).await.requests.is_empty(),
        "no request for another account"
    );

    // Allow, with a name. Both phones of the owner get the frame; the phone of another account gets nothing.
    let (status, read) = act(
        &mut a,
        A_NEW,
        "m-1",
        AgentOpKind::Allow,
        Some("Refund cleaner"),
        None,
    )
    .await;
    assert_eq!(status, AgentResultStatus::Ok);
    assert_eq!(read.agents.len(), 1, "the acting phone gets the frame too");
    let on_a = &read.agents[0];
    assert_eq!(
        (
            on_a.agent_id.as_str(),
            on_a.state,
            on_a.named,
            on_a.label.as_str(),
            on_a.lock_accept,
            on_a.open
        ),
        (A_NEW, AgentState::Known, true, "Refund cleaner", false, 1)
    );
    assert!(on_a.rev > 1);
    let read = gather(&mut b, QUIET).await;
    assert_eq!(read.agents.len(), 1, "the other phone of the owner");
    assert_eq!(
        (
            read.agents[0].agent_id.as_str(),
            read.agents[0].rev,
            read.agents[0].id.clone()
        ),
        (A_NEW, on_a.rev, on_a.id.clone())
    );
    let read = gather(&mut eve, QUIET).await;
    assert!(
        read.agents.is_empty() && read.results.is_empty(),
        "nothing for another account"
    );

    // The same action again, as the outbox of the phone sends it: a success, and no new frame.
    let (status, read) = act(
        &mut a,
        A_NEW,
        "m-1",
        AgentOpKind::Allow,
        Some("Refund cleaner"),
        None,
    )
    .await;
    assert_eq!(status, AgentResultStatus::Ok);
    assert!(
        read.agents.is_empty(),
        "an action that changed nothing sends nothing"
    );

    // Refusals: each one is a status, and none of them changes the agent.
    let before = agents_as_seen_by(&mut a).await;
    for (agent, op, name, into, want) in [
        (
            A_KNOWN,
            AgentOpKind::Rename,
            Some("a\nb"),
            None,
            AgentResultStatus::InvalidName,
        ),
        (
            A_KNOWN,
            AgentOpKind::Rename,
            Some("a\u{202e}b"),
            None,
            AgentResultStatus::InvalidName,
        ),
        (
            A_KNOWN,
            AgentOpKind::Rename,
            Some(" padded"),
            None,
            AgentResultStatus::InvalidName,
        ),
        (
            A_KNOWN,
            AgentOpKind::Rename,
            Some("REFUND  cleaner"),
            None,
            AgentResultStatus::NameTaken,
        ),
        (
            A_NEW,
            AgentOpKind::Rename,
            Some("night etl"),
            None,
            AgentResultStatus::NameTaken,
        ),
        (
            A_NEW,
            AgentOpKind::Merge,
            None,
            Some(A_NEW),
            AgentResultStatus::BadState,
        ),
        (
            A_NEW,
            AgentOpKind::Merge,
            None,
            Some(A_FOREIGN),
            AgentResultStatus::CrossKey,
        ),
        (
            A_NEW,
            AgentOpKind::Merge,
            None,
            Some(NOBODY),
            AgentResultStatus::NotFound,
        ),
        (
            A_NEW,
            AgentOpKind::Unblock,
            None,
            None,
            AgentResultStatus::Ok,
        ),
        (
            NOBODY,
            AgentOpKind::Block,
            None,
            None,
            AgentResultStatus::NotFound,
        ),
        (
            NOBODY,
            AgentOpKind::Rename,
            Some("x"),
            None,
            AgentResultStatus::NotFound,
        ),
        (
            &bucket_id,
            AgentOpKind::Block,
            None,
            None,
            AgentResultStatus::BadState,
        ),
        (
            &bucket_id,
            AgentOpKind::Rename,
            Some("x"),
            None,
            AgentResultStatus::BadState,
        ),
        (
            &bucket_id,
            AgentOpKind::Allow,
            None,
            None,
            AgentResultStatus::BadState,
        ),
        (
            &bucket_id,
            AgentOpKind::Merge,
            None,
            Some(A_NEW),
            AgentResultStatus::BadState,
        ),
    ] {
        let (status, read) = act(&mut a, agent, "m-2", op, name, into).await;
        assert_eq!(status, want, "{op:?} {name:?} {into:?} on {agent}");
        assert!(read.agents.is_empty(), "{op:?} on {agent} sent a frame");
    }
    assert_eq!(
        agents_as_seen_by(&mut a).await,
        before,
        "the refusals changed nothing"
    );

    // Another account: the agent is not its own. It gets `not_found` for each action, as for an agent that does not exist.
    for (op, name) in [
        (AgentOpKind::Rename, Some("Mine")),
        (AgentOpKind::Block, None),
        (AgentOpKind::Allow, None),
    ] {
        let (status, read) = act(&mut eve, A_NEW, "m-eve", op, name, None).await;
        assert_eq!(status, AgentResultStatus::NotFound, "{op:?}");
        assert!(read.agents.is_empty());
    }
    let (status, _) = act(
        &mut eve,
        A_NEW,
        "m-eve2",
        AgentOpKind::Merge,
        None,
        Some(A_KNOWN),
    )
    .await;
    assert_eq!(status, AgentResultStatus::NotFound);
    eve.send(&ClientMsg::Sync {
        open: vec![],
        agents: Some(vec![]),
    })
    .await;
    assert!(
        gather(&mut eve, QUIET).await.agents.is_empty(),
        "no agent of another owner in a sync"
    );
    assert_eq!(
        agents_as_seen_by(&mut a).await,
        before,
        "the other account changed nothing"
    );

    // Block: the open item is cancelled with the reason `agent_blocked`, and the service reads `void`.
    let (status, read) = act(&mut a, A_NEW, "m-3", AgentOpKind::Block, None, None).await;
    assert_eq!(status, AgentResultStatus::Ok);
    assert_eq!(by_id(&read.agents, A_NEW).state, AgentState::Blocked);
    assert_eq!(by_id(&read.agents, A_NEW).open, 0);
    let s = read
        .settled
        .iter()
        .find(|s| s.request_id == first)
        .expect("the item is settled");
    assert_eq!(
        (s.state, s.reason.as_str()),
        (ItemState::Cancelled, "agent_blocked")
    );
    assert_eq!(pep.verdict(&first)["verdict"], "void");
    let read = gather(&mut b, QUIET).await;
    assert_eq!(by_id(&read.agents, A_NEW).state, AgentState::Blocked);
    assert!(read.settled.iter().any(|s| s.request_id == first));
    // Allow on a blocked agent: a bad state. Unblock: known again, and nothing comes back to life.
    let (status, _) = act(&mut a, A_NEW, "m-4", AgentOpKind::Allow, None, None).await;
    assert_eq!(status, AgentResultStatus::BadState);
    let (status, read) = act(&mut a, A_NEW, "m-5", AgentOpKind::Unblock, None, None).await;
    assert_eq!(status, AgentResultStatus::Ok);
    assert_eq!(
        (
            by_id(&read.agents, A_NEW).state,
            by_id(&read.agents, A_NEW).open
        ),
        (AgentState::Known, 0)
    );
    assert_eq!(pep.verdict(&first)["verdict"], "void");
    // Merge into the other agent of the key: the agent is `merged`, the target gets a frame too.
    let (status, read) = act(
        &mut a,
        A_NEW,
        "m-6",
        AgentOpKind::Merge,
        None,
        Some(A_KNOWN),
    )
    .await;
    assert_eq!(status, AgentResultStatus::Ok);
    let merged = by_id(&read.agents, A_NEW);
    assert_eq!(
        (merged.state, merged.merged_into.as_deref()),
        (AgentState::Merged, Some(A_KNOWN))
    );
    assert_eq!(by_id(&read.agents, A_KNOWN).state, AgentState::Known);
    // The same merge again: a success that sends nothing.
    let (status, read) = act(
        &mut a,
        A_NEW,
        "m-6",
        AgentOpKind::Merge,
        None,
        Some(A_KNOWN),
    )
    .await;
    assert_eq!(status, AgentResultStatus::Ok);
    assert!(read.agents.is_empty());
}

/// What a phone that holds nothing learns about the agents of its owner, by agent.
async fn agents_as_seen_by(c: &mut Conn) -> Vec<(String, u64, &'static str, String)> {
    c.send(&ClientMsg::Sync {
        open: vec![],
        agents: Some(vec![]),
    })
    .await;
    let mut seen: Vec<_> = gather(c, QUIET)
        .await
        .agents
        .into_iter()
        .map(|a| (a.agent_id, a.rev, a.state.as_str(), a.label))
        .collect();
    seen.sort();
    seen
}

/// A malformed `agent_do` is a protocol error: the session closes with 4426, and the agent is as it was.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_malformed_agent_do_closes_the_session() {
    let pep = with_agents(true);
    let phone = pep.phone("pixel").await;
    pep.submit(&command(41), 60);
    seed_agent(&pep, A_NEW, &pep.key_id, "new", 1, None);
    for frame in [
        // A rename with no name.
        format!(r#"{{"t":"agent_do","agent_id":"{A_NEW}","client_msg_id":"m-1","op":"rename"}}"#),
        // A block with a name.
        format!(
            r#"{{"t":"agent_do","agent_id":"{A_NEW}","client_msg_id":"m-1","op":"block","name":"x"}}"#
        ),
        // An op that does not exist, and a member that does not exist.
        format!(r#"{{"t":"agent_do","agent_id":"{A_NEW}","client_msg_id":"m-1","op":"delete"}}"#),
        format!(
            r#"{{"t":"agent_do","agent_id":"{A_NEW}","client_msg_id":"m-1","op":"block","force":true}}"#
        ),
        // A name of 33 characters.
        format!(
            r#"{{"t":"agent_do","agent_id":"{A_NEW}","client_msg_id":"m-1","op":"rename","name":"{}"}}"#,
            "n".repeat(33)
        ),
        // A sync with an agent that has no rev.
        format!(r#"{{"t":"sync","open":[],"agents":[{{"agent_id":"{A_NEW}","rev":0}}]}}"#),
    ] {
        let mut conn = connect(&pep, &phone).await;
        conn.send_raw(&frame).await;
        assert_eq!(conn.closed_code().await, Some(4426), "{frame}");
    }
    let mut conn = connect(&pep, &phone).await;
    let (status, _) = act(&mut conn, A_NEW, "m-9", AgentOpKind::Allow, None, None).await;
    assert_eq!(
        status,
        AgentResultStatus::Ok,
        "the session and the agent work after the refusals"
    );
}

/// The lock of a new agent. With `lock_new = true` the server refuses `accept` for the item of a new agent with the status
/// `agent_locked`, and the item stays open. A decline works. An allow unlocks it. With `lock_new = false` (the default
/// of the file) the owner can approve at once, and the frame of the agent says so.
async fn lock_cycle(lock: bool) {
    let pep = with_agents(lock);
    let phone = pep.phone("pixel").await;
    let (locked, _) = pep.submit(&command(51), 60);
    let (declined, _) = pep.submit(&command(52), 60);
    let (after_allow, _) = pep.submit(&command(53), 60);
    let (of_known, c_known) = (pep.submit(&command(54), 60).0, command(54));
    seed_agent(&pep, A_NEW, &pep.key_id, "new", 1, None);
    seed_agent(&pep, A_KNOWN, &pep.key_id, "known", 2, Some("Night ETL"));
    for id in [&locked, &declined, &after_allow] {
        give_item(&pep, id, A_NEW);
    }
    give_item(&pep, &of_known, A_KNOWN);
    let mut conn = connect(&pep, &phone).await;
    let read = gather(&mut conn, QUIET).await;
    let req = |id: &str| {
        read.requests
            .iter()
            .find(|r| r.request_id == id)
            .unwrap()
            .clone()
    };
    conn.send(&ClientMsg::Sync {
        open: vec![],
        agents: Some(vec![]),
    })
    .await;
    let frames = gather(&mut conn, QUIET).await;
    let new = by_id(&frames.agents, A_NEW);
    assert_eq!(
        (new.state, new.lock_accept),
        (AgentState::New, lock),
        "the frame says what the server does"
    );

    // Accept on the item of a new agent.
    let r = req(&locked);
    conn.send(&answer(&r, "m-a", Action::Accept, Some(shown(&r))))
        .await;
    let result = next_result(&mut conn, WAIT).await;
    if lock {
        assert_eq!(
            (result.status, result.state, result.client_msg_id.as_str()),
            (AnswerStatus::AgentLocked, Some(ItemState::Pending), "m-a")
        );
        assert_eq!(
            pep.verdict(&locked)["verdict"],
            "pending",
            "the item stays open"
        );
        assert_eq!(pep.resume(&command(51))["decision"], false);
        // The same answer again, as the outbox sends it: the same status.
        conn.send(&answer(&r, "m-a", Action::Accept, Some(shown(&r))))
            .await;
        assert_eq!(
            next_result(&mut conn, WAIT).await.status,
            AnswerStatus::AgentLocked
        );
    } else {
        assert_eq!(
            (result.status, result.state),
            (AnswerStatus::Accepted, Some(ItemState::Answered))
        );
        assert_eq!(pep.verdict(&locked)["verdict"], "approved");
        assert_eq!(pep.resume(&command(51))["decision"], true);
    }

    // A decline works for a new agent, locked or not.
    let r = req(&declined);
    conn.send(&answer(&r, "m-d", Action::Decline, None)).await;
    assert_eq!(
        next_result(&mut conn, WAIT).await.status,
        AnswerStatus::Accepted
    );
    assert_eq!(pep.verdict(&declined)["verdict"], "denied");

    // The item of a known agent is not locked.
    let r = req(&of_known);
    conn.send(&answer(&r, "m-k", Action::Accept, Some(shown(&r))))
        .await;
    assert_eq!(
        next_result(&mut conn, WAIT).await.status,
        AnswerStatus::Accepted
    );
    assert_eq!(pep.verdict(&of_known)["verdict"], "approved");
    assert_eq!(pep.resume(&c_known)["decision"], true);

    // The owner allows the agent: the lock is gone, and the item can be approved.
    let (status, read) = act(&mut conn, A_NEW, "m-allow", AgentOpKind::Allow, None, None).await;
    assert_eq!(status, AgentResultStatus::Ok);
    assert_eq!(
        (
            by_id(&read.agents, A_NEW).state,
            by_id(&read.agents, A_NEW).lock_accept
        ),
        (AgentState::Known, false)
    );
    let r = req(&after_allow);
    conn.send(&answer(&r, "m-b", Action::Accept, Some(shown(&r))))
        .await;
    assert_eq!(
        next_result(&mut conn, WAIT).await.status,
        AnswerStatus::Accepted
    );
    assert_eq!(pep.verdict(&after_allow)["verdict"], "approved");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_new_agent_is_locked_for_accept_until_the_owner_allows_it() {
    lock_cycle(true).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_new_agent_is_not_locked_when_lock_new_is_false() {
    lock_cycle(false).await;
}

/// A phone of another account that sends the answer of an item of a new agent gets the same status as for any item that is
/// not its own: `closed`. The lock status tells nothing about the items of another owner.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_phone_of_another_account_learns_nothing_of_the_lock() {
    let pep = with_agents(true);
    let owner_phone = pep.phone("pixel").await;
    let (id, _) = pep.submit(&command(61), 60);
    seed_agent(&pep, A_NEW, &pep.key_id, "new", 1, None);
    give_item(&pep, &id, A_NEW);
    let mut owner = connect(&pep, &owner_phone).await;
    let req = first_request(&mut owner).await;
    let eve_phone = common::enroll_as(&pep.s, "eve-phone", "eve")
        .await
        .expect("a phone of another account");
    let mut eve = connect(&pep, &eve_phone).await;
    eve.send(&answer(&req, "m-eve", Action::Accept, Some(shown(&req))))
        .await;
    let result = next_result(&mut eve, WAIT).await;
    assert_eq!((result.status, result.state), (AnswerStatus::Closed, None));
}

// ---------------------------------------------------------------------------------------------
// The agents through the real channel (lot G5). No SQL writes an agent here: two agents of one key call `/mcp` with the
// token that the portal gave them, and the Rust phones of the owner read the frames and act with `agent_do`.

/// The `User-Agent` of the first agent and of the second agent: the client that the owner reads.
const UA_X: &str = "agent-x/1.0";
const UA_Y: &str = "agent-y/2.0";

/// The id of the agent of a request, as the engine wrote it.
fn agent_of(pep: &Pep, request: &str) -> String {
    pep.s
        .scalar(&format!(
            "SELECT agent_id FROM items WHERE request_id = '{request}'"
        ))
        .expect("the request has an agent")
}

fn row_of(read: &Read, request: &str) -> RequestMsg {
    read.requests
        .iter()
        .find(|r| r.request_id == request)
        .unwrap_or_else(|| panic!("no request frame for {request}"))
        .clone()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn two_agents_of_one_key_with_real_tokens_reach_the_phones_and_agent_do_changes_them() {
    let pep = with_agents(true);
    let phone_a = pep.phone("pixel").await;
    let phone_b = pep.phone("second").await;
    let mut a = connect(&pep, &phone_a).await;
    let mut b = connect(&pep, &phone_b).await;

    // Two agents of one key make a request with no token: the portal makes two agents and gives each one its token.
    let x = pep.agent_request(
        None,
        UA_X,
        &command(61),
        json!({"agent_label": "billing-bot"}),
    );
    let y = pep.agent_request(
        None,
        UA_Y,
        &command(62),
        json!({"agent_label": "night-etl"}),
    );
    assert_eq!((x.status.as_str(), y.status.as_str()), ("new", "new"));
    let (tok_x, tok_y) = (
        x.token.clone().expect("token"),
        y.token.clone().expect("token"),
    );
    assert_ne!(tok_x, tok_y);
    let (ax, ay) = (agent_of(&pep, &x.request_id), agent_of(&pep, &y.request_id));
    assert_ne!(ax, ay);

    // Each phone gets the two requests and the two frames `agent`, with the same ids and the same text of the portal.
    let mut ids = Vec::new();
    for conn in [&mut a, &mut b] {
        let read = gather(conn, QUIET).await;
        assert_eq!(
            read.requests.len(),
            2,
            "two requests: {:?}",
            read.requests
                .iter()
                .map(|r| (r.id.clone(), r.request_id.clone(), r.seq))
                .collect::<Vec<_>>()
        );
        assert_eq!(read.agents.len(), 2, "two frames of agents");
        let (rx, ry) = (row_of(&read, &x.request_id), row_of(&read, &y.request_id));
        assert_eq!(
            (rx.from.agent_id.as_str(), ry.from.agent_id.as_str()),
            (ax.as_str(), ay.as_str())
        );
        assert_eq!(
            (rx.from.label.as_str(), ry.from.label.as_str()),
            ("agent-laptop #1", "agent-laptop #2"),
            "the label is portal text: the number, never what the agent says"
        );
        assert_eq!(
            (rx.from.agent_state, ry.from.agent_state),
            (AgentState::New, AgentState::New)
        );
        // What the agent says is outside `from`, and it is the text that the channel got.
        assert_eq!(rx.agent_label.as_deref(), Some("billing-bot"));
        assert_eq!(ry.agent_label.as_deref(), Some("night-etl"));
        let (fx, fy) = (by_id(&read.agents, &ax), by_id(&read.agents, &ay));
        assert_eq!(
            (fx.state, fx.lock_accept, fx.named),
            (AgentState::New, true, false)
        );
        let seen = fx.observed.as_ref().expect("the hints of the agent");
        assert_eq!(
            (seen.says.as_deref(), seen.client.as_deref(), seen.nets_seen),
            (Some("billing-bot"), Some(UA_X), 1)
        );
        assert_eq!(
            fy.observed.as_ref().and_then(|o| o.says.as_deref()),
            Some("night-etl")
        );
        assert!(
            fx.similar.is_none() && fy.similar.is_none(),
            "no known agent yet"
        );
        ids.push((fx.id.clone(), fx.rev));
    }

    // The agent X asks again with its token: the same agent. No new agent and no new frame of an agent.
    let again = pep.agent_request(
        Some(&tok_x),
        UA_X,
        &command(63),
        json!({"agent_label": "billing-bot"}),
    );
    assert_eq!(
        (again.status.as_str(), again.token.is_none()),
        ("new", true)
    );
    assert_eq!(agent_of(&pep, &again.request_id), ax);
    for conn in [&mut a, &mut b] {
        let read = gather(conn, QUIET).await;
        assert_eq!(read.requests.len(), 1);
        assert_eq!(row_of(&read, &again.request_id).from.agent_id, ax);
        assert!(
            read.agents.is_empty(),
            "a call of a known token changes no agent"
        );
    }
    assert_eq!(
        pep.s
            .scalar("SELECT COUNT(*) FROM agents WHERE kind = 'agent'")
            .as_deref(),
        Some("2")
    );

    // The agent Y asks for the effect that X asked: the same request comes back (the PEP rule) and the item keeps X.
    let also = pep.agent_request(Some(&tok_y), UA_Y, &command(61), json!({}));
    assert_eq!(
        also.request_id, x.request_id,
        "the same effect gives the open request"
    );
    assert_eq!(
        agent_of(&pep, &x.request_id),
        ax,
        "the item keeps its first agent"
    );
    assert_eq!(
        pep.s
            .scalar(&format!(
                "SELECT COUNT(*) FROM item_events WHERE request_id = '{}' AND event = 'also_requested'",
                x.request_id
            ))
            .as_deref(),
        Some("1")
    );
    for conn in [&mut a, &mut b] {
        let read = gather(conn, QUIET).await;
        assert!(
            read.requests.is_empty(),
            "no second request frame for the same effect"
        );
    }

    // The lock: a new agent cannot get an approval. The phone gets `agent_locked`, and the item stays open.
    let rx = {
        let read = gather(&mut a, QUIET).await;
        assert!(read.requests.is_empty());
        // The phone holds the request frames of the first step: ask the server again with `sync`.
        a.send(&ClientMsg::Sync {
            open: vec![],
            agents: Some(vec![]),
        })
        .await;
        row_of(&gather(&mut a, QUIET).await, &x.request_id)
    };
    a.send(&answer(&rx, "m-lock", Action::Accept, Some(shown(&rx))))
        .await;
    assert_eq!(
        next_result(&mut a, WAIT).await.status,
        AnswerStatus::AgentLocked
    );
    assert_eq!(pep.verdict(&x.request_id)["verdict"], "pending");

    // `agent_do` allow, with a name, from the first phone: both phones get the frame, with the same delivery id.
    let (status, read) = act(
        &mut a,
        &ax,
        "m-1",
        AgentOpKind::Allow,
        Some("Billing bot"),
        None,
    )
    .await;
    assert_eq!(status, AgentResultStatus::Ok);
    let on_a = by_id(&read.agents, &ax).clone();
    assert_eq!(
        (
            on_a.state,
            on_a.named,
            on_a.label.as_str(),
            on_a.lock_accept
        ),
        (AgentState::Known, true, "Billing bot", false)
    );
    assert!(on_a.rev > ids[0].1);
    let on_b = gather(&mut b, QUIET).await;
    assert_eq!(by_id(&on_b.agents, &ax).rev, on_a.rev);
    // The unlocked agent gets its approval. The phone has the answer, and the service reads `approved`.
    a.send(&answer(&rx, "m-ok", Action::Accept, Some(shown(&rx))))
        .await;
    assert_eq!(
        next_result(&mut a, WAIT).await.status,
        AnswerStatus::Accepted
    );
    assert_eq!(pep.verdict(&x.request_id)["verdict"], "approved");

    // Rename of Y from the second phone: both phones see the new name. The label of the frame is portal text.
    let (status, read) = act(
        &mut b,
        &ay,
        "m-2",
        AgentOpKind::Rename,
        Some("Night ETL"),
        None,
    )
    .await;
    assert_eq!(status, AgentResultStatus::Ok);
    assert_eq!(by_id(&read.agents, &ay).label, "Night ETL");
    let read = gather(&mut a, QUIET).await;
    assert_eq!(
        (
            by_id(&read.agents, &ay).label.as_str(),
            by_id(&read.agents, &ay).state
        ),
        ("Night ETL", AgentState::New)
    );

    // Block Y from the first phone: its open request is cancelled, and its token is refused at the channel.
    let (status, read) = act(&mut a, &ay, "m-3", AgentOpKind::Block, None, None).await;
    assert_eq!(status, AgentResultStatus::Ok);
    assert_eq!(by_id(&read.agents, &ay).state, AgentState::Blocked);
    assert!(
        read.settled.iter().any(|s| s.request_id == y.request_id
            && s.state == ItemState::Cancelled
            && s.reason == "agent_blocked"),
        "{:?}",
        read.settled
    );
    let (is_error, e) = pep.tool_as(
        Some(&tok_y),
        UA_Y,
        "get_answer",
        json!({"request_id": y.request_id}),
    );
    assert!(is_error, "{e}");
    assert_eq!(e["error"]["code"], "agent_blocked", "{e}");
    assert_eq!(pep.verdict(&y.request_id)["verdict"], "void");
    let read = gather(&mut b, QUIET).await;
    assert_eq!(by_id(&read.agents, &ay).state, AgentState::Blocked);

    // Unblock: a known agent again. Its token works, and it makes a new request.
    let (status, _) = act(&mut b, &ay, "m-4", AgentOpKind::Unblock, None, None).await;
    assert_eq!(status, AgentResultStatus::Ok);
    gather(&mut a, QUIET).await;
    let back = pep.agent_request(Some(&tok_y), UA_Y, &command(64), json!({}));
    assert_eq!(
        (back.status.as_str(), agent_of(&pep, &back.request_id)),
        ("known", ay.clone())
    );
    for conn in [&mut a, &mut b] {
        let read = gather(conn, QUIET).await;
        assert_eq!(row_of(&read, &back.request_id).from.agent_id, ay);
    }

    // Merge: an agent that lost its token makes a new agent Z. The owner merges Z into Y from a phone. The request of Z
    // moves, and the old token of Z gives Y.
    let z = pep.agent_request(
        None,
        "agent-z/3.0",
        &command(65),
        json!({"agent_label": "night-etl"}),
    );
    let (tok_z, az) = (
        z.token.clone().expect("token"),
        agent_of(&pep, &z.request_id),
    );
    let read = gather(&mut a, QUIET).await;
    let on_z = by_id(&read.agents, &az);
    assert_eq!(on_z.state, AgentState::New);
    assert_eq!(
        on_z.similar
            .as_ref()
            .map(|s| (s.agent_id.as_str(), s.label.as_str())),
        Some((ay.as_str(), "Night ETL")),
        "the new agent says the same text as the agent Y: the frame names it"
    );
    assert_eq!(on_z.state, AgentState::New, "the hint never merges");
    gather(&mut b, QUIET).await;
    let (status, read) = act(&mut b, &az, "m-5", AgentOpKind::Merge, None, Some(&ay)).await;
    assert_eq!(status, AgentResultStatus::Ok);
    let merged = by_id(&read.agents, &az);
    assert_eq!(
        (merged.state, merged.merged_into.as_deref()),
        (AgentState::Merged, Some(ay.as_str()))
    );
    assert!(
        by_id(&read.agents, &ay).open >= 2,
        "the target holds the items that moved"
    );
    assert_eq!(
        agent_of(&pep, &z.request_id),
        ay,
        "the request of Z is a request of Y now"
    );
    let read = gather(&mut a, QUIET).await;
    assert_eq!(by_id(&read.agents, &az).state, AgentState::Merged);
    let alias = pep.agent_request(Some(&tok_z), "agent-z/3.0", &command(66), json!({}));
    assert_eq!(
        (alias.status.as_str(), agent_of(&pep, &alias.request_id)),
        ("known", ay.clone()),
        "the token of the merged agent gives the target"
    );
    // The agent X was not touched by any of it.
    assert_eq!(agent_of(&pep, &x.request_id), ax);
    let _ = (z.request_id, ids);
}

/// The mode `enrolled` (lot G6). The owner makes an agent on the web. The phone gets the frame `agent` with the state `known`,
/// the name of the owner and no lock, and no first-use notice. A reset of the token raises `rev`. No frame holds a token.
/// The phone cannot enroll: `agent_do` has no such operation, so the web is the only place.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_enrolled_agent_reaches_the_phone_as_known_and_a_reset_raises_its_rev() {
    let pep = Pep::with_mode(
        |s| {
            s.add_to_inbox("agents { max_per_key = 8 new_per_hour = 20 lock_new = true }");
        },
        Some("enrolled"),
    );
    let phone = pep.phone("pixel").await;
    let mut conn = connect(&pep, &phone).await;
    gather(&mut conn, QUIET).await;
    let cookie = pep.cookie(OWNER);
    let (st, _, page) = pep.enroll_via_web(&cookie, "Night ETL");
    assert_eq!(st, 200, "{page}");
    let token = Pep::token_of(&page);
    let read = gather(&mut conn, QUIET).await;
    let frames: Vec<&AgentMsg> = read
        .agents
        .iter()
        .filter(|a| a.kind == AgentKind::Agent)
        .collect();
    assert_eq!(
        frames.len(),
        1,
        "one frame for the new agent: {:?}",
        read.agents
    );
    let f = frames[0];
    assert_eq!(
        (f.state, f.named, f.label.as_str(), f.lock_accept, f.rev),
        (AgentState::Known, true, "Night ETL", false, 1)
    );
    assert!(f.observed.is_none() && f.similar.is_none());

    // The agent asks with its token: the request names it, and the phone can approve it at once.
    let ask = pep.agent_request(
        Some(&token),
        "claude-code 2.1",
        &command(61),
        json!({"agent_label": "night-etl-says"}),
    );
    assert_eq!(ask.status, "known");
    let read = gather(&mut conn, QUIET).await;
    // The call showed hints, so the agent has a new `rev` (the frame `agent` with the hints).
    let before = read
        .agents
        .iter()
        .filter(|a| a.agent_id == f.agent_id)
        .map(|a| a.rev)
        .max()
        .unwrap_or(f.rev);
    let request = read
        .requests
        .iter()
        .find(|r| r.request_id == ask.request_id)
        .expect("the request reaches the phone");
    assert_eq!(
        (
            request.from.agent_id.as_str(),
            request.from.label.as_str(),
            request.from.agent_state
        ),
        (f.agent_id.as_str(), "Night ETL", AgentState::Known)
    );

    // The reset gives a new frame with a higher `rev`, for the same agent.
    let (st, _, page) = pep.reset_via_web(&cookie, &f.agent_id);
    assert_eq!(st, 200, "{page}");
    let fresh = Pep::token_of(&page);
    let read = gather(&mut conn, QUIET).await;
    let after = by_id(&read.agents, &f.agent_id);
    assert_eq!((after.rev, after.state), (before + 1, AgentState::Known));
    // No frame holds a token.
    for frame in read.agents.iter().chain(std::iter::once(f)) {
        let text = serde_json::to_string(frame).unwrap();
        for t in [&token, &fresh] {
            assert!(!text.contains(&t[4..]), "{text}");
        }
        assert!(!text.contains("agt_"));
    }
    // The old token is dead, and the new one gives the same agent.
    let (is_error, e) = pep.tool_as(
        Some(&token),
        "claude-code 2.1",
        "approval_intent",
        json!({"lease_s": 60}),
    );
    assert!(is_error && e["error"]["code"] == "agent_unknown", "{e}");
    let again = pep.agent_request(Some(&fresh), "claude-code 2.1", &command(62), json!({}));
    assert_eq!(again.status, "known");
}
