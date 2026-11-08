//! Wire messages of protocol 5. All are JSON; binary fields are base64.
//!
//! Enrollment (`/v1/enroll`) and sessions (`/v1/session`) run over WebSocket binary
//! frames. Each frame is one Noise message.
//!
//! The parsers are strict. Each frame refuses an unknown field (`deny_unknown_fields`) and a
//! duplicate field. Use [`ClientMsg::decode`] and [`ServerMsg::decode`]: they add the checks of
//! size, text and cross-field rules that serde cannot do. Use [`ClientMsg::encode`] and
//! [`ServerMsg::encode`] to write a frame: they check the same rules and the size of one Noise
//! message ([`MAX_PLAINTEXT`]).

use serde::{Deserialize, Serialize};

use crate::b64::serde_bytes;
use crate::effect::{Op, View, is_effect};
use crate::error::{Error, Result};
use crate::noise::MAX_PLAINTEXT;

/// Maximum length of a title, in characters. A title is one line.
pub const MAX_TITLE_CHARS: usize = 128;
/// Maximum length of the explanation of an agent, in characters.
pub const MAX_EXPLANATION_CHARS: usize = 8000;
/// Maximum size of a command, in bytes. For an approval the limit covers `op` and `view` together.
pub const MAX_COMMAND_BYTES: usize = 32 * 1024;
/// Maximum length of the label of a command, in characters.
pub const MAX_COMMAND_LABEL_CHARS: usize = 32;
/// Maximum length of a text of the user (answer, reply), in characters.
pub const MAX_USER_TEXT_CHARS: usize = 8000;
/// Number of options of a choice question.
pub const MIN_OPTIONS: usize = 2;
pub const MAX_OPTIONS: usize = 8;
/// Maximum length of the label and of the description of an option, in characters.
pub const MAX_OPTION_LABEL_CHARS: usize = 48;
pub const MAX_OPTION_DESCRIPTION_CHARS: usize = 160;
/// Maximum length of the placeholder of a text question, in characters.
pub const MAX_PLACEHOLDER_CHARS: usize = 128;
/// Maximum number of entries of a `sync` frame.
pub const MAX_SYNC_ENTRIES: usize = 500;
/// Maximum number of ids in a `digest` frame.
pub const MAX_DIGEST_IDS: usize = 200;

/// The kind of a message.
///
/// The kind is in the stored row of each delivery. The kind `alert` is also in the frame `alert`.
/// The other kinds map to frames: `question` and `approval` to `request`, `turn` to `message`
/// and `digest` to `digest`. A parser MUST reject any other value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    /// One way: a sender tells people. The device sends only a delivery receipt.
    Alert,
    /// An item that an agent asks and the owner answers.
    Question,
    /// An item that asks the owner to approve one effect.
    Approval,
    /// A text of the agent or of the user in the thread of an item.
    Turn,
    /// The count and the age of the open items. It has no body.
    Digest,
}

impl Kind {
    /// The text of the kind, as the wire and the database hold it.
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Alert => "alert",
            Kind::Question => "question",
            Kind::Approval => "approval",
            Kind::Turn => "turn",
            Kind::Digest => "digest",
        }
    }

    /// The kind that a text names. `None` for each text that is not a known kind.
    pub fn parse(text: &str) -> Option<Kind> {
        match text {
            "alert" => Some(Kind::Alert),
            "question" => Some(Kind::Question),
            "approval" => Some(Kind::Approval),
            "turn" => Some(Kind::Turn),
            "digest" => Some(Kind::Digest),
            _ => None,
        }
    }
}

/// Delivery receipt sent by a device.
///
/// The device sends the receipt once the frame is stored durably, so the server knows when to
/// stop retransmitting. It is automatic: it is not a human answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AckStage {
    /// Stored durably on the device. Stops server retransmission.
    Received,
}

impl AckStage {
    pub fn as_str(self) -> &'static str {
        match self {
            AckStage::Received => "received",
        }
    }
}

// ---------------------------------------------------------------- checks

fn bad<T>(what: &'static str) -> Result<T> {
    Err(Error::Malformed(what))
}

/// One line of 1 to `max` characters with no control character.
fn check_line(text: &str, max: usize, what: &'static str) -> Result<()> {
    let n = text.chars().count();
    if n == 0 || n > max || text.chars().any(char::is_control) {
        return bad(what);
    }
    Ok(())
}

/// Characters that make a text read in another order than it executes.
fn is_bidi(c: char) -> bool {
    matches!(c, '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}')
}

/// A title of an item: one line of 1 to 128 characters, no control character, no bidi control,
/// no line separator. (The title of an alert keeps the older rule: no control character.)
fn check_title(text: &str) -> Result<()> {
    check_line(text, MAX_TITLE_CHARS, "title")?;
    // U+2028 and U+2029 break a line on some screens: a title is one line.
    if text.chars().any(|c| is_bidi(c) || c == '\u{2028}' || c == '\u{2029}') {
        return bad("title");
    }
    Ok(())
}

/// A free text of 1 to `max` characters. Control characters stay: the screen shows them.
fn check_text(text: &str, max: usize, what: &'static str) -> Result<()> {
    let n = text.chars().count();
    if n == 0 || n > max {
        return bad(what);
    }
    Ok(())
}

