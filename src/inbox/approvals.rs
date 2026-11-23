//! The approval API behind the HTTP routes: the submission of a service, the verdict that it reads, and the intent of
//! an agent.
//!
//! The service (the PEP) learns the verdict and, at the submission, the id and the code of the request. The text of an
//! agent (the explanation, the hint, a comment of the owner) never goes to the service: it lives in the sealed body of
//! the item and in the sealed intent. The functions of this module return plain values. The HTTP layer
//! (`http/approval_api.rs`) reads the request and writes the answer; the agent channel calls [`intent`].
//!
//! # The limits that this module checks
//!
//! - The effect that the service sends must be the effect of the operation, the service of the token, and the action
//!   and the resource of the question. The PDP wrote the last two in the sealed part of the `evaluation_id`.
//! - One request is open for each effect (idempotence): a second submission gives the same id and renews nothing.
//! - The creation limits (cap each hour, mute, deny time) run in the transaction of the creation, so two instances
//!   cannot both pass them.
//! - An item belongs to an agent. The intent resolves the agent of the call and keeps its id. A submission
//!   that names the intent gives its item to that agent. A submission with no intent goes to the bucket of the key. A
//!   chain (`previous_id`, `supersedes`) stays inside one agent.

use inbox_core::effect::{check_view, effect_hash, Op, View};
use inbox_core::proto::{ExplanationFormat, Hint, Interaction, ItemState, Kind, Language, Via};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use zeroize::Zeroizing;

use super::agents::{AgentCall, AgentError, Resolved};
use super::items::{approval_key_hash, check_wait, Caller, Loaded, Wait, WaitError};
use super::items_ops::{Guard, NewApproval, NewItem, Refused, Renewed, SupersedeOut};
use super::items_store::{Created, ItemError, Items, NewIntent};
use super::util::{hex, random_id};
use super::App;
use crate::config::ApprovalsConfig;

/// What the service reads of a request (protocol 17.6). Nothing else of the item goes to the service.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verdict {
    Pending,
    Approved,
    Denied,
    /// Not valid any more. The service does not learn why.
    Void,
}

impl Verdict {
    pub fn as_str(self) -> &'static str {
        match self {
            Verdict::Pending => "pending",
            Verdict::Approved => "approved",
            Verdict::Denied => "denied",
            Verdict::Void => "void",
        }
    }
}

/// The verdict of a request: the only state that goes to the service.
pub fn verdict_of(l: &Loaded) -> Verdict {
    match l.item.state {
        ItemState::Pending | ItemState::Replied => Verdict::Pending,
        ItemState::Answered => match &l.approval {
            Some(a) if a.lapsed_at.is_some() => Verdict::Void,
            Some(a) => match a.decision {
                Some(inbox_core::proto::Decision::Approve) => Verdict::Approved,
                Some(inbox_core::proto::Decision::Deny) => Verdict::Denied,
                None => Verdict::Void,
            },
            None => Verdict::Void,
        },
        _ => Verdict::Void,
    }
}

/// An error of the approval API: the HTTP status and the code of protocol 17.4. The body of the answer is
/// `{"error": code}` and, for a status 429, `retry_after` in whole seconds.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ApprovalError {
    pub status: u16,
    pub code: &'static str,
    pub retry_after: Option<u64>,
}

impl ApprovalError {
    pub const fn new(status: u16, code: &'static str) -> Self {
        Self {
            status,
            code,
            retry_after: None,
        }
    }

    /// `429 rate_limited` with the seconds to wait: at least one, rounded up.
    pub fn rate_limited(retry_after_ms: i64) -> Self {
        Self {
            status: 429,
            code: "rate_limited",
            retry_after: Some(((retry_after_ms.max(0) + 999) / 1000).max(1) as u64),
        }
    }

    pub fn to_value(&self) -> Value {
        match self.retry_after {
            Some(s) => json!({"error": self.code, "retry_after": s}),
            None => json!({"error": self.code}),
        }
    }
}

impl From<AgentError> for ApprovalError {
    /// The code and the status of the table of the channel: `agent_unknown`, `agent_blocked`, `agent_token_required`
    /// and `agent_mismatch`.
    fn from(e: AgentError) -> Self {
        match e {
            AgentError::Storage(e) => storage(e),
            other => ApprovalError::new(other.status(), other.code()),
        }
    }
}

