//! Abuse and load tests of the questions (lot L15): the real binary, an agent that asks through `/mcp`, and a Rust phone
//! that speaks Noise to the session of the portal. Each test pushes a flood or a huge value at the portal and checks
//! that the rows, the events and the frames stay inside the limits, and that nothing of a question leaks.
//!
//! The floods are small enough for a normal run (tens of calls): the limits are set low in the file, so a small flood
//! reaches them. Run them on PostgreSQL with `INBOX_E2E_BACKEND=postgres`.

mod common;

use std::collections::HashSet;
use std::time::{Duration, Instant};

use common::approvals::{next_message, next_request, next_result, Pep};
use common::{Conn, Phone, Recv};
use inbox_core::proto::{
    Action, AnswerMsg, AnswerStatus, ClientMsg, ItemState, ReplyMsg, RequestMsg, ServerMsg,
};
use serde_json::{json, Value};

const WAIT: Duration = Duration::from_secs(20);

fn edit(s: &common::Server, from: &str, to: &str) {
    let text = std::fs::read_to_string(s.config()).unwrap();
    assert!(text.contains(from), "{from} is not in the file");
    std::fs::write(s.config(), text.replacen(from, to, 1)).unwrap();
}

fn number(pep: &Pep, sql: &str) -> i64 {
    pep.s
        .scalar(sql)
        .unwrap_or_else(|| "0".into())
        .parse()
        .unwrap()
}

async fn connect(pep: &Pep, phone: &Phone) -> Conn {
    let (conn, _) = Conn::open(pep.s.port, phone)
        .await
        .expect("the session opens");
    conn
}

/// A question with a distinct idempotency key. `n` makes the title and the key.
fn question(n: u32) -> Value {
    json!({"title": format!("Purge table {n}?"), "lease_s": 600, "idempotency_key": format!("load-{n}"),
           "input": {"type": "choice", "allow_custom": true, "allow_ask_back": true,
                     "options": [{"id": "go", "label": "Run it"}, {"id": "wait", "label": "Wait"}]}})
}

/// The request frames that arrive within `window`. The phone sends the receipt of each one.
async fn drain_requests(c: &mut Conn, window: Duration) -> Vec<RequestMsg> {
    let deadline = Instant::now() + window;
    let mut seen: HashSet<String> = HashSet::new();
    let mut out = Vec::new();
    while Instant::now() < deadline {
        match c
            .recv(deadline.saturating_duration_since(Instant::now()))
            .await
        {
            Recv::Msg(ServerMsg::Request(r)) => {
                c.receipt(&r.id).await;
                c.seen.insert(r.id.clone());
                if seen.insert(r.id.clone()) {
                    out.push(*r);
                }
            }
            Recv::Closed(code) => panic!("the session closed: {code:?}"),
            _ => {}
        }
    }
    out
}

/// The sizes of the tables that a flood could grow.
fn sizes(pep: &Pep) -> [i64; 6] {
    [
        number(pep, "SELECT COUNT(*) FROM items"),
        number(pep, "SELECT COUNT(*) FROM item_events"),
        number(pep, "SELECT COUNT(*) FROM turns"),
        number(pep, "SELECT COUNT(*) FROM agents"),
        number(pep, "SELECT COUNT(*) FROM agent_events"),
        number(pep, "SELECT COUNT(*) FROM alerts"),
    ]
}

/// A POST that the server may refuse before it reads the whole body: the writer ignores a broken pipe. The result is the
/// status and the body.
fn post_big(port: u16, path: &str, headers: &[(&str, String)], body: &str) -> (u16, String) {
    use std::io::{Read, Write};
    let mut s = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(30))).unwrap();
    let mut head = format!(
        "POST {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\nContent-Length: {}\r\n",
        body.len()
    );
    for (k, v) in headers {
        head.push_str(&format!("{k}: {v}\r\n"));
    }
    head.push_str("\r\n");
    let _ = s.write_all(head.as_bytes());
    let _ = s.write_all(body.as_bytes());
    let mut out = Vec::new();
    let _ = s.read_to_end(&mut out);
    let text = String::from_utf8_lossy(&out).to_string();
    let status = text
        .split_whitespace()
        .nth(1)
        .and_then(|c| c.parse().ok())
        .unwrap_or(0);
    let body = text
        .split_once("\r\n\r\n")
        .map_or(String::new(), |(_, b)| b.to_string());
    (status, body)
}

