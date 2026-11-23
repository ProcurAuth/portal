//! The agents on the phones, with the real approval API and the real engine, on SQLite and on PostgreSQL. The
//! frames are the ones that a session builds from its queue: the test reads them as a phone does. A change of an agent
//! writes one message for each device of its owner and for no other device. The frame holds the state of the row when the
//! session sends it, and an older row is withdrawn. A `sync` brings the agents that a phone lacks. `agent_do` goes
//! through the engine and answers with a closed list of statuses.

use super::*;
use crate::inbox::agents::{AgentCall, AgentMode, Hints};
use crate::inbox::approvals::IntentOut;
use crate::inbox::testing::{
    agent_do_status, agent_frames_for_sync, result_status, Builder, Built,
};
use inbox_core::proto::{
    AgentDoMsg, AgentKind as WireKind, AgentMsg, AgentOpKind, AgentRef, AgentResultStatus,
    AgentState as WireState, ServerMsg,
};

/// The block of the agents. It closes the block `inbox.approvals` and opens the block `inbox.agents`. A new agent is
/// locked.
const AGENTS: &str = "} agents { max_per_key = 8 new_per_hour = 20 lock_new = true";
/// The default of the file: a new agent is not locked.
const OPEN: &str = "} agents { max_per_key = 8 new_per_hour = 20";

fn hints(says: &str) -> Hints {
    Hints {
        says: Some(says.into()),
        client: Some("claude-code/2.1.0".into()),
        user_agent: Some("claude-code/2.1.0 (linux; x64)".into()),
        net: Some("203.0.113.0/24".into()),
    }
}

/// A key of the mode `discover`: a call with no token makes an agent.
fn discover(r: &Rig, subject: &str, seen: Hints) -> Requester {
    let mut who = r.requester(subject, Interaction::Conversational);
    who.agent = AgentCall {
        mode: AgentMode::Discover,
        token: None,
        hints: seen,
    };
    who
}

fn holding(who: &Requester, token: &str, seen: Hints) -> Requester {
    let mut who = who.clone();
    who.agent.token = Some(token.to_string());
    who.agent.hints = seen;
    who
}

fn intent_out(r: &Rig, who: &Requester) -> IntentOut {
    let mut result = runtime().block_on(approvals::intent(&r.app, who, explained()));
    for _ in 0..12 {
        if !matches!(&result, Err(e) if e.status == 503) {
            break;
        }
        std::thread::sleep(Duration::from_millis(400));
        result = runtime().block_on(approvals::intent(&r.app, who, explained()));
    }
    result.unwrap_or_else(|e| panic!("{} {}", e.status, e.code))
}

/// An intent with no token that makes an agent, and the item that the service makes with it. The result is the id of
/// the agent, its token and the request id.
fn first_item(r: &Rig, who: &Requester, key: &str, n: u32) -> (String, String, String) {
    let out = intent_out(r, who);
    let token = out.agent.token.as_ref().expect("a new agent").to_string();
    let created = r.post(&r.body(
        key,
        &command(n),
        json!({ "approval_ref": out.approval_ref }),
    ));
    assert_eq!(created.status, 201, "{}", created.body);
    (out.agent.agent_id.clone(), token, created.id())
}

/// The frames that the phone `device` of `login` gets from its queue, as the server builds them.
fn frames_as(r: &Rig, device: &str, login: &str) -> Vec<ServerMsg> {
    runtime().block_on(async {
        let queue = r
            .items()
            .pending_item_deliveries(device, 100)
            .await
            .unwrap();
        let mut builder = Builder::new(&r.app, login, &queue).expect("the engine runs");
        let mut out = Vec::new();
        for d in &queue {
            if let Built::Frame(bytes) = builder.delivery(d).await.unwrap() {
                out.push(ServerMsg::decode(&bytes).unwrap());
            }
        }
        out
    })
}

fn agent_frames(frames: Vec<ServerMsg>) -> Vec<AgentMsg> {
    frames
        .into_iter()
        .filter_map(|f| match f {
            ServerMsg::Agent(a) => Some(*a),
            _ => None,
        })
        .collect()
}

/// The `agent` frames that a `sync` of `login` brings back when the phone holds `held`.
fn sync_frames(r: &Rig, login: &str, held: &[AgentRef]) -> Vec<AgentMsg> {
    runtime().block_on(async {
        let mut builder = Builder::new(&r.app, login, &[]).expect("the engine runs");
        agent_frames_for_sync(&mut builder, held)
            .await
            .unwrap()
            .into_iter()
            .map(|bytes| match ServerMsg::decode(&bytes).unwrap() {
                ServerMsg::Agent(a) => *a,
                other => panic!("not an agent frame: {other:?}"),
            })
            .collect()
    })
}

