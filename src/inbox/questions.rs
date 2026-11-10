//! The questions: an agent asks a question to the owner of its key (lot L13, DIAL 1.3 to 1.6, 3 and 5.1).
//!
//! The tool `ask` of the MCP endpoint and the route `POST /api/v1/questions` call [`ask`]. The functions `supersede`
//! and the `say` of a thread are in this module and in `mcp_tools.rs`. The module holds the rules that are special to
//! a question: the arguments, the matrix of the flags, the rules of the `interaction` of the key, the characters of
//! `command`, the rights and the budget, and the check of an answer. The engine of the items (`items_store.rs`) keeps the
//! states, the lease and the storage, and this module adds no state.
//!
//! # What the module guarantees
//!
//! - The owner of the key is the only recipient. A question has no field `to`.
//! - A question that is not valid never reaches the engine. The refusal is `422` and the portal never changes a value.
//! - A text of the question that holds a live token of an agent of the owner is refused (`agent_token_in_text`).
//! - The budget `questions` takes one unit for each `ask`, `say` and `supersede`. A repeat of a call (the same
//!   `idempotency_key`), a renewal of a lease and a read take none.
//! - The text of the question is sealed at rest by the engine. No log line, no metric and no error of this module holds
//!   a text of the question.
//! - The check of an answer ([`check_answer`]) is one function for the phone and for the web page.

use std::time::Duration;

use inbox_core::form::validate_form_answer;
use inbox_core::proto::{
    check_question_parts, Action, Command, ExplanationFormat, FormQuestion, FormQuestionType, Hint,
    Input, InputOption, InputType, Interaction, ItemState, Kind, Language, Urgency, Via,
};
use serde::Deserialize;
use serde_json::{json, Map, Value};
use zeroize::Zeroizing;

use super::approvals::Requester;
use super::frames::OwnerAnswer;
use super::items::{check_wait, Caller, Loaded, Wait};
use super::items_ops::{NewItem, Renewed, SupersedeOut};
use super::items_store::{Created, ItemError, Items, Limit};
use super::mcp_tools::{self as tools, bind, bound, items_of, view, AgentError};
use super::rights::Decision;
use super::util::parse_rfc3339_ms;
use super::App;
use crate::portal_actions::{QUESTION_ASK, QUESTION_CANCEL};

/// The longest explanation of a question, in characters.
pub const MAX_EXPLANATION_CHARS: usize = 2000;
/// The longest command of a question, in bytes.
pub const MAX_COMMAND_BYTES: usize = 16 * 1024;
/// The longest free text of the owner in an answer, in characters.
pub const MAX_FREE_TEXT_CHARS: usize = 2000;
/// The longest turn of an agent in a thread, in characters.
pub const MAX_TURN_CHARS: usize = 4000;
/// The seconds that a client waits when a limit of open items refuses its question.
const RETRY_WHEN_FULL_S: u64 = 60;

// ---------------------------------------------------------------- the arguments

/// The input of a question, as the agent writes it. `min` and `max` have defaults: 1 and 1 for a single choice, 1 and the
/// number of options for a multiple choice, and 0 and 0 for the other types.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AskInput {
    #[serde(rename = "type")]
    pub kind: InputType,
    #[serde(default)]
    pub multiple: bool,
    #[serde(default)]
    pub min: Option<u8>,
    #[serde(default)]
    pub max: Option<u8>,
    #[serde(default)]
    pub options: Vec<InputOption>,
    /// The owner can answer with a free text. The text ends the question.
    #[serde(default)]
    pub allow_custom: bool,
    /// The owner can ask the agent a question. The question stays open.
    #[serde(default)]
    pub allow_ask_back: bool,
    #[serde(default)]
    pub placeholder: Option<String>,
    /// With the type `form`: 1 to 4 questions.
    #[serde(default)]
    pub questions: Option<Vec<AskFormQuestion>>,
}

/// One question of a form, with the defaults of [`AskInput`].
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AskFormQuestion {
    pub id: String,
    pub header: String,
    pub text: String,
    #[serde(rename = "type")]
    pub kind: FormQuestionType,
    #[serde(default)]
    pub multiple: bool,
    #[serde(default)]
    pub min: Option<u8>,
    #[serde(default)]
    pub max: Option<u8>,
    #[serde(default)]
    pub options: Vec<InputOption>,
    #[serde(default)]
    pub placeholder: Option<String>,
}

/// The hint of an agent or of a decision system. It is a suggestion: the screen shows it as not verified.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AskHint {
    pub suggested: String,
    #[serde(default)]
    pub confidence: Option<f64>,
    pub source: String,
}

/// The arguments of `ask`, as the two doors read them. A member that is not known is an error.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AskArgs {
    pub title: String,
    #[serde(default)]
    pub explanation: Option<String>,
    /// `plain` (the default) or `markdown`. It applies to `explanation` only.
    #[serde(default)]
    pub explanation_format: Option<ExplanationFormat>,
    /// The exact text to run or to confirm. Plain text: no control character except the line end and the tab, no bidi
    /// control.
    #[serde(default)]
    pub command: Option<String>,
    #[serde(default)]
    pub command_label: Option<String>,
    /// A hint for the color of the command. It never changes the text.
    #[serde(default)]
    pub language: Option<Language>,
    pub input: Option<AskInput>,
    #[serde(default)]
    pub hint: Option<AskHint>,
    /// The time of the data that the question is about, in RFC 3339 form.
    #[serde(default)]
    pub as_of: Option<String>,
    #[serde(default)]
    pub urgency: Option<Urgency>,
    #[serde(default)]
    pub lease_s: Option<i64>,
    #[serde(default)]
    pub until: Option<String>,
    /// The seconds that the call waits for an answer. The portal cuts it to its own limit.
    #[serde(default)]
    pub wait_s: Option<u64>,
    #[serde(default)]
    pub idempotency_key: Option<String>,
    #[serde(default)]
    pub previous_id: Option<String>,
    #[serde(default)]
    pub agent_token: Option<String>,
    #[serde(default)]
    pub agent_label: Option<String>,
}

impl std::fmt::Debug for AskArgs {
    /// The debug text holds no text of the question and no token.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AskArgs")
            .field("lease_s", &self.lease_s)
            .field("until", &self.until)
            .field("wait_s", &self.wait_s)
            .field("previous_id", &self.previous_id)
            .finish_non_exhaustive()
    }
}

// ---------------------------------------------------------------- the draft

