//! The rules of the items, without storage: the clock, the limits, the states, the lease and the events.
//!
//! An item is a message that waits for an answer of its owner. An approval asks to run one effect. A question
//! asks for an answer. The functions of this module read and change plain values. The storage code
//! (`items_ops.rs`) reads a row, calls these functions, and writes the result in one transaction. Each rule of
//! time takes `now` from an injected [`Clock`], so a test simulates 90 days without a pause.
//!
//! The states are those of `inbox_core::proto::ItemState`. `pending` and `replied` are open. The other states
//! are final, except `abandoned` during `lease.revive_window`. An approval that is `answered` has a decision.
//! The consumption and the lapse of an approval are marks of the row of the approval, not states.

// The approval API, the agent channel, the phones and the web call this API in the next lots.
#![allow(dead_code)]

#[cfg(test)]
use std::sync::atomic::{AtomicI64, Ordering};
use std::time::Duration;

use inbox_core::proto::{
    Action, AnswerStatus, Decision, InputType, Interaction, ItemState, Kind, Urgency, Via,
};

use crate::config::ItemsConfig;

/// The time source of the engine.
pub trait Clock: Send + Sync {
    /// Unix time in milliseconds.
    fn now_ms(&self) -> i64;
}

/// The clock of the machine. A test of the whole server can fix it (the conformance harness does).
#[derive(Default)]
pub struct SystemClock {
    #[cfg(test)]
    fixed: AtomicI64,
}

impl SystemClock {
    pub fn new() -> SystemClock {
        SystemClock::default()
    }

    /// Stops the clock at `ms`, or lets it run again with `None`.
    #[cfg(test)]
    pub fn fix(&self, ms: Option<i64>) {
        self.fixed.store(ms.unwrap_or(0), Ordering::SeqCst);
    }
}

impl Clock for SystemClock {
    fn now_ms(&self) -> i64 {
        #[cfg(test)]
        {
            let fixed = self.fixed.load(Ordering::SeqCst);
            if fixed != 0 {
                return fixed;
            }
        }
        super::util::now_ms()
    }
}

/// A clock that a test moves by hand.
#[cfg(test)]
pub struct ManualClock(AtomicI64);

#[cfg(test)]
impl ManualClock {
    pub fn new(start_ms: i64) -> ManualClock {
        ManualClock(AtomicI64::new(start_ms))
    }

    pub fn advance(&self, by: Duration) {
        self.0.fetch_add(by.as_millis() as i64, Ordering::SeqCst);
    }

    /// Puts the clock at `ms` (Unix time).
    pub fn set(&self, ms: i64) {
        self.0.store(ms, Ordering::SeqCst);
    }
}

#[cfg(test)]
impl Clock for ManualClock {
    fn now_ms(&self) -> i64 {
        self.0.load(Ordering::SeqCst)
    }
}

/// The effective lease is this many times `lease_s`, from the last renewal.
pub const LEASE_FACTOR: i64 = 3;
/// An agent turn comes at least this long after the last agent turn.
pub const MIN_AGENT_TURN_GAP_MS: i64 = 2_000;
/// An item with `until` has no lease: its `agent_seen_at` moves at most once in this time.
pub const UNTIL_SEEN_STEP_MS: i64 = 60_000;

/// The limits of the file, in milliseconds, as the rules read them.
#[derive(Clone, Copy, Debug)]
pub struct Rules {
    pub max_lifetime_ms: i64,
    pub lease_min_s: i64,
    pub lease_max_s: i64,
    pub grace_ms: i64,
    pub revive_window_ms: i64,
    pub max_revivals: u32,
    pub max_chain: u32,
    pub max_turns: u32,
    pub max_open_per_key: u32,
    pub max_open_per_owner: u32,
    pub max_open_total: u32,
    pub retention_ms: i64,
    pub body_purge_ms: i64,
    pub audit_retention_ms: i64,
    /// `inbox.approvals.mute_after_refusals`. 0: a key is never muted.
    pub mute_after_refusals: u32,
    /// `inbox.approvals.mute_for`, in milliseconds.
    pub mute_for_ms: i64,
    /// The limits and the lock of the agents of a key. Off without the block `inbox.agents`.
    pub agents: super::agents::AgentRules,
}