fn held(a: &AgentMsg) -> AgentRef {
    AgentRef {
        agent_id: a.agent_id.clone(),
        rev: a.rev,
    }
}

fn act(
    r: &Rig,
    login: &str,
    agent: &str,
    op: AgentOpKind,
    name: Option<&str>,
    into: Option<&str>,
) -> AgentResultStatus {
    let frame = AgentDoMsg {
        agent_id: agent.into(),
        client_msg_id: "m-0001".into(),
        op,
        name: name.map(str::to_string),
        into: into.map(str::to_string),
    };
    eventually(|| runtime().block_on(agent_do_status(&r.items(), login, &frame)))
}

/// Another phone of `login`. `n` makes its key, which is unique.
fn seed_phone(r: &Rig, name: &str, id: &str, login: &str, n: u8) {
    let key = if name == "sqlite" {
        format!("x'{n:02x}{n:02x}'")
    } else {
        format!("decode('{n:02x}{n:02x}', 'hex')")
    };
    let sql = format!(
        "INSERT INTO devices (id, name, user_name, public_key, enrolled_at, enrolled_ip) VALUES ('{id}', 'phone', '{login}', {key}, 1, 'ip')"
    );
    runtime().block_on(r.items().test_exec(&sql)).unwrap();
}

fn agent_messages(r: &Rig) -> i64 {
    r.count("SELECT COUNT(*) FROM alerts WHERE kind = 'agent'")
}

#[test]
fn the_agent_frame_goes_to_each_device_of_the_owner_and_to_nobody_else() {
    each_rig(true, AGENTS, |r, name| {
        r.device();
        seed_phone(r, name, "device-two", OWNER, 2);
        seed_phone(r, name, "device-bob", "bob", 3);
        let key = r.key_a();
        let who = discover(r, &key, hints("billing-bot"));
        let (agent, _, id) = first_item(r, &who, &key, 1);
        assert_eq!(
            agent_messages(r),
            1,
            "[{name}] one message for the new agent"
        );
        assert_eq!(
            r.count("SELECT COUNT(*) FROM deliveries d JOIN alerts a ON a.id = d.alert_id WHERE a.kind = 'agent'"),
            2,
            "[{name}] one delivery for each phone of the owner"
        );
        for device in [DEVICE, "device-two"] {
            let frames = frames_as(r, device, OWNER);
            let request = frames
                .iter()
                .find_map(|f| match f {
                    ServerMsg::Request(q) => Some(q),
                    _ => None,
                })
                .expect("the request");
            // The line "from" holds the agent, in portal text.
            assert_eq!(request.request_id, id, "[{name}]");
            assert_eq!(request.from.agent_id, agent, "[{name}]");
            assert_eq!(request.from.agent_kind, WireKind::Agent, "[{name}]");
            assert_eq!(request.from.agent_state, WireState::New, "[{name}]");
            assert_eq!(
                request.from.label, "key 2b7c91d0e4a853f6 #1",
                "[{name}] the number of the agent, not its text"
            );
            let agents = agent_frames(frames);
            assert_eq!(agents.len(), 1, "[{name}] {device}");
            let a = &agents[0];
            assert_eq!(
                (a.agent_id.as_str(), a.rev, a.kind, a.state),
                (agent.as_str(), 1, WireKind::Agent, WireState::New),
                "[{name}]"
            );
            assert_eq!(a.label, "key 2b7c91d0e4a853f6 #1", "[{name}]");
            assert_eq!(a.key_name, "key 2b7c91d0e4a853f6", "[{name}]");
            assert!(
                !a.named && a.lock_accept,
                "[{name}] a new agent is locked with the default of the file"
            );
            assert_eq!(a.open, 1, "[{name}]");
            assert_eq!(a.merged_into, None, "[{name}]");
            let o = a.observed.as_ref().expect("the hints");
            assert_eq!(o.says.as_deref(), Some("billing-bot"), "[{name}]");
            assert_eq!(o.client.as_deref(), Some("claude-code/2.1.0"), "[{name}]");
            assert_eq!(
                o.user_agent.as_deref(),
                Some("claude-code/2.1.0 (linux; x64)"),
                "[{name}]"
            );
            assert_eq!(
                (o.net.as_deref(), o.nets_seen, o.says_changed),
                (Some("203.0.113.0/24"), 1, None),
                "[{name}]"
            );
        }
        // The phone of another owner gets nothing: no message and no request.
        assert!(frames_as(r, "device-bob", "bob").is_empty(), "[{name}]");
        // A session of another owner that read the queue of this phone would build nothing: the agent is not its own.
        let queue = runtime()
            .block_on(r.items().pending_item_deliveries(DEVICE, 100))
            .unwrap();
        let mut stolen = 0;
        runtime().block_on(async {
            let mut builder = Builder::new(&r.app, "bob", &queue).unwrap();
            for d in queue.iter().filter(|d| d.kind == "agent") {
                match builder.delivery(d).await.unwrap() {
                    Built::Withdraw(why) => assert_eq!(why, "agent_gone"),
                    _ => stolen += 1,
                }
            }
        });
        assert_eq!(stolen, 0, "[{name}]");
        // The text of the agent is in no label of the portal.
        let all = serde_json::to_string(&frames_as(r, DEVICE, OWNER)).unwrap();
        assert!(
            !all.contains("billing-bot") || all.contains("\"says\":\"billing-bot\""),
            "[{name}]"
        );
    });
}