/// The limits of the file that the checks of a question read.
#[derive(Debug, Clone, Copy)]
pub struct Limits {
    /// `inbox.questions.max_options`.
    pub max_options: usize,
    /// `inbox.items.max_body_bytes`: the size of the body of the item.
    pub max_body_bytes: usize,
}

/// A question that passed every check. The body is the clear JSON object that the engine seals.
pub struct Draft {
    pub body: Zeroizing<Vec<u8>>,
    pub input_type: InputType,
    pub allow_custom: bool,
    pub allow_ask_back: bool,
    pub wait: Wait,
    pub urgency: Urgency,
    pub previous_id: Option<String>,
    pub idempotency_key: String,
    /// Each text of the agent in the question, for the check of the tokens.
    pub texts: Vec<String>,
}

fn refuse(code: &'static str, message: &str) -> AgentError {
    AgentError::new(422, code, message)
}

fn invalid(what: &str) -> AgentError {
    AgentError::new(
        422,
        "invalid_argument",
        format!("{what} is absent, has the wrong type or is out of range"),
    )
}

/// A control character other than the line end and the tab, or a bidirectional control. A command must not show another
/// text than it runs.
fn bad_command_char(c: char) -> bool {
    (c.is_control() && c != '\n' && c != '\t')
        || matches!(c, '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}')
}

/// The matrix of the flags (DIAL 1.4.2) and the rules of the `interaction` of the key (DIAL 1.6).
///
/// - A key `one_shot` sets no flag and asks `choice` or `confirm` only: `interaction_not_allowed`.
/// - `text`: `allow_custom` is refused, because the text is the answer already.
/// - `none`: both flags are refused.
/// - `form`: `allow_custom` needs one choice question, because it adds the row "Other" to the choice questions.
/// - `choice` and `confirm`: both flags are free.
pub fn check_flags(input: &AskInput, interaction: Interaction) -> Result<(), AgentError> {
    let any = input.allow_custom || input.allow_ask_back;
    if interaction == Interaction::OneShot {
        if any {
            return Err(refuse(
                "interaction_not_allowed",
                "the key is not conversational: it cannot set allow_custom or allow_ask_back",
            ));
        }
        if !matches!(input.kind, InputType::Choice | InputType::Confirm) {
            return Err(refuse(
                "interaction_not_allowed",
                "the key is not conversational: it asks a choice or a confirm only",
            ));
        }
    }
    let bad = |message: &str| Err(refuse("invalid_flags", message));
    match input.kind {
        InputType::Text if input.allow_custom => {
            bad("allow_custom is not valid with a text input: the text is the answer")
        }
        InputType::None if any => bad("a message with no answer takes no flag"),
        InputType::Form if input.allow_custom => {
            let has_choice = input
                .questions
                .iter()
                .flatten()
                .any(|q| q.kind == FormQuestionType::Choice);
            if has_choice {
                Ok(())
            } else {
                bad("allow_custom needs a form with one choice question")
            }
        }
        _ => Ok(()),
    }
}

/// The `min` and `max` of a choice with their defaults.
fn bounds(
    kind_is_choice: bool,
    multiple: bool,
    min: Option<u8>,
    max: Option<u8>,
    options: usize,
) -> (u8, u8) {
    if !kind_is_choice {
        return (min.unwrap_or(0), max.unwrap_or(0));
    }
    let count = u8::try_from(options).unwrap_or(u8::MAX);
    if multiple {
        (min.unwrap_or(1), max.unwrap_or(count))
    } else {
        (min.unwrap_or(1), max.unwrap_or(1))
    }
}

/// The input of the frame, with the defaults. The wire rules are checked by `check_question_parts`.
fn wire_input(input: &AskInput) -> Input {
    let choice = input.kind == InputType::Choice;
    let (min, max) = bounds(
        choice,
        input.multiple,
        input.min,
        input.max,
        input.options.len(),
    );
    Input {
        kind: input.kind,
        multiple: input.multiple,
        min,
        max,
        options: input.options.clone(),
        placeholder: input.placeholder.clone(),
        questions: input.questions.as_ref().map(|list| {
            list.iter()
                .map(|q| {
                    let choice = q.kind == FormQuestionType::Choice;
                    let (min, max) = bounds(choice, q.multiple, q.min, q.max, q.options.len());
                    FormQuestion {
                        id: q.id.clone(),
                        header: q.header.clone(),
                        text: q.text.clone(),
                        kind: q.kind,
                        multiple: q.multiple,
                        min,
                        max,
                        options: q.options.clone(),
                        placeholder: q.placeholder.clone(),
                    }
                })
                .collect()
        }),
    }
}

/// Every text of the agent in a question, for the check of the tokens.
fn texts_of(args: &AskArgs, input: &Input) -> Vec<String> {
    let mut texts = vec![args.title.clone()];
    texts.extend(args.explanation.clone());
    texts.extend(args.command.clone());
    texts.extend(args.command_label.clone());
    texts.extend(args.agent_label.clone());
    texts.extend(input.placeholder.clone());
    let options = |list: &[InputOption], texts: &mut Vec<String>| {
        for o in list {
            texts.push(o.label.clone());
            texts.extend(o.description.clone());
        }
    };
    options(&input.options, &mut texts);
    for q in input.questions.iter().flatten() {
        texts.push(q.header.clone());
        texts.push(q.text.clone());
        texts.extend(q.placeholder.clone());
        options(&q.options, &mut texts);
    }
    if let Some(h) = &args.hint {
        texts.push(h.suggested.clone());
        texts.push(h.source.clone());
    }
    texts
}