const NOT_FOUND: ApprovalError = ApprovalError::new(404, "not_found");
const INVALID_ARGUMENT: ApprovalError = ApprovalError::new(422, "invalid_argument");
const INVALID_LEASE: ApprovalError = ApprovalError::new(422, "invalid_lease");
/// A `previous_id` that belongs to another agent (the table of the channel gives 403).
const AGENT_MISMATCH: ApprovalError = ApprovalError::new(403, "agent_mismatch");
/// A text of the agent (an explanation, a label) that holds a live token of an agent of the same owner.
const AGENT_TOKEN_IN_TEXT: ApprovalError = ApprovalError::new(422, "agent_token_in_text");

fn storage(e: ItemError) -> ApprovalError {
    match e {
        ItemError::Degraded => ApprovalError::new(503, "storage_degraded"),
        other => {
            super::log::error!(event = "approval_api_store_error", error = %other);
            ApprovalError::new(503, "unavailable")
        }
    }
}

fn wait_error(_: WaitError) -> ApprovalError {
    INVALID_LEASE
}

/// The seconds that a repeated request must wait when a limit of open requests refuses it.
const RETRY_WHEN_FULL_MS: i64 = 60_000;

/// The key that asks. The HTTP layer finds it from the subject of the question.
#[derive(Clone, Debug)]
pub struct Requester {
    pub key_id: String,
    pub owner: String,
    pub interaction: Interaction,
    /// What the call tells about its agent: the mode of the key, the token and the hints. The agent channel fills it
    ///. `Default` is a key in the mode `key`: no token, the bucket of the key.
    pub agent: AgentCall,
}

impl Requester {
    pub fn subject(&self) -> String {
        format!("key:{}", self.key_id)
    }
}

/// How the agent states the time that it waits (protocol 17.5).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Control {
    /// The `approval_ref` of an intent. The wait and the flags are in the intent.
    Reference(String),
    LeaseSeconds(i64),
    /// A time, in Unix milliseconds.
    UntilMs(i64),
}

/// A submission, as the HTTP layer read it and the PDP checked the `evaluation_id`.
pub struct Submission {
    /// The service of the token.
    pub service: String,
    pub action: String,
    /// The path of the resource without the service, as the effect keeps it.
    pub resource: String,
    pub evaluation_id: String,
    pub effect: String,
    pub op: Op,
    pub view: Value,
    pub control: Control,
    pub supersedes: Option<String>,
}

/// The answer to a submission.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Submitted {
    pub request_id: String,
    pub code: String,
    /// False for a second submission of an open request.
    pub created: bool,
}

/// Reads a time in RFC 3339 form as Unix milliseconds (the member `until` of a submission).
pub fn parse_time(text: &str) -> Option<i64> {
    super::util::parse_rfc3339_ms(text)
}

/// The digest of an `approval_ref`. The database keeps the digest and never the reference.
pub fn ref_digest(reference: &str) -> String {
    hex(&Sha256::digest(reference.as_bytes()))
}

/// The size of a command: the compact JSON of `op` and `view`.
pub fn command_bytes(op: &Value, view: &Value) -> usize {
    serde_json::to_vec(op).map_or(usize::MAX, |v| v.len())
        + serde_json::to_vec(view).map_or(usize::MAX, |v| v.len())
}

fn config(app: &App) -> Result<(&ApprovalsConfig, Items<'_>), ApprovalError> {
    let (Some(config), Some(items)) = (app.config.approvals.as_ref(), app.items()) else {
        return Err(ApprovalError::new(503, "unavailable"));
    };
    Ok((config, items))
}

fn check_control(wait: Wait, items: &Items<'_>) -> Result<(), ApprovalError> {
    let now = items.clock.now_ms();
    check_wait(wait, now, now, &items.rules).map_err(wait_error)
}

