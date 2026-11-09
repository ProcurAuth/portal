//! The operations of the agents, written once for both databases (lot G1a).
//!
//! Each operation runs in one transaction, as the operations of the items do (`items_ops.rs`). The two databases
//! implement [`AgentTx`], the few steps that touch SQL: SQLite in `agents_db.rs`, PostgreSQL in `agents_pg.rs`.
//! The order of the checks and of the writes is in this file only, so the two databases cannot differ.
//!
//! - [`resolve`] finds the agent of a call, or makes it (section 4.3 of the proposal). The limits of a key are
//!   counted in the transaction of the insert, with the lock of the creations, so two instances cannot both pass them.
//! - [`item_agent`] gives the agent that a new item belongs to, and checks it. The composite foreign key of the
//!   database is the last defence: an item cannot name an agent of another key, whatever the code does.
//! - [`human_op`] is the core of the actions of the owner: rename, allow, block, unblock and merge. Each action
//!   checks the owner, writes one row in `agent_events` and raises `rev`.
//!
//! Each change that raises `rev` also writes a message for the devices of the owner (lot G2): the phones get the frame
//! `agent` from the queue of deliveries, with a receipt, so the notice of a new agent is not lost. See [`agent_notice`].

// The channel (G1b), the phones (G2) and the web (G3) call this API in the next lots.
#![allow(dead_code)]

use std::future::Future;

use zeroize::Zeroizing;

use super::agents::{
    check_name, fold, AgentError, AgentEventRow, AgentKind, AgentMode, AgentOrigin, AgentRow,
    AgentState, AgentStatus, Resolved, TextError, FLOOD_ALERT_MS, SEEN_STEP_MS,
};
use super::db::DbError;
use super::items::{Effects, Notice, OwnerAlert};
use super::items_ops::{self as ops, CancelBy, Cancelled, Cx, Done, ItemTx, ListFilter, Sealed};
use super::util::random_id;

/// The steps of the operations of the agents that touch SQL. Each method runs in the transaction of the operation.
pub trait AgentTx {
    fn agent_by_id(&mut self, id: &str) -> impl Future<Output = Result<Option<AgentRow>, DbError>>;
    /// The same read, with the row locked on PostgreSQL. A human action locks the agent before it touches the items.
    fn agent_locked(&mut self, id: &str)
        -> impl Future<Output = Result<Option<AgentRow>, DbError>>;
    fn agent_by_digest(
        &mut self,
        digest: &str,
    ) -> impl Future<Output = Result<Option<AgentRow>, DbError>>;
    fn bucket_of(
        &mut self,
        key_id: &str,
    ) -> impl Future<Output = Result<Option<AgentRow>, DbError>>;
    fn insert_agent(&mut self, a: &AgentRow) -> impl Future<Output = Result<(), DbError>>;
    /// Makes the bucket of a key. A bucket that exists is left as it is.
    fn insert_bucket(&mut self, a: &AgentRow) -> impl Future<Output = Result<(), DbError>>;
    /// Writes every column of the row except the id.
    fn save_agent(&mut self, a: &AgentRow) -> impl Future<Output = Result<(), DbError>>;
    /// Moves the time of the last call forward. It never moves it back, and it never changes `rev`.
    fn touch_agent(&mut self, id: &str, now: i64) -> impl Future<Output = Result<(), DbError>>;
    /// Replaces the sealed hints when `rev` is still `expected_rev`, and raises `rev`. False when the row moved.
    fn set_hints(
        &mut self,
        id: &str,
        expected_rev: i64,
        hints: &Sealed,
        now: i64,
    ) -> impl Future<Output = Result<bool, DbError>>;
    /// The agents of a key that count against `max_per_key`: `new`, `known` and `blocked`, never the bucket.
    fn count_agents(&mut self, key_id: &str) -> impl Future<Output = Result<i64, DbError>>;
    /// The agents that calls made for a key since `since`.
    fn count_made_since(
        &mut self,
        key_id: &str,
        since: i64,
    ) -> impl Future<Output = Result<i64, DbError>>;
    /// Raises the counter of the key (the ordinal of its bucket) and gives the new value.
    fn bump_ordinal(&mut self, key_id: &str) -> impl Future<Output = Result<i64, DbError>>;
    /// Whether another agent of the key has this name key.
    fn name_taken(
        &mut self,
        key_id: &str,
        name_key: &str,
        except_id: &str,
    ) -> impl Future<Output = Result<bool, DbError>>;
    /// The agents with the number of their open items, by key and creation time.
    fn list_agents(
        &mut self,
        owner: Option<&str>,
        key_id: Option<&str>,
    ) -> impl Future<Output = Result<Vec<(AgentRow, i64)>, DbError>>;
    fn insert_agent_event(
        &mut self,
        agent_id: &str,
        at: i64,
        event: &str,
        actor: &str,
        detail: Option<&str>,
    ) -> impl Future<Output = Result<(), DbError>>;
    /// The events of an agent, the newest first.
    fn agent_events(
        &mut self,
        agent_id: &str,
        limit: u32,
    ) -> impl Future<Output = Result<Vec<AgentEventRow>, DbError>>;
    /// Moves the items and the live intents of `from` to `to`, and points the agents that were merged into `from` at
    /// `to`. The result is the number of items and the number of intents that moved.
    fn merge_rows(
        &mut self,
        from: &str,
        to: &str,
    ) -> impl Future<Output = Result<(u64, u64), DbError>>;
    /// Marks the live intents of an agent as used, and erases their sealed text. The result is their number.
    fn kill_intents(
        &mut self,
        agent_id: &str,
        now: i64,
    ) -> impl Future<Output = Result<u64, DbError>>;
    /// Erases the agents with no item, no intent, no alias and no call since `cutoff`. The result is their number.
    fn purge_agents(&mut self, cutoff: i64) -> impl Future<Output = Result<u64, DbError>>;
}

