//! The frames of protocol 5 that the server builds from an item: `request`, `message`, `settled` and `digest`.
//!
//! The row of `alerts` for an item holds metadata only. The server unseals the body and the turns when it sends a
//! frame, and builds the frame here. A function of this module reads plain values and writes a frame: it touches no
//! storage and no socket, so a test checks each rule without a server.
//!
//! The module also holds the formats of the sealed texts of the turns, so that every part of the portal that writes or
//! reads a turn uses the same one.
//!
//! # Formats of the sealed texts
//!
//! - The body of an approval is the JSON object
//!   `{op, view, explanation?, explanation_format?, language?, hint?}`. `language` colors the command.
//! - The body of a question is the JSON object
//!   `{title, explanation?, explanation_format?, command?: {text, label?, language?}, input, hint?, as_of_ms?}`.
//!   The `input` can be a form: its `questions` are in the body.
//! - A turn of the agent (`say`) is the JSON object
//!   `{explanation?, explanation_format?, command?: {text, label?, language?}}`. A text that is not such an object
//!   reads as the explanation, in plain text.
//! - A `reply` of the owner is the text itself.
//! - The text of an answer of the owner is the JSON object `{choice?: [id], text?, intent?, answers?}`. `answers` is
//!   the answer to a form: `[{question_id, choice?: [id], text?}]`. A text that is not such an object reads as the
//!   text of the owner, with no intent.

// The agent channel (lot L9) writes the sealed turns with the types of this module.
#![allow(dead_code)]

use inbox_core::effect::{Op, View};
use inbox_core::proto::{
    ApprovalBody, Author, Channel, Command, DigestMsg, ExplanationFormat, Follows, FormAnswer,
    Hint, Input, Intent, ItemState, Kind, Language, LeaseMode, MessageMsg, Previous, RequestMsg,
    Sender, SettledBy, SettledMsg,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::items::{ItemRow, Loaded};
use super::items_ops::TurnRow;

/// Why a frame cannot be built. The delivery is then withdrawn: the phone never gets a frame that it cannot trust.
#[derive(Debug, PartialEq, Eq)]
pub enum FrameError {
    /// The sealed text is not the format of its kind.
    Body(&'static str),
    /// The frame breaks a rule of the core crate (a size, a character).
    Invalid(String),
}

impl std::fmt::Display for FrameError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FrameError::Body(why) => write!(f, "the sealed text is not valid: {why}"),
            FrameError::Invalid(why) => write!(f, "the frame is not valid: {why}"),
        }
    }
}

// ---------------------------------------------------------------- the sealed texts

/// The command of the agent in a turn.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommandText {
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub language: Option<Language>,
}

/// A turn of the agent, as the portal seals it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentTurn {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub explanation: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub explanation_format: Option<ExplanationFormat>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<CommandText>,
}

impl AgentTurn {
    /// The text to seal.
    pub fn to_plain(&self) -> Vec<u8> {
        serde_json::to_vec(self).expect("a turn serializes")
    }

    /// Reads a sealed turn. A text that is not the object of a turn is the explanation.
    pub fn from_plain(plain: &[u8]) -> AgentTurn {
        match serde_json::from_slice::<AgentTurn>(plain) {
            Ok(turn) if turn.explanation.is_some() || turn.command.is_some() => turn,
            _ => AgentTurn {
                explanation: Some(String::from_utf8_lossy(plain).into_owned()),
                explanation_format: None,
                command: None,
            },
        }
    }
}

/// The text and the choice of the owner in an answer, as the portal seals them.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct OwnerAnswer {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub choice: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub intent: Option<Intent>,
    /// The answer to a form. It excludes `choice`, `text` and `intent`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub answers: Option<Vec<FormAnswer>>,
}

impl OwnerAnswer {
    /// What an `answer` frame says besides the action: the choice, the text and the intent.
    pub fn from_msg(msg: &inbox_core::proto::AnswerMsg) -> OwnerAnswer {
        OwnerAnswer {
            choice: msg.choice.clone(),
            text: msg.text.clone(),
            intent: msg.intent,
            answers: msg.answers.clone(),
        }
    }

