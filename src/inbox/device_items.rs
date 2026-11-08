//! The items on the side of a phone: the frames that the server builds when it sends a delivery, and the handling of
//! `answer`, `reply`, `refresh`, `sync` and `set_prefs`.
//!
//! The rows of `alerts` for an item hold metadata only (kind, number of the turn). When the session sends such a
//! delivery, this module reads the item, unseals the body or the turn, and builds the frame (`frames.rs`). The
//! unsealed text lives for the time of one flush and is never logged.
//!
//! # The order of the frames
//!
//! The queue of a device is ordered by the number of the delivery. A `request` comes before the `settled` of its item,
//! and a phone that is offline gets both in this order at its next session. A phone that gets a `settled` before its
//! `request` (a lost receipt, a retransmission) keeps the `settled` and applies it when the `request` arrives.
//!
//! # What the server does not send
//!
//! A delivery that the server cannot build is withdrawn, never sent half. An item that is `abandoned` when its first
//! delivery is due is not sent to a phone that never got it (SPEC 4.11.2). A `settled` for an item that is open again
//! (a revival) is withdrawn: it is out of date.

use std::collections::{HashMap, HashSet};
use std::time::Duration;

use inbox_core::noise::Session;
use inbox_core::proto::{
    close, Action, AnswerMsg, AnswerResultMsg, AnswerStatus, ItemState, Prefs, ReplyMsg, ServerMsg,
};
use zeroize::Zeroizing;

use super::device::{send_encoded, send_msg, End};
use super::frames::{self, ApprovalFacts, OwnerAnswer};
use super::items::{closed_status, Caller, Loaded};
use super::items_ops::{AnswerVia, ItemDelivery, ListFilter, TurnOut};
use super::items_store::{Answer, Answered, ItemError, Items, Served, TurnPlain};
use super::log;
use super::state::App;
use super::util::{now_ms, random_id};
use super::ws::Socket;

/// The window of an approval when the file has no block `inbox.approvals` (a question has no consumption).
const DEFAULT_CONSUME_WITHIN: Duration = Duration::from_secs(300);

/// The most open items that one `sync` can bring back.
const SYNC_LIMIT: u32 = 1000;

/// What the builder makes of one delivery.
pub(crate) enum Built {
    /// The encoded frame, ready to encrypt.
    Frame(Vec<u8>),
    /// The delivery is not sent and not kept: the reason is for the log.
    Withdraw(&'static str),
    /// The delivery stays in the queue for the next flush: the engine is not ready.
    Keep,
}

/// Reads the items of one flush once and builds the frames of their deliveries.
pub(crate) struct Builder<'a> {
    app: &'a App,
    items: Items<'a>,
    login: &'a str,
    loaded: HashMap<String, Option<Loaded>>,
    turns: HashMap<String, Vec<TurnPlain>>,
    /// The items whose first request is still in the queue: the phone never got them.
    unseen: HashSet<String>,
}

fn encode(msg: &ServerMsg) -> Built {
    match msg.encode() {
        Ok(bytes) => Built::Frame(bytes),
        Err(e) => {
            log::error!(event = "item_frame_refused", error = %e);
            Built::Withdraw("frame_not_valid")
        }
    }
}