// ---------------------------------------------------------------- the message to the phones

/// Writes the message `agent` for the devices of the owner: one row of `alerts` that names the agent and its `rev`, and a
/// delivery for each device that is not revoked. The phone gets the frame `agent` when the server sends the row. The
/// frame holds the state of the row at that time, so a row of an older `rev` is withdrawn when a newer row waits (see
/// `device_items.rs`). The key of the row is `<agent_id>:agent:<rev>`: two instances that write one change write one row.
///
/// The bucket has no message: its frame never changes, and the request of its first item carries what the phone needs.
/// The phone gets it with `sync`. A portal without the block `inbox.agents` sends no frame of an agent.
pub fn agent_notice(fx: &mut Effects, cx: &Cx, a: &AgentRow) {
    if !cx.rules.agents.enabled || a.kind == AgentKind::Bucket {
        return;
    }
    fx.notices.push(Notice {
        kind: "agent",
        request_id: a.agent_id.clone(),
        seq: u64::try_from(a.rev).unwrap_or(1),
        title: "Agent".into(),
        owner: a.owner.clone(),
        except_device: None,
    });
}

// ---------------------------------------------------------------- the bucket and the agent of an item

/// The bucket of a key. It is made at the first item that needs it, and it stays.
pub async fn ensure_bucket<T: ItemTx>(
    tx: &mut T,
    cx: &Cx,
    key_id: &str,
    owner: &str,
) -> Result<AgentRow, DbError> {
    if let Some(bucket) = tx.bucket_of(key_id).await? {
        return Ok(bucket);
    }
    let row = AgentRow {
        agent_id: random_id(),
        key_id: key_id.to_string(),
        owner: owner.to_string(),
        kind: AgentKind::Bucket,
        state: AgentState::Known,
        origin: AgentOrigin::Key,
        ordinal: 0,
        name: None,
        name_key: None,
        token_digest: None,
        merged_into: None,
        rev: 1,
        created_at: cx.now,
        named_at: None,
        last_seen_at: cx.now,
        hints: None,
    };
    // Two instances can make the bucket at once: the unique index keeps one, and both read it.
    tx.insert_bucket(&row).await?;
    tx.bucket_of(key_id)
        .await?
        .ok_or_else(|| DbError::Pg("agents: the bucket of the key is missing".into()))
}