/// 32 lowercase hexadecimal characters: an id of an item, of a thread, of a delivery or of a device.
pub fn is_id(text: &str) -> bool {
    text.len() == 32 && text.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn check_id(text: &str, what: &'static str) -> Result<()> {
    if is_id(text) { Ok(()) } else { bad(what) }
}

fn check_opt_id(text: &Option<String>, what: &'static str) -> Result<()> {
    match text {
        Some(t) => check_id(t, what),
        None => Ok(()),
    }
}

fn check_time(ms: i64, what: &'static str) -> Result<()> {
    if ms < 0 { bad(what) } else { Ok(()) }
}

/// 1 to 64 characters of `A-Z a-z 0-9 _ -`. The device makes it, one for each answer or reply.
fn check_client_msg_id(text: &str) -> Result<()> {
    let ok = (1..=64).contains(&text.len())
        && text.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-');
    if ok { Ok(()) } else { bad("client_msg_id") }
}

/// A token of the vocabulary of the server: 1 to 32 characters of `a-z 0-9 _`.
fn check_token(text: &str, what: &'static str) -> Result<()> {
    let ok = (1..=32).contains(&text.len())
        && text.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_');
    if ok { Ok(()) } else { bad(what) }
}

fn is_false(v: &bool) -> bool {
    !*v
}

// ---------------------------------------------------------------- enrollment

/// Payload of enrollment Noise message 1 (device to server).
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EnrollHello {
    pub protocol: u32,
    /// The one-time secret of the enrollment code (16 bytes).
    #[serde(with = "serde_bytes")]
    pub secret: Vec<u8>,
    /// 1..64 bytes, no control characters.
    pub device_name: String,
    pub app_version: String,
}

impl std::fmt::Debug for EnrollHello {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EnrollHello")
            .field("protocol", &self.protocol)
            .field("secret", &"<hidden>")
            .field("device_name", &self.device_name)
            .field("app_version", &self.app_version)
            .finish()
    }
}

/// Payload of enrollment Noise message 2 (server to device).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EnrollAccept {
    pub protocol: u32,
    /// 32 lowercase hexadecimal characters.
    pub device_id: String,
    pub server_name: String,
    /// The login of the account. It is the name of the device owner.
    pub user_name: String,
}

// ---------------------------------------------------------------- sessions

/// Payload of Noise message 1.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionHello {
    pub protocol: u32,
    pub device_id: String,
    pub app_version: String,
}

/// Payload of Noise message 2.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionAccept {
    pub protocol: u32,
    pub server_name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GroupInfo {
    pub id: String,
    pub name: String,
    pub description: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AlertMsg {
    /// Globally unique (128 random bits, hex). The device deduplicates on it.
    /// The receipt `ack` of this frame names it in its field `id`.
    pub alert_id: String,
    /// The kind of the message. Only `alert` is valid in this frame.
    pub kind: Kind,
    pub group: String,
    pub group_name: String,
    /// One line, at most [`MAX_TITLE_CHARS`] characters, no control character.
    pub title: String,
    /// Unix milliseconds at which the server accepted the alert.
    pub created_at_ms: i64,
}

// ---------------------------------------------------------------- items

/// The state of an item. `pending` and `replied` are open. The others are final, except
/// `abandoned` during `lease.revive_window`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ItemState {
    Pending,
    Replied,
    Answered,
    Abandoned,
    Expired,
    Cancelled,
    Superseded,
}

impl ItemState {
    pub fn is_final(self) -> bool {
        !matches!(self, ItemState::Pending | ItemState::Replied)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Urgency {
    Low,
    Normal,
    High,
}

/// How the agent keeps its item alive. `lease`: it renews at its own pace (`lease_until_ms`).
/// `until`: it states a date (`until_at_ms`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LeaseMode {
    Lease,
    Until,
}

/// What the key of the agent can do. `one_shot`: no text, no thread.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Interaction {
    Conversational,
    OneShot,
}

/// The channel of the key of the agent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Via {
    Mcp,
    Api,
}

/// Who asks. The portal computes each value. None of it is text of the agent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Sender {
    /// Name to show, 1 to 64 characters, one line. The portal computes it.
    pub label: String,
    /// Name of the key, 1 to 64 characters, one line.
    pub key_name: String,
    /// Login of the owner, 1 to 64 characters, one line.
    pub owner: String,
    pub via: Via,
    pub interaction: Interaction,
}

/// A command: the exact text to run or to confirm. It is plain text, shown in a fixed font.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Command {
    /// 1 to 32768 bytes.
    pub text: String,
    /// Name of the command ("SQL"), 1 to 32 characters, one line.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InputType {
    /// One or several options. The answer has `choice`.
    Choice,
    /// Yes or no. The answer has `choice` `["yes"]` or `["no"]`. No options.
    Confirm,
    /// A free text. The answer has `text`.
    Text,
    /// A message with no answer. The owner can only decline.
    None,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InputOption {
    /// 1 to 32 characters of `a-z 0-9 _ -`.
    pub id: String,
    /// 1 to 48 characters, one line.
    pub label: String,
    /// 1 to 160 characters, one line.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// The input of a question. For a `choice`: 2 to 8 options, `1 <= min <= max <= options`, and
/// `min = max = 1` when `multiple` is false. For the other types: no option, `multiple` false,
/// `min = max = 0`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Input {
    #[serde(rename = "type")]
    pub kind: InputType,
    pub multiple: bool,
    pub min: u8,
    pub max: u8,
    pub options: Vec<InputOption>,
    /// Hint in the field of a `text` question, 1 to 128 characters, one line.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub placeholder: Option<String>,
}

/// A suggestion of the agent or of a system. The screen shows it as not verified.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Hint {
    /// The id of an option, or a short text. 1 to 32 characters, one line.
    pub suggested: String,
    /// From 0.0 to 1.0.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confidence: Option<f64>,
    /// Name of the source, 1 to 32 characters, one line.
    pub source: String,
}