    /// Whether the answer holds anything to seal.
    pub fn is_empty(&self) -> bool {
        self.choice.is_none()
            && self.text.is_none()
            && self.intent.is_none()
            && self.answers.is_none()
    }

    pub fn to_plain(&self) -> Vec<u8> {
        serde_json::to_vec(self).expect("an answer serializes")
    }

    /// Reads a sealed answer. A text that is not the object of an answer is the text of the owner, with no intent.
    pub fn from_plain(plain: &[u8]) -> OwnerAnswer {
        match serde_json::from_slice::<OwnerAnswer>(plain) {
            Ok(answer) if !answer.is_empty() => answer,
            _ => OwnerAnswer {
                choice: None,
                text: Some(String::from_utf8_lossy(plain).into_owned()),
                intent: None,
                answers: None,
            },
        }
    }
}

/// The body of a question.
#[derive(Debug, Clone, Deserialize)]
struct QuestionBody {
    title: String,
    #[serde(default)]
    explanation: Option<String>,
    #[serde(default)]
    explanation_format: Option<ExplanationFormat>,
    #[serde(default)]
    command: Option<Command>,
    input: Input,
    #[serde(default)]
    hint: Option<Hint>,
    #[serde(default)]
    as_of_ms: Option<i64>,
}

/// The extra facts of an approval that the server finds in other rows.
#[derive(Debug, Clone, Default)]
pub struct ApprovalFacts {
    /// The same effect was approved before: the time of the answer, and whether the service used it.
    pub previous: Option<(i64, bool)>,
    /// The earlier item that the owner declined with a text: its code and the intent of the text.
    pub follows: Option<(String, Intent)>,
}

// ---------------------------------------------------------------- the frames

/// The line "from": the portal writes it. A label that an agent gives is never here.
pub fn sender(item: &ItemRow, key_name: Option<&str>) -> Sender {
    let name = key_name
        .map(clean_line)
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| {
            let short: String = item.requester_key_id.chars().take(8).collect();
            format!("key {short}")
        });
    Sender {
        label: name.clone(),
        key_name: name,
        owner: clean_line(&item.requester_owner),
        via: item.via,
        interaction: item.interaction,
    }
}

/// One line of 1 to 64 characters: a control character becomes a space.
fn clean_line(text: &str) -> String {
    text.chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .take(64)
        .collect::<String>()
        .trim()
        .to_string()
}

/// The hook of the quiet hours: true when the item came inside the quiet hours of the account (`created_at_ms` is the
/// time of the arrival, in the zone of the window). The server never holds an item back, so the answer only tells the
/// phone to be silent. The flag does not depend on the urgency: the phone reads `high_rings` of its preferences.
pub fn arrived_quiet(prefs: &inbox_core::proto::Prefs, created_at_ms: i64) -> bool {
    prefs
        .quiet
        .as_ref()
        .is_some_and(|q| super::prefs::in_quiet(q, created_at_ms))
}

