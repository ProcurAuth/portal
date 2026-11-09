//! The PostgreSQL side of the agents: the steps of `agents_ops.rs` with the syntax of PostgreSQL. The row that a
//! human action changes is read with `FOR UPDATE`.

use super::agents::{AgentEventRow, AgentRow};
use super::agents_ops::AgentTx;
use super::agents_sql::{agent_values, decode_agent, decode_event, select_agents, AGENT_COLUMNS};
use super::db::DbError;
use super::items_ops::Sealed;
use super::items_pg::{args, ph, Arg, PgTx};
use super::items_sql::{insert_sql, update_sql};

impl PgTx<'_> {
    async fn agent_where(
        &mut self,
        clause: &str,
        arg: &str,
        lock: bool,
    ) -> Result<Option<AgentRow>, DbError> {
        let sql = format!(
            "{} WHERE {clause}{}",
            select_agents(false),
            if lock { " FOR UPDATE OF a" } else { "" }
        );
        let rows = self.rows(&sql, &[Box::new(arg.to_string())]).await?;
        rows.first().map(decode_agent).transpose()
    }

    async fn number(&mut self, sql: &str, a: &[Arg]) -> Result<i64, DbError> {
        Ok(self.rows(sql, a).await?[0].get(0))
    }
}

fn text(v: &str) -> Arg {
    Box::new(v.to_string())
}

