//! Golden frames of protocol 5. The files under `fixtures/frames` are the contract between the
//! server, the Android app and the tests: the Kotlin side reads the same files.
//!
//! `server/` and `client/` hold valid frames, one file for each shape. `invalid/server/` and
//! `invalid/client/` hold frames that a parser MUST refuse.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use inbox_core::effect;
use inbox_core::noise::{self, Initiator, MAX_PLAINTEXT, Responder};
use inbox_core::proto::*;
use serde_json::{Value, json};

fn dir(sub: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/frames").join(sub)
}

/// `(file name, bytes, value)` of every `.json` file of a folder, sorted by name.
fn files(sub: &str) -> Vec<(String, Vec<u8>, Value)> {
    let mut out: Vec<_> = std::fs::read_dir(dir(sub))
        .unwrap_or_else(|e| panic!("{sub}: {e}"))
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|x| x == "json"))
        .map(|p| {
            let bytes = std::fs::read(&p).unwrap();
            let value = serde_json::from_slice(&bytes).unwrap();
            (p.file_name().unwrap().to_string_lossy().into_owned(), bytes, value)
        })
        .collect();
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

fn tags(sub: &str) -> BTreeSet<String> {
    files(sub).iter().map(|f| f.2["t"].as_str().unwrap().to_string()).collect()
}

#[test]
fn frames_round_trip_golden() {
    let server = files("server");
    let client = files("client");
    assert!(server.len() >= 15 && client.len() >= 12);
    for (name, bytes, value) in &server {
        let msg = ServerMsg::decode(bytes).unwrap_or_else(|e| panic!("server/{name}: {e}"));
        let again = serde_json::to_value(&msg).unwrap();
        assert_eq!(&again, value, "server/{name}: the frame does not round trip");
        // Encode, then decode the bytes that the server would send.
        let wire = msg.encode().unwrap();
        assert_eq!(ServerMsg::decode(&wire).unwrap(), msg, "server/{name}");
    }
    for (name, bytes, value) in &client {
        let msg = ClientMsg::decode(bytes).unwrap_or_else(|e| panic!("client/{name}: {e}"));
        assert_eq!(&serde_json::to_value(&msg).unwrap(), value, "client/{name}");
        let wire = msg.encode().unwrap();
        assert_eq!(ClientMsg::decode(&wire).unwrap(), msg, "client/{name}");
    }
}