impl<'a> Builder<'a> {
    /// `None` when the server has no items. `queue` is the part of the queue that the flush will send.
    pub(crate) fn new(app: &'a App, login: &'a str, queue: &[ItemDelivery]) -> Option<Builder<'a>> {
        Some(Builder {
            app,
            items: app.items()?,
            login,
            loaded: HashMap::new(),
            turns: HashMap::new(),
            unseen: queue
                .iter()
                .filter(|d| d.kind == "question" || d.kind == "approval")
                .map(|d| d.request_id.clone())
                .collect(),
        })
    }

    async fn load(&mut self, request_id: &str) -> Result<Option<Loaded>, ItemError> {
        if let Some(known) = self.loaded.get(request_id) {
            return Ok(known.clone());
        }
        let found = self
            .items
            .get(request_id, &Caller::Owner(self.login.to_string()))
            .await?;
        self.loaded.insert(request_id.to_string(), found.clone());
        Ok(found)
    }

    /// The frame of one delivery of the queue.
    pub(crate) async fn delivery(&mut self, d: &ItemDelivery) -> Result<Built, ItemError> {
        let Some(l) = self.load(&d.request_id).await? else {
            return Ok(Built::Withdraw("item_gone"));
        };
        // An abandoned item is not sent to a phone that never got it. The agent can come back: a revival makes a new
        // request message.
        if l.item.state == ItemState::Abandoned && self.unseen.contains(&d.request_id) {
            return Ok(Built::Withdraw("abandoned_before_first_delivery"));
        }
        match d.kind.as_str() {
            "question" | "approval" => self.request(&l, &d.alert_id).await,
            "turn" => self.message(&l, d.item_seq, &d.alert_id).await,
            "settled" if !l.item.state.is_final() => Ok(Built::Withdraw("item_open_again")),
            "settled" => self.settled(&l, &d.alert_id).await,
            _ => Ok(Built::Withdraw("unknown_kind")),
        }
    }

    /// The `request` frame of an item with the current state of the thread.
    pub(crate) async fn request(&mut self, l: &Loaded, id: &str) -> Result<Built, ItemError> {
        let plain = match self.items.serve_body(l).await? {
            Served::Body(plain) => plain,
            Served::Purged => return Ok(Built::Withdraw("body_purged")),
            Served::NotFound | Served::Mismatch => return Ok(Built::Withdraw("body_not_served")),
        };
        let facts = ApprovalFacts {
            previous: self.items.previous_approval(l).await?,
            follows: self.follows(l).await?,
        };
        let key_name = self
            .app
            .authorizer()
            .and_then(|a| a.key_name(&l.item.requester_key_id));
        let from = frames::sender(&l.item, key_name.as_deref());
        match frames::request_frame(id, l, &plain, from, &facts) {
            Ok(frame) => Ok(encode(&ServerMsg::Request(Box::new(frame)))),
            Err(e) => {
                log::error!(event = "item_frame_refused", request.id = %l.item.request_id, error = %e);
                Ok(Built::Withdraw("frame_not_built"))
            }
        }
    }

    /// The earlier item that the owner declined with a text, for the field `follows`.
    async fn follows(
        &mut self,
        l: &Loaded,
    ) -> Result<Option<(String, inbox_core::proto::Intent)>, ItemError> {
        let Some(previous) = l.item.previous_id.clone() else {
            return Ok(None);
        };
        let Some(old) = self.load(&previous).await? else {
            return Ok(None);
        };
        let Some(code) = old
            .approval
            .as_ref()
            .filter(|a| a.decision == Some(inbox_core::proto::Decision::Deny))
            .map(|a| a.code.clone())
        else {
            return Ok(None);
        };
        let turns = self.items.serve_turns(&previous).await?;
        let intent = turns
            .iter()
            .filter(|t| t.row.kind == "decline")
            .filter_map(|t| t.text.as_ref())
            .map(|text| OwnerAnswer::from_plain(text))
            .find_map(|a| a.intent.filter(|_| a.text.is_some()));
        Ok(intent.map(|i| (code, i)))
    }

    async fn message(&mut self, l: &Loaded, seq: u64, id: &str) -> Result<Built, ItemError> {
        let request_id = &l.item.request_id;
        if !self.turns.contains_key(request_id) {
            let turns = self.items.serve_turns(request_id).await?;
            self.turns.insert(request_id.clone(), turns);
        }
        let Some(turn) = self.turns[request_id].iter().find(|t| t.row.seq == seq) else {
            return Ok(Built::Withdraw("turn_gone"));
        };
        let Some(text) = &turn.text else {
            return Ok(Built::Withdraw("turn_not_served"));
        };
        match frames::message_frame(id, &turn.row, text) {
            Ok(frame) => Ok(encode(&ServerMsg::Message(frame))),
            Err(e) => {
                log::error!(event = "item_frame_refused", request.id = %request_id, error = %e);
                Ok(Built::Withdraw("frame_not_built"))
            }
        }
    }

    /// The `settled` frame of a final item.
    pub(super) async fn settled(&mut self, l: &Loaded, id: &str) -> Result<Built, ItemError> {
        let device = l
            .item
            .answered_via
            .as_deref()
            .and_then(|v| v.strip_prefix("device:"))
            .map(str::to_string);
        let name = match device {
            Some(device) => self
                .app
                .db
                .find_device(&device)
                .await
                .ok()
                .flatten()
                .map(|d| d.name),
            None => None,
        };
        Ok(match frames::settled_frame(id, l, name.as_deref()) {
            Some(frame) => encode(&ServerMsg::Settled(frame)),
            None => Built::Withdraw("item_open_again"),
        })
    }
}