impl Rules {
    pub fn from_config(c: &ItemsConfig) -> Rules {
        let ms = |d: Duration| d.as_millis() as i64;
        Rules {
            max_lifetime_ms: ms(c.max_lifetime),
            lease_min_s: c.lease.min.as_secs() as i64,
            lease_max_s: c.lease.max.as_secs() as i64,
            grace_ms: ms(c.lease.grace),
            revive_window_ms: ms(c.lease.revive_window),
            max_revivals: c.lease.max_revivals,
            max_chain: c.max_chain,
            max_turns: c.max_turns,
            max_open_per_key: c.max_open_per_key,
            max_open_per_owner: c.max_open_per_owner,
            max_open_total: c.max_open_total,
            retention_ms: ms(c.retention),
            body_purge_ms: ms(c.body_purge_after),
            audit_retention_ms: ms(c.audit_retention),
            mute_after_refusals: 0,
            mute_for_ms: 0,
            agents: super::agents::AgentRules::off(),
        }
    }

    /// The same rules with the values of the block `inbox.agents`.
    pub fn with_agents(mut self, a: &crate::config::AgentsConfig) -> Rules {
        self.agents = super::agents::AgentRules::from_config(a);
        self
    }

    /// The same rules with the values of the block `inbox.approvals`.
    pub fn with_approvals(mut self, a: &crate::config::ApprovalsConfig) -> Rules {
        self.mute_after_refusals = a.mute_after_refusals;
        self.mute_for_ms = a.mute_for.as_millis() as i64;
        self
    }

    /// The end of the life of a chain. A supersede never moves it.
    pub fn lifetime_until(&self, chain_created_at: i64) -> i64 {
        chain_created_at + self.max_lifetime_ms
    }

    /// The end of the lease that a renewal at `now` gives: 3 times `lease_s`, and never past the lifetime.
    pub fn lease_until(&self, now: i64, lease_s: i64, chain_created_at: i64) -> i64 {
        (now + LEASE_FACTOR * lease_s * 1000).min(self.lifetime_until(chain_created_at))
    }
}

/// How the agent states the time that it waits. One of the two, never both.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Wait {
    /// The cadence of the renewals, in seconds.
    Lease { lease_s: i64 },
    /// A date, in Unix milliseconds. The item has no lease.
    Until { at_ms: i64 },
}

/// Why a wait is not valid. The caller answers `422` and never changes the value.
#[derive(Debug, PartialEq, Eq)]
pub enum WaitError {
    LeaseOutOfRange { min_s: i64, max_s: i64 },
    UntilNotInFuture,
    UntilBeyondLifetime { limit_ms: i64 },
}

/// Checks a wait against the limits of the file. `chain_created_at` is `now` for a new chain.
pub fn check_wait(
    wait: Wait,
    now: i64,
    chain_created_at: i64,
    rules: &Rules,
) -> Result<(), WaitError> {
    match wait {
        Wait::Lease { lease_s } if lease_s < rules.lease_min_s || lease_s > rules.lease_max_s => {
            Err(WaitError::LeaseOutOfRange {
                min_s: rules.lease_min_s,
                max_s: rules.lease_max_s,
            })
        }
        Wait::Lease { .. } => Ok(()),
        Wait::Until { at_ms } if at_ms <= now => Err(WaitError::UntilNotInFuture),
        Wait::Until { at_ms } if at_ms > rules.lifetime_until(chain_created_at) => {
            Err(WaitError::UntilBeyondLifetime {
                limit_ms: rules.lifetime_until(chain_created_at),
            })
        }
        Wait::Until { .. } => Ok(()),
    }
}

/// The two ids of a text that the agent cannot set.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Caller {
    /// The requester key, with its id.
    Agent(String),
    /// The owner, with the login.
    Owner(String),
    /// The server itself.
    System,
}

impl Caller {
    /// The text of the audit.
    pub fn actor(&self) -> String {
        match self {
            Caller::Agent(key) => format!("key:{key}"),
            Caller::Owner(login) => format!("user:{login}"),
            Caller::System => "server".into(),
        }
    }
}

/// The columns of an item, except the sealed body.
#[derive(Clone, Debug, PartialEq)]
pub struct ItemRow {
    pub request_id: String,
    pub kind: Kind,
    pub chain_id: String,
    pub supersedes: Option<String>,
    pub superseded_by: Option<String>,
    pub previous_id: Option<String>,
    pub requester_key_id: String,
    pub requester_owner: String,
    /// The agent of the item: a row of `agents` of the same key. The bucket of the key when no agent is
    /// known. A merge moves the item to another agent, so this value can change.
    pub agent_id: String,
    pub via: Via,
    pub interaction: Interaction,
    pub urgency: Urgency,
    pub input_type: Option<InputType>,
    pub state: ItemState,
    /// The reason of the final state, as the frame `settled` names it.
    pub settle_reason: Option<String>,
    pub body_kid: Option<String>,
    pub body_size: i64,
    pub body_purged_at: Option<i64>,
    pub created_at: i64,
    pub chain_created_at: i64,
    pub lease_s: Option<i64>,
    pub lease_until: Option<i64>,
    pub until_at: Option<i64>,
    pub agent_seen_at: i64,
    pub last_agent_turn_at: Option<i64>,
    pub abandoned_at: Option<i64>,
    pub revivals: u32,
    pub last_seq: u64,
    pub allow_custom: bool,
    pub allow_ask_back: bool,
    pub answer_action: Option<Action>,
    pub answered_at: Option<i64>,
    pub answered_by: Option<String>,
    /// `device:<id>` or `web:<login>`.
    pub answered_via: Option<String>,
    /// When the item became final. It starts the retention.
    pub final_at: Option<i64>,
    pub idempotency_key: String,
}

