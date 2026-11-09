//! The operations of the items, written once for both databases.
//!
//! Each operation runs in one transaction. It reads the row that it changes with a lock, calls the pure rules of
//! `items.rs`, and writes the result. The two databases implement [`ItemTx`], the few steps that touch SQL:
//! SQLite in `items_db.rs`, PostgreSQL in `items_pg.rs`. The state machine, the lease, the checks of an answer and
//! the order of the writes are in this file only, so the two databases cannot differ.
//!
//! A row is read in the same transaction that changes it. The lock makes the answer a compare-and-set: two
//! devices that answer at once get one `accepted` and one `already_answered`. Every operation starts with
//! [`settle_due`], so the server clock decides an abandon or an expiry at each read and each write.
//! A refusal is a value, not an error, and its writes (an abandon that the clock decided) are committed.

// The approval API, the agent channel, the phones and the web call this API in the next lots.
#![allow(dead_code)]

use std::future::Future;

use inbox_core::proto::{
    Action, AnswerStatus, Decision, InputType, Interaction, ItemState, Kind, Prefs, Urgency, Via,
};

use super::db::DbError;
use super::items::{
    can_revive, check_wait, finish, renew, settle_due, transition, ApprovalRow, Caller, Effects,
    Event, ItemRow, Loaded, NewEvent, NoRevive, Notice, OwnerAlert, Rules, Wait, WaitError,
    MIN_AGENT_TURN_GAP_MS,
};

/// A sealed value with the id of the key that sealed it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Sealed {
    pub kid: String,
    pub ct: Vec<u8>,
}

/// A row of `approval_intents`: what an agent said about its next approval. The explanation and the hint are sealed
/// in `body`. The reference is never kept, only its digest.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IntentRow {
    pub ref_digest: String,
    pub requester_key_id: String,
    pub lease_s: Option<i64>,
    pub until_at: Option<i64>,
    pub allow_custom: bool,
    pub allow_ask_back: bool,
    pub body: Option<Sealed>,
    pub body_size: i64,
    pub expires_at: i64,
    pub used_at: Option<i64>,
    pub request_id: Option<String>,
    pub previous_id: Option<String>,
}

/// A row of `turns`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TurnRow {
    pub request_id: String,
    pub seq: u64,
    /// `agent` or `user:<login>`.
    pub author: String,
    /// `say`, `reply`, `answer`, `decline` or `revive`.
    pub kind: String,
    pub payload: Option<Sealed>,
    pub client_msg_id: Option<String>,
    pub device_id: Option<String>,
    /// `phone` or `web`.
    pub via: Option<String>,
    pub created_at: i64,
}

/// A delivery of an item that a device has not confirmed. The row holds metadata only: the frame is built from the
/// item when the server sends it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ItemDelivery {
    /// The order of the queue of the device.
    pub seq: i64,
    /// The id of the delivery. The frame carries it as `id`, and the receipt of the phone names it.
    pub alert_id: String,
    /// `question`, `approval`, `turn` or `settled`.
    pub kind: String,
    pub request_id: String,
    /// The number of the turn that the row names.
    pub item_seq: u64,
    pub created_at: i64,
}