/// A call of `ask` that may be refused at the edge (a status above 399) or by the tool (`isError`). The result is the
/// status, whether it was refused, and the text of the refusal.
fn raw_ask(pep: &Pep, args: Value) -> (u16, bool, String) {
    let body = json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call",
                      "params": {"name": "ask", "arguments": args}})
    .to_string();
    let (status, text) = post_big(
        pep.s.port,
        "/mcp",
        &[
            ("Authorization", format!("Bearer {}", pep.key_text)),
            ("Content-Type", "application/json".into()),
        ],
        &body,
    );
    let refused = status >= 400
        || serde_json::from_str::<Value>(&text)
            .map(|v| v["result"]["isError"] == true || v.get("error").is_some())
            .unwrap_or(false);
    (status, refused, text)
}

fn code_of(value: &Value) -> &str {
    value["error"]["code"].as_str().unwrap_or("")
}

/// A key that asks more questions than `max_open_per_key` allows: the limit holds, the rows, the events and the frames
/// stop at the limit, a repeat of a call is free, and an answer gives the slot back.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn many_open_questions_per_key_stop_at_max_open_per_key() {
    let pep = Pep::with_questions(|s| edit(s, "max_open_per_key = 20", "max_open_per_key = 5"));
    let phone = pep.phone("pixel").await;
    let mut conn = connect(&pep, &phone).await;
    let (mut made, mut refused) = (Vec::new(), 0);
    let mut one_item = [0i64; 6];
    for n in 0..40 {
        let (is_error, value) = pep.tool_with_key(&pep.key_text, "ask", question(n));
        if is_error {
            assert_eq!(code_of(&json!({"error": value["error"]})), "rate_limited");
            assert_eq!(value["error"]["retryable"], true, "{value}");
            assert!(value["error"]["retry_after"].as_u64().unwrap() >= 1);
            refused += 1;
        } else {
            made.push(value["request_id"].as_str().unwrap().to_string());
            if made.len() == 1 {
                one_item = sizes(&pep);
            }
        }
    }
    assert_eq!((made.len(), refused), (5, 35));
    let after = sizes(&pep);
    assert_eq!(after[0], 5, "five open items");
    assert_eq!(
        after[1],
        5 * one_item[1],
        "the events of a refused question are not written: {after:?}"
    );
    assert_eq!(
        after[5], 5,
        "one alert for each stored question, none for the refused ones"
    );
    assert_eq!(
        number(
            &pep,
            "SELECT COUNT(*) FROM alerts WHERE title <> 'Question' OR message IS NOT NULL OR source <> 'item'"
        ),
        0,
        "the alert of a question holds no text of the agent"
    );
    // The phone gets five frames and no more.
    let frames = drain_requests(&mut conn, Duration::from_secs(3)).await;
    assert_eq!(frames.len(), 5, "one frame for each stored question");
    // A repeat of a stored call is free: the same item, no new row.
    let (is_error, again) = pep.tool_with_key(&pep.key_text, "ask", question(0));
    assert!(!is_error, "{again}");
    assert_eq!(again["request_id"], json!(made[0]));
    assert_eq!(sizes(&pep), after);
    // An answer gives the slot back: the next question is stored, and then the limit holds again.
    conn.send(&ClientMsg::Answer(AnswerMsg {
        request_id: made[0].clone(),
        client_msg_id: "m-slot".into(),
        action: Action::Accept,
        shown_effect: None,
        choice: Some(vec!["go".into()]),
        text: None,
        intent: None,
        answers: None,
    }))
    .await;
    assert_eq!(
        next_result(&mut conn, WAIT).await.status,
        AnswerStatus::Accepted
    );
    let (is_error, next) = pep.tool_with_key(&pep.key_text, "ask", question(100));
    assert!(!is_error, "{next}");
    let (is_error, full) = pep.tool_with_key(&pep.key_text, "ask", question(101));
    assert!(is_error, "{full}");
    assert_eq!(number(&pep, "SELECT COUNT(*) FROM items"), 6);
}