/// An earlier approval of the same effect. The portal knows it from the hash, not from the text.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Previous {
    pub approved_at_ms: i64,
    /// True when the service already ran it.
    pub consumed: bool,
}

/// The item follows an earlier item that the user declined with a text.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Follows {
    /// The code of the earlier item.
    pub code: String,
    pub intent: Intent,
}

/// The kind of the text that comes with a decline of an approval.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Intent {
    /// A reason.
    Comment,
    /// A change that the user wants. The user never edits the command.
    ChangeRequest,
}

/// The part of a `request` that is specific to an approval.
///
/// The command is the value that `view.body` points to in `op`. It is not in the frame twice.
/// The hash `effect` covers `service`, `action`, `resource` and `op`. The explanation is not
/// part of it. The phone computes the hash again and shows "Invalid request" if it differs.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ApprovalBody {
    /// Short code of the request: 4 to 8 characters of `A-Z 0-9`.
    pub code: String,
    /// Id of the service, 1 to 64 characters, one line.
    pub service: String,
    /// Name of the action, 1 to 128 characters, one line.
    pub action: String,
    /// Path of the resource, 0 to 512 characters, no control character. Empty: the service.
    pub resource: String,
    /// Who or what the action is for, 1 to 128 characters, one line.
    pub subject: String,
    /// `sha256:` and 64 lowercase hexadecimal digits.
    pub effect: String,
    /// The operation. See `effect.rs` for the rules of the values.
    pub op: Op,
    pub view: View,
    /// Text of the agent, not verified, 1 to 8000 characters.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub explanation: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hint: Option<Hint>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub previous: Option<Previous>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub follows: Option<Follows>,
}

impl ApprovalBody {
    /// The hash that `op`, `service`, `action` and `resource` give.
    pub fn computed_effect(&self) -> std::result::Result<String, crate::effect::EffectError> {
        crate::effect::effect_hash(&self.service, &self.action, &self.resource, self.op.as_value())
    }

    /// True when `effect` is the hash of the content and every pointer of the view names a value.
    /// A phone shows "Invalid request" when it is false.
    pub fn verify(&self) -> bool {
        self.computed_effect().is_ok_and(|e| e == self.effect)
            && crate::effect::check_view(self.op.as_value(), &self.view).is_ok()
    }

    fn validate(&self) -> Result<()> {
        let ok_code = (4..=8).contains(&self.code.len())
            && self.code.bytes().all(|b| b.is_ascii_uppercase() || b.is_ascii_digit());
        if !ok_code {
            return bad("approval.code");
        }
        check_line(&self.service, 64, "approval.service")?;
        check_line(&self.action, 128, "approval.action")?;
        if self.resource.chars().count() > 512 || self.resource.chars().any(char::is_control) {
            return bad("approval.resource");
        }
        check_line(&self.subject, 128, "approval.subject")?;
        if !is_effect(&self.effect) {
            return bad("approval.effect");
        }
        if self.view.check_shape().is_err() {
            return bad("approval.view");
        }
        let size = serde_json::to_vec(&self.op).map(|v| v.len()).unwrap_or(usize::MAX)
            + serde_json::to_vec(&self.view).map(|v| v.len()).unwrap_or(usize::MAX);
        if size > MAX_COMMAND_BYTES {
            return bad("approval.op");
        }
        if let Some(e) = &self.explanation {
            check_text(e, MAX_EXPLANATION_CHARS, "approval.explanation")?;
        }
        if let Some(h) = &self.hint {
            h.validate()?;
        }
        if let Some(p) = &self.previous {
            check_time(p.approved_at_ms, "approval.previous")?;
        }
        if let Some(f) = &self.follows
            && !((4..=8).contains(&f.code.len())
                && f.code.bytes().all(|b| b.is_ascii_uppercase() || b.is_ascii_digit()))
        {
            return bad("approval.follows");
        }
        Ok(())
    }
}

impl Hint {
    fn validate(&self) -> Result<()> {
        check_line(&self.suggested, 32, "hint.suggested")?;
        check_line(&self.source, 32, "hint.source")?;
        if let Some(c) = self.confidence
            && !(0.0..=1.0).contains(&c)
        {
            return bad("hint.confidence");
        }
        Ok(())
    }
}

impl Input {
    fn validate(&self) -> Result<()> {
        match self.kind {
            InputType::Choice => {
                let n = self.options.len();
                if !(MIN_OPTIONS..=MAX_OPTIONS).contains(&n) {
                    return bad("input.options");
                }
                let (min, max) = (self.min as usize, self.max as usize);
                if min < 1 || min > max || max > n {
                    return bad("input.min");
                }
                if !self.multiple && (min != 1 || max != 1) {
                    return bad("input.min");
                }
                let mut seen = std::collections::HashSet::new();
                for o in &self.options {
                    let ok = (1..=32).contains(&o.id.len())
                        && o.id.bytes().all(|b| {
                            b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-'
                        });
                    if !ok || !seen.insert(o.id.as_str()) {
                        return bad("input.options.id");
                    }
                    check_line(&o.label, MAX_OPTION_LABEL_CHARS, "input.options.label")?;
                    if let Some(d) = &o.description {
                        check_line(d, MAX_OPTION_DESCRIPTION_CHARS, "input.options.description")?;
                    }
                }
                if self.placeholder.is_some() {
                    return bad("input.placeholder");
                }
            }
            InputType::Confirm | InputType::Text | InputType::None => {
                if !self.options.is_empty() || self.multiple || self.min != 0 || self.max != 0 {
                    return bad("input.options");
                }
                match &self.placeholder {
                    Some(p) if self.kind == InputType::Text => {
                        check_line(p, MAX_PLACEHOLDER_CHARS, "input.placeholder")?
                    }
                    Some(_) => return bad("input.placeholder"),
                    None => {}
                }
            }
        }
        Ok(())
    }
}