#[test]
fn a_change_raises_the_rev_and_the_phone_gets_one_frame_with_the_state_of_now() {
    each_rig(true, AGENTS, |r, name| {
        r.device();
        let key = r.key_a();
        let who = discover(r, &key, hints("billing-bot"));
        let (agent, token, _) = first_item(r, &who, &key, 1);
        assert_eq!(agent_messages(r), 1, "[{name}]");
        // New hints: the same agent calls again with another text. One more message, and the rev grows.
        intent_out(r, &holding(&who, &token, hints("night-etl")));
        assert_eq!(agent_messages(r), 2, "[{name}]");
        // The same hints again change nothing: no message.
        intent_out(r, &holding(&who, &token, hints("night-etl")));
        assert_eq!(agent_messages(r), 2, "[{name}]");
        // Two rows wait for the phone. It gets one frame, with the newest state: the older row is withdrawn.
        let a = agent_frames(frames_as(r, DEVICE, OWNER));
        assert_eq!(a.len(), 1, "[{name}] the older row is withdrawn");
        assert_eq!(a[0].rev, 2, "[{name}]");
        let o = a[0].observed.as_ref().unwrap();
        assert_eq!(
            (o.says.as_deref(), o.says_changed),
            (Some("night-etl"), Some(1)),
            "[{name}]"
        );
        // The owner allows the agent and names it: state, name and lock follow, and the label is the name.
        assert_eq!(
            act(
                r,
                OWNER,
                &agent,
                AgentOpKind::Allow,
                Some("Billing bot"),
                None
            ),
            AgentResultStatus::Ok,
            "[{name}]"
        );
        assert_eq!(agent_messages(r), 3, "[{name}] one message for the action");
        let a = agent_frames(frames_as(r, DEVICE, OWNER));
        assert_eq!(a.len(), 1, "[{name}]");
        assert_eq!(
            (a[0].rev, a[0].state, a[0].named, a[0].lock_accept),
            (4, WireState::Known, true, false),
            "[{name}] an allow with a name changes the row twice: the state, then the name"
        );
        assert_eq!(a[0].label, "Billing bot", "[{name}]");
        // The same action again is a success that writes nothing: the phone sends it until it gets the result.
        assert_eq!(
            act(
                r,
                OWNER,
                &agent,
                AgentOpKind::Allow,
                Some("Billing bot"),
                None
            ),
            AgentResultStatus::Ok,
            "[{name}]"
        );
        assert_eq!(
            agent_messages(r),
            3,
            "[{name}] an action that changed nothing sends nothing"
        );
        // A rename, a block and an unblock each give a new rev and a new message.
        assert_eq!(
            act(
                r,
                OWNER,
                &agent,
                AgentOpKind::Rename,
                Some("Refund cleaner"),
                None
            ),
            AgentResultStatus::Ok
        );
        assert_eq!(
            act(r, OWNER, &agent, AgentOpKind::Block, None, None),
            AgentResultStatus::Ok
        );
        assert_eq!(agent_messages(r), 5, "[{name}]");
        let a = agent_frames(frames_as(r, DEVICE, OWNER));
        assert_eq!(
            (a[0].rev, a[0].state, a[0].open, a[0].label.as_str()),
            (6, WireState::Blocked, 0, "Refund cleaner"),
            "[{name}] the block cancelled the item"
        );
        assert_eq!(
            act(r, OWNER, &agent, AgentOpKind::Unblock, None, None),
            AgentResultStatus::Ok
        );
        let a = agent_frames(frames_as(r, DEVICE, OWNER));
        assert_eq!((a[0].rev, a[0].state), (7, WireState::Known), "[{name}]");
        // A receipt ends the row: a phone that confirmed the frame gets no more of it.
        let queue = runtime()
            .block_on(r.items().pending_item_deliveries(DEVICE, 100))
            .unwrap();
        let rows: Vec<_> = queue
            .iter()
            .filter(|d| d.kind == "agent")
            .map(|d| d.alert_id.clone())
            .collect();
        assert_eq!(rows.len(), 6, "[{name}]");
        for alert_id in &rows {
            runtime().block_on(r.app.db.ack(DEVICE, alert_id)).unwrap();
        }
        assert!(
            agent_frames(frames_as(r, DEVICE, OWNER)).is_empty(),
            "[{name}]"
        );
    });
}