/// The columns of an approval.
#[derive(Clone, Debug, PartialEq)]
pub struct ApprovalRow {
    pub request_id: String,
    pub service: String,
    pub subject: String,
    pub action: String,
    pub resource_path: String,
    pub effect: String,
    pub key_hash: String,
    pub evaluation_id: Option<String>,
    pub code: String,
    pub decision: Option<Decision>,
    pub shown_effect: Option<String>,
    pub consume_before: Option<i64>,
    pub consumed_at: Option<i64>,
    pub lapsed_at: Option<i64>,
}

/// An item with its approval.
#[derive(Clone, Debug, PartialEq)]
pub struct Loaded {
    pub item: ItemRow,
    pub approval: Option<ApprovalRow>,
}

impl ItemRow {
    pub fn is_open(&self) -> bool {
        !self.state.is_final()
    }

    /// The end of the time in which an `abandoned` item revives. `None` for an item in another state.
    pub fn revivable_until(&self, rules: &Rules) -> Option<i64> {
        (self.state == ItemState::Abandoned && self.body_purged_at.is_none())
            .then(|| self.abandoned_at.map(|at| at + rules.revive_window_ms))
            .flatten()
            .filter(|_| self.revivals < rules.max_revivals)
    }
}

pub fn state_str(state: ItemState) -> &'static str {
    match state {
        ItemState::Pending => "pending",
        ItemState::Replied => "replied",
        ItemState::Answered => "answered",
        ItemState::Abandoned => "abandoned",
        ItemState::Expired => "expired",
        ItemState::Cancelled => "cancelled",
        ItemState::Superseded => "superseded",
    }
}

pub fn parse_state(text: &str) -> Option<ItemState> {
    Some(match text {
        "pending" => ItemState::Pending,
        "replied" => ItemState::Replied,
        "answered" => ItemState::Answered,
        "abandoned" => ItemState::Abandoned,
        "expired" => ItemState::Expired,
        "cancelled" => ItemState::Cancelled,
        "superseded" => ItemState::Superseded,
        _ => return None,
    })
}

pub fn status_str(status: AnswerStatus) -> &'static str {
    match status {
        AnswerStatus::Accepted => "accepted",
        AnswerStatus::AlreadyAnswered => "already_answered",
        AnswerStatus::Closed => "closed",
        AnswerStatus::Abandoned => "abandoned",
        AnswerStatus::Superseded => "superseded",
        AnswerStatus::Expired => "expired",
        AnswerStatus::Cancelled => "cancelled",
        AnswerStatus::EffectMismatch => "effect_mismatch",
        AnswerStatus::DeviceRevoked => "device_revoked",
        AnswerStatus::CustomNotAllowed => "custom_not_allowed",
        AnswerStatus::AskBackNotAllowed => "ask_back_not_allowed",
        AnswerStatus::AgentLocked => "agent_locked",
    }
}

/// The status of an answer that comes after the item is final.
pub fn closed_status(state: ItemState) -> AnswerStatus {
    match state {
        ItemState::Answered => AnswerStatus::AlreadyAnswered,
        ItemState::Abandoned => AnswerStatus::Abandoned,
        ItemState::Superseded => AnswerStatus::Superseded,
        ItemState::Expired => AnswerStatus::Expired,
        ItemState::Cancelled => AnswerStatus::Cancelled,
        ItemState::Pending | ItemState::Replied => AnswerStatus::Closed,
    }
}

// ---------------------------------------------------------------- the state machine

/// What can happen to an item.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Event {
    /// The owner answers.
    Answer(Action),
    /// The owner asks the agent.
    Reply,
    /// The agent speaks.
    Say,
    /// The agent or the server withdraws the item.
    Cancel,
    /// The agent replaces the item with a new one.
    Supersede,
    /// The lease and its grace ended.
    Abandon,
    /// `until` or `max_lifetime` was reached.
    Expire,
    /// The agent comes back after an abandon.
    Revive,
    /// The agent renews the lease.
    Renew,
}

