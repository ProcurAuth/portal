//! The items on the side of a phone: the frames that the server builds when it sends a delivery, and the handling of
//! `answer`, `reply`, `refresh`, `sync`, `set_prefs` and `agent_do`.
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
//! # The agents
//!
//! A change of an agent that raises its `rev` writes one row of the queue for each device of the owner (see
//! `agents_ops::agent_notice`). The row holds the id of the agent and the `rev`. The frame `agent` is built from the row
//! of the agent when the session sends it, so it holds the state of that time. A row whose `rev` is older than the row of
//! the agent is withdrawn: a newer row waits in the queue. A phone that lacks an agent, or holds it in an older `rev`,
//! says so in `sync`, and the server sends the frames that it lacks. Those frames have a new id that is in no queue.
//! `agent_do` is the action of the owner on an agent: the engine decides and the server answers `agent_result`.
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
    close, Action, AgentDoMsg, AgentOpKind, AgentRef, AgentResultMsg, AgentResultStatus, AnswerMsg,
    AnswerResultMsg, AnswerStatus, ItemState, Prefs, ReplyMsg, ServerMsg,
};
use zeroize::Zeroizing;

use super::agents::{AgentKind, AgentMode, AgentRow, AgentState, AgentSummary, Observed};
use super::agents_ops::{AgentOp, OpError};
use super::device::{send_encoded, send_msg, End};
use super::frames::{self, AgentFacts, ApprovalFacts, OwnerAnswer};
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
    /// The preferences of the owner, read once for the flush.
    prefs: Option<Prefs>,
    /// The agents of the owner with the number of their open items, read once for the flush.
    agents: Option<HashMap<String, AgentSummary>>,
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
            prefs: None,
            agents: None,
        })
    }

    /// The agents of the owner, by id. An agent of another owner is not in it, so no frame names it.
    async fn agents(&mut self) -> Result<&HashMap<String, AgentSummary>, ItemError> {
        if self.agents.is_none() {
            let list = self.items.list_agents(self.login, None).await?;
            self.agents = Some(
                list.into_iter()
                    .map(|s| (s.agent.agent_id.clone(), s))
                    .collect(),
            );
        }
        Ok(self.agents.as_ref().expect("the agents were read"))
    }

    /// What the key of an agent does with the agents. The portal without the block `inbox.agents` treats each key as the
    /// mode `key`, and so does a key that the portal cannot read.
    fn mode_of(&self, key_id: &str) -> AgentMode {
        let mode = self
            .app
            .authorizer()
            .and_then(|a| a.key_agent_mode(key_id))
            .unwrap_or_default();
        self.items.rules.agents.effective(mode)
    }

    /// The frame `agent` of an agent, with the id of the delivery.
    pub(crate) async fn agent_frame(
        &mut self,
        s: &AgentSummary,
        id: &str,
    ) -> Result<Built, ItemError> {
        if !self.items.rules.agents.enabled {
            return Ok(Built::Withdraw("agents_not_enabled"));
        }
        let row = &s.agent;
        let key_name = frames::key_display(
            &row.key_id,
            self.app
                .authorizer()
                .and_then(|a| a.key_name(&row.key_id))
                .as_deref(),
        );
        let observed = self.items.observed(row);
        let mode = self.mode_of(&row.key_id);
        // A `new` agent can look like a known agent of the same key: the frame names it (a hint, never a merge).
        let similar = if row.state == AgentState::New && row.kind == AgentKind::Agent {
            let rows: Vec<AgentRow> = self
                .agents()
                .await?
                .values()
                .filter(|o| o.agent.key_id == row.key_id)
                .map(|o| o.agent.clone())
                .collect();
            let others: Vec<(AgentRow, Option<Observed>)> = rows
                .into_iter()
                .map(|a| {
                    let seen = self.items.observed(&a);
                    (a, seen)
                })
                .collect();
            frames::similar_of(
                row,
                observed.as_ref(),
                others.iter().map(|(a, o)| (a, o.clone())),
                &key_name,
                mode,
            )
        } else {
            None
        };
        let facts = AgentFacts {
            key_name: &key_name,
            mode,
            observed: observed.as_ref(),
            open: s.open,
            lock_new: self.items.rules.agents.lock_new,
            similar,
        };
        match frames::agent_frame(id, row, &facts) {
            Ok(frame) => Ok(encode(&ServerMsg::Agent(Box::new(frame)))),
            Err(e) => {
                log::error!(event = "agent_frame_refused", agent.id = %row.agent_id, error = %e);
                Ok(Built::Withdraw("frame_not_built"))
            }
        }
    }

    /// The delivery of a message `agent`: the row of the queue names the agent and the `rev` of the change.
    async fn agent_delivery(&mut self, d: &ItemDelivery) -> Result<Built, ItemError> {
        let Some(summary) = self.agents().await?.get(&d.request_id).cloned() else {
            return Ok(Built::Withdraw("agent_gone"));
        };
        // The agent changed again: the row of the new `rev` waits in the queue of this device, and it gives the state.
        if u64::try_from(summary.agent.rev).unwrap_or(0) > d.item_seq {
            return Ok(Built::Withdraw("agent_rev_replaced"));
        }
        self.agent_frame(&summary, &d.alert_id).await
    }

    /// The preferences of the owner: the quiet hours decide the flag `quiet` of a request.
    async fn prefs(&mut self) -> Result<Prefs, ItemError> {
        if let Some(prefs) = &self.prefs {
            return Ok(prefs.clone());
        }
        let prefs = self.items.prefs(self.login).await?;
        self.prefs = Some(prefs.clone());
        Ok(prefs)
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
        // The row of an agent names the agent, not an item.
        if d.kind == "agent" {
            return self.agent_delivery(d).await;
        }
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
        // The agent of the item gives the label and the state of the line "from".
        let Some(agent) = self
            .agents()
            .await?
            .get(&l.item.agent_id)
            .map(|s| s.agent.clone())
        else {
            return Ok(Built::Withdraw("agent_not_found"));
        };
        let mode = self.mode_of(&agent.key_id);
        // An item of a blocked agent is final, and the phone gets its `settled`.
        let Some(from) = frames::sender(&l.item, key_name.as_deref(), &agent, mode) else {
            return Ok(Built::Withdraw("agent_not_open"));
        };
        let quiet = frames::arrived_quiet(&self.prefs().await?, l.item.created_at);
        match frames::request_frame(id, l, &plain, from, &facts) {
            Ok(mut frame) => {
                frame.quiet = quiet;
                Ok(encode(&ServerMsg::Request(Box::new(frame))))
            }
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
        agent_id: None,
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

/// The encoded `digest` frame of an owner, or `None` when the session would not send it. For the tests of the portal.
#[cfg(test)]
pub(crate) async fn digest_for_test(app: &App, login: &str, id: &str) -> Option<Vec<u8>> {
    match digest(app, login, id).await {
        Ok(Built::Frame(bytes)) => Some(bytes),
        _ => None,
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
        // The answer does not fit the question. The phone checks it before it sends, so this is a fault of the phone: the
        // server says so, and it takes no answer. The code names the rule and holds no value.
        Ok(Answered::Invalid(rule)) => {
            log::warn!(event = "phone_answer_invalid", device.id = %device_id, request.id = %a.request_id);
            send_msg(
                socket,
                session,
                &ServerMsg::Error {
                    code: "invalid_answer".into(),
                    message: rule.into(),
                },
            )
            .await?;
            return Ok(true);
        }
        // The agent is new and `lock_new` is true: the owner can decline and reply, not accept. The item stays open.
        Ok(Answered::AgentLocked(loaded)) => (AnswerStatus::AgentLocked, Some(loaded.item.state)),
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

/// `agent_do`: one action of the owner on an agent. The engine decides, and the server answers `agent_result`. The
/// phone sends the frame again until it gets the result, and every action is idempotent. The owner is the account of
/// the phone: an agent of another owner is `not_found`, as an agent that does not exist. A change reaches every device of
/// the owner as a frame `agent`, the acting device too.
pub(super) async fn agent_do(
    app: &App,
    socket: &mut Socket,
    session: &mut Session,
    device_id: &str,
    login: &str,
    a: AgentDoMsg,
) -> Result<bool, End> {
    let Some(items) = app.items() else {
        return not_enabled(socket, session).await;
    };
    let status = match agent_do_status(&items, login, &a).await {
        Ok(status) => status,
        Err(ItemError::Degraded | ItemError::Replica) => return unavailable(socket, session).await,
        Err(e) => return Err(storage(e)),
    };
    log::info!(
        event = "phone_agent_do",
        device.id = %device_id,
        agent.id = %a.agent_id,
        status = status.as_str()
    );
    let msg = ServerMsg::AgentResult(AgentResultMsg {
        client_msg_id: a.client_msg_id,
        agent_id: a.agent_id,
        status,
    });
    send_msg(socket, session, &msg).await?;
    // The change wrote a message for each device: send it now.
    Ok(true)
}

/// What the engine says about an `agent_do`: the status of the frame `agent_result`. A server without the block
/// `inbox.agents` answers `not_enabled`. A refusal changes nothing.
pub(crate) async fn agent_do_status(
    items: &Items<'_>,
    login: &str,
    a: &AgentDoMsg,
) -> Result<AgentResultStatus, ItemError> {
    if !items.rules.agents.enabled {
        return Ok(AgentResultStatus::NotEnabled);
    }
    // The parser checked which members the action has.
    let op = match (a.op, a.name.clone(), a.into.clone()) {
        (AgentOpKind::Rename, Some(name), None) => AgentOp::Rename { name },
        (AgentOpKind::Allow, name, None) => AgentOp::Allow { name },
        (AgentOpKind::Block, None, None) => AgentOp::Block,
        (AgentOpKind::Unblock, None, None) => AgentOp::Unblock,
        (AgentOpKind::Merge, None, Some(into)) => AgentOp::Merge { into },
        _ => return Ok(AgentResultStatus::BadState),
    };
    Ok(
        match items.agent_op(login, &a.agent_id, op, "phone").await? {
            Ok(_done) => AgentResultStatus::Ok,
            Err(e) => result_status(e),
        },
    )
}

/// The status of the frame `agent_result` for a refusal of the engine.
pub(crate) fn result_status(e: OpError) -> AgentResultStatus {
    match e {
        OpError::NotFound => AgentResultStatus::NotFound,
        OpError::InvalidName(_) => AgentResultStatus::InvalidName,
        OpError::NameTaken => AgentResultStatus::NameTaken,
        OpError::BadState => AgentResultStatus::BadState,
        OpError::CrossKey => AgentResultStatus::CrossKey,
    }
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

/// The `agent` frames that a phone lacks, as an answer to its `sync`. The phone lists the agents that it holds with their
/// `rev`. The server sends an agent that the phone does not list, unless it is idle or gone (`blocked`, `merged`), and an
/// agent that the phone lists in an older `rev`, whatever its state. The frames have a new id that is in no queue. A
/// portal without the block `inbox.agents` sends none.
pub(crate) async fn agent_frames_for_sync(
    builder: &mut Builder<'_>,
    held: &[AgentRef],
) -> Result<Vec<Vec<u8>>, ItemError> {
    if !builder.items.rules.agents.enabled {
        return Ok(Vec::new());
    }
    let held: HashMap<&str, u64> = held.iter().map(|a| (a.agent_id.as_str(), a.rev)).collect();
    let mut list: Vec<AgentSummary> = builder.agents().await?.values().cloned().collect();
    list.sort_by(|a, b| {
        (a.agent.created_at, &a.agent.agent_id).cmp(&(b.agent.created_at, &b.agent.agent_id))
    });
    let mut out = Vec::new();
    for s in list {
        let rev = u64::try_from(s.agent.rev).unwrap_or(0);
        let wanted = match held.get(s.agent.agent_id.as_str()) {
            Some(known) => *known < rev,
            None => !s.idle && !matches!(s.agent.state, AgentState::Blocked | AgentState::Merged),
        };
        if !wanted {
            continue;
        }
        match builder.agent_frame(&s, &random_id()).await? {
            Built::Frame(bytes) => out.push(bytes),
            Built::Withdraw(why) => {
                log::info!(event = "agent_state_not_sent", agent.id = %s.agent.agent_id, reason = why)
            }
            Built::Keep => {}
        }
    }
    Ok(out)
}

/// `sync`: the phone says which items it holds open, and which agents it holds. The server sends the agents that it
/// lacks, the open items that it lacks or knows in an older state, and the `settled` frame of each item that is final.
pub(super) async fn sync(
    app: &App,
    socket: &mut Socket,
    session: &mut Session,
    login: &str,
    open: Vec<inbox_core::proto::OpenRef>,
    agents: Option<Vec<AgentRef>>,
) -> Result<bool, End> {
    let Some(mut builder) = Builder::new(app, login, &[]) else {
        return not_enabled(socket, session).await;
    };
    // The agents come first: the phone groups the items by agent when they arrive.
    match agent_frames_for_sync(&mut builder, agents.as_deref().unwrap_or_default()).await {
        Ok(frames) => {
            for bytes in frames {
                send_encoded(socket, session, &bytes).await?;
            }
        }
        Err(ItemError::Degraded | ItemError::Replica) => return unavailable(socket, session).await,
        Err(e) => return Err(storage(e)),
    }
    let held: HashMap<&str, u64> = open
        .iter()
        .map(|o| (o.request_id.as_str(), o.seq))
        .collect();
    let filter = ListFilter {
        owner: Some(login.to_string()),
        open: Some(true),
        limit: SYNC_LIMIT,
        agent_id: None,
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

/// `set_prefs`: the server stores the preferences of the account, after it checked them against the time zone database
/// and the limits of the file. A bad value gets the error `invalid_prefs` that names the field, and nothing is stored.
/// The page "Preferences" of the owner reads the same row.
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
        Err(ItemError::Prefs(e)) => {
            send_msg(
                socket,
                session,
                &ServerMsg::Error {
                    code: "invalid_prefs".into(),
                    message: e.to_string(),
                },
            )
            .await?;
            Ok(false)
        }
        Err(e) => Err(storage(e)),
    }
}
