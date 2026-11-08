//! The items for the rest of the portal: the calls that the approval API, the agent channel, the phones and the web
//! use, on SQLite or PostgreSQL.
//!
//! This module seals what it writes and unseals what it serves. The operations (`items_ops.rs`) never see a clear
//! body: they get sealed values. Only the functions `serve_*` of this module unseal, and each of them checks the
//! seal. The clock, the box and the limits come from the caller, so a test builds the engine without a server.
//! An item never uses the spool: when PostgreSQL has no primary, a write gets [`ItemError::Degraded`].

// The PDP calls `consume`, `release` and `status_for_pdp`. The approval API, the agent channel, the phones and the web
// call the rest of this API in the next lots.
#![allow(dead_code)]

use inbox_core::proto::{Action, AnswerStatus, Kind, Prefs};
use zeroize::Zeroizing;

use super::content_box::{self, ContentBox, FIELD_BODY, FIELD_INTENT, FIELD_TURN};
use super::db::DbError;
use super::hub::Hub;
use super::items::{Caller, Clock, Effects, Loaded, NewEvent, Rules, Wait};
use super::items_db;
use super::items_ops::{
    self as ops, AnswerIn, AnswerOut, AnswerVia, CancelBy, Cancelled, Consumed, CreateOutcome, Cx,
    IntentRow, ItemDelivery, ItemTx, ListFilter, MaintainReport, NewItem, Refused, Renewed, Sealed,
    Speaker, SupersedeOut, SweepReport, TurnIn, TurnOut, TurnRow,
};
use super::items_pg::PgTx;
use super::log;
use super::metrics::InboxMetrics;
use super::pg;
use super::replica::Replica;
use super::store::Store;
use super::util::random_id;

/// Why a call of the engine failed. A refusal of a rule is a value of the operation, not an error.
#[derive(Debug)]
pub enum ItemError {
    /// PostgreSQL has no usable primary: the caller answers `503 storage_degraded`.
    Degraded,
    Storage(DbError),
    /// The seal failed. The message never holds a value.
    Seal(String),
    /// The copy of the replica does not hold the write and `sync_alerts` is true.
    Replica,
}

impl From<DbError> for ItemError {
    fn from(e: DbError) -> Self {
        if e.is_unavailable() {
            ItemError::Degraded
        } else {
            ItemError::Storage(e)
        }
    }
}

impl std::fmt::Display for ItemError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ItemError::Degraded => f.write_str("storage_degraded"),
            ItemError::Storage(e) => write!(f, "{e}"),
            ItemError::Seal(e) => write!(f, "seal: {e}"),
            ItemError::Replica => f.write_str("the replica does not hold the write"),
        }
    }
}

/// Runs the operation `$body` (a future that takes `$tx`) in one transaction of the database in use. The names in
/// `$cap` are the values that the operation borrows: the SQLite thread gets a copy of each.
macro_rules! in_tx {
    ($self:expr, [$($cap:ident),*], $tx:ident, $body:expr) => {{
        let result: Result<_, DbError> = match $self.store {
            Store::Sqlite(db) => {
                $( let $cap = $cap.clone(); )*
                db.call(move |c| items_db::in_tx(c, |$tx| items_db::ready($body)))
                    .await
            }
            Store::Pg(pg) => {
                pg.guard(async {
                    let mut c = pg.client().await?;
                    let t = c.transaction().await.map_err(pg::db_error)?;
                    let mut owner = PgTx(t);
                    let out = {
                        let $tx = &mut owner;
                        $body.await?
                    };
                    owner.0.commit().await.map_err(pg::db_error)?;
                    Ok(out)
                })
                .await
            }
        };
        result.map_err(ItemError::from)
    }};
}

/// The store alone, for the checks of the start and the commands.
struct StoreRef<'a> {
    store: &'a Store,
}

/// The name of the journal line of an event of an item: `approval_requested`, `approval_answered`, `approval_consumed`,
/// `approval_abandoned`, `approval_revived`, `approval_superseded`, `approval_expired` and the others. A question has
/// the prefix `question_`.
pub fn log_name(kind: Kind, event: &str) -> String {
    match kind {
        Kind::Approval => format!("approval_{event}"),
        _ => format!("question_{event}"),
    }
}

/// The result of `create`.
#[derive(Debug)]
pub enum Created {
    New(Box<Loaded>),
    /// The same request again: the item that exists. The caller renews the lease when the agent asks.
    Repeat(Box<Loaded>),
    /// The idempotency key names another request.
    Conflict,
    BadWait(super::items::WaitError),
    NoDevice,
    TooMany(Limit),
    /// The approval API does not create the request: see [`Refused`].
    Refused(Refused),
}

/// The limit that a creation passed.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum Limit {
    PerKey,
    PerOwner,
    Total,
}