/// The state after an event, or `None` when the event is not valid in the state. The engine adds what the table
/// cannot know: the lease dates, the flags and, for an `answered` approval, the consumption.
pub fn transition(state: ItemState, event: Event) -> Option<ItemState> {
    use ItemState::*;
    Some(match (state, event) {
        (Pending | Replied, Event::Answer(_)) => Answered,
        (Pending | Replied, Event::Reply) => Replied,
        (Pending | Replied, Event::Say) => Pending,
        (Pending | Replied, Event::Cancel) => Cancelled,
        (Pending | Replied, Event::Supersede) => Superseded,
        (Pending | Replied, Event::Abandon) => Abandoned,
        (Pending | Replied, Event::Expire) => Expired,
        (Pending, Event::Renew) => Pending,
        (Replied, Event::Renew) => Replied,
        // An approval that was answered and not consumed can still be withdrawn or replaced.
        (Answered, Event::Cancel) => Cancelled,
        (Answered, Event::Supersede) => Superseded,
        (Abandoned, Event::Revive | Event::Renew) => Pending,
        _ => return None,
    })
}

/// What the clock decides for an open item.
#[derive(Debug, PartialEq, Eq)]
pub enum Due {
    /// `until` or `max_lifetime` was reached. The value is the reason of `settled`.
    Expire(&'static str),
    Abandon,
}

/// The change of state that the time asks for, or `None`.
pub fn due(item: &ItemRow, now: i64, rules: &Rules) -> Option<Due> {
    if !item.is_open() {
        return None;
    }
    if now >= rules.lifetime_until(item.chain_created_at) {
        return Some(Due::Expire("max_lifetime"));
    }
    if item.until_at.is_some_and(|at| now >= at) {
        return Some(Due::Expire("until_reached"));
    }
    if item
        .lease_until
        .is_some_and(|until| now >= until + rules.grace_ms)
    {
        return Some(Due::Abandon);
    }
    None
}

/// Renews the lease of an open item. The write happens at most once in `lease_s`. The result tells whether the
/// row changed.
pub fn renew(item: &mut ItemRow, now: i64, rules: &Rules) -> bool {
    match item.lease_s {
        Some(lease_s) => {
            if item.lease_until.is_some() && now - item.agent_seen_at < lease_s * 1000 {
                return false;
            }
            item.agent_seen_at = now;
            item.lease_until = Some(rules.lease_until(now, lease_s, item.chain_created_at));
            true
        }
        None => {
            if now - item.agent_seen_at < UNTIL_SEEN_STEP_MS {
                return false;
            }
            item.agent_seen_at = now;
            true
        }
    }
}

/// Why an abandoned item does not revive.
#[derive(Debug, PartialEq, Eq)]
pub enum NoRevive {
    NotAbandoned,
    WindowEnded,
    TooManyRevivals,
    BodyPurged,
    LifetimeEnded,
}

/// Whether a renewal at `now` brings an abandoned item back.
pub fn can_revive(item: &ItemRow, now: i64, rules: &Rules) -> Result<(), NoRevive> {
    if item.state != ItemState::Abandoned {
        return Err(NoRevive::NotAbandoned);
    }
    if item.body_purged_at.is_some() {
        return Err(NoRevive::BodyPurged);
    }
    if item.revivals >= rules.max_revivals {
        return Err(NoRevive::TooManyRevivals);
    }
    if now >= rules.lifetime_until(item.chain_created_at) {
        return Err(NoRevive::LifetimeEnded);
    }
    match item.abandoned_at {
        Some(at) if now < at + rules.revive_window_ms => Ok(()),
        _ => Err(NoRevive::WindowEnded),
    }
}

// ---------------------------------------------------------------- what an operation writes besides the rows

/// A row of `item_events`: one for each transition, never one for a renewal, and no body.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NewEvent {
    pub request_id: String,
    pub kind: Kind,
    pub at: i64,
    pub event: &'static str,
    pub actor: String,
    pub detail: Option<String>,
}

/// A message for the devices of the owner: a row of `alerts` with its deliveries. The title is a fixed text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Notice {
    /// `question`, `approval`, `turn` or `settled`. `settled` is a database kind that no frame names.
    pub kind: &'static str,
    pub request_id: String,
    pub seq: u64,
    pub title: String,
    pub owner: String,
    /// The device that made the change and needs no message.
    pub except_device: Option<String>,
}