/// Server to device: an item that waits for the owner.
///
/// The same `request_id` can come again with a higher `seq`: the frame is the new state of
/// the item (a revival, a new turn). The device keeps one item for each `request_id`.
/// The fields of a question are `title`, `explanation`, `command`, `input`, `hint` and `as_of_ms`.
/// The field of an approval is `approval`. A frame has the fields of its kind only.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RequestMsg {
    /// Id of the delivery (128 random bits, hex). The receipt `ack` names it in its field `id`.
    pub id: String,
    pub request_id: String,
    /// `question` or `approval`.
    pub kind: Kind,
    /// Id of the thread. The first item of a thread has `chain_id` = `request_id`.
    pub chain_id: String,
    /// Number of the last turn of the thread when the server made the frame. 0 for no turn.
    pub seq: u64,
    pub from: Sender,
    pub urgency: Urgency,
    /// True when the item came inside the quiet hours of the account. The server never holds it.
    #[serde(default, skip_serializing_if = "is_false")]
    pub quiet: bool,
    pub created_at_ms: i64,
    pub chain_created_at_ms: i64,
    pub agent_seen_at_ms: i64,
    pub lease_mode: LeaseMode,
    /// With `lease`: the end of the lease. The field is absent with `until`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lease_until_ms: Option<i64>,
    /// With `until`: the date that the agent chose. The field is absent with `lease`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub until_at_ms: Option<i64>,
    /// The owner can add a text to a decline (an approval) or answer with a text (a question).
    pub allow_custom: bool,
    /// The owner can ask the agent a question with `reply`.
    pub allow_ask_back: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supersedes: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub previous_id: Option<String>,
    // Question.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub explanation: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<Command>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input: Option<Input>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hint: Option<Hint>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub as_of_ms: Option<i64>,
    // Approval.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approval: Option<ApprovalBody>,
}

impl RequestMsg {
    fn validate(&self) -> Result<()> {
        check_id(&self.id, "request.id")?;
        check_id(&self.request_id, "request.request_id")?;
        check_id(&self.chain_id, "request.chain_id")?;
        check_opt_id(&self.supersedes, "request.supersedes")?;
        check_opt_id(&self.previous_id, "request.previous_id")?;
        check_line(&self.from.label, 64, "from.label")?;
        check_line(&self.from.key_name, 64, "from.key_name")?;
        check_line(&self.from.owner, 64, "from.owner")?;
        check_time(self.created_at_ms, "request.created_at_ms")?;
        check_time(self.chain_created_at_ms, "request.chain_created_at_ms")?;
        check_time(self.agent_seen_at_ms, "request.agent_seen_at_ms")?;
        match (self.lease_mode, self.lease_until_ms, self.until_at_ms) {
            (LeaseMode::Lease, Some(t), None) | (LeaseMode::Until, None, Some(t)) => {
                check_time(t, "request.lease")?
            }
            _ => return bad("request.lease_mode"),
        }
        if self.from.interaction == Interaction::OneShot
            && (self.allow_custom || self.allow_ask_back)
        {
            return bad("request.allow");
        }
        match self.kind {
            Kind::Question => {
                let (Some(title), Some(input)) = (&self.title, &self.input) else {
                    return bad("request.title");
                };
                if self.approval.is_some() {
                    return bad("request.approval");
                }
                check_title(title)?;
                if let Some(e) = &self.explanation {
                    check_text(e, MAX_EXPLANATION_CHARS, "request.explanation")?;
                }
                if let Some(c) = &self.command {
                    if c.text.is_empty() || c.text.len() > MAX_COMMAND_BYTES {
                        return bad("command.text");
                    }
                    if let Some(l) = &c.label {
                        check_line(l, MAX_COMMAND_LABEL_CHARS, "command.label")?;
                    }
                }
                input.validate()?;
                if input.kind == InputType::Text && self.allow_custom {
                    return bad("request.allow_custom");
                }
                if input.kind == InputType::None && (self.allow_custom || self.allow_ask_back) {
                    return bad("request.allow");
                }
                if let Some(h) = &self.hint {
                    h.validate()?;
                }
                if let Some(t) = self.as_of_ms {
                    check_time(t, "request.as_of_ms")?;
                }
            }
            Kind::Approval => {
                let Some(a) = &self.approval else {
                    return bad("request.approval");
                };
                if self.title.is_some()
                    || self.explanation.is_some()
                    || self.command.is_some()
                    || self.input.is_some()
                    || self.hint.is_some()
                    || self.as_of_ms.is_some()
                {
                    return bad("request.approval");
                }
                a.validate()?;
            }
            _ => return bad("request.kind"),
        }
        Ok(())
    }
}

/// Who wrote a turn.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Author {
    Agent,
    User,
}

/// Server to device: a turn in the thread of an item.
/// An agent turn has `explanation`, `command` or both. A user turn has `text` only.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MessageMsg {
    /// Id of the delivery. The receipt `ack` names it in its field `id`.
    pub id: String,
    pub request_id: String,
    /// Number of the turn in the thread, 1 or more.
    pub seq: u64,
    pub by: Author,
    pub created_at_ms: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub explanation: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<Command>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
}