/// An answer of the owner, as the caller got it from a phone or from the web.
pub struct Answer {
    pub login: String,
    pub via: AnswerVia,
    pub action: Action,
    pub shown_effect: Option<String>,
    pub client_msg_id: Option<String>,
    /// The text or the choice of the owner, as the caller built it. The engine seals it.
    pub payload: Option<Zeroizing<Vec<u8>>>,
    /// `inbox.approvals.consume_within`.
    pub consume_within: std::time::Duration,
}

/// The result of an `answer`.
#[derive(Debug)]
pub enum Answered {
    NotFound,
    Refused {
        status: AnswerStatus,
        loaded: Box<Loaded>,
    },
    Accepted {
        loaded: Box<Loaded>,
        seq: u64,
    },
}

/// What `serve_body` finds.
pub enum Served {
    NotFound,
    /// The body is gone: `body_purge_after` passed, or the key was lost and purged.
    Purged,
    /// The seal does not open, or the effect of the opened command differs from the stored effect.
    Mismatch,
    Body(Zeroizing<Vec<u8>>),
}

/// A turn of a thread, unsealed.
pub struct TurnPlain {
    pub row: TurnRow,
    pub text: Option<Zeroizing<Vec<u8>>>,
}

/// An intent that the engine stores.
pub struct NewIntent<'a> {
    /// The digest of the `approval_ref`.
    pub digest: &'a str,
    pub key_id: &'a str,
    pub wait: Wait,
    pub allow_custom: bool,
    pub allow_ask_back: bool,
    pub previous_id: Option<&'a str>,
    /// The clear JSON that the engine seals.
    pub content: &'a [u8],
    pub ttl_ms: i64,
}

/// An intent that a submission reads, with its text unsealed.
pub struct IntentPlain {
    pub wait: Wait,
    pub allow_custom: bool,
    pub allow_ask_back: bool,
    pub previous_id: Option<String>,
    pub content: Zeroizing<Vec<u8>>,
}

/// The engine of the items, as the caller uses it.
pub struct Items<'a> {
    pub store: &'a Store,
    pub content: &'a ContentBox,
    pub rules: Rules,
    pub clock: &'a dyn Clock,
    /// The sessions to wake after a commit. A test passes `None`.
    pub hub: Option<&'a Hub>,
    pub replica: Option<&'a Replica>,
    pub metrics: Option<&'a InboxMetrics>,
    /// The agents that wait for an item wake up when it changes.
    pub changed: Option<&'a tokio::sync::Notify>,
    pub admin_body_read: bool,
}