/// Makes a request, or gives the open request of the same effect. The checks run in this order: the size of the
/// command, the effect, the view, the wait, the open request of the same effect, the withdrawal that the request
/// names, the intent, and then the limits of the creation.
pub async fn submit(app: &App, who: &Requester, s: Submission) -> Result<Submitted, ApprovalError> {
    let (config, items) = config(app)?;
    if command_bytes(s.op.as_value(), &s.view) > config.max_command_bytes {
        return Err(ApprovalError::new(413, "command_too_large"));
    }
    let computed = effect_hash(&s.service, &s.action, &s.resource, s.op.as_value())
        .map_err(|_| ApprovalError::new(422, "invalid_op"))?;
    if computed != s.effect {
        return Err(ApprovalError::new(422, "effect_mismatch"));
    }
    let view: View = serde_json::from_value(s.view.clone())
        .map_err(|_| ApprovalError::new(422, "invalid_view"))?;
    check_view(s.op.as_value(), &view).map_err(|_| ApprovalError::new(422, "invalid_view"))?;
    let subject = who.subject();
    let key_hash = approval_key_hash(&s.service, &subject, &s.action, &s.resource, &s.effect);
    match &s.control {
        Control::LeaseSeconds(n) => check_control(Wait::Lease { lease_s: *n }, &items)?,
        Control::UntilMs(at_ms) => check_control(Wait::Until { at_ms: *at_ms }, &items)?,
        Control::Reference(_) => {}
    }
    // A second submission of an open request gives the same id. It renews nothing and replaces nothing. When the second
    // submission comes from another agent, the item keeps its first agent and the event `also_requested` records the
    // other one.
    if let Some(existing) = items.open_approval(&key_hash).await.map_err(storage)? {
        let other = match &s.control {
            Control::Reference(reference) => items
                .read_intent(&ref_digest(reference), &who.key_id)
                .await
                .map_err(storage)?
                .map(|intent| intent.agent_id),
            _ => Some(
                items
                    .bucket(&who.key_id, &who.owner)
                    .await
                    .map_err(storage)?
                    .agent_id,
            ),
        };
        if let Some(other) = other.filter(|id| *id != existing.item.agent_id) {
            items
                .record_also_requested(&existing, &other)
                .await
                .map_err(storage)?;
        }
        return Ok(submitted(&existing, false));
    }
    let digest = match &s.control {
        Control::Reference(reference) => Some(ref_digest(reference)),
        _ => None,
    };
    let (agent_id, wait, allow_custom, allow_ask_back, previous_id, explanation, hint, shape) =
        match &digest {
            Some(digest) => {
                let Some(intent) = items
                    .read_intent(digest, &who.key_id)
                    .await
                    .map_err(storage)?
                else {
                    return Err(INVALID_ARGUMENT);
                };
                let content: Value =
                    serde_json::from_slice(&intent.content).map_err(|_| INVALID_ARGUMENT)?;
                (
                    Some(intent.agent_id),
                    intent.wait,
                    intent.allow_custom,
                    intent.allow_ask_back,
                    intent.previous_id,
                    content.get("explanation").cloned().filter(|v| !v.is_null()),
                    content.get("hint").cloned().filter(|v| !v.is_null()),
                    // How to show the texts: the format of the explanation and the language of the command. The
                    // text that the agent says about itself goes with them: the screens show it as not verified.
                    ["explanation_format", "language", "agent_label"]
                        .into_iter()
                        .filter_map(|k| content.get(k).cloned().map(|v| (k, v)))
                        .collect::<Vec<_>>(),
                )
            }
            // No intent: the request belongs to the bucket of the key. A service never names an agent.
            None => (
                None,
                match s.control {
                    Control::LeaseSeconds(n) => Wait::Lease { lease_s: n },
                    Control::UntilMs(at_ms) => Wait::Until { at_ms },
                    Control::Reference(_) => unreachable!("a reference has a digest"),
                },
                false,
                false,
                None,
                None,
                None,
                Vec::new(),
            ),
        };
    if config.require_explanation && explanation.is_none() {
        return Err(ApprovalError::new(422, "explanation_required"));
    }
    let mut body = json!({"op": s.op.as_value(), "view": view});
    if let Some(text) = explanation {
        body["explanation"] = text;
    }
    if let Some(hint) = hint {
        body["hint"] = hint;
    }
    for (key, value) in shape {
        body[key] = value;
    }
    let body = Zeroizing::new(serde_json::to_vec(&body).map_err(|_| INVALID_ARGUMENT)?);
    let new = NewItem {
        kind: Kind::Approval,
        requester_key_id: who.key_id.clone(),
        owner: who.owner.clone(),
        via: Via::Mcp,
        interaction: who.interaction,
        urgency: inbox_core::proto::Urgency::Normal,
        input_type: None,
        wait,
        allow_custom,
        allow_ask_back,
        idempotency_key: random_id(),
        previous_id,
        agent_id,
        body_size: 0,
        approval: Some(NewApproval {
            service: s.service.clone(),
            subject: subject.clone(),
            action: s.action.clone(),
            resource_path: s.resource.clone(),
            effect: s.effect.clone(),
            key_hash,
            evaluation_id: Some(s.evaluation_id.clone()),
        }),
        // The owner can receive the request on the web page when `web_approval` is true.
        require_device: !config.web_approval,
        guard: Some(Guard {
            subject: subject.clone(),
            per_hour: config.max_requests_per_key_per_hour,
            deny_cooldown_ms: config.deny_cooldown.as_millis() as i64,
            intent: digest,
        }),
    };
    let created = match &s.supersedes {
        None => items.create(new, &body).await.map_err(storage)?,
        Some(old_id) => supersede(&items, who, &subject, &s.service, old_id, new, &body).await?,
    };
    match created {
        Created::New(l) => Ok(submitted(&l, true)),
        Created::Repeat(l) => Ok(submitted(&l, false)),
        Created::Refused(Refused::Duplicate(l)) => Ok(submitted(&l, false)),
        Created::Conflict => Err(ApprovalError::new(503, "unavailable")),
        // The agent of the intent was blocked or erased after the intent was read: the reference is dead.
        Created::BadAgent | Created::AgentBlocked => Err(INVALID_ARGUMENT),
        Created::BadWait(e) => Err(wait_error(e)),
        Created::NoDevice => Err(ApprovalError::new(409, "no_device")),
        Created::TooMany(_) => Err(ApprovalError::rate_limited(RETRY_WHEN_FULL_MS)),
        Created::Refused(r) => Err(refusal(r, items.clock.now_ms())),
    }
}