#[test]
fn a_merge_sends_the_frame_of_the_agent_and_the_frame_of_the_target() {
    each_rig(true, AGENTS, |r, name| {
        r.device();
        let key = r.key_a();
        let who = discover(r, &key, Hints::default());
        let (lost, _, lost_item) = first_item(r, &who, &key, 1);
        let (target, _, _) = first_item(r, &who, &key, 2);
        assert_eq!(
            act(
                r,
                OWNER,
                &target,
                AgentOpKind::Allow,
                Some("Billing bot"),
                None
            ),
            AgentResultStatus::Ok
        );
        let before = agent_messages(r);
        assert_eq!(
            act(r, OWNER, &lost, AgentOpKind::Merge, None, Some(&target)),
            AgentResultStatus::Ok
        );
        assert_eq!(
            agent_messages(r),
            before + 2,
            "[{name}] the agent and the target"
        );
        let frames = agent_frames(frames_as(r, DEVICE, OWNER));
        let by_id = |id: &str| {
            frames
                .iter()
                .find(|a| a.agent_id == id)
                .unwrap_or_else(|| panic!("[{name}] no frame for {id}"))
        };
        let (from, to) = (by_id(&lost), by_id(&target));
        assert_eq!(
            (from.state, from.merged_into.as_deref()),
            (WireState::Merged, Some(target.as_str())),
            "[{name}]"
        );
        assert_eq!(
            (to.state, to.open),
            (WireState::Known, 2),
            "[{name}] the target holds the items of the agent"
        );
        // The item that moved is the item of the target now: its request frame says so.
        let request = frames_as(r, DEVICE, OWNER)
            .into_iter()
            .find_map(|f| match f {
                ServerMsg::Request(q) if q.request_id == lost_item => Some(q),
                _ => None,
            })
            .expect("the request");
        assert_eq!(
            (request.from.agent_id.as_str(), request.from.label.as_str()),
            (target.as_str(), "Billing bot"),
            "[{name}]"
        );
        // The same merge again writes nothing.
        let sent = agent_messages(r);
        assert_eq!(
            act(r, OWNER, &lost, AgentOpKind::Merge, None, Some(&target)),
            AgentResultStatus::Ok
        );
        assert_eq!(agent_messages(r), sent, "[{name}]");
    });
}