impl MessageMsg {
    fn validate(&self) -> Result<()> {
        check_id(&self.id, "message.id")?;
        check_id(&self.request_id, "message.request_id")?;
        check_time(self.created_at_ms, "message.created_at_ms")?;
        if self.seq < 1 {
            return bad("message.seq");
        }
        match self.by {
            Author::Agent => {
                if self.text.is_some() || (self.explanation.is_none() && self.command.is_none()) {
                    return bad("message.text");
                }
                if let Some(e) = &self.explanation {
                    check_text(e, MAX_EXPLANATION_CHARS, "message.explanation")?;
                }
                if let Some(c) = &self.command {
                    if c.text.is_empty() || c.text.len() > MAX_COMMAND_BYTES {
                        return bad("command.text");
                    }
                    if let Some(l) = &c.label {
                        check_line(l, MAX_COMMAND_LABEL_CHARS, "command.label")?;
                    }
                }
            }
            Author::User => {
                if self.explanation.is_some() || self.command.is_some() {
                    return bad("message.explanation");
                }
                match &self.text {
                    Some(t) => check_text(t, MAX_USER_TEXT_CHARS, "message.text")?,
                    None => return bad("message.text"),
                }
            }
        }
        Ok(())
    }
}

/// Where an answer came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Channel {
    Phone,
    Web,
}

/// Who settled an item, when a person did.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SettledBy {
    pub via: Channel,
    /// With `phone`: the name of the device, 1 to 64 characters, one line.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device_name: Option<String>,
}

/// The decision of an answered approval. A question has none.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Decision {
    Approve,
    Deny,
}

/// Server to device: an item is final. The device removes it from "to answer" and withdraws its
/// notification. `state` is `answered`, `abandoned`, `expired`, `cancelled` or `superseded`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SettledMsg {
    /// Id of the delivery. The receipt `ack` names it in its field `id`.
    pub id: String,
    pub request_id: String,
    pub state: ItemState,
    /// Why, as a token of 1 to 32 characters of `a-z 0-9 _`. Examples: `answered`, `lease_lapsed`,
    /// `until_reached`, `max_lifetime`, `agent_cancelled`, `key_revoked`, `owner_deleted`.
    /// A device shows a token that it does not know as it is.
    pub reason: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub by: Option<SettledBy>,
    /// With `superseded`: the id of the new item.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub superseded_by: Option<String>,
    /// With `answered` and an approval.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decision: Option<Decision>,
    pub at_ms: i64,
}

impl SettledMsg {
    fn validate(&self) -> Result<()> {
        check_id(&self.id, "settled.id")?;
        check_id(&self.request_id, "settled.request_id")?;
        if !self.state.is_final() {
            return bad("settled.state");
        }
        check_token(&self.reason, "settled.reason")?;
        check_time(self.at_ms, "settled.at_ms")?;
        check_opt_id(&self.superseded_by, "settled.superseded_by")?;
        if self.superseded_by.is_some() && self.state != ItemState::Superseded {
            return bad("settled.superseded_by");
        }
        if self.decision.is_some() && self.state != ItemState::Answered {
            return bad("settled.decision");
        }
        if let Some(b) = &self.by {
            match (&b.device_name, b.via) {
                (Some(n), Channel::Phone) => check_line(n, 64, "settled.by.device_name")?,
                (None, _) => {}
                _ => return bad("settled.by.device_name"),
            }
        }
        Ok(())
    }
}

/// Server to device: the open items of the owner, as numbers. It has no body.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DigestMsg {
    /// Id of the delivery. The receipt `ack` names it in its field `id`.
    pub id: String,
    /// Number of open items, 1 or more.
    pub count: u32,
    pub oldest_created_at_ms: i64,
    /// The oldest first, at most 200. `count` can be higher.
    pub ids: Vec<String>,
}

impl DigestMsg {
    fn validate(&self) -> Result<()> {
        check_id(&self.id, "digest.id")?;
        check_time(self.oldest_created_at_ms, "digest.oldest_created_at_ms")?;
        if self.count < 1 || self.ids.len() > MAX_DIGEST_IDS || self.ids.len() > self.count as usize
        {
            return bad("digest.count");
        }
        self.ids.iter().try_for_each(|i| check_id(i, "digest.ids"))
    }
}

/// The result of an `answer` or a `reply`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AnswerStatus {
    /// The server took the answer. It is the only answer of the item.
    Accepted,
    /// Another answer won before this one. The retransmission of the same `client_msg_id` also gets it.
    AlreadyAnswered,
    /// The item is final in another way.
    Closed,
    Abandoned,
    Superseded,
    Expired,
    Cancelled,
    /// `shown_effect` is not the hash of the item.
    EffectMismatch,
    /// The device was revoked.
    DeviceRevoked,
    /// The item does not allow a text (`allow_custom` is false).
    CustomNotAllowed,
    /// The item does not allow a reply (`allow_ask_back` is false).
    AskBackNotAllowed,
}

/// Server to device: the result of an `answer` or of a `reply`. The device keeps the frame in
/// its outbox and sends it again until it gets this result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AnswerResultMsg {
    pub request_id: String,
    pub client_msg_id: String,
    pub status: AnswerStatus,
    /// The state of the item at the time of the result.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<ItemState>,
}

// ---------------------------------------------------------------- preferences

/// Quiet hours of the account. A request that comes inside the window has `quiet` true.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QuietPrefs {
    /// `HH:MM`, 24 hours.
    pub from: String,
    /// `HH:MM`. It differs from `from`. The window can cross midnight.
    pub to: String,
    /// IANA name of the time zone, 1 to 64 characters of `A-Z a-z 0-9 _ + - /`.
    pub tz: String,
    /// True: an item with the urgency `high` still rings.
    pub high_rings: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DigestMode {
    Off,
    Daily,
    Every,
}