/// Checks the arguments of a question and builds its body. `now` is the time of the call, in Unix milliseconds. Nothing is
/// changed to make a value fit: a value that is not valid gives `422`.
pub fn build(
    args: &AskArgs,
    limits: &Limits,
    interaction: Interaction,
    now: i64,
    rules: &super::items::Rules,
) -> Result<Draft, AgentError> {
    let Some(input) = &args.input else {
        return Err(invalid("input"));
    };
    check_flags(input, interaction)?;
    if input.kind == InputType::Choice && input.options.len() > limits.max_options {
        return Err(refuse(
            "invalid_argument",
            "input.options has more options than inbox.questions.max_options",
        ));
    }
    for q in input.questions.iter().flatten() {
        if q.options.len() > limits.max_options {
            return Err(refuse(
                "invalid_argument",
                "a question of the form has more options than inbox.questions.max_options",
            ));
        }
    }
    let explanation = args.explanation.as_deref();
    if let Some(text) = explanation {
        if text.chars().count() > MAX_EXPLANATION_CHARS {
            return Err(refuse(
                "explanation_too_long",
                "the explanation is longer than 2000 characters",
            ));
        }
    }
    if args.explanation_format.is_some() && explanation.is_none() {
        return Err(invalid("explanation_format"));
    }
    if args.command.is_none() && (args.command_label.is_some() || args.language.is_some()) {
        return Err(invalid("command"));
    }
    let command = match &args.command {
        None => None,
        Some(text) => {
            if text.len() > MAX_COMMAND_BYTES {
                return Err(refuse(
                    "command_too_large",
                    "the command is longer than 16 KiB",
                ));
            }
            if text.chars().any(bad_command_char) {
                return Err(refuse(
                    "invalid_command_chars",
                    "the command has a control character or a bidirectional control: send plain text with no such character",
                ));
            }
            Some(Command {
                text: text.clone(),
                label: args.command_label.clone(),
                language: args.language,
            })
        }
    };
    let as_of_ms = match &args.as_of {
        None => None,
        Some(text) => Some(parse_rfc3339_ms(text).ok_or_else(|| invalid("as_of"))?),
    };
    let hint = args.hint.as_ref().map(|h| Hint {
        suggested: h.suggested.clone(),
        confidence: h.confidence,
        source: h.source.clone(),
    });
    let wire = wire_input(input);
    check_question_parts(
        &args.title,
        explanation,
        command.as_ref(),
        &wire,
        hint.as_ref(),
        as_of_ms,
    )
    .map_err(|e| {
        AgentError::new(
            422,
            "invalid_argument",
            format!("the question is not valid: {e}"),
        )
    })?;
    let wait = match (args.lease_s, &args.until) {
        (None, None) => return Err(refuse("lease_required", "give lease_s or until")),
        (Some(_), Some(_)) => {
            return Err(refuse(
                "invalid_lease",
                "give one of lease_s and until, inside the limits of the portal",
            ))
        }
        (Some(lease_s), None) => Wait::Lease { lease_s },
        (None, Some(text)) => Wait::Until {
            at_ms: parse_rfc3339_ms(text).ok_or_else(|| {
                refuse(
                    "invalid_lease",
                    "give one of lease_s and until, inside the limits of the portal",
                )
            })?,
        },
    };
    check_wait(wait, now, now, rules).map_err(|_| {
        refuse(
            "invalid_lease",
            "give one of lease_s and until, inside the limits of the portal",
        )
    })?;
    if let Some(key) = &args.idempotency_key {
        if !super::api::valid_idempotency_key(key) {
            return Err(invalid("idempotency_key"));
        }
    }
    if let Some(previous) = &args.previous_id {
        if !super::util::is_hex_id(previous) {
            return Err(invalid("previous_id"));
        }
    }
    let mut body = Map::new();
    body.insert("title".into(), json!(args.title));
    if let Some(text) = explanation {
        body.insert("explanation".into(), json!(text));
        if let Some(format) = args.explanation_format {
            body.insert("explanation_format".into(), json!(format));
        }
    }
    if let Some(c) = &command {
        body.insert(
            "command".into(),
            serde_json::to_value(c).map_err(|_| invalid("command"))?,
        );
    }
    body.insert(
        "input".into(),
        serde_json::to_value(&wire).map_err(|_| invalid("input"))?,
    );
    if let Some(h) = &hint {
        body.insert(
            "hint".into(),
            serde_json::to_value(h).map_err(|_| invalid("hint"))?,
        );
    }
    if let Some(ms) = as_of_ms {
        body.insert("as_of_ms".into(), json!(ms));
    }
    if let Some(label) = &args.agent_label {
        body.insert("agent_label".into(), json!(label));
    }
    let body =
        Zeroizing::new(serde_json::to_vec(&Value::Object(body)).map_err(|_| invalid("question"))?);
    if body.len() > limits.max_body_bytes {
        return Err(refuse(
            "body_too_large",
            "the question is larger than inbox.items.max_body_bytes",
        ));
    }
    Ok(Draft {
        body,
        input_type: input.kind,
        allow_custom: input.allow_custom,
        allow_ask_back: input.allow_ask_back,
        wait,
        urgency: args.urgency.unwrap_or(Urgency::Normal),
        previous_id: args.previous_id.clone(),
        idempotency_key: args
            .idempotency_key
            .clone()
            .unwrap_or_else(super::util::random_id),
        texts: texts_of(args, &wire),
    })
}

// ---------------------------------------------------------------- the check of an answer

/// Why an answer does not fit its question.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnswerFault {
    /// The owner sent a free text, and the question does not allow it (`allow_custom` is false).
    CustomNotAllowed,
    /// The answer does not fit the input: an option that does not exist, a wrong count, a text where a choice is due.
    /// The text names the rule. It holds no value.
    Invalid(&'static str),
}

impl std::fmt::Display for AnswerFault {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AnswerFault::CustomNotAllowed => f.write_str("a free answer is not allowed"),
            AnswerFault::Invalid(rule) => f.write_str(rule),
        }
    }
}

/// The input of a stored question: the member `input` of its sealed body. `None` when the body is not a question.
pub fn input_of(body: &[u8]) -> Option<Input> {
    let value: Value = serde_json::from_slice(body).ok()?;
    serde_json::from_value(value.get("input")?.clone()).ok()
}