impl Items<'_> {
    fn cx(&self) -> Cx {
        Cx {
            rules: self.rules,
            now: self.clock.now_ms(),
        }
    }

    fn writable(&self) -> Result<(), ItemError> {
        if self.store.is_degraded() {
            return Err(ItemError::Degraded);
        }
        Ok(())
    }

    // ------------------------------------------------------------ sealing

    fn seal(&self, plain: &[u8], aad: &[u8]) -> Result<Sealed, ItemError> {
        let ct = self.content.seal(plain, aad).map_err(ItemError::Seal)?;
        let kid = ContentBox::kid_of(&ct)
            .ok_or_else(|| ItemError::Seal("the sealed value has no key id".into()))?
            .to_string();
        Ok(Sealed { kid, ct })
    }

    fn seal_body(
        &self,
        request_id: &str,
        effect: Option<&str>,
        plain: &[u8],
    ) -> Result<Sealed, ItemError> {
        self.seal(plain, &content_box::aad(request_id, FIELD_BODY, 0, effect))
    }

    fn seal_turn(&self, request_id: &str, seq: u64, plain: &[u8]) -> Result<Sealed, ItemError> {
        self.seal(plain, &content_box::aad(request_id, FIELD_TURN, seq, None))
    }

    fn open_body(&self, l: &Loaded, sealed: &Sealed) -> Option<Zeroizing<Vec<u8>>> {
        let effect = l.approval.as_ref().map(|a| a.effect.as_str());
        self.content.open(
            &sealed.ct,
            &content_box::aad(&l.item.request_id, FIELD_BODY, 0, effect),
        )
    }

    // ------------------------------------------------------------ after a commit

    /// The log, the metrics and the wake of the sessions and of the agents. Nothing here holds a body.
    fn after(&self, fx: &Effects) {
        if let Some(hub) = self.hub {
            hub.wake(&fx.woken);
        }
        if let Some(changed) = self.changed {
            changed.notify_waiters();
        }
        for e in &fx.events {
            if let Some(m) = self.metrics {
                m.item_event(e.event);
            }
            let name = log_name(e.kind, e.event);
            log::info!(event = name, request.id = %e.request_id, actor = %e.actor);
        }
    }

    async fn replicate(&self) -> Result<(), ItemError> {
        let Some(replica) = self.replica else {
            return Ok(());
        };
        let Err(error) = replica.sync().await else {
            return Ok(());
        };
        if let Some(m) = self.metrics {
            m.replica_sync_failure();
        }
        log::warn!(event = "replica_sync_failed", error = %error);
        if replica.strict_alerts() {
            return Err(ItemError::Replica);
        }
        Ok(())
    }

    // ------------------------------------------------------------ creation

    /// Creates an item. `body` is the clear body: the engine seals it and keeps no clear copy.
    pub async fn create(&self, mut new: NewItem, body: &[u8]) -> Result<Created, ItemError> {
        self.writable()?;
        let request_id = random_id();
        let effect = new.approval.as_ref().map(|a| a.effect.clone());
        let sealed = self.seal_body(&request_id, effect.as_deref(), body)?;
        new.body_size = sealed.ct.len() as i64;
        let cx = self.cx();
        let done = in_tx!(
            self,
            [cx, request_id, new, sealed],
            tx,
            ops::create(tx, &cx, &request_id, &new, &sealed)
        )?;
        self.after(&done.fx);
        let created = match done.value {
            CreateOutcome::Created(l) => {
                if let Some(m) = self.metrics {
                    m.deliveries_created(done.fx.woken.len() as u64);
                }
                self.replicate().await?;
                Created::New(l)
            }
            CreateOutcome::Existing(l, stored) => {
                let same = l.item.kind == new.kind
                    && match (&l.approval, &new.approval) {
                        (Some(a), Some(b)) => a.effect == b.effect,
                        (None, None) => stored
                            .as_ref()
                            .and_then(|s| self.open_body(&l, s))
                            .is_some_and(|plain| plain.as_slice() == body),
                        _ => false,
                    };
                if same {
                    Created::Repeat(l)
                } else {
                    Created::Conflict
                }
            }
            CreateOutcome::BadWait(e) => Created::BadWait(e),
            CreateOutcome::NoDevice => Created::NoDevice,
            CreateOutcome::TooManyForKey => Created::TooMany(Limit::PerKey),
            CreateOutcome::TooManyForOwner => Created::TooMany(Limit::PerOwner),
            CreateOutcome::TooManyOpen => Created::TooMany(Limit::Total),
            CreateOutcome::Refused(r) => Created::Refused(r),
        };
        Ok(created)
    }

    /// Replaces an item with a new one in the same chain (see `ops::supersede`).
    pub async fn supersede(
        &self,
        old_id: &str,
        key_id: &str,
        mut new: NewItem,
        body: &[u8],
    ) -> Result<SupersedeOut, ItemError> {
        self.writable()?;
        let new_id = random_id();
        let effect = new.approval.as_ref().map(|a| a.effect.clone());
        let sealed = self.seal_body(&new_id, effect.as_deref(), body)?;
        new.body_size = sealed.ct.len() as i64;
        let cx = self.cx();
        let (old_id, key_id) = (old_id.to_string(), key_id.to_string());
        let done = in_tx!(
            self,
            [cx, old_id, key_id, new_id, new, sealed],
            tx,
            ops::supersede(tx, &cx, &old_id, &key_id, &new_id, &new, &sealed)
        )?;
        self.after(&done.fx);
        if matches!(done.value, SupersedeOut::Created { .. }) {
            self.replicate().await?;
        }
        Ok(done.value)
    }

    // ------------------------------------------------------------ reads

    /// An item as a caller may see it, after the clock decided its state. `None` for an item that does not exist or
    /// that the caller does not own.
    pub async fn get(&self, id: &str, caller: &Caller) -> Result<Option<Loaded>, ItemError> {
        let cx = self.cx();
        let (id, caller) = (id.to_string(), caller.clone());
        let done = in_tx!(self, [cx, id, caller], tx, ops::read(tx, &cx, &id, &caller))?;
        self.after(&done.fx);
        Ok(done.value)
    }

    pub async fn list(&self, filter: ListFilter) -> Result<Vec<Loaded>, ItemError> {
        let cx = self.cx();
        Ok(in_tx!(self, [cx, filter], tx, ops::list(tx, &cx, &filter))?.value)
    }

    /// The renewal of the lease by the requester key (see `ops::renew_lease`).
    pub async fn renew(&self, id: &str, key_id: &str) -> Result<Renewed, ItemError> {
        self.writable()?;
        let cx = self.cx();
        let (id, key_id) = (id.to_string(), key_id.to_string());
        let done = in_tx!(
            self,
            [cx, id, key_id],
            tx,
            ops::renew_lease(tx, &cx, &id, &key_id)
        )?;
        self.after(&done.fx);
        if let Some(m) = self.metrics {
            if let Renewed::Open { written: true, .. } = &done.value {
                m.item_renewal_written();
            }
        }
        if matches!(done.value, Renewed::Revived(_)) {
            self.replicate().await?;
        }
        Ok(done.value)
    }

    pub async fn cancel(&self, id: &str, by: CancelBy) -> Result<Cancelled, ItemError> {
        self.writable()?;
        let cx = self.cx();
        let id = id.to_string();
        let done = in_tx!(self, [cx, id, by], tx, ops::cancel(tx, &cx, &id, &by))?;
        self.after(&done.fx);
        Ok(done.value)
    }

    /// Cancels the open items of a key that was revoked (`reason` is `key_revoked`) or of an owner that was deleted
    /// (`owner_deleted`). The result is the number of items cancelled.
    pub async fn cancel_where(
        &self,
        key_id: Option<&str>,
        owner: Option<&str>,
        reason: &'static str,
    ) -> Result<usize, ItemError> {
        let rows = self
            .list(ListFilter {
                owner: owner.map(str::to_string),
                key_id: key_id.map(str::to_string),
                open: Some(true),
                limit: 100_000,
            })
            .await?;
        let mut n = 0;
        for l in rows {
            let by = CancelBy::System {
                reason,
                actor: "server".into(),
            };
            if let Cancelled::Done(_) = self.cancel(&l.item.request_id, by).await? {
                n += 1;
            }
        }
        Ok(n)
    }

    // ------------------------------------------------------------ turns and answers

    /// A `say` of the agent.
    pub async fn say(&self, id: &str, key_id: &str, text: &[u8]) -> Result<TurnOut, ItemError> {
        self.turn(id, Speaker::Agent(key_id.to_string()), None, text)
            .await
    }

    /// A `reply` of the owner, from a device or from the web (`device` is `None`).
    pub async fn reply(
        &self,
        id: &str,
        login: &str,
        device: Option<String>,
        client_msg_id: String,
        text: &[u8],
    ) -> Result<TurnOut, ItemError> {
        let speaker = Speaker::Owner {
            login: login.to_string(),
            device,
        };
        self.turn(id, speaker, Some(client_msg_id), text).await
    }

    async fn turn(
        &self,
        id: &str,
        speaker: Speaker,
        client_msg_id: Option<String>,
        text: &[u8],
    ) -> Result<TurnOut, ItemError> {
        self.writable()?;
        let caller = match &speaker {
            Speaker::Agent(k) => Caller::Agent(k.clone()),
            Speaker::Owner { login, .. } => Caller::Owner(login.clone()),
        };
        let Some(l) = self.get(id, &caller).await? else {
            return Ok(TurnOut::NotFound);
        };
        let mut seq = l.item.last_seq + 1;
        for _ in 0..5 {
            let input = TurnIn {
                speaker: speaker.clone(),
                payload: self.seal_turn(id, seq, text)?,
                expected_seq: seq,
                client_msg_id: client_msg_id.clone(),
            };
            let cx = self.cx();
            let id = id.to_string();
            let done = in_tx!(self, [cx, id, input], tx, ops::turn(tx, &cx, &id, &input))?;
            self.after(&done.fx);
            match done.value {
                // The thread moved between the read and the write: seal again for the next number.
                TurnOut::SeqMoved { last_seq } => seq = last_seq + 1,
                other => {
                    if matches!(other, TurnOut::Stored { .. }) {
                        self.replicate().await?;
                    }
                    return Ok(other);
                }
            }
        }
        Err(ItemError::Storage(DbError::Pg(
            "the thread moves too fast".into(),
        )))
    }

    /// The only answer of an item (see `ops::answer`).
    pub async fn answer(&self, id: &str, a: Answer) -> Result<Answered, ItemError> {
        self.writable()?;
        let Some(l) = self.get(id, &Caller::Owner(a.login.clone())).await? else {
            return Ok(Answered::NotFound);
        };
        let mut seq = l.item.last_seq + 1;
        for _ in 0..5 {
            let payload = match &a.payload {
                Some(plain) => Some(self.seal_turn(id, seq, plain)?),
                None => None,
            };
            let input = AnswerIn {
                login: a.login.clone(),
                via: a.via.clone(),
                action: a.action,
                shown_effect: a.shown_effect.clone(),
                client_msg_id: a.client_msg_id.clone(),
                payload,
                expected_seq: seq,
                consume_within_ms: a.consume_within.as_millis() as i64,
            };
            let cx = self.cx();
            let id = id.to_string();
            let done = in_tx!(self, [cx, id, input], tx, ops::answer(tx, &cx, &id, &input))?;
            self.after(&done.fx);
            match done.value {
                AnswerOut::SeqMoved { last_seq } => seq = last_seq + 1,
                AnswerOut::NotFound => return Ok(Answered::NotFound),
                AnswerOut::Refused { status, loaded } => {
                    if let Some(m) = self.metrics {
                        m.item_answer(super::items::status_str(status));
                    }
                    return Ok(Answered::Refused { status, loaded });
                }
                AnswerOut::Accepted { loaded, seq } => {
                    if let Some(m) = self.metrics {
                        m.item_answer("accepted");
                    }
                    // The answer is confirmed only after the copy of the replica holds it.
                    self.replicate().await?;
                    return Ok(Answered::Accepted { loaded, seq });
                }
            }
        }
        Err(ItemError::Storage(DbError::Pg(
            "the thread moves too fast".into(),
        )))
    }

    // ------------------------------------------------------------ the intents of the agents

    /// Stores an intent: what an agent says about its next approval. `content` is the clear JSON of the explanation and
    /// the hint: the engine seals it and keeps no clear copy. `digest` is the digest of the `approval_ref`; the
    /// reference itself is never kept. The result is the time at which the intent expires.
    pub async fn create_intent(&self, new: NewIntent<'_>) -> Result<i64, ItemError> {
        self.writable()?;
        let now = self.clock.now_ms();
        let expires_at = now + new.ttl_ms;
        let sealed = self.seal(
            new.content,
            &content_box::aad(new.digest, FIELD_INTENT, 0, None),
        )?;
        let (lease_s, until_at) = match new.wait {
            Wait::Lease { lease_s } => (Some(lease_s), None),
            Wait::Until { at_ms } => (None, Some(at_ms)),
        };
        let row = IntentRow {
            ref_digest: new.digest.to_string(),
            requester_key_id: new.key_id.to_string(),
            lease_s,
            until_at,
            allow_custom: new.allow_custom,
            allow_ask_back: new.allow_ask_back,
            body_size: sealed.ct.len() as i64,
            body: Some(sealed),
            expires_at,
            used_at: None,
            request_id: None,
            previous_id: new.previous_id.map(str::to_string),
        };
        in_tx!(self, [row], tx, tx.insert_intent(&row))?;
        Ok(expires_at)
    }

    /// The number of intents of a key that are not used and not expired.
    pub async fn live_intents(&self, key_id: &str) -> Result<i64, ItemError> {
        let (key_id, now) = (key_id.to_string(), self.clock.now_ms());
        in_tx!(self, [key_id, now], tx, tx.live_intents(&key_id, now))
    }

    /// The open approval that has this `key_hash`, after the clock settled it. `None` when none waits.
    pub async fn open_approval(&self, key_hash: &str) -> Result<Option<Loaded>, ItemError> {
        Ok(match self.status_for_pdp(key_hash).await? {
            Consumed::Pending(l) => Some(*l),
            _ => None,
        })
    }

    /// Reads an intent for the submission that names it. `None` when the digest is unknown, or the intent is of another
    /// key, used, expired, or its text does not open. The read does not use the intent: the creation does.
    pub async fn read_intent(
        &self,
        digest: &str,
        key_id: &str,
    ) -> Result<Option<IntentPlain>, ItemError> {
        let owned = digest.to_string();
        let Some(row) = in_tx!(self, [owned], tx, tx.find_intent(&owned))? else {
            return Ok(None);
        };
        if row.requester_key_id != key_id
            || row.used_at.is_some()
            || row.expires_at <= self.clock.now_ms()
        {
            return Ok(None);
        }
        let Some(sealed) = row.body else {
            return Ok(None);
        };
        let Some(content) = self
            .content
            .open(&sealed.ct, &content_box::aad(digest, FIELD_INTENT, 0, None))
        else {
            return Ok(None);
        };
        let wait = match (row.lease_s, row.until_at) {
            (Some(lease_s), None) => Wait::Lease { lease_s },
            (None, Some(at_ms)) => Wait::Until { at_ms },
            _ => return Ok(None),
        };
        Ok(Some(IntentPlain {
            wait,
            allow_custom: row.allow_custom,
            allow_ask_back: row.allow_ask_back,
            previous_id: row.previous_id,
            content,
        }))
    }

    // ------------------------------------------------------------ the approval of the PDP

    /// Uses the approval that fits `key_hash` (see `ops::consume`).
    pub async fn consume(&self, key_hash: &str) -> Result<Consumed, ItemError> {
        self.writable()?;
        let cx = self.cx();
        let key_hash = key_hash.to_string();
        let done = in_tx!(self, [cx, key_hash], tx, ops::consume(tx, &cx, &key_hash))?;
        self.after(&done.fx);
        Ok(done.value)
    }

    /// What the PDP finds for an effect, without consuming it (see `ops::status`). A question without `usage` asks
    /// this. The result is `Consumed::Usable` when the approval can be used.
    pub async fn status_for_pdp(&self, key_hash: &str) -> Result<Consumed, ItemError> {
        let cx = self.cx();
        let key_hash = key_hash.to_string();
        let done = in_tx!(self, [cx, key_hash], tx, ops::status(tx, &cx, &key_hash))?;
        self.after(&done.fx);
        Ok(done.value)
    }

    /// Gives back a consumed approval (see `ops::release`).
    pub async fn release(&self, id: &str, consumed_at: i64) -> Result<bool, ItemError> {
        self.writable()?;
        let cx = self.cx();
        let id = id.to_string();
        let done = in_tx!(
            self,
            [cx, id, consumed_at],
            tx,
            ops::release(tx, &cx, &id, consumed_at)
        )?;
        self.after(&done.fx);
        Ok(done.value)
    }

    // ------------------------------------------------------------ the bodies

    /// The clear body of an item, for the owner that the caller authenticated. It checks the seal, and for an approval
    /// it computes the effect of the opened command again: on a difference it serves nothing.
    pub async fn serve_body(&self, l: &Loaded) -> Result<Served, ItemError> {
        let id = l.item.request_id.clone();
        let sealed = in_tx!(self, [id], tx, tx.sealed_body(&id))?;
        let Some(sealed) = sealed else {
            return Ok(if l.item.body_purged_at.is_some() {
                Served::Purged
            } else {
                Served::NotFound
            });
        };
        let mismatch = |why: &str| {
            if let Some(m) = self.metrics {
                m.item_body_effect_mismatch();
            }
            log::error!(event = "body_effect_mismatch", request.id = %id, reason = why);
            Ok(Served::Mismatch)
        };
        let Some(plain) = self.open_body(l, &sealed) else {
            return mismatch("the seal does not open");
        };
        if let Some(a) = &l.approval {
            let op = serde_json::from_slice::<serde_json::Value>(&plain)
                .ok()
                .and_then(|v| v.get("op").cloned());
            let effect = op.and_then(|op| {
                inbox_core::effect::effect_hash(&a.service, &a.action, &a.resource_path, &op).ok()
            });
            if effect.as_deref() != Some(a.effect.as_str()) {
                return mismatch("the effect of the command differs");
            }
        }
        if let Some(m) = self.metrics {
            m.item_body_served();
        }
        log::info!(event = "body_served", request.id = %id);
        Ok(Served::Body(plain))
    }

    /// The thread of an item with the texts unsealed. A turn that does not open has no text.
    pub async fn serve_turns(&self, id: &str) -> Result<Vec<TurnPlain>, ItemError> {
        let owned = id.to_string();
        let rows = in_tx!(self, [owned], tx, tx.turns(&owned))?;
        Ok(rows
            .into_iter()
            .map(|row| {
                let text = row.payload.as_ref().and_then(|p| {
                    self.content
                        .open(&p.ct, &content_box::aad(id, FIELD_TURN, row.seq, None))
                });
                TurnPlain { row, text }
            })
            .collect())
    }

    // ------------------------------------------------------------ the phones

    /// The deliveries of items that the device has not received, oldest first.
    pub async fn pending_item_deliveries(
        &self,
        device_id: &str,
        limit: u32,
    ) -> Result<Vec<ItemDelivery>, ItemError> {
        let device_id = device_id.to_string();
        in_tx!(
            self,
            [device_id],
            tx,
            tx.pending_item_deliveries(&device_id, limit)
        )
    }

    /// Gives a phone the open items of its owner that it never got (see `ItemTx::catch_up_device`). The result is the
    /// number of new deliveries.
    pub async fn catch_up_device(&self, device_id: &str, owner: &str) -> Result<u64, ItemError> {
        self.writable()?;
        let (device_id, owner) = (device_id.to_string(), owner.to_string());
        in_tx!(
            self,
            [device_id, owner],
            tx,
            tx.catch_up_device(&device_id, &owner)
        )
    }

    /// The newest earlier approval of the same effect that the owner accepted: the time of the answer and whether the
    /// service used it.
    pub async fn previous_approval(&self, l: &Loaded) -> Result<Option<(i64, bool)>, ItemError> {
        let Some(a) = &l.approval else {
            return Ok(None);
        };
        let (hash, id) = (a.key_hash.clone(), l.item.request_id.clone());
        in_tx!(self, [hash, id], tx, tx.previous_approval(&hash, &id))
    }

    /// The preferences of an account.
    pub async fn prefs(&self, login: &str) -> Result<Prefs, ItemError> {
        let login = login.to_string();
        in_tx!(self, [login], tx, tx.prefs(&login))
    }

    /// Replaces the preferences of an account. The server stores them and applies nothing here.
    pub async fn set_prefs(&self, login: &str, prefs: &Prefs) -> Result<(), ItemError> {
        self.writable()?;
        let (login, prefs) = (login.to_string(), prefs.clone());
        in_tx!(self, [login, prefs], tx, tx.put_prefs(&login, &prefs))?;
        self.replicate().await
    }

    /// Writes the event `body_read`: a superadmin read a body (`inbox.at_rest.admin_body_read`). It holds the login, the
    /// time and the request id, never the body.
    pub async fn record_body_read(
        &self,
        id: &str,
        kind: Kind,
        login: &str,
    ) -> Result<(), ItemError> {
        let cx = self.cx();
        let event = NewEvent {
            request_id: id.to_string(),
            kind,
            at: cx.now,
            event: "body_read",
            actor: format!("user:{login}"),
            detail: None,
        };
        in_tx!(self, [event], tx, tx.insert_event(&event))?;
        if let Some(m) = self.metrics {
            m.item_body_read();
        }
        log::info!(event = "body_read", request.id = %id, actor = %login);
        Ok(())
    }

    // ------------------------------------------------------------ the sweeper

    /// One pass of the sweeper.
    pub async fn sweep(&self) -> Result<SweepReport, ItemError> {
        self.writable()?;
        let cx = self.cx();
        let done = in_tx!(self, [cx], tx, ops::sweep(tx, &cx, 200))?;
        self.after(&done.fx);
        if done.value.extended > 0 {
            log::warn!(
                event = "leases_extended",
                count = done.value.extended,
                reason = "the sweeper was away"
            );
        }
        Ok(done.value)
    }

    /// The retention: bodies, items and events.
    pub async fn maintain(&self) -> Result<MaintainReport, ItemError> {
        self.writable()?;
        let cx = self.cx();
        Ok(in_tx!(self, [cx], tx, ops::maintain(tx, &cx))?)
    }
}