/// The digest of the account. `daily`: `at` and `tz`. `every`: `every_h`. `off`: no other field.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DigestPrefs {
    pub mode: DigestMode,
    /// `HH:MM`, with `daily`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub at: Option<String>,
    /// IANA name of the time zone, with `daily`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tz: Option<String>,
    /// Hours between two digests, 1 to 168, with `every`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub every_h: Option<u16>,
}

/// The preferences of an account. A missing block means: no preference, use the default of the
/// file. `set_prefs` replaces the whole object. `welcome` carries the current one.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Prefs {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quiet: Option<QuietPrefs>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub digest: Option<DigestPrefs>,
}

fn check_hhmm(text: &str, what: &'static str) -> Result<()> {
    let b = text.as_bytes();
    let ok = b.len() == 5
        && b[2] == b':'
        && b[..2].iter().chain(&b[3..]).all(u8::is_ascii_digit)
        && (b[0] - b'0') * 10 + (b[1] - b'0') < 24
        && (b[3] - b'0') * 10 + (b[4] - b'0') < 60;
    if ok { Ok(()) } else { bad(what) }
}

fn check_tz(text: &str, what: &'static str) -> Result<()> {
    let ok = (1..=64).contains(&text.len())
        && text
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'+' | b'-' | b'/'));
    if ok { Ok(()) } else { bad(what) }
}

impl Prefs {
    fn validate(&self) -> Result<()> {
        if let Some(q) = &self.quiet {
            check_hhmm(&q.from, "prefs.quiet.from")?;
            check_hhmm(&q.to, "prefs.quiet.to")?;
            check_tz(&q.tz, "prefs.quiet.tz")?;
            if q.from == q.to {
                return bad("prefs.quiet.to");
            }
        }
        if let Some(d) = &self.digest {
            match (d.mode, &d.at, &d.tz, d.every_h) {
                (DigestMode::Off, None, None, None) => {}
                (DigestMode::Daily, Some(at), Some(tz), None) => {
                    check_hhmm(at, "prefs.digest.at")?;
                    check_tz(tz, "prefs.digest.tz")?;
                }
                (DigestMode::Every, None, None, Some(h)) if (1..=168).contains(&h) => {}
                _ => return bad("prefs.digest"),
            }
        }
        Ok(())
    }
}

// ---------------------------------------------------------------- client frames

/// What the owner does with an item.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Action {
    /// For an approval: approve. For a question: answer.
    Accept,
    /// For an approval: the only refusal. For a question: refuse to answer.
    Decline,
}

/// `answer`: the only answer of an item. There is no signature field: the proof is the
/// authenticated session and the hash `shown_effect`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AnswerMsg {
    pub request_id: String,
    /// Made by the device for this answer, 1 to 64 characters of `A-Z a-z 0-9 _ -`.
    /// The device sends the same value again until it gets `answer_result`.
    pub client_msg_id: String,
    pub action: Action,
    /// With `accept` of an approval: the hash that the device computed from the `op` that it
    /// received. Absent with `decline`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shown_effect: Option<String>,
    /// The chosen option ids, 0 to 8. With `accept` of a question.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub choice: Option<Vec<String>>,
    /// Text of the user, 1 to 8000 characters. Never with an `accept` of an approval.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    /// Only with `decline` of an approval, and only with `text`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub intent: Option<Intent>,
}

impl AnswerMsg {
    fn validate(&self) -> Result<()> {
        check_id(&self.request_id, "answer.request_id")?;
        check_client_msg_id(&self.client_msg_id)?;
        if let Some(e) = &self.shown_effect
            && (self.action != Action::Accept || !is_effect(e))
        {
            return bad("answer.shown_effect");
        }
        if let Some(c) = &self.choice {
            if c.len() > MAX_OPTIONS {
                return bad("answer.choice");
            }
            for id in c {
                let ok = (1..=32).contains(&id.len())
                    && id.bytes().all(|b| {
                        b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-'
                    });
                if !ok {
                    return bad("answer.choice");
                }
            }
        }
        if let Some(t) = &self.text {
            check_text(t, MAX_USER_TEXT_CHARS, "answer.text")?;
            if t.contains('\0') {
                return bad("answer.text");
            }
        }
        if self.intent.is_some() && (self.text.is_none() || self.action != Action::Decline) {
            return bad("answer.intent");
        }
        if self.action == Action::Decline && self.choice.is_some() {
            return bad("answer.choice");
        }
        Ok(())
    }
}

/// `reply`: the owner asks the agent a question. The item stays open and becomes `replied`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReplyMsg {
    pub request_id: String,
    /// As for `answer`.
    pub client_msg_id: String,
    /// 1 to 8000 characters.
    pub text: String,
}

/// An item that the device holds open, with the last turn that it knows.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OpenRef {
    pub request_id: String,
    pub seq: u64,
}

/// Device -> server, inside the Noise session.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "snake_case", deny_unknown_fields)]
pub enum ClientMsg {
    /// First transport frame; proves the device completed the handshake.
    Ready,
    /// Delivery receipt. `id` is the `alert_id` of an `alert` frame, or the `id` of a
    /// `request`, `message`, `settled` or `digest` frame.
    Ack {
        id: String,
        stage: AckStage,
    },
    /// Subscribes to a group. How the device reacts (alarm, ring, notification) is
    /// a local setting the server never knows.
    Subscribe {
        group: String,
    },
    Unsubscribe {
        group: String,
    },
    Ping {
        nonce: u64,
    },
    Answer(AnswerMsg),
    Reply(ReplyMsg),
    /// Asks for the current state of one item. The server sends `request` or `settled`.
    Refresh {
        request_id: String,
    },
    /// The items that the device holds open. The server sends the `request` frames that the
    /// device lacks or knows in an older state, and a `settled` for each one that is final.
    Sync {
        open: Vec<OpenRef>,
    },
    SetPrefs(Prefs),
}