/// An alert of the portal to the devices of an owner. It names no item: it is a plain alert (`kind = 'alert'`), or the
/// row of a digest (`kind = 'digest'`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OwnerAlert {
    pub owner: String,
    /// A fixed text. It never holds a body.
    pub title: String,
    /// Makes the alert idempotent: one alert for each key.
    pub key: String,
    /// True: the row is the digest of the owner. The frame holds numbers that the server reads when it sends the row.
    pub digest: bool,
}

/// What one operation writes besides the rows of the item: the events and the messages. The storage code writes
/// them in the transaction. The caller reads them after the commit, for the log and the metrics.
#[derive(Debug, Default)]
pub struct Effects {
    pub events: Vec<NewEvent>,
    pub notices: Vec<Notice>,
    /// Alerts of the portal to an owner that name no item, such as the mute of a key.
    pub alerts: Vec<OwnerAlert>,
    /// The devices that have a new delivery, filled when the notices are written.
    pub woken: Vec<String>,
}

/// The fixed title of the message of an item. The phone builds the visible title from the sealed body.
pub fn notice_title(item: &ItemRow, approval: Option<&ApprovalRow>, kind: &'static str) -> String {
    match (kind, approval) {
        ("settled", _) => "Item settled".into(),
        ("turn", _) => "Message".into(),
        (_, Some(a)) => format!("Approval {}", a.code),
        _ => match item.kind {
            Kind::Approval => "Approval".into(),
            _ => "Question".into(),
        },
    }
}

impl Effects {
    pub fn event(
        &mut self,
        item: &ItemRow,
        at: i64,
        event: &'static str,
        actor: &str,
        detail: Option<String>,
    ) {
        self.events.push(NewEvent {
            request_id: item.request_id.clone(),
            kind: item.kind,
            at,
            event,
            actor: actor.to_string(),
            detail,
        });
    }

    /// A message of the kind of the item, for a new request frame (a creation or a revival).
    pub fn request_notice(&mut self, l: &Loaded) {
        let kind = if l.item.kind == Kind::Approval {
            "approval"
        } else {
            "question"
        };
        self.notices.push(Notice {
            kind,
            request_id: l.item.request_id.clone(),
            seq: l.item.last_seq,
            title: notice_title(&l.item, l.approval.as_ref(), kind),
            owner: l.item.requester_owner.clone(),
            except_device: None,
        });
    }

    pub fn turn_notice(&mut self, l: &Loaded, except_device: Option<String>) {
        self.notices.push(Notice {
            kind: "turn",
            request_id: l.item.request_id.clone(),
            seq: l.item.last_seq,
            title: notice_title(&l.item, l.approval.as_ref(), "turn"),
            owner: l.item.requester_owner.clone(),
            except_device,
        });
    }

    pub fn settled_notice(&mut self, l: &Loaded, except_device: Option<String>) {
        self.notices.push(Notice {
            kind: "settled",
            request_id: l.item.request_id.clone(),
            seq: l.item.last_seq,
            title: notice_title(&l.item, l.approval.as_ref(), "settled"),
            owner: l.item.requester_owner.clone(),
            except_device,
        });
    }
}

/// Moves an open item to a final state and records the event and the message `settled`.
pub fn finish(
    l: &mut Loaded,
    state: ItemState,
    reason: &'static str,
    actor: &str,
    now: i64,
    fx: &mut Effects,
) {
    l.item.state = state;
    l.item.settle_reason = Some(reason.to_string());
    l.item.final_at = Some(now);
    if state == ItemState::Abandoned {
        l.item.abandoned_at = Some(now);
    }
    fx.event(
        &l.item,
        now,
        state_str(state),
        actor,
        Some(format!("reason={reason}")),
    );
    fx.settled_notice(l, None);
}

/// Applies what the clock decides: an abandon, an expiry, and the lapse of an approval that nobody consumed in
/// time. Every read and every write of an item starts here, so the server clock decides each time. The result is
/// true when the row changed.
pub fn settle_due(l: &mut Loaded, now: i64, rules: &Rules, fx: &mut Effects) -> bool {
    let mut changed = false;
    match due(&l.item, now, rules) {
        Some(Due::Expire(reason)) => {
            finish(l, ItemState::Expired, reason, "server", now, fx);
            changed = true;
        }
        Some(Due::Abandon) => {
            finish(l, ItemState::Abandoned, "lease_lapsed", "server", now, fx);
            changed = true;
        }
        None => {}
    }
    if l.item.state == ItemState::Answered {
        if let Some(a) = l.approval.as_mut() {
            let late = a.consume_before.is_some_and(|until| now > until);
            if a.decision == Some(Decision::Approve)
                && a.consumed_at.is_none()
                && a.lapsed_at.is_none()
                && late
            {
                a.lapsed_at = Some(now);
                fx.event(&l.item, now, "lapsed", "server", None);
                changed = true;
            }
        }
    }
    changed
}

