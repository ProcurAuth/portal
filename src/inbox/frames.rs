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
//!   `{op, view, explanation?, explanation_format?, language?, hint?, agent_label?}`. `language` colors the command.
//! - The body of a question is the JSON object
//!   `{title, explanation?, explanation_format?, command?: {text, label?, language?}, input, hint?, as_of_ms?,
//!   agent_label?}`. The `input` can be a form: its `questions` are in the body.
//! - `agent_label` is what the agent says that it is. It is text of the agent, not verified: the frame carries it
//!   next to the explanation and never in `from`.
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
    is_untrusted_line, AgentKind as WireKind, AgentMsg, AgentState as WireState, ApprovalBody,
    Author, Channel, Command, DigestMsg, ExplanationFormat, Follows, FormAnswer, Hint, Input,
    Intent, ItemState, Kind, Language, LeaseMode, MessageMsg, Observed as WireObserved, Previous,
    RequestMsg, Sender, SettledBy, SettledMsg, Similar, MAX_AGENT_CLIENT_CHARS,
    MAX_AGENT_LABEL_CHARS, MAX_AGENT_NET_CHARS, MAX_AGENT_USER_AGENT_CHARS,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::agents::{AgentKind, AgentMode, AgentRow, AgentState, Observed};
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

/// The name of the key in a frame: the name that the owner gave, else the word "key" and the first 8 characters of its
/// id. It is portal text.
pub fn key_display(key_id: &str, key_name: Option<&str>) -> String {
    key_name
        .map(clean_line)
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| {
            let short: String = key_id.chars().take(8).collect();
            format!("key {short}")
        })
}

/// The text of the portal for an agent, in a frame: the name that the owner gave, else the key name and the number of the
/// agent ("ops-key #4"). For the bucket: the key name in the mode `key`, else "Unattributed · <key name>". It has at most
/// 64 characters: a long key name is cut, never the part of the portal. The text of an agent is never in it.
pub fn agent_label(agent: &AgentRow, key_name: &str, mode: AgentMode) -> String {
    let full = agent.label(key_name, mode);
    let over = full.chars().count().saturating_sub(MAX_AGENT_LABEL_CHARS);
    if over == 0 {
        return full;
    }
    let keep = key_name.chars().count().saturating_sub(over);
    agent.label(&key_name.chars().take(keep).collect::<String>(), mode)
}

/// The state of an agent as `from.agent_state` of a request says it. An item of a `blocked` or a `merged` agent is
/// never an open request: the action of the owner cancels it or moves it, so there is no value for it.
fn from_state(agent: &AgentRow) -> Option<WireState> {
    match agent.state {
        AgentState::New => Some(WireState::New),
        AgentState::Known => Some(WireState::Known),
        AgentState::Blocked | AgentState::Merged => None,
    }
}

fn wire_kind(kind: AgentKind) -> WireKind {
    match kind {
        AgentKind::Agent => WireKind::Agent,
        AgentKind::Bucket => WireKind::Bucket,
    }
}

/// The line "from": the portal writes it. A label that an agent gives is never here. `agent` is the agent that the item
/// belongs to, and `mode` is what its key does with the agents. `None` when the agent is `blocked` or `merged`: the item
/// is not an open request.
pub fn sender(
    item: &ItemRow,
    key_name: Option<&str>,
    agent: &AgentRow,
    mode: AgentMode,
) -> Option<Sender> {
    let name = key_display(&item.requester_key_id, key_name);
    Some(Sender {
        label: agent_label(agent, &name, mode),
        key_name: name,
        owner: clean_line(&item.requester_owner),
        via: item.via,
        interaction: item.interaction,
        agent_id: agent.agent_id.clone(),
        agent_kind: wire_kind(agent.kind),
        agent_state: from_state(agent)?,
    })
}

/// What an `agent` frame needs besides the row of the agent.
pub struct AgentFacts<'a> {
    /// The name of the key, as the owner gave it.
    pub key_name: &'a str,
    pub mode: AgentMode,
    /// What the portal remembers of the hints of the agent. Unsealed for this frame only.
    pub observed: Option<&'a Observed>,
    /// The open items of the agent now.
    pub open: i64,
    /// `inbox.agents.lock_new`.
    pub lock_new: bool,
    /// The known agent that a `new` agent looks like. See [`similar_of`].
    pub similar: Option<Similar>,
}