/// What to count: the open items of a key, of an owner, or all.
pub enum Scope<'a> {
    Key(&'a str),
    Owner(&'a str),
    All,
}

/// The filter of a list of items.
#[derive(Clone, Debug, Default)]
pub struct ListFilter {
    pub owner: Option<String>,
    pub key_id: Option<String>,
    /// `Some(true)`: the open items. `Some(false)`: the final items. `None`: all.
    pub open: Option<bool>,
    pub limit: u32,
}

/// The steps of an operation that touch SQL. Each method runs in the transaction of the operation.
pub trait ItemTx {
    /// Reads an item and its approval, and locks the rows.
    fn load(&mut self, id: &str) -> impl Future<Output = Result<Option<Loaded>, DbError>>;
    /// Writes the columns that an operation changes.
    fn save(&mut self, l: &Loaded) -> impl Future<Output = Result<(), DbError>>;
    fn insert(
        &mut self,
        l: &Loaded,
        body: Option<&Sealed>,
    ) -> impl Future<Output = Result<(), DbError>>;
    fn sealed_body(&mut self, id: &str) -> impl Future<Output = Result<Option<Sealed>, DbError>>;
    fn find_by_idempotency(
        &mut self,
        key_id: &str,
        idempotency_key: &str,
    ) -> impl Future<Output = Result<Option<String>, DbError>>;
    /// The open item of an approval with this `key_hash`.
    fn find_open_by_hash(
        &mut self,
        key_hash: &str,
    ) -> impl Future<Output = Result<Option<String>, DbError>>;
    /// The approvals with this `key_hash`, the newest first.
    fn approvals_by_hash(
        &mut self,
        key_hash: &str,
        limit: u32,
    ) -> impl Future<Output = Result<Vec<String>, DbError>>;
    fn count_open(&mut self, scope: Scope<'_>) -> impl Future<Output = Result<i64, DbError>>;
    fn chain_len(&mut self, chain_id: &str) -> impl Future<Output = Result<i64, DbError>>;
    fn insert_turn(&mut self, t: &TurnRow) -> impl Future<Output = Result<(), DbError>>;
    fn turn_by_client_msg(
        &mut self,
        request_id: &str,
        client_msg_id: &str,
    ) -> impl Future<Output = Result<Option<u64>, DbError>>;
    fn turns(&mut self, request_id: &str) -> impl Future<Output = Result<Vec<TurnRow>, DbError>>;
    /// True when the device is revoked, unknown, or not a device of `owner`.
    fn device_unusable(
        &mut self,
        device_id: &str,
        owner: &str,
    ) -> impl Future<Output = Result<bool, DbError>>;
    fn owner_devices(&mut self, owner: &str) -> impl Future<Output = Result<Vec<String>, DbError>>;
    fn insert_event(&mut self, e: &NewEvent) -> impl Future<Output = Result<(), DbError>>;
    // The phones: the queue of the deliveries of items, the catch-up of a device, the preferences.
    /// The deliveries of items that the device has not received, oldest first. A row of an item has no age limit: the
    /// lease and `max_lifetime` decide, not `delivery_ttl`.
    fn pending_item_deliveries(
        &mut self,
        device_id: &str,
        limit: u32,
    ) -> impl Future<Output = Result<Vec<ItemDelivery>, DbError>>;
    /// Gives a device the open items of its owner that it never got: one delivery for the newest request message of
    /// each item and one for each turn. The result is the number of new deliveries. Idempotent.
    fn catch_up_device(
        &mut self,
        device_id: &str,
        owner: &str,
    ) -> impl Future<Output = Result<u64, DbError>>;
    /// The newest earlier approval with the same `key_hash` that the owner accepted: the time of the answer, and whether
    /// it was consumed.
    fn previous_approval(
        &mut self,
        key_hash: &str,
        except_id: &str,
    ) -> impl Future<Output = Result<Option<(i64, bool)>, DbError>>;
    /// The preferences of an account. Empty when the account has none.
    fn prefs(&mut self, login: &str) -> impl Future<Output = Result<Prefs, DbError>>;
    /// Replaces the preferences of an account. Nothing here applies them.
    fn put_prefs(
        &mut self,
        login: &str,
        prefs: &Prefs,
    ) -> impl Future<Output = Result<(), DbError>>;
    /// The owners that have an open item, each with the creation time of the oldest one.
    fn open_owners(&mut self) -> impl Future<Output = Result<Vec<(String, i64)>, DbError>>;
    /// Writes a row of `alerts` and a delivery for each device of the owner that is not revoked. The result is the
    /// devices of the deliveries.
    fn insert_notice(
        &mut self,
        n: &Notice,
        now: i64,
    ) -> impl Future<Output = Result<Vec<String>, DbError>>;
    /// Tells the other instances at the commit: the devices with a new delivery, and the items that changed.
    fn announce(
        &mut self,
        devices: &[String],
        requests: &[String],
    ) -> impl Future<Output = Result<(), DbError>>;
    /// Serializes the creations, so the counts of the limits and the idempotency hold with several instances.
    fn lock_creation(&mut self) -> impl Future<Output = Result<(), DbError>>;
    fn list(&mut self, filter: &ListFilter) -> impl Future<Output = Result<Vec<Loaded>, DbError>>;
    // The approval API: the cap of the creations, the mute of a key, the intents, the alerts to an owner.
    /// The number of approvals that the key created since `since`, and the time of the oldest of them.
    fn created_since(
        &mut self,
        key_id: &str,
        since: i64,
    ) -> impl Future<Output = Result<(i64, Option<i64>), DbError>>;
    /// The end of the mute of a subject. `None` when the subject was never muted.
    fn mute_until(&mut self, subject: &str) -> impl Future<Output = Result<Option<i64>, DbError>>;
    fn set_mute(
        &mut self,
        subject: &str,
        until: i64,
        reason: &str,
    ) -> impl Future<Output = Result<(), DbError>>;
    /// The `answer_action` of the newest answered approvals of a key since `since`, newest first.
    fn recent_answers(
        &mut self,
        key_id: &str,
        since: i64,
        limit: u32,
    ) -> impl Future<Output = Result<Vec<Option<String>>, DbError>>;
    fn insert_intent(&mut self, row: &IntentRow) -> impl Future<Output = Result<(), DbError>>;
    /// The intents of a key that are not used and not expired.
    fn live_intents(
        &mut self,
        key_id: &str,
        now: i64,
    ) -> impl Future<Output = Result<i64, DbError>>;
    fn find_intent(
        &mut self,
        digest: &str,
    ) -> impl Future<Output = Result<Option<IntentRow>, DbError>>;
    /// Marks an intent used by a request, and erases its sealed text. It is one compare-and-set: the result is false
    /// when the intent is unknown, of another key, used, or expired.
    fn use_intent(
        &mut self,
        digest: &str,
        key_id: &str,
        now: i64,
        request_id: &str,
    ) -> impl Future<Output = Result<bool, DbError>>;
    fn delete_intents(&mut self, now: i64) -> impl Future<Output = Result<u64, DbError>>;
    /// Writes a plain alert for the devices of an owner. The result is the devices of the deliveries.
    fn insert_owner_alert(
        &mut self,
        a: &OwnerAlert,
        now: i64,
    ) -> impl Future<Output = Result<Vec<String>, DbError>>;
    // The sweeper.
    /// Writes the time of the pass and returns the time of the previous one.
    fn beat(&mut self, now: i64) -> impl Future<Output = Result<Option<i64>, DbError>>;
    fn extend_leases(
        &mut self,
        gap_ms: i64,
        max_lifetime_ms: i64,
    ) -> impl Future<Output = Result<u64, DbError>>;
    fn due_candidates(
        &mut self,
        now: i64,
        rules: &Rules,
        limit: u32,
    ) -> impl Future<Output = Result<Vec<String>, DbError>>;
    fn lapse_candidates(
        &mut self,
        now: i64,
        limit: u32,
    ) -> impl Future<Output = Result<Vec<String>, DbError>>;
    fn purge_bodies(
        &mut self,
        now: i64,
        rules: &Rules,
    ) -> impl Future<Output = Result<u64, DbError>>;
    fn delete_final(
        &mut self,
        now: i64,
        rules: &Rules,
    ) -> impl Future<Output = Result<u64, DbError>>;
    fn delete_events(
        &mut self,
        now: i64,
        rules: &Rules,
    ) -> impl Future<Output = Result<u64, DbError>>;
    // The checks of the start and the command `content-purge`.
    /// The name of a table of the items that the database lacks: a version 5.0 made it.
    fn missing_table(&mut self) -> impl Future<Output = Result<Option<&'static str>, DbError>>;
    /// Each key id that seals a content, with the number of sealed values.
    fn kid_counts(&mut self) -> impl Future<Output = Result<Vec<(String, u64)>, DbError>>;
    /// Erases the sealed values of a key and keeps the metadata. The result is the rows of the items, of the turns and
    /// of the intents.
    fn purge_key(
        &mut self,
        kid: &str,
        now: i64,
    ) -> impl Future<Output = Result<(u64, u64, u64), DbError>>;
}

/// The tables that a version 5.1 needs for the items. A database that lacks one was made by a version 5.0.
pub const TABLES: [&str; 8] = [
    "items",
    "turns",
    "approvals",
    "item_events",
    "approval_intents",
    "approval_mute",
    "account_prefs",
    "items_heartbeat",
];

/// The time and the limits of one operation.
#[derive(Clone, Copy, Debug)]
pub struct Cx {
    pub rules: Rules,
    pub now: i64,
}

/// The result of an operation and what it wrote besides the rows.
pub struct Done<R> {
    pub value: R,
    pub fx: Effects,
}

async fn flush<T: ItemTx>(tx: &mut T, cx: &Cx, mut fx: Effects) -> Result<Effects, DbError> {
    for e in &fx.events {
        tx.insert_event(e).await?;
    }
    for n in &fx.notices {
        let devices = tx.insert_notice(n, cx.now).await?;
        fx.woken.extend(devices);
    }
    for a in &fx.alerts {
        let devices = tx.insert_owner_alert(a, cx.now).await?;
        fx.woken.extend(devices);
    }
    fx.woken.sort();
    fx.woken.dedup();
    let mut requests: Vec<String> = fx.events.iter().map(|e| e.request_id.clone()).collect();
    requests.extend(fx.notices.iter().map(|n| n.request_id.clone()));
    requests.sort();
    requests.dedup();
    if !requests.is_empty() || !fx.woken.is_empty() {
        tx.announce(&fx.woken, &requests).await?;
    }
    Ok(fx)
}

async fn done<T: ItemTx, R>(
    tx: &mut T,
    cx: &Cx,
    value: R,
    fx: Effects,
) -> Result<Done<R>, DbError> {
    Ok(Done {
        value,
        fx: flush(tx, cx, fx).await?,
    })
}

/// Reads an item and applies what the clock decides.
async fn fresh<T: ItemTx>(
    tx: &mut T,
    cx: &Cx,
    id: &str,
    fx: &mut Effects,
) -> Result<Option<Loaded>, DbError> {
    let Some(mut l) = tx.load(id).await? else {
        return Ok(None);
    };
    if settle_due(&mut l, cx.now, &cx.rules, fx) {
        tx.save(&l).await?;
    }
    Ok(Some(l))
}

/// An agent reaches its own items, an owner reaches the items that name the owner. Another caller gets nothing:
/// the answer is the same as for an id that does not exist.
fn visible(l: &Loaded, caller: &Caller) -> bool {
    match caller {
        Caller::Agent(key) => l.item.requester_key_id == *key,
        Caller::Owner(login) => l.item.requester_owner == *login,
        Caller::System => true,
    }
}

// ---------------------------------------------------------------- creation

/// The values of a new item. The id of the item and the sealed body come from the caller.
#[derive(Clone, Debug)]
pub struct NewItem {
    pub kind: Kind,
    pub requester_key_id: String,
    pub owner: String,
    pub via: Via,
    pub interaction: Interaction,
    pub urgency: Urgency,
    pub input_type: Option<InputType>,
    pub wait: Wait,
    pub allow_custom: bool,
    pub allow_ask_back: bool,
    pub idempotency_key: String,
    pub previous_id: Option<String>,
    pub body_size: i64,
    pub approval: Option<NewApproval>,
    /// Refuse the creation when the owner has no phone that is not revoked.
    pub require_device: bool,
    /// The limits of the approval API. `None` for an item that the API did not make.
    pub guard: Option<Guard>,
}

/// The limits of the approval API that need the same transaction as the creation, so that two instances cannot both
/// pass them.
#[derive(Clone, Debug)]
pub struct Guard {
    /// The subject of the question, such as `key:conf0001`. The mute is bound to it.
    pub subject: String,
    /// `max_requests_per_key_per_hour`.
    pub per_hour: u32,
    /// `deny_cooldown` in milliseconds. 0: no cooldown.
    pub deny_cooldown_ms: i64,
    /// The digest of the `approval_ref` that the request uses. The creation marks the intent used.
    pub intent: Option<String>,
}

/// Why the approval API does not create a request. The item that exists, or a time, goes with it.
#[derive(Debug)]
pub enum Refused {
    /// The key is muted until this time (Unix milliseconds).
    Muted { until: i64 },
    /// The same effect was declined, and the deny time lasts until this time.
    Cooling { until: i64 },
    /// The key created too many requests in one hour.
    Capped { retry_after_ms: i64 },
    /// The intent is unknown, used, expired, or of another key.
    BadIntent,
    /// An open request has the same effect: it is the answer.
    Duplicate(Box<Loaded>),
}

/// The metadata of a new approval.
#[derive(Clone, Debug)]
pub struct NewApproval {
    pub service: String,
    pub subject: String,
    pub action: String,
    pub resource_path: String,
    pub effect: String,
    /// The hash of the service, the subject, the action, the resource and the effect.
    pub key_hash: String,
    pub evaluation_id: Option<String>,
}

#[derive(Debug)]
pub enum CreateOutcome {
    Created(Box<Loaded>),
    /// The idempotency key, or the `key_hash` of an approval, names an item that is open or kept. The sealed body
    /// of the item comes with it, so the caller can tell a repeat from another request.
    Existing(Box<Loaded>, Option<Sealed>),
    BadWait(WaitError),
    NoDevice,
    TooManyForKey,
    TooManyForOwner,
    TooManyOpen,
    Refused(Refused),
}

/// The limits of the approval API, checked in the transaction of the creation. `old` is the request that a
/// replacement withdraws.
async fn guard_checks<T: ItemTx>(
    tx: &mut T,
    cx: &Cx,
    new: &NewItem,
    fx: &mut Effects,
) -> Result<Option<Refused>, DbError> {
    let (Some(g), Some(a)) = (&new.guard, &new.approval) else {
        return Ok(None);
    };
    if let Some(id) = tx.find_open_by_hash(&a.key_hash).await? {
        if let Some(l) = fresh(tx, cx, &id, fx).await? {
            if l.item.is_open() {
                return Ok(Some(Refused::Duplicate(Box::new(l))));
            }
        }
    }
    if let Some(until) = tx.mute_until(&g.subject).await? {
        if until > cx.now {
            return Ok(Some(Refused::Muted { until }));
        }
    }
    if g.deny_cooldown_ms > 0 {
        if let Some(id) = tx
            .approvals_by_hash(&a.key_hash, 1)
            .await?
            .into_iter()
            .next()
        {
            if let Some(l) = tx.load(&id).await? {
                let until = l.item.answered_at.unwrap_or(0) + g.deny_cooldown_ms;
                let declined = l.item.state == ItemState::Answered
                    && l.approval.as_ref().and_then(|a| a.decision) == Some(Decision::Deny);
                if declined && until > cx.now {
                    return Ok(Some(Refused::Cooling { until }));
                }
            }
        }
    }
    if g.per_hour > 0 {
        let hour = 3_600_000;
        let (count, oldest) = tx
            .created_since(&new.requester_key_id, cx.now - hour)
            .await?;
        if count >= g.per_hour as i64 {
            let retry = oldest.map_or(hour, |at| at + hour - cx.now).max(1000);
            return Ok(Some(Refused::Capped {
                retry_after_ms: retry,
            }));
        }
    }
    Ok(None)
}

fn build_row(
    cx: &Cx,
    request_id: &str,
    new: &NewItem,
    kid: &str,
    chain_id: String,
    chain_created_at: i64,
    supersedes: Option<String>,
) -> Loaded {
    let (lease_s, lease_until, until_at) = match new.wait {
        Wait::Lease { lease_s } => (
            Some(lease_s),
            Some(cx.rules.lease_until(cx.now, lease_s, chain_created_at)),
            None,
        ),
        Wait::Until { at_ms } => (None, None, Some(at_ms)),
    };
    let item = ItemRow {
        request_id: request_id.to_string(),
        kind: new.kind,
        chain_id,
        supersedes,
        superseded_by: None,
        previous_id: new.previous_id.clone(),
        requester_key_id: new.requester_key_id.clone(),
        requester_owner: new.owner.clone(),
        via: new.via,
        interaction: new.interaction,
        urgency: new.urgency,
        input_type: new.input_type,
        state: ItemState::Pending,
        settle_reason: None,
        body_kid: Some(kid.to_string()),
        body_size: new.body_size,
        body_purged_at: None,
        created_at: cx.now,
        chain_created_at,
        lease_s,
        lease_until,
        until_at,
        agent_seen_at: cx.now,
        last_agent_turn_at: None,
        abandoned_at: None,
        revivals: 0,
        last_seq: 0,
        allow_custom: new.allow_custom,
        allow_ask_back: new.allow_ask_back,
        answer_action: None,
        answered_at: None,
        answered_by: None,
        answered_via: None,
        final_at: None,
        idempotency_key: new.idempotency_key.clone(),
    };
    let approval = new.approval.as_ref().map(|a| ApprovalRow {
        request_id: request_id.to_string(),
        service: a.service.clone(),
        subject: a.subject.clone(),
        action: a.action.clone(),
        resource_path: a.resource_path.clone(),
        effect: a.effect.clone(),
        key_hash: a.key_hash.clone(),
        evaluation_id: a.evaluation_id.clone(),
        code: super::items::new_code(),
        decision: None,
        shown_effect: None,
        consume_before: None,
        consumed_at: None,
        lapsed_at: None,
    });
    Loaded { item, approval }
}

/// Creates an item with its approval, its message and its deliveries: one transaction.
pub async fn create<T: ItemTx>(
    tx: &mut T,
    cx: &Cx,
    request_id: &str,
    new: &NewItem,
    body: &Sealed,
) -> Result<Done<CreateOutcome>, DbError> {
    let mut fx = Effects::default();
    tx.lock_creation().await?;
    let mut existing = tx
        .find_by_idempotency(&new.requester_key_id, &new.idempotency_key)
        .await?;
    if existing.is_none() {
        if let Some(a) = &new.approval {
            existing = tx.find_open_by_hash(&a.key_hash).await?;
        }
    }
    if let Some(id) = existing {
        if let Some(l) = fresh(tx, cx, &id, &mut fx).await? {
            let sealed = tx.sealed_body(&id).await?;
            return done(tx, cx, CreateOutcome::Existing(Box::new(l), sealed), fx).await;
        }
    }
    if let Err(e) = check_wait(new.wait, cx.now, cx.now, &cx.rules) {
        return done(tx, cx, CreateOutcome::BadWait(e), fx).await;
    }
    if let Some(refused) = guard_checks(tx, cx, new, &mut fx).await? {
        return done(tx, cx, CreateOutcome::Refused(refused), fx).await;
    }
    let rules = &cx.rules;
    let counts = [
        (
            Scope::Key(&new.requester_key_id),
            rules.max_open_per_key,
            CreateOutcome::TooManyForKey,
        ),
        (
            Scope::Owner(&new.owner),
            rules.max_open_per_owner,
            CreateOutcome::TooManyForOwner,
        ),
        (Scope::All, rules.max_open_total, CreateOutcome::TooManyOpen),
    ];
    for (scope, limit, refusal) in counts {
        if tx.count_open(scope).await? >= limit as i64 {
            return done(tx, cx, refusal, fx).await;
        }
    }
    if new.require_device && tx.owner_devices(&new.owner).await?.is_empty() {
        return done(tx, cx, CreateOutcome::NoDevice, fx).await;
    }
    if let Some(digest) = new.guard.as_ref().and_then(|g| g.intent.as_deref()) {
        if !tx
            .use_intent(digest, &new.requester_key_id, cx.now, request_id)
            .await?
        {
            return done(tx, cx, CreateOutcome::Refused(Refused::BadIntent), fx).await;
        }
    }
    let l = build_row(
        cx,
        request_id,
        new,
        &body.kid,
        request_id.to_string(),
        cx.now,
        None,
    );
    tx.insert(&l, Some(body)).await?;
    fx.event(
        &l.item,
        cx.now,
        "requested",
        &Caller::Agent(new.requester_key_id.clone()).actor(),
        l.approval.as_ref().map(|a| format!("effect={}", a.effect)),
    );
    fx.request_notice(&l);
    done(tx, cx, CreateOutcome::Created(Box::new(l)), fx).await
}

// ---------------------------------------------------------------- reads and renewals

/// Reads an item for a caller after the clock decided its state.
pub async fn read<T: ItemTx>(
    tx: &mut T,
    cx: &Cx,
    id: &str,
    caller: &Caller,
) -> Result<Done<Option<Loaded>>, DbError> {
    let mut fx = Effects::default();
    let l = fresh(tx, cx, id, &mut fx)
        .await?
        .filter(|l| visible(l, caller));
    done(tx, cx, l, fx).await
}

pub async fn list<T: ItemTx>(
    tx: &mut T,
    cx: &Cx,
    filter: &ListFilter,
) -> Result<Done<Vec<Loaded>>, DbError> {
    let mut fx = Effects::default();
    let mut rows = tx.list(filter).await?;
    // A row that the clock has not settled yet reads as the clock decides, and the sweeper writes it.
    for l in &mut rows {
        settle_due(l, cx.now, &cx.rules, &mut fx);
    }
    fx.events.clear();
    fx.notices.clear();
    if filter.open == Some(true) {
        rows.retain(|l| l.item.is_open());
    }
    Ok(Done { value: rows, fx })
}

#[derive(Debug)]
pub enum Renewed {
    NotFound,
    /// The item is open. `written` tells whether the row changed: a renewal writes at most once in `lease_s`.
    Open {
        loaded: Box<Loaded>,
        written: bool,
    },
    /// The agent is back: the item was `abandoned` and is `pending` again.
    Revived(Box<Loaded>),
    /// The item is final. An `abandoned` item that cannot revive is here too (`410 item_abandoned`).
    Final(Box<Loaded>),
}

/// A call of the requester key that renews the lease: `get_answer`, a read with `wait`, `keepalive`.
pub async fn renew_lease<T: ItemTx>(
    tx: &mut T,
    cx: &Cx,
    id: &str,
    key_id: &str,
) -> Result<Done<Renewed>, DbError> {
    let mut fx = Effects::default();
    let caller = Caller::Agent(key_id.to_string());
    let Some(mut l) = fresh(tx, cx, id, &mut fx)
        .await?
        .filter(|l| visible(l, &caller))
    else {
        return done(tx, cx, Renewed::NotFound, fx).await;
    };
    let value = if l.item.is_open() {
        let written = renew(&mut l.item, cx.now, &cx.rules);
        if written {
            tx.save(&l).await?;
        }
        Renewed::Open {
            loaded: Box::new(l),
            written,
        }
    } else if can_revive(&l.item, cx.now, &cx.rules).is_ok()
        && match &l.approval {
            // A request for the same effect is open: the unique index allows one.
            Some(a) => tx.find_open_by_hash(&a.key_hash).await?.is_none(),
            None => true,
        }
    {
        let item = &mut l.item;
        item.state = transition(ItemState::Abandoned, Event::Revive).unwrap_or(ItemState::Pending);
        item.revivals += 1;
        item.abandoned_at = None;
        item.final_at = None;
        item.settle_reason = None;
        item.agent_seen_at = cx.now;
        item.last_seq += 1;
        if let Some(lease_s) = item.lease_s {
            item.lease_until = Some(cx.rules.lease_until(cx.now, lease_s, item.chain_created_at));
        }
        tx.save(&l).await?;
        tx.insert_turn(&TurnRow {
            request_id: id.to_string(),
            seq: l.item.last_seq,
            author: "agent".into(),
            kind: "revive".into(),
            payload: None,
            client_msg_id: None,
            device_id: None,
            via: None,
            created_at: cx.now,
        })
        .await?;
        fx.event(
            &l.item,
            cx.now,
            "revived",
            &caller.actor(),
            Some(format!("revivals={}", l.item.revivals)),
        );
        fx.request_notice(&l);
        Renewed::Revived(Box::new(l))
    } else {
        Renewed::Final(Box::new(l))
    };
    done(tx, cx, value, fx).await
}

/// Why a call cannot revive, for the answer `410`.
pub fn revive_refusal(l: &Loaded, cx: &Cx) -> Option<NoRevive> {
    can_revive(&l.item, cx.now, &cx.rules).err()
}

// ---------------------------------------------------------------- turns

#[derive(Clone, Debug)]
pub enum Speaker {
    /// The requester key says something.
    Agent(String),
    /// The owner asks the agent, from a device or from the web.
    Owner {
        login: String,
        device: Option<String>,
    },
}

#[derive(Clone, Debug)]
pub struct TurnIn {
    pub speaker: Speaker,
    pub payload: Sealed,
    /// The number that the turn must have. The payload was sealed for it.
    pub expected_seq: u64,
    pub client_msg_id: Option<String>,
}

#[derive(Debug)]
pub enum TurnOut {
    NotFound,
    Stored {
        loaded: Box<Loaded>,
        seq: u64,
    },
    /// The thread moved: seal again for `last_seq + 1`.
    SeqMoved {
        last_seq: u64,
    },
    /// The same `client_msg_id` was stored before.
    Duplicate {
        seq: u64,
    },
    Closed(Box<Loaded>),
    DeviceRevoked,
    AskBackNotAllowed,
    TooManyTurns,
    TooFast,
}

/// A `say` of the agent or a `reply` of the owner.
pub async fn turn<T: ItemTx>(
    tx: &mut T,
    cx: &Cx,
    id: &str,
    input: &TurnIn,
) -> Result<Done<TurnOut>, DbError> {
    let mut fx = Effects::default();
    let (caller, device) = match &input.speaker {
        Speaker::Agent(key) => (Caller::Agent(key.clone()), None),
        Speaker::Owner { login, device } => (Caller::Owner(login.clone()), device.clone()),
    };
    let Some(mut l) = fresh(tx, cx, id, &mut fx)
        .await?
        .filter(|l| visible(l, &caller))
    else {
        return done(tx, cx, TurnOut::NotFound, fx).await;
    };
    if let Some(cmid) = &input.client_msg_id {
        if let Some(seq) = tx.turn_by_client_msg(id, cmid).await? {
            return done(tx, cx, TurnOut::Duplicate { seq }, fx).await;
        }
    }
    if let (Some(device), Speaker::Owner { login, .. }) = (&device, &input.speaker) {
        if tx.device_unusable(device, login).await? {
            return done(tx, cx, TurnOut::DeviceRevoked, fx).await;
        }
    }
    let agent = matches!(input.speaker, Speaker::Agent(_));
    let event = if agent { Event::Say } else { Event::Reply };
    let refusal = if !l.item.is_open() {
        Some(TurnOut::Closed(Box::new(l.clone())))
    } else if !l.item.allow_ask_back || l.item.interaction == Interaction::OneShot {
        Some(TurnOut::AskBackNotAllowed)
    } else if l.item.last_seq >= cx.rules.max_turns as u64 {
        Some(TurnOut::TooManyTurns)
    } else if agent
        && l.item
            .last_agent_turn_at
            .is_some_and(|at| cx.now - at < MIN_AGENT_TURN_GAP_MS)
    {
        Some(TurnOut::TooFast)
    } else if input.expected_seq != l.item.last_seq + 1 {
        Some(TurnOut::SeqMoved {
            last_seq: l.item.last_seq,
        })
    } else {
        None
    };
    if let Some(refusal) = refusal {
        return done(tx, cx, refusal, fx).await;
    }
    let before = l.item.state;
    let Some(after) = transition(before, event) else {
        return done(tx, cx, TurnOut::Closed(Box::new(l)), fx).await;
    };
    l.item.state = after;
    l.item.last_seq = input.expected_seq;
    if agent {
        l.item.last_agent_turn_at = Some(cx.now);
        // The agent that speaks is alive: the lease renews too.
        renew(&mut l.item, cx.now, &cx.rules);
    }
    tx.save(&l).await?;
    let author = if agent {
        "agent".to_string()
    } else {
        caller.actor()
    };
    tx.insert_turn(&TurnRow {
        request_id: id.to_string(),
        seq: input.expected_seq,
        author,
        kind: if agent { "say" } else { "reply" }.into(),
        payload: Some(input.payload.clone()),
        client_msg_id: input.client_msg_id.clone(),
        device_id: device.clone(),
        via: (!agent).then(|| if device.is_some() { "phone" } else { "web" }.to_string()),
        created_at: cx.now,
    })
    .await?;
    if after != before {
        fx.event(
            &l.item,
            cx.now,
            if agent { "resumed" } else { "replied" },
            &caller.actor(),
            None,
        );
    }
    fx.turn_notice(&l, device);
    let seq = l.item.last_seq;
    done(
        tx,
        cx,
        TurnOut::Stored {
            loaded: Box::new(l),
            seq,
        },
        fx,
    )
    .await
}

// ---------------------------------------------------------------- the answer

#[derive(Clone, Debug)]
pub enum AnswerVia {
    Device(String),
    Web,
}

#[derive(Clone, Debug)]
pub struct AnswerIn {
    /// The owner: the login of the device or of the web session.
    pub login: String,
    pub via: AnswerVia,
    pub action: Action,
    /// The effect that the screen showed. An accept of an approval needs it.
    pub shown_effect: Option<String>,
    pub client_msg_id: Option<String>,
    /// The text or the choice of the owner, sealed for `expected_seq`.
    pub payload: Option<Sealed>,
    pub expected_seq: u64,
    /// `consume_within` of the file, in milliseconds. The window of an approval is the larger of this and `lease_s`.
    pub consume_within_ms: i64,
}

#[derive(Debug)]
pub enum AnswerOut {
    NotFound,
    /// The answer was not taken. The state of the item is in the row.
    Refused {
        status: AnswerStatus,
        loaded: Box<Loaded>,
    },
    Accepted {
        loaded: Box<Loaded>,
        seq: u64,
    },
    SeqMoved {
        last_seq: u64,
    },
}

/// The only answer of an item: a compare-and-set on the state, the lease, the lifetime and the device.
pub async fn answer<T: ItemTx>(
    tx: &mut T,
    cx: &Cx,
    id: &str,
    a: &AnswerIn,
) -> Result<Done<AnswerOut>, DbError> {
    let mut fx = Effects::default();
    let caller = Caller::Owner(a.login.clone());
    let Some(mut l) = fresh(tx, cx, id, &mut fx)
        .await?
        .filter(|l| visible(l, &caller))
    else {
        return done(tx, cx, AnswerOut::NotFound, fx).await;
    };
    let device = match &a.via {
        AnswerVia::Device(d) => Some(d.clone()),
        AnswerVia::Web => None,
    };
    let refuse = |status: AnswerStatus, l: Loaded| AnswerOut::Refused {
        status,
        loaded: Box::new(l),
    };
    if let Some(d) = &device {
        if tx.device_unusable(d, &a.login).await? {
            return done(tx, cx, refuse(AnswerStatus::DeviceRevoked, l), fx).await;
        }
    }
    if !l.item.is_open() {
        let status = super::items::closed_status(l.item.state);
        return done(tx, cx, refuse(status, l), fx).await;
    }
    if let Some(approval) = &l.approval {
        if a.action == Action::Accept && a.shown_effect.as_deref() != Some(approval.effect.as_str())
        {
            return done(tx, cx, refuse(AnswerStatus::EffectMismatch, l), fx).await;
        }
        // The text of the owner goes with a decline only, and only when the agent allowed it.
        if a.payload.is_some() && (a.action == Action::Accept || !l.item.allow_custom) {
            return done(tx, cx, refuse(AnswerStatus::CustomNotAllowed, l), fx).await;
        }
    }
    if a.payload.is_some() && a.expected_seq != l.item.last_seq + 1 {
        let last_seq = l.item.last_seq;
        return done(tx, cx, AnswerOut::SeqMoved { last_seq }, fx).await;
    }
    let Some(after) = transition(l.item.state, Event::Answer(a.action)) else {
        return done(tx, cx, refuse(AnswerStatus::Closed, l), fx).await;
    };
    let via = match &a.via {
        AnswerVia::Device(d) => format!("device:{d}"),
        AnswerVia::Web => format!("web:{}", a.login),
    };
    let item = &mut l.item;
    item.state = after;
    item.answer_action = Some(a.action);
    item.answered_at = Some(cx.now);
    item.answered_by = Some(a.login.clone());
    item.answered_via = Some(via.clone());
    item.settle_reason = Some("answered".into());
    item.final_at = Some(cx.now);
    item.last_seq += 1;
    let seq = item.last_seq;
    let window = a.consume_within_ms.max(item.lease_s.unwrap_or(0) * 1000);
    let mut detail = format!(
        "action={}",
        if a.action == Action::Accept {
            "accept"
        } else {
            "decline"
        }
    );
    if let Some(approval) = l.approval.as_mut() {
        let approve = a.action == Action::Accept;
        approval.decision = Some(if approve {
            Decision::Approve
        } else {
            Decision::Deny
        });
        approval.shown_effect = a.shown_effect.clone();
        approval.consume_before = approve.then_some(cx.now + window);
        detail.push_str(&format!(
            " effect={} key={}",
            approval.effect, l.item.requester_key_id
        ));
    }
    tx.save(&l).await?;
    tx.insert_turn(&TurnRow {
        request_id: id.to_string(),
        seq,
        author: caller.actor(),
        kind: if a.action == Action::Accept {
            "answer"
        } else {
            "decline"
        }
        .into(),
        payload: a.payload.clone(),
        client_msg_id: a.client_msg_id.clone(),
        device_id: device.clone(),
        via: Some(if device.is_some() { "phone" } else { "web" }.into()),
        created_at: cx.now,
    })
    .await?;
    fx.event(&l.item, cx.now, "answered", &via, Some(detail));
    fx.settled_notice(&l, device);
    if a.action == Action::Decline && l.approval.is_some() {
        mute_after_refusals(tx, cx, &l, &mut fx).await?;
    }
    done(
        tx,
        cx,
        AnswerOut::Accepted {
            loaded: Box::new(l),
            seq,
        },
        fx,
    )
    .await
}

/// After an explicit refusal: when the key has `mute_after_refusals` refusals in a row since its last mute, the key is
/// muted for `mute_for` and the owner gets an alert. An answer `accept` ends the row. Another end of a request (an
/// abandon, an expiry) is not an answer and does not count.
async fn mute_after_refusals<T: ItemTx>(
    tx: &mut T,
    cx: &Cx,
    l: &Loaded,
    fx: &mut Effects,
) -> Result<(), DbError> {
    let (n, Some(a)) = (cx.rules.mute_after_refusals, l.approval.as_ref()) else {
        return Ok(());
    };
    if n == 0 {
        return Ok(());
    }
    let since = tx.mute_until(&a.subject).await?.unwrap_or(0);
    let recent = tx
        .recent_answers(&l.item.requester_key_id, since, n)
        .await?;
    if recent.len() as u32 == n && recent.iter().all(|r| r.as_deref() == Some("decline")) {
        let until = cx.now + cx.rules.mute_for_ms;
        tx.set_mute(&a.subject, until, "refusals").await?;
        fx.event(
            &l.item,
            cx.now,
            "muted",
            "server",
            Some(format!("subject={} until={until}", a.subject)),
        );
        fx.alerts.push(OwnerAlert {
            owner: l.item.requester_owner.clone(),
            title: format!(
                "The key {} is muted: {n} approvals in a row were declined",
                l.item.requester_key_id
            ),
            key: format!("mute:{}:{until}", l.item.requester_key_id),
            digest: false,
        });
    }
    Ok(())
}

// ---------------------------------------------------------------- cancel and supersede

#[derive(Clone, Debug)]
pub enum CancelBy {
    /// The requester key withdraws the item.
    Agent(String),
    /// The server withdraws it: `key_revoked`, `owner_deleted` or `admin_cancelled`.
    System { reason: &'static str, actor: String },
}

#[derive(Debug)]
pub enum Cancelled {
    NotFound,
    Done(Box<Loaded>),
    /// The item was final or consumed before.
    Final(Box<Loaded>),
}

/// An approval that was answered and not consumed can still be withdrawn or replaced.
fn withdrawable(l: &Loaded) -> bool {
    match l.item.state {
        ItemState::Pending | ItemState::Replied => true,
        ItemState::Answered => l.approval.as_ref().is_some_and(|a| {
            a.decision == Some(Decision::Approve)
                && a.consumed_at.is_none()
                && a.lapsed_at.is_none()
        }),
        _ => false,
    }
}

pub async fn cancel<T: ItemTx>(
    tx: &mut T,
    cx: &Cx,
    id: &str,
    by: &CancelBy,
) -> Result<Done<Cancelled>, DbError> {
    let mut fx = Effects::default();
    let (caller, reason, actor) = match by {
        CancelBy::Agent(key) => (
            Caller::Agent(key.clone()),
            "agent_cancelled",
            format!("key:{key}"),
        ),
        CancelBy::System { reason, actor } => (Caller::System, *reason, actor.clone()),
    };
    let Some(mut l) = fresh(tx, cx, id, &mut fx)
        .await?
        .filter(|l| visible(l, &caller))
    else {
        return done(tx, cx, Cancelled::NotFound, fx).await;
    };
    if !withdrawable(&l) {
        return done(tx, cx, Cancelled::Final(Box::new(l)), fx).await;
    }
    finish(
        &mut l,
        ItemState::Cancelled,
        reason,
        &actor,
        cx.now,
        &mut fx,
    );
    tx.save(&l).await?;
    done(tx, cx, Cancelled::Done(Box::new(l)), fx).await
}

#[derive(Debug)]
pub enum SupersedeOut {
    NotFound,
    /// The old item is final, or consumed.
    Closed(Box<Loaded>),
    ChainFull,
    BadWait(WaitError),
    /// The idempotency key of the new item names an item that exists.
    Existing(Box<Loaded>),
    Refused(Refused),
    Created {
        old: Box<Loaded>,
        new: Box<Loaded>,
    },
}

/// Replaces an item: the old item is `superseded`, a new item is `pending` in the same chain, one transaction.
/// The new item keeps `chain_created_at` of the chain, so a supersede never extends `max_lifetime`.
pub async fn supersede<T: ItemTx>(
    tx: &mut T,
    cx: &Cx,
    old_id: &str,
    key_id: &str,
    new_id: &str,
    new: &NewItem,
    body: &Sealed,
) -> Result<Done<SupersedeOut>, DbError> {
    let mut fx = Effects::default();
    let caller = Caller::Agent(key_id.to_string());
    let Some(mut old) = fresh(tx, cx, old_id, &mut fx)
        .await?
        .filter(|l| visible(l, &caller))
    else {
        return done(tx, cx, SupersedeOut::NotFound, fx).await;
    };
    if let Some(id) = tx.find_by_idempotency(key_id, &new.idempotency_key).await? {
        if let Some(l) = fresh(tx, cx, &id, &mut fx).await? {
            return done(tx, cx, SupersedeOut::Existing(Box::new(l)), fx).await;
        }
    }
    if !withdrawable(&old) {
        return done(tx, cx, SupersedeOut::Closed(Box::new(old)), fx).await;
    }
    if tx.chain_len(&old.item.chain_id).await? >= cx.rules.max_chain as i64 {
        return done(tx, cx, SupersedeOut::ChainFull, fx).await;
    }
    if let Err(e) = check_wait(new.wait, cx.now, old.item.chain_created_at, &cx.rules) {
        return done(tx, cx, SupersedeOut::BadWait(e), fx).await;
    }
    let mut item = new.clone();
    item.requester_key_id = key_id.to_string();
    item.owner = old.item.requester_owner.clone();
    if let Some(refused) = guard_checks(tx, cx, &item, &mut fx).await? {
        return done(tx, cx, SupersedeOut::Refused(refused), fx).await;
    }
    if let Some(digest) = item.guard.as_ref().and_then(|g| g.intent.as_deref()) {
        if !tx.use_intent(digest, key_id, cx.now, new_id).await? {
            return done(tx, cx, SupersedeOut::Refused(Refused::BadIntent), fx).await;
        }
    }
    let created = build_row(
        cx,
        new_id,
        &item,
        &body.kid,
        old.item.chain_id.clone(),
        old.item.chain_created_at,
        Some(old_id.to_string()),
    );
    old.item.superseded_by = Some(new_id.to_string());
    finish(
        &mut old,
        ItemState::Superseded,
        "superseded",
        &caller.actor(),
        cx.now,
        &mut fx,
    );
    tx.save(&old).await?;
    tx.insert(&created, Some(body)).await?;
    fx.event(
        &created.item,
        cx.now,
        "requested",
        &caller.actor(),
        Some(format!("supersedes={old_id}")),
    );
    fx.request_notice(&created);
    done(
        tx,
        cx,
        SupersedeOut::Created {
            old: Box::new(old),
            new: Box::new(created),
        },
        fx,
    )
    .await
}

// ---------------------------------------------------------------- the approval of the PDP

#[derive(Debug)]
pub enum Consumed {
    /// The approval is used. The value `consumed_at` goes back to `release`.
    Used(Box<Loaded>),
    /// The approval can be used. Only `status` gives this value: it consumes nothing.
    Usable(Box<Loaded>),
    /// No approval has this `key_hash`.
    NoApproval,
    /// The owner has not decided yet.
    Pending(Box<Loaded>),
    /// The owner declined.
    Denied(Box<Loaded>),
    /// The approval is consumed, expired, lapsed, abandoned, cancelled or superseded. A consumed approval has
    /// `consumed_at`.
    Gone(Box<Loaded>),
}

/// Consumes the approval that fits a `key_hash`: the first one that is approved and not used, else the one that
/// waits, else the newest. The consumption is one compare-and-set, so an approval runs once.
pub async fn consume<T: ItemTx>(
    tx: &mut T,
    cx: &Cx,
    key_hash: &str,
) -> Result<Done<Consumed>, DbError> {
    gate(tx, cx, key_hash, true).await
}

/// Reads what `consume` would find and consumes nothing. The clock still settles the items that are due.
pub async fn status<T: ItemTx>(
    tx: &mut T,
    cx: &Cx,
    key_hash: &str,
) -> Result<Done<Consumed>, DbError> {
    gate(tx, cx, key_hash, false).await
}

async fn gate<T: ItemTx>(
    tx: &mut T,
    cx: &Cx,
    key_hash: &str,
    take: bool,
) -> Result<Done<Consumed>, DbError> {
    let mut fx = Effects::default();
    let ids = tx.approvals_by_hash(key_hash, 8).await?;
    let mut all = Vec::new();
    for id in &ids {
        if let Some(l) = fresh(tx, cx, id, &mut fx).await? {
            all.push(l);
        }
    }
    let usable = |l: &Loaded| {
        l.item.state == ItemState::Answered
            && l.approval.as_ref().is_some_and(|a| {
                a.decision == Some(Decision::Approve)
                    && a.consumed_at.is_none()
                    && a.lapsed_at.is_none()
            })
    };
    let value = if let Some(pos) = all.iter().position(usable) {
        let mut l = all.swap_remove(pos);
        if take {
            if let Some(a) = l.approval.as_mut() {
                a.consumed_at = Some(cx.now);
            }
            tx.save(&l).await?;
            fx.event(&l.item, cx.now, "consumed", "server", None);
            Consumed::Used(Box::new(l))
        } else {
            Consumed::Usable(Box::new(l))
        }
    } else if let Some(pos) = all.iter().position(|l| l.item.is_open()) {
        Consumed::Pending(Box::new(all.swap_remove(pos)))
    } else if all.is_empty() {
        Consumed::NoApproval
    } else {
        let l = all.swap_remove(0);
        let denied = l.item.state == ItemState::Answered
            && l.approval
                .as_ref()
                .is_some_and(|a| a.decision == Some(Decision::Deny));
        if denied {
            Consumed::Denied(Box::new(l))
        } else {
            Consumed::Gone(Box::new(l))
        }
    };
    done(tx, cx, value, fx).await
}

/// Gives an approval back after the count of the budget refused the action: the approval stays usable until its
/// `consume_before`. The value `consumed_at` of the consumption must still be the one in the row.
pub async fn release<T: ItemTx>(
    tx: &mut T,
    cx: &Cx,
    id: &str,
    consumed_at: i64,
) -> Result<Done<bool>, DbError> {
    let mut fx = Effects::default();
    let mut released = false;
    if let Some(mut l) = tx.load(id).await? {
        if let Some(a) = l
            .approval
            .as_mut()
            .filter(|a| a.consumed_at == Some(consumed_at))
        {
            a.consumed_at = None;
            tx.save(&l).await?;
            fx.event(&l.item, cx.now, "released", "server", None);
            released = true;
        }
    }
    done(tx, cx, released, fx).await
}

// ---------------------------------------------------------------- the sweeper

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SweepReport {
    pub abandoned: u64,
    pub expired: u64,
    pub lapsed: u64,
    /// The leases extended because the last pass was long ago.
    pub extended: u64,
}

/// One pass of the sweeper. It writes the heartbeat, extends the leases after a gap, and applies the clock to the
/// items that are due. The rows are locked with `SKIP LOCKED` on PostgreSQL, so two instances do not meet.
pub async fn sweep<T: ItemTx>(
    tx: &mut T,
    cx: &Cx,
    limit: u32,
) -> Result<Done<SweepReport>, DbError> {
    let mut fx = Effects::default();
    let mut report = SweepReport::default();
    if let Some(previous) = tx.beat(cx.now).await? {
        let gap = cx.now - previous;
        if gap > cx.rules.grace_ms {
            report.extended = tx.extend_leases(gap, cx.rules.max_lifetime_ms).await?;
        }
    }
    let mut ids = tx.due_candidates(cx.now, &cx.rules, limit).await?;
    ids.extend(tx.lapse_candidates(cx.now, limit).await?);
    for id in ids {
        if let Some(mut l) = tx.load(&id).await? {
            if settle_due(&mut l, cx.now, &cx.rules, &mut fx) {
                tx.save(&l).await?;
            }
        }
    }
    for e in &fx.events {
        match e.event {
            "abandoned" => report.abandoned += 1,
            "expired" => report.expired += 1,
            "lapsed" => report.lapsed += 1,
            _ => {}
        }
    }
    done(tx, cx, report, fx).await
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MaintainReport {
    pub bodies_purged: u64,
    pub items_deleted: u64,
    pub events_deleted: u64,
    pub intents_deleted: u64,
}

/// The retention: erases the sealed bodies after `body_purge_after`, deletes the final items after `retention` and
/// the events after `audit_retention`.
pub async fn maintain<T: ItemTx>(tx: &mut T, cx: &Cx) -> Result<MaintainReport, DbError> {
    Ok(MaintainReport {
        bodies_purged: tx.purge_bodies(cx.now, &cx.rules).await?,
        items_deleted: tx.delete_final(cx.now, &cx.rules).await?,
        events_deleted: tx.delete_events(cx.now, &cx.rules).await?,
        intents_deleted: tx.delete_intents(cx.now).await?,
    })
}

/// An owner with an open item, and the preferences that tell when its digest is due.
#[derive(Clone, Debug)]
pub struct DigestCandidate {
    pub owner: String,
    /// The creation time of the oldest open item of the owner.
    pub oldest_ms: i64,
    pub prefs: Prefs,
}

/// The owners that have an open item, each with its preferences. An owner with no open item has no digest.
pub async fn digest_candidates<T: ItemTx>(tx: &mut T) -> Result<Vec<DigestCandidate>, DbError> {
    let mut out = Vec::new();
    for (owner, oldest_ms) in tx.open_owners().await? {
        let prefs = tx.prefs(&owner).await?;
        out.push(DigestCandidate {
            owner,
            oldest_ms,
            prefs,
        });
    }
    Ok(out)
}

/// The result of [`digest_rows`].
#[derive(Debug, Default)]
pub struct DigestRows {
    /// The rows that this call wrote and that have a device to go to.
    pub written: u32,
    /// The devices with a new delivery.
    pub woken: Vec<String>,
}

/// Writes the rows of the digests. A row whose key exists is not written again: PostgreSQL and SQLite refuse the second
/// insert of the same key, so one row exists for each owner and due time, with any number of instances.
pub async fn digest_rows<T: ItemTx>(
    tx: &mut T,
    cx: &Cx,
    rows: &[OwnerAlert],
) -> Result<DigestRows, DbError> {
    let mut out = DigestRows::default();
    for row in rows {
        let devices = tx.insert_owner_alert(row, cx.now).await?;
        if !devices.is_empty() {
            out.written += 1;
        }
        out.woken.extend(devices);
    }
    out.woken.sort();
    out.woken.dedup();
    if !out.woken.is_empty() {
        tx.announce(&out.woken, &[]).await?;
    }
    Ok(out)
}

/// Adds the counts of the same key id.
pub fn merge_counts(rows: Vec<(String, u64)>) -> Vec<(String, u64)> {
    let mut out: std::collections::BTreeMap<String, u64> = std::collections::BTreeMap::new();
    for (kid, n) in rows {
        *out.entry(kid).or_default() += n;
    }
    out.into_iter().collect()
}