// ---------------------------------------------------------------- the session

/// What the `welcome` says about the items: the preferences of the account and the age at which a body is erased.
/// A server without items says 0, and the phone then sends none of the frames of items.
pub(super) async fn welcome_parts(app: &App, login: &str) -> (Box<Prefs>, u64) {
    let Some(items) = app.items() else {
        return (Box::default(), 0);
    };
    let prefs = match items.prefs(login).await {
        Ok(p) => p,
        Err(e) => {
            log::warn!(event = "prefs_read_failed", error = %e);
            Prefs::default()
        }
    };
    (Box::new(prefs), items.rules.body_purge_ms.max(1) as u64)
}

/// Gives a new phone, and a phone that comes back, the open items of its owner that it never got.
pub(super) async fn catch_up(app: &App, device_id: &str, login: &str) {
    let Some(items) = app.items() else {
        return;
    };
    match items.catch_up_device(device_id, login).await {
        Ok(0) | Err(ItemError::Degraded) => {}
        Ok(n) => log::info!(event = "items_caught_up", device.id = %device_id, count = n),
        Err(e) => log::warn!(event = "items_catch_up_failed", device.id = %device_id, error = %e),
    }
}

/// The deliveries of items that wait for the device. Without the engine, or while PostgreSQL has no primary, none.
pub(super) async fn pending(
    app: &App,
    device_id: &str,
    limit: u32,
) -> Result<Vec<ItemDelivery>, End> {
    let Some(items) = app.items() else {
        return Ok(Vec::new());
    };
    match items.pending_item_deliveries(device_id, limit).await {
        Ok(rows) => Ok(rows),
        Err(ItemError::Degraded) => Ok(Vec::new()),
        Err(ItemError::Storage(e)) => Err(End::Storage(e)),
        Err(e) => {
            log::error!(event = "item_queue_failed", error = %e);
            Ok(Vec::new())
        }
    }
}

/// The digest of an owner: the count and the age of the open items. The row of the queue holds no number.
pub(super) async fn digest(app: &App, login: &str, id: &str) -> Result<Built, End> {
    let Some(items) = app.items() else {
        return Ok(Built::Withdraw("no_items"));
    };
    let filter = ListFilter {
        owner: Some(login.to_string()),
        open: Some(true),
        limit: SYNC_LIMIT,
        ..ListFilter::default()
    };
    match items.list(filter).await {
        Ok(open) => Ok(match frames::digest_frame(id, &open) {
            Some(frame) => encode(&ServerMsg::Digest(frame)),
            None => Built::Withdraw("nothing_open"),
        }),
        Err(ItemError::Degraded | ItemError::Replica) => Ok(Built::Keep),
        Err(e) => Err(storage(e)),
    }
}

fn storage(e: ItemError) -> End {
    match e {
        ItemError::Storage(e) => End::Storage(e),
        other => {
            log::error!(event = "item_store_failed", error = %other);
            End::Storage(super::db::DbError::Closed)
        }
    }
}

/// A change that needs the engine while it is not ready: the phone is told, and the session goes on. The outbox of
/// the phone sends the frame again.
async fn unavailable(socket: &mut Socket, session: &mut Session) -> Result<bool, End> {
    send_msg(
        socket,
        session,
        &ServerMsg::Error {
            code: "unavailable".into(),
            message: "the items are not ready, retry in a moment".into(),
        },
    )
    .await?;
    Ok(false)
}

async fn not_enabled(socket: &mut Socket, session: &mut Session) -> Result<bool, End> {
    send_msg(
        socket,
        session,
        &ServerMsg::Error {
            code: "not_enabled".into(),
            message: "items are not enabled on this server".into(),
        },
    )
    .await?;
    Ok(false)
}

async fn send_result(
    socket: &mut Socket,
    session: &mut Session,
    request_id: &str,
    client_msg_id: &str,
    status: AnswerStatus,
    state: Option<ItemState>,
) -> Result<(), End> {
    let msg = ServerMsg::AnswerResult(AnswerResultMsg {
        request_id: request_id.to_string(),
        client_msg_id: client_msg_id.to_string(),
        status,
        state,
    });
    let bytes = msg.encode().map_err(|_| End::Protocol("answer_result"))?;
    send_encoded(socket, session, &bytes).await
}

