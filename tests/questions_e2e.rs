//! End-to-end tests of the questions: the real binary, an agent that asks through `/mcp`, and a Rust phone that
//! speaks Noise to the session of the portal. The rig is the rig of the approvals with the block `inbox.questions`.
//! Run them on PostgreSQL with `INBOX_E2E_BACKEND=postgres`.

mod common;

use std::time::Duration;

use common::approvals::{next_message, next_request, next_result, Pep};
use common::{Conn, Phone};
use inbox_core::proto::{
    Action, AnswerMsg, AnswerStatus, ClientMsg, Interaction, ItemState, Kind, ReplyMsg, RequestMsg,
};
use serde_json::{json, Value};

const WAIT: Duration = Duration::from_secs(15);

async fn connect(pep: &Pep, phone: &Phone) -> Conn {
    let (conn, _) = Conn::open(pep.s.port, phone)
        .await
        .expect("the session opens");
    conn
}

async fn first_request(c: &mut Conn) -> RequestMsg {
    let req = next_request(c, WAIT).await;
    c.receipt(&req.id).await;
    req
}

fn answer(
    req: &RequestMsg,
    id: &str,
    action: Action,
    choice: Option<&[&str]>,
    text: Option<&str>,
) -> ClientMsg {
    ClientMsg::Answer(AnswerMsg {
        request_id: req.request_id.clone(),
        client_msg_id: id.into(),
        action,
        shown_effect: None,
        choice: choice.map(|c| c.iter().map(|s| s.to_string()).collect()),
        text: text.map(str::to_string),
        intent: None,
        answers: None,
    })
}

fn options() -> Value {
    json!([{"id": "go", "label": "Run it", "description": "On production"}, {"id": "wait", "label": "Wait"}])
}

fn question(extra: Value) -> Value {
    let mut args = json!({"title": "Purge the staging?", "lease_s": 600,
        "input": {"type": "choice", "options": options()}});
    for (k, v) in extra.as_object().unwrap() {
        args[k] = v.clone();
    }
    args
}

fn with_flags(mut args: Value, custom: bool, back: bool) -> Value {
    args["input"]["allow_custom"] = json!(custom);
    args["input"]["allow_ask_back"] = json!(back);
    args
}