/// The budget `questions` of a key: one unit for each new question. The excess gets `429 rate_limited` with the seconds
/// to wait, and it writes nothing.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_flood_of_asks_hits_the_budget_questions_and_writes_nothing() {
    let pep = Pep::with_questions(|_| {});
    let phone = pep.phone("pixel").await;
    let mut conn = connect(&pep, &phone).await;
    let (_, key) = pep.make_question_key_with(
        "budget-key",
        "conversational",
        "&grant2=access+on+service%3Aportal&limit2.questions.hour=3",
    );
    let mut made = 0;
    for n in 0..15 {
        let (is_error, value) = pep.tool_with_key(&key, "ask", question(n));
        if is_error {
            assert_eq!(code_of(&json!({"error": value["error"]})), "rate_limited");
            assert!(value["error"]["retry_after"].as_u64().unwrap() >= 1);
        } else {
            made += 1;
        }
    }
    assert_eq!(made, 3, "the budget of the hour is 3");
    assert_eq!(number(&pep, "SELECT COUNT(*) FROM items"), 3);
    assert_eq!(
        drain_requests(&mut conn, Duration::from_secs(2))
            .await
            .len(),
        3
    );
    // A repeat of a stored call takes no unit.
    let (is_error, value) = pep.tool_with_key(&key, "ask", question(0));
    assert!(!is_error, "{value}");
}

/// The flood of `ask` for the two tests below: calls without a token, with a good token and with a bad token. `agents`
/// names the User-Agent of each call.
fn flood(pep: &Pep, first: u32, token: &mut Option<String>, agent: &dyn Fn(u32) -> String) {
    for n in first..first + 30 {
        // A call without a token makes an agent or goes to the bucket. A call with a token asks as that agent. A bad
        // token is refused with no row.
        let used = match n % 3 {
            0 => None,
            1 => token.clone(),
            _ => Some("agt_00000000000000000000000000000000".to_string()),
        };
        let (is_error, value) = pep.tool_as(used.as_deref(), &agent(n), "ask", question(n));
        if !is_error && token.is_none() {
            if let Some(t) = value["agent_token"].as_str() {
                *token = Some(t.to_string());
            }
        }
    }
}

fn agents_pep() -> Pep {
    Pep::with_questions(|s| {
        edit(s, "max_open_per_key = 20", "max_open_per_key = 5");
        s.add_to_inbox("agents { max_per_key = 3 new_per_hour = 10 }");
    })
}

/// A flood of `ask` without a token and with a token. The agents of the key stop at `max_per_key` (the others go to
/// the bucket), the open items stop at `max_open_per_key`, and a second flood adds no row, no event and no alert.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_flood_of_asks_with_and_without_agent_tokens_keeps_rows_events_and_frames_bounded() {
    let pep = agents_pep();
    let phone = pep.phone("pixel").await;
    let mut conn = connect(&pep, &phone).await;
    let mut token: Option<String> = None;
    flood(&pep, 0, &mut token, &|_| "flood-agent/1".to_string());
    let first = sizes(&pep);
    assert!(first[0] <= 5, "open items stop at the limit: {first:?}");
    assert!(
        first[3] <= 3 + 1,
        "agents stop at max_per_key and the bucket: {first:?}"
    );
    let frames = drain_requests(&mut conn, Duration::from_secs(3)).await;
    assert_eq!(frames.len() as i64, first[0], "one frame for each item");
    // The second flood meets the same limits: nothing grows.
    flood(&pep, 100, &mut token, &|_| "flood-agent/1".to_string());
    assert_eq!(
        sizes(&pep),
        first,
        "a second flood adds no row, no event and no alert"
    );
    assert!(drain_requests(&mut conn, Duration::from_secs(2))
        .await
        .is_empty());
}