/// After `device_revoked` the session ends: the phone must enroll again.
fn after(status: AnswerStatus) -> Result<bool, End> {
    if status == AnswerStatus::DeviceRevoked {
        return Err(End::Code(close::REVOKED, "device revoked"));
    }
    Ok(true)
}

/// `answer`: the only answer of an item, through the engine.
pub(super) async fn answer(
    app: &App,
    socket: &mut Socket,
    session: &mut Session,
    device_id: &str,
    login: &str,
    a: AnswerMsg,
) -> Result<bool, End> {
    let Some(items) = app.items() else {
        return not_enabled(socket, session).await;
    };
    let owner_answer = OwnerAnswer::from_msg(&a);
    let payload = (!owner_answer.is_empty()).then(|| Zeroizing::new(owner_answer.to_plain()));
    let given = Answer {
        login: login.to_string(),
        via: AnswerVia::Device(device_id.to_string()),
        action: a.action,
        shown_effect: a.shown_effect.clone(),
        client_msg_id: Some(a.client_msg_id.clone()),
        payload,
        consume_within: app
            .config
            .approvals
            .as_ref()
            .map_or(DEFAULT_CONSUME_WITHIN, |c| c.consume_within),
    };
    let (status, state) = match items.answer(&a.request_id, given).await {
        Ok(Answered::Accepted { loaded, .. }) => (AnswerStatus::Accepted, Some(loaded.item.state)),
        Ok(Answered::Refused { status, loaded }) => (status, Some(loaded.item.state)),
        Ok(Answered::NotFound) => (AnswerStatus::Closed, None),
        Err(ItemError::Degraded | ItemError::Replica) => return unavailable(socket, session).await,
        Err(e) => return Err(storage(e)),
    };
    log::info!(
        event = "phone_answer",
        device.id = %device_id,
        request.id = %a.request_id,
        action = if a.action == Action::Accept { "accept" } else { "decline" },
        status = super::items::status_str(status)
    );
    send_result(
        socket,
        session,
        &a.request_id,
        &a.client_msg_id,
        status,
        state,
    )
    .await?;
    after(status)
}

/// `reply`: the owner asks the agent. The item stays open and moves to `replied`.
pub(super) async fn reply(
    app: &App,
    socket: &mut Socket,
    session: &mut Session,
    device_id: &str,
    login: &str,
    r: ReplyMsg,
) -> Result<bool, End> {
    let Some(items) = app.items() else {
        return not_enabled(socket, session).await;
    };
    let turn = items
        .reply(
            &r.request_id,
            login,
            Some(device_id.to_string()),
            r.client_msg_id.clone(),
            r.text.as_bytes(),
        )
        .await;
    let (status, state) = match turn {
        Ok(TurnOut::Stored { loaded, .. }) => (AnswerStatus::Accepted, Some(loaded.item.state)),
        Ok(TurnOut::Duplicate { .. }) => {
            // The same `client_msg_id` came again: the thread holds the turn already.
            let state = items
                .get(&r.request_id, &Caller::Owner(login.to_string()))
                .await
                .map_err(storage)?
                .map(|l| l.item.state);
            (AnswerStatus::Accepted, state)
        }
        Ok(TurnOut::Closed(l)) => (closed_status(l.item.state), Some(l.item.state)),
        Ok(TurnOut::DeviceRevoked) => (AnswerStatus::DeviceRevoked, None),
        Ok(TurnOut::AskBackNotAllowed | TurnOut::TooManyTurns | TurnOut::TooFast) => {
            (AnswerStatus::AskBackNotAllowed, None)
        }
        Ok(TurnOut::NotFound) => (AnswerStatus::Closed, None),
        Ok(TurnOut::SeqMoved { .. }) | Err(ItemError::Degraded | ItemError::Replica) => {
            return unavailable(socket, session).await
        }
        Err(e) => return Err(storage(e)),
    };
    send_result(
        socket,
        session,
        &r.request_id,
        &r.client_msg_id,
        status,
        state,
    )
    .await?;
    after(status)
}

