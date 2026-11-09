//! The SQLite side of the items: the steps of `items_ops.rs` that touch SQL.
//!
//! The database thread runs a closure that opens one transaction and drives an operation to its end. The steps of
//! SQLite never wait, so [`ready`] runs the operation on that thread without a runtime.

use std::future::Future;
use std::task::{Context, Poll, Waker};

use rusqlite::types::Value;
use rusqlite::{params, params_from_iter, OptionalExtension, Transaction, TransactionBehavior};

use super::db::DbError;
use super::items::{Loaded, NewEvent, Notice, OwnerAlert, Rules};
use super::items_ops::{IntentRow, ItemDelivery, ItemTx, ListFilter, Scope, Sealed, TurnRow};
use super::items_sql::PrefsRow;
use super::items_sql::{
    approval_write_values, decode, insert_sql, item_values, select_sql, update_sql, Cols,
    APPROVAL_WRITE_COLUMNS, ITEM_COLUMNS, V,
};
use inbox_core::proto::Prefs;

/// Runs a future whose steps never wait.
pub fn ready<F: Future>(f: F) -> F::Output {
    let mut f = std::pin::pin!(f);
    let mut cx = Context::from_waker(Waker::noop());
    match f.as_mut().poll(&mut cx) {
        Poll::Ready(v) => v,
        Poll::Pending => unreachable!("a step of the SQLite side never waits"),
    }
}

/// An open transaction of the inbox.
pub struct SqliteTx<'t, 'c>(pub &'t mut Transaction<'c>);

impl Cols for rusqlite::Row<'_> {
    fn int(&self, i: usize) -> Result<Option<i64>, DbError> {
        Ok(self.get(i)?)
    }

    fn text(&self, i: usize) -> Result<Option<String>, DbError> {
        Ok(self.get(i)?)
    }

    fn blob(&self, i: usize) -> Result<Option<Vec<u8>>, DbError> {
        Ok(self.get(i)?)
    }
}

pub(super) fn value(v: &V) -> Value {
    match v {
        V::Int(i) => Value::Integer(*i),
        V::Text(t) => Value::Text(t.clone()),
        V::Blob(b) => Value::Blob(b.clone()),
        V::NullInt | V::NullText | V::NullBlob => Value::Null,
    }
}

pub(super) fn ph(n: usize) -> String {
    format!("?{n}")
}

fn turn_from(r: &rusqlite::Row<'_>, request_id: &str) -> rusqlite::Result<TurnRow> {
    let kid: Option<String> = r.get(4)?;
    let ct: Option<Vec<u8>> = r.get(3)?;
    Ok(TurnRow {
        request_id: request_id.to_string(),
        seq: r.get::<_, i64>(0)? as u64,
        author: r.get(1)?,
        kind: r.get(2)?,
        payload: kid.zip(ct).map(|(kid, ct)| Sealed { kid, ct }),
        client_msg_id: r.get(5)?,
        device_id: r.get(6)?,
        via: r.get(7)?,
        created_at: r.get(8)?,
    })
}

impl ItemTx for SqliteTx<'_, '_> {
    async fn load(&mut self, id: &str) -> Result<Option<Loaded>, DbError> {
        let sql = format!("{} WHERE i.request_id = ?1", select_sql());
        let mut stmt = self.0.prepare_cached(&sql)?;
        stmt.query_row(params![id], |r| Ok(decode(r)))
            .optional()?
            .transpose()
    }

    async fn save(&mut self, l: &Loaded) -> Result<(), DbError> {
        let sql = update_sql("items", &ITEM_COLUMNS, &ph);
        self.0
            .prepare_cached(&sql)?
            .execute(params_from_iter(item_values(&l.item).iter().map(value)))?;
        if let Some(a) = &l.approval {
            let sql = update_sql("approvals", &APPROVAL_WRITE_COLUMNS, &ph);
            self.0.prepare_cached(&sql)?.execute(params_from_iter(
                approval_write_values(l, a).iter().map(value),
            ))?;
        }
        Ok(())
    }