/// FINDING (reported to the coordinator, not fixed in lot L15): the hints of an agent keep the last `User-Agent` and
/// the last client. A caller that changes its `User-Agent` at each call changes the hints at each call, and each change
/// raises `rev`, so the portal writes one row in `alerts` and one delivery for each phone for each call. No limit stops
/// it: the key is valid, and the refused `ask` still updates the hints. Run this test with `--ignored` to see it fail.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "finding: a flipping User-Agent writes one alert row for each call (hints of an agent)"]
async fn a_flood_that_flips_the_user_agent_writes_no_alert_for_each_call() {
    let pep = agents_pep();
    let phone = pep.phone("pixel").await;
    let _conn = connect(&pep, &phone).await;
    let mut token: Option<String> = None;
    flood(&pep, 0, &mut token, &|n| format!("flood-agent/{}", n % 7));
    let first = sizes(&pep);
    flood(&pep, 100, &mut token, &|n| format!("flood-agent/{}", n % 7));
    assert_eq!(sizes(&pep), first, "a second flood adds nothing");
}

/// Huge or malformed values are refused before they reach the storage: no row, no event, no frame, and the database
/// holds no byte of them.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_huge_explanation_options_and_forms_are_refused_before_storage() {
    let pep = Pep::with_questions(|_| {});
    let phone = pep.phone("pixel").await;
    let mut conn = connect(&pep, &phone).await;
    let before = sizes(&pep);
    let big = |what: &str, n: usize| format!("canary-huge-{what}-{}", "x".repeat(n));
    let options = |label: String| json!([{"id": "a", "label": label}, {"id": "b", "label": "B"}]);
    let many: Vec<Value> = (0..200)
        .map(|i| json!({"id": format!("o{i}"), "label": format!("canary-huge-many-{i}")}))
        .collect();
    let form = |n: usize| {
        let questions: Vec<Value> = (0..n)
            .map(|i| json!({"id": format!("q{i}"), "header": "H", "text": big("form", 300), "type": "text"}))
            .collect();
        json!({"title": "form", "lease_s": 60, "input": {"type": "form", "questions": questions}})
    };
    let choice = |extra: Value| {
        let mut args = question(9000);
        for (k, v) in extra.as_object().unwrap() {
            args[k] = v.clone();
        }
        args
    };
    let cases = vec![
        (
            "explanation 100 KB",
            choice(json!({"explanation": big("explanation", 100_000)})),
        ),
        (
            "explanation 2001",
            choice(json!({"explanation": big("explanation", 2_001)})),
        ),
        (
            "command 1 MB",
            choice(json!({"command": big("command", 1_000_000)})),
        ),
        (
            "title 10 KB",
            choice(json!({"title": big("title", 10_000)})),
        ),
        (
            "option label 100 KB",
            choice(json!({"input": {"type": "choice", "options": options(big("label", 100_000))}})),
        ),
        (
            "200 options",
            choice(json!({"input": {"type": "choice", "options": many}})),
        ),
        ("form of 5 questions", form(5)),
        (
            "hint source 100 KB",
            choice(json!({"hint": {"suggested": "a", "source": big("hint", 100_000)}})),
        ),
        (
            "placeholder 100 KB",
            choice(json!({"input": {"type": "text", "placeholder": big("placeholder", 100_000)}})),
        ),
        (
            "idempotency key 10 KB",
            choice(json!({"idempotency_key": big("key", 10_000)})),
        ),
    ];
    for (what, args) in cases {
        let (status, refused, text) = raw_ask(&pep, args);
        assert!(
            refused,
            "{what} must be refused: {status} {}",
            &text[..text.len().min(300)]
        );
        assert!(
            !text.contains("canary-huge"),
            "{what}: the answer repeats the value: {}",
            &text[..text.len().min(300)]
        );
        if status == 200 {
            let value: Value = serde_json::from_str(&text).unwrap();
            let error = &value["result"]["structuredContent"]["error"];
            assert_eq!(error["retryable"], false, "{what}: {value}");
        }
    }
    // A body above the limit of the listener is refused at the edge, as a client error.
    let (status, refused, text) =
        raw_ask(&pep, choice(json!({"explanation": "x".repeat(4_000_000)})));
    assert!(
        refused,
        "a 4 MB body: {status} {}",
        &text[..text.len().min(200)]
    );
    assert_eq!(sizes(&pep), before, "nothing was stored");
    assert!(
        !pep.s.database_text().contains("canary-huge"),
        "no byte of the refused values is in the database"
    );
    assert!(drain_requests(&mut conn, Duration::from_secs(2))
        .await
        .is_empty());
    assert!(
        !pep.s.log().contains("canary-huge"),
        "the log holds no value"
    );
}