/// The key of the requests of an approval: the hash of the service, the subject, the action, the path of the
/// resource and the effect. Each part ends with a zero byte, so two different parts never give the same text. One
/// non-final item exists for each value. The PDP and the submission API compute it the same way.
pub fn approval_key_hash(
    service: &str,
    subject: &str,
    action: &str,
    resource_path: &str,
    effect: &str,
) -> String {
    use sha2::{Digest, Sha256};
    let mut hash = Sha256::new();
    for part in [service, subject, action, resource_path, effect] {
        hash.update(part.as_bytes());
        hash.update([0u8]);
    }
    super::util::hex(&hash.finalize())
}

/// The sealed payload of a text of the owner: a `decline` with a comment or a change request. It is the compact JSON
/// `{"text": ..., "intent": "comment" | "change_request"}`. The phone path and the web path build it with
/// [`owner_text_payload`]. The agent channel reads it with [`read_owner_text`].
pub fn owner_text_payload(
    text: &str,
    intent: inbox_core::proto::Intent,
) -> zeroize::Zeroizing<Vec<u8>> {
    zeroize::Zeroizing::new(
        serde_json::to_vec(&serde_json::json!({"text": text, "intent": intent}))
            .unwrap_or_default(),
    )
}

/// Reads the payload of a text of the owner. A payload that is not of this form is a plain text with the intent
/// `comment`: the reader never fails on the text of a person.
pub fn read_owner_text(payload: &[u8]) -> (String, inbox_core::proto::Intent) {
    use inbox_core::proto::Intent;
    #[derive(serde::Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Form {
        text: String,
        intent: Intent,
    }
    match serde_json::from_slice::<Form>(payload) {
        Ok(form) => (form.text, form.intent),
        Err(_) => (
            String::from_utf8_lossy(payload).into_owned(),
            Intent::Comment,
        ),
    }
}