/// Why an item cannot take the agent that the caller named.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ItemAgentFault {
    /// The agent does not exist, or it belongs to another key or to another owner.
    Unknown,
    /// The owner blocked it.
    Blocked,
}

/// The agent that a new item belongs to. `wanted` is `None` for the bucket of the key. A merged agent gives the agent
/// that it was merged into (one hop: the merge keeps the chain flat), so a call that raced a merge still works.
pub async fn item_agent<T: ItemTx>(
    tx: &mut T,
    cx: &Cx,
    key_id: &str,
    owner: &str,
    wanted: Option<&str>,
) -> Result<Result<String, ItemAgentFault>, DbError> {
    let Some(id) = wanted else {
        return Ok(Ok(ensure_bucket(tx, cx, key_id, owner).await?.agent_id));
    };
    let Some(mut agent) = tx.agent_by_id(id).await? else {
        return Ok(Err(ItemAgentFault::Unknown));
    };
    if agent.state == AgentState::Merged {
        let target = match &agent.merged_into {
            Some(into) => tx.agent_by_id(into).await?,
            None => None,
        };
        match target {
            Some(t) => agent = t,
            None => return Ok(Err(ItemAgentFault::Unknown)),
        }
    }
    if agent.key_id != key_id || agent.owner != owner {
        return Ok(Err(ItemAgentFault::Unknown));
    }
    Ok(match agent.state {
        AgentState::Blocked => Err(ItemAgentFault::Blocked),
        AgentState::Merged => Err(ItemAgentFault::Unknown),
        AgentState::New | AgentState::Known => Ok(agent.agent_id),
    })
}

// ---------------------------------------------------------------- the resolution

/// An agent that the call makes when it has no token. The caller prepares the values, so the transaction needs no
/// random source and no seal: the id and the token come from the caller, and the hints are sealed for this id.
#[derive(Clone)]
pub struct NewAgent {
    pub agent_id: String,
    pub token: Zeroizing<String>,
    pub digest: String,
    pub hints: Option<Sealed>,
}

/// What `resolve` needs.
#[derive(Clone)]
pub struct ResolveReq {
    pub key_id: String,
    pub owner: String,
    /// The mode that applies: the mode of the key, or `key` when the file has no block `inbox.agents`.
    pub mode: AgentMode,
    /// The digest of the token of the call. `None` for a call with no token.
    pub digest: Option<String>,
    /// True for a call that makes an item (`approval_intent`, later `ask`).
    pub creating: bool,
    /// Set for a creating call with no token in the mode `discover`.
    pub new: Option<NewAgent>,
}

/// What a resolution found, with the row that it read. The row lets the caller update the hints without another read.
#[derive(Debug)]
pub struct Outcome {
    pub resolved: Resolved,
    pub row: Option<AgentRow>,
    /// `cap` or `rate` when a limit of the key sent the call to the bucket.
    pub limit: Option<&'static str>,
}

/// The decision of the resolution, before any write. A value lives for one call, so the size of its largest variant
/// costs nothing.
#[allow(clippy::large_enum_variant)]
enum Plan {
    /// No agent: a read or a write on an item, with no token. The item decides.
    NoAgent,
    Refused(AgentError, Option<AgentRow>),
    Existing {
        agent: AgentRow,
        alias: Option<AgentRow>,
    },
    /// The bucket. `why` is `cap` or `rate` when a limit sent the call here.
    Bucket {
        why: Option<&'static str>,
    },
    Create,
}