#[test]
fn sync_brings_the_agents_that_the_phone_lacks() {
    each_rig(true, AGENTS, |r, name| {
        r.device();
        let key = r.key_a();
        let who = discover(r, &key, hints("billing-bot"));
        let (a1, _, _) = first_item(r, &who, &key, 1);
        let (a2, _, _) = first_item(r, &who, &key, 2);
        let (a3, _, _) = first_item(r, &who, &key, 3);
        let (a4, _, _) = first_item(r, &who, &key, 4);
        assert_eq!(
            act(r, OWNER, &a2, AgentOpKind::Allow, Some("Night ETL"), None),
            AgentResultStatus::Ok
        );
        assert_eq!(
            act(r, OWNER, &a3, AgentOpKind::Block, None, None),
            AgentResultStatus::Ok
        );
        assert_eq!(
            act(r, OWNER, &a4, AgentOpKind::Merge, None, Some(&a2)),
            AgentResultStatus::Ok
        );
        // The bucket of the key: an item that no agent claims.
        r.submit(&key, 9);
        let ids = |v: &[AgentMsg]| {
            let mut ids: Vec<String> = v.iter().map(|a| a.agent_id.clone()).collect();
            ids.sort();
            ids
        };
        let sorted = |mut v: Vec<String>| {
            v.sort();
            v
        };
        // A phone that holds nothing gets the agents that are alive, and the bucket. The blocked agent and the merged
        // agent are gone: a phone that never knew them has no use for them.
        let all = sync_frames(r, OWNER, &[]);
        let bucket = all
            .iter()
            .find(|a| a.kind == WireKind::Bucket)
            .expect("the bucket");
        assert_eq!(
            ids(&all),
            sorted(vec![a1.clone(), a2.clone(), bucket.agent_id.clone()]),
            "[{name}]"
        );
        assert_eq!(
            (
                bucket.state,
                bucket.named,
                bucket.lock_accept,
                bucket.observed.is_none()
            ),
            (WireState::Known, false, false, true),
            "[{name}]"
        );
        // The key store gives the mode of the key. A key of the kind `mcp` is in the mode `discover`: the bucket
        // is "Unattributed" and not the key name.
        assert_eq!(
            bucket.label, "Unattributed · key 2b7c91d0e4a853f6",
            "[{name}]"
        );
        let one = all.iter().find(|a| a.agent_id == a1).unwrap();
        assert_eq!(
            (one.state, one.lock_accept, one.open),
            (WireState::New, true, 1),
            "[{name}]"
        );
        let two = all.iter().find(|a| a.agent_id == a2).unwrap();
        assert_eq!(
            (two.state, two.label.as_str(), two.open),
            (WireState::Known, "Night ETL", 2),
            "[{name}]"
        );
        // The frames of a sync have new ids that are in no queue: two syncs give two ids.
        let again = sync_frames(r, OWNER, &[]);
        assert!(
            all.iter().zip(&again).all(|(a, b)| a.id != b.id),
            "[{name}]"
        );
        // A phone that holds the newest rev of each gets nothing.
        let current: Vec<AgentRef> = all.iter().map(held).collect();
        assert!(sync_frames(r, OWNER, &current).is_empty(), "[{name}]");
        // A phone that holds an older rev gets the agent, whatever its state: the blocked agent and the merged agent
        // too. A phone that holds a rev of a higher number than the server's gets nothing.
        let old = |id: &str, rev: u64| AgentRef {
            agent_id: id.into(),
            rev,
        };
        let news = sync_frames(r, OWNER, &[old(&a3, 1), old(&a4, 1), old(&a1, 1)]);
        let news_ids = ids(&news);
        assert!(news_ids.contains(&a3) && news_ids.contains(&a4), "[{name}]");
        assert!(
            !news_ids.contains(&a1),
            "[{name}] the rev 1 of the agent 1 is the newest: nothing to send"
        );
        let blocked = news.iter().find(|a| a.agent_id == a3).unwrap();
        assert_eq!(blocked.state, WireState::Blocked, "[{name}]");
        let merged = news.iter().find(|a| a.agent_id == a4).unwrap();
        assert_eq!(
            (merged.state, merged.merged_into.as_deref()),
            (WireState::Merged, Some(a2.as_str())),
            "[{name}]"
        );
        let mut ahead = current.clone();
        ahead[0].rev = u64::MAX >> 12;
        assert!(sync_frames(r, OWNER, &ahead).len() < all.len(), "[{name}]");
        // An agent with no open item, and no call for the idle time, is left out when the phone does not hold it.
        let (a5, _, item5) = first_item(r, &who, &key, 5);
        assert_eq!(
            act(r, OWNER, &a5, AgentOpKind::Allow, None, None),
            AgentResultStatus::Ok
        );
        r.answer(&item5, Action::Decline, None);
        assert!(
            ids(&sync_frames(r, OWNER, &[])).contains(&a5),
            "[{name}] a known agent that was active just now"
        );
        r.fix_clock(r.now() + 15 * 86_400_000);
        let later = sync_frames(r, OWNER, &[]);
        let later_ids = ids(&later);
        assert!(!later_ids.contains(&a5), "[{name}] idle");
        assert!(
            later_ids.contains(&a1) && later_ids.contains(&bucket.agent_id),
            "[{name}] an open item, and the bucket, are never idle"
        );
        // An idle agent that the phone holds in an older rev is sent: the phone converges.
        let idle = sync_frames(r, OWNER, &[old(&a2, 1)]);
        assert_eq!(
            idle.iter().filter(|a| a.agent_id == a2).count(),
            1,
            "[{name}]"
        );
        // Another owner gets nothing: the agents are not its own.
        assert!(sync_frames(r, "bob", &[]).is_empty(), "[{name}]");
        assert!(sync_frames(r, "bob", &[old(&a1, 1)]).is_empty(), "[{name}]");
    });
}