impl AgentTx for PgTx<'_> {
    async fn agent_by_id(&mut self, id: &str) -> Result<Option<AgentRow>, DbError> {
        self.agent_where("a.agent_id = $1", id, false).await
    }

    async fn agent_locked(&mut self, id: &str) -> Result<Option<AgentRow>, DbError> {
        self.agent_where("a.agent_id = $1", id, true).await
    }

    async fn agent_by_digest(&mut self, digest: &str) -> Result<Option<AgentRow>, DbError> {
        self.agent_where("a.token_digest = $1", digest, false).await
    }

    async fn bucket_of(&mut self, key_id: &str) -> Result<Option<AgentRow>, DbError> {
        self.agent_where("a.key_id = $1 AND a.kind = 'bucket'", key_id, false)
            .await
    }

    async fn insert_agent(&mut self, a: &AgentRow) -> Result<(), DbError> {
        self.run(
            &insert_sql("agents", &AGENT_COLUMNS, &ph),
            &args(&agent_values(a)),
        )
        .await?;
        Ok(())
    }

    async fn insert_bucket(&mut self, a: &AgentRow) -> Result<(), DbError> {
        let sql = format!(
            "{} ON CONFLICT (key_id) WHERE kind = 'bucket' DO NOTHING",
            insert_sql("agents", &AGENT_COLUMNS, &ph)
        );
        self.run(&sql, &args(&agent_values(a))).await?;
        Ok(())
    }

    async fn save_agent(&mut self, a: &AgentRow) -> Result<(), DbError> {
        self.run(
            &update_sql("agents", &AGENT_COLUMNS, &ph),
            &args(&agent_values(a)),
        )
        .await?;
        Ok(())
    }

    async fn touch_agent(&mut self, id: &str, now: i64) -> Result<(), DbError> {
        self.run(
            "UPDATE agents SET last_seen_at = $2 WHERE agent_id = $1 AND last_seen_at < $2",
            &[text(id), Box::new(now)],
        )
        .await?;
        Ok(())
    }

    async fn set_hints(
        &mut self,
        id: &str,
        expected_rev: i64,
        hints: &Sealed,
        now: i64,
    ) -> Result<bool, DbError> {
        let n = self
            .run(
                "UPDATE agents SET hints_kid = $3, hints_ct = $4, rev = rev + 1,
                     last_seen_at = CASE WHEN last_seen_at < $5 THEN $5 ELSE last_seen_at END
                 WHERE agent_id = $1 AND rev = $2",
                &[
                    text(id),
                    Box::new(expected_rev),
                    text(&hints.kid),
                    Box::new(hints.ct.clone()),
                    Box::new(now),
                ],
            )
            .await?;
        Ok(n == 1)
    }

    async fn count_agents(&mut self, key_id: &str) -> Result<i64, DbError> {
        self.number(
            "SELECT COUNT(*)::bigint FROM agents WHERE key_id = $1 AND kind = 'agent' AND state IN ('new', 'known', 'blocked')",
            &[text(key_id)],
        )
        .await
    }

    async fn count_made_since(&mut self, key_id: &str, since: i64) -> Result<i64, DbError> {
        self.number(
            "SELECT COUNT(*)::bigint FROM agents WHERE key_id = $1 AND kind = 'agent' AND origin = 'call' AND created_at > $2",
            &[text(key_id), Box::new(since)],
        )
        .await
    }

    async fn bump_ordinal(&mut self, key_id: &str) -> Result<i64, DbError> {
        self.number(
            "UPDATE agents SET ordinal = ordinal + 1 WHERE key_id = $1 AND kind = 'bucket' RETURNING ordinal",
            &[text(key_id)],
        )
        .await
    }

    async fn name_taken(
        &mut self,
        key_id: &str,
        name_key: &str,
        except_id: &str,
    ) -> Result<bool, DbError> {
        Ok(self
            .number(
                "SELECT COUNT(*)::bigint FROM agents WHERE key_id = $1 AND name_key = $2 AND agent_id <> $3",
                &[text(key_id), text(name_key), text(except_id)],
            )
            .await?
            > 0)
    }

    async fn list_agents(
        &mut self,
        owner: Option<&str>,
        key_id: Option<&str>,
    ) -> Result<Vec<(AgentRow, i64)>, DbError> {
        let mut sql = format!("{} WHERE TRUE", select_agents(true));
        let mut a: Vec<Arg> = Vec::new();
        for (column, wanted) in [("owner", owner), ("key_id", key_id)] {
            if let Some(w) = wanted {
                a.push(text(w));
                sql.push_str(&format!(" AND a.{column} = ${}", a.len()));
            }
        }
        sql.push_str(" ORDER BY a.created_at, a.agent_id");
        let rows = self.rows(&sql, &a).await?;
        rows.iter()
            .map(|r| {
                Ok((
                    decode_agent(r)?,
                    r.try_get::<_, i64>(AGENT_COLUMNS.len())
                        .map_err(super::pg::db_error)?,
                ))
            })
            .collect()
    }

    async fn insert_agent_event(
        &mut self,
        agent_id: &str,
        at: i64,
        event: &str,
        actor: &str,
        detail: Option<&str>,
    ) -> Result<(), DbError> {
        self.run(
            "INSERT INTO agent_events (agent_id, at, event, actor, detail) VALUES ($1, $2, $3, $4, $5)",
            &[
                text(agent_id),
                Box::new(at),
                text(event),
                text(actor),
                Box::new(detail.map(str::to_string)),
            ],
        )
        .await?;
        Ok(())
    }

    async fn agent_events(
        &mut self,
        agent_id: &str,
        limit: u32,
    ) -> Result<Vec<AgentEventRow>, DbError> {
        let rows = self
            .rows(
                "SELECT at, event, actor, detail FROM agent_events WHERE agent_id = $1 ORDER BY at DESC, id DESC LIMIT $2",
                &[text(agent_id), Box::new(i64::from(limit))],
            )
            .await?;
        rows.iter().map(decode_event).collect()
    }

    async fn merge_rows(&mut self, from: &str, to: &str) -> Result<(u64, u64), DbError> {
        let ft = || [text(from), text(to)];
        let items = self
            .run("UPDATE items SET agent_id = $2 WHERE agent_id = $1", &ft())
            .await?;
        let intents = self
            .run(
                "UPDATE approval_intents SET agent_id = $2 WHERE agent_id = $1",
                &ft(),
            )
            .await?;
        self.run(
            "UPDATE agents SET merged_into = $2 WHERE merged_into = $1",
            &ft(),
        )
        .await?;
        Ok((items, intents))
    }

    async fn kill_intents(&mut self, agent_id: &str, now: i64) -> Result<u64, DbError> {
        self.run(
            "UPDATE approval_intents SET used_at = $2, body_ct = NULL
             WHERE agent_id = $1 AND used_at IS NULL AND expires_at > $2",
            &[text(agent_id), Box::new(now)],
        )
        .await
    }

    async fn purge_agents(&mut self, cutoff: i64) -> Result<u64, DbError> {
        self.run(
            "DELETE FROM agents
             WHERE last_seen_at < $1
               AND NOT EXISTS (SELECT 1 FROM items i WHERE i.agent_id = agents.agent_id)
               AND NOT EXISTS (SELECT 1 FROM approval_intents n WHERE n.agent_id = agents.agent_id)
               AND NOT EXISTS (SELECT 1 FROM agents m WHERE m.merged_into = agents.agent_id)
               AND (kind = 'agent' OR NOT EXISTS (SELECT 1 FROM agents x WHERE x.key_id = agents.key_id AND x.kind = 'agent'))",
            &[Box::new(cutoff)],
        )
        .await
    }
}