/// A phone that floods answers: a wrong answer gets a result and writes nothing, the replies stop at `max_turns`, the
/// answer that fits ends the question one time, and the answers after it change nothing. Each message gets one result.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_flood_of_answers_from_a_phone_is_bounded() {
    let pep = Pep::with_questions(|_| {});
    let phone = pep.phone("pixel").await;
    let mut conn = connect(&pep, &phone).await;
    let (is_error, made) = pep.tool_with_key(&pep.key_text, "ask", question(1));
    assert!(!is_error, "{made}");
    let id = made["request_id"].as_str().unwrap().to_string();
    let req = next_request(&mut conn, WAIT).await;
    conn.receipt(&req.id).await;
    let answer = |n: u32, choice: Option<&str>, text: Option<&str>| {
        ClientMsg::Answer(AnswerMsg {
            request_id: id.clone(),
            client_msg_id: format!("a-{n:04}"),
            action: Action::Accept,
            shown_effect: None,
            choice: choice.map(|c| vec![c.to_string()]),
            text: text.map(str::to_string),
            intent: None,
            answers: None,
        })
    };
    let sent_wrong = 60;
    for n in 0..sent_wrong {
        conn.send(&answer(n, Some("not-an-option"), None)).await;
    }
    let mut statuses = Vec::new();
    for _ in 0..sent_wrong {
        statuses.push(next_result(&mut conn, WAIT).await.status);
    }
    assert!(
        statuses.iter().all(|s| *s != AnswerStatus::Accepted),
        "a wrong answer is never accepted: {statuses:?}"
    );
    assert_eq!(
        number(
            &pep,
            &format!("SELECT COUNT(*) FROM turns WHERE request_id = '{id}'")
        ),
        0,
        "a wrong answer writes no turn"
    );
    assert_eq!(
        number(&pep, "SELECT COUNT(*) FROM items WHERE state = 'pending'"),
        1
    );
    // The replies stop at the limit of turns of a thread.
    let sent_replies = 70u32;
    for n in 0..sent_replies {
        conn.send(&ClientMsg::Reply(ReplyMsg {
            request_id: id.clone(),
            client_msg_id: format!("r-{n:04}"),
            text: format!("why {n}?"),
        }))
        .await;
    }
    let mut accepted = 0;
    for _ in 0..sent_replies {
        if next_result(&mut conn, WAIT).await.status == AnswerStatus::Accepted {
            accepted += 1;
        }
    }
    let turns = number(
        &pep,
        &format!("SELECT COUNT(*) FROM turns WHERE request_id = '{id}'"),
    );
    assert_eq!(turns, accepted, "each accepted reply is one turn");
    assert!(
        turns <= 40,
        "the turns of a thread stop at max_turns (40): {turns}"
    );
    // The answer that fits ends the question one time. The answers after it change nothing.
    conn.send(&answer(1000, Some("go"), None)).await;
    let result = next_result(&mut conn, WAIT).await;
    assert!(
        matches!(
            result.status,
            AnswerStatus::Accepted | AnswerStatus::AlreadyAnswered
        ),
        "{:?}",
        result.status
    );
    let after_first = sizes(&pep);
    for n in 1001..1101 {
        conn.send(&answer(n, Some("wait"), None)).await;
    }
    for _ in 1001..1101 {
        let r = next_result(&mut conn, WAIT).await;
        assert!(
            !matches!(r.status, AnswerStatus::Accepted),
            "a second answer is never accepted: {:?}",
            r.status
        );
        assert_ne!(r.state, Some(ItemState::Pending));
    }
    assert_eq!(sizes(&pep), after_first, "100 more answers change nothing");
    let (_, got) = pep.tool_with_key(&pep.key_text, "get_answer", json!({"request_id": id}));
    assert_eq!(got["status"], "answered", "{got}");
    assert_eq!(got["answer"]["choice"], json!(["go"]), "{got}");
}