// ---------------------------------------------------------------- the start

/// The checks of the start. A database made by a version 5.0 has no table of the items: the start stops and names
/// the table. A sealed content whose key left the file stops the start with the key id and the count.
pub async fn start_checks(store: &Store, content: &ContentBox) -> Result<(), String> {
    let at = StoreRef { store };
    let missing =
        in_tx!(at, [], tx, tx.missing_table()).map_err(|e| format!("inbox storage: {e}"))?;
    if let Some(table) = missing {
        return Err(format!(
            "the inbox database was made by a version before 5.1.0 and has no table {table}: this portal has no migration for it; make a new database (see README, Upgrades, version 5.1.0)"
        ));
    }
    let counts = in_tx!(at, [], tx, tx.kid_counts()).map_err(|e| format!("inbox storage: {e}"))?;
    content.check_orphans(&counts)
}

/// Erases the sealed values of the key `kid` and keeps the metadata (`portal inbox content-purge`). The result is the
/// rows of the items, of the turns and of the intents.
pub async fn purge_key(store: &Store, kid: &str) -> Result<(u64, u64, u64), String> {
    let at = StoreRef { store };
    let (kid, now) = (kid.to_string(), super::util::now_ms());
    in_tx!(at, [kid, now], tx, tx.purge_key(&kid, now)).map_err(|e| format!("inbox storage: {e}"))
}