    async fn insert(&mut self, l: &Loaded, body: Option<&Sealed>) -> Result<(), DbError> {
        let mut columns = ITEM_COLUMNS.to_vec();
        columns.push("body_ct");
        let mut values = item_values(&l.item);
        values.push(body.map_or(V::NullBlob, |b| V::Blob(b.ct.clone())));
        let sql = insert_sql("items", &columns, &ph);
        self.0
            .prepare_cached(&sql)?
            .execute(params_from_iter(values.iter().map(value)))?;
        if let Some(a) = &l.approval {
            let sql = insert_sql("approvals", &APPROVAL_WRITE_COLUMNS, &ph);
            self.0.prepare_cached(&sql)?.execute(params_from_iter(
                approval_write_values(l, a).iter().map(value),
            ))?;
        }
        Ok(())
    }

    async fn sealed_body(&mut self, id: &str) -> Result<Option<Sealed>, DbError> {
        Ok(self
            .0
            .query_row(
                "SELECT body_kid, body_ct FROM items WHERE request_id = ?1 AND body_ct IS NOT NULL",
                params![id],
                |r| {
                    Ok(Sealed {
                        kid: r.get(0)?,
                        ct: r.get(1)?,
                    })
                },
            )
            .optional()?)
    }

    async fn find_by_idempotency(
        &mut self,
        key_id: &str,
        key: &str,
    ) -> Result<Option<String>, DbError> {
        Ok(self
            .0
            .query_row(
                "SELECT request_id FROM items WHERE requester_key_id = ?1 AND idempotency_key = ?2",
                params![key_id, key],
                |r| r.get(0),
            )
            .optional()?)
    }

    async fn find_open_by_hash(&mut self, key_hash: &str) -> Result<Option<String>, DbError> {
        Ok(self
            .0
            .query_row(
                "SELECT i.request_id FROM approvals a JOIN items i ON i.request_id = a.request_id
                 WHERE a.key_hash = ?1 AND i.state IN ('pending', 'replied') LIMIT 1",
                params![key_hash],
                |r| r.get(0),
            )
            .optional()?)
    }

