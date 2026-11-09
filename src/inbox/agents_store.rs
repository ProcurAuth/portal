//! The agents for the rest of the portal: the calls that the agent channel, the phones and the web use (lot G1a).
//!
//! The engine of the items (`items_store.rs`) seals what it writes and unseals what it serves. This file adds the
//! calls of the agents: the resolution of a call, the check of the binding to an item, the reads for the screens and the
//! actions of the owner. The operations are in `agents_ops.rs`. The hints are the only sealed values here: the
//! engine opens them for the owner and never logs them.

// The channel (G1b), the phones (G2) and the web (G3) call this API in the next lots.
#![allow(dead_code)]

use super::agents::{
    check_binding, merge_hints, new_token, token_digest, token_has_form, AgentCall, AgentError,
    AgentEventRow, AgentKind, AgentMode, AgentRow, AgentSummary, Hints, Observed, Resolved,
};
use super::agents_ops::{self as ops, AgentOp, NewAgent, OpDone, OpError, ResolveReq};
use super::content_box::{aad, FIELD_HINTS};
use super::items::Loaded;
use super::items_ops::Sealed;
use super::items_store::{in_tx, ItemError, Items};
use super::log;
use super::util::random_id;

impl Items<'_> {
    fn seal_hints(&self, agent_id: &str, observed: &Observed) -> Result<Sealed, ItemError> {
        let plain = zeroize::Zeroizing::new(
            serde_json::to_vec(observed).map_err(|e| ItemError::Seal(e.to_string()))?,
        );
        self.seal(&plain, &aad(agent_id, FIELD_HINTS, 0, None))
    }

    /// What the portal remembers of the hints of an agent, unsealed. `None` when the agent has none, or when the seal
    /// does not open (the key was lost and purged).
    pub fn observed(&self, agent: &AgentRow) -> Option<Observed> {
        let sealed = agent.hints.as_ref()?;
        let plain = self
            .content
            .open(&sealed.ct, &aad(&agent.agent_id, FIELD_HINTS, 0, None))?;
        serde_json::from_slice(&plain).ok()
    }

    // ------------------------------------------------------------ the resolution

    fn resolve_request(
        &self,
        key_id: &str,
        owner: &str,
        call: &AgentCall,
        creating: bool,
        prepare: bool,
    ) -> Result<Result<ResolveReq, AgentError>, ItemError> {
        // Without the block `inbox.agents`, each key is one agent, whatever the mode of the key says.
        let mode = self.rules.agents.effective(call.mode);
        let digest = match (&call.token, mode) {
            // The mode `key` ignores the token.
            (_, AgentMode::Key) | (None, _) => None,
            (Some(token), _) if token_has_form(token) => Some(token_digest(token)),
            // A text of another form is unknown at once. It never reaches the database.
            (Some(_), _) => return Ok(Err(AgentError::Unknown)),
        };
        let new = if prepare && creating && mode == AgentMode::Discover && digest.is_none() {
            let token = new_token();
            let agent_id = random_id();
            let (observed, _) = merge_hints(None, &call.hints);
            let hints = if observed == Observed::default() {
                None
            } else {
                Some(self.seal_hints(&agent_id, &observed)?)
            };
            Some(NewAgent {
                digest: token_digest(&token),
                agent_id,
                token,
                hints,
            })
        } else {
            None
        };
        Ok(Ok(ResolveReq {
            key_id: key_id.to_string(),
            owner: owner.to_string(),
            mode,
            digest,
            creating,
            new,
        }))
    }

    /// Finds the agent of a call, or makes it (section 4.3 of the proposal).
    ///
    /// - The mode `key`: the bucket of the key (made at the first item). The token is ignored.
    /// - A token: its agent. An unknown token, a token of another key and a token of another owner give the same
    ///   error, `Unknown`. A blocked agent gives `Blocked`. A merged agent gives the agent that it was merged into.
    /// - No token and `creating`: in the mode `enrolled`, `TokenRequired`. In the mode `discover`, a new agent with a
    ///   token, unless the key has `max_per_key` agents or made `new_per_hour` in the last hour: then the bucket, with
    ///   the status `Unattributed`, no token, and one alert each hour to the owner.
    /// - No token and not `creating` (a read or a write on an item): `None`. The item decides: see
    ///   [`Items::check_item_agent`]. A call that does not make an item never makes an agent.
    pub async fn resolve_agent(
        &self,
        key_id: &str,
        owner: &str,
        call: &AgentCall,
        creating: bool,
    ) -> Result<Option<Resolved>, AgentError> {
        let req = match self.resolve_request(key_id, owner, call, creating, true)? {
            Ok(req) => req,
            Err(e) => {
                self.refused(&e);
                return Err(e);
            }
        };
        // A call that needs no row needs no database: the mode `key` for a read, or a read with no token.
        if !creating && req.digest.is_none() {
            return Ok(None);
        }
        self.writable()?;
        let cx = self.cx();
        let done = in_tx!(self, [cx, req], tx, ops::resolve(tx, &cx, &req))?;
        self.after(&done.fx);
        let outcome = match done.value {
            Ok(outcome) => outcome,
            Err(e) => {
                self.refused(&e);
                return Err(e);
            }
        };
        let Some(outcome) = outcome else {
            return Ok(None);
        };
        if let Some(m) = self.metrics {
            if outcome.resolved.token.is_some() {
                m.agent_created("call");
            }
            if let Some(why) = outcome.limit {
                m.agent_refused(why);
            }
        }
        // A new hint of an agent that exists is a best effort: a call never fails because of it.
        if let Some(row) = &outcome.row {
            if row.kind == AgentKind::Agent
                && outcome.resolved.token.is_none()
                && !call.hints.is_empty()
            {
                if let Err(e) = self.update_hints(row, &call.hints).await {
                    log::warn!(event = "agent_hints_failed", agent.id = %row.agent_id, error = %e);
                }
            }
        }
        Ok(Some(outcome.resolved))
    }

    fn refused(&self, e: &AgentError) {
        if let (Some(m), Some(reason)) = (self.metrics, e.metric()) {
            m.agent_refused(reason);
        }
    }

    async fn update_hints(&self, row: &AgentRow, seen: &Hints) -> Result<(), ItemError> {
        let old = self.observed(row);
        let (now, events) = merge_hints(old.as_ref(), seen);
        if old.as_ref() == Some(&now) {
            return Ok(());
        }
        let sealed = self.seal_hints(&row.agent_id, &now)?;
        let cx = self.cx();
        let (id, rev) = (row.agent_id.clone(), row.rev);
        let written = in_tx!(
            self,
            [cx, id, rev, sealed, events],
            tx,
            ops::write_hints(tx, &cx, &id, rev, &sealed, &events)
        )?;
        // A lost race means that another call wrote a newer record: this one is dropped.
        let _ = written;
        Ok(())
    }

    /// The id of the agent that a creating call would get now: `Some(id)` for an agent that exists or for the bucket,
    /// `None` when the call would make a new agent. Nothing is written. It serves the check of `previous_id`: a new
    /// agent has no item, so no `previous_id` fits it.
    pub async fn agent_target(
        &self,
        key_id: &str,
        owner: &str,
        call: &AgentCall,
    ) -> Result<Option<String>, AgentError> {
        let req = self
            .resolve_request(key_id, owner, call, true, false)?
            .inspect_err(|e| self.refused(e))?;
        let cx = self.cx();
        let target = in_tx!(self, [cx, req], tx, ops::target(tx, &cx, &req))??;
        Ok(target)
    }

    /// The rule of section 4.4 for a call that names an item (see [`check_binding`]). `resolved` is what
    /// [`Items::resolve_agent`] gave with `creating = false`. A read goes by the key: no token is allowed, and a token
    /// must be the agent of the item. A write on the item of a real agent needs the token. An item of the bucket needs
    /// the key only.
    pub async fn check_item_agent(
        &self,
        l: &Loaded,
        resolved: Option<&Resolved>,
        write: bool,
    ) -> Result<(), AgentError> {
        let is_bucket = if write {
            let id = l.item.agent_id.clone();
            in_tx!(self, [id], tx, ops::AgentTx::agent_by_id(tx, &id))?
                .is_none_or(|a| a.kind == AgentKind::Bucket)
        } else {
            false
        };
        let result = check_binding(
            &l.item.agent_id,
            is_bucket,
            resolved.map(|r| r.agent_id.as_str()),
            write,
        );
        if let Err(e) = &result {
            self.refused(e);
        }
        result
    }

    // ------------------------------------------------------------ the reads

    pub async fn agent(&self, agent_id: &str) -> Result<Option<AgentRow>, ItemError> {
        let id = agent_id.to_string();
        in_tx!(self, [id], tx, ops::AgentTx::agent_by_id(tx, &id))
    }

    /// The agents of an owner, with the number of their open items. Pass a key id to keep one key. The order is the
    /// order of creation. An agent with no open item and no call for `inbox.agents.idle_after` is `idle`: the active
    /// views leave it out, and it still works.
    pub async fn list_agents(
        &self,
        owner: &str,
        key_id: Option<&str>,
    ) -> Result<Vec<AgentSummary>, ItemError> {
        let (owner, key_id) = (owner.to_string(), key_id.map(str::to_string));
        let rows = in_tx!(
            self,
            [owner, key_id],
            tx,
            ops::AgentTx::list_agents(tx, Some(&owner), key_id.as_deref())
        )?;
        let (now, rules) = (self.clock.now_ms(), self.rules.agents);
        Ok(rows
            .into_iter()
            .map(|(agent, open)| AgentSummary {
                idle: rules.enabled
                    && open == 0
                    && agent.kind == AgentKind::Agent
                    && now - agent.last_seen_at >= rules.idle_after_ms,
                agent,
                open,
            })
            .collect())
    }

    /// The events of an agent for its page, the newest first.
    pub async fn agent_events(
        &self,
        agent_id: &str,
        limit: u32,
    ) -> Result<Vec<AgentEventRow>, ItemError> {
        let id = agent_id.to_string();
        in_tx!(self, [id], tx, ops::AgentTx::agent_events(tx, &id, limit))
    }

    /// A known or new agent of the owner, in the same key, whose name folds to what `agent` says about itself. The
    /// screens warn the owner: "Same text as the name of your agent X. This can be a copy."
    pub async fn lookalike(&self, agent: &AgentRow) -> Result<Option<AgentRow>, ItemError> {
        let Some(says) = self.observed(agent).and_then(|o| o.says) else {
            return Ok(None);
        };
        let folded = super::agents::fold(&says);
        let others = self.list_agents(&agent.owner, Some(&agent.key_id)).await?;
        Ok(others.into_iter().map(|s| s.agent).find(|a| {
            a.agent_id != agent.agent_id
                && a.state != super::agents::AgentState::Merged
                && a.name_key.as_deref() == Some(folded.as_str())
        }))
    }

    // ------------------------------------------------------------ the actions of the owner

    /// One action of the owner: rename, allow, block, unblock or merge. The owner is the login: an agent of another
    /// owner is `NotFound`. A refusal is a value and writes nothing. An action that finds the agent in the wanted state
    /// is a success with `changed = false`: the phone sends an action again until it gets the result. `via` is `web`
    /// or `phone`, for the metrics.
    pub async fn agent_op(
        &self,
        login: &str,
        agent_id: &str,
        op: AgentOp,
        via: &'static str,
    ) -> Result<Result<OpDone, OpError>, ItemError> {
        self.writable()?;
        let cx = self.cx();
        let (login_text, id) = (login.to_string(), agent_id.to_string());
        let done = in_tx!(
            self,
            [cx, login_text, id, op],
            tx,
            ops::human_op(tx, &cx, &login_text, &id, &op)
        )?;
        self.after(&done.fx);
        if let Ok(out) = &done.value {
            if out.changed {
                if let Some(m) = self.metrics {
                    m.agent_action(op.as_str(), via);
                }
                log::info!(event = "agent_action", op = op.as_str(), agent.id = %agent_id, actor = %login, via = via);
                self.replicate().await?;
            }
        }
        Ok(done.value)
    }
}