/// The hint "looks like" for a `new` agent: a known agent of the same key that says the same text, or that has the same
/// client and the same address range (lot G5). The label is portal text, as for `label`. The hint is help for the owner:
/// the portal never merges by itself, and a hint can be copied. `others` are the agents of the owner with their hints.
pub fn similar_of<'a>(
    agent: &AgentRow,
    mine: Option<&Observed>,
    others: impl IntoIterator<Item = (&'a AgentRow, Option<Observed>)>,
    key_name: &str,
    mode: AgentMode,
) -> Option<Similar> {
    let (other, _) = super::agents::find_similar(agent, mine, others)?;
    Some(Similar {
        agent_id: other.agent_id.clone(),
        label: agent_label(other, key_name, mode),
    })
}

/// The hints of an agent as the frame carries them. A hint that the frame would refuse (a control character, a format
/// character, a length) is left out: a client text never stops the notice of a new agent. `None` for a bucket and for an
/// agent that shows nothing.
fn observed_frame(agent: &AgentRow, observed: Option<&Observed>) -> Option<WireObserved> {
    let o = observed.filter(|_| agent.kind == AgentKind::Agent)?;
    let keep = |t: &Option<String>, max| t.clone().filter(|t| is_untrusted_line(t, max));
    let says = keep(&o.says, MAX_AGENT_LABEL_CHARS);
    let out = WireObserved {
        says_changed: Some(o.says_changes).filter(|n| *n > 0 && says.is_some()),
        says,
        client: keep(&o.client, MAX_AGENT_CLIENT_CHARS),
        user_agent: keep(&o.user_agent, MAX_AGENT_USER_AGENT_CHARS),
        net: keep(&o.net, MAX_AGENT_NET_CHARS),
        nets_seen: o.nets_seen(),
    };
    let empty = out.says.is_none()
        && out.client.is_none()
        && out.user_agent.is_none()
        && out.net.is_none()
        && out.nets_seen == 0;
    (!empty).then_some(out)
}