impl ClientMsg {
    /// Reads a frame and applies every rule of the protocol.
    pub fn decode(bytes: &[u8]) -> Result<ClientMsg> {
        let msg: ClientMsg =
            serde_json::from_slice(bytes).map_err(|_| Error::Malformed("frame"))?;
        // Serde does not refuse an unknown field of a frame without fields.
        if matches!(msg, ClientMsg::Ready)
            && !serde_json::from_slice::<serde_json::Map<String, serde_json::Value>>(bytes)
                .is_ok_and(|m| m.len() == 1)
        {
            return bad("ready");
        }
        msg.validate()?;
        Ok(msg)
    }

    /// Writes a frame. The frame must obey the rules and fit in one Noise message.
    pub fn encode(&self) -> Result<Vec<u8>> {
        self.validate()?;
        encode_checked(self)
    }

    pub fn validate(&self) -> Result<()> {
        match self {
            ClientMsg::Ack { id, .. } => check_id(id, "ack.id"),
            ClientMsg::Answer(a) => a.validate(),
            ClientMsg::Reply(r) => {
                check_id(&r.request_id, "reply.request_id")?;
                check_client_msg_id(&r.client_msg_id)?;
                check_text(&r.text, MAX_USER_TEXT_CHARS, "reply.text")?;
                if r.text.contains('\0') {
                    return bad("reply.text");
                }
                Ok(())
            }
            ClientMsg::Refresh { request_id } => check_id(request_id, "refresh.request_id"),
            ClientMsg::Sync { open } => {
                if open.len() > MAX_SYNC_ENTRIES {
                    return bad("sync.open");
                }
                open.iter().try_for_each(|o| check_id(&o.request_id, "sync.open"))
            }
            ClientMsg::SetPrefs(p) => p.validate(),
            ClientMsg::Ready
            | ClientMsg::Subscribe { .. }
            | ClientMsg::Unsubscribe { .. }
            | ClientMsg::Ping { .. } => Ok(()),
        }
    }
}

// ---------------------------------------------------------------- server frames

/// Server -> device, inside the Noise session.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "snake_case", deny_unknown_fields)]
pub enum ServerMsg {
    Welcome {
        server_name: String,
        groups: Vec<GroupInfo>,
        /// Ids of the groups this device is subscribed to.
        subscriptions: Vec<String>,
        heartbeat_interval_ms: u64,
        offline_warning_after_ms: u64,
        server_time_ms: i64,
        /// Login of the account of this device. Read-only.
        user_name: String,
        /// The preferences of the account. An empty object when the account has none.
        prefs: Box<Prefs>,
        /// The age at which the server erases the body of a final item, in milliseconds.
        /// 0: the server has no items. The device keeps its own copy for its own retention.
        body_purge_after_ms: u64,
    },
    Alert(AlertMsg),
    Request(Box<RequestMsg>),
    Message(MessageMsg),
    Settled(SettledMsg),
    Digest(DigestMsg),
    AnswerResult(AnswerResultMsg),
    Subscriptions {
        subscriptions: Vec<String>,
    },
    Pong {
        nonce: u64,
    },
    Error {
        code: String,
        message: String,
    },
}

impl ServerMsg {
    /// Reads a frame and applies every rule of the protocol.
    pub fn decode(bytes: &[u8]) -> Result<ServerMsg> {
        let msg: ServerMsg =
            serde_json::from_slice(bytes).map_err(|_| Error::Malformed("frame"))?;
        msg.validate()?;
        Ok(msg)
    }

    /// Writes a frame. The frame must obey the rules and fit in one Noise message.
    pub fn encode(&self) -> Result<Vec<u8>> {
        self.validate()?;
        encode_checked(self)
    }

    pub fn validate(&self) -> Result<()> {
        match self {
            ServerMsg::Welcome { prefs, .. } => prefs.validate(),
            ServerMsg::Alert(a) => {
                check_id(&a.alert_id, "alert.alert_id")?;
                if a.kind != Kind::Alert {
                    return bad("alert.kind");
                }
                check_line(&a.title, MAX_TITLE_CHARS, "title")?;
                check_time(a.created_at_ms, "alert.created_at_ms")
            }
            ServerMsg::Request(r) => r.validate(),
            ServerMsg::Message(m) => m.validate(),
            ServerMsg::Settled(s) => s.validate(),
            ServerMsg::Digest(d) => d.validate(),
            ServerMsg::AnswerResult(r) => {
                check_id(&r.request_id, "answer_result.request_id")?;
                check_client_msg_id(&r.client_msg_id)
            }
            ServerMsg::Subscriptions { .. } | ServerMsg::Pong { .. } | ServerMsg::Error { .. } => {
                Ok(())
            }
        }
    }
}

fn encode_checked<T: Serialize>(msg: &T) -> Result<Vec<u8>> {
    let bytes = serde_json::to_vec(msg).map_err(|_| Error::Internal("frame encoding"))?;
    if bytes.len() > MAX_PLAINTEXT {
        return bad("frame too large for one noise message");
    }
    Ok(bytes)
}