// ---------------------------------------------------------------- seeds for the tests of the PDP

/// An approval in any state, for the tests of the PDP: the row is written as it is, without the rules of the
/// operations. The conformance data of the protocol gives approvals in each state.
#[cfg(test)]
pub struct SeedApproval {
    pub key_id: String,
    pub owner: String,
    pub service: String,
    /// The subject of the question, such as `key:conf0001`.
    pub subject: String,
    pub action: String,
    pub resource_path: String,
    pub effect: String,
    pub state: inbox_core::proto::ItemState,
    pub decision: Option<inbox_core::proto::Decision>,
    pub answered_at: Option<i64>,
    pub consume_before: Option<i64>,
    pub consumed_at: Option<i64>,
    pub lease_s: i64,
}

#[cfg(test)]
impl Items<'_> {
    /// Writes one approval and returns its request id.
    pub async fn seed_approval(&self, s: SeedApproval) -> Result<String, ItemError> {
        use super::items::{approval_key_hash, ItemRow};
        use inbox_core::proto::{Decision, Interaction, ItemState, Urgency, Via};
        let now = self.clock.now_ms();
        let request_id = random_id();
        let answered = s.state == ItemState::Answered;
        let open = matches!(s.state, ItemState::Pending | ItemState::Replied);
        let item = ItemRow {
            request_id: request_id.clone(),
            kind: Kind::Approval,
            chain_id: request_id.clone(),
            supersedes: None,
            superseded_by: None,
            previous_id: None,
            requester_key_id: s.key_id.clone(),
            requester_owner: s.owner.clone(),
            via: Via::Mcp,
            interaction: Interaction::Conversational,
            urgency: Urgency::Normal,
            input_type: None,
            state: s.state,
            settle_reason: (!open).then(|| "seeded".to_string()),
            body_kid: None,
            body_size: 0,
            body_purged_at: None,
            created_at: now,
            chain_created_at: now,
            lease_s: Some(s.lease_s),
            lease_until: Some(now + 3 * s.lease_s * 1000),
            until_at: None,
            agent_seen_at: now,
            last_agent_turn_at: None,
            abandoned_at: (s.state == ItemState::Abandoned).then_some(now),
            revivals: 0,
            last_seq: 0,
            allow_custom: false,
            allow_ask_back: false,
            answer_action: s.decision.map(|d| {
                if d == Decision::Approve {
                    Action::Accept
                } else {
                    Action::Decline
                }
            }),
            answered_at: answered.then(|| s.answered_at.unwrap_or(now)),
            answered_by: answered.then(|| s.owner.clone()),
            answered_via: answered.then(|| "device:seeded-device".to_string()),
            final_at: (!open).then_some(now),
            idempotency_key: random_id(),
        };
        let approval = super::items::ApprovalRow {
            request_id: request_id.clone(),
            service: s.service.clone(),
            subject: s.subject.clone(),
            action: s.action.clone(),
            resource_path: s.resource_path.clone(),
            effect: s.effect.clone(),
            key_hash: approval_key_hash(
                &s.service,
                &s.subject,
                &s.action,
                &s.resource_path,
                &s.effect,
            ),
            evaluation_id: None,
            code: super::items::new_code(),
            decision: s.decision,
            shown_effect: answered.then(|| s.effect.clone()),
            consume_before: s.consume_before,
            consumed_at: s.consumed_at,
            lapsed_at: None,
        };
        let l = Loaded {
            item,
            approval: Some(approval),
        };
        let sealed = self.seal_body(&request_id, Some(&s.effect), b"{\"op\":{},\"view\":{}}")?;
        in_tx!(self, [l, sealed], tx, tx.insert(&l, Some(&sealed)))?;
        Ok(request_id)
    }
}