/// The `agent` frame of an agent. `id` is the id of the delivery. The state of the frame is the state of the row now,
/// whatever `rev` the queue row named.
pub fn agent_frame(
    id: &str,
    agent: &AgentRow,
    facts: &AgentFacts<'_>,
) -> Result<AgentMsg, FrameError> {
    let rev = u64::try_from(agent.rev).map_err(|_| FrameError::Invalid("agent.rev".into()))?;
    let state = match agent.state {
        AgentState::New => WireState::New,
        AgentState::Known => WireState::Known,
        AgentState::Blocked => WireState::Blocked,
        AgentState::Merged => WireState::Merged,
    };
    let frame = AgentMsg {
        id: id.to_string(),
        agent_id: agent.agent_id.clone(),
        rev,
        key_name: clean_line(facts.key_name),
        kind: wire_kind(agent.kind),
        state,
        label: agent_label(agent, facts.key_name, facts.mode),
        named: agent.name.is_some(),
        merged_into: agent
            .merged_into
            .clone()
            .filter(|_| agent.state == AgentState::Merged),
        created_at_ms: agent.created_at.max(0),
        last_seen_ms: agent.last_seen_at.max(0),
        open: u32::try_from(facts.open.max(0)).unwrap_or(u32::MAX),
        lock_accept: agent.state == AgentState::New && facts.lock_new,
        observed: observed_frame(agent, facts.observed),
        similar: facts.similar.clone().filter(|_| state == WireState::New),
    };
    inbox_core::proto::ServerMsg::Agent(Box::new(frame.clone()))
        .validate()
        .map_err(|e| FrameError::Invalid(e.to_string()))?;
    Ok(frame)
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
    // What the agent says that it is. The channel checked it, and the frame refuses a text that it does not carry.
    let agent_label = value
        .get("agent_label")
        .and_then(Value::as_str)
        .filter(|t| is_untrusted_line(t, MAX_AGENT_LABEL_CHARS))
        .map(str::to_string);
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
        agent_label,
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

    /// The agent of the test item: a known agent that the owner named "Billing bot".
    fn agent_row(item: &ItemRow) -> AgentRow {
        AgentRow {
            agent_id: item.agent_id.clone(),
            key_id: item.requester_key_id.clone(),
            owner: item.requester_owner.clone(),
            kind: AgentKind::Agent,
            state: AgentState::Known,
            origin: super::super::agents::AgentOrigin::Call,
            ordinal: 4,
            name: Some("Billing bot".into()),
            name_key: Some("billing bot".into()),
            token_digest: Some("d".repeat(64)),
            merged_into: None,
            rev: 5,
            created_at: 1_787_000_000_000,
            named_at: Some(1_787_000_100_000),
            last_seen_at: 1_788_000_000_000,
            hints: None,
        }
    }

    /// The line "from" of a test item, with the agent of the test.
    fn sender(item: &ItemRow, key_name: Option<&str>) -> Sender {
        super::sender(item, key_name, &agent_row(item), AgentMode::Discover).unwrap()
    }

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
        assert_eq!(f.from.label, "Billing bot");
        assert_eq!(f.from.key_name, "claude-laptop");
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
        // The key name falls back to the first characters of the id, and a control character becomes a space.
        let s = sender(&l.item, None);
        assert_eq!(s.key_name, "key 2b7c91d0");
        let s = sender(&l.item, Some("bad\nname\u{7}"));
        assert_eq!(s.key_name, "bad name");
        // The label is the name that the owner gave. It never comes from the body of the item: the text of the agent is
        // in `agent_label`, outside `from`.
        let (mut body, _) = approval_body();
        body["agent_label"] = serde_json::json!("billing-bot");
        let from = sender(&l.item, Some("k"));
        let f = request_frame(
            &"a".repeat(32),
            &l,
            body.to_string().as_bytes(),
            from,
            &ApprovalFacts::default(),
        )
        .unwrap();
        assert_eq!(
            (f.from.label.as_str(), f.agent_label.as_deref()),
            ("Billing bot", Some("billing-bot"))
        );
        // Without a name, the label is the key name and the number of the agent: portal text.
        let mut unnamed = agent_row(&l.item);
        unnamed.name = None;
        unnamed.name_key = None;
        unnamed.state = AgentState::New;
        let s = super::sender(&l.item, Some("ops-key"), &unnamed, AgentMode::Discover).unwrap();
        assert_eq!(
            (s.label.as_str(), s.agent_kind, s.agent_state),
            ("ops-key #4", WireKind::Agent, WireState::New)
        );
        // The bucket: the key name in the mode `key`, "Unattributed" in the other modes. It is `known`.
        let mut bucket = unnamed.clone();
        bucket.kind = AgentKind::Bucket;
        bucket.state = AgentState::Known;
        bucket.token_digest = None;
        let key_mode = super::sender(&l.item, Some("ops-key"), &bucket, AgentMode::Key).unwrap();
        assert_eq!(
            (key_mode.label.as_str(), key_mode.agent_kind),
            ("ops-key", WireKind::Bucket)
        );
        let other = super::sender(&l.item, Some("ops-key"), &bucket, AgentMode::Discover).unwrap();
        assert_eq!(other.label, "Unattributed · ops-key");
        assert_eq!(other.agent_state, WireState::Known);
        // An item of a blocked or a merged agent is not an open request: no line "from".
        for state in [AgentState::Blocked, AgentState::Merged] {
            let mut gone = unnamed.clone();
            gone.state = state;
            assert!(
                super::sender(&l.item, Some("k"), &gone, AgentMode::Discover).is_none(),
                "{state:?}"
            );
        }
    }

    #[test]
    fn the_label_of_an_agent_has_at_most_64_characters_and_cuts_the_key_name_only() {
        let l = approval();
        let mut unnamed = agent_row(&l.item);
        unnamed.name = None;
        unnamed.ordinal = 12;
        let long_key = "k".repeat(64);
        let s = super::sender(&l.item, Some(&long_key), &unnamed, AgentMode::Discover).unwrap();
        assert_eq!(s.label.chars().count(), 64);
        assert!(
            s.label.ends_with(" #12"),
            "the number of the agent stays: {}",
            s.label
        );
        let mut bucket = unnamed.clone();
        bucket.kind = AgentKind::Bucket;
        bucket.token_digest = None;
        let s = super::sender(&l.item, Some(&long_key), &bucket, AgentMode::Discover).unwrap();
        assert_eq!(s.label.chars().count(), 64);
        assert!(s.label.starts_with("Unattributed · k"), "{}", s.label);
        // In the mode `key`, the label is the key name, and it fits already.
        let s = super::sender(&l.item, Some(&long_key), &bucket, AgentMode::Key).unwrap();
        assert_eq!(s.label, long_key);
        // The frame passes the rules of the core crate for each case.
        let mut frame = request_frame(
            &"a".repeat(32),
            &l,
            approval_body().0.to_string().as_bytes(),
            s,
            &ApprovalFacts::default(),
        )
        .unwrap();
        frame.agent_label = None;
        inbox_core::proto::ServerMsg::Request(Box::new(frame))
            .encode()
            .unwrap();
    }

    #[test]
    fn the_text_that_an_agent_says_reaches_the_frame_only_when_the_frame_carries_it() {
        let l = approval();
        let (mut body, _) = approval_body();
        let make = |body: &Value| {
            request_frame(
                &"a".repeat(32),
                &l,
                body.to_string().as_bytes(),
                sender(&l.item, Some("k")),
                &ApprovalFacts::default(),
            )
            .unwrap()
        };
        assert_eq!(make(&body).agent_label, None);
        body["agent_label"] = serde_json::json!("Night ETL");
        assert_eq!(make(&body).agent_label.as_deref(), Some("Night ETL"));
        // A text that the frame would refuse is left out: the item is still delivered, with no label.
        for bad in ["a\u{202e}b", "a\nb", "", &"x".repeat(65)] {
            body["agent_label"] = serde_json::json!(bad);
            assert_eq!(make(&body).agent_label, None, "{bad:?}");
        }
        body["agent_label"] = serde_json::json!(7);
        assert_eq!(make(&body).agent_label, None);
    }

    fn facts<'a>(observed: Option<&'a Observed>, open: i64) -> AgentFacts<'a> {
        AgentFacts {
            key_name: "ops-key",
            mode: AgentMode::Discover,
            observed,
            open,
            lock_new: true,
            similar: None,
        }
    }

    fn observed_all() -> Observed {
        Observed {
            says: Some("billing-bot".into()),
            says_first: Some("report-bot".into()),
            says_changes: 2,
            client: Some("claude-code/2.1.0".into()),
            user_agent: Some("claude-code/2.1.0 (linux)".into()),
            net: Some("203.0.113.0/24".into()),
            nets: vec!["203.0.113.0/24".into(), "198.51.100.0/24".into()],
        }
    }

    #[test]
    fn the_frame_of_a_new_agent_names_a_similar_known_agent_and_no_other_frame_does() {
        let l = approval();
        let mut new = agent_row(&l.item);
        new.agent_id = "d4".repeat(16);
        new.name = None;
        new.name_key = None;
        new.state = AgentState::New;
        let mut known = agent_row(&l.item);
        known.agent_id = "a1".repeat(16);
        known.name = Some("Billing bot".into());
        known.name_key = Some("billing bot".into());
        known.state = AgentState::Known;
        let o = observed_all();
        let similar = similar_of(
            &new,
            Some(&o),
            [(&known, Some(o.clone()))],
            "ops-key",
            AgentMode::Discover,
        )
        .expect("the same text");
        assert_eq!(
            (similar.agent_id.as_str(), similar.label.as_str()),
            (known.agent_id.as_str(), "Billing bot")
        );
        let mut with = facts(Some(&o), 1);
        with.similar = Some(similar.clone());
        let f = agent_frame(&"c".repeat(32), &new, &with).unwrap();
        assert_eq!(f.similar.as_ref(), Some(&similar));
        // A known agent has no hint, even when the facts carry one. The label is portal text, so it is a valid line.
        let f = agent_frame(&"c".repeat(32), &known, &with).unwrap();
        assert!(f.similar.is_none());
        // An unnamed known agent shows as "<key> #<number>", and a long key name is cut as in `label`.
        known.name = None;
        known.name_key = None;
        let unnamed = similar_of(
            &new,
            Some(&o),
            [(&known, Some(o.clone()))],
            "ops-key",
            AgentMode::Discover,
        )
        .unwrap();
        assert!(unnamed.label.starts_with("ops-key #"), "{}", unnamed.label);
        let long = "k".repeat(80);
        let cut = similar_of(
            &new,
            Some(&o),
            [(&known, Some(o.clone()))],
            &long,
            AgentMode::Discover,
        )
        .unwrap();
        assert!(cut.label.chars().count() <= 64);
    }

    #[test]
    fn the_agent_frame_is_portal_text_and_the_hints_of_the_row() {
        let l = approval();
        let mut row = agent_row(&l.item);
        row.name = None;
        row.name_key = None;
        row.state = AgentState::New;
        row.rev = 3;
        let o = observed_all();
        let f = agent_frame(&"c".repeat(32), &row, &facts(Some(&o), 1)).unwrap();
        assert_eq!(
            (f.rev, f.kind, f.state),
            (3, WireKind::Agent, WireState::New)
        );
        assert_eq!(
            (f.label.as_str(), f.key_name.as_str(), f.named),
            ("ops-key #4", "ops-key", false)
        );
        assert!(f.lock_accept, "a new agent with lock_new");
        assert_eq!((f.open, f.merged_into.clone()), (1, None));
        assert_eq!(
            (f.created_at_ms, f.last_seen_ms),
            (1_787_000_000_000, 1_788_000_000_000)
        );
        let w = f.observed.as_ref().unwrap();
        assert_eq!(w.says.as_deref(), Some("billing-bot"));
        // The first text of the agent is for the page of the owner: it is not on the wire.
        assert_eq!((w.says_changed, w.nets_seen), (Some(2), 2));
        assert_eq!(w.net.as_deref(), Some("203.0.113.0/24"));
        assert!(f.similar.is_none());
        inbox_core::proto::ServerMsg::Agent(Box::new(f))
            .encode()
            .unwrap();
        // `lock_new` false: the owner can approve a new agent. The flag is false.
        let mut open = facts(Some(&o), 0);
        open.lock_new = false;
        assert!(
            !agent_frame(&"c".repeat(32), &row, &open)
                .unwrap()
                .lock_accept
        );
        // Known: the flag is false whatever the file says.
        row.state = AgentState::Known;
        row.name = Some("Billing bot".into());
        let f = agent_frame(&"c".repeat(32), &row, &facts(Some(&o), 0)).unwrap();
        assert_eq!(
            (f.label.as_str(), f.named, f.lock_accept),
            ("Billing bot", true, false)
        );
        // Merged: the target is on the frame. Blocked: nothing else.
        row.state = AgentState::Merged;
        row.merged_into = Some("e".repeat(32));
        let f = agent_frame(&"c".repeat(32), &row, &facts(None, 0)).unwrap();
        assert_eq!(
            (f.state, f.merged_into.as_deref()),
            (WireState::Merged, Some("e".repeat(32).as_str()))
        );
        row.state = AgentState::Blocked;
        let f = agent_frame(&"c".repeat(32), &row, &facts(None, 0)).unwrap();
        assert_eq!((f.state, f.merged_into), (WireState::Blocked, None));
    }

    #[test]
    fn the_hints_that_the_frame_would_refuse_are_left_out_and_never_stop_the_frame() {
        let l = approval();
        let mut row = agent_row(&l.item);
        row.state = AgentState::New;
        let mut o = observed_all();
        o.client = Some("a\u{202e}b".into());
        o.user_agent = Some("x\ny".into());
        o.net = Some("n".repeat(44));
        let f = agent_frame(&"c".repeat(32), &row, &facts(Some(&o), 1)).unwrap();
        let w = f.observed.unwrap();
        assert_eq!((w.client, w.user_agent, w.net), (None, None, None));
        assert_eq!((w.says.as_deref(), w.nets_seen), (Some("billing-bot"), 2));
        // The text of the agent with a hidden character: left out, and the count of changes goes with it.
        o.says = Some("a\u{200b}b".into());
        let w = agent_frame(&"c".repeat(32), &row, &facts(Some(&o), 1))
            .unwrap()
            .observed
            .unwrap();
        assert_eq!((w.says, w.says_changed), (None, None));
        // Nothing to show: no member.
        let empty = Observed::default();
        assert!(agent_frame(&"c".repeat(32), &row, &facts(Some(&empty), 0))
            .unwrap()
            .observed
            .is_none());
        assert!(agent_frame(&"c".repeat(32), &row, &facts(None, 0))
            .unwrap()
            .observed
            .is_none());
        // A count of changes of zero is no member. A count with no text is no member.
        let mut o = observed_all();
        o.says_changes = 0;
        assert_eq!(
            agent_frame(&"c".repeat(32), &row, &facts(Some(&o), 0))
                .unwrap()
                .observed
                .unwrap()
                .says_changed,
            None
        );
    }

    #[test]
    fn the_bucket_frame_has_no_name_no_hints_and_no_lock() {
        let l = approval();
        let mut bucket = agent_row(&l.item);
        bucket.kind = AgentKind::Bucket;
        bucket.name = None;
        bucket.name_key = None;
        bucket.token_digest = None;
        bucket.state = AgentState::Known;
        // Even a bucket row that holds hints (it never does) shows none.
        let o = observed_all();
        let f = agent_frame(&"c".repeat(32), &bucket, &facts(Some(&o), 2)).unwrap();
        assert_eq!(
            (f.kind, f.label.as_str()),
            (WireKind::Bucket, "Unattributed · ops-key")
        );
        assert!(f.observed.is_none() && !f.lock_accept && !f.named);
        let mut key_mode = facts(None, 0);
        key_mode.mode = AgentMode::Key;
        assert_eq!(
            agent_frame(&"c".repeat(32), &bucket, &key_mode)
                .unwrap()
                .label,
            "ops-key"
        );
        // A count above the largest u32 stops there. A negative count is zero.
        assert_eq!(
            agent_frame(&"c".repeat(32), &bucket, &facts(None, i64::MAX))
                .unwrap()
                .open,
            u32::MAX
        );
        assert_eq!(
            agent_frame(&"c".repeat(32), &bucket, &facts(None, -3))
                .unwrap()
                .open,
            0
        );
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