/// WebSocket close codes used on `/v1/enroll` and `/v1/session`.
pub mod close {
    /// Enrollment: the code is unknown, already used or expired, or the account is revoked.
    pub const ENROLL_REFUSED: u16 = 4410;
    /// The device id is unknown or its key does not match: enroll again.
    pub const UNKNOWN_DEVICE: u16 = 4401;
    /// The device was revoked from the admin front: enroll again.
    pub const REVOKED: u16 = 4403;
    /// Incompatible protocol version: update the app.
    pub const PROTOCOL: u16 = 4426;
    /// Another session of the same device took over.
    pub const REPLACED: u16 = 4409;
    /// Too many failed attempts from this address.
    pub const RATE_LIMITED: u16 = 4429;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_shapes_are_stable() {
        let m = ClientMsg::Ack { id: "ab".into(), stage: AckStage::Received };
        assert_eq!(
            serde_json::to_string(&m).unwrap(),
            r#"{"t":"ack","id":"ab","stage":"received"}"#
        );
        let a = ServerMsg::Alert(AlertMsg {
            alert_id: "ab".into(),
            kind: Kind::Alert,
            group: "g".into(),
            group_name: "G".into(),
            title: "disk full".into(),
            created_at_ms: 1,
        });
        let s = serde_json::to_string(&a).unwrap();
        assert!(s.starts_with(r#"{"t":"alert","alert_id":"ab","kind":"alert","group":"g""#), "{s}");
        let back: ServerMsg = serde_json::from_str(&s).unwrap();
        assert!(matches!(back, ServerMsg::Alert(ref x) if x.title == "disk full"));
        assert!(!s.contains("action"), "the server never knows alert levels");
        assert!(
            serde_json::from_str::<ClientMsg>(r#"{"t":"ack","id":"a","stage":"received","x":1}"#)
                .is_err()
        );
    }

    #[test]
    fn the_kind_of_an_alert_is_alert_and_nothing_else() {
        let frame = |kind: &str| {
            format!(
                r#"{{"t":"alert","alert_id":"{}","kind":{kind},"group":"g","group_name":"G","title":"x","created_at_ms":1}}"#,
                "ab".repeat(16)
            )
        };
        assert!(ServerMsg::decode(frame(r#""alert""#).as_bytes()).is_ok());
        for bad in [r#""question""#, r#""approval""#, r#""turn""#, r#""digest""#] {
            // The kind is known, but the frame `alert` carries only the kind `alert`.
            assert!(ServerMsg::decode(frame(bad).as_bytes()).is_err(), "{bad}");
        }
        for bad in [r#""Alert""#, r#""""#, "null", "1", r#""settled""#] {
            assert!(serde_json::from_str::<ServerMsg>(&frame(bad)).is_err(), "{bad}");
        }
        // The field is mandatory.
        let none = r#"{"t":"alert","alert_id":"ab","group":"g","group_name":"G","title":"x","created_at_ms":1}"#;
        assert!(serde_json::from_str::<ServerMsg>(none).is_err());
        assert_eq!(Kind::parse("alert"), Some(Kind::Alert));
        for k in [Kind::Alert, Kind::Question, Kind::Approval, Kind::Turn, Kind::Digest] {
            assert_eq!(Kind::parse(k.as_str()), Some(k));
        }
        assert_eq!(Kind::parse("settled"), None);
        assert_eq!(Kind::Alert.as_str(), "alert");
    }

    #[test]
    fn an_alert_to_an_account_is_a_normal_alert_frame() {
        // The server sends the fixed group id `@direct` and the fixed name `Direct`.
        // Protocol 4 does not change: the phone applies its own level for this id.
        let frame = r#"{"t":"alert","alert_id":"ab","kind":"alert","group":"@direct","group_name":"Direct","title":"x","created_at_ms":1}"#;
        match serde_json::from_str::<ServerMsg>(frame).unwrap() {
            ServerMsg::Alert(a) => {
                assert_eq!(a.group, "@direct");
                assert_eq!(a.group_name, "Direct");
                assert_eq!(serde_json::to_string(&ServerMsg::Alert(a)).unwrap(), frame);
            }
            other => panic!("not an alert: {other:?}"),
        }
        // The phone never subscribes to this id on the server: the app sends no frame for it.
        // The frame of a subscription keeps the same shape as for any group.
        let sub = serde_json::to_string(&ClientMsg::Subscribe { group: "ops".into() }).unwrap();
        assert_eq!(sub, r#"{"t":"subscribe","group":"ops"}"#);
    }

    #[test]
    fn user_name_is_read_only() {
        // The device cannot change the login of the account.
        assert!(
            serde_json::from_str::<ClientMsg>(r#"{"t":"set_user_name","user_name":"x"}"#).is_err()
        );
        assert!(serde_json::from_str::<ServerMsg>(r#"{"t":"user_name","user_name":"x"}"#).is_err());
        let w = r#"{"t":"welcome","server_name":"S","groups":[],"subscriptions":[],"heartbeat_interval_ms":1,"offline_warning_after_ms":2,"server_time_ms":3,"user_name":"alice","prefs":{},"body_purge_after_ms":0}"#;
        assert!(matches!(
            serde_json::from_str::<ServerMsg>(w).unwrap(),
            ServerMsg::Welcome { ref user_name, .. } if user_name == "alice"
        ));
    }

    #[test]
    fn close_codes_are_stable() {
        assert_eq!(close::ENROLL_REFUSED, 4410);
        assert_eq!(close::PROTOCOL, 4426);
        assert_eq!(close::RATE_LIMITED, 4429);
    }
}