/// Checks the answer of the owner against its question. The phone and the web page share this one function.
///
/// - A `decline` has no choice and no text.
/// - An `accept` of `none` is refused: the owner declines a message that has no answer.
/// - `confirm`: the choice is `["yes"]` or `["no"]`.
/// - `choice`: the ticks are known options, with no repeat, and their number is in `min..=max`.
/// - `text`: a text of 1 to 2000 characters.
/// - `form`: the answers go through `validate_form_answer`.
/// - With `allow_custom`, a `choice` or a `confirm` takes a free text in place of the ticks. The text is exclusive of
///   the ticks.
///
/// A free text has at most 2000 characters and no NUL.
pub fn check_answer(
    input: &Input,
    allow_custom: bool,
    action: Action,
    answer: &OwnerAnswer,
) -> Result<(), AnswerFault> {
    if action == Action::Decline {
        if answer.is_empty() {
            return Ok(());
        }
        return Err(AnswerFault::Invalid(
            "a decline of a question has no content",
        ));
    }
    if let Some(text) = &answer.text {
        let n = text.chars().count();
        if n == 0 || n > MAX_FREE_TEXT_CHARS || text.contains('\0') {
            return Err(AnswerFault::Invalid("the text has 1 to 2000 characters"));
        }
    }
    if answer.intent.is_some() {
        return Err(AnswerFault::Invalid("a question has no intent"));
    }
    let ticks = answer.choice.as_deref().unwrap_or_default();
    match input.kind {
        InputType::None => Err(AnswerFault::Invalid(
            "a message with no answer takes no accept",
        )),
        InputType::Form => {
            if answer.choice.is_some() || answer.text.is_some() {
                return Err(AnswerFault::Invalid("a form takes answers only"));
            }
            let Some(list) = &answer.answers else {
                return Err(AnswerFault::Invalid("a form takes answers"));
            };
            let questions = input.questions.as_deref().unwrap_or_default();
            validate_form_answer(questions, allow_custom, list).map_err(|e| {
                use inbox_core::form::FormAnswerError as E;
                match e {
                    E::CustomNotAllowed(_) => AnswerFault::CustomNotAllowed,
                    _ => AnswerFault::Invalid("the answers do not fit the form"),
                }
            })
        }
        kind => {
            if answer.answers.is_some() {
                return Err(AnswerFault::Invalid("this question has no form"));
            }
            if kind == InputType::Text {
                if !ticks.is_empty() || answer.text.is_none() {
                    return Err(AnswerFault::Invalid("a text question takes a text"));
                }
                return Ok(());
            }
            // `choice` and `confirm`: the ticks, or a free text in place of them.
            if let Some(_text) = &answer.text {
                if !ticks.is_empty() {
                    return Err(AnswerFault::Invalid("a free answer excludes the ticks"));
                }
                return if allow_custom {
                    Ok(())
                } else {
                    Err(AnswerFault::CustomNotAllowed)
                };
            }
            check_ticks(input, ticks)
        }
    }
}

fn check_ticks(input: &Input, ticks: &[String]) -> Result<(), AnswerFault> {
    let mut seen = std::collections::HashSet::new();
    if !ticks.iter().all(|t| seen.insert(t.as_str())) {
        return Err(AnswerFault::Invalid("an option is ticked two times"));
    }
    if input.kind == InputType::Confirm {
        return match ticks {
            [one] if one == "yes" || one == "no" => Ok(()),
            _ => Err(AnswerFault::Invalid("a confirm takes yes or no")),
        };
    }
    let (min, max) = (input.min as usize, input.max as usize);
    if ticks.len() < min || ticks.len() > max {
        return Err(AnswerFault::Invalid("the number of ticks is out of range"));
    }
    if ticks
        .iter()
        .any(|t| !input.options.iter().any(|o| &o.id == t))
    {
        return Err(AnswerFault::Invalid("an option is not in the question"));
    }
    Ok(())
}

// ---------------------------------------------------------------- rights and budget

/// The decision of the portal for one action of the key on the service `portal`. With `count` the decision takes one
/// unit of the budget `questions` of the key and of its owner. A refusal is an error with the status of the table of the
/// channel: no message holds a value of the request.
pub fn allow(app: &App, who: &Requester, action: &str, count: bool) -> Result<(), AgentError> {
    let Some(authorizer) = app.authorizer() else {
        return Err(AgentError::new(
            503,
            "directory_not_ready",
            "the portal cannot check keys yet, retry in a moment",
        ));
    };
    match authorizer.decide_service(&who.key_id, action, count) {
        Decision::Allowed => Ok(()),
        Decision::Refused {
            reason: "key_not_valid",
        } => Err(AgentError::new(
            401,
            "unauthorized",
            "missing or invalid bearer token",
        )),
        Decision::Refused { .. } => Err(AgentError::new(
            403,
            "forbidden",
            format!("the key has no permission {action:?} on the service portal"),
        )),
        Decision::Limited { retry_after } => {
            let mut e = AgentError::new(
                429,
                "rate_limited",
                "the budget of questions of this key is used, retry later",
            );
            e.retry_after = Some(retry_after);
            Err(e)
        }
        Decision::NotReady => Err(AgentError::new(
            503,
            "directory_not_ready",
            "the portal cannot check keys yet, retry in a moment",
        )),
        Decision::Unavailable => Err(AgentError::new(
            503,
            "rights_unavailable",
            "the portal cannot count the use of this key now, retry in a moment",
        )),
    }
}

/// Whether the key may see the tool `ask`: the block is on, and the key holds `question.ask`. A key whose budget is used
/// still sees the tool. The check takes no unit.
pub fn may_ask(app: &App, key_id: &str) -> bool {
    if app.config.questions.is_none() {
        return false;
    }
    let Some(authorizer) = app.authorizer() else {
        return false;
    };
    matches!(
        authorizer.decide_service(key_id, QUESTION_ASK, false),
        Decision::Allowed | Decision::Limited { .. }
    )
}

// ---------------------------------------------------------------- the tools

fn limits_of(app: &App) -> Result<Limits, AgentError> {
    let (Some(q), Some(i)) = (app.config.questions.as_ref(), app.config.items.as_ref()) else {
        return Err(AgentError::not_found());
    };
    Ok(Limits {
        max_options: q.max_options,
        max_body_bytes: i.max_body_bytes,
    })
}

fn store(e: ItemError) -> AgentError {
    e.into()
}

/// A text of the question that holds a live token of an agent of the owner is refused. The check comes before any write.
async fn check_tokens(items: &Items<'_>, who: &Requester, draft: &Draft) -> Result<(), AgentError> {
    let texts: Vec<&str> = draft.texts.iter().map(String::as_str).collect();
    if items
        .text_holds_live_token(&who.owner, &texts)
        .await
        .map_err(store)?
    {
        return Err(AgentError::token_in_text());
    }
    Ok(())
}

/// The next step for the agent, a short text. It holds no text of the question.
fn next_step(l: &Loaded) -> String {
    match (l.item.lease_s, l.item.until_at) {
        (Some(s), _) => format!(
            "Call get_answer(request_id, wait_s) or keepalive(request_id) at least every {s} s."
        ),
        (None, Some(_)) => {
            "Call get_answer(request_id, wait_s) before the time in until.".to_string()
        }
        _ => "Call get_answer(request_id).".to_string(),
    }
}

/// Adds the fields of a call that made or found a question: the agent, the token (once) and the next step.
fn finish(mut v: Value, l: &Loaded, resolved: Option<&super::agents::Resolved>) -> Value {
    if let Value::Object(map) = &mut v {
        map.insert("chain_id".into(), json!(l.item.chain_id));
        if let Some(lease_s) = l.item.lease_s {
            map.insert("lease_s".into(), json!(lease_s));
        }
        if let Some(r) = resolved {
            map.insert("agent_status".into(), json!(r.status.as_str()));
            // The token shows once: only the call that made the agent has it.
            if let Some(token) = &r.token {
                map.insert("agent_token".into(), json!(token.as_str()));
            }
        }
        if l.item.is_open() {
            map.insert("next".into(), json!(next_step(l)));
        }
    }
    v
}