    async fn approvals_by_hash(
        &mut self,
        key_hash: &str,
        limit: u32,
    ) -> Result<Vec<String>, DbError> {
        let mut stmt = self.0.prepare_cached(
            "SELECT i.request_id FROM approvals a JOIN items i ON i.request_id = a.request_id
             WHERE a.key_hash = ?1 ORDER BY i.created_at DESC, i.request_id LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![key_hash, limit], |r| r.get(0))?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    async fn count_open(&mut self, scope: Scope<'_>) -> Result<i64, DbError> {
        let open = "state IN ('pending', 'replied')";
        let (sql, arg) = match scope {
            Scope::Key(k) => (
                format!("SELECT COUNT(*) FROM items WHERE {open} AND requester_key_id = ?1"),
                Some(k),
            ),
            Scope::Owner(o) => (
                format!("SELECT COUNT(*) FROM items WHERE {open} AND requester_owner = ?1"),
                Some(o),
            ),
            Scope::All => (format!("SELECT COUNT(*) FROM items WHERE {open}"), None),
        };
        let mut stmt = self.0.prepare_cached(&sql)?;
        Ok(match arg {
            Some(a) => stmt.query_row(params![a], |r| r.get(0))?,
            None => stmt.query_row([], |r| r.get(0))?,
        })
    }

    async fn chain_len(&mut self, chain_id: &str) -> Result<i64, DbError> {
        Ok(self.0.query_row(
            "SELECT COUNT(*) FROM items WHERE chain_id = ?1",
            params![chain_id],
            |r| r.get(0),
        )?)
    }

    async fn insert_turn(&mut self, t: &TurnRow) -> Result<(), DbError> {
        self.0.prepare_cached(
            "INSERT INTO turns (request_id, seq, author, kind, payload_ct, body_kid, client_msg_id, device_id, via, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
        )?
        .execute(params![
            t.request_id,
            t.seq as i64,
            t.author,
            t.kind,
            t.payload.as_ref().map(|p| p.ct.clone()),
            t.payload.as_ref().map(|p| p.kid.clone()),
            t.client_msg_id,
            t.device_id,
            t.via,
            t.created_at,
        ])?;
        Ok(())
    }

    async fn turn_by_client_msg(
        &mut self,
        request_id: &str,
        cmid: &str,
    ) -> Result<Option<u64>, DbError> {
        Ok(self
            .0
            .query_row(
                "SELECT seq FROM turns WHERE request_id = ?1 AND client_msg_id = ?2",
                params![request_id, cmid],
                |r| r.get::<_, i64>(0),
            )
            .optional()?
            .map(|s| s as u64))
    }

    async fn turns(&mut self, request_id: &str) -> Result<Vec<TurnRow>, DbError> {
        let mut stmt = self.0.prepare_cached(
            "SELECT seq, author, kind, payload_ct, body_kid, client_msg_id, device_id, via, created_at
             FROM turns WHERE request_id = ?1 ORDER BY seq",
        )?;
        let rows = stmt.query_map(params![request_id], |r| turn_from(r, request_id))?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    async fn device_unusable(&mut self, device_id: &str, owner: &str) -> Result<bool, DbError> {
        let n: i64 = self.0.query_row(
            "SELECT COUNT(*) FROM devices WHERE id = ?1 AND user_name = ?2 AND revoked_at IS NULL",
            params![device_id, owner],
            |r| r.get(0),
        )?;
        Ok(n == 0)
    }

    async fn owner_devices(&mut self, owner: &str) -> Result<Vec<String>, DbError> {
        let mut stmt = self.0.prepare_cached(
            "SELECT id FROM devices WHERE user_name = ?1 AND revoked_at IS NULL ORDER BY id",
        )?;
        let rows = stmt.query_map(params![owner], |r| r.get(0))?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    async fn insert_event(&mut self, e: &NewEvent) -> Result<(), DbError> {
        self.0
            .prepare_cached("INSERT INTO item_events (request_id, at, event, actor, detail) VALUES (?1, ?2, ?3, ?4, ?5)")?
            .execute(params![e.request_id, e.at, e.event, e.actor, e.detail])?;
        Ok(())
    }

    async fn pending_item_deliveries(
        &mut self,
        device_id: &str,
        limit: u32,
    ) -> Result<Vec<ItemDelivery>, DbError> {
        let mut stmt = self.0.prepare_cached(
            "SELECT d.seq, a.id, a.kind, a.request_id, COALESCE(a.seq, 0), a.created_at
             FROM deliveries d JOIN alerts a ON a.id = d.alert_id
             WHERE d.device_id = ?1 AND d.received_at IS NULL AND a.request_id IS NOT NULL
             ORDER BY d.seq LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![device_id, limit], |r| {
            Ok(ItemDelivery {
                seq: r.get(0)?,
                alert_id: r.get(1)?,
                kind: r.get(2)?,
                request_id: r.get(3)?,
                item_seq: r.get::<_, i64>(4)? as u64,
                created_at: r.get(5)?,
            })
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    async fn catch_up_device(&mut self, device_id: &str, owner: &str) -> Result<u64, DbError> {
        let added = self.0.execute(
            "INSERT INTO deliveries (alert_id, device_id)
             SELECT a.id, ?1 FROM alerts a JOIN items i ON i.request_id = a.request_id
             WHERE i.requester_owner = ?2 AND i.state IN ('pending', 'replied')
               AND (a.kind = 'turn'
                    OR (a.kind IN ('question', 'approval')
                        AND a.seq = (SELECT MAX(a2.seq) FROM alerts a2
                                     WHERE a2.request_id = a.request_id AND a2.kind IN ('question', 'approval'))))
               AND NOT EXISTS (SELECT 1 FROM deliveries d JOIN alerts a3 ON a3.id = d.alert_id
                               WHERE d.device_id = ?1 AND a3.request_id = a.request_id
                                 AND a3.kind IN ('question', 'approval'))
             ORDER BY a.created_at, a.seq
             ON CONFLICT (alert_id, device_id) DO NOTHING",
            params![device_id, owner],
        )?;
        Ok(added as u64)
    }

    async fn previous_approval(
        &mut self,
        key_hash: &str,
        except_id: &str,
    ) -> Result<Option<(i64, bool)>, DbError> {
        Ok(self
            .0
            .query_row(
                "SELECT i.answered_at, a.consumed_at IS NOT NULL
                 FROM approvals a JOIN items i ON i.request_id = a.request_id
                 WHERE a.key_hash = ?1 AND a.request_id <> ?2 AND a.decision = 'approve' AND i.answered_at IS NOT NULL
                 ORDER BY i.answered_at DESC LIMIT 1",
                params![key_hash, except_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?)
    }

    async fn prefs(&mut self, login: &str) -> Result<Prefs, DbError> {
        let row = self
            .0
            .query_row(
                "SELECT digest_mode, digest_at, digest_tz, digest_every_h, quiet_from, quiet_to, quiet_tz, quiet_high_rings
                 FROM account_prefs WHERE login = ?1",
                params![login],
                |r| {
                    Ok(PrefsRow {
                        digest_mode: r.get(0)?,
                        digest_at: r.get(1)?,
                        digest_tz: r.get(2)?,
                        digest_every_h: r.get(3)?,
                        quiet_from: r.get(4)?,
                        quiet_to: r.get(5)?,
                        quiet_tz: r.get(6)?,
                        quiet_high_rings: r.get(7)?,
                    })
                },
            )
            .optional()?;
        Ok(row.map(PrefsRow::into_prefs).unwrap_or_default())
    }

    async fn put_prefs(&mut self, login: &str, prefs: &Prefs) -> Result<(), DbError> {
        let r = PrefsRow::from_prefs(prefs);
        self.0.execute(
            "INSERT INTO account_prefs (login, digest_mode, digest_at, digest_tz, digest_every_h, quiet_from, quiet_to, quiet_tz, quiet_high_rings)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
             ON CONFLICT (login) DO UPDATE SET digest_mode = excluded.digest_mode, digest_at = excluded.digest_at,
               digest_tz = excluded.digest_tz, digest_every_h = excluded.digest_every_h, quiet_from = excluded.quiet_from,
               quiet_to = excluded.quiet_to, quiet_tz = excluded.quiet_tz, quiet_high_rings = excluded.quiet_high_rings",
            params![
                login, r.digest_mode, r.digest_at, r.digest_tz, r.digest_every_h, r.quiet_from, r.quiet_to, r.quiet_tz,
                r.quiet_high_rings
            ],
        )?;
        Ok(())
    }

    async fn open_owners(&mut self) -> Result<Vec<(String, i64)>, DbError> {
        let mut stmt = self.0.prepare_cached(
            "SELECT requester_owner, MIN(created_at) FROM items WHERE state IN ('pending', 'replied')
             GROUP BY requester_owner ORDER BY requester_owner",
        )?;
        let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    async fn insert_notice(&mut self, n: &Notice, now: i64) -> Result<Vec<String>, DbError> {
        let id = super::util::random_id();
        let key = format!("{}:{}:{}", n.request_id, n.kind, n.seq);
        let created = self
            .0
            .prepare_cached(
                "INSERT INTO alerts (id, kind, target_kind, target_id, title, source, idempotency_key, created_at, request_id, seq)
                 VALUES (?1, ?2, 'user', ?3, ?4, 'item', ?5, ?6, ?7, ?8)
                 ON CONFLICT (source, idempotency_key) DO NOTHING",
            )?
            .execute(params![id, n.kind, n.owner, n.title, key, now, n.request_id, n.seq as i64])?;
        if created == 0 {
            return Ok(Vec::new());
        }
        self.0
            .prepare_cached(
                "INSERT INTO deliveries (alert_id, device_id)
                 SELECT ?1, d.id FROM devices d
                 WHERE d.user_name = ?2 AND d.revoked_at IS NULL AND (?3 IS NULL OR d.id <> ?3)
                 ORDER BY d.id",
            )?
            .execute(params![id, n.owner, n.except_device])?;
        let mut stmt = self.0.prepare_cached(
            "SELECT device_id FROM deliveries WHERE alert_id = ?1 ORDER BY device_id",
        )?;
        let rows = stmt.query_map(params![id], |r| r.get(0))?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    async fn announce(&mut self, _devices: &[String], _requests: &[String]) -> Result<(), DbError> {
        Ok(())
    }

    async fn lock_creation(&mut self) -> Result<(), DbError> {
        // The transaction is `IMMEDIATE`: there is one writer.
        Ok(())
    }

    async fn list(&mut self, f: &ListFilter) -> Result<Vec<Loaded>, DbError> {
        let mut sql = format!("{} WHERE 1 = 1", select_sql());
        let mut args: Vec<Value> = Vec::new();
        for (column, wanted) in [
            ("requester_owner", &f.owner),
            ("requester_key_id", &f.key_id),
            ("agent_id", &f.agent_id),
        ] {
            if let Some(w) = wanted {
                args.push(Value::Text(w.clone()));
                sql.push_str(&format!(" AND i.{column} = ?{}", args.len()));
            }
        }
        match f.open {
            Some(true) => sql.push_str(" AND i.state IN ('pending', 'replied')"),
            Some(false) => sql.push_str(" AND i.state NOT IN ('pending', 'replied')"),
            None => {}
        }
        args.push(Value::Integer(f.limit.max(1) as i64));
        sql.push_str(&format!(
            " ORDER BY i.created_at DESC, i.request_id LIMIT ?{}",
            args.len()
        ));
        let mut stmt = self.0.prepare_cached(&sql)?;
        let rows = stmt.query_map(params_from_iter(args), |r| Ok(decode(r)))?;
        rows.map(|r| r?).collect()
    }

    async fn created_since(
        &mut self,
        key_id: &str,
        since: i64,
    ) -> Result<(i64, Option<i64>), DbError> {
        Ok(self.0.query_row(
            "SELECT COUNT(*), MIN(created_at) FROM items
             WHERE kind = 'approval' AND requester_key_id = ?1 AND created_at > ?2",
            params![key_id, since],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?)
    }

    async fn mute_until(&mut self, subject: &str) -> Result<Option<i64>, DbError> {
        Ok(self
            .0
            .query_row(
                "SELECT muted_until FROM approval_mute WHERE subject = ?1",
                params![subject],
                |r| r.get(0),
            )
            .optional()?)
    }

    async fn set_mute(&mut self, subject: &str, until: i64, reason: &str) -> Result<(), DbError> {
        self.0.execute(
            "INSERT INTO approval_mute (subject, muted_until, reason) VALUES (?1, ?2, ?3)
             ON CONFLICT (subject) DO UPDATE SET muted_until = excluded.muted_until, reason = excluded.reason",
            params![subject, until, reason],
        )?;
        Ok(())
    }

    async fn recent_answers(
        &mut self,
        key_id: &str,
        since: i64,
        limit: u32,
    ) -> Result<Vec<Option<String>>, DbError> {
        let mut stmt = self.0.prepare_cached(
            "SELECT answer_action FROM items
             WHERE kind = 'approval' AND requester_key_id = ?1 AND state = 'answered' AND answered_at > ?2
             ORDER BY answered_at DESC, request_id LIMIT ?3",
        )?;
        let rows = stmt.query_map(params![key_id, since, limit], |r| r.get(0))?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    async fn insert_intent(&mut self, row: &IntentRow) -> Result<(), DbError> {
        self.0.execute(
            "INSERT INTO approval_intents (ref_digest, requester_key_id, agent_id, lease_s, until_at, allow_custom, allow_ask_back,
                 body_kid, body_ct, body_size, expires_at, used_at, request_id, previous_id)
             VALUES (?1, ?2, ?14, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
            params![
                row.ref_digest,
                row.requester_key_id,
                row.lease_s,
                row.until_at,
                row.allow_custom as i64,
                row.allow_ask_back as i64,
                row.body.as_ref().map(|b| b.kid.clone()),
                row.body.as_ref().map(|b| b.ct.clone()),
                row.body_size,
                row.expires_at,
                row.used_at,
                row.request_id,
                row.previous_id,
                row.agent_id
            ],
        )?;
        Ok(())
    }

    async fn live_intents(&mut self, key_id: &str, now: i64) -> Result<i64, DbError> {
        Ok(self.0.query_row(
            "SELECT COUNT(*) FROM approval_intents WHERE requester_key_id = ?1 AND used_at IS NULL AND expires_at > ?2",
            params![key_id, now],
            |r| r.get(0),
        )?)
    }

    async fn find_intent(&mut self, digest: &str) -> Result<Option<IntentRow>, DbError> {
        Ok(self
            .0
            .query_row(
                "SELECT requester_key_id, lease_s, until_at, allow_custom, allow_ask_back, body_kid, body_ct, body_size,
                        expires_at, used_at, request_id, previous_id, agent_id
                 FROM approval_intents WHERE ref_digest = ?1",
                params![digest],
                |r| {
                    let kid: Option<String> = r.get(5)?;
                    let ct: Option<Vec<u8>> = r.get(6)?;
                    Ok(IntentRow {
                        ref_digest: digest.to_string(),
                        requester_key_id: r.get(0)?,
                        lease_s: r.get(1)?,
                        until_at: r.get(2)?,
                        allow_custom: r.get::<_, i64>(3)? != 0,
                        allow_ask_back: r.get::<_, i64>(4)? != 0,
                        body: kid.zip(ct).map(|(kid, ct)| Sealed { kid, ct }),
                        body_size: r.get(7)?,
                        expires_at: r.get(8)?,
                        used_at: r.get(9)?,
                        request_id: r.get(10)?,
                        previous_id: r.get(11)?,
                        agent_id: r.get(12)?,
                    })
                },
            )
            .optional()?)
    }

    async fn use_intent(
        &mut self,
        digest: &str,
        key_id: &str,
        now: i64,
        request_id: &str,
    ) -> Result<bool, DbError> {
        let n = self.0.execute(
            "UPDATE approval_intents SET used_at = ?1, request_id = ?2, body_ct = NULL
             WHERE ref_digest = ?3 AND requester_key_id = ?4 AND used_at IS NULL AND expires_at > ?1",
            params![now, request_id, digest, key_id],
        )?;
        Ok(n == 1)
    }

    async fn delete_intents(&mut self, now: i64) -> Result<u64, DbError> {
        Ok(self.0.execute(
            "DELETE FROM approval_intents WHERE expires_at < ?1",
            params![now],
        )? as u64)
    }

    async fn insert_owner_alert(
        &mut self,
        a: &OwnerAlert,
        now: i64,
    ) -> Result<Vec<String>, DbError> {
        let id = super::util::random_id();
        let created = self
            .0
            .prepare_cached(
                "INSERT INTO alerts (id, kind, target_kind, target_id, title, source, idempotency_key, created_at)
                 VALUES (?1, ?6, 'user', ?2, ?3, 'portal', ?4, ?5)
                 ON CONFLICT (source, idempotency_key) DO NOTHING",
            )?
            .execute(params![id, a.owner, a.title, a.key, now, if a.digest { "digest" } else { "alert" }])?;
        if created == 0 {
            return Ok(Vec::new());
        }
        self.0
            .prepare_cached(
                "INSERT INTO deliveries (alert_id, device_id)
                 SELECT ?1, d.id FROM devices d WHERE d.user_name = ?2 AND d.revoked_at IS NULL ORDER BY d.id",
            )?
            .execute(params![id, a.owner])?;
        let mut stmt = self.0.prepare_cached(
            "SELECT device_id FROM deliveries WHERE alert_id = ?1 ORDER BY device_id",
        )?;
        let rows = stmt.query_map(params![id], |r| r.get(0))?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    async fn beat(&mut self, now: i64) -> Result<Option<i64>, DbError> {
        let previous = self
            .0
            .query_row(
                "SELECT swept_at FROM items_heartbeat WHERE id = 1",
                [],
                |r| r.get(0),
            )
            .optional()?;
        self.0.execute(
            "INSERT INTO items_heartbeat (id, swept_at) VALUES (1, ?1)
             ON CONFLICT (id) DO UPDATE SET swept_at = excluded.swept_at",
            params![now],
        )?;
        Ok(previous)
    }

    async fn extend_leases(&mut self, gap_ms: i64, max_lifetime_ms: i64) -> Result<u64, DbError> {
        Ok(self.0.execute(
            "UPDATE items SET lease_until = CASE WHEN lease_until + ?1 < chain_created_at + ?2
                                                 THEN lease_until + ?1 ELSE chain_created_at + ?2 END
             WHERE state IN ('pending', 'replied') AND lease_until IS NOT NULL",
            params![gap_ms, max_lifetime_ms],
        )? as u64)
    }

    async fn due_candidates(
        &mut self,
        now: i64,
        rules: &Rules,
        limit: u32,
    ) -> Result<Vec<String>, DbError> {
        let mut stmt = self.0.prepare_cached(
            "SELECT request_id FROM items
             WHERE state IN ('pending', 'replied')
               AND (chain_created_at <= ?1 OR (until_at IS NOT NULL AND until_at <= ?2)
                    OR (lease_until IS NOT NULL AND lease_until <= ?3))
             ORDER BY request_id LIMIT ?4",
        )?;
        let rows = stmt.query_map(
            params![
                now - rules.max_lifetime_ms,
                now,
                now - rules.grace_ms,
                limit
            ],
            |r| r.get(0),
        )?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    async fn lapse_candidates(&mut self, now: i64, limit: u32) -> Result<Vec<String>, DbError> {
        let mut stmt = self.0.prepare_cached(
            "SELECT i.request_id FROM approvals a JOIN items i ON i.request_id = a.request_id
             WHERE i.state = 'answered' AND a.decision = 'approve' AND a.consumed_at IS NULL
               AND a.lapsed_at IS NULL AND a.consume_before < ?1
             ORDER BY i.request_id LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![now, limit], |r| r.get(0))?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    async fn purge_bodies(&mut self, now: i64, rules: &Rules) -> Result<u64, DbError> {
        let n = self.0.execute(
            "UPDATE items SET body_ct = NULL, body_purged_at = ?1
             WHERE body_ct IS NOT NULL AND state NOT IN ('pending', 'replied') AND final_at IS NOT NULL
               AND final_at <= CASE WHEN state = 'abandoned' THEN ?2 ELSE ?3 END",
            params![
                now,
                now - rules.body_purge_ms.max(rules.revive_window_ms),
                now - rules.body_purge_ms
            ],
        )?;
        self.0.execute(
            "UPDATE turns SET payload_ct = NULL WHERE payload_ct IS NOT NULL
               AND request_id IN (SELECT request_id FROM items WHERE body_purged_at IS NOT NULL)",
            [],
        )?;
        Ok(n as u64)
    }

    async fn delete_final(&mut self, now: i64, rules: &Rules) -> Result<u64, DbError> {
        Ok(self.0.execute(
            "DELETE FROM items WHERE state NOT IN ('pending', 'replied') AND final_at IS NOT NULL AND final_at <= ?1",
            params![now - rules.retention_ms],
        )? as u64)
    }

    async fn delete_events(&mut self, now: i64, rules: &Rules) -> Result<u64, DbError> {
        Ok(self.0.execute(
            "DELETE FROM item_events WHERE at <= ?1",
            params![now - rules.audit_retention_ms],
        )? as u64)
    }

    async fn missing_table(&mut self) -> Result<Option<&'static str>, DbError> {
        for table in super::items_ops::TABLES {
            let n: i64 = self.0.query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = ?1",
                params![table],
                |r| r.get(0),
            )?;
            if n == 0 {
                return Ok(Some(table));
            }
        }
        Ok(None)
    }

    async fn kid_counts(&mut self) -> Result<Vec<(String, u64)>, DbError> {
        let mut stmt = self.0.prepare_cached(
            "SELECT body_kid, COUNT(*) FROM items WHERE body_ct IS NOT NULL AND body_kid IS NOT NULL GROUP BY body_kid
             UNION ALL SELECT body_kid, COUNT(*) FROM turns WHERE payload_ct IS NOT NULL AND body_kid IS NOT NULL GROUP BY body_kid
             UNION ALL SELECT body_kid, COUNT(*) FROM approval_intents WHERE body_ct IS NOT NULL AND body_kid IS NOT NULL GROUP BY body_kid
             UNION ALL SELECT hints_kid, COUNT(*) FROM agents WHERE hints_ct IS NOT NULL AND hints_kid IS NOT NULL GROUP BY hints_kid",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)? as u64))
        })?;
        Ok(super::items_ops::merge_counts(
            rows.collect::<Result<Vec<_>, _>>()?,
        ))
    }

    async fn purge_key(&mut self, kid: &str, now: i64) -> Result<(u64, u64, u64, u64), DbError> {
        let items = self.0.execute(
            "UPDATE items SET body_ct = NULL, body_purged_at = ?1 WHERE body_kid = ?2 AND body_ct IS NOT NULL",
            params![now, kid],
        )?;
        let turns = self.0.execute(
            "UPDATE turns SET payload_ct = NULL WHERE body_kid = ?1 AND payload_ct IS NOT NULL",
            params![kid],
        )?;
        let intents = self.0.execute(
            "UPDATE approval_intents SET body_ct = NULL WHERE body_kid = ?1 AND body_ct IS NOT NULL",
            params![kid],
        )?;
        let agents = self.0.execute(
            "UPDATE agents SET hints_ct = NULL WHERE hints_kid = ?1 AND hints_ct IS NOT NULL",
            params![kid],
        )?;
        Ok((items as u64, turns as u64, intents as u64, agents as u64))
    }
}

/// Runs one operation of the items in one transaction. An error rolls back. A refusal is a value, and the
/// transaction commits what the operation wrote before it.
pub fn in_tx<R>(
    conn: &mut rusqlite::Connection,
    f: impl FnOnce(&mut SqliteTx<'_, '_>) -> Result<R, DbError>,
) -> Result<R, DbError> {
    let mut tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let out = f(&mut SqliteTx(&mut tx))?;
    tx.commit()?;
    Ok(out)
}

/// `portal inbox content-purge` on a SQLite file. It opens the file as a plain connection: it does not run the
/// migrations and it does not touch the rows of the sessions that a running instance owns.
pub fn purge_key_file(path: &std::path::Path, kid: &str) -> Result<(u64, u64, u64, u64), String> {
    let mut conn = rusqlite::Connection::open(path)
        .map_err(|e| format!("storage.database: cannot open {}: {e}", path.display()))?;
    conn.busy_timeout(std::time::Duration::from_secs(5))
        .map_err(|e| e.to_string())?;
    in_tx(&mut conn, |tx| {
        ready(tx.purge_key(kid, super::util::now_ms()))
    })
    .map_err(|e| format!("inbox storage: {e}"))
}