fn submitted(l: &Loaded, created: bool) -> Submitted {
    Submitted {
        request_id: l.item.request_id.clone(),
        code: l
            .approval
            .as_ref()
            .map(|a| a.code.clone())
            .unwrap_or_default(),
        created,
    }
}

fn refusal(r: Refused, now: i64) -> ApprovalError {
    match r {
        Refused::Muted { until } | Refused::Cooling { until } => {
            ApprovalError::rate_limited(until - now)
        }
        Refused::Capped { retry_after_ms } => ApprovalError::rate_limited(retry_after_ms),
        Refused::BadIntent => INVALID_ARGUMENT,
        Refused::Duplicate(_) => ApprovalError::new(503, "unavailable"),
    }
}

/// A request that replaces another one of the same service and subject. The old request must be one that the PDP
/// can still withdraw: a request that is already final stays as it is, and the new request is a plain creation. An
/// approval that the service already consumed cannot be replaced.
async fn supersede(
    items: &Items<'_>,
    who: &Requester,
    subject: &str,
    service: &str,
    old_id: &str,
    new: NewItem,
    body: &[u8],
) -> Result<Created, ApprovalError> {
    let old = items
        .get(old_id, &Caller::Agent(who.key_id.clone()))
        .await
        .map_err(storage)?;
    let Some(old) = old.filter(|l| {
        l.approval
            .as_ref()
            .is_some_and(|a| a.subject == subject && a.service == service)
    }) else {
        return Err(INVALID_ARGUMENT);
    };
    if old
        .approval
        .as_ref()
        .is_some_and(|a| a.consumed_at.is_some())
    {
        return Err(ApprovalError::new(409, "cannot_supersede"));
    }
    match items
        .supersede(old_id, &who.key_id, new.clone(), body)
        .await
        .map_err(storage)?
    {
        SupersedeOut::Created { new, .. } => Ok(Created::New(new)),
        SupersedeOut::Existing(l) => Ok(Created::Repeat(l)),
        SupersedeOut::Refused(r) => Ok(Created::Refused(r)),
        SupersedeOut::BadWait(e) => Ok(Created::BadWait(e)),
        // The old request is final and not consumed: nothing to withdraw.
        SupersedeOut::Closed(_) => items.create(new, body).await.map_err(storage),
        SupersedeOut::NotFound => Err(INVALID_ARGUMENT),
        // A chain stays inside one agent. The code set of the PEP is frozen: the answer is `invalid_argument`.
        SupersedeOut::AgentMismatch | SupersedeOut::BadAgent => Err(INVALID_ARGUMENT),
        SupersedeOut::ChainFull => Err(ApprovalError::rate_limited(RETRY_WHEN_FULL_MS)),
    }
}

// ---------------------------------------------------------------- the verdict