/// The wait of a call: `wait_s` cut to the limit of the door. A call with no `wait_s` does not wait.
fn wait_of(wait_s: Option<u64>, cap: Duration) -> Option<Duration> {
    wait_s
        .filter(|s| *s > 0)
        .map(|s| Duration::from_secs(s).min(cap))
}

/// The result of a call after the item exists: the view, or the answer when the owner answers inside `wait`.
async fn result_of(
    app: &App,
    items: &Items<'_>,
    who: &Requester,
    l: &Loaded,
    wait: Option<Duration>,
    resolved: Option<&super::agents::Resolved>,
) -> Result<Value, AgentError> {
    let id = l.item.request_id.clone();
    let value = match wait {
        // The agent that waits renews the lease with its read, as `get_answer` does.
        Some(wait) if l.item.is_open() => {
            tools::get_answer(app, who, None, &id, Some(wait), 0).await?
        }
        _ => view(items, l, 0).await?,
    };
    Ok(finish(value, l, resolved))
}

/// `ask`: the agent asks its owner a question. `via` is the door of the call. `cap` is the longest wait of the door. The
/// result holds a flag: true for a new question, false for a repeat of an earlier call.
///
/// The checks run in this order: the arguments, the tokens in the texts, `previous_id`, the repeat of a call, the right
/// and the budget, the agent, and then the creation. A call that the portal refuses before the creation takes no unit
/// of the budget and makes no agent.
pub async fn ask(
    app: &App,
    who: &Requester,
    via: Via,
    args: AskArgs,
    cap: Duration,
) -> Result<(Value, bool), AgentError> {
    let limits = limits_of(app)?;
    let items = items_of(app)?;
    let who = &bind(
        who,
        args.agent_token.as_deref(),
        args.agent_label.as_deref(),
    )?;
    let draft = build(
        &args,
        &limits,
        who.interaction,
        items.clock.now_ms(),
        &items.rules,
    )?;
    check_tokens(&items, who, &draft).await?;
    // A `previous_id` names a closed item of the same key and of the same agent as this call.
    let mut previous_agent = None;
    if let Some(previous) = &draft.previous_id {
        let found = items
            .get(previous, &Caller::Agent(who.key_id.clone()))
            .await
            .map_err(store)?
            .filter(|l| !l.item.is_open())
            .ok_or_else(|| invalid("previous_id"))?;
        let target = items
            .agent_target(&who.key_id, &who.owner, &who.agent)
            .await?;
        if target.as_deref() != Some(found.item.agent_id.as_str()) {
            return Err(AgentError::new(
                403,
                "agent_mismatch",
                "the agent token is not the agent of this item, or previous_id belongs to another agent",
            ));
        }
        previous_agent = Some(found.item.agent_id);
    }
    let wait = wait_of(args.wait_s, cap);
    // A call that repeats an earlier one takes no unit and makes no agent.
    let existing = items
        .by_idempotency(&who.key_id, &draft.idempotency_key)
        .await
        .map_err(store)?;
    let (agent_id, resolved) = match &existing {
        Some(l) => {
            bound(&items, who, &l.item.request_id, true).await?;
            (None, None)
        }
        None => {
            allow(app, who, QUESTION_ASK, true)?;
            // The agent: found by the token, or made now. The last check of the call before the creation.
            let resolved = items
                .resolve_agent(&who.key_id, &who.owner, &who.agent, true)
                .await?
                .ok_or_else(|| {
                    AgentError::new(
                        503,
                        "unavailable",
                        "the portal cannot do this now, retry later",
                    )
                })?;
            if previous_agent.is_some_and(|id| id != resolved.agent_id) {
                return Err(AgentError::new(
                    403,
                    "agent_mismatch",
                    "the agent token is not the agent of this item, or previous_id belongs to another agent",
                ));
            }
            tools::note_agent(&resolved.agent_id);
            (Some(resolved.agent_id.clone()), Some(resolved))
        }
    };
    let new = NewItem {
        kind: Kind::Question,
        requester_key_id: who.key_id.clone(),
        owner: who.owner.clone(),
        via,
        interaction: who.interaction,
        urgency: draft.urgency,
        input_type: Some(draft.input_type),
        wait: draft.wait,
        allow_custom: draft.allow_custom,
        allow_ask_back: draft.allow_ask_back,
        idempotency_key: draft.idempotency_key.clone(),
        previous_id: draft.previous_id.clone(),
        agent_id,
        body_size: 0,
        approval: None,
        // The owner can answer on the web page when no phone is enrolled.
        require_device: false,
        guard: None,
    };
    let created = items.create(new, &draft.body).await.map_err(store)?;
    match created {
        Created::New(l) => Ok((
            result_of(app, &items, who, &l, wait, resolved.as_ref()).await?,
            true,
        )),
        Created::Repeat(l) => {
            // A repeat renews the lease, and it brings back an item that was abandoned.
            let l = match items
                .renew(&l.item.request_id, &who.key_id)
                .await
                .map_err(store)?
            {
                Renewed::NotFound => return Err(AgentError::not_found()),
                Renewed::Open { loaded, .. } | Renewed::Revived(loaded) => *loaded,
                Renewed::Final(loaded) => {
                    if loaded.item.state == ItemState::Abandoned {
                        return Err(tools::closed(&loaded));
                    }
                    *loaded
                }
            };
            Ok((result_of(app, &items, who, &l, wait, None).await?, false))
        }
        Created::Conflict => Err(AgentError::new(
            409,
            "idempotency_conflict",
            "the idempotency_key names another question of this key",
        )),
        Created::BadWait(_) => Err(refuse(
            "invalid_lease",
            "give one of lease_s and until, inside the limits of the portal",
        )),
        Created::NoDevice => Err(AgentError::new(409, "no_device", "the owner has no phone")),
        Created::TooMany(limit) => Err(too_many(limit)),
        Created::BadAgent => Err(AgentError::new(
            422,
            "agent_unknown",
            "the agent token is not known: call ask with no agent_token to get a new one",
        )),
        Created::AgentBlocked => Err(AgentError::new(
            403,
            "agent_blocked",
            "the owner blocked this agent",
        )),
        Created::Refused(_) => Err(AgentError::new(
            503,
            "unavailable",
            "the portal cannot do this now, retry later",
        )),
    }
}