async fn existing<T: ItemTx>(tx: &mut T, req: &ResolveReq, digest: &str) -> Result<Plan, DbError> {
    // One answer for an unknown token, a token of another key and a token of another owner.
    let unknown = || Plan::Refused(AgentError::Unknown, None);
    let Some(a) = tx.agent_by_digest(digest).await? else {
        return Ok(unknown());
    };
    if a.key_id != req.key_id || a.owner != req.owner {
        return Ok(unknown());
    }
    Ok(match a.state {
        AgentState::Blocked => Plan::Refused(AgentError::Blocked, Some(a)),
        AgentState::New | AgentState::Known => Plan::Existing {
            agent: a,
            alias: None,
        },
        AgentState::Merged => {
            let target = match a.merged_into.as_deref() {
                Some(id) => tx.agent_by_id(id).await?,
                None => None,
            };
            match target {
                Some(t) if t.key_id == a.key_id && t.owner == a.owner => match t.state {
                    AgentState::New | AgentState::Known => Plan::Existing {
                        agent: t,
                        alias: Some(a),
                    },
                    AgentState::Blocked => Plan::Refused(AgentError::Blocked, Some(t)),
                    // The merge keeps the chain flat: a target that is merged too is not a valid state.
                    AgentState::Merged => unknown(),
                },
                _ => unknown(),
            }
        }
    })
}

async fn plan<T: ItemTx>(tx: &mut T, cx: &Cx, req: &ResolveReq) -> Result<Plan, DbError> {
    if req.mode == AgentMode::Key {
        return Ok(if req.creating {
            Plan::Bucket { why: None }
        } else {
            Plan::NoAgent
        });
    }
    if let Some(digest) = &req.digest {
        return existing(tx, req, digest).await;
    }
    if !req.creating {
        return Ok(Plan::NoAgent);
    }
    if req.mode == AgentMode::Enrolled {
        return Ok(Plan::Refused(AgentError::TokenRequired, None));
    }
    let rules = &cx.rules.agents;
    if tx.count_agents(&req.key_id).await? >= i64::from(rules.max_per_key) {
        return Ok(Plan::Bucket { why: Some("cap") });
    }
    let since = cx.now - FLOOD_ALERT_MS;
    if tx.count_made_since(&req.key_id, since).await? >= i64::from(rules.new_per_hour) {
        return Ok(Plan::Bucket { why: Some("rate") });
    }
    Ok(Plan::Create)
}

async fn touch<T: ItemTx>(tx: &mut T, cx: &Cx, a: &AgentRow) -> Result<(), DbError> {
    if cx.now - a.last_seen_at >= SEEN_STEP_MS {
        tx.touch_agent(&a.agent_id, cx.now).await?;
    }
    Ok(())
}

/// A flood: the call went to the bucket because a limit of the key was reached. The event and the alert come once in an
/// hour for each key, so a flood cannot fill the table or the screen of the owner.
async fn flood<T: ItemTx>(
    tx: &mut T,
    cx: &Cx,
    bucket: &AgentRow,
    why: &'static str,
    fx: &mut Effects,
) -> Result<(), DbError> {
    let event = if why == "cap" {
        "refused_cap"
    } else {
        "refused_rate"
    };
    let hour_ago = cx.now - FLOOD_ALERT_MS;
    let recent = tx.agent_events(&bucket.agent_id, 20).await?;
    if recent.iter().any(|e| e.event == event && e.at > hour_ago) {
        return Ok(());
    }
    let rules = &cx.rules.agents;
    let detail = if why == "cap" {
        format!("max_per_key={}", rules.max_per_key)
    } else {
        format!("new_per_hour={}", rules.new_per_hour)
    };
    tx.insert_agent_event(&bucket.agent_id, cx.now, event, "agent", Some(&detail))
        .await?;
    fx.alerts.push(OwnerAlert {
        owner: bucket.owner.clone(),
        title: format!(
            "The key {} makes many new agents: its calls go to Unattributed",
            bucket.key_id
        ),
        key: format!("agents-flood:{}:{}", bucket.key_id, cx.now / FLOOD_ALERT_MS),
        digest: false,
    });
    Ok(())
}

fn status_of(state: AgentState) -> AgentStatus {
    if state == AgentState::New {
        AgentStatus::New
    } else {
        AgentStatus::Known
    }
}