#[test]
fn agent_do_answers_with_a_closed_list_of_statuses_and_never_for_another_owner() {
    each_rig(true, AGENTS, |r, name| {
        r.device();
        let (key, other_key) = (r.key_a(), r.key_b());
        let who = discover(r, &key, Hints::default());
        let (a1, _, _) = first_item(r, &who, &key, 1);
        let (a2, _, _) = first_item(r, &who, &key, 2);
        let foreign = first_item(r, &discover(r, &other_key, Hints::default()), &other_key, 3).0;
        use AgentOpKind::*;
        use AgentResultStatus as S;
        // A name: given, again (idempotent), refused for a control character, a format character, a white space at an
        // end and a length, and refused when another agent of the key has it, with the fold of the case.
        assert_eq!(
            act(r, OWNER, &a1, Rename, Some("Billing bot"), None),
            S::Ok,
            "[{name}]"
        );
        assert_eq!(
            act(r, OWNER, &a1, Rename, Some("Billing bot"), None),
            S::Ok,
            "[{name}]"
        );
        for bad in ["a\nb", "a\u{202e}b", " padded", "padded ", "a\u{200b}b"] {
            assert_eq!(
                act(r, OWNER, &a2, Rename, Some(bad), None),
                S::InvalidName,
                "[{name}] {bad:?}"
            );
        }
        assert_eq!(
            act(r, OWNER, &a2, Rename, Some("BILLING  BOT"), None),
            S::NameTaken,
            "[{name}]"
        );
        assert_eq!(
            act(r, OWNER, &a2, Allow, Some("billing bot"), None),
            S::NameTaken,
            "[{name}]"
        );
        // A refusal changes nothing: the agent is still `new`.
        let still = sync_frames(r, OWNER, &[]);
        assert_eq!(
            still.iter().find(|a| a.agent_id == a2).unwrap().state,
            WireState::New,
            "[{name}]"
        );
        // The same name in another key is free.
        assert_eq!(
            act(r, OWNER, &foreign, Rename, Some("Billing bot"), None),
            S::Ok,
            "[{name}]"
        );
        // Allow, with and without a name, and again.
        assert_eq!(act(r, OWNER, &a2, Allow, None, None), S::Ok, "[{name}]");
        assert_eq!(act(r, OWNER, &a2, Allow, None, None), S::Ok, "[{name}]");
        assert_eq!(
            act(r, OWNER, &a2, Allow, Some("Night ETL"), None),
            S::Ok,
            "[{name}]"
        );
        // Unblock needs a blocked agent. Block, then allow of a blocked agent is a bad state, and unblock heals it.
        assert_eq!(act(r, OWNER, &a1, Allow, None, None), S::Ok, "[{name}]");
        assert_eq!(
            act(r, OWNER, &a1, Unblock, None, None),
            S::Ok,
            "[{name}] a known agent is not blocked: nothing to do"
        );
        let fresh = first_item(r, &who, &key, 4).0;
        assert_eq!(
            act(r, OWNER, &fresh, Unblock, None, None),
            S::BadState,
            "[{name}] a new agent is not blocked"
        );
        assert_eq!(act(r, OWNER, &a2, Block, None, None), S::Ok, "[{name}]");
        assert_eq!(act(r, OWNER, &a2, Block, None, None), S::Ok, "[{name}]");
        assert_eq!(
            act(r, OWNER, &a2, Allow, None, None),
            S::BadState,
            "[{name}]"
        );
        assert_eq!(act(r, OWNER, &a2, Unblock, None, None), S::Ok, "[{name}]");
        // A merge: into itself, into an agent that does not exist, across two keys, and the real one, twice.
        assert_eq!(
            act(r, OWNER, &a2, Merge, None, Some(&a2)),
            S::BadState,
            "[{name}]"
        );
        assert_eq!(
            act(r, OWNER, &a2, Merge, None, Some(&"0".repeat(32))),
            S::NotFound,
            "[{name}]"
        );
        assert_eq!(
            act(r, OWNER, &a2, Merge, None, Some(&foreign)),
            S::CrossKey,
            "[{name}]"
        );
        assert_eq!(
            act(r, OWNER, &a2, Merge, None, Some(&a1)),
            S::Ok,
            "[{name}]"
        );
        assert_eq!(
            act(r, OWNER, &a2, Merge, None, Some(&a1)),
            S::Ok,
            "[{name}]"
        );
        assert_eq!(
            act(r, OWNER, &a2, Rename, Some("again"), None),
            S::BadState,
            "[{name}] a merged agent has no name"
        );
        // The bucket cannot be named, merged, blocked or locked.
        let bucket = runtime()
            .block_on(r.items().bucket(key.trim_start_matches("key:"), OWNER))
            .unwrap()
            .agent_id;
        for (op, nm, into) in [
            (Rename, Some("x"), None),
            (Block, None, None),
            (Allow, None, None),
            (Merge, None, Some(a1.as_str())),
        ] {
            assert_eq!(
                act(r, OWNER, &bucket, op, nm, into),
                S::BadState,
                "[{name}] {op:?}"
            );
        }
        // An agent that does not exist, and an agent of another owner, get the same answer.
        assert_eq!(
            act(r, OWNER, &"0".repeat(32), Block, None, None),
            S::NotFound,
            "[{name}]"
        );
        assert_eq!(
            act(r, "bob", &a1, Block, None, None),
            S::NotFound,
            "[{name}]"
        );
        assert_eq!(
            act(r, "bob", &a1, Rename, Some("Mine"), None),
            S::NotFound,
            "[{name}]"
        );
        assert_eq!(
            act(r, "bob", &foreign, Merge, None, Some(&a1)),
            S::NotFound,
            "[{name}]"
        );
        assert_eq!(
            act(r, OWNER, &a1, Merge, None, Some(&foreign)),
            S::CrossKey,
            "[{name}]"
        );
        // Bob changed nothing: the agents of the owner are as they were.
        assert_eq!(
            sync_frames(r, OWNER, &[])
                .iter()
                .find(|a| a.agent_id == a1)
                .unwrap()
                .label,
            "Billing bot",
            "[{name}]"
        );
        // The status of the frame is the status of the engine: one text for each refusal.
        use crate::inbox::agents::TextError;
        use crate::inbox::testing::OpError;
        for e in [
            OpError::NotFound,
            OpError::InvalidName(TextError::Control),
            OpError::NameTaken,
            OpError::BadState,
            OpError::CrossKey,
        ] {
            assert_eq!(result_status(e).as_str(), e.status(), "[{name}]");
        }
    });
}