fn read(pep: &Pep, id: &str) -> Value {
    pep.tool("get_answer", json!({"request_id": id}))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_phone_gets_a_question_and_answers_a_choice_a_custom_text_a_decline_and_asks_back() {
    let pep = Pep::with_questions(|_| {});
    let phone = pep.phone("pixel").await;
    let mut conn = connect(&pep, &phone).await;

    // A choice with a command, a hint and both flags.
    let made = pep.tool(
        "ask",
        with_flags(
            question(
                json!({"explanation": "It frees 40 GB.", "command": "DELETE FROM staging.events;",
                "command_label": "SQL", "language": "sql", "urgency": "high",
                "hint": {"suggested": "wait", "confidence": 0.6, "source": "agent"}}),
            ),
            true,
            true,
        ),
    );
    assert_eq!(made["status"], "pending", "{made}");
    let id = made["request_id"].as_str().unwrap().to_string();
    let req = first_request(&mut conn).await;
    assert_eq!(req.kind, Kind::Question);
    assert_eq!(req.request_id, id);
    assert_eq!(req.title.as_deref(), Some("Purge the staging?"));
    assert_eq!(req.explanation.as_deref(), Some("It frees 40 GB."));
    let command = req.command.as_ref().expect("the command");
    assert_eq!(command.text, "DELETE FROM staging.events;");
    assert_eq!(command.label.as_deref(), Some("SQL"));
    assert!(req.allow_custom && req.allow_ask_back);
    assert_eq!(req.from.interaction, Interaction::Conversational);
    let input = req.input.as_ref().expect("the input");
    assert_eq!(input.options.len(), 2);
    assert_eq!(req.hint.as_ref().unwrap().suggested, "wait");
    assert!(req.approval.is_none());

    // The owner asks the agent: the item is `replied`, and the agent reads the message. The agent talks back.
    conn.send(&ClientMsg::Reply(ReplyMsg {
        request_id: id.clone(),
        client_msg_id: "m-1".into(),
        text: "Which database?".into(),
    }))
    .await;
    let result = next_result(&mut conn, WAIT).await;
    assert_eq!(
        (result.status, result.state),
        (AnswerStatus::Accepted, Some(ItemState::Replied))
    );
    let got = read(&pep, &id);
    assert_eq!(got["status"], "replied");
    assert_eq!(got["untrusted"]["messages"][0]["text"], "Which database?");
    tokio::time::sleep(Duration::from_millis(2100)).await;
    let said = pep.tool("say", json!({"request_id": id, "text": "The staging one."}));
    assert_eq!(said["status"], "pending", "{said}");
    let message = next_message(&mut conn, WAIT).await;
    conn.receipt(&message.id).await;
    assert_eq!(message.explanation.as_deref(), Some("The staging one."));

    // The owner picks an option.
    conn.send(&answer(&req, "m-2", Action::Accept, Some(&["go"]), None))
        .await;
    let result = next_result(&mut conn, WAIT).await;
    assert_eq!(
        (result.status, result.state),
        (AnswerStatus::Accepted, Some(ItemState::Answered))
    );
    let got = read(&pep, &id);
    assert_eq!(got["status"], "answered");
    assert_eq!(got["answer"]["action"], "accept");
    assert_eq!(got["answer"]["choice"], json!(["go"]));
    assert!(got["answer"]["by"].is_string());

    // A custom text ends a question that allows it, and it arrives as an empty choice and an untrusted text.
    let made = pep.tool(
        "ask",
        with_flags(question(json!({"idempotency_key": "two"})), true, false),
    );
    let id2 = made["request_id"].as_str().unwrap().to_string();
    let req2 = first_request(&mut conn).await;
    assert!(req2.allow_custom && !req2.allow_ask_back);
    conn.send(&answer(
        &req2,
        "m-3",
        Action::Accept,
        None,
        Some("not before 18h"),
    ))
    .await;
    assert_eq!(
        next_result(&mut conn, WAIT).await.status,
        AnswerStatus::Accepted
    );
    let got = read(&pep, &id2);
    assert_eq!(got["answer"]["choice"], json!([]));
    assert_eq!(got["answer"]["untrusted"]["text"], "not before 18h");
    assert!(got["data_notice"].is_string());

    // A decline is an answer with no choice.
    let made = pep.tool("ask", question(json!({"idempotency_key": "three"})));
    let id3 = made["request_id"].as_str().unwrap().to_string();
    let req3 = first_request(&mut conn).await;
    assert!(!req3.allow_custom && !req3.allow_ask_back);
    conn.send(&answer(&req3, "m-4", Action::Decline, None, None))
        .await;
    assert_eq!(
        next_result(&mut conn, WAIT).await.status,
        AnswerStatus::Accepted
    );
    let got = read(&pep, &id3);
    assert_eq!(got["status"], "answered");
    assert_eq!(got["answer"]["action"], "decline");
}

/// An answer that does not fit the question gets an `answer_result`, so the entry leaves the outbox of the phone. A text
/// that the question does not allow gets its own status. The item stays open.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn answers_that_do_not_fit_the_question_get_a_result_and_change_nothing() {
    let pep = Pep::with_questions(|_| {});
    let phone = pep.phone("pixel").await;
    let mut conn = connect(&pep, &phone).await;
    let made = pep.tool("ask", question(json!({})));
    let id = made["request_id"].as_str().unwrap().to_string();
    let req = first_request(&mut conn).await;

    conn.send(&answer(
        &req,
        "bad-1",
        Action::Accept,
        None,
        Some("free text"),
    ))
    .await;
    let result = next_result(&mut conn, WAIT).await;
    assert_eq!(
        (result.client_msg_id.as_str(), result.status, result.state),
        (
            "bad-1",
            AnswerStatus::CustomNotAllowed,
            Some(ItemState::Pending)
        )
    );
    for (n, bad) in [
        Some(&["nope"][..]),
        Some(&["go", "wait"][..]),
        Some(&[][..]),
        None,
    ]
    .into_iter()
    .enumerate()
    {
        let cmid = format!("bad-{}", n + 2);
        conn.send(&answer(&req, &cmid, Action::Accept, bad, None))
            .await;
        let result = next_result(&mut conn, WAIT).await;
        // The entry is named and closed: the phone drops it and never sends it again.
        assert_eq!(
            (result.client_msg_id.as_str(), result.status, result.state),
            (cmid.as_str(), AnswerStatus::Closed, None)
        );
    }
    assert_eq!(read(&pep, &id)["status"], "pending");
    conn.send(&answer(&req, "good", Action::Accept, Some(&["wait"]), None))
        .await;
    assert_eq!(
        next_result(&mut conn, WAIT).await.status,
        AnswerStatus::Accepted
    );
    assert_eq!(read(&pep, &id)["answer"]["choice"], json!(["wait"]));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_one_shot_question_is_closed_on_the_phone_and_a_reply_is_refused() {
    let pep = Pep::with_questions(|_| {});
    let phone = pep.phone("pixel").await;
    let mut conn = connect(&pep, &phone).await;
    let (_, shot) = pep.make_question_key("jev", "one_shot");
    // The flags are refused for the key.
    let (is_error, value) =
        pep.tool_with_key(&shot, "ask", with_flags(question(json!({})), true, false));
    assert!(is_error, "{value}");
    assert_eq!(value["error"]["code"], "interaction_not_allowed");
    let (is_error, made) = pep.tool_with_key(
        &shot,
        "ask",
        question(json!({"hint": {"suggested": "go", "confidence": 0.9, "source": "jev"}})),
    );
    assert!(!is_error, "{made}");
    let id = made["request_id"].as_str().unwrap().to_string();
    let req = first_request(&mut conn).await;
    assert_eq!(req.from.interaction, Interaction::OneShot);
    assert!(!req.allow_custom && !req.allow_ask_back);
    assert_eq!(req.hint.as_ref().unwrap().source, "jev");
    // A reply is refused, and nothing changes.
    conn.send(&ClientMsg::Reply(ReplyMsg {
        request_id: id.clone(),
        client_msg_id: "r-1".into(),
        text: "why?".into(),
    }))
    .await;
    assert_eq!(
        next_result(&mut conn, WAIT).await.status,
        AnswerStatus::AskBackNotAllowed
    );
    // The owner answers, and the key reads the answer.
    conn.send(&answer(&req, "a-1", Action::Accept, Some(&["go"]), None))
        .await;
    assert_eq!(
        next_result(&mut conn, WAIT).await.status,
        AnswerStatus::Accepted
    );
    let (_, got) = pep.tool_with_key(&shot, "get_answer", json!({"request_id": id}));
    assert_eq!(got["answer"]["choice"], json!(["go"]));
}

/// No text of a question is in the database, in clear text or in hexadecimal, and none is in the log, after a whole life:
/// the question, a reply, a turn of the agent, an answer with a text, and a second question that the owner declines.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn canary_for_question_bodies() {
    let pep = Pep::with_questions(|_| {});
    let phone = pep.phone("pixel").await;
    let mut conn = connect(&pep, &phone).await;
    let canaries = [
        "canary-q-title-5d1a",
        "canary-q-explanation-5d1a",
        "canary-q-command-5d1a",
        "canary-q-option-5d1a",
        "canary-q-hint-5d1a",
        "canary-q-reply-5d1a",
        "canary-q-say-5d1a",
        "canary-q-answer-5d1a",
    ];
    let args = json!({"title": canaries[0], "explanation": canaries[1], "command": canaries[2], "lease_s": 600,
        "input": {"type": "choice", "allow_custom": true, "allow_ask_back": true,
                  "options": [{"id": "a", "label": canaries[3]}, {"id": "b", "label": "B"}]},
        "hint": {"suggested": "a", "source": canaries[4]}});
    let made = pep.tool("ask", args);
    let id = made["request_id"].as_str().unwrap().to_string();
    let req = first_request(&mut conn).await;
    assert_eq!(
        req.title.as_deref(),
        Some(canaries[0]),
        "the phone gets the text"
    );
    conn.send(&ClientMsg::Reply(ReplyMsg {
        request_id: id.clone(),
        client_msg_id: "m-1".into(),
        text: canaries[5].into(),
    }))
    .await;
    assert_eq!(
        next_result(&mut conn, WAIT).await.status,
        AnswerStatus::Accepted
    );
    tokio::time::sleep(Duration::from_millis(2100)).await;
    pep.tool("say", json!({"request_id": id, "text": canaries[6]}));
    let message = next_message(&mut conn, WAIT).await;
    conn.receipt(&message.id).await;
    conn.send(&answer(
        &req,
        "m-2",
        Action::Accept,
        None,
        Some(canaries[7]),
    ))
    .await;
    assert_eq!(
        next_result(&mut conn, WAIT).await.status,
        AnswerStatus::Accepted
    );
    assert_eq!(read(&pep, &id)["answer"]["untrusted"]["text"], canaries[7]);

    let rows = pep.s.database_text();
    let log = pep.s.log();
    for canary in canaries {
        let hex: String = canary.bytes().map(|b| format!("{b:02x}")).collect();
        assert!(
            !rows.contains(canary),
            "the database holds {canary} in clear text"
        );
        assert!(
            !rows.contains(&hex),
            "the database holds {canary} in hexadecimal"
        );
        assert!(!log.contains(canary), "the log holds {canary}");
    }
    // The log names the events of the question, with no text.
    assert!(log.contains("\"event\":\"question_requested\""), "{log}");
    assert!(log.contains("\"event\":\"question_answered\""));
    // The question never reached a service: the PEP reads no question as an approval.
    let subject = pep.subject().replace(':', "%3A");
    let reply = pep.pdp(
        "GET",
        &format!("/approval/v1/requests/{id}?subject={subject}"),
        "",
    );
    assert_eq!(reply.status, 404, "{}", reply.body);
}