/// The verdict of a request for the service that made it. An unknown request, a request of another service and a
/// request of another subject give the same error: `404 not_found`. A renewal needs `renew` and the subject of the
/// request, which is the requester key.
pub async fn verdict(
    app: &App,
    service: &str,
    subject: &str,
    request_id: &str,
    renew: bool,
) -> Result<Verdict, ApprovalError> {
    let (_, items) = config(app)?;
    let Some(l) = items
        .get(request_id, &Caller::System)
        .await
        .map_err(storage)?
    else {
        return Err(NOT_FOUND);
    };
    let ours = l
        .approval
        .as_ref()
        .is_some_and(|a| a.service == service && a.subject == subject);
    if !ours || l.item.kind != Kind::Approval {
        return Err(NOT_FOUND);
    }
    if !renew {
        return Ok(verdict_of(&l));
    }
    let renewed = items
        .renew(request_id, &l.item.requester_key_id)
        .await
        .map_err(storage)?;
    match renewed {
        Renewed::NotFound => Err(NOT_FOUND),
        Renewed::Open { loaded, .. } | Renewed::Revived(loaded) | Renewed::Final(loaded) => {
            Ok(verdict_of(&loaded))
        }
    }
}

/// Waits for a verdict other than `pending`, at most `wait_s` seconds. The wait never goes above
/// `inbox.items.wait_max`, and it never fails because of it. Only the first read renews the lease.
pub async fn wait_verdict(
    app: &App,
    service: &str,
    subject: &str,
    request_id: &str,
    renew: bool,
    wait_s: u64,
) -> Result<Verdict, ApprovalError> {
    config(app)?;
    let bound = app
        .config
        .items
        .as_ref()
        .map_or(std::time::Duration::ZERO, |i| i.wait_max);
    let end = std::time::Instant::now() + std::time::Duration::from_secs(wait_s).min(bound);
    let mut first = renew;
    loop {
        // The notification is armed before the read, so a change between the read and the wait is not lost.
        let notified = app.item_changed.notified();
        tokio::pin!(notified);
        notified.as_mut().enable();
        let v = verdict(app, service, subject, request_id, first).await?;
        first = false;
        let left = end.saturating_duration_since(std::time::Instant::now());
        if v != Verdict::Pending
            || left.is_zero()
            || app.draining.load(std::sync::atomic::Ordering::Relaxed)
        {
            return Ok(v);
        }
        // The other instances write the database too: look again every second.
        let step = left.min(std::time::Duration::from_secs(1));
        tokio::select! {
            _ = &mut notified => {}
            _ = tokio::time::sleep(step) => {}
        }
    }
}

// ---------------------------------------------------------------- the intent of an agent

/// What an agent says about its next approval. It goes to the portal, never to the service.
#[derive(Clone, Debug, Default)]
pub struct IntentIn {
    pub lease_s: Option<i64>,
    pub until_ms: Option<i64>,
    pub explanation: Option<String>,
    /// How to read the explanation. `None`: plain text.
    pub explanation_format: Option<ExplanationFormat>,
    /// A hint for the color of the command of the approval. It never changes the text.
    pub language: Option<Language>,
    pub allow_custom: bool,
    pub allow_ask_back: bool,
    pub hint: Option<Value>,
    /// The id of an earlier request of the same key that the new one follows.
    pub previous_id: Option<String>,
    /// What the agent says about itself, checked by the channel with `agents::check_says`. It is sealed with the
    /// explanation, and the item keeps it as text that is not verified.
    pub agent_label: Option<String>,
}

/// The `approval_ref` that the agent passes to the service, and the time at which it expires.
#[derive(Clone, Debug)]
pub struct IntentOut {
    pub approval_ref: String,
    pub expires_at_ms: i64,
    /// The agent of the call: its id, the `agent_status`, and the token when this call made the agent. The token is
    /// shown once: the portal keeps its digest only.
    pub agent: Resolved,
}

fn check_hint(hint: &Value) -> bool {
    let Ok(hint) = serde_json::from_value::<Hint>(hint.clone()) else {
        return false;
    };
    let line = |t: &str| {
        let n = t.chars().count();
        (1..=32).contains(&n) && !t.chars().any(char::is_control)
    };
    line(&hint.suggested)
        && line(&hint.source)
        && hint.confidence.is_none_or(|c| (0.0..=1.0).contains(&c))
}