#[test]
fn agent_do_without_the_block_agents_is_not_enabled() {
    each_rig(true, "", |r, name| {
        r.device();
        let key = r.key_a();
        r.submit(&key, 1);
        let bucket = runtime()
            .block_on(r.items().bucket(key.trim_start_matches("key:"), OWNER))
            .unwrap()
            .agent_id;
        for op in [AgentOpKind::Block, AgentOpKind::Unblock, AgentOpKind::Allow] {
            assert_eq!(
                act(r, OWNER, &bucket, op, None, None),
                AgentResultStatus::NotEnabled,
                "[{name}] {op:?}"
            );
        }
        assert_eq!(
            act(r, OWNER, &bucket, AgentOpKind::Rename, Some("x"), None),
            AgentResultStatus::NotEnabled,
            "[{name}]"
        );
        // No agent frame exists without the block: no message, no frame at a sync. The request has the bucket.
        assert_eq!(agent_messages(r), 0, "[{name}]");
        assert!(sync_frames(r, OWNER, &[]).is_empty(), "[{name}]");
        let frames = frames_as(r, DEVICE, OWNER);
        let request = frames
            .iter()
            .find_map(|f| match f {
                ServerMsg::Request(q) => Some(q),
                _ => None,
            })
            .expect("the request");
        assert_eq!(
            (
                request.from.agent_kind,
                request.from.agent_state,
                request.from.agent_id.as_str()
            ),
            (WireKind::Bucket, WireState::Known, bucket.as_str()),
            "[{name}]"
        );
        assert_eq!(
            request.from.label, "key 2b7c91d0e4a853f6",
            "[{name}] the key name"
        );
    });
}

#[test]
fn lock_new_false_the_default_gives_the_frame_without_the_lock_and_the_same_fields() {
    each_rig(true, OPEN, |r, name| {
        r.device();
        let key = r.key_a();
        let who = discover(r, &key, hints("billing-bot"));
        let (agent, _, _) = first_item(r, &who, &key, 1);
        let a = agent_frames(frames_as(r, DEVICE, OWNER));
        assert_eq!(a.len(), 1, "[{name}]");
        // The agent is `new` and not locked: the owner can approve before the first allow.
        assert_eq!(
            (a[0].agent_id.as_str(), a[0].state, a[0].lock_accept),
            (agent.as_str(), WireState::New, false),
            "[{name}]"
        );
        assert_eq!(a[0].label, "key 2b7c91d0e4a853f6 #1", "[{name}]");
        assert_eq!(
            a[0].observed.as_ref().unwrap().says.as_deref(),
            Some("billing-bot"),
            "[{name}]"
        );
    });
}

