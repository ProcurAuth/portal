//! What the two databases share for the agents: the columns of `agents`, the values of a row, the reading of a row
//! and the text of the statements. The two drivers differ only in the form of a parameter (`?1` or `$1`).

// The steps of the agents call this module from the two drivers.
#![allow(dead_code)]

use super::agents::{AgentEventRow, AgentKind, AgentOrigin, AgentRow, AgentState};
use super::db::DbError;
use super::items_ops::Sealed;
use super::items_sql::{Cols, V};

/// The columns of the table `agents`, in the order of [`agent_values`]. The first is the key of an update.
pub const AGENT_COLUMNS: [&str; 17] = [
    "agent_id",
    "key_id",
    "owner",
    "kind",
    "state",
    "origin",
    "ordinal",
    "name",
    "name_key",
    "token_digest",
    "merged_into",
    "rev",
    "created_at",
    "named_at",
    "last_seen_at",
    "hints_kid",
    "hints_ct",
];

fn text(v: Option<&str>) -> V {
    v.map_or(V::NullText, |t| V::Text(t.to_string()))
}

pub fn agent_values(a: &AgentRow) -> Vec<V> {
    vec![
        V::Text(a.agent_id.clone()),
        V::Text(a.key_id.clone()),
        V::Text(a.owner.clone()),
        V::Text(a.kind.as_str().to_string()),
        V::Text(a.state.as_str().to_string()),
        V::Text(a.origin.as_str().to_string()),
        V::Int(a.ordinal),
        text(a.name.as_deref()),
        text(a.name_key.as_deref()),
        text(a.token_digest.as_deref()),
        text(a.merged_into.as_deref()),
        V::Int(a.rev),
        V::Int(a.created_at),
        a.named_at.map_or(V::NullInt, V::Int),
        V::Int(a.last_seen_at),
        text(a.hints.as_ref().map(|h| h.kid.as_str())),
        a.hints
            .as_ref()
            .map_or(V::NullBlob, |h| V::Blob(h.ct.clone())),
    ]
}

/// `SELECT` of the agents. With `open`, the last column is the number of open items of the agent.
pub fn select_agents(open: bool) -> String {
    let columns: Vec<String> = AGENT_COLUMNS.iter().map(|c| format!("a.{c}")).collect();
    let count = if open {
        ", (SELECT COUNT(*) FROM items i WHERE i.agent_id = a.agent_id AND i.state IN ('pending', 'replied'))"
    } else {
        ""
    };
    format!("SELECT {}{count} FROM agents a", columns.join(", "))
}

fn bad(column: &str, value: &str) -> DbError {
    DbError::Pg(format!("agents: the column {column} holds {value:?}"))
}

/// Reads an agent from a row of [`select_agents`].
pub fn decode_agent(r: &impl Cols) -> Result<AgentRow, DbError> {
    let need_text = |i: usize| -> Result<String, DbError> {
        r.text(i)?.ok_or_else(|| bad(AGENT_COLUMNS[i], "NULL"))
    };
    let need_int = |i: usize| -> Result<i64, DbError> {
        r.int(i)?.ok_or_else(|| bad(AGENT_COLUMNS[i], "NULL"))
    };
    let kind = need_text(3)?;
    let state = need_text(4)?;
    let origin = need_text(5)?;
    let kid = r.text(15)?;
    let ct = r.blob(16)?;
    Ok(AgentRow {
        agent_id: need_text(0)?,
        key_id: need_text(1)?,
        owner: need_text(2)?,
        kind: AgentKind::parse(&kind).ok_or_else(|| bad("kind", &kind))?,
        state: AgentState::parse(&state).ok_or_else(|| bad("state", &state))?,
        origin: AgentOrigin::parse(&origin).ok_or_else(|| bad("origin", &origin))?,
        ordinal: need_int(6)?,
        name: r.text(7)?,
        name_key: r.text(8)?,
        token_digest: r.text(9)?,
        merged_into: r.text(10)?,
        rev: need_int(11)?,
        created_at: need_int(12)?,
        named_at: r.int(13)?,
        last_seen_at: need_int(14)?,
        hints: kid.zip(ct).map(|(kid, ct)| Sealed { kid, ct }),
    })
}

/// Reads a row of `agent_events`: `at, event, actor, detail`.
pub fn decode_event(r: &impl Cols) -> Result<AgentEventRow, DbError> {
    Ok(AgentEventRow {
        at: r.int(0)?.ok_or_else(|| bad("at", "NULL"))?,
        event: r.text(1)?.ok_or_else(|| bad("event", "NULL"))?,
        actor: r.text(2)?.ok_or_else(|| bad("actor", "NULL"))?,
        detail: r.text(3)?,
    })
}