/// The `request` frame of an item. `id` is the id of the delivery. `body` is the unsealed body.
pub fn request_frame(
    id: &str,
    l: &Loaded,
    body: &[u8],
    from: Sender,
    facts: &ApprovalFacts,
) -> Result<RequestMsg, FrameError> {
    let item = &l.item;
    let value: Value =
        serde_json::from_slice(body).map_err(|_| FrameError::Body("the body is not JSON"))?;
    let (lease_mode, lease_until_ms, until_at_ms) = match (item.lease_s, item.until_at) {
        (Some(_), None) => (LeaseMode::Lease, item.lease_until, None),
        (None, Some(at)) => (LeaseMode::Until, None, Some(at)),
        _ => return Err(FrameError::Body("the item has no wait")),
    };
    let mut frame = RequestMsg {
        id: id.to_string(),
        request_id: item.request_id.clone(),
        kind: item.kind,
        chain_id: item.chain_id.clone(),
        seq: item.last_seq,
        from,
        urgency: item.urgency,
        // The caller sets the flag from the preferences of the owner (see `arrived_quiet`).
        quiet: false,
        created_at_ms: item.created_at,
        chain_created_at_ms: item.chain_created_at,
        agent_seen_at_ms: item.agent_seen_at,
        lease_mode,
        lease_until_ms,
        until_at_ms,
        allow_custom: item.allow_custom,
        allow_ask_back: item.allow_ask_back,
        supersedes: item.supersedes.clone(),
        previous_id: item.previous_id.clone(),
        title: None,
        explanation: None,
        explanation_format: None,
        command: None,
        input: None,
        hint: None,
        as_of_ms: None,
        approval: None,
    };
    match (item.kind, &l.approval) {
        (Kind::Approval, Some(a)) => {
            let op = value
                .get("op")
                .cloned()
                .ok_or(FrameError::Body("the body has no op"))?;
            let op = Op::from_value(op).map_err(|_| FrameError::Body("the op is not valid"))?;
            let view: View = serde_json::from_value(
                value
                    .get("view")
                    .cloned()
                    .ok_or(FrameError::Body("the body has no view"))?,
            )
            .map_err(|_| FrameError::Body("the view is not valid"))?;
            let hint = match value.get("hint").filter(|h| !h.is_null()) {
                Some(h) => Some(
                    serde_json::from_value::<Hint>(h.clone())
                        .map_err(|_| FrameError::Body("the hint is not valid"))?,
                ),
                None => None,
            };
            let explanation = value
                .get("explanation")
                .and_then(Value::as_str)
                .map(str::to_string);
            let explanation_format = match value.get("explanation_format").filter(|v| !v.is_null())
            {
                Some(f) => Some(
                    serde_json::from_value::<ExplanationFormat>(f.clone())
                        .map_err(|_| FrameError::Body("the explanation format is not valid"))?,
                ),
                None => None,
            };
            let language = match value.get("language").filter(|v| !v.is_null()) {
                Some(l) => Some(
                    serde_json::from_value::<Language>(l.clone())
                        .map_err(|_| FrameError::Body("the language is not valid"))?,
                ),
                None => None,
            };
            frame.approval = Some(ApprovalBody {
                code: a.code.clone(),
                service: a.service.clone(),
                action: a.action.clone(),
                resource: a.resource_path.clone(),
                subject: a.subject.clone(),
                effect: a.effect.clone(),
                op,
                view,
                explanation,
                explanation_format,
                language,
                hint,
                previous: facts.previous.map(|(approved_at_ms, consumed)| Previous {
                    approved_at_ms,
                    consumed,
                }),
                follows: facts
                    .follows
                    .clone()
                    .map(|(code, intent)| Follows { code, intent }),
            });
        }
        (Kind::Question, _) => {
            let q: QuestionBody = serde_json::from_value(value)
                .map_err(|_| FrameError::Body("the question is not valid"))?;
            frame.title = Some(q.title);
            frame.explanation = q.explanation;
            frame.explanation_format = q.explanation_format;
            frame.command = q.command;
            frame.input = Some(q.input);
            frame.hint = q.hint;
            frame.as_of_ms = q.as_of_ms;
        }
        _ => return Err(FrameError::Body("the item has no approval row")),
    }
    // The size and the character rules of the core crate: the server never sends what a phone must refuse.
    inbox_core::proto::ServerMsg::Request(Box::new(frame.clone()))
        .validate()
        .map_err(|e| FrameError::Invalid(e.to_string()))?;
    Ok(frame)
}