#[test]
fn the_golden_files_cover_every_frame_in_both_directions() {
    let server: BTreeSet<String> = [
        "welcome",
        "alert",
        "request",
        "message",
        "settled",
        "digest",
        "answer_result",
        "agent",
        "agent_result",
        "subscriptions",
        "pong",
        "error",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    let client: BTreeSet<String> = [
        "ready",
        "ack",
        "subscribe",
        "unsubscribe",
        "ping",
        "answer",
        "reply",
        "refresh",
        "sync",
        "set_prefs",
        "agent_do",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    assert_eq!(tags("server"), server);
    assert_eq!(tags("client"), client);
    // Both kinds of `request`, and both lease modes.
    let kinds: BTreeSet<String> = files("server")
        .iter()
        .filter(|f| f.2["t"] == "request")
        .map(|f| f.2["kind"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(kinds.len(), 2);
    // An invalid frame exists for most frames, in each direction.
    assert!(tags("invalid/server").len() >= 7);
    assert!(tags("invalid/client").len() >= 5);
}

#[test]
fn invalid_frames_are_refused() {
    for (name, bytes, _) in files("invalid/server") {
        assert!(ServerMsg::decode(&bytes).is_err(), "invalid/server/{name} was accepted");
    }
    for (name, bytes, _) in files("invalid/client") {
        assert!(ClientMsg::decode(&bytes).is_err(), "invalid/client/{name} was accepted");
    }
}

/// Adds an unknown member to every object of a frame, one object at a time. The free form
/// `op` of an approval is the only object that takes any member.
fn with_extra_member(
    v: &Value,
    path: &mut Vec<String>,
    out: &mut Vec<(String, Value)>,
    root: &Value,
) {
    match v {
        Value::Object(map) => {
            if path.last().is_none_or(|p| p != "op") {
                let mut copy = root.clone();
                let mut cur = &mut copy;
                for p in path.iter() {
                    cur = match cur {
                        Value::Array(a) => &mut a[p.parse::<usize>().unwrap()],
                        other => other.get_mut(p.as_str()).unwrap(),
                    };
                }
                cur.as_object_mut().unwrap().insert("zz".into(), json!(1));
                out.push((path.join("/"), copy));
            }
            for (k, child) in map {
                path.push(k.clone());
                with_extra_member(child, path, out, root);
                path.pop();
            }
        }
        Value::Array(items) => {
            for (i, child) in items.iter().enumerate() {
                path.push(i.to_string());
                with_extra_member(child, path, out, root);
                path.pop();
            }
        }
        _ => {}
    }
}

#[test]
fn every_frame_rejects_unknown_field() {
    let mut tried = 0;
    for (name, _, value) in files("server") {
        // A tuple of a view (`["Host","/host"]`) is an array: it has no member to add.
        let mut cases = Vec::new();
        with_extra_member(&value, &mut vec![], &mut cases, &value);
        for (path, frame) in cases {
            tried += 1;
            let bytes = serde_json::to_vec(&frame).unwrap();
            assert!(
                ServerMsg::decode(&bytes).is_err(),
                "server/{name} accepts an unknown member at '{path}'"
            );
        }
    }
    for (name, _, value) in files("client") {
        let mut cases = Vec::new();
        with_extra_member(&value, &mut vec![], &mut cases, &value);
        for (path, frame) in cases {
            tried += 1;
            let bytes = serde_json::to_vec(&frame).unwrap();
            assert!(
                ClientMsg::decode(&bytes).is_err(),
                "client/{name} accepts an unknown member at '{path}'"
            );
        }
    }
    assert!(tried > 60, "{tried}");
    // A duplicate member is refused too, in a frame and in `op`.
    let dup = r#"{"t":"ack","id":"00000000000000000000000000000000","stage":"received","stage":"received"}"#;
    assert!(ClientMsg::decode(dup.as_bytes()).is_err());
    let approval = std::fs::read_to_string(dir("server/request-approval.json")).unwrap();
    let dup_op = approval.replace("\"host\": \"prod\",", "\"host\": \"prod\", \"host\": \"prod\",");
    assert_ne!(dup_op, approval);
    assert!(ServerMsg::decode(dup_op.as_bytes()).is_err());
}

#[test]
fn title_with_newline_or_control_rejected() {
    let question =
        files("server").into_iter().find(|f| f.0 == "request-question-choice.json").unwrap().2;
    let alert = files("server").into_iter().find(|f| f.0 == "alert.json").unwrap().2;
    for bad in ["a\nb", "a\rb", "a\u{7}b", "a\u{0}b", "a\u{7f}b", "a\u{85}b", "a\u{2028}b", "\t"] {
        let mut q = question.clone();
        q["title"] = json!(bad);
        assert!(ServerMsg::decode(&serde_json::to_vec(&q).unwrap()).is_err(), "question {bad:?}");
        let mut a = alert.clone();
        a["title"] = json!(bad);
        // U+2028 is not a control character of Unicode (category Zl). The alert keeps the old rule
        // (`char::is_control`). The title of a question refuses it, as it breaks a line.
        let expect_err = bad != "a\u{2028}b";
        assert_eq!(
            ServerMsg::decode(&serde_json::to_vec(&a).unwrap()).is_err(),
            expect_err,
            "alert {bad:?}"
        );
    }
    // The title of a question has no bidi control either.
    for bidi in ['\u{202A}', '\u{202E}', '\u{2066}', '\u{2069}'] {
        let mut q = question.clone();
        q["title"] = json!(format!("a{bidi}b"));
        assert!(ServerMsg::decode(&serde_json::to_vec(&q).unwrap()).is_err(), "{bidi:?}");
    }
    // 128 characters pass, 129 do not. A character counts once, whatever its size in bytes.
    let mut q = question.clone();
    q["title"] = json!("é".repeat(128));
    assert!(ServerMsg::decode(&serde_json::to_vec(&q).unwrap()).is_ok());
    q["title"] = json!("é".repeat(129));
    assert!(ServerMsg::decode(&serde_json::to_vec(&q).unwrap()).is_err());
}

fn max_question() -> RequestMsg {
    RequestMsg {
        id: "a".repeat(32),
        request_id: "b".repeat(32),
        kind: Kind::Question,
        chain_id: "b".repeat(32),
        seq: u64::from(u32::MAX),
        from: Sender {
            label: "l".repeat(64),
            key_name: "k".repeat(64),
            owner: "o".repeat(64),
            via: Via::Mcp,
            interaction: Interaction::Conversational,
            agent_id: "a".repeat(32),
            agent_kind: AgentKind::Agent,
            agent_state: AgentState::New,
        },
        urgency: Urgency::High,
        quiet: true,
        created_at_ms: i64::MAX,
        chain_created_at_ms: i64::MAX,
        agent_seen_at_ms: i64::MAX,
        lease_mode: LeaseMode::Lease,
        lease_until_ms: Some(i64::MAX),
        until_at_ms: None,
        allow_custom: true,
        allow_ask_back: true,
        supersedes: Some("c".repeat(32)),
        previous_id: Some("d".repeat(32)),
        agent_label: Some("l".repeat(MAX_AGENT_LABEL_CHARS)),
        title: Some("t".repeat(MAX_TITLE_CHARS)),
        explanation: Some("e".repeat(MAX_EXPLANATION_CHARS)),
        explanation_format: Some(ExplanationFormat::Markdown),
        command: Some(Command {
            text: "c".repeat(MAX_COMMAND_BYTES),
            label: Some("l".repeat(32)),
            language: Some(Language::Typescript),
        }),
        input: Some(Input {
            kind: InputType::Choice,
            multiple: true,
            min: 1,
            max: 8,
            options: (0..MAX_OPTIONS)
                .map(|i| InputOption {
                    id: format!("{i}").repeat(32).chars().take(32).collect(),
                    label: "L".repeat(MAX_OPTION_LABEL_CHARS),
                    description: Some("D".repeat(MAX_OPTION_DESCRIPTION_CHARS)),
                })
                .collect(),
            placeholder: None,
            questions: None,
        }),
        hint: Some(Hint {
            suggested: "s".repeat(32),
            confidence: Some(0.62),
            source: "x".repeat(32),
        }),
        as_of_ms: Some(i64::MAX),
        approval: None,
    }
}

/// An `agent` frame with every member at its limit. Characters of two bytes make the longest text.
fn max_agent() -> AgentMsg {
    AgentMsg {
        id: "a".repeat(32),
        agent_id: "b".repeat(32),
        rev: i64::MAX as u64,
        key_name: "k".repeat(MAX_AGENT_LABEL_CHARS),
        kind: AgentKind::Agent,
        state: AgentState::New,
        label: "l".repeat(MAX_AGENT_LABEL_CHARS),
        named: true,
        merged_into: None,
        created_at_ms: i64::MAX,
        last_seen_ms: i64::MAX,
        open: u32::MAX,
        lock_accept: true,
        observed: Some(Observed {
            says: Some("\u{e9}".repeat(MAX_AGENT_LABEL_CHARS)),
            says_changed: Some(u32::MAX),
            client: Some("\u{e9}".repeat(MAX_AGENT_CLIENT_CHARS)),
            user_agent: Some("\u{e9}".repeat(MAX_AGENT_USER_AGENT_CHARS)),
            net: Some("n".repeat(MAX_AGENT_NET_CHARS)),
            nets_seen: MAX_AGENT_NETS_SEEN,
        }),
        similar: Some(Similar {
            agent_id: "c".repeat(32),
            label: "s".repeat(MAX_AGENT_LABEL_CHARS),
        }),
    }
}

fn max_approval() -> RequestMsg {
    let mut r = max_question();
    r.kind = Kind::Approval;
    r.title = None;
    r.explanation = None;
    r.explanation_format = None;
    r.command = None;
    r.input = None;
    r.hint = None;
    r.as_of_ms = None;
    // `op` and `view` together are at most 32 KiB.
    let op = json!({"sql": "q".repeat(MAX_COMMAND_BYTES - 2000), "host": "prod"});
    let view = effect::View {
        title: "t".repeat(128),
        fields: (0..12)
            .map(|i| (format!("{i}").repeat(48).chars().take(48).collect(), "/host".to_string()))
            .collect(),
        body: "/sql".into(),
        body_label: Some("b".repeat(48)),
    };
    let effect = effect::effect_hash("s".repeat(64).as_str(), "a", "r", &op).unwrap();
    r.approval = Some(ApprovalBody {
        code: "ABCDEFGH".into(),
        service: "s".repeat(64),
        action: "a".into(),
        resource: "r".into(),
        subject: "u".repeat(128),
        effect,
        op: effect::Op::from_value(op).unwrap(),
        view,
        explanation: Some("e".repeat(MAX_EXPLANATION_CHARS)),
        explanation_format: Some(ExplanationFormat::Markdown),
        language: Some(Language::Sql),
        hint: Some(Hint { suggested: "s".repeat(32), confidence: None, source: "x".repeat(32) }),
        previous: Some(Previous { approved_at_ms: i64::MAX, consumed: true }),
        follows: Some(Follows { code: "ABCDEFGH".into(), intent: Intent::ChangeRequest }),
    });
    r
}

#[test]
fn request_fits_one_noise_message_at_max_sizes() {
    // A real Noise session, to prove that the encrypted frame fits too.
    let device = noise::generate_keypair().unwrap();
    let server = noise::generate_keypair().unwrap();
    let (init, m1) = Initiator::start(&device.private, &server.public, b"{}").unwrap();
    let (resp, _, _) = Responder::start(&server.private, &m1).unwrap();
    let (mut srv, m2) = resp.finish(b"{}").unwrap();
    let (mut phone, _) = init.finish(&m2).unwrap();

    let frames = [
        ServerMsg::Request(Box::new(max_question())),
        ServerMsg::Request(Box::new(max_approval())),
        ServerMsg::Message(MessageMsg {
            id: "a".repeat(32),
            request_id: "b".repeat(32),
            seq: u64::MAX >> 11,
            by: Author::Agent,
            created_at_ms: i64::MAX,
            explanation: Some("e".repeat(MAX_EXPLANATION_CHARS)),
            explanation_format: Some(ExplanationFormat::Markdown),
            command: Some(Command {
                text: "c".repeat(MAX_COMMAND_BYTES),
                label: Some("l".repeat(32)),
                language: Some(Language::Bash),
            }),
            text: None,
        }),
        ServerMsg::Digest(DigestMsg {
            id: "a".repeat(32),
            count: u32::MAX,
            oldest_created_at_ms: i64::MAX,
            ids: vec!["f".repeat(32); MAX_DIGEST_IDS],
        }),
        ServerMsg::Agent(Box::new(max_agent())),
        ServerMsg::AgentResult(AgentResultMsg {
            client_msg_id: "m".repeat(64),
            agent_id: "a".repeat(32),
            status: AgentResultStatus::NotEnabled,
        }),
    ];
    for frame in frames {
        let bytes = frame.encode().unwrap_or_else(|e| panic!("{e}"));
        assert!(bytes.len() <= MAX_PLAINTEXT, "{} bytes", bytes.len());
        let sealed = srv.encrypt(&bytes).unwrap();
        assert!(sealed.len() <= noise::MAX_MESSAGE);
        let opened = phone.decrypt(&sealed).unwrap();
        assert_eq!(ServerMsg::decode(&opened).unwrap(), frame);
    }
    // The client side: a `sync` of 500 entries and an answer with a text of the maximum length.
    let sync = ClientMsg::Sync {
        open: (0..MAX_SYNC_ENTRIES)
            .map(|_| OpenRef { request_id: "a".repeat(32), seq: u64::MAX >> 11 })
            .collect(),
        agents: Some(
            (0..MAX_SYNC_AGENTS)
                .map(|i| AgentRef { agent_id: format!("{i:032x}"), rev: i64::MAX as u64 })
                .collect(),
        ),
    };
    // The most items and the most agents together fit in one Noise message.
    assert!(sync.encode().unwrap().len() <= MAX_PLAINTEXT);
    let answer = ClientMsg::Answer(AnswerMsg {
        request_id: "a".repeat(32),
        client_msg_id: "m".repeat(64),
        action: Action::Decline,
        shown_effect: None,
        choice: None,
        text: Some("é".repeat(MAX_USER_TEXT_CHARS)),
        intent: Some(Intent::Comment),
        answers: None,
    });
    assert!(answer.encode().unwrap().len() <= MAX_PLAINTEXT);
    // One more entry, one more character: refused.
    let ClientMsg::Sync { mut open, agents } = sync else { unreachable!() };
    open.push(OpenRef { request_id: "a".repeat(32), seq: 0 });
    assert!(ClientMsg::Sync { open, agents: agents.clone() }.encode().is_err());
    // One more agent: refused too.
    let mut agents = agents.unwrap();
    agents.push(AgentRef { agent_id: "f".repeat(32), rev: 1 });
    assert!(ClientMsg::Sync { open: vec![], agents: Some(agents) }.encode().is_err());
}

#[test]
fn encode_refuses_a_frame_above_one_noise_message() {
    // The limits are in characters and bytes of text. The JSON escaping can make a frame larger:
    // each control character of an explanation or a command costs 6 bytes. The sender checks.
    let mut r = max_question();
    r.command =
        Some(Command { text: "\u{1}".repeat(MAX_COMMAND_BYTES), label: None, language: None });
    r.explanation = Some("\u{1}".repeat(MAX_EXPLANATION_CHARS));
    let frame = ServerMsg::Request(Box::new(r));
    assert!(frame.validate().is_ok(), "the rules of text allow it");
    assert!(frame.encode().is_err(), "but it does not fit in one Noise message");
}

#[test]
fn text_limits_are_exact() {
    let base =
        files("server").into_iter().find(|f| f.0 == "request-question-choice.json").unwrap().2;
    let try_with = |f: &dyn Fn(&mut Value)| {
        let mut v = base.clone();
        f(&mut v);
        ServerMsg::decode(&serde_json::to_vec(&v).unwrap()).is_ok()
    };
    assert!(try_with(&|v| v["explanation"] = json!("x".repeat(8000))));
    assert!(!try_with(&|v| v["explanation"] = json!("x".repeat(8001))));
    assert!(!try_with(&|v| v["explanation"] = json!("")));
    assert!(try_with(&|v| v["command"]["text"] = json!("x".repeat(32768))));
    assert!(!try_with(&|v| v["command"]["text"] = json!("x".repeat(32769))));
    assert!(try_with(&|v| v["command"]["label"] = json!("x".repeat(32))));
    assert!(!try_with(&|v| v["command"]["label"] = json!("x".repeat(33))));
    assert!(try_with(&|v| v["input"]["options"][0]["description"] = json!("x".repeat(160))));
    assert!(!try_with(&|v| v["input"]["options"][0]["description"] = json!("x".repeat(161))));
    assert!(!try_with(&|v| v["input"]["options"][0]["label"] = json!("x".repeat(49))));
    assert!(!try_with(&|v| v["input"]["options"][0]["id"] = json!("A")));
    assert!(!try_with(&|v| v["from"]["label"] = json!("x".repeat(65))));
    assert!(!try_with(&|v| v["from"]["via"] = json!("web")));
    assert!(!try_with(&|v| v["urgency"] = json!("urgent")));
    assert!(!try_with(&|v| v["created_at_ms"] = json!(-1)));
}

#[test]
fn golden_approval_verifies_and_a_changed_op_does_not() {
    let frame = files("server").into_iter().find(|f| f.0 == "request-approval.json").unwrap();
    let ServerMsg::Request(req) = ServerMsg::decode(&frame.1).unwrap() else { panic!() };
    let body = req.approval.unwrap();
    assert!(body.verify());
    assert_eq!(body.computed_effect().unwrap(), body.effect);
    assert_eq!(effect::short(&body.effect).unwrap().len(), 8);
    let resolved = effect::resolve_view(body.op.as_value(), &body.view).unwrap();
    assert_eq!(resolved.body, "INSERT INTO analytics.events SELECT 1");
    assert_eq!(resolved.fields[0].value, "prod");
    // The server keeps the frame valid but the phone sees the mismatch.
    let mut changed = body.clone();
    changed.service = "other".into();
    assert!(!changed.verify());
    let mut changed = body.clone();
    changed.view.body = "/missing".into();
    assert!(!changed.verify());
    // The explanation is outside the hash.
    let mut changed = body;
    changed.explanation = Some("another text".into());
    assert!(changed.verify());
}

#[test]
fn old_hello_and_old_ack_do_not_parse() {
    // Protocol 4 sent `alert_id` in the receipt. Protocol 5 has `id` only. No alias.
    let old = br#"{"t":"ack","alert_id":"00000000000000000000000000000000","stage":"received"}"#;
    assert!(ClientMsg::decode(old).is_err());
    assert_eq!(PROTOCOL_VERSION_IS_5, 5);
}

const PROTOCOL_VERSION_IS_5: u32 = inbox_core::PROTOCOL_VERSION;

#[test]
fn new_invalid_frames_fail_for_the_named_reason() {
    // (folder, file, the rule that the error names)
    let table: &[(&str, &str, &str)] = &[
        ("server", "form-zero-questions", "input.questions"),
        ("server", "form-five-questions", "input.questions"),
        ("server", "form-duplicate-ids", "form.questions.id"),
        ("server", "form-one-option", "form.questions.options"),
        ("server", "form-nine-options", "form.questions.options"),
        ("server", "form-min-above-max", "form.questions.options"),
        ("server", "form-single-with-max-two", "form.questions.options"),
        ("server", "form-text-with-options", "form.questions.options"),
        ("server", "form-header-17-chars", "form.questions.header"),
        ("server", "form-header-newline", "form.questions.header"),
        ("server", "form-text-601-chars", "form.questions.text"),
        ("server", "form-question-id-uppercase", "form.questions.id"),
        ("server", "form-unknown-question-type", "frame"),
        ("server", "form-with-top-level-options", "input.options"),
        ("server", "form-without-questions", "input.questions"),
        ("server", "questions-on-a-choice-input", "input.questions"),
        ("server", "form-on-one-shot-key", "request.form"),
        ("server", "form-allow-custom-with-text-only", "request.allow_custom"),
        ("server", "explanation-format-html", "frame"),
        ("server", "explanation-format-uppercase", "frame"),
        ("server", "explanation-format-without-explanation", "request.explanation_format"),
        ("server", "command-language-unknown", "frame"),
        ("server", "command-language-uppercase", "frame"),
        (
            "server",
            "approval-explanation-format-without-explanation",
            "approval.explanation_format",
        ),
        ("server", "approval-language-unknown", "frame"),
        ("server", "approval-with-top-level-explanation-format", "request.approval"),
        ("server", "message-explanation-format-without-explanation", "message.explanation_format"),
        ("server", "message-user-with-explanation-format", "message.explanation"),
        ("client", "answer-form-with-choice", "answer.answers"),
        ("client", "answer-form-with-text", "answer.answers"),
        ("client", "answer-form-with-decline", "answer.answers"),
        ("client", "answer-form-with-intent", "answer.intent"),
        ("client", "answer-form-empty-list", "answer.answers"),
        ("client", "answer-form-five-entries", "answer.answers"),
        ("client", "answer-form-duplicate-question-id", "answer.answers.question_id"),
        ("client", "answer-form-entry-with-both", "answer.answers"),
        ("client", "answer-form-entry-with-neither", "answer.answers"),
        ("client", "answer-form-entry-empty-choice", "answer.answers.choice"),
        ("client", "answer-form-entry-nine-choices", "answer.answers.choice"),
        ("client", "answer-form-entry-repeated-choice", "answer.answers.choice"),
        ("client", "answer-form-entry-text-8001-chars", "answer.answers.text"),
        ("client", "answer-form-entry-bad-question-id", "answer.answers.question_id"),
        ("client", "answer-form-entry-unknown-member", "frame"),
    ];
    for (side, name, rule) in table {
        let path = dir(&format!("invalid/{side}/{name}.json"));
        let bytes = std::fs::read(&path).unwrap_or_else(|e| panic!("{name}: {e}"));
        let err = if *side == "server" {
            ServerMsg::decode(&bytes).err()
        } else {
            ClientMsg::decode(&bytes).err()
        }
        .unwrap_or_else(|| panic!("{name} was accepted"))
        .to_string();
        assert!(err.contains(rule), "{name}: the error is '{err}', expected '{rule}'");
    }
}

#[test]
fn markdown_and_language_members_are_kept_and_plain_is_a_valid_spelling() {
    // Every language of the closed list is a valid hint, in a command and in an approval.
    let q =
        files("server").into_iter().find(|f| f.0 == "request-question-markdown.json").unwrap().2;
    let a =
        files("server").into_iter().find(|f| f.0 == "request-approval-markdown.json").unwrap().2;
    for l in inbox_core::render::highlight::Language::ALL {
        let mut v = q.clone();
        v["command"]["language"] = json!(l.as_str());
        assert!(ServerMsg::decode(&serde_json::to_vec(&v).unwrap()).is_ok(), "{l:?}");
        let mut v = a.clone();
        v["approval"]["language"] = json!(l.as_str());
        assert!(ServerMsg::decode(&serde_json::to_vec(&v).unwrap()).is_ok(), "{l:?}");
    }
    // `plain` is the default and a valid value. A frame that holds it comes back as it came.
    let mut v = q.clone();
    v["explanation_format"] = json!("plain");
    let msg = ServerMsg::decode(&serde_json::to_vec(&v).unwrap()).unwrap();
    assert_eq!(serde_json::to_value(&msg).unwrap(), v);
    let ServerMsg::Request(r) = msg else { panic!() };
    assert_eq!(r.explanation_format, Some(ExplanationFormat::Plain));
    // The markdown fixture holds a markdown member in each place.
    let ServerMsg::Request(r) = ServerMsg::decode(&serde_json::to_vec(&q).unwrap()).unwrap() else {
        panic!()
    };
    assert_eq!(r.explanation_format, Some(ExplanationFormat::Markdown));
    assert_eq!(r.command.unwrap().language, Some(inbox_core::render::highlight::Language::Sql));
}

fn form_question(id: &str, kind: FormQuestionType) -> FormQuestion {
    let choice = kind == FormQuestionType::Choice;
    FormQuestion {
        id: id.repeat(32).chars().take(32).collect(),
        header: "h".repeat(MAX_FORM_HEADER_CHARS),
        text: "q".repeat(MAX_FORM_TEXT_CHARS),
        kind,
        multiple: choice,
        min: u8::from(choice),
        max: if choice { 8 } else { 0 },
        options: if choice {
            (0..MAX_OPTIONS)
                .map(|i| InputOption {
                    id: format!("{i}").repeat(32).chars().take(32).collect(),
                    label: "L".repeat(MAX_OPTION_LABEL_CHARS),
                    description: Some("D".repeat(MAX_OPTION_DESCRIPTION_CHARS)),
                })
                .collect()
        } else {
            vec![]
        },
        placeholder: (!choice).then(|| "p".repeat(MAX_PLACEHOLDER_CHARS)),
    }
}

fn max_form() -> RequestMsg {
    let mut r = max_question();
    r.command = Some(Command {
        text: "c".repeat(MAX_COMMAND_BYTES),
        label: Some("l".repeat(32)),
        language: Some(Language::Sql),
    });
    r.input = Some(Input {
        kind: InputType::Form,
        multiple: false,
        min: 0,
        max: 0,
        options: vec![],
        placeholder: None,
        questions: Some(vec![
            form_question("a", FormQuestionType::Choice),
            form_question("b", FormQuestionType::Choice),
            form_question("c", FormQuestionType::Choice),
            form_question("d", FormQuestionType::Choice),
        ]),
    });
    r
}

#[test]
fn the_largest_form_fits_one_noise_message() {
    // 4 questions, each with 8 options at the maximum length, next to the largest explanation and the largest
    // command. The form is the part that grows: report its own size too.
    let frame = ServerMsg::Request(Box::new(max_form()));
    let bytes = frame.encode().unwrap_or_else(|e| panic!("{e}"));
    assert!(bytes.len() <= MAX_PLAINTEXT, "{} bytes", bytes.len());
    let form_only = {
        let mut r = max_form();
        r.command = None;
        r.explanation = None;
        r.explanation_format = None;
        serde_json::to_vec(&r).unwrap().len()
    };
    assert!(form_only < 16 * 1024, "the form alone is {form_only} bytes");
    // A real Noise session carries it.
    let device = noise::generate_keypair().unwrap();
    let server = noise::generate_keypair().unwrap();
    let (init, m1) = Initiator::start(&device.private, &server.public, b"{}").unwrap();
    let (resp, _, _) = Responder::start(&server.private, &m1).unwrap();
    let (mut srv, m2) = resp.finish(b"{}").unwrap();
    let (mut phone, _) = init.finish(&m2).unwrap();
    let sealed = srv.encrypt(&bytes).unwrap();
    assert!(sealed.len() <= noise::MAX_MESSAGE);
    assert_eq!(ServerMsg::decode(&phone.decrypt(&sealed).unwrap()).unwrap(), frame);
    // The answer to the largest form: four answers with the longest text.
    let answer = ClientMsg::Answer(AnswerMsg {
        request_id: "a".repeat(32),
        client_msg_id: "m".repeat(64),
        action: Action::Accept,
        shown_effect: None,
        choice: None,
        text: None,
        intent: None,
        answers: Some(
            ["a", "b", "c", "d"]
                .iter()
                .map(|id| FormAnswer {
                    question_id: id.repeat(32).chars().take(32).collect(),
                    choice: None,
                    text: Some("é".repeat(MAX_USER_TEXT_CHARS)),
                })
                .collect(),
        ),
    });
    // Four texts of 8000 characters of two bytes each fit: 64000 bytes and the frame around them.
    assert!(answer.encode().unwrap().len() <= MAX_PLAINTEXT);
    // Four texts of 8000 characters of three bytes each do not: the sender refuses the frame. A phone limits the
    // total of what the user types, as for a `reply`.
    let ClientMsg::Answer(mut a) = answer else { unreachable!() };
    for e in a.answers.as_mut().unwrap() {
        e.text = Some("\u{20ac}".repeat(MAX_USER_TEXT_CHARS));
    }
    assert!(ClientMsg::Answer(a.clone()).validate().is_ok(), "the rules of the text allow it");
    assert!(ClientMsg::Answer(a).encode().is_err(), "but it does not fit in one Noise message");
}

#[test]
fn the_protocol_document_names_what_the_code_names() {
    use inbox_core::render::highlight::{Class, Language};
    let doc = std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/inbox/protocol.md"),
    )
    .expect("protocol.md");
    // The list of the languages, in the order of the code.
    let list = Language::ALL.iter().map(|l| format!("`{}`", l.as_str())).collect::<Vec<_>>();
    let line =
        doc.lines().find(|l| l.contains("The value is one of")).expect("the list of the languages");
    for name in &list {
        assert!(line.contains(name.as_str()), "{name} is not in: {line}");
    }
    assert_eq!(
        line.matches('`').count(),
        list.len() * 2,
        "the list of the document has another name: {line}"
    );
    // The classes of a token.
    for class in [
        Class::Keyword,
        Class::String,
        Class::Number,
        Class::Comment,
        Class::Function,
        Class::Type,
        Class::Operator,
        Class::Punctuation,
        Class::Plain,
        Class::Add,
        Class::Del,
        Class::Hunk,
        Class::Meta,
    ] {
        assert!(doc.contains(&format!("`{}`", class.as_str())), "{class:?}");
    }
    // The limits of a form.
    for (text, n) in [
        ("1 to 4 items", MAX_FORM_QUESTIONS),
        ("line of 1 to 16 characters", MAX_FORM_HEADER_CHARS),
        ("1 to 600 characters", MAX_FORM_TEXT_CHARS),
    ] {
        assert!(doc.contains(text), "{text}");
        assert!(text.contains(&n.to_string()), "{text} against {n}");
    }
    assert!(
        doc.contains("input of 8000 characters") || doc.contains("an input of 8000 characters")
    );
    assert_eq!(inbox_core::render::markdown::MAX_INPUT_CHARS, MAX_EXPLANATION_CHARS);
    assert!(doc.contains("2000 nodes") && doc.contains("depth of 4"));
    assert_eq!(inbox_core::render::markdown::MAX_NODES, 2000);
    assert_eq!(inbox_core::render::markdown::MAX_DEPTH, 4);
    assert_eq!(inbox_core::render::markdown::MAX_LIST_DEPTH, 3);
}

// ---------------------------------------------------------------- agents (lot G2)

fn golden(sub: &str, name: &str) -> Value {
    files(sub).into_iter().find(|f| f.0 == name).unwrap_or_else(|| panic!("{sub}/{name}")).2
}

fn server_ok(v: &Value) -> bool {
    ServerMsg::decode(&serde_json::to_vec(v).unwrap()).is_ok()
}

fn client_ok(v: &Value) -> bool {
    ClientMsg::decode(&serde_json::to_vec(v).unwrap()).is_ok()
}

#[test]
fn request_from_has_agent_members_and_refuses_unknown() {
    let ServerMsg::Request(r) =
        ServerMsg::decode(&std::fs::read(dir("server/request-approval.json")).unwrap()).unwrap()
    else {
        panic!()
    };
    assert_eq!(r.from.label, "Billing bot");
    assert_eq!(r.from.agent_id, "a1".repeat(16));
    assert_eq!((r.from.agent_kind, r.from.agent_state), (AgentKind::Agent, AgentState::Known));
    assert_eq!(r.agent_label.as_deref(), Some("billing-bot"));
    // The text of the agent is outside `from`: a question frame has it in the same place.
    let ServerMsg::Request(q) =
        ServerMsg::decode(&std::fs::read(dir("server/request-question-form.json")).unwrap())
            .unwrap()
    else {
        panic!()
    };
    assert_eq!((q.from.agent_kind, q.from.agent_state), (AgentKind::Agent, AgentState::New));
    assert_eq!(q.from.label, "claude-laptop #4");
    assert_eq!(q.agent_label.as_deref(), Some("billing-bot"));
    // The bucket, in key mode: the label is the key name, and the state is `known`.
    let ServerMsg::Request(b) = ServerMsg::decode(
        &std::fs::read(dir("server/request-question-confirm-one-shot.json")).unwrap(),
    )
    .unwrap() else {
        panic!()
    };
    assert_eq!((b.from.agent_kind, b.from.agent_state), (AgentKind::Bucket, AgentState::Known));
    assert_eq!(b.agent_label, None);
    // Each new member is mandatory in `from`, and `from` stays a closed object.
    let base = golden("server", "request-approval.json");
    for key in ["agent_id", "agent_kind", "agent_state"] {
        let mut v = base.clone();
        v["from"].as_object_mut().unwrap().remove(key);
        assert!(!server_ok(&v), "from without {key}");
    }
    // `agent_label` is optional, and a null is an absent member.
    let mut v = base.clone();
    v.as_object_mut().unwrap().remove("agent_label");
    assert!(server_ok(&v));
    v["agent_label"] = Value::Null;
    assert!(server_ok(&v));
    // The states that an open request cannot have.
    for state in ["blocked", "merged"] {
        let mut v = base.clone();
        v["from"]["agent_state"] = json!(state);
        assert!(!server_ok(&v), "{state}");
    }
    // A bucket is `known`. An agent can be `new` or `known`.
    let mut v = base.clone();
    v["from"]["agent_kind"] = json!("bucket");
    v["from"]["agent_state"] = json!("new");
    assert!(!server_ok(&v));
    v["from"]["agent_state"] = json!("known");
    assert!(server_ok(&v));
}

#[test]
fn agent_label_limits_are_exact_and_it_refuses_hidden_characters() {
    let base = golden("server", "request-approval.json");
    let with = |text: &str| {
        let mut v = base.clone();
        v["agent_label"] = json!(text);
        server_ok(&v)
    };
    assert!(with("x"));
    assert!(with(&"x".repeat(64)));
    assert!(with(&"\u{e9}".repeat(64)), "a character counts once, whatever its size in bytes");
    assert!(!with(""));
    assert!(!with(&"x".repeat(65)));
    assert!(!with(&"\u{e9}".repeat(65)));
    for bad in [
        "a\nb",
        "a\rb",
        "a\tb",
        "a\u{7}b",
        "a\u{0}b",
        "a\u{85}b",
        "a\u{202e}b",
        "a\u{2066}b",
        "a\u{200b}b",
        "a\u{200d}b",
        "a\u{feff}b",
        "a\u{ad}b",
        "a\u{2028}b",
        "a\u{2029}b",
        "a\u{e0041}b",
    ] {
        assert!(!with(bad), "{bad:?}");
    }
    // A space inside is a normal character. A text that equals the name of another agent is a matter for the screen.
    assert!(with("billing bot"));
    assert!(with("billing-bot "));
}

#[test]
fn agent_frame_roundtrip_values_and_rules() {
    let ServerMsg::Agent(a) =
        ServerMsg::decode(&std::fs::read(dir("server/agent-new.json")).unwrap()).unwrap()
    else {
        panic!()
    };
    assert_eq!((a.kind, a.state, a.rev), (AgentKind::Agent, AgentState::New, 3));
    assert!(a.lock_accept && !a.named);
    assert_eq!(a.label, "claude-laptop #4");
    let o = a.observed.as_ref().unwrap();
    assert_eq!(o.says.as_deref(), Some("billing-bot"));
    assert_eq!((o.says_changed, o.nets_seen), (Some(1), 2));
    assert_eq!(a.similar.as_ref().unwrap().label, "Billing bot");
    // A merged agent names its target. A blocked agent and a known agent have no other member.
    let ServerMsg::Agent(m) =
        ServerMsg::decode(&std::fs::read(dir("server/agent-merged.json")).unwrap()).unwrap()
    else {
        panic!()
    };
    assert_eq!(
        (m.state, m.merged_into.as_deref()),
        (AgentState::Merged, Some("a1".repeat(16).as_str()))
    );
    let ServerMsg::Agent(b) =
        ServerMsg::decode(&std::fs::read(dir("server/agent-bucket.json")).unwrap()).unwrap()
    else {
        panic!()
    };
    assert_eq!(
        (b.kind, b.state, b.observed.is_none()),
        (AgentKind::Bucket, AgentState::Known, true)
    );
    // The members `observed`, `similar` and `merged_into` are optional. The others are mandatory.
    let base = golden("server", "agent-new.json");
    for key in [
        "id",
        "agent_id",
        "rev",
        "key_name",
        "kind",
        "state",
        "label",
        "named",
        "created_at_ms",
        "last_seen_ms",
        "open",
        "lock_accept",
    ] {
        let mut v = base.clone();
        v.as_object_mut().unwrap().remove(key);
        assert!(!server_ok(&v), "agent without {key}");
    }
    for key in ["observed", "similar"] {
        let mut v = base.clone();
        v.as_object_mut().unwrap().remove(key);
        assert!(server_ok(&v), "agent without {key}");
    }
    // `nets_seen` is the only mandatory member of `observed`.
    let mut v = base.clone();
    v["observed"] = json!({"nets_seen": 0});
    assert!(server_ok(&v));
    v["observed"] = json!({});
    assert!(!server_ok(&v));
    // `lock_accept` only with `new`. `similar` only with `new`.
    let known = golden("server", "agent-known.json");
    let mut v = known.clone();
    v["lock_accept"] = json!(true);
    assert!(!server_ok(&v));
    let mut v = known.clone();
    v["similar"] = base["similar"].clone();
    assert!(!server_ok(&v));
    let mut v = base.clone();
    v["lock_accept"] = json!(false);
    assert!(server_ok(&v), "a new agent with the lock off: the server lets the owner approve");
    // Every state parses, and nothing else does.
    for state in ["new", "known", "blocked"] {
        let mut v = known.clone();
        v["state"] = json!(state);
        assert!(server_ok(&v), "{state}");
    }
    let mut v = known.clone();
    v["state"] = json!("merged");
    assert!(!server_ok(&v), "merged needs merged_into");
    v["merged_into"] = json!("c".repeat(32));
    assert!(server_ok(&v));
}

#[test]
fn agent_text_and_number_limits_are_exact() {
    let base = golden("server", "agent-new.json");
    let ok = |f: &dyn Fn(&mut Value)| {
        let mut v = base.clone();
        f(&mut v);
        server_ok(&v)
    };
    // Characters of two bytes: the limits count characters.
    let e = |n: usize| json!("\u{e9}".repeat(n));
    for (name, max) in [("says", 64), ("client", 64), ("user_agent", 128), ("net", 43)] {
        assert!(ok(&|v| v["observed"][name] = e(max)), "{name} at the limit");
        assert!(!ok(&|v| v["observed"][name] = e(max + 1)), "{name} above the limit");
        assert!(!ok(&|v| v["observed"][name] = json!("")), "{name} empty");
        assert!(!ok(&|v| v["observed"][name] = json!("a\u{202e}b")), "{name} with a bidi control");
        assert!(
            !ok(&|v| v["observed"][name] = json!("a\u{200b}b")),
            "{name} with a zero-width character"
        );
        assert!(!ok(&|v| v["observed"][name] = json!("a\nb")), "{name} with a line end");
        assert!(!ok(&|v| v["observed"][name] = json!(7)), "{name} as a number");
    }
    for name in ["label", "key_name"] {
        assert!(ok(&|v| v[name] = e(64)), "{name}");
        assert!(!ok(&|v| v[name] = e(65)), "{name}");
        assert!(!ok(&|v| v[name] = json!("")), "{name}");
    }
    assert!(ok(&|v| v["similar"]["label"] = e(64)));
    assert!(!ok(&|v| v["similar"]["label"] = e(65)));
    // `nets_seen` is 0 to 8. `says_changed` is 1 or more and needs `says`.
    for n in 0..=8 {
        assert!(ok(&|v| v["observed"]["nets_seen"] = json!(n)), "{n}");
    }
    for bad in [json!(9), json!(-1), json!(256), json!(1.5), json!("2")] {
        assert!(!ok(&|v| v["observed"]["nets_seen"] = bad.clone()), "{bad}");
    }
    assert!(ok(&|v| v["observed"]["says_changed"] = json!(u32::MAX)));
    assert!(!ok(&|v| v["observed"]["says_changed"] = json!(0)));
    assert!(!ok(&|v| v["observed"]["says_changed"] = json!(u64::from(u32::MAX) + 1)));
    assert!(!ok(&|v| {
        v["observed"].as_object_mut().unwrap().remove("says");
    }));
    // `rev` is 1 to the largest signed 64-bit integer: a device that reads a signed integer reads it.
    assert!(ok(&|v| v["rev"] = json!(1)));
    assert!(ok(&|v| v["rev"] = json!(i64::MAX)));
    assert!(!ok(&|v| v["rev"] = json!(0)));
    assert!(!ok(&|v| v["rev"] = json!(i64::MAX as u64 + 1)));
    assert!(!ok(&|v| v["rev"] = json!(-1)));
    assert!(!ok(&|v| v["rev"] = json!(1.5)));
    // `open` is a u32. The times are 0 or more.
    assert!(ok(&|v| v["open"] = json!(0)));
    assert!(ok(&|v| v["open"] = json!(u32::MAX)));
    assert!(!ok(&|v| v["open"] = json!(u64::from(u32::MAX) + 1)));
    assert!(!ok(&|v| v["open"] = json!(-1)));
    assert!(ok(&|v| v["last_seen_ms"] = json!(0)));
    assert!(!ok(&|v| v["last_seen_ms"] = json!(-1)));
    assert!(!ok(&|v| v["created_at_ms"] = json!(-1)));
    // A bucket has no hints, no name, no lock and is `known`.
    let bucket = golden("server", "agent-bucket.json");
    let bad_bucket = |f: &dyn Fn(&mut Value)| {
        let mut v = bucket.clone();
        f(&mut v);
        !server_ok(&v)
    };
    assert!(server_ok(&bucket));
    assert!(bad_bucket(&|v| v["state"] = json!("new")));
    assert!(bad_bucket(&|v| v["state"] = json!("blocked")));
    assert!(bad_bucket(&|v| v["named"] = json!(true)));
    assert!(bad_bucket(&|v| v["observed"] = json!({"nets_seen": 0})));
    assert!(bad_bucket(&|v| v["merged_into"] = json!("c".repeat(32))));
}

/// Lot G6: the web is the only place where an owner makes an agent or resets a token. The frame has no such operation.
#[test]
fn agent_do_has_no_operation_to_enroll_or_to_reset_a_token() {
    let a = "d4".repeat(16);
    for op in ["enroll", "reset_token", "reset", "add", "make"] {
        for extra in [json!({}), json!({"name": "Refund cleaner"})] {
            let mut v = json!({"t": "agent_do", "agent_id": a, "client_msg_id": "m-0010", "op": op});
            for (k, x) in extra.as_object().unwrap() {
                v[k] = x.clone();
            }
            assert!(!client_ok(&v), "{op} {extra}");
        }
    }
}

#[test]
fn agent_do_ops_and_limits() {
    let a = "d4".repeat(16);
    let to = "a1".repeat(16);
    let frame = |op: &str, extra: Value| {
        let mut v = json!({"t": "agent_do", "agent_id": a, "client_msg_id": "m-0010", "op": op});
        for (k, x) in extra.as_object().unwrap() {
            v[k] = x.clone();
        }
        v
    };
    // The five ops, with the members that each one takes.
    assert!(client_ok(&frame("rename", json!({"name": "Refund cleaner"}))));
    assert!(client_ok(&frame("allow", json!({}))));
    assert!(client_ok(&frame("allow", json!({"name": "Refund cleaner"}))));
    assert!(client_ok(&frame("block", json!({}))));
    assert!(client_ok(&frame("unblock", json!({}))));
    assert!(client_ok(&frame("merge", json!({"into": to}))));
    // A merge into the agent itself is well formed: the server answers `bad_state`, and the session stays open.
    assert!(client_ok(&frame("merge", json!({"into": a}))));
    // A member that the op does not take, and a missing member that it needs.
    for (op, extra) in [
        ("rename", json!({})),
        ("rename", json!({"name": "x", "into": to})),
        ("allow", json!({"into": to})),
        ("allow", json!({"name": "x", "into": to})),
        ("block", json!({"name": "x"})),
        ("block", json!({"into": to})),
        ("unblock", json!({"name": "x"})),
        ("unblock", json!({"into": to})),
        ("merge", json!({})),
        ("merge", json!({"name": "x", "into": to})),
        ("merge", json!({"into": "A1".repeat(16)})),
        ("merge", json!({"into": "a1"})),
        ("delete", json!({})),
        ("Rename", json!({"name": "x"})),
        ("", json!({})),
    ] {
        assert!(!client_ok(&frame(op, extra.clone())), "{op} {extra}");
    }
    // The name: 1 to 32 characters. The other rules of a name are the rules of the server (`invalid_name`).
    for op in ["rename", "allow"] {
        assert!(client_ok(&frame(op, json!({"name": "n"}))));
        assert!(client_ok(&frame(op, json!({"name": "n".repeat(32)}))));
        assert!(
            client_ok(&frame(op, json!({"name": "\u{e9}".repeat(32)}))),
            "{op}: characters, not bytes"
        );
        assert!(!client_ok(&frame(op, json!({"name": "n".repeat(33)}))));
        assert!(!client_ok(&frame(op, json!({"name": "\u{e9}".repeat(33)}))));
        assert!(!client_ok(&frame(op, json!({"name": ""}))));
        assert!(!client_ok(&frame(op, json!({"name": "a\u{0}b"}))), "NUL");
        assert!(!client_ok(&frame(op, json!({"name": 7}))));
        // A name with a control or a format character is a matter for the server: it answers `invalid_name`.
        assert!(client_ok(&frame(op, json!({"name": "a\u{202e}b"}))));
        assert!(client_ok(&frame(op, json!({"name": " padded "}))));
    }
    // A null member is an absent member.
    assert!(client_ok(&frame("allow", json!({"name": null, "into": null}))));
    // `client_msg_id` follows the rule of `answer`: 1 to 64 characters of `A-Z a-z 0-9 _ -`.
    for (id, ok) in [
        ("m", true),
        ("M-1_z", true),
        ("m".repeat(64).as_str(), true),
        ("", false),
        ("m".repeat(65).as_str(), false),
        ("a b", false),
        ("a/b", false),
    ] {
        let mut v = frame("block", json!({}));
        v["client_msg_id"] = json!(id);
        assert_eq!(client_ok(&v), ok, "{id:?}");
    }
    for key in ["agent_id", "client_msg_id", "op"] {
        let mut v = frame("block", json!({}));
        v.as_object_mut().unwrap().remove(key);
        assert!(!client_ok(&v), "without {key}");
    }
    let mut v = frame("block", json!({}));
    v["force"] = json!(true);
    assert!(!client_ok(&v));
    // The typed value round trips.
    let msg = ClientMsg::decode(&serde_json::to_vec(&frame("merge", json!({"into": to}))).unwrap())
        .unwrap();
    assert_eq!(
        msg,
        ClientMsg::AgentDo(AgentDoMsg {
            agent_id: a,
            client_msg_id: "m-0010".into(),
            op: AgentOpKind::Merge,
            name: None,
            into: Some(to)
        })
    );
}

#[test]
fn agent_result_status_closed() {
    let base = golden("server", "agent-result.json");
    let all = [
        AgentResultStatus::Ok,
        AgentResultStatus::NotFound,
        AgentResultStatus::InvalidName,
        AgentResultStatus::NameTaken,
        AgentResultStatus::BadState,
        AgentResultStatus::CrossKey,
        AgentResultStatus::NotEnabled,
    ];
    let texts =
        ["ok", "not_found", "invalid_name", "name_taken", "bad_state", "cross_key", "not_enabled"];
    for (s, text) in all.iter().zip(texts) {
        assert_eq!(s.as_str(), text);
        let mut v = base.clone();
        v["status"] = json!(text);
        let ServerMsg::AgentResult(r) =
            ServerMsg::decode(&serde_json::to_vec(&v).unwrap()).unwrap()
        else {
            panic!()
        };
        assert_eq!(r.status, *s);
    }
    for bad in ["", "OK", "Ok", "done", "agent_locked", "accepted", "not-found", "unknown"] {
        let mut v = base.clone();
        v["status"] = json!(bad);
        assert!(!server_ok(&v), "{bad:?}");
    }
    // The frame has no delivery id and no state: it is not in the queue and it is not acknowledged.
    let mut v = base.clone();
    v["state"] = json!("known");
    assert!(!server_ok(&v));
    let mut v = base;
    v["id"] = json!("a".repeat(32));
    assert!(!server_ok(&v));
}

#[test]
fn agent_locked_is_a_status_of_an_answer() {
    let ServerMsg::AnswerResult(r) =
        ServerMsg::decode(&std::fs::read(dir("server/answer-result-agent-locked.json")).unwrap())
            .unwrap()
    else {
        panic!()
    };
    assert_eq!((r.status, r.state), (AnswerStatus::AgentLocked, Some(ItemState::Pending)));
    assert_eq!(serde_json::to_value(r.status).unwrap(), json!("agent_locked"));
}

#[test]
fn sync_with_agents() {
    let item = "bb".repeat(16);
    let a1 = "a1".repeat(16);
    let a2 = "d4".repeat(16);
    let wire = |agents: Value| {
        let mut v = json!({"t": "sync", "open": [{"request_id": item, "seq": 2}]});
        if !agents.is_null() {
            v["agents"] = agents;
        }
        v
    };
    // Absent, null and empty: the device holds no agent.
    for ok in [wire(Value::Null), wire(json!([]))] {
        assert!(client_ok(&ok), "{ok}");
    }
    let mut v = wire(Value::Null);
    v["agents"] = Value::Null;
    assert!(client_ok(&v));
    // An absent list and an empty list are two spellings of one frame: each one reads back as it came.
    let ClientMsg::Sync { agents, .. } =
        ClientMsg::decode(&serde_json::to_vec(&wire(Value::Null)).unwrap()).unwrap()
    else {
        panic!()
    };
    assert_eq!(agents, None);
    let ClientMsg::Sync { agents, .. } =
        ClientMsg::decode(&serde_json::to_vec(&wire(json!([]))).unwrap()).unwrap()
    else {
        panic!()
    };
    assert_eq!(agents, Some(vec![]));
    // Entries: an id, and a `rev` of 1 or more. No duplicate.
    assert!(client_ok(&wire(
        json!([{"agent_id": a1, "rev": 1}, {"agent_id": a2, "rev": i64::MAX}])
    )));
    for bad in [
        json!([{"agent_id": a1, "rev": 0}]),
        json!([{"agent_id": a1, "rev": -1}]),
        json!([{"agent_id": a1, "rev": i64::MAX as u64 + 1}]),
        json!([{"agent_id": a1}]),
        json!([{"rev": 1}]),
        json!([{"agent_id": "A1".repeat(16), "rev": 1}]),
        json!([{"agent_id": a1, "rev": 1, "zz": 1}]),
        json!([{"agent_id": a1, "rev": 1}, {"agent_id": a1, "rev": 2}]),
        json!({}),
        json!("a1"),
    ] {
        assert!(!client_ok(&wire(bad.clone())), "{bad}");
    }
    // At most 200 entries.
    let list = |n: usize| -> Value {
        (1..=n).map(|i| json!({"agent_id": format!("{i:032x}"), "rev": 1})).collect()
    };
    assert!(client_ok(&wire(list(200))));
    assert!(!client_ok(&wire(list(201))));
    // The entries of `open` keep their rules.
    let mut v = wire(json!([]));
    v["open"][0]["zz"] = json!(1);
    assert!(!client_ok(&v));
}

#[test]
fn agent_invalid_frames_fail_for_the_named_reason() {
    // (folder, file, the rule that the error names)
    let table: &[(&str, &str, &str)] = &[
        ("server", "request-from-without-agent-id", "frame"),
        ("server", "request-from-without-agent-kind", "frame"),
        ("server", "request-from-without-agent-state", "frame"),
        ("server", "request-from-agent-id-uppercase", "from.agent_id"),
        ("server", "request-from-agent-id-short", "from.agent_id"),
        ("server", "request-from-agent-kind-unknown", "frame"),
        ("server", "request-from-agent-state-blocked", "from.agent_state"),
        ("server", "request-from-agent-state-merged", "from.agent_state"),
        ("server", "request-from-bucket-state-new", "from.agent_state"),
        ("server", "request-question-from-without-agent-id", "frame"),
        ("server", "request-agent-label-empty", "request.agent_label"),
        ("server", "request-agent-label-65-chars", "request.agent_label"),
        ("server", "request-agent-label-newline", "request.agent_label"),
        ("server", "request-agent-label-bidi", "request.agent_label"),
        ("server", "request-agent-label-zero-width", "request.agent_label"),
        ("server", "request-agent-label-line-separator", "request.agent_label"),
        ("server", "request-agent-label-not-a-string", "frame"),
        ("server", "agent-without-id", "frame"),
        ("server", "agent-without-rev", "frame"),
        ("server", "agent-rev-zero", "agent.rev"),
        ("server", "agent-rev-above-i64", "agent.rev"),
        ("server", "agent-rev-negative", "frame"),
        ("server", "agent-agent-id-not-hex", "agent.agent_id"),
        ("server", "agent-state-unknown", "frame"),
        ("server", "agent-state-uppercase", "frame"),
        ("server", "agent-kind-unknown", "frame"),
        ("server", "agent-label-empty", "agent.label"),
        ("server", "agent-label-65-chars", "agent.label"),
        ("server", "agent-label-newline", "agent.label"),
        ("server", "agent-key-name-empty", "agent.key_name"),
        ("server", "agent-key-name-65-chars", "agent.key_name"),
        ("server", "agent-created-negative", "agent.created_at_ms"),
        ("server", "agent-last-seen-negative", "agent.last_seen_ms"),
        ("server", "agent-open-negative", "frame"),
        ("server", "agent-open-above-u32", "frame"),
        ("server", "agent-named-as-text", "frame"),
        ("server", "agent-merged-without-target", "agent.merged_into"),
        ("server", "agent-merged-into-itself", "agent.merged_into"),
        ("server", "agent-merged-into-not-hex", "agent.merged_into"),
        ("server", "agent-target-without-merged-state", "agent.merged_into"),
        ("server", "agent-lock-accept-on-known", "agent.lock_accept"),
        ("server", "agent-bucket-new", "agent.kind"),
        ("server", "agent-bucket-named", "agent.kind"),
        ("server", "agent-bucket-with-observed", "agent.kind"),
        ("server", "agent-bucket-with-similar", "agent.similar"),
        ("server", "agent-observed-says-empty", "agent.observed.says"),
        ("server", "agent-observed-says-65-chars", "agent.observed.says"),
        ("server", "agent-observed-says-bidi", "agent.observed.says"),
        ("server", "agent-observed-says-newline", "agent.observed.says"),
        ("server", "agent-observed-client-65-chars", "agent.observed.client"),
        ("server", "agent-observed-client-control", "agent.observed.client"),
        ("server", "agent-observed-user-agent-129-chars", "agent.observed.user_agent"),
        ("server", "agent-observed-user-agent-zero-width", "agent.observed.user_agent"),
        ("server", "agent-observed-net-44-chars", "agent.observed.net"),
        ("server", "agent-observed-nets-seen-9", "agent.observed.nets_seen"),
        ("server", "agent-observed-nets-seen-negative", "frame"),
        ("server", "agent-observed-without-nets-seen", "frame"),
        ("server", "agent-observed-says-changed-zero", "agent.observed.says_changed"),
        ("server", "agent-observed-says-changed-without-says", "agent.observed.says_changed"),
        ("server", "agent-observed-says-changed-as-text", "frame"),
        ("server", "agent-similar-on-known", "agent.similar"),
        ("server", "agent-similar-itself", "agent.similar"),
        ("server", "agent-similar-label-empty", "agent.similar"),
        ("server", "agent-similar-without-label", "frame"),
        ("server", "agent-result-status-unknown", "frame"),
        ("server", "agent-result-status-uppercase", "frame"),
        ("server", "agent-result-without-agent-id", "frame"),
        ("server", "agent-result-agent-id-short", "agent_result.agent_id"),
        ("server", "agent-result-client-msg-id-empty", "client_msg_id"),
        ("server", "agent-result-with-id", "frame"),
        ("server", "answer-result-agent-locked-uppercase", "frame"),
        ("client", "agent-do-without-agent-id", "frame"),
        ("client", "agent-do-agent-id-short", "agent_do.agent_id"),
        ("client", "agent-do-client-msg-id-empty", "client_msg_id"),
        ("client", "agent-do-client-msg-id-65-chars", "client_msg_id"),
        ("client", "agent-do-without-op", "frame"),
        ("client", "agent-do-op-unknown", "frame"),
        ("client", "agent-do-op-uppercase", "frame"),
        ("client", "agent-do-rename-without-name", "agent_do.op"),
        ("client", "agent-do-rename-with-into", "agent_do.op"),
        ("client", "agent-do-name-empty", "agent_do.name"),
        ("client", "agent-do-name-33-chars", "agent_do.name"),
        ("client", "agent-do-name-with-nul", "agent_do.name"),
        ("client", "agent-do-name-as-number", "frame"),
        ("client", "agent-do-allow-with-into", "agent_do.op"),
        ("client", "agent-do-allow-name-33-chars", "agent_do.name"),
        ("client", "agent-do-block-with-name", "agent_do.op"),
        ("client", "agent-do-block-with-into", "agent_do.op"),
        ("client", "agent-do-unblock-with-name", "agent_do.op"),
        ("client", "agent-do-merge-without-into", "agent_do.op"),
        ("client", "agent-do-merge-with-name", "agent_do.op"),
        ("client", "agent-do-merge-into-not-hex", "agent_do.into"),
        ("client", "agent-do-unknown-member", "frame"),
        ("client", "sync-agents-bad-id", "sync.agents"),
        ("client", "sync-agents-rev-zero", "sync.agents"),
        ("client", "sync-agents-rev-negative", "frame"),
        ("client", "sync-agents-without-rev", "frame"),
        ("client", "sync-agents-duplicate-agent", "sync.agents"),
        ("client", "sync-agents-extra-member", "frame"),
        ("client", "sync-agents-not-a-list", "frame"),
        ("client", "sync-agents-201-entries", "sync.agents"),
    ];
    for (side, name, rule) in table {
        let path = dir(&format!("invalid/{side}/{name}.json"));
        let bytes = std::fs::read(&path).unwrap_or_else(|e| panic!("{name}: {e}"));
        let err = if *side == "server" {
            ServerMsg::decode(&bytes).err()
        } else {
            ClientMsg::decode(&bytes).err()
        }
        .unwrap_or_else(|| panic!("{name} was accepted"))
        .to_string();
        assert!(err.contains(rule), "{name}: the error is '{err}', expected '{rule}'");
    }
    // Each file of the agents is in the table: a new file without a named reason does not pass unseen.
    let listed: BTreeSet<String> = table.iter().map(|(s, n, _)| format!("{s}/{n}")).collect();
    for side in ["server", "client"] {
        for (name, _, _) in files(&format!("invalid/{side}")) {
            let stem = name.trim_end_matches(".json");
            let is_agent = stem.starts_with("agent")
                || stem.starts_with("request-from-")
                || stem.starts_with("request-agent-")
                || stem.starts_with("sync-agents")
                || stem == "request-question-from-without-agent-id"
                || stem == "answer-result-agent-locked-uppercase";
            if is_agent {
                assert!(
                    listed.contains(&format!("{side}/{stem}")),
                    "{side}/{stem} has no named reason"
                );
            }
        }
    }
}

#[test]
fn every_agent_frame_rejects_a_duplicate_member() {
    for name in ["agent-new.json", "agent-result.json"] {
        let text = std::fs::read_to_string(dir(&format!("server/{name}"))).unwrap();
        // Repeat the line of `agent_id`.
        let line = text.lines().find(|l| l.contains("\"agent_id\"")).unwrap().to_string();
        let dup = text.replacen(&line, &format!("{line}\n{line}"), 1);
        assert_ne!(dup, text);
        assert!(ServerMsg::decode(dup.as_bytes()).is_err(), "{name}");
    }
    let text = std::fs::read_to_string(dir("client/agent-do-rename.json")).unwrap();
    let dup = text.replacen("\"op\": \"rename\"", "\"op\": \"rename\", \"op\": \"rename\"", 1);
    assert_ne!(dup, text);
    assert!(ClientMsg::decode(dup.as_bytes()).is_err());
}

#[test]
fn the_format_characters_are_the_class_cf_of_unicode() {
    // The list of the core is the list that the server uses to refuse a name: one function, one answer.
    for c in [
        '\u{ad}',
        '\u{200b}',
        '\u{200d}',
        '\u{200e}',
        '\u{202e}',
        '\u{2060}',
        '\u{2066}',
        '\u{feff}',
        '\u{e0001}',
    ] {
        assert!(is_format_char(c), "{c:?}");
    }
    for c in ['a', ' ', '\u{e9}', '-', '\u{b7}', '\u{2028}', '\n', '\u{4e2d}'] {
        assert!(!is_format_char(c), "{c:?}");
    }
}

#[test]
fn the_protocol_document_names_the_agent_members_and_its_examples_read_with_the_code() {
    let doc = std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/inbox/protocol.md"),
    )
    .expect("protocol.md");
    // The limits of the text of an agent, in the order of the table of the document.
    for (text, n) in [
        ("at most 200 entries", MAX_SYNC_AGENTS),
        ("1 to 32 characters, with no NUL", MAX_AGENT_NAME_CHARS),
        ("with 1 to 64 characters", MAX_AGENT_CLIENT_CHARS),
        ("with 1 to 128 characters", MAX_AGENT_USER_AGENT_CHARS),
        ("with 1 to 43 characters", MAX_AGENT_NET_CHARS),
        ("1 to 64 characters, one line", MAX_AGENT_LABEL_CHARS),
        ("0 to 8", MAX_AGENT_NETS_SEEN as usize),
    ] {
        assert!(doc.contains(text), "{text}");
        let digits: String = text.chars().filter(|c| c.is_ascii_digit()).collect();
        assert!(digits.contains(&n.to_string()), "{text} against {n}");
    }
    assert!(doc.contains("From 1 to 9223372036854775807"), "the largest rev");
    assert_eq!(i64::MAX.to_string(), "9223372036854775807");
    // Every value of every closed list of the agents is in the document, and the document has no other value in them.
    for text in ["agent", "bucket", "new", "known", "blocked", "merged"] {
        assert!(doc.contains(&format!("`{text}`")), "{text}");
    }
    for status in [
        AgentResultStatus::Ok,
        AgentResultStatus::NotFound,
        AgentResultStatus::InvalidName,
        AgentResultStatus::NameTaken,
        AgentResultStatus::BadState,
        AgentResultStatus::CrossKey,
        AgentResultStatus::NotEnabled,
    ] {
        assert!(doc.contains(&format!("| `{}` |", status.as_str())), "{status:?}");
    }
    assert!(doc.contains("| `agent_locked` |"), "the status of an answer");
    for op in [
        AgentOpKind::Rename,
        AgentOpKind::Allow,
        AgentOpKind::Block,
        AgentOpKind::Unblock,
        AgentOpKind::Merge,
    ] {
        assert!(doc.contains(&format!("- `{}`:", op.as_str())), "{op:?}");
    }
    // The members of the new frames are in the document.
    for member in [
        "agent_id",
        "rev",
        "key_name",
        "kind",
        "state",
        "label",
        "named",
        "merged_into",
        "created_at_ms",
        "last_seen_ms",
        "open",
        "lock_accept",
        "observed",
        "similar",
        "says",
        "says_changed",
        "client",
        "user_agent",
        "net",
        "nets_seen",
        "client_msg_id",
        "op",
        "name",
        "into",
        "agent_kind",
        "agent_state",
        "agent_label",
        "status",
    ] {
        assert!(doc.contains(&format!("`{member}`")), "{member}");
    }
    // The examples are frames that the parsers read, once their short ids are made whole.
    let whole = |text: &str| -> String {
        let mut out = String::new();
        let mut rest = text;
        while let Some(at) = rest.find("...\"") {
            // `"aaaa..."`: a short id that stands for 32 hexadecimal characters.
            let start = rest[..at].rfind('"').unwrap();
            let hex = &rest[start + 1..at];
            if hex.len() == 4 && hex.bytes().all(|b| b.is_ascii_hexdigit()) {
                out.push_str(&rest[..start + 1]);
                out.push_str(&hex.repeat(8));
                out.push('"');
            } else {
                out.push_str(&rest[..at + 4]);
            }
            rest = &rest[at + 4..];
        }
        out.push_str(rest);
        out
    };
    let example = |start: &str| -> String {
        let at = doc.find(start).unwrap_or_else(|| panic!("no example {start}"));
        let end = doc[at..].find("\n```").unwrap();
        whole(&doc[at..at + end])
    };
    for start in [r#"{"t":"agent","#, r#"{"t":"agent_result","#] {
        let frame = example(start);
        ServerMsg::decode(frame.as_bytes()).unwrap_or_else(|e| panic!("{frame}: {e}"));
    }
    let frame = example(r#"{"t":"agent_do","#);
    ClientMsg::decode(frame.as_bytes()).unwrap_or_else(|e| panic!("{frame}: {e}"));
    // The example of `sync` is in a line of text: it ends at the full stop that follows it.
    let at = doc
        .find(r#"`{"t":"sync","open":[{"request_id":"bbbb...","seq":2}],"agents""#)
        .expect("the example of sync");
    let line = &doc[at + 1..];
    let frame = whole(&line[..line.find("`.").unwrap()]);
    let ClientMsg::Sync { open, agents } = ClientMsg::decode(frame.as_bytes()).unwrap() else {
        panic!()
    };
    assert_eq!((open.len(), agents.map(|a| a.len())), (1, Some(1)));
    // The request example keeps its form and now has the agent in `from`.
    assert!(doc.contains(r#""from":{"label":"claude-laptop","key_name":"claude-laptop","owner":"alice","via":"mcp","interaction":"conversational","agent_id":"#));
}