/// Finds the agent of a call, or makes it. A call with no token that does not make an item gets no agent: the item
/// decides (see `agents::check_binding`). An agent is made only by a call that makes an item.
pub async fn resolve<T: ItemTx>(
    tx: &mut T,
    cx: &Cx,
    req: &ResolveReq,
) -> Result<Done<Result<Option<Outcome>, AgentError>>, DbError> {
    let mut fx = Effects::default();
    if req.new.is_some() {
        // The limits are counted after this lock: two instances that make an agent for one key are in line.
        tx.lock_creation().await?;
    }
    let value = match plan(tx, cx, req).await? {
        Plan::NoAgent => Ok(None),
        Plan::Refused(e, row) => {
            // A blocked agent that calls is alive: its row must not be erased as idle.
            if let Some(r) = row {
                touch(tx, cx, &r).await?;
            }
            Err(e)
        }
        Plan::Existing { agent, alias } => {
            touch(tx, cx, &agent).await?;
            if let Some(alias) = &alias {
                touch(tx, cx, alias).await?;
            }
            Ok(Some(Outcome {
                resolved: Resolved {
                    agent_id: agent.agent_id.clone(),
                    kind: agent.kind,
                    state: agent.state,
                    status: status_of(agent.state),
                    token: None,
                },
                row: Some(agent),
                limit: None,
            }))
        }
        Plan::Bucket { why } => {
            let bucket = ensure_bucket(tx, cx, &req.key_id, &req.owner).await?;
            touch(tx, cx, &bucket).await?;
            if let Some(why) = why {
                flood(tx, cx, &bucket, why, &mut fx).await?;
            }
            Ok(Some(Outcome {
                resolved: Resolved {
                    agent_id: bucket.agent_id.clone(),
                    kind: AgentKind::Bucket,
                    state: AgentState::Known,
                    status: if why.is_some() {
                        AgentStatus::Unattributed
                    } else {
                        AgentStatus::Key
                    },
                    token: None,
                },
                row: Some(bucket),
                limit: why,
            }))
        }
        Plan::Create => {
            let Some(new) = &req.new else {
                return Err(DbError::Pg(
                    "agents: a call that makes an agent has no values for it".into(),
                ));
            };
            ensure_bucket(tx, cx, &req.key_id, &req.owner).await?;
            let ordinal = tx.bump_ordinal(&req.key_id).await?;
            let row = AgentRow {
                agent_id: new.agent_id.clone(),
                key_id: req.key_id.clone(),
                owner: req.owner.clone(),
                kind: AgentKind::Agent,
                state: AgentState::New,
                origin: AgentOrigin::Call,
                ordinal,
                name: None,
                name_key: None,
                token_digest: Some(new.digest.clone()),
                merged_into: None,
                rev: 1,
                created_at: cx.now,
                named_at: None,
                last_seen_at: cx.now,
                hints: new.hints.clone(),
            };
            tx.insert_agent(&row).await?;
            tx.insert_agent_event(
                &row.agent_id,
                cx.now,
                "created",
                "agent",
                Some(&format!("ordinal={ordinal}")),
            )
            .await?;
            agent_notice(&mut fx, cx, &row);
            Ok(Some(Outcome {
                resolved: Resolved {
                    agent_id: row.agent_id.clone(),
                    kind: AgentKind::Agent,
                    state: AgentState::New,
                    status: AgentStatus::New,
                    token: Some(new.token.clone()),
                },
                row: Some(row),
                limit: None,
            }))
        }
    };
    ops::done(tx, cx, value, fx).await
}

