//! The items for the rest of the portal: the calls that the approval API, the agent channel, the phones and the web
//! use, on SQLite or PostgreSQL.
//!
//! This module seals what it writes and unseals what it serves. The operations (`items_ops.rs`) never see a clear
//! body: they get sealed values. Only the functions `serve_*` of this module unseal, and each of them checks the
//! seal. The clock, the box and the limits come from the caller, so a test builds the engine without a server.
//! An item never uses the spool: when PostgreSQL has no primary, a write gets [`ItemError::Degraded`].

// The approval API, the agent channel, the phones and the web call this API in the next lots.
#![allow(dead_code)]

use inbox_core::proto::{Action, AnswerStatus, Kind};
use zeroize::Zeroizing;

use super::content_box::{self, ContentBox, FIELD_BODY, FIELD_TURN};
use super::db::DbError;
use super::hub::Hub;
use super::items::{Caller, Clock, Effects, Loaded, NewEvent, Rules};
use super::items_db;
use super::items_ops::{
    self as ops, AnswerIn, AnswerOut, AnswerVia, CancelBy, Cancelled, Consumed, CreateOutcome, Cx,
    ItemTx, ListFilter, MaintainReport, NewItem, Renewed, Sealed, Speaker, SupersedeOut,
    SweepReport, TurnIn, TurnOut, TurnRow,
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
