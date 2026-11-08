//! The PostgreSQL side of the items: the steps of `items_ops.rs` that touch SQL.
//!
//! The statements are those of `items_db.rs` with the syntax of PostgreSQL. A row that an operation changes is read
//! with `FOR UPDATE`. The sweeper reads its rows with `SKIP LOCKED`, so two instances never take the same row.

use deadpool_postgres::Transaction;
use tokio_postgres::types::ToSql;

use super::db::DbError;
use super::items::{Loaded, NewEvent, Notice, Rules};
use super::items_ops::{ItemTx, ListFilter, Scope, Sealed, TurnRow};
use super::items_sql::{
    approval_values, decode, insert_sql, item_values, select_sql, update_sql, Cols,
    APPROVAL_COLUMNS, ITEM_COLUMNS, V,
};
use super::pg::{db_error, LOCK_ITEMS, WAKE_CHUNK};

/// An open transaction of the inbox.
pub struct PgTx<'a>(pub Transaction<'a>);

type Arg = Box<dyn ToSql + Sync + Send>;

impl Cols for tokio_postgres::Row {
    fn int(&self, i: usize) -> Result<Option<i64>, DbError> {
        self.try_get(i).map_err(db_error)
    }

    fn text(&self, i: usize) -> Result<Option<String>, DbError> {
        self.try_get(i).map_err(db_error)
    }
}

fn args(values: &[V]) -> Vec<Arg> {
    values
        .iter()
        .map(|v| -> Arg {
            match v {
                V::Int(i) => Box::new(*i),
                V::Text(t) => Box::new(t.clone()),
                V::Blob(b) => Box::new(b.clone()),
                V::NullInt => Box::new(Option::<i64>::None),
                V::NullText => Box::new(Option::<String>::None),
                V::NullBlob => Box::new(Option::<Vec<u8>>::None),
            }
        })
        .collect()
}

fn refs(a: &[Arg]) -> Vec<&(dyn ToSql + Sync)> {
    a.iter()
        .map(|x| x.as_ref() as &(dyn ToSql + Sync))
        .collect()
}

fn ph(n: usize) -> String {
    format!("${n}")
}

fn ids(rows: Vec<tokio_postgres::Row>) -> Vec<String> {
    rows.iter().map(|r| r.get(0)).collect()
}

impl PgTx<'_> {
    async fn run(&self, sql: &str, a: &[Arg]) -> Result<u64, DbError> {
        self.0.execute(sql, &refs(a)).await.map_err(db_error)
    }

    async fn rows(&self, sql: &str, a: &[Arg]) -> Result<Vec<tokio_postgres::Row>, DbError> {
        self.0.query(sql, &refs(a)).await.map_err(db_error)
    }
}

fn one(v: impl ToSql + Sync + Send + 'static) -> Vec<Arg> {
    vec![Box::new(v)]
}