/// Writes the sealed hints of an agent when nobody changed the row since it was read, and the events that the change
/// raised. The result is false when the row moved: the caller drops the update, because a hint is never worth a retry.
/// The write raises `rev`, so the phones get the frame `agent` with the new hints.
pub async fn write_hints<T: ItemTx>(
    tx: &mut T,
    cx: &Cx,
    agent: &AgentRow,
    hints: &Sealed,
    events: &[&'static str],
) -> Result<Done<bool>, DbError> {
    let mut fx = Effects::default();
    if !tx
        .set_hints(&agent.agent_id, agent.rev, hints, cx.now)
        .await?
    {
        return ops::done(tx, cx, false, fx).await;
    }
    for event in events {
        tx.insert_agent_event(&agent.agent_id, cx.now, event, "agent", None)
            .await?;
    }
    // `set_hints` raised `rev` by one.
    let mut now = agent.clone();
    now.rev += 1;
    agent_notice(&mut fx, cx, &now);
    ops::done(tx, cx, true, fx).await
}

/// The id of the agent that a creating call would get now, and nothing is written. `Some(None)` means that the call
/// would make a new agent. It serves the check of `previous_id`: the new agent has no item, so no `previous_id` fits it.
pub async fn target<T: ItemTx>(
    tx: &mut T,
    cx: &Cx,
    req: &ResolveReq,
) -> Result<Result<Option<String>, AgentError>, DbError> {
    Ok(match plan(tx, cx, req).await? {
        Plan::NoAgent | Plan::Create => Ok(None),
        Plan::Refused(e, _) => Err(e),
        Plan::Existing { agent, .. } => Ok(Some(agent.agent_id)),
        Plan::Bucket { .. } => Ok(tx.bucket_of(&req.key_id).await?.map(|b| b.agent_id)),
    })
}

// ---------------------------------------------------------------- the actions of the owner

/// An action of the owner on an agent.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AgentOp {
    /// Gives or changes the name.
    Rename { name: String },
    /// `new` to `known`. The name is optional. On a `known` agent with a name, it is a rename.
    Allow { name: Option<String> },
    /// `new` or `known` to `blocked`: the open items are cancelled and the live intents are dead.
    Block,
    /// `blocked` to `known`. It revives nothing.
    Unblock,
    /// Moves the items of this agent to another agent of the same key. This agent becomes `merged`.
    Merge { into: String },
}

impl AgentOp {
    /// The label `op` of `portal_agent_actions_total`, and the name of the event of the audit.
    pub fn as_str(&self) -> &'static str {
        match self {
            AgentOp::Rename { .. } => "rename",
            AgentOp::Allow { .. } => "allow",
            AgentOp::Block => "block",
            AgentOp::Unblock => "unblock",
            AgentOp::Merge { .. } => "merge",
        }
    }
}

/// Why an action is refused. Each case is a value of the status of the frame `agent_result`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OpError {
    /// The agent does not exist, or it belongs to another owner. One answer for each case.
    NotFound,
    InvalidName(TextError),
    /// Another agent of the key has this name, after the fold.
    NameTaken,
    /// The state of the agent, or its kind, does not allow the action: the bucket, a blocked agent that is renamed, a
    /// merge of an agent into itself.
    BadState,
    /// A merge across two keys.
    CrossKey,
}

impl OpError {
    /// The member `status` of the frame `agent_result`.
    pub fn status(&self) -> &'static str {
        match self {
            OpError::NotFound => "not_found",
            OpError::InvalidName(_) => "invalid_name",
            OpError::NameTaken => "name_taken",
            OpError::BadState => "bad_state",
            OpError::CrossKey => "cross_key",
        }
    }
}

/// What an action did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OpDone {
    /// The agent after the action.
    pub agent: AgentRow,
    /// With a merge: the target after the action.
    pub other: Option<AgentRow>,
    /// False when the agent was already in the wanted state: the action is idempotent and writes nothing.
    pub changed: bool,
    /// With a block: the open items that the action cancelled.
    pub cancelled: u64,
    /// With a block: the live intents that the action killed.
    pub killed_intents: u64,
    /// With a merge: the items that moved.
    pub moved_items: u64,
}

fn name_key_of(name: &str) -> Result<String, OpError> {
    check_name(name).map_err(OpError::InvalidName)?;
    Ok(fold(name))
}