/// No part of a question is in a place that a leak would reach: the log, the metrics, the tables (the alerts too), the
/// errors that the agent reads, the answer to the service, and the requests that the portal sends to a service. The
/// parts are the title, the explanation, the command, the options, the hint, a form, the answers and the turns.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn canary_for_question_bodies_in_every_place() {
    let pep = Pep::with_questions(|s| edit(s, "max_open_per_key = 20", "max_open_per_key = 4"));
    let phone = pep.phone("pixel").await;
    let mut conn = connect(&pep, &phone).await;
    let c = |what: &str| format!("canary-load-{what}-7c42e1");
    let canaries = [
        c("title"),
        c("explanation"),
        c("command"),
        c("option-label"),
        c("option-description"),
        c("hint-source"),
        "cnry-hdr-7c42".to_string(),
        c("form-text"),
        c("reply"),
        c("say"),
        c("answer-text"),
        c("form-answer"),
        c("option-id"),
        c("hint-suggested"),
    ];
    let canary_option_id = canaries[12].clone();
    let args = json!({
        "title": canaries[0], "explanation": canaries[1], "command": canaries[2], "lease_s": 600,
        "input": {"type": "choice", "allow_custom": true, "allow_ask_back": true,
                  "options": [{"id": canary_option_id, "label": canaries[3], "description": canaries[4]},
                              {"id": "b", "label": "B"}]},
        "hint": {"suggested": canary_option_id, "source": canaries[5], "confidence": 0.5}
    });
    let (is_error, made) = pep.tool_with_key(&pep.key_text, "ask", args);
    assert!(!is_error, "{made}");
    let id = made["request_id"].as_str().unwrap().to_string();
    let form = json!({"title": "form question", "lease_s": 600, "idempotency_key": "form-1",
        "input": {"type": "form", "questions": [
            {"id": "who", "header": canaries[6], "text": canaries[7], "type": "text"}]}});
    let (is_error, made_form) = pep.tool_with_key(&pep.key_text, "ask", form);
    assert!(!is_error, "{made_form}");
    let form_id = made_form["request_id"].as_str().unwrap().to_string();
    let mut frames = Vec::new();
    while frames.len() < 2 {
        let r = next_request(&mut conn, WAIT).await;
        conn.receipt(&r.id).await;
        frames.push(r);
    }
    let req = frames.iter().find(|r| r.request_id == id).unwrap().clone();
    assert_eq!(req.title.as_deref(), Some(canaries[0].as_str()));
    // The thread: a reply of the owner, a turn of the agent, an answer with a text.
    conn.send(&ClientMsg::Reply(ReplyMsg {
        request_id: id.clone(),
        client_msg_id: "m-1".into(),
        text: canaries[8].clone(),
    }))
    .await;
    assert_eq!(
        next_result(&mut conn, WAIT).await.status,
        AnswerStatus::Accepted
    );
    tokio::time::sleep(Duration::from_millis(2100)).await;
    let (is_error, said) = pep.tool_with_key(
        &pep.key_text,
        "say",
        json!({"request_id": id, "text": canaries[9]}),
    );
    assert!(!is_error, "{said}");
    let message = next_message(&mut conn, WAIT).await;
    conn.receipt(&message.id).await;
    conn.send(&ClientMsg::Answer(AnswerMsg {
        request_id: id.clone(),
        client_msg_id: "m-2".into(),
        action: Action::Accept,
        shown_effect: None,
        choice: None,
        text: Some(canaries[10].clone()),
        intent: None,
        answers: None,
    }))
    .await;
    assert_eq!(
        next_result(&mut conn, WAIT).await.status,
        AnswerStatus::Accepted
    );
    // The second question is declined by the owner: no text, only the end.
    conn.send(&ClientMsg::Answer(AnswerMsg {
        request_id: form_id.clone(),
        client_msg_id: "m-3".into(),
        action: Action::Decline,
        shown_effect: None,
        choice: None,
        text: None,
        intent: None,
        answers: None,
    }))
    .await;
    assert_eq!(
        next_result(&mut conn, WAIT).await.status,
        AnswerStatus::Accepted
    );
    // The refusals that echo the input: the error never repeats a text. Each argument holds a canary.
    let mut errors = Vec::new();
    for (what, args) in [
        (
            "unknown member",
            json!({"title": canaries[0], "lease_s": 60, canaries[13].clone(): canaries[1],
                   "input": {"type": "none"}}),
        ),
        (
            "bad flag",
            json!({"title": canaries[0], "lease_s": 60,
                   "input": {"type": "text", "allow_custom": canaries[3]}}),
        ),
        (
            "flags that do not fit",
            json!({"title": canaries[0], "lease_s": 60, "explanation": canaries[1],
                   "input": {"type": "none", "allow_ask_back": true}}),
        ),
        (
            "bad characters in the command",
            json!({"title": canaries[0], "lease_s": 60, "command": format!("{}\u{202e}", canaries[2]),
                   "input": {"type": "none"}}),
        ),
        (
            "an answer that does not fit",
            json!({"title": canaries[0], "lease_s": 60,
                   "input": {"type": "choice", "options": [{"id": "a", "label": canaries[3]}]}}),
        ),
    ] {
        let (is_error, value) = pep.tool_with_key(&pep.key_text, "ask", args);
        assert!(is_error, "{what} must be refused: {value}");
        errors.push(value.to_string());
    }
    let (_, unknown) = pep.tool_with_key(
        &pep.key_text,
        "get_answer",
        json!({"request_id": canaries[0]}),
    );
    errors.push(unknown.to_string());
    // The service side: the PDP answers a service about the subject of the key, and the verdict route of the approvals
    // does not know a question. The owner key sees its own answer, but no service reads it.
    let subject = pep.subject().replace(':', "%3A");
    let verdict = pep.pdp(
        "GET",
        &format!("/approval/v1/requests/{id}?subject={subject}"),
        "",
    );
    assert_eq!(verdict.status, 404, "{}", verdict.body);
    errors.push(verdict.body.clone());
    let evaluation = pep.evaluation(&common::approvals::command(1).effect, false);
    errors.push(evaluation.to_string());

    let rows = pep.s.database_text();
    let log = pep.s.log();
    let (_, _, metrics) = common::http_full(
        pep.s.port,
        &format!("127.0.0.1:{}", pep.s.port),
        "GET",
        "/metrics",
        &[],
        "",
    );
    let service = String::from_utf8_lossy(&pep.service_tap.lock().unwrap()).to_string();
    assert!(
        !service.is_empty(),
        "the tap saw the requests of the portal to the fake services"
    );
    for canary in &canaries {
        let hex: String = canary.bytes().map(|b| format!("{b:02x}")).collect();
        assert!(
            !rows.contains(canary.as_str()),
            "the database (and the alerts) hold {canary} in clear text"
        );
        assert!(
            !rows.contains(&hex),
            "the database holds {canary} in hexadecimal"
        );
        assert!(!log.contains(canary.as_str()), "the log holds {canary}");
        assert!(
            !metrics.contains(canary.as_str()),
            "the metrics hold {canary}"
        );
        assert!(
            !service.contains(canary.as_str()),
            "a service received {canary}"
        );
        for error in &errors {
            assert!(
                !error.contains(canary.as_str()),
                "an error holds {canary}: {error}"
            );
        }
    }
    // The portal named the events, and counted them, without a text.
    assert!(log.contains("\"event\":\"question_requested\""), "{log}");
    assert!(metrics.contains("portal_item"), "{metrics}");
    assert_eq!(
        number(
            &pep,
            "SELECT COUNT(*) FROM alerts WHERE title <> 'Question' OR message IS NOT NULL OR source <> 'item'"
        ),
        0,
        "an alert of a question has no text of the agent"
    );
    assert_eq!(number(&pep, "SELECT COUNT(*) FROM approvals"), 0);
}