fn too_many(limit: Limit) -> AgentError {
    let which = match limit {
        Limit::PerKey => "this key has the most open items",
        Limit::PerOwner => "the owner has the most open items",
        Limit::Total => "the portal has the most open items",
    };
    let mut e = AgentError::new(429, "rate_limited", format!("{which}: retry later"));
    e.retry_after = Some(RETRY_WHEN_FULL_S);
    e
}

/// `supersede` of a question: the new question replaces the old one in the same thread. The old item is `superseded`, and
/// the new item is `pending`. `old` is the item that the caller found and bound to the agent of the call. It takes one
/// unit of the budget, and a repeat of the call takes none.
pub async fn supersede(
    app: &App,
    who: &Requester,
    old: &Loaded,
    args: AskArgs,
    via: Via,
    cap: Duration,
) -> Result<Value, AgentError> {
    let limits = limits_of(app)?;
    let items = items_of(app)?;
    let draft = build(
        &args,
        &limits,
        who.interaction,
        items.clock.now_ms(),
        &items.rules,
    )?;
    check_tokens(&items, who, &draft).await?;
    let wait = wait_of(args.wait_s, cap);
    let repeat = items
        .by_idempotency(&who.key_id, &draft.idempotency_key)
        .await
        .map_err(store)?;
    if repeat.is_none() {
        if !old.item.is_open() {
            return Err(tools::closed(old));
        }
        allow(app, who, QUESTION_ASK, true)?;
    }
    let new = NewItem {
        kind: Kind::Question,
        requester_key_id: who.key_id.clone(),
        owner: who.owner.clone(),
        via,
        interaction: who.interaction,
        urgency: draft.urgency,
        input_type: Some(draft.input_type),
        wait: draft.wait,
        allow_custom: draft.allow_custom,
        allow_ask_back: draft.allow_ask_back,
        idempotency_key: draft.idempotency_key.clone(),
        previous_id: None,
        // A chain stays inside one agent: the new item has the agent of the old one.
        agent_id: Some(old.item.agent_id.clone()),
        body_size: 0,
        approval: None,
        require_device: false,
        guard: None,
    };
    let out = items
        .supersede(&old.item.request_id, &who.key_id, new, &draft.body)
        .await
        .map_err(store)?;
    match out {
        SupersedeOut::Created { new, .. } => result_of(app, &items, who, &new, wait, None).await,
        SupersedeOut::Existing(l) => result_of(app, &items, who, &l, wait, None).await,
        SupersedeOut::NotFound => Err(AgentError::not_found()),
        SupersedeOut::Closed(l) => Err(tools::closed(&l)),
        SupersedeOut::ChainFull => Err(AgentError::new(
            409,
            "chain_full",
            "the thread has the most items that the portal allows",
        )),
        SupersedeOut::BadWait(_) => Err(refuse(
            "invalid_lease",
            "give one of lease_s and until, inside the limits of the portal",
        )),
        SupersedeOut::AgentMismatch | SupersedeOut::BadAgent => Err(AgentError::new(
            403,
            "agent_mismatch",
            "the agent token is not the agent of this item, or previous_id belongs to another agent",
        )),
        SupersedeOut::Refused(_) => Err(AgentError::new(
            503,
            "unavailable",
            "the portal cannot do this now, retry later",
        )),
    }
}