/// Gives a name to an agent. The caller locked the row and checked the owner.
async fn set_name<T: ItemTx>(
    tx: &mut T,
    cx: &Cx,
    login: &str,
    a: &mut AgentRow,
    name: &str,
) -> Result<Result<bool, OpError>, DbError> {
    let key = match name_key_of(name) {
        Ok(k) => k,
        Err(e) => return Ok(Err(e)),
    };
    if a.name.as_deref() == Some(name) && a.name_key.as_deref() == Some(key.as_str()) {
        return Ok(Ok(false));
    }
    if tx.name_taken(&a.key_id, &key, &a.agent_id).await? {
        return Ok(Err(OpError::NameTaken));
    }
    let event = if a.name.is_some() { "renamed" } else { "named" };
    a.name = Some(name.to_string());
    a.name_key = Some(key);
    a.named_at.get_or_insert(cx.now);
    a.rev += 1;
    tx.save_agent(a).await?;
    tx.insert_agent_event(&a.agent_id, cx.now, event, login, None)
        .await?;
    Ok(Ok(true))
}

/// One action of the owner. The result is a value: a refusal writes nothing.
pub async fn human_op<T: ItemTx>(
    tx: &mut T,
    cx: &Cx,
    login: &str,
    agent_id: &str,
    op: &AgentOp,
) -> Result<Done<Result<OpDone, OpError>>, DbError> {
    // The rows of a merge are locked in the order of their ids, so two merges never wait for each other.
    let into = match op {
        AgentOp::Merge { into } => Some(into.as_str()),
        _ => None,
    };
    let (first, second) = match into {
        Some(into) if into < agent_id => {
            let second = tx.agent_locked(into).await?;
            (tx.agent_locked(agent_id).await?, second)
        }
        Some(into) => {
            let first = tx.agent_locked(agent_id).await?;
            (first, tx.agent_locked(into).await?)
        }
        None => (tx.agent_locked(agent_id).await?, None),
    };
    let refuse = |e: OpError| Done {
        value: Err(e),
        fx: Effects::default(),
    };
    // One answer for an agent that does not exist and an agent of another owner.
    let Some(mut a) = first.filter(|a| a.owner == login) else {
        return Ok(refuse(OpError::NotFound));
    };
    if a.kind == AgentKind::Bucket {
        return Ok(refuse(OpError::BadState));
    }
    let mut out = OpDone {
        agent: a.clone(),
        other: None,
        changed: false,
        cancelled: 0,
        killed_intents: 0,
        moved_items: 0,
    };
    let mut fx = Effects::default();
    match op {
        AgentOp::Rename { name } => {
            if a.state == AgentState::Merged {
                return Ok(refuse(OpError::BadState));
            }
            match set_name(tx, cx, login, &mut a, name).await? {
                Ok(changed) => out.changed = changed,
                Err(e) => return Ok(refuse(e)),
            }
        }
        AgentOp::Allow { name } => {
            if matches!(a.state, AgentState::Blocked | AgentState::Merged) {
                return Ok(refuse(OpError::BadState));
            }
            // The name is checked before the first write, so a refusal leaves the agent as it was.
            if let Some(name) = name {
                let key = match name_key_of(name) {
                    Ok(k) => k,
                    Err(e) => return Ok(refuse(e)),
                };
                if a.name_key.as_deref() != Some(key.as_str())
                    && tx.name_taken(&a.key_id, &key, &a.agent_id).await?
                {
                    return Ok(refuse(OpError::NameTaken));
                }
            }
            if a.state == AgentState::New {
                a.state = AgentState::Known;
                a.rev += 1;
                tx.save_agent(&a).await?;
                tx.insert_agent_event(&a.agent_id, cx.now, "allowed", login, None)
                    .await?;
                out.changed = true;
            }
            if let Some(name) = name {
                if let Ok(changed) = set_name(tx, cx, login, &mut a, name).await? {
                    out.changed |= changed;
                }
            }
        }
        AgentOp::Block => match a.state {
            AgentState::Merged => return Ok(refuse(OpError::BadState)),
            AgentState::Blocked => {}
            AgentState::New | AgentState::Known => {
                a.state = AgentState::Blocked;
                a.rev += 1;
                tx.save_agent(&a).await?;
                let open = tx
                    .list(&ListFilter {
                        agent_id: Some(a.agent_id.clone()),
                        open: Some(true),
                        limit: 100_000,
                        ..Default::default()
                    })
                    .await?;
                let by = CancelBy::System {
                    reason: "agent_blocked",
                    actor: format!("user:{login}"),
                };
                for l in open {
                    let done = ops::cancel(tx, cx, &l.item.request_id, &by).await?;
                    if matches!(done.value, Cancelled::Done(_)) {
                        out.cancelled += 1;
                    }
                    // The cancel wrote its own rows. Its effects are for the log, the metrics and the wake.
                    fx.events.extend(done.fx.events);
                    fx.woken.extend(done.fx.woken);
                }
                out.killed_intents = tx.kill_intents(&a.agent_id, cx.now).await?;
                tx.insert_agent_event(
                    &a.agent_id,
                    cx.now,
                    "blocked",
                    login,
                    Some(&format!(
                        "cancelled={} intents={}",
                        out.cancelled, out.killed_intents
                    )),
                )
                .await?;
                out.changed = true;
            }
        },
        AgentOp::Unblock => match a.state {
            AgentState::New | AgentState::Merged => return Ok(refuse(OpError::BadState)),
            AgentState::Known => {}
            AgentState::Blocked => {
                a.state = AgentState::Known;
                a.rev += 1;
                tx.save_agent(&a).await?;
                tx.insert_agent_event(&a.agent_id, cx.now, "unblocked", login, None)
                    .await?;
                out.changed = true;
            }
        },
        AgentOp::Merge { into } => {
            let Some(mut target) = second else {
                return Ok(refuse(OpError::NotFound));
            };
            if target.owner != login {
                return Ok(refuse(OpError::NotFound));
            }
            if target.key_id != a.key_id {
                return Ok(refuse(OpError::CrossKey));
            }
            // The same merge again: the agent is already in the target. Nothing changed, so nothing is sent.
            if a.state == AgentState::Merged && a.merged_into.as_deref() == Some(into.as_str()) {
                out.other = Some(target);
                return Ok(Done { value: Ok(out), fx });
            }
            let valid_target = target.kind == AgentKind::Agent
                && matches!(target.state, AgentState::New | AgentState::Known);
            let valid_source = matches!(a.state, AgentState::New | AgentState::Known);
            if target.agent_id == a.agent_id || !valid_target || !valid_source {
                return Ok(refuse(OpError::BadState));
            }
            let (items, intents) = tx.merge_rows(&a.agent_id, &target.agent_id).await?;
            a.state = AgentState::Merged;
            a.merged_into = Some(target.agent_id.clone());
            // The name of a merged agent is free for another agent of the key.
            a.name_key = None;
            a.rev += 1;
            tx.save_agent(&a).await?;
            target.rev += 1;
            tx.save_agent(&target).await?;
            let detail = format!("items={items} intents={intents}");
            tx.insert_agent_event(
                &target.agent_id,
                cx.now,
                "merged_in",
                login,
                Some(&format!("from={} {detail}", a.agent_id)),
            )
            .await?;
            tx.insert_agent_event(
                &a.agent_id,
                cx.now,
                "merged_into",
                login,
                Some(&format!("into={} {detail}", target.agent_id)),
            )
            .await?;
            out.moved_items = items;
            out.other = Some(target);
            out.changed = true;
        }
    }
    // The phones of the owner see each change: one `agent` frame for the agent, and one for the target of a merge. An
    // action that changed nothing sends nothing. The notices are written apart from the effects of the cancellations
    // of a block: those were written already.
    if out.changed {
        let mut notices = Effects::default();
        agent_notice(&mut notices, cx, &a);
        if let Some(target) = &out.other {
            agent_notice(&mut notices, cx, target);
        }
        let written = ops::done(tx, cx, (), notices).await?;
        fx.woken.extend(written.fx.woken);
        fx.woken.sort();
        fx.woken.dedup();
    }
    out.agent = a;
    Ok(Done { value: Ok(out), fx })
}