/// Sends an item to the phone in the state that the server holds: `request` while it is open, `settled` when it is
/// final. The frame has a new delivery id: the phone ignores a repeated id, and it must read this one.
async fn send_state(
    builder: &mut Builder<'_>,
    socket: &mut Socket,
    session: &mut Session,
    l: &Loaded,
) -> Result<(), End> {
    let id = random_id();
    let built = if l.item.state.is_final() {
        builder.settled(l, &id).await
    } else {
        builder.request(l, &id).await
    };
    match built {
        Ok(Built::Frame(bytes)) => send_encoded(socket, session, &bytes).await,
        Ok(Built::Withdraw(why)) => {
            log::info!(event = "item_state_not_sent", request.id = %l.item.request_id, reason = why);
            Ok(())
        }
        Ok(Built::Keep) | Err(ItemError::Degraded) => Ok(()),
        Err(e) => Err(storage(e)),
    }
}

/// The item of a request id that the server does not hold any more: its retention ended, so it is final.
async fn send_gone(
    socket: &mut Socket,
    session: &mut Session,
    request_id: &str,
) -> Result<(), End> {
    let frame = frames::gone_frame(&random_id(), request_id, now_ms());
    let bytes = ServerMsg::Settled(frame)
        .encode()
        .map_err(|_| End::Protocol("settled"))?;
    send_encoded(socket, session, &bytes).await
}

/// `refresh`: the current state of one item.
pub(super) async fn refresh(
    app: &App,
    socket: &mut Socket,
    session: &mut Session,
    login: &str,
    request_id: &str,
) -> Result<bool, End> {
    let Some(mut builder) = Builder::new(app, login, &[]) else {
        return not_enabled(socket, session).await;
    };
    match builder.load(request_id).await {
        Ok(Some(l)) => send_state(&mut builder, socket, session, &l).await?,
        Ok(None) => send_gone(socket, session, request_id).await?,
        Err(ItemError::Degraded | ItemError::Replica) => return unavailable(socket, session).await,
        Err(e) => return Err(storage(e)),
    }
    Ok(false)
}

/// `sync`: the phone says which items it holds open. The server sends the open items that it lacks or knows in an
/// older state, and the `settled` frame of each item that is final.
pub(super) async fn sync(
    app: &App,
    socket: &mut Socket,
    session: &mut Session,
    login: &str,
    open: Vec<inbox_core::proto::OpenRef>,
) -> Result<bool, End> {
    let Some(mut builder) = Builder::new(app, login, &[]) else {
        return not_enabled(socket, session).await;
    };
    let held: HashMap<&str, u64> = open
        .iter()
        .map(|o| (o.request_id.as_str(), o.seq))
        .collect();
    let filter = ListFilter {
        owner: Some(login.to_string()),
        open: Some(true),
        limit: SYNC_LIMIT,
        ..ListFilter::default()
    };
    let mut still_open = match builder.items.list(filter).await {
        Ok(open) => open,
        Err(ItemError::Degraded | ItemError::Replica) => return unavailable(socket, session).await,
        Err(e) => return Err(storage(e)),
    };
    // The oldest first: the list comes newest first.
    still_open.reverse();
    let open_ids: HashSet<String> = still_open
        .iter()
        .map(|l| l.item.request_id.clone())
        .collect();
    for l in &still_open {
        let known = held.get(l.item.request_id.as_str());
        if known.is_some_and(|seq| *seq >= l.item.last_seq) {
            continue;
        }
        send_state(&mut builder, socket, session, l).await?;
    }
    // The items that the phone holds open and that are not open any more.
    for o in &open {
        if open_ids.contains(&o.request_id) {
            continue;
        }
        match builder.load(&o.request_id).await {
            Ok(Some(l)) => send_state(&mut builder, socket, session, &l).await?,
            Ok(None) => send_gone(socket, session, &o.request_id).await?,
            Err(ItemError::Degraded | ItemError::Replica) => break,
            Err(e) => return Err(storage(e)),
        }
    }
    Ok(false)
}

/// `set_prefs`: the server stores the preferences of the account. It applies none of them here (lot L11).
pub(super) async fn set_prefs(
    app: &App,
    socket: &mut Socket,
    session: &mut Session,
    login: &str,
    prefs: Prefs,
) -> Result<bool, End> {
    let Some(items) = app.items() else {
        return not_enabled(socket, session).await;
    };
    match items.set_prefs(login, &prefs).await {
        Ok(()) => Ok(false),
        Err(ItemError::Degraded | ItemError::Replica) => unavailable(socket, session).await,
        Err(e) => Err(storage(e)),
    }
}