/// The `message` frame of a turn. `id` is the id of the delivery. `plain` is the unsealed text of the turn.
pub fn message_frame(id: &str, row: &TurnRow, plain: &[u8]) -> Result<MessageMsg, FrameError> {
    let by = if row.kind == "say" {
        Author::Agent
    } else {
        Author::User
    };
    let mut frame = MessageMsg {
        id: id.to_string(),
        request_id: row.request_id.clone(),
        seq: row.seq,
        by,
        created_at_ms: row.created_at,
        explanation: None,
        explanation_format: None,
        command: None,
        text: None,
    };
    match by {
        Author::Agent => {
            let turn = AgentTurn::from_plain(plain);
            frame.explanation = turn.explanation;
            // A format without a text is a mistake of the writer: the frame would be refused.
            frame.explanation_format = turn
                .explanation_format
                .filter(|_| frame.explanation.is_some());
            frame.command = turn.command.map(|c| Command {
                text: c.text,
                label: c.label,
                language: c.language,
            });
        }
        Author::User => frame.text = Some(String::from_utf8_lossy(plain).into_owned()),
    }
    inbox_core::proto::ServerMsg::Message(frame.clone())
        .validate()
        .map_err(|e| FrameError::Invalid(e.to_string()))?;
    Ok(frame)
}

/// The `settled` frame of a final item. `None` for an item that is open. `device_name` is the name of the phone that
/// answered, when a phone did.
pub fn settled_frame(id: &str, l: &Loaded, device_name: Option<&str>) -> Option<SettledMsg> {
    let item = &l.item;
    if !item.state.is_final() {
        return None;
    }
    let by = match item.answered_via.as_deref() {
        Some(v) if v.starts_with("device:") && item.state == ItemState::Answered => {
            Some(SettledBy {
                via: Channel::Phone,
                device_name: device_name.map(clean_line).filter(|n| !n.is_empty()),
            })
        }
        Some(v) if v.starts_with("web:") && item.state == ItemState::Answered => Some(SettledBy {
            via: Channel::Web,
            device_name: None,
        }),
        _ => None,
    };
    Some(SettledMsg {
        id: id.to_string(),
        request_id: item.request_id.clone(),
        state: item.state,
        reason: item
            .settle_reason
            .clone()
            .unwrap_or_else(|| super::items::state_str(item.state).to_string()),
        by,
        superseded_by: item
            .superseded_by
            .clone()
            .filter(|_| item.state == ItemState::Superseded),
        decision: l
            .approval
            .as_ref()
            .and_then(|a| a.decision)
            .filter(|_| item.state == ItemState::Answered),
        at_ms: item.final_at.unwrap_or(item.created_at).max(0),
    })
}

/// The `settled` frame of an item that the server does not hold any more (its retention ended). The item is final.
pub fn gone_frame(id: &str, request_id: &str, now_ms: i64) -> SettledMsg {
    SettledMsg {
        id: id.to_string(),
        request_id: request_id.to_string(),
        state: ItemState::Cancelled,
        reason: "item_gone".into(),
        by: None,
        superseded_by: None,
        decision: None,
        at_ms: now_ms.max(0),
    }
}