/// The right to withdraw a question: `question.cancel`. It takes no unit.
pub fn allow_cancel(app: &App, who: &Requester) -> Result<(), AgentError> {
    allow(app, who, QUESTION_CANCEL, false)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rules() -> super::super::items::Rules {
        super::super::items::tests::rules()
    }

    fn limits() -> Limits {
        Limits {
            max_options: 8,
            max_body_bytes: 16384,
        }
    }

    fn args(input: Value) -> AskArgs {
        serde_json::from_value(json!({
            "title": "Purge the staging?",
            "lease_s": 60,
            "input": input,
        }))
        .unwrap()
    }

    fn two() -> Value {
        json!([{"id": "go", "label": "Run"}, {"id": "wait", "label": "Wait"}])
    }

    fn code(r: Result<Draft, AgentError>) -> (u16, &'static str) {
        let e = r.err().expect("must be refused");
        (e.status, e.code)
    }

    fn build_for(a: &AskArgs, interaction: Interaction) -> Result<Draft, AgentError> {
        let r = rules();
        build(a, &limits(), interaction, 1_000_000, &r)
    }

    /// DIAL 1.4.2: the valid combinations of the flags for each type of input. The `choice` and the `confirm` take every
    /// combination, the `text` takes `allow_ask_back` only, and the type `none` takes no flag.
    #[test]
    fn flags_matrix_all_valid_combinations_per_type() {
        let combos = [(false, false), (true, false), (false, true), (true, true)];
        for (custom, back) in combos {
            for (kind, options) in [("choice", two()), ("confirm", json!([]))] {
                let a = args(json!({"type": kind, "options": options,
                    "allow_custom": custom, "allow_ask_back": back}));
                let d = build_for(&a, Interaction::Conversational)
                    .unwrap_or_else(|e| panic!("{kind} {custom} {back}: {}", e.code));
                assert_eq!((d.allow_custom, d.allow_ask_back), (custom, back));
            }
            let text =
                args(json!({"type": "text", "allow_custom": custom, "allow_ask_back": back}));
            let r = build_for(&text, Interaction::Conversational);
            if custom {
                assert_eq!(code(r), (422, "invalid_flags"), "text {custom} {back}");
            } else {
                assert_eq!(r.unwrap().allow_ask_back, back);
            }
            let none =
                args(json!({"type": "none", "allow_custom": custom, "allow_ask_back": back}));
            let r = build_for(&none, Interaction::Conversational);
            if custom || back {
                assert_eq!(code(r), (422, "invalid_flags"), "none {custom} {back}");
            } else {
                assert!(r.is_ok());
            }
        }
        // A form takes `allow_ask_back` always, and `allow_custom` when it has a choice question.
        let form = |kind: &str, custom: bool| {
            args(
                json!({"type": "form", "allow_custom": custom, "allow_ask_back": true,
                "questions": [{"id": "q1", "header": "Q", "text": "Which?", "type": kind,
                    "options": if kind == "choice" { two() } else { json!([]) }}]}),
            )
        };
        assert!(build_for(&form("choice", true), Interaction::Conversational).is_ok());
        assert!(build_for(&form("text", false), Interaction::Conversational).is_ok());
        assert_eq!(
            code(build_for(&form("text", true), Interaction::Conversational)),
            (422, "invalid_flags")
        );
    }

    #[test]
    fn invalid_flag_combinations_422() {
        // The refusal gives the code `invalid_flags`, never a silent correction.
        for input in [
            json!({"type": "text", "allow_custom": true}),
            json!({"type": "text", "allow_custom": true, "allow_ask_back": true}),
            json!({"type": "none", "allow_custom": true}),
            json!({"type": "none", "allow_ask_back": true}),
        ] {
            let e = build_for(&args(input.clone()), Interaction::Conversational)
                .err()
                .unwrap_or_else(|| panic!("{input}"));
            assert_eq!((e.status, e.code), (422, "invalid_flags"), "{input}");
            assert!(!e.retryable());
        }
        // A flag of a wrong type is an invalid argument, not a coercion.
        let r: Result<AskArgs, _> = serde_json::from_value(json!({
            "title": "t", "lease_s": 60,
            "input": {"type": "choice", "options": two(), "allow_custom": "yes"}}));
        assert!(r.is_err());
    }

    #[test]
    fn one_shot_with_any_flag_interaction_not_allowed() {
        for (custom, back) in [(true, false), (false, true), (true, true)] {
            for (kind, options) in [
                ("choice", two()),
                ("confirm", json!([])),
                ("text", json!([])),
            ] {
                let a = args(json!({"type": kind, "options": options,
                    "allow_custom": custom, "allow_ask_back": back}));
                assert_eq!(
                    code(build_for(&a, Interaction::OneShot)),
                    (422, "interaction_not_allowed"),
                    "{kind} {custom} {back}"
                );
            }
        }
        // A closed choice or confirm is valid for a one_shot key. A text, a none and a form are not.
        for kind in ["choice", "confirm"] {
            let a = args(
                json!({"type": kind, "options": if kind == "choice" { two() } else { json!([]) }}),
            );
            assert!(build_for(&a, Interaction::OneShot).is_ok(), "{kind}");
        }
        for input in [
            json!({"type": "text"}),
            json!({"type": "none"}),
            json!({"type": "form", "questions": [{"id": "q", "header": "Q", "text": "?", "type": "text"}]}),
        ] {
            assert_eq!(
                code(build_for(&args(input), Interaction::OneShot)),
                (422, "interaction_not_allowed")
            );
        }
    }

    #[test]
    fn command_with_bidi_or_control_refused() {
        let with = |command: &str| {
            let mut a = args(json!({"type": "confirm"}));
            a.command = Some(command.to_string());
            build_for(&a, Interaction::Conversational)
        };
        for bad in [
            "rm -rf /\u{202E}",
            "a\u{202A}b",
            "a\u{2066}b",
            "a\u{2069}b",
            "a\u{0}b",
            "a\rb",
            "a\u{7}b",
            "a\u{1b}[31mb",
            "a\u{85}b",
        ] {
            assert_eq!(code(with(bad)), (422, "invalid_command_chars"), "{bad:?}");
        }
        // A line end and a tab stay.
        assert!(with("line one\n\tline two").is_ok());
        // The refusal never holds the command.
        let e = with("x\u{202E}y").err().unwrap();
        assert!(!e.message.contains('x') || !e.message.contains("\u{202E}"));
    }

    #[test]
    fn command_zero_width_accepted_and_flagged_in_frame() {
        // A zero-width character is accepted, and the body keeps it: the screens show it as a visible mark, and the
        // portal never removes it.
        let mut a = args(json!({"type": "confirm"}));
        let command = "ls\u{200B} -l\u{FEFF}";
        a.command = Some(command.to_string());
        let d = build_for(&a, Interaction::Conversational).unwrap();
        let body: Value = serde_json::from_slice(&d.body).unwrap();
        assert_eq!(body["command"]["text"], command);
        // The body gives a valid `request` frame, and the frame holds the same text.
        let q = input_of(&d.body).unwrap();
        assert_eq!(q.kind, InputType::Confirm);
    }

    #[test]
    fn the_limits_refuse_and_never_cut() {
        let long = |n: usize| "x".repeat(n);
        let mut a = args(json!({"type": "confirm"}));
        a.explanation = Some(long(2001));
        assert_eq!(
            code(build_for(&a, Interaction::Conversational)),
            (422, "explanation_too_long")
        );
        a.explanation = Some(long(2000));
        assert!(build_for(&a, Interaction::Conversational).is_ok());
        a.explanation = None;
        a.command = Some(long(16 * 1024 + 1));
        assert_eq!(
            code(build_for(&a, Interaction::Conversational)),
            (422, "command_too_large")
        );
        a.command = None;
        a.title = long(129);
        assert_eq!(
            code(build_for(&a, Interaction::Conversational)),
            (422, "invalid_argument")
        );
        a.title = "two\nlines".into();
        assert_eq!(
            code(build_for(&a, Interaction::Conversational)),
            (422, "invalid_argument")
        );
        // More options than the file allows, and more than the protocol allows.
        let many = |n: usize| {
            Value::Array(
                (0..n)
                    .map(|i| json!({"id": format!("o{i}"), "label": format!("Option {i}")}))
                    .collect(),
            )
        };
        let a = args(json!({"type": "choice", "options": many(9)}));
        assert_eq!(
            code(build_for(&a, Interaction::Conversational)),
            (422, "invalid_argument")
        );
        let a = args(json!({"type": "choice", "options": many(1)}));
        assert_eq!(
            code(build_for(&a, Interaction::Conversational)),
            (422, "invalid_argument")
        );
        let a = args(json!({"type": "choice", "options": many(8)}));
        assert!(build_for(&a, Interaction::Conversational).is_ok());
        let mut small = limits();
        small.max_options = 3;
        let a = args(json!({"type": "choice", "options": many(4)}));
        assert!(build(&a, &small, Interaction::Conversational, 1_000_000, &rules()).is_err());
        // An option id out of its alphabet, and a label out of its length.
        for options in [
            json!([{"id": "A", "label": "x"}, {"id": "b", "label": "y"}]),
            json!([{"id": "a", "label": long(49)}, {"id": "b", "label": "y"}]),
            json!([{"id": "a", "label": "x", "description": long(161)}, {"id": "b", "label": "y"}]),
            json!([{"id": "a", "label": "x"}, {"id": "a", "label": "y"}]),
        ] {
            let a = args(json!({"type": "choice", "options": options}));
            assert_eq!(
                code(build_for(&a, Interaction::Conversational)),
                (422, "invalid_argument")
            );
        }
        // A wait: neither lease_s nor until, and both.
        let mut a = args(json!({"type": "confirm"}));
        a.lease_s = None;
        assert_eq!(
            code(build_for(&a, Interaction::Conversational)),
            (422, "lease_required")
        );
        a.lease_s = Some(60);
        a.until = Some("2099-01-01T00:00:00Z".into());
        assert_eq!(
            code(build_for(&a, Interaction::Conversational)),
            (422, "invalid_lease")
        );
    }

    #[test]
    fn the_defaults_of_the_bounds_and_the_body_form() {
        let a = args(json!({"type": "choice", "options": two()}));
        let d = build_for(&a, Interaction::Conversational).unwrap();
        let input = input_of(&d.body).unwrap();
        assert_eq!((input.min, input.max, input.multiple), (1, 1, false));
        let a = args(json!({"type": "choice", "multiple": true, "options": two()}));
        let d = build_for(&a, Interaction::Conversational).unwrap();
        let input = input_of(&d.body).unwrap();
        assert_eq!((input.min, input.max, input.multiple), (1, 2, true));
        // An explicit value that the wire refuses is refused: the portal never changes it.
        let a = args(
            json!({"type": "choice", "multiple": false, "min": 2, "max": 2, "options": two()}),
        );
        assert_eq!(
            code(build_for(&a, Interaction::Conversational)),
            (422, "invalid_argument")
        );
        // The same call gives the same body, so a repeat is found by the comparison.
        let a1 = args(json!({"type": "confirm"}));
        let a2 = args(json!({"type": "confirm"}));
        assert_eq!(
            build_for(&a1, Interaction::Conversational).unwrap().body,
            build_for(&a2, Interaction::Conversational).unwrap().body
        );
    }

    fn ans(choice: Option<&[&str]>, text: Option<&str>) -> OwnerAnswer {
        OwnerAnswer {
            choice: choice.map(|c| c.iter().map(|s| s.to_string()).collect()),
            text: text.map(str::to_string),
            intent: None,
            answers: None,
        }
    }

    fn input_for(v: Value) -> Input {
        let d = build_for(&args(v), Interaction::Conversational).unwrap();
        input_of(&d.body).unwrap()
    }

    #[test]
    fn the_check_of_an_answer_follows_the_input_and_the_flags() {
        let choice = input_for(json!({"type": "choice", "options": two()}));
        let accept = Action::Accept;
        assert_eq!(
            check_answer(&choice, false, accept, &ans(Some(&["go"]), None)),
            Ok(())
        );
        for bad in [
            ans(Some(&["nope"]), None),
            ans(Some(&["go", "wait"]), None),
            ans(Some(&[]), None),
            ans(None, None),
        ] {
            assert!(matches!(
                check_answer(&choice, false, accept, &bad),
                Err(AnswerFault::Invalid(_))
            ));
        }
        // A free text needs `allow_custom`. With it, the text is exclusive of the ticks.
        let free = ans(None, Some("not before 18h"));
        assert_eq!(
            check_answer(&choice, false, accept, &free),
            Err(AnswerFault::CustomNotAllowed)
        );
        assert_eq!(check_answer(&choice, true, accept, &free), Ok(()));
        let both = ans(Some(&["go"]), Some("x"));
        assert!(matches!(
            check_answer(&choice, true, accept, &both),
            Err(AnswerFault::Invalid(_))
        ));
        // The text has at most 2000 characters.
        assert!(check_answer(&choice, true, accept, &ans(None, Some(&"x".repeat(2000)))).is_ok());
        assert!(check_answer(&choice, true, accept, &ans(None, Some(&"x".repeat(2001)))).is_err());
        // A multiple choice with `min` and `max`.
        let multi = input_for(
            json!({"type": "choice", "multiple": true, "min": 1, "max": 2,
            "options": [{"id": "a", "label": "A"}, {"id": "b", "label": "B"}, {"id": "c", "label": "C"}]}),
        );
        assert!(check_answer(&multi, false, accept, &ans(Some(&["a", "c"]), None)).is_ok());
        assert!(check_answer(&multi, false, accept, &ans(Some(&["a", "a"]), None)).is_err());
        let three = ans(Some(&["a", "b", "c"]), None);
        assert!(check_answer(&multi, false, accept, &three).is_err());
        // A confirm, a text and a message with no answer.
        let confirm = input_for(json!({"type": "confirm"}));
        assert!(check_answer(&confirm, false, accept, &ans(Some(&["yes"]), None)).is_ok());
        assert!(check_answer(&confirm, false, accept, &ans(Some(&["maybe"]), None)).is_err());
        let text = input_for(json!({"type": "text"}));
        assert!(check_answer(&text, false, accept, &ans(None, Some("hello"))).is_ok());
        assert!(check_answer(&text, false, accept, &ans(Some(&["yes"]), None)).is_err());
        let none = input_for(json!({"type": "none"}));
        assert!(check_answer(&none, false, accept, &ans(Some(&["yes"]), None)).is_err());
        // A decline has no content, for each type.
        for input in [&choice, &confirm, &text, &none] {
            assert_eq!(
                check_answer(input, true, Action::Decline, &OwnerAnswer::default()),
                Ok(())
            );
            assert!(check_answer(input, true, Action::Decline, &ans(None, Some("why"))).is_err());
        }
    }

    #[test]
    fn a_form_answer_goes_through_the_form_validator() {
        let form = input_for(json!({"type": "form", "allow_custom": true, "questions": [
            {"id": "target", "header": "Target", "text": "Where?", "type": "choice", "options": two()},
            {"id": "note", "header": "Note", "text": "More?", "type": "text"}]}));
        let given = |a: Value| OwnerAnswer {
            answers: Some(serde_json::from_value(a).unwrap()),
            ..OwnerAnswer::default()
        };
        let good = given(json!([{"question_id": "target", "choice": ["go"]},
            {"question_id": "note", "text": "ok"}]));
        assert_eq!(check_answer(&form, true, Action::Accept, &good), Ok(()));
        let free = given(json!([{"question_id": "target", "text": "other"},
            {"question_id": "note", "text": "ok"}]));
        assert_eq!(check_answer(&form, true, Action::Accept, &free), Ok(()));
        assert_eq!(
            check_answer(&form, false, Action::Accept, &free),
            Err(AnswerFault::CustomNotAllowed)
        );
        let missing = given(json!([{"question_id": "target", "choice": ["go"]}]));
        assert!(check_answer(&form, true, Action::Accept, &missing).is_err());
        assert!(check_answer(&form, true, Action::Accept, &ans(Some(&["go"]), None)).is_err());
    }
}