impl ItemTx for PgTx<'_> {
    async fn load(&mut self, id: &str) -> Result<Option<Loaded>, DbError> {
        let sql = format!("{} WHERE i.request_id = $1 FOR UPDATE OF i", select_sql());
        let rows = self.rows(&sql, &one(id.to_string())).await?;
        rows.first().map(decode).transpose()
    }

    async fn save(&mut self, l: &Loaded) -> Result<(), DbError> {
        self.run(
            &update_sql("items", &ITEM_COLUMNS, &ph),
            &args(&item_values(&l.item)),
        )
        .await?;
        if let Some(a) = &l.approval {
            self.run(
                &update_sql("approvals", &APPROVAL_COLUMNS, &ph),
                &args(&approval_values(a)),
            )
            .await?;
        }
        Ok(())
    }

    async fn insert(&mut self, l: &Loaded, body: Option<&Sealed>) -> Result<(), DbError> {
        let mut columns = ITEM_COLUMNS.to_vec();
        columns.push("body_ct");
        let mut values = item_values(&l.item);
        values.push(body.map_or(V::NullBlob, |b| V::Blob(b.ct.clone())));
        self.run(&insert_sql("items", &columns, &ph), &args(&values))
            .await?;
        if let Some(a) = &l.approval {
            self.run(
                &insert_sql("approvals", &APPROVAL_COLUMNS, &ph),
                &args(&approval_values(a)),
            )
            .await?;
        }
        Ok(())
    }

    async fn sealed_body(&mut self, id: &str) -> Result<Option<Sealed>, DbError> {
        let rows = self
            .rows(
                "SELECT body_kid, body_ct FROM items WHERE request_id = $1 AND body_ct IS NOT NULL",
                &one(id.to_string()),
            )
            .await?;
        Ok(rows.first().map(|r| Sealed {
            kid: r.get(0),
            ct: r.get(1),
        }))
    }

    async fn find_by_idempotency(
        &mut self,
        key_id: &str,
        key: &str,
    ) -> Result<Option<String>, DbError> {
        let rows = self
            .rows(
                "SELECT request_id FROM items WHERE requester_key_id = $1 AND idempotency_key = $2",
                &[Box::new(key_id.to_string()), Box::new(key.to_string())],
            )
            .await?;
        Ok(ids(rows).into_iter().next())
    }

    async fn find_open_by_hash(&mut self, key_hash: &str) -> Result<Option<String>, DbError> {
        let rows = self
            .rows(
                "SELECT i.request_id FROM approvals a JOIN items i ON i.request_id = a.request_id
                 WHERE a.key_hash = $1 AND i.state IN ('pending', 'replied') LIMIT 1",
                &one(key_hash.to_string()),
            )
            .await?;
        Ok(ids(rows).into_iter().next())
    }

    async fn approvals_by_hash(
        &mut self,
        key_hash: &str,
        limit: u32,
    ) -> Result<Vec<String>, DbError> {
        let rows = self
            .rows(
                "SELECT i.request_id FROM approvals a JOIN items i ON i.request_id = a.request_id
                 WHERE a.key_hash = $1 ORDER BY i.created_at DESC, i.request_id LIMIT $2",
                &[Box::new(key_hash.to_string()), Box::new(limit as i64)],
            )
            .await?;
        Ok(ids(rows))
    }

    async fn count_open(&mut self, scope: Scope<'_>) -> Result<i64, DbError> {
        let open = "state IN ('pending', 'replied')";
        let (sql, a) = match scope {
            Scope::Key(k) => (
                format!(
                    "SELECT COUNT(*)::bigint FROM items WHERE {open} AND requester_key_id = $1"
                ),
                one(k.to_string()),
            ),
            Scope::Owner(o) => (
                format!("SELECT COUNT(*)::bigint FROM items WHERE {open} AND requester_owner = $1"),
                one(o.to_string()),
            ),
            Scope::All => (
                format!("SELECT COUNT(*)::bigint FROM items WHERE {open}"),
                Vec::new(),
            ),
        };
        Ok(self.rows(&sql, &a).await?[0].get(0))
    }

    async fn chain_len(&mut self, chain_id: &str) -> Result<i64, DbError> {
        let rows = self
            .rows(
                "SELECT COUNT(*)::bigint FROM items WHERE chain_id = $1",
                &one(chain_id.to_string()),
            )
            .await?;
        Ok(rows[0].get(0))
    }

    async fn insert_turn(&mut self, t: &TurnRow) -> Result<(), DbError> {
        let a: Vec<Arg> = vec![
            Box::new(t.request_id.clone()),
            Box::new(t.seq as i64),
            Box::new(t.author.clone()),
            Box::new(t.kind.clone()),
            Box::new(t.payload.as_ref().map(|p| p.ct.clone())),
            Box::new(t.payload.as_ref().map(|p| p.kid.clone())),
            Box::new(t.client_msg_id.clone()),
            Box::new(t.device_id.clone()),
            Box::new(t.via.clone()),
            Box::new(t.created_at),
        ];
        self.run(
            "INSERT INTO turns (request_id, seq, author, kind, payload_ct, body_kid, client_msg_id, device_id, via, created_at)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)",
            &a,
        )
        .await?;
        Ok(())
    }

    async fn turn_by_client_msg(
        &mut self,
        request_id: &str,
        cmid: &str,
    ) -> Result<Option<u64>, DbError> {
        let rows = self
            .rows(
                "SELECT seq FROM turns WHERE request_id = $1 AND client_msg_id = $2",
                &[Box::new(request_id.to_string()), Box::new(cmid.to_string())],
            )
            .await?;
        Ok(rows.first().map(|r| r.get::<_, i64>(0) as u64))
    }

    async fn turns(&mut self, request_id: &str) -> Result<Vec<TurnRow>, DbError> {
        let rows = self
            .rows(
                "SELECT seq, author, kind, payload_ct, body_kid, client_msg_id, device_id, via, created_at
                 FROM turns WHERE request_id = $1 ORDER BY seq",
                &one(request_id.to_string()),
            )
            .await?;
        Ok(rows
            .iter()
            .map(|r| {
                let ct: Option<Vec<u8>> = r.get(3);
                let kid: Option<String> = r.get(4);
                TurnRow {
                    request_id: request_id.to_string(),
                    seq: r.get::<_, i64>(0) as u64,
                    author: r.get(1),
                    kind: r.get(2),
                    payload: kid.zip(ct).map(|(kid, ct)| Sealed { kid, ct }),
                    client_msg_id: r.get(5),
                    device_id: r.get(6),
                    via: r.get(7),
                    created_at: r.get(8),
                }
            })
            .collect())
    }

    async fn device_unusable(&mut self, device_id: &str, owner: &str) -> Result<bool, DbError> {
        let rows = self
            .rows(
                "SELECT COUNT(*)::bigint FROM devices WHERE id = $1 AND user_name = $2 AND revoked_at IS NULL",
                &[Box::new(device_id.to_string()), Box::new(owner.to_string())],
            )
            .await?;
        Ok(rows[0].get::<_, i64>(0) == 0)
    }

    async fn owner_devices(&mut self, owner: &str) -> Result<Vec<String>, DbError> {
        let rows = self
            .rows(
                "SELECT id FROM devices WHERE user_name = $1 AND revoked_at IS NULL ORDER BY id",
                &one(owner.to_string()),
            )
            .await?;
        Ok(ids(rows))
    }

    async fn insert_event(&mut self, e: &NewEvent) -> Result<(), DbError> {
        let a: Vec<Arg> = vec![
            Box::new(e.request_id.clone()),
            Box::new(e.at),
            Box::new(e.event.to_string()),
            Box::new(e.actor.clone()),
            Box::new(e.detail.clone()),
        ];
        self.run("INSERT INTO item_events (request_id, at, event, actor, detail) VALUES ($1, $2, $3, $4, $5)", &a)
            .await?;
        Ok(())
    }

    async fn insert_notice(&mut self, n: &Notice, now: i64) -> Result<Vec<String>, DbError> {
        let id = super::util::random_id();
        let key = format!("{}:{}:{}", n.request_id, n.kind, n.seq);
        let a: Vec<Arg> = vec![
            Box::new(id.clone()),
            Box::new(n.kind.to_string()),
            Box::new(n.owner.clone()),
            Box::new(n.title.clone()),
            Box::new(key),
            Box::new(now),
            Box::new(n.request_id.clone()),
            Box::new(n.seq as i64),
        ];
        let created = self
            .run(
                "INSERT INTO alerts (id, kind, target_kind, target_id, title, source, idempotency_key, created_at, request_id, seq)
                 VALUES ($1, $2, 'user', $3, $4, 'item', $5, $6, $7, $8)
                 ON CONFLICT (source, idempotency_key) DO NOTHING",
                &a,
            )
            .await?;
        if created == 0 {
            return Ok(Vec::new());
        }
        let a: Vec<Arg> = vec![
            Box::new(id.clone()),
            Box::new(n.owner.clone()),
            Box::new(n.except_device.clone()),
        ];
        self.run(
            "INSERT INTO deliveries (alert_id, device_id)
             SELECT $1, d.id FROM devices d
             WHERE d.user_name = $2 AND d.revoked_at IS NULL AND ($3::text IS NULL OR d.id <> $3::text)
             ORDER BY d.id",
            &a,
        )
        .await?;
        let rows = self
            .rows(
                "SELECT device_id FROM deliveries WHERE alert_id = $1 ORDER BY device_id",
                &one(id),
            )
            .await?;
        Ok(ids(rows))
    }

    async fn announce(&mut self, devices: &[String], requests: &[String]) -> Result<(), DbError> {
        for chunk in devices.chunks(WAKE_CHUNK) {
            self.run("SELECT pg_notify('inbox_wake', $1)", &one(chunk.join(" ")))
                .await?;
        }
        for chunk in requests.chunks(WAKE_CHUNK) {
            self.run("SELECT pg_notify('inbox_item', $1)", &one(chunk.join(" ")))
                .await?;
        }
        Ok(())
    }

    async fn lock_creation(&mut self) -> Result<(), DbError> {
        self.run("SELECT pg_advisory_xact_lock($1)", &one(LOCK_ITEMS))
            .await?;
        Ok(())
    }

    async fn list(&mut self, f: &ListFilter) -> Result<Vec<Loaded>, DbError> {
        let mut sql = format!("{} WHERE TRUE", select_sql());
        let mut a: Vec<Arg> = Vec::new();
        for (column, wanted) in [
            ("requester_owner", &f.owner),
            ("requester_key_id", &f.key_id),
        ] {
            if let Some(w) = wanted {
                a.push(Box::new(w.clone()));
                sql.push_str(&format!(" AND i.{column} = ${}", a.len()));
            }
        }
        match f.open {
            Some(true) => sql.push_str(" AND i.state IN ('pending', 'replied')"),
            Some(false) => sql.push_str(" AND i.state NOT IN ('pending', 'replied')"),
            None => {}
        }
        a.push(Box::new(f.limit.max(1) as i64));
        sql.push_str(&format!(
            " ORDER BY i.created_at DESC, i.request_id LIMIT ${}",
            a.len()
        ));
        self.rows(&sql, &a).await?.iter().map(decode).collect()
    }

    async fn beat(&mut self, now: i64) -> Result<Option<i64>, DbError> {
        let rows = self
            .rows(
                "SELECT swept_at FROM items_heartbeat WHERE id = 1 FOR UPDATE",
                &[],
            )
            .await?;
        let previous = rows.first().map(|r| r.get::<_, i64>(0));
        self.run(
            "INSERT INTO items_heartbeat (id, swept_at) VALUES (1, $1)
             ON CONFLICT (id) DO UPDATE SET swept_at = EXCLUDED.swept_at",
            &one(now),
        )
        .await?;
        Ok(previous)
    }

    async fn extend_leases(&mut self, gap_ms: i64, max_lifetime_ms: i64) -> Result<u64, DbError> {
        self.run(
            "UPDATE items SET lease_until = CASE WHEN lease_until + $1 < chain_created_at + $2
                                                 THEN lease_until + $1 ELSE chain_created_at + $2 END
             WHERE state IN ('pending', 'replied') AND lease_until IS NOT NULL",
            &[Box::new(gap_ms), Box::new(max_lifetime_ms)],
        )
        .await
    }

    async fn due_candidates(
        &mut self,
        now: i64,
        rules: &Rules,
        limit: u32,
    ) -> Result<Vec<String>, DbError> {
        let rows = self
            .rows(
                "SELECT request_id FROM items
                 WHERE state IN ('pending', 'replied')
                   AND (chain_created_at <= $1 OR (until_at IS NOT NULL AND until_at <= $2)
                        OR (lease_until IS NOT NULL AND lease_until <= $3))
                 ORDER BY request_id LIMIT $4 FOR UPDATE SKIP LOCKED",
                &[
                    Box::new(now - rules.max_lifetime_ms),
                    Box::new(now),
                    Box::new(now - rules.grace_ms),
                    Box::new(limit as i64),
                ],
            )
            .await?;
        Ok(ids(rows))
    }

    async fn lapse_candidates(&mut self, now: i64, limit: u32) -> Result<Vec<String>, DbError> {
        let rows = self
            .rows(
                "SELECT i.request_id FROM approvals a JOIN items i ON i.request_id = a.request_id
                 WHERE i.state = 'answered' AND a.decision = 'approve' AND a.consumed_at IS NULL
                   AND a.lapsed_at IS NULL AND a.consume_before < $1
                 ORDER BY i.request_id LIMIT $2 FOR UPDATE OF i SKIP LOCKED",
                &[Box::new(now), Box::new(limit as i64)],
            )
            .await?;
        Ok(ids(rows))
    }

    async fn purge_bodies(&mut self, now: i64, rules: &Rules) -> Result<u64, DbError> {
        let n = self
            .run(
                "UPDATE items SET body_ct = NULL, body_purged_at = $1
                 WHERE body_ct IS NOT NULL AND state NOT IN ('pending', 'replied') AND final_at IS NOT NULL
                   AND final_at <= CASE WHEN state = 'abandoned' THEN $2::bigint ELSE $3::bigint END",
                &[
                    Box::new(now),
                    Box::new(now - rules.body_purge_ms.max(rules.revive_window_ms)),
                    Box::new(now - rules.body_purge_ms),
                ],
            )
            .await?;
        self.run(
            "UPDATE turns SET payload_ct = NULL WHERE payload_ct IS NOT NULL
               AND request_id IN (SELECT request_id FROM items WHERE body_purged_at IS NOT NULL)",
            &[],
        )
        .await?;
        Ok(n)
    }

    async fn delete_final(&mut self, now: i64, rules: &Rules) -> Result<u64, DbError> {
        self.run(
            "DELETE FROM items WHERE state NOT IN ('pending', 'replied') AND final_at IS NOT NULL AND final_at <= $1",
            &one(now - rules.retention_ms),
        )
        .await
    }

    async fn delete_events(&mut self, now: i64, rules: &Rules) -> Result<u64, DbError> {
        self.run(
            "DELETE FROM item_events WHERE at <= $1",
            &one(now - rules.audit_retention_ms),
        )
        .await
    }

    async fn missing_table(&mut self) -> Result<Option<&'static str>, DbError> {
        for table in super::items_ops::TABLES {
            let rows = self
                .rows(
                    "SELECT COUNT(*)::bigint FROM information_schema.tables
                     WHERE table_schema = current_schema() AND table_name = $1",
                    &one(table.to_string()),
                )
                .await?;
            if rows[0].get::<_, i64>(0) == 0 {
                return Ok(Some(table));
            }
        }
        Ok(None)
    }

    async fn kid_counts(&mut self) -> Result<Vec<(String, u64)>, DbError> {
        let rows = self
            .rows(
                "SELECT body_kid, COUNT(*)::bigint FROM items WHERE body_ct IS NOT NULL AND body_kid IS NOT NULL GROUP BY body_kid
                 UNION ALL SELECT body_kid, COUNT(*)::bigint FROM turns WHERE payload_ct IS NOT NULL AND body_kid IS NOT NULL GROUP BY body_kid
                 UNION ALL SELECT body_kid, COUNT(*)::bigint FROM approval_intents WHERE body_ct IS NOT NULL AND body_kid IS NOT NULL GROUP BY body_kid",
                &[],
            )
            .await?;
        Ok(super::items_ops::merge_counts(
            rows.iter()
                .map(|r| (r.get::<_, String>(0), r.get::<_, i64>(1) as u64))
                .collect(),
        ))
    }

    async fn purge_key(&mut self, kid: &str, now: i64) -> Result<(u64, u64, u64), DbError> {
        let items = self
            .run(
                "UPDATE items SET body_ct = NULL, body_purged_at = $1 WHERE body_kid = $2 AND body_ct IS NOT NULL",
                &[Box::new(now), Box::new(kid.to_string())],
            )
            .await?;
        let turns = self
            .run(
                "UPDATE turns SET payload_ct = NULL WHERE body_kid = $1 AND payload_ct IS NOT NULL",
                &one(kid.to_string()),
            )
            .await?;
        let intents = self
            .run(
                "UPDATE approval_intents SET body_ct = NULL WHERE body_kid = $1 AND body_ct IS NOT NULL",
                &one(kid.to_string()),
            )
            .await?;
        Ok((items, turns, intents))
    }
}