/// The `digest` frame: the count and the age of the open items. `open` holds the open items of the owner. `None`
/// when no item is open: the server sends no empty digest.
pub fn digest_frame(id: &str, open: &[Loaded]) -> Option<DigestMsg> {
    if open.is_empty() {
        return None;
    }
    let mut ordered: Vec<&ItemRow> = open.iter().map(|l| &l.item).collect();
    ordered.sort_by(|a, b| {
        a.created_at
            .cmp(&b.created_at)
            .then_with(|| a.request_id.cmp(&b.request_id))
    });
    Some(DigestMsg {
        id: id.to_string(),
        count: u32::try_from(ordered.len()).unwrap_or(u32::MAX),
        oldest_created_at_ms: ordered[0].created_at.max(0),
        ids: ordered
            .iter()
            .take(inbox_core::proto::MAX_DIGEST_IDS)
            .map(|i| i.request_id.clone())
            .collect(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::inbox::items::ApprovalRow;
    use inbox_core::effect::effect_hash;
    use inbox_core::proto::{Decision, InputType, Interaction, Urgency, Via};

    fn item(kind: Kind) -> ItemRow {
        ItemRow {
            request_id: "b".repeat(32),
            kind,
            chain_id: "b".repeat(32),
            supersedes: None,
            superseded_by: None,
            previous_id: None,
            requester_key_id: "2b7c91d0e4a853f6".into(),
            requester_owner: "alice".into(),
            agent_id: "a".repeat(32),
            via: Via::Mcp,
            interaction: Interaction::Conversational,
            urgency: Urgency::High,
            input_type: None,
            state: ItemState::Pending,
            settle_reason: None,
            body_kid: Some("k1".into()),
            body_size: 10,
            body_purged_at: None,
            created_at: 1_788_000_000_000,
            chain_created_at: 1_788_000_000_000,
            lease_s: Some(60),
            lease_until: Some(1_788_000_180_000),
            until_at: None,
            agent_seen_at: 1_788_000_001_000,
            last_agent_turn_at: None,
            abandoned_at: None,
            revivals: 0,
            last_seq: 0,
            allow_custom: true,
            allow_ask_back: true,
            answer_action: None,
            answered_at: None,
            answered_by: None,
            answered_via: None,
            final_at: None,
            idempotency_key: "k".into(),
        }
    }

    fn approval_body() -> (Value, String) {
        let op = serde_json::json!({"tool": "run_write_query", "sql": "INSERT INTO t SELECT 1"});
        let effect = effect_hash("db", "data.write", "host:prod", &op).unwrap();
        let body = serde_json::json!({
            "op": op,
            "view": {"title": "Write", "fields": [["Tool", "/tool"]], "body": "/sql"},
            "explanation": "The migration needs this row."
        });
        (body, effect)
    }

    fn approval() -> Loaded {
        let (_, effect) = approval_body();
        Loaded {
            item: item(Kind::Approval),
            approval: Some(ApprovalRow {
                request_id: "b".repeat(32),
                service: "db".into(),
                subject: "key:2b7c91d0e4a853f6".into(),
                action: "data.write".into(),
                resource_path: "host:prod".into(),
                effect,
                key_hash: "h".into(),
                evaluation_id: None,
                code: "K7QF".into(),
                decision: None,
                shown_effect: None,
                consume_before: None,
                consumed_at: None,
                lapsed_at: None,
            }),
        }
    }

    #[test]
    fn an_approval_frame_carries_the_command_and_verifies() {
        let l = approval();
        let (body, _) = approval_body();
        let from = sender(&l.item, Some("claude-laptop"));
        let f = request_frame(
            &"a".repeat(32),
            &l,
            body.to_string().as_bytes(),
            from,
            &ApprovalFacts::default(),
        )
        .unwrap();
        assert_eq!(f.kind, Kind::Approval);
        assert_eq!(f.from.label, "claude-laptop");
        assert_eq!(f.lease_mode, LeaseMode::Lease);
        let a = f.approval.unwrap();
        assert!(a.verify(), "the phone computes the same effect");
        assert_eq!(a.code, "K7QF");
        assert_eq!(
            a.explanation.as_deref(),
            Some("The migration needs this row.")
        );
    }

    #[test]
    fn the_line_from_never_holds_text_of_an_agent() {
        let l = approval();
        let s = sender(&l.item, None);
        assert_eq!(s.label, "key 2b7c91d0");
        let s = sender(&l.item, Some("bad\nname\u{7}"));
        assert_eq!(s.label, "bad name");
    }

    #[test]
    fn previous_and_follows_come_from_the_facts() {
        let l = approval();
        let (body, _) = approval_body();
        let facts = ApprovalFacts {
            previous: Some((1_788_000_000_500, true)),
            follows: Some(("AB12".into(), Intent::ChangeRequest)),
        };
        let f = request_frame(
            &"a".repeat(32),
            &l,
            body.to_string().as_bytes(),
            sender(&l.item, None),
            &facts,
        )
        .unwrap();
        let a = f.approval.unwrap();
        assert_eq!(
            a.previous,
            Some(Previous {
                approved_at_ms: 1_788_000_000_500,
                consumed: true
            })
        );
        assert_eq!(
            a.follows,
            Some(Follows {
                code: "AB12".into(),
                intent: Intent::ChangeRequest
            })
        );
    }

    #[test]
    fn a_broken_body_gives_an_error_and_no_frame() {
        let l = approval();
        let none = ApprovalFacts::default();
        let id = "a".repeat(32);
        assert!(request_frame(&id, &l, b"not json", sender(&l.item, None), &none).is_err());
        assert!(request_frame(&id, &l, b"{\"view\":{}}", sender(&l.item, None), &none).is_err());
    }

    #[test]
    fn a_question_frame_has_the_members_of_a_question() {
        let mut l = approval();
        l.approval = None;
        l.item.kind = Kind::Question;
        l.item.input_type = Some(InputType::Confirm);
        let body = serde_json::json!({
            "title": "Deploy now?",
            "input": {"type": "confirm", "multiple": false, "min": 0, "max": 0, "options": []}
        });
        let f = request_frame(
            &"a".repeat(32),
            &l,
            body.to_string().as_bytes(),
            sender(&l.item, Some("k")),
            &ApprovalFacts::default(),
        )
        .unwrap();
        assert_eq!(f.title.as_deref(), Some("Deploy now?"));
        assert!(f.approval.is_none());
    }

    fn form_input() -> Value {
        serde_json::json!({
            "type": "form", "multiple": false, "min": 0, "max": 0, "options": [],
            "questions": [
                {"id": "target", "header": "Target", "text": "Where?", "type": "choice",
                 "multiple": false, "min": 1, "max": 1,
                 "options": [{"id": "staging", "label": "Staging", "description": "Test"},
                             {"id": "prod", "label": "Production"}]},
                {"id": "note", "header": "Note", "text": "Anything else?", "type": "text",
                 "multiple": false, "min": 0, "max": 0, "options": [], "placeholder": "No secret"}
            ]
        })
    }

    fn question(body: Value) -> Result<RequestMsg, FrameError> {
        let mut l = approval();
        l.approval = None;
        l.item.kind = Kind::Question;
        l.item.input_type = Some(InputType::Form);
        request_frame(
            &"a".repeat(32),
            &l,
            body.to_string().as_bytes(),
            sender(&l.item, Some("k")),
            &ApprovalFacts::default(),
        )
    }

    #[test]
    fn a_form_and_the_text_formats_reach_the_question_frame() {
        let f = question(serde_json::json!({
            "title": "Set up the release",
            "explanation": "Two **short** questions.",
            "explanation_format": "markdown",
            "command": {"text": "make release", "label": "Shell", "language": "bash"},
            "input": form_input()
        }))
        .unwrap();
        assert_eq!(f.explanation_format, Some(ExplanationFormat::Markdown));
        assert_eq!(f.command.as_ref().unwrap().language, Some(Language::Bash));
        let input = f.input.as_ref().unwrap();
        assert_eq!(input.kind, inbox_core::proto::InputType::Form);
        let questions = input.questions.as_ref().unwrap();
        assert_eq!(questions.len(), 2);
        assert_eq!(questions[0].options[0].description.as_deref(), Some("Test"));
        // The frame passes the rules of the core crate: it is encodable as it stands.
        inbox_core::proto::ServerMsg::Request(Box::new(f))
            .encode()
            .unwrap();
    }

    #[test]
    fn a_stored_body_that_breaks_a_rule_of_the_form_gives_no_frame() {
        let mut bad_input = form_input();
        bad_input["questions"][1]["id"] = serde_json::json!("target");
        assert!(question(serde_json::json!({"title": "t", "input": bad_input})).is_err());
        let mut five = form_input();
        let q = five["questions"][1].clone();
        five["questions"] = serde_json::json!([q, q, q, q, q]);
        assert!(question(serde_json::json!({"title": "t", "input": five})).is_err());
        // A format without an explanation, a language out of the list.
        let ok = form_input();
        assert!(question(
            serde_json::json!({"title": "t", "explanation_format": "markdown", "input": ok})
        )
        .is_err());
        assert!(question(serde_json::json!({"title": "t", "command": {"text": "x", "language": "cobol"}, "input": ok})).is_err());
        assert!(question(serde_json::json!({"title": "t", "explanation": "x", "explanation_format": "html", "input": ok})).is_err());
        // A form on a one_shot key.
        let mut l = approval();
        l.approval = None;
        l.item.kind = Kind::Question;
        l.item.interaction = Interaction::OneShot;
        l.item.allow_custom = false;
        l.item.allow_ask_back = false;
        let r = request_frame(
            &"a".repeat(32),
            &l,
            serde_json::json!({"title": "t", "input": form_input()})
                .to_string()
                .as_bytes(),
            sender(&l.item, Some("k")),
            &ApprovalFacts::default(),
        );
        assert!(r.is_err(), "a closed automatic system gives one decision");
    }

    #[test]
    fn an_approval_frame_carries_the_format_and_the_language() {
        let l = approval();
        let (mut body, _) = approval_body();
        body["explanation_format"] = serde_json::json!("markdown");
        body["language"] = serde_json::json!("sql");
        let f = request_frame(
            &"a".repeat(32),
            &l,
            body.to_string().as_bytes(),
            sender(&l.item, None),
            &ApprovalFacts::default(),
        )
        .unwrap();
        let a = f.approval.unwrap();
        assert_eq!(a.explanation_format, Some(ExplanationFormat::Markdown));
        assert_eq!(a.language, Some(Language::Sql));
        assert!(a.verify(), "the hint and the format are outside the hash");
        // Without them the members are absent.
        let (plain, _) = approval_body();
        let f = request_frame(
            &"a".repeat(32),
            &l,
            plain.to_string().as_bytes(),
            sender(&l.item, None),
            &ApprovalFacts::default(),
        )
        .unwrap();
        let a = f.approval.unwrap();
        assert_eq!((a.explanation_format, a.language), (None, None));
        // A value out of the list is not a frame.
        let mut bad = plain;
        bad["language"] = serde_json::json!("brainfuck");
        assert!(request_frame(
            &"a".repeat(32),
            &l,
            bad.to_string().as_bytes(),
            sender(&l.item, None),
            &ApprovalFacts::default()
        )
        .is_err());
    }

    #[test]
    fn a_markdown_turn_of_the_agent_keeps_its_format() {
        let sealed = AgentTurn {
            explanation: Some("Try **this**".into()),
            explanation_format: Some(ExplanationFormat::Markdown),
            command: Some(CommandText {
                text: "ls".into(),
                label: None,
                language: Some(Language::Bash),
            }),
        };
        let row = TurnRow {
            request_id: "b".repeat(32),
            seq: 2,
            author: "key:k".into(),
            kind: "say".into(),
            payload: None,
            client_msg_id: None,
            device_id: None,
            via: None,
            created_at: 1_788_000_100_000,
        };
        let f = message_frame(&"c".repeat(32), &row, &sealed.to_plain()).unwrap();
        assert_eq!(f.explanation_format, Some(ExplanationFormat::Markdown));
        assert_eq!(f.command.unwrap().language, Some(Language::Bash));
        // A text that is not the object of a turn is plain text: it has no format.
        let f = message_frame(&"c".repeat(32), &row, b"**not** markdown").unwrap();
        assert_eq!(f.explanation.as_deref(), Some("**not** markdown"));
        assert_eq!(f.explanation_format, None);
        // The text of the owner never has one.
        let mut user = row;
        user.kind = "reply".into();
        user.author = "user:alice".into();
        let f = message_frame(&"c".repeat(32), &user, b"**plain** too").unwrap();
        assert_eq!(
            (f.explanation_format, f.text.as_deref()),
            (None, Some("**plain** too"))
        );
    }

    #[test]
    fn the_owner_answer_holds_the_answers_of_a_form() {
        let msg = inbox_core::proto::AnswerMsg {
            request_id: "b".repeat(32),
            client_msg_id: "m-1".into(),
            action: inbox_core::proto::Action::Accept,
            shown_effect: None,
            choice: None,
            text: None,
            intent: None,
            answers: Some(vec![
                FormAnswer {
                    question_id: "target".into(),
                    choice: Some(vec!["prod".into()]),
                    text: None,
                },
                FormAnswer {
                    question_id: "note".into(),
                    choice: None,
                    text: Some("later".into()),
                },
            ]),
        };
        let a = OwnerAnswer::from_msg(&msg);
        assert!(!a.is_empty());
        let back = OwnerAnswer::from_plain(&a.to_plain());
        assert_eq!(back, a);
        assert_eq!(
            back.answers.as_ref().unwrap()[1].text.as_deref(),
            Some("later")
        );
        assert_eq!(
            String::from_utf8(a.to_plain()).unwrap(),
            r#"{"answers":[{"question_id":"target","choice":["prod"]},{"question_id":"note","text":"later"}]}"#
        );
    }

    #[test]
    fn a_turn_reads_as_the_explanation_when_it_is_plain_text() {
        let t = AgentTurn::from_plain(b"I need the production database.");
        assert_eq!(
            t.explanation.as_deref(),
            Some("I need the production database.")
        );
        let sealed = AgentTurn {
            explanation: Some("why".into()),
            explanation_format: Some(ExplanationFormat::Markdown),
            command: Some(CommandText {
                text: "ls".into(),
                label: Some("Shell".into()),
                language: Some(Language::Bash),
            }),
        };
        assert_eq!(AgentTurn::from_plain(&sealed.to_plain()), sealed);
    }

    #[test]
    fn a_message_of_the_owner_is_the_text() {
        let row = TurnRow {
            request_id: "b".repeat(32),
            seq: 2,
            author: "user:alice".into(),
            kind: "reply".into(),
            payload: None,
            client_msg_id: Some("m-1".into()),
            device_id: Some("d".repeat(32)),
            via: Some("phone".into()),
            created_at: 5,
        };
        let m = message_frame(&"c".repeat(32), &row, "Why this database?".as_bytes()).unwrap();
        assert_eq!(m.by, Author::User);
        assert_eq!(m.text.as_deref(), Some("Why this database?"));
    }

    #[test]
    fn a_settled_frame_names_the_state_the_decision_and_the_phone() {
        let mut l = approval();
        assert!(
            settled_frame(&"d".repeat(32), &l, None).is_none(),
            "an open item is not settled"
        );
        l.item.state = ItemState::Answered;
        l.item.settle_reason = Some("answered".into());
        l.item.final_at = Some(1_788_000_010_000);
        l.item.answered_via = Some(format!("device:{}", "e".repeat(32)));
        l.approval.as_mut().unwrap().decision = Some(Decision::Approve);
        let s = settled_frame(&"d".repeat(32), &l, Some("Pixel")).unwrap();
        assert_eq!(s.state, ItemState::Answered);
        assert_eq!(s.decision, Some(Decision::Approve));
        assert_eq!(
            s.by,
            Some(SettledBy {
                via: Channel::Phone,
                device_name: Some("Pixel".into())
            })
        );
        l.item.state = ItemState::Abandoned;
        l.item.settle_reason = Some("lease_lapsed".into());
        let s = settled_frame(&"d".repeat(32), &l, None).unwrap();
        assert_eq!((s.by, s.decision), (None, None));
    }

    #[test]
    fn a_digest_has_the_count_and_the_oldest_first_and_never_comes_empty() {
        assert!(digest_frame(&"f".repeat(32), &[]).is_none());
        let mut young = approval();
        young.item.request_id = "1".repeat(32);
        young.item.created_at = 2_000;
        let mut old = approval();
        old.item.request_id = "2".repeat(32);
        old.item.created_at = 1_000;
        let d = digest_frame(&"f".repeat(32), &[young, old]).unwrap();
        assert_eq!(d.count, 2);
        assert_eq!(d.oldest_created_at_ms, 1_000);
        assert_eq!(d.ids, vec!["2".repeat(32), "1".repeat(32)]);
    }
}