// ---------------------------------------------------------------- the hint "similar"

/// A new agent that looks like a known agent of the same key carries `similar` in its frame, on both ways of delivery
/// (the queue and `sync`). The hint is help for the owner: the frames of the other agents carry none, and nothing merges
/// by itself.
#[test]
fn a_new_agent_that_looks_like_a_known_one_carries_similar_and_nothing_merges() {
    each_rig(true, OPEN, |r, name| {
        r.device();
        let key = r.key_a();
        // The known agent: the owner allowed it and gave it a name.
        let known = first_item(r, &discover(r, &key, hints("billing-bot")), &key, 1).0;
        assert_eq!(
            act(
                r,
                OWNER,
                &known,
                AgentOpKind::Allow,
                Some("Billing bot"),
                None
            ),
            AgentResultStatus::Ok
        );
        // The same text with other letters: it folds to the same text.
        let copy = first_item(r, &discover(r, &key, hints("BILLING-bot")), &key, 2).0;
        // The same client and the same range, with another text.
        let same_client = first_item(r, &discover(r, &key, hints("night-etl")), &key, 3).0;
        // Another text, another client and another range: nothing looks like it.
        let other = Hints {
            says: Some("report-writer".into()),
            client: Some("curl/8.0".into()),
            user_agent: Some("curl/8.0".into()),
            net: Some("198.51.100.0/24".into()),
        };
        let stranger = first_item(r, &discover(r, &key, other), &key, 4).0;
        let want = |frames: &[AgentMsg], id: &str| -> Option<(String, String)> {
            frames
                .iter()
                .find(|a| a.agent_id == id)
                .unwrap_or_else(|| panic!("[{name}] no frame for {id}"))
                .similar
                .as_ref()
                .map(|s| (s.agent_id.clone(), s.label.clone()))
        };
        let named = Some((known.clone(), "Billing bot".to_string()));
        // `sync`: a phone that holds nothing gets each agent.
        let all = sync_frames(r, OWNER, &[]);
        assert_eq!(want(&all, &copy), named, "[{name}] the same text");
        assert_eq!(want(&all, &same_client), named, "[{name}] the same client");
        assert_eq!(want(&all, &stranger), None, "[{name}]");
        assert_eq!(
            want(&all, &known),
            None,
            "[{name}] a known agent has no hint"
        );
        for a in &all {
            assert_eq!(
                a.similar.is_some(),
                a.state == WireState::New && a.agent_id != stranger,
                "[{name}]"
            );
        }
        // The queue: the frame of the new agent that the phone got when it was made is built with the state of now.
        let queued = agent_frames(frames_as(r, DEVICE, OWNER));
        assert_eq!(want(&queued, &copy), named, "[{name}] from the queue");
        assert_eq!(want(&queued, &stranger), None, "[{name}] from the queue");
        // The hint changes nothing: every new agent is still new, and no agent was merged.
        for id in [&copy, &same_client, &stranger] {
            assert_eq!(
                r.count(&format!(
                    "SELECT COUNT(*) FROM agents WHERE agent_id = '{id}' AND state = 'new'"
                )),
                1,
                "[{name}]"
            );
        }
        assert_eq!(
            r.count("SELECT COUNT(*) FROM agents WHERE state = 'merged'"),
            0,
            "[{name}]"
        );
        // The hint ends with the known agent: blocked, it is no candidate.
        assert_eq!(
            act(r, OWNER, &known, AgentOpKind::Block, None, None),
            AgentResultStatus::Ok
        );
        let after = sync_frames(r, OWNER, &[]);
        assert_eq!(
            want(&after, &copy),
            None,
            "[{name}] a blocked agent is no hint"
        );
        assert_eq!(want(&after, &same_client), None, "[{name}]");
        // A bucket never gets a hint.
        r.submit(&key, 9);
        let with_bucket = sync_frames(r, OWNER, &[]);
        assert!(
            with_bucket
                .iter()
                .filter(|a| a.kind == WireKind::Bucket)
                .all(|a| a.similar.is_none()),
            "[{name}]"
        );
    });
}