/// Raw SQL for the tests of the layers above the engine: the tables are those of the inbox database.
#[cfg(test)]
impl Items<'_> {
    /// A phone of `login` that is not revoked.
    pub async fn seed_device(&self, id: &str, login: &str) -> Result<(), ItemError> {
        match self.store {
            Store::Sqlite(db) => {
                let (id, login) = (id.to_string(), login.to_string());
                db.call(move |c| {
                    c.execute(
                        "INSERT INTO devices (id, name, user_name, public_key, enrolled_at, enrolled_ip)
                         VALUES (?1, 'phone', ?2, x'00', 1, 'ip')",
                        rusqlite::params![id, login],
                    )?;
                    Ok(())
                })
                .await
                .map_err(ItemError::from)
            }
            Store::Pg(pg) => pg
                .guard(async {
                    let c = pg.client().await?;
                    c.execute(
                        "INSERT INTO devices (id, name, user_name, public_key, enrolled_at, enrolled_ip)
                         VALUES ($1, 'phone', $2, '\\x00'::bytea, 1, 'ip')",
                        &[&id, &login],
                    )
                    .await
                    .map_err(pg::db_error)?;
                    Ok(())
                })
                .await
                .map_err(ItemError::from),
        }
    }

    /// The first number of the result of a `SELECT`.
    pub async fn test_count(&self, sql: &str) -> i64 {
        match self.store {
            Store::Sqlite(db) => {
                let sql = sql.to_string();
                db.call(move |c| Ok(c.query_row(&sql, [], |r| r.get(0))?))
                    .await
                    .unwrap()
            }
            Store::Pg(pg) => {
                let c = pg.client().await.unwrap();
                c.query_one(sql, &[]).await.unwrap().get(0)
            }
        }
    }

    /// Runs a statement. The error is the text of the database error.
    pub async fn test_exec(&self, sql: &str) -> Result<(), String> {
        match self.store {
            Store::Sqlite(db) => {
                let sql = sql.to_string();
                db.call(move |c| Ok(c.execute_batch(&sql)?))
                    .await
                    .map_err(|e| e.to_string())
            }
            Store::Pg(pg) => {
                let c = pg.client().await.map_err(|e| e.to_string())?;
                c.execute(sql, &[])
                    .await
                    .map(|_| ())
                    .map_err(|e| e.to_string())
            }
        }
    }

    /// The row of an intent, as the database holds it.
    pub async fn test_intent(&self, digest: &str) -> Option<IntentRow> {
        let digest = digest.to_string();
        in_tx!(self, [digest], tx, tx.find_intent(&digest)).unwrap()
    }
}
