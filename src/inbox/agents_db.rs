//! The SQLite side of the agents: the steps of `agents_ops.rs` that touch SQL.

use rusqlite::params;
use rusqlite::params_from_iter;
use rusqlite::OptionalExtension;

use super::agents::{AgentEventRow, AgentRow};
use super::agents_ops::AgentTx;
use super::agents_sql::{agent_values, decode_agent, decode_event, select_agents, AGENT_COLUMNS};
use super::db::DbError;
use super::items_db::{ph, value, SqliteTx};
use super::items_ops::Sealed;
use super::items_sql::{insert_sql, update_sql};

impl SqliteTx<'_, '_> {
    fn agent_where(&mut self, clause: &str, arg: &str) -> Result<Option<AgentRow>, DbError> {
        let sql = format!("{} WHERE {clause}", select_agents(false));
        self.0
            .prepare_cached(&sql)?
            .query_row(params![arg], |r| Ok(decode_agent(r)))
            .optional()?
            .transpose()
    }

    fn count(&mut self, sql: &str, args: &[&dyn rusqlite::ToSql]) -> Result<i64, DbError> {
        Ok(self.0.prepare_cached(sql)?.query_row(args, |r| r.get(0))?)
    }
}

impl AgentTx for SqliteTx<'_, '_> {
    async fn agent_by_id(&mut self, id: &str) -> Result<Option<AgentRow>, DbError> {
        self.agent_where("a.agent_id = ?1", id)
    }

    async fn agent_locked(&mut self, id: &str) -> Result<Option<AgentRow>, DbError> {
        // The transaction is `IMMEDIATE`: there is one writer, so no row needs a lock.
        self.agent_where("a.agent_id = ?1", id)
    }

    async fn agent_by_digest(&mut self, digest: &str) -> Result<Option<AgentRow>, DbError> {
        self.agent_where("a.token_digest = ?1", digest)
    }

    async fn bucket_of(&mut self, key_id: &str) -> Result<Option<AgentRow>, DbError> {
        self.agent_where("a.key_id = ?1 AND a.kind = 'bucket'", key_id)
    }

    async fn insert_agent(&mut self, a: &AgentRow) -> Result<(), DbError> {
        let sql = insert_sql("agents", &AGENT_COLUMNS, &ph);
        self.0
            .prepare_cached(&sql)?
            .execute(params_from_iter(agent_values(a).iter().map(value)))?;
        Ok(())
    }

    async fn insert_bucket(&mut self, a: &AgentRow) -> Result<(), DbError> {
        let sql = format!(
            "{} ON CONFLICT (key_id) WHERE kind = 'bucket' DO NOTHING",
            insert_sql("agents", &AGENT_COLUMNS, &ph)
        );
        self.0
            .prepare_cached(&sql)?
            .execute(params_from_iter(agent_values(a).iter().map(value)))?;
        Ok(())
    }

    async fn save_agent(&mut self, a: &AgentRow) -> Result<(), DbError> {
        let sql = update_sql("agents", &AGENT_COLUMNS, &ph);
        self.0
            .prepare_cached(&sql)?
            .execute(params_from_iter(agent_values(a).iter().map(value)))?;
        Ok(())
    }

    async fn touch_agent(&mut self, id: &str, now: i64) -> Result<(), DbError> {
        self.0
            .prepare_cached(
                "UPDATE agents SET last_seen_at = ?2 WHERE agent_id = ?1 AND last_seen_at < ?2",
            )?
            .execute(params![id, now])?;
        Ok(())
    }

    async fn set_hints(
        &mut self,
        id: &str,
        expected_rev: i64,
        hints: &Sealed,
        now: i64,
        bump: bool,
    ) -> Result<bool, DbError> {
        let n = self
            .0
            .prepare_cached(
                "UPDATE agents SET hints_kid = ?3, hints_ct = ?4, rev = rev + ?6,
                 last_seen_at = CASE WHEN last_seen_at < ?5 THEN ?5 ELSE last_seen_at END
             WHERE agent_id = ?1 AND rev = ?2",
            )?
            .execute(params![
                id,
                expected_rev,
                hints.kid,
                hints.ct,
                now,
                i64::from(bump)
            ])?;
        Ok(n == 1)
    }

    async fn count_agents(&mut self, key_id: &str) -> Result<i64, DbError> {
        self.count(
            "SELECT COUNT(*) FROM agents WHERE key_id = ?1 AND kind = 'agent' AND state IN ('new', 'known', 'blocked')",
            &[&key_id],
        )
    }