/// Makes an intent: it checks the wait, the explanation and the flags, seals the explanation and the hint, and gives
/// the `approval_ref`. The reference is 32 random bytes in base64url; the database keeps only its digest. It works
/// once, for the key that made it, until `inbox.approvals.intent_ttl` ends.
pub async fn intent(
    app: &App,
    who: &Requester,
    input: IntentIn,
) -> Result<IntentOut, ApprovalError> {
    let (config, items) = config(app)?;
    let wait = match (input.lease_s, input.until_ms) {
        (None, None) => return Err(ApprovalError::new(422, "lease_required")),
        (Some(_), Some(_)) => return Err(INVALID_LEASE),
        (Some(lease_s), None) => Wait::Lease { lease_s },
        (None, Some(at_ms)) => Wait::Until { at_ms },
    };
    check_control(wait, &items)?;
    let explanation = input.explanation.filter(|t| !t.is_empty());
    if input.explanation_format.is_some() && explanation.is_none() {
        return Err(INVALID_ARGUMENT);
    }
    match &explanation {
        Some(text) if text.chars().count() > config.max_explanation_chars => {
            return Err(ApprovalError::new(422, "explanation_too_long"));
        }
        None if config.require_explanation => {
            return Err(ApprovalError::new(422, "explanation_required"));
        }
        _ => {}
    }
    if (input.allow_custom || input.allow_ask_back)
        && who.interaction != Interaction::Conversational
    {
        return Err(ApprovalError::new(422, "interaction_not_allowed"));
    }
    if input.hint.as_ref().is_some_and(|h| !check_hint(h)) {
        return Err(INVALID_ARGUMENT);
    }
    // A token must not go into a text that a person or a phone reads. The check comes before any write.
    let texts: Vec<&str> = explanation
        .as_deref()
        .into_iter()
        .chain(input.agent_label.as_deref())
        .collect();
    if items
        .text_holds_live_token(&who.owner, &texts)
        .await
        .map_err(storage)?
    {
        return Err(AGENT_TOKEN_IN_TEXT);
    }
    // A `previous_id` must name a closed item of the same key and of the same agent as this call. A new agent has no
    // item, so it can never borrow the thread of another. The check comes before the agent is made.
    let mut previous_agent = None;
    if let Some(previous) = &input.previous_id {
        let found = items
            .get(previous, &Caller::Agent(who.key_id.clone()))
            .await
            .map_err(storage)?;
        let Some(found) = found.filter(|l| !l.item.is_open()) else {
            return Err(INVALID_ARGUMENT);
        };
        let target = items
            .agent_target(&who.key_id, &who.owner, &who.agent)
            .await?;
        if target.as_deref() != Some(found.item.agent_id.as_str()) {
            return Err(AGENT_MISMATCH);
        }
        previous_agent = Some(found.item.agent_id);
    }
    if items.live_intents(&who.key_id).await.map_err(storage)?
        >= config.max_requests_per_key_per_hour as i64
    {
        return Err(ApprovalError::rate_limited(RETRY_WHEN_FULL_MS));
    }
    // The agent: found by the token, or made now. The last check of the call, so a refusal above makes no agent.
    let agent = items
        .resolve_agent(&who.key_id, &who.owner, &who.agent, true)
        .await?
        .ok_or(ApprovalError::new(503, "unavailable"))?;
    if previous_agent.is_some_and(|id| id != agent.agent_id) {
        return Err(AGENT_MISMATCH);
    }
    let reference = {
        use base64ct::{Base64UrlUnpadded, Encoding};
        Base64UrlUnpadded::encode_string(&inbox_core::rng::Rng::os().bytes::<32>())
    };
    let digest = ref_digest(&reference);
    let mut content = json!({});
    if let Some(text) = explanation {
        content["explanation"] = Value::String(text);
    }
    if let Some(format) = input.explanation_format {
        content["explanation_format"] = json!(format);
    }
    if let Some(language) = input.language {
        content["language"] = json!(language);
    }
    if let Some(hint) = input.hint {
        content["hint"] = hint;
    }
    if let Some(label) = input.agent_label {
        content["agent_label"] = Value::String(label);
    }
    let content = Zeroizing::new(serde_json::to_vec(&content).map_err(|_| INVALID_ARGUMENT)?);
    let expires_at_ms = items
        .create_intent(NewIntent {
            digest: &digest,
            key_id: &who.key_id,
            agent_id: &agent.agent_id,
            wait,
            allow_custom: input.allow_custom,
            allow_ask_back: input.allow_ask_back,
            previous_id: input.previous_id.as_deref(),
            content: &content,
            ttl_ms: config.intent_ttl.as_millis() as i64,
        })
        .await
        .map_err(storage)?;
    Ok(IntentOut {
        approval_ref: reference,
        expires_at_ms,
        agent,
    })
}