/// The code of an approval: 4 characters that a person reads aloud without a doubt. It is not a secret.
pub fn new_code() -> String {
    const ALPHABET: &[u8] = b"ABCDEFGHJKLMNPQRSTUVWXYZ23456789";
    inbox_core::rng::Rng::os()
        .bytes::<4>()
        .iter()
        .map(|b| ALPHABET[(*b as usize) % ALPHABET.len()] as char)
        .collect()
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub fn rules() -> Rules {
        let day = 86_400_000;
        Rules {
            max_lifetime_ms: 90 * day,
            lease_min_s: 30,
            lease_max_s: 7 * 86_400,
            grace_ms: 120_000,
            revive_window_ms: 7 * day,
            max_revivals: 3,
            max_chain: 10,
            max_turns: 40,
            max_open_per_key: 5,
            max_open_per_owner: 20,
            max_open_total: 1000,
            retention_ms: 180 * day,
            body_purge_ms: 30 * day,
            audit_retention_ms: 365 * day,
            mute_after_refusals: 3,
            mute_for_ms: 3_600_000,
            agents: crate::inbox::agents::AgentRules {
                enabled: true,
                max_per_key: 4,
                new_per_hour: 3,
                lock_new: false,
                idle_after_ms: 14 * day,
                purge_after_ms: 365 * day,
            },
        }
    }

    pub fn row(state: ItemState) -> ItemRow {
        ItemRow {
            request_id: "r".into(),
            kind: Kind::Approval,
            chain_id: "r".into(),
            supersedes: None,
            superseded_by: None,
            previous_id: None,
            requester_key_id: "key".into(),
            requester_owner: "alice".into(),
            agent_id: "agent".into(),
            via: Via::Mcp,
            interaction: Interaction::Conversational,
            urgency: Urgency::Normal,
            input_type: None,
            state,
            settle_reason: None,
            body_kid: Some("k1".into()),
            body_size: 10,
            body_purged_at: None,
            created_at: 1_000,
            chain_created_at: 1_000,
            lease_s: Some(900),
            lease_until: Some(1_000 + 3 * 900_000),
            until_at: None,
            agent_seen_at: 1_000,
            last_agent_turn_at: None,
            abandoned_at: None,
            revivals: 0,
            last_seq: 0,
            allow_custom: false,
            allow_ask_back: false,
            answer_action: None,
            answered_at: None,
            answered_by: None,
            answered_via: None,
            final_at: None,
            idempotency_key: "i".into(),
        }
    }

    #[test]
    fn lease_effective_is_three_times_lease_s() {
        let r = rules();
        assert_eq!(r.lease_until(10_000, 900, 10_000), 10_000 + 3 * 900_000);
        // The lifetime caps it: a chain that is 89 days old has 1 day left.
        let day = 86_400_000;
        let created = 0;
        let now = 89 * day;
        assert_eq!(r.lease_until(now, 7 * 86_400, created), 90 * day);
        // A renewal gives the same effective lease.
        let mut item = row(ItemState::Pending);
        item.agent_seen_at = 0;
        item.lease_until = Some(1);
        assert!(renew(&mut item, 900_000, &r));
        assert_eq!(item.lease_until, Some(900_000 + 3 * 900_000));
    }

    #[test]
    fn renewal_write_at_most_once_per_lease_s() {
        let r = rules();
        let mut item = row(ItemState::Pending);
        let first = item.lease_until;
        // 899 s after the last write: no write.
        let seen = item.agent_seen_at;
        assert!(!renew(&mut item, seen + 899_000, &r));
        assert_eq!(item.lease_until, first);
        // 900 s after: one write.
        let at = seen + 900_000;
        assert!(renew(&mut item, at, &r));
        assert_eq!(item.agent_seen_at, at);
        assert!(
            !renew(&mut item, at + 1, &r),
            "the next call writes nothing"
        );
        // An item with `until` has no lease and moves its mark once a minute.
        let mut until = row(ItemState::Pending);
        until.lease_s = None;
        until.lease_until = None;
        until.until_at = Some(10_000_000);
        let seen = until.agent_seen_at;
        assert!(!renew(&mut until, seen + 59_000, &r));
        assert!(renew(&mut until, seen + 60_000, &r));
        assert_eq!(until.lease_until, None);
    }

    #[test]
    fn abandon_after_lease_plus_grace_not_before() {
        let r = rules();
        let item = row(ItemState::Pending);
        let end = item.lease_until.unwrap() + r.grace_ms;
        assert_eq!(due(&item, end - 1, &r), None);
        assert_eq!(due(&item, end, &r), Some(Due::Abandon));
        // A final item is never due.
        assert_eq!(due(&row(ItemState::Answered), end * 10, &r), None);
    }

    #[test]
    fn until_reached_is_expired_neutral() {
        let r = rules();
        let mut item = row(ItemState::Pending);
        item.lease_s = None;
        item.lease_until = None;
        item.until_at = Some(50_000);
        assert_eq!(due(&item, 49_999, &r), None);
        assert_eq!(due(&item, 50_000, &r), Some(Due::Expire("until_reached")));
        // Far in the future the item never becomes abandoned: it has no lease.
        assert_eq!(due(&item, 50_000, &r), Some(Due::Expire("until_reached")));
        let mut l = Loaded {
            item,
            approval: None,
        };
        let mut fx = Effects::default();
        assert!(settle_due(&mut l, 50_000, &r, &mut fx));
        assert_eq!(l.item.state, ItemState::Expired);
        assert_eq!(fx.events.len(), 1);
        assert_eq!(fx.events[0].event, "expired");
        assert_eq!(fx.notices[0].kind, "settled");
    }

    #[test]
    fn max_lifetime_caps_lease_and_supersede_does_not_extend() {
        let r = rules();
        let day = 86_400_000;
        let mut item = row(ItemState::Pending);
        item.chain_created_at = 0;
        item.lease_s = Some(7 * 86_400);
        item.lease_until = Some(1);
        item.agent_seen_at = 0;
        assert!(renew(&mut item, 80 * day, &r));
        assert_eq!(
            item.lease_until,
            Some(90 * day),
            "the lifetime caps the lease"
        );
        assert_eq!(due(&item, 90 * day, &r), Some(Due::Expire("max_lifetime")));
        // A wait that passes the lifetime of the chain is refused: a successor shares `chain_created_at`.
        assert_eq!(
            check_wait(Wait::Until { at_ms: 91 * day }, 80 * day, 0, &r),
            Err(WaitError::UntilBeyondLifetime { limit_ms: 90 * day })
        );
        assert_eq!(
            check_wait(Wait::Until { at_ms: 89 * day }, 80 * day, 0, &r),
            Ok(())
        );
        // The same limit for a lease: the effective lease of the successor still stops at the lifetime.
        assert_eq!(r.lease_until(85 * day, 3600, 0), 85 * day + 3 * 3_600_000);
        assert_eq!(
            r.lease_until(89 * day + 3_600_000 * 23, 86_400, 0),
            90 * day
        );
    }

    #[test]
    fn waits_are_checked_and_never_changed() {
        let r = rules();
        assert_eq!(check_wait(Wait::Lease { lease_s: 30 }, 0, 0, &r), Ok(()));
        assert_eq!(
            check_wait(Wait::Lease { lease_s: 29 }, 0, 0, &r),
            Err(WaitError::LeaseOutOfRange {
                min_s: 30,
                max_s: 604_800
            })
        );
        assert!(check_wait(Wait::Lease { lease_s: 604_801 }, 0, 0, &r).is_err());
        assert_eq!(
            check_wait(Wait::Until { at_ms: 5 }, 5, 0, &r),
            Err(WaitError::UntilNotInFuture)
        );
    }

    #[test]
    fn state_machine_every_state_times_event_table() {
        use Event::*;
        use ItemState::*;
        let states = [
            Pending, Replied, Answered, Abandoned, Expired, Cancelled, Superseded,
        ];
        let events = [
            Answer(Action::Accept),
            Answer(Action::Decline),
            Reply,
            Say,
            Cancel,
            Supersede,
            Abandon,
            Expire,
            Revive,
            Renew,
        ];
        // The expected table, written out: `None` is a refusal.
        let expected = |s: ItemState, e: Event| -> Option<ItemState> {
            match (s, e) {
                (Pending | Replied, Answer(_)) => Some(Answered),
                (Pending | Replied, Reply) => Some(Replied),
                (Pending | Replied, Say) => Some(Pending),
                (Pending | Replied, Cancel) => Some(Cancelled),
                (Pending | Replied, Supersede) => Some(Superseded),
                (Pending | Replied, Abandon) => Some(Abandoned),
                (Pending | Replied, Expire) => Some(Expired),
                (Pending, Renew) => Some(Pending),
                (Replied, Renew) => Some(Replied),
                (Answered, Cancel) => Some(Cancelled),
                (Answered, Supersede) => Some(Superseded),
                (Abandoned, Revive | Renew) => Some(Pending),
                _ => None,
            }
        };
        for s in states {
            for e in events {
                assert_eq!(transition(s, e), expected(s, e), "{s:?} x {e:?}");
            }
        }
        // Only an open state accepts an answer; each final state names its own status.
        for s in [Expired, Cancelled, Superseded, Abandoned, Answered] {
            assert_eq!(transition(s, Answer(Action::Accept)), None);
        }
        assert_eq!(closed_status(Abandoned), AnswerStatus::Abandoned);
        assert_eq!(closed_status(Answered), AnswerStatus::AlreadyAnswered);
        assert_eq!(closed_status(Superseded), AnswerStatus::Superseded);
    }

    #[test]
    fn revive_needs_the_window_the_count_and_the_body() {
        let r = rules();
        let mut item = row(ItemState::Abandoned);
        item.abandoned_at = Some(1_000_000);
        assert_eq!(
            can_revive(&item, 1_000_000 + r.revive_window_ms - 1, &r),
            Ok(())
        );
        assert_eq!(
            can_revive(&item, 1_000_000 + r.revive_window_ms, &r),
            Err(NoRevive::WindowEnded)
        );
        item.revivals = 3;
        assert_eq!(
            can_revive(&item, 1_000_001, &r),
            Err(NoRevive::TooManyRevivals)
        );
        item.revivals = 0;
        item.body_purged_at = Some(1);
        assert_eq!(can_revive(&item, 1_000_001, &r), Err(NoRevive::BodyPurged));
        assert_eq!(
            can_revive(&row(ItemState::Pending), 1, &r),
            Err(NoRevive::NotAbandoned)
        );
    }

    #[test]
    fn the_text_of_the_owner_goes_through_its_payload_and_a_plain_text_reads_as_a_comment() {
        use inbox_core::proto::Intent;
        let payload = owner_text_payload("Use \"staging\".\n", Intent::ChangeRequest);
        assert_eq!(
            read_owner_text(&payload),
            ("Use \"staging\".\n".to_string(), Intent::ChangeRequest)
        );
        assert_eq!(
            read_owner_text(b"no, not now"),
            ("no, not now".to_string(), Intent::Comment)
        );
        // A JSON text with other members is a plain text: the form is closed.
        let odd = br#"{"text":"a","intent":"comment","by":"x"}"#;
        assert_eq!(read_owner_text(odd).0, String::from_utf8_lossy(odd));
    }

    #[test]
    fn the_code_has_four_characters_without_look_alikes() {
        for _ in 0..50 {
            let code = new_code();
            assert_eq!(code.len(), 4);
            assert!(!code.contains(['0', 'O', '1', 'I']), "{code}");
        }
    }
}