    async fn count_made_since(&mut self, key_id: &str, since: i64) -> Result<i64, DbError> {
        self.count(
            "SELECT COUNT(*) FROM agents WHERE key_id = ?1 AND kind = 'agent' AND origin = 'call' AND created_at > ?2",
            &[&key_id, &since],
        )
    }

    async fn bump_ordinal(&mut self, key_id: &str) -> Result<i64, DbError> {
        self.0
            .prepare_cached(
                "UPDATE agents SET ordinal = ordinal + 1 WHERE key_id = ?1 AND kind = 'bucket'",
            )?
            .execute(params![key_id])?;
        self.count(
            "SELECT ordinal FROM agents WHERE key_id = ?1 AND kind = 'bucket'",
            &[&key_id],
        )
    }

    async fn name_taken(
        &mut self,
        key_id: &str,
        name_key: &str,
        except_id: &str,
    ) -> Result<bool, DbError> {
        Ok(self.count(
            "SELECT COUNT(*) FROM agents WHERE key_id = ?1 AND name_key = ?2 AND agent_id <> ?3",
            &[&key_id, &name_key, &except_id],
        )? > 0)
    }

    async fn list_agents(
        &mut self,
        owner: Option<&str>,
        key_id: Option<&str>,
    ) -> Result<Vec<(AgentRow, i64)>, DbError> {
        let mut sql = format!("{} WHERE 1 = 1", select_agents(true));
        let mut args: Vec<rusqlite::types::Value> = Vec::new();
        for (column, wanted) in [("owner", owner), ("key_id", key_id)] {
            if let Some(w) = wanted {
                args.push(rusqlite::types::Value::Text(w.to_string()));
                sql.push_str(&format!(" AND a.{column} = ?{}", args.len()));
            }
        }
        sql.push_str(" ORDER BY a.created_at, a.agent_id");
        let mut stmt = self.0.prepare_cached(&sql)?;
        let rows = stmt.query_map(params_from_iter(args), |r| {
            Ok(decode_agent(r).and_then(|a| Ok((a, r.get::<_, i64>(AGENT_COLUMNS.len())?))))
        })?;
        rows.map(|r| r?).collect()
    }

    async fn insert_agent_event(
        &mut self,
        agent_id: &str,
        at: i64,
        event: &str,
        actor: &str,
        detail: Option<&str>,
    ) -> Result<(), DbError> {
        self.0
            .prepare_cached(
                "INSERT INTO agent_events (agent_id, at, event, actor, detail) VALUES (?1, ?2, ?3, ?4, ?5)",
            )?
            .execute(params![agent_id, at, event, actor, detail])?;
        Ok(())
    }

    async fn agent_events(
        &mut self,
        agent_id: &str,
        limit: u32,
    ) -> Result<Vec<AgentEventRow>, DbError> {
        let mut stmt = self.0.prepare_cached(
            "SELECT at, event, actor, detail FROM agent_events WHERE agent_id = ?1 ORDER BY at DESC, id DESC LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![agent_id, limit], |r| Ok(decode_event(r)))?;
        rows.map(|r| r?).collect()
    }

    async fn merge_rows(&mut self, from: &str, to: &str) -> Result<(u64, u64), DbError> {
        let items = self.0.execute(
            "UPDATE items SET agent_id = ?2 WHERE agent_id = ?1",
            params![from, to],
        )?;
        let intents = self.0.execute(
            "UPDATE approval_intents SET agent_id = ?2 WHERE agent_id = ?1",
            params![from, to],
        )?;
        self.0.execute(
            "UPDATE agents SET merged_into = ?2 WHERE merged_into = ?1",
            params![from, to],
        )?;
        Ok((items as u64, intents as u64))
    }

    async fn kill_intents(&mut self, agent_id: &str, now: i64) -> Result<u64, DbError> {
        Ok(self.0.execute(
            "UPDATE approval_intents SET used_at = ?2, body_ct = NULL
             WHERE agent_id = ?1 AND used_at IS NULL AND expires_at > ?2",
            params![agent_id, now],
        )? as u64)
    }

    async fn purge_agents(&mut self, cutoff: i64) -> Result<u64, DbError> {
        Ok(self.0.execute(
            "DELETE FROM agents
             WHERE last_seen_at < ?1
               AND NOT EXISTS (SELECT 1 FROM items i WHERE i.agent_id = agents.agent_id)
               AND NOT EXISTS (SELECT 1 FROM approval_intents n WHERE n.agent_id = agents.agent_id)
               AND NOT EXISTS (SELECT 1 FROM agents m WHERE m.merged_into = agents.agent_id)
               AND (kind = 'agent' OR NOT EXISTS (SELECT 1 FROM agents x WHERE x.key_id = agents.key_id AND x.kind = 'agent'))",
            params![cutoff],
        )? as u64)
    }
}
