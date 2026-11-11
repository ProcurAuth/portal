//! The keys, the directory version and the counter rows in PostgreSQL.
//!
//! Each write to the users, the grants or the keys first adds 1 to `directory_version` in its
//! transaction (portal 10.7). This takes the lock of the one row, so two writes never run at the
//! same time, and the counts of the write (portal 5.4, 7.2f, 10.7d) are exact. A write that adds
//! a limit value makes the counter rows of the value in the same transaction, with the
//! synchronous commit of the write (portal 7.2a, 7.7b).

use postgres::{GenericClient, IsolationLevel, Transaction};

use super::*;
use crate::decision::{Counter, CounterOwner};
use crate::grants::{counter_ids, CounterId};

/// The most rows that one insert statement makes (portal 7.7b).
const ROWS_PER_STATEMENT: usize = 1_000;

/// Who owns the counters of a write.
pub(super) enum Subject<'a> {
    User(i64),
    Key(&'a str),
}

/// Adds 1 to the version of the directory and keeps the lock of its row until the commit.
pub(super) fn bump(tx: &mut Transaction<'_>) -> Result<(), postgres::Error> {
    tx.execute(
        "UPDATE directory_version SET version = version + 1 WHERE id = 1",
        &[],
    )
    .map(drop)
}

/// Makes the counter rows that the grants need and that the table does not hold yet.
pub(super) fn insert_counter_rows(
    tx: &mut Transaction<'_>,
    subject: Subject<'_>,
    grants: &[Grant],
) -> Result<(), postgres::Error> {
    for CounterId {
        service,
        budget,
        window,
    } in counter_ids(grants)
    {
        let window = window.name();
        match subject {
            Subject::User(id) => tx.execute(
                "INSERT INTO budget_counters (user_id, service, budget, \"window\", window_start, units)
                 VALUES ($1, $2, $3, $4, 0, 0) ON CONFLICT DO NOTHING",
                &[&id, &service, &budget, &window],
            ),
            Subject::Key(id) => tx.execute(
                "INSERT INTO budget_counters (key_id, service, budget, \"window\", window_start, units)
                 VALUES ($1, $2, $3, $4, 0, 0) ON CONFLICT DO NOTHING",
                &[&id, &service, &budget, &window],
            ),
        }?;
    }
    Ok(())
}

/// The bounds of portal 7.2f and 10.7d, checked after the write inserted its rows. The caller
/// drops the transaction when this answers `false`.
fn within_capacity(tx: &mut Transaction<'_>, caps: &KeyCaps) -> Result<bool, postgres::Error> {
    let counters: i64 = tx
        .query_one("SELECT COUNT(*) FROM budget_counters", &[])?
        .get(0);
    let grants: i64 = tx
        .query_one(
            "SELECT (SELECT COUNT(*) FROM user_grants) + (SELECT COUNT(*) FROM key_grants)",
            &[],
        )?
        .get(0);
    Ok(counters <= caps.max_counter_rows as i64 && grants <= caps.max_directory_grants as i64)
}

pub(super) fn insert_key_grants(
    tx: &mut Transaction<'_>,
    key_id: &str,
    grants: &[Grant],
) -> Result<(), postgres::Error> {
    for grant in grants {
        let (kind, id, path, limits, confirm) = grant_columns(grant);
        tx.execute(
            "INSERT INTO key_grants (key_id, action, resource_type, resource_id, resource_path, limits, confirm)
             VALUES ($1, $2, $3, $4, $5, $6, $7) ON CONFLICT DO NOTHING",
            &[&key_id, &grant.action, &kind, &id, &path, &limits, &confirm],
        )?;
    }
    Ok(())
}

fn digest_of(bytes: Vec<u8>) -> Result<[u8; 32], String> {
    bytes
        .try_into()
        .map_err(|_| "keys: a digest does not have 32 bytes".to_string())
}

/// The counters of one use as the parallel arrays that the statement reads: the login, the id of
/// the key, the subject of the file, the service, the budget, the window, the amount and the
/// length of the window.
#[derive(Default)]
struct Columns {
    login: Vec<Option<String>>,
    key: Vec<Option<String>>,
    file: Vec<Option<String>>,
    service: Vec<String>,
    budget: Vec<String>,
    window: Vec<String>,
    amount: Vec<i64>,
    length: Vec<i64>,
}

impl Columns {
    fn push(&mut self, owner: &CounterOwner, id: &CounterId, amount: u64) {
        let (login, key, file) = match owner {
            CounterOwner::User(login) => (Some(login.clone()), None, None),
            CounterOwner::Key(key) => (None, Some(key.clone()), None),
            CounterOwner::File(file) => (None, None, Some(file.clone())),
        };
        self.login.push(login);
        self.key.push(key);
        self.file.push(file);
        self.service.push(id.service.clone());
        self.budget.push(id.budget.clone());
        self.window.push(id.window.name().to_string());
        self.amount.push(i64::try_from(amount).unwrap_or(i64::MAX));
        self.length.push(id.window.seconds() as i64);
    }
}

/// One statement counts one use on all its counters (portal 7.2b). It locks the rows in the order
/// of their id, computes the window start, the reset and the room from the locked values and from
/// the clock of the database, and it changes the rows only when each counter has room.
const COUNT_USE: &str = "
WITH want AS (
  SELECT * FROM unnest($1::text[], $2::text[], $3::text[], $4::text[], $5::text[], $6::text[],
                       $7::bigint[], $8::bigint[])
         WITH ORDINALITY AS w(login, key_id, file_subject, service, budget, win, amount, len, ord)
), clock AS (
  SELECT COALESCE($9::float8, extract(epoch FROM clock_timestamp())::float8) AS t
), locked AS (
  SELECT c.id, c.window_start, c.units, w.amount, w.len, w.ord
    FROM budget_counters c
    JOIN want w ON c.service = w.service AND c.budget = w.budget AND c.\"window\" = w.win
     AND ((w.login IS NOT NULL AND c.user_id = (SELECT u.id FROM users u WHERE u.login = w.login))
       OR (w.key_id IS NOT NULL AND c.key_id = w.key_id)
       OR (w.file_subject IS NOT NULL AND c.file_subject = w.file_subject))
   ORDER BY c.id
     FOR UPDATE OF c
), state AS (
  SELECT l.id, l.amount, l.len, l.ord, clock.t,
         (floor(clock.t / l.len)::bigint) * l.len AS start,
         CASE WHEN l.window_start < (floor(clock.t / l.len)::bigint) * l.len THEN 0
              ELSE l.units END AS cur
    FROM locked l CROSS JOIN clock
), verdict AS (
  SELECT count(*) AS n,
         bool_and(cur + 1 <= amount) AND count(*) = (SELECT count(*) FROM want) AS ok
    FROM state
), changed AS (
  UPDATE budget_counters c SET window_start = s.start, units = s.cur + 1
    FROM state s
   WHERE c.id = s.id AND (SELECT ok FROM verdict)
  RETURNING c.id
)
SELECT v.n, v.ok,
       (SELECT array_agg(ord::bigint) FROM state WHERE cur + 1 > amount),
       (SELECT array_agg((start + len - t)::float8) FROM state WHERE cur + 1 > amount)
  FROM verdict v";

impl Pg {
    pub fn count_use(
        &self,
        counters: &[Counter],
        clock: Option<f64>,
    ) -> Result<CountOutcome, String> {
        let mut columns = Columns::default();
        for counter in counters {
            columns.push(&counter.owner, &counter.id, counter.amount);
        }
        let mut conn = self.counters.get()?;
        let row = conn.query_one(
            COUNT_USE,
            &[
                &columns.login,
                &columns.key,
                &columns.file,
                &columns.service,
                &columns.budget,
                &columns.window,
                &columns.amount,
                &columns.length,
                &clock,
            ],
        );
        let row = match row {
            Ok(row) => row,
            Err(error) => {
                // A statement that timed out or a connection that broke: the next use opens a
                // new connection when this one is closed.
                if conn.is_closed() || error.code() != Some(&SqlState::QUERY_CANCELED) {
                    conn.discard();
                }
                return Err(db(error));
            }
        };
        let found: i64 = row.get(0);
        if found != counters.len() as i64 {
            return Ok(CountOutcome::RowMissing);
        }
        let ok: Option<bool> = row.get(1);
        if ok == Some(true) {
            return Ok(CountOutcome::Counted);
        }
        let ords: Vec<i64> = row.get::<_, Option<Vec<i64>>>(2).unwrap_or_default();
        let left: Vec<f64> = row.get::<_, Option<Vec<f64>>>(3).unwrap_or_default();
        Ok(CountOutcome::Refused(
            ords.into_iter()
                .zip(left)
                .map(|(ord, seconds_left)| Refusal {
                    index: (ord - 1) as usize,
                    seconds_left,
                })
                .collect(),
        ))
    }

    pub fn ensure_counter_rows(&self, rows: &[(CounterOwner, CounterId)]) -> Result<usize, String> {
        let mut made = 0;
        for batch in rows.chunks(ROWS_PER_STATEMENT) {
            let mut columns = Columns::default();
            for (owner, id) in batch {
                columns.push(owner, id, 0);
            }
            // The statement is a transaction of its own, with the synchronous commit: a crash or a
            // failover cannot lose a counter row (portal 7.7b).
            made += self.run(|c| {
                let mut tx = c.transaction()?;
                tx.batch_execute("SET LOCAL synchronous_commit = on; SET LOCAL statement_timeout = 5000")?;
                let made = tx.execute(
                    "INSERT INTO budget_counters
                       (user_id, key_id, file_subject, service, budget, \"window\", window_start, units)
                     SELECT u.id, k.id, w.file_subject, w.service, w.budget, w.win, 0, 0
                       FROM unnest($1::text[], $2::text[], $3::text[], $4::text[], $5::text[],
                                   $6::text[]) AS w(login, key_id, file_subject, service, budget, win)
                       LEFT JOIN users u ON u.login = w.login
                       LEFT JOIN keys k ON k.id = w.key_id
                      WHERE (w.login IS NULL OR u.id IS NOT NULL)
                        AND (w.key_id IS NULL OR k.id IS NOT NULL)
                     ON CONFLICT DO NOTHING",
                    &[
                        &columns.login,
                        &columns.key,
                        &columns.file,
                        &columns.service,
                        &columns.budget,
                        &columns.window,
                    ],
                )?;
                tx.commit()?;
                Ok(made as usize)
            })?;
        }
        Ok(made)
    }

    pub fn delete_file_counters(&self, keep: &[String]) -> Result<usize, String> {
        self.run(|c| {
            Ok(c.execute(
                "DELETE FROM budget_counters
                  WHERE file_subject IS NOT NULL AND NOT (file_subject = ANY($1::text[]))",
                &[&keep],
            )? as usize)
        })
    }

    pub fn counter_rows(&self) -> Result<i64, String> {
        self.run(|c| {
            Ok(c.query_one("SELECT COUNT(*) FROM budget_counters", &[])?
                .get(0))
        })
    }

    pub fn has_key_history(&self, id: &str, owner: Option<Option<&str>>) -> Result<bool, String> {
        let (any, login) = match owner {
            None => (true, None),
            Some(login) => (false, login),
        };
        self.run(|c| {
            Ok(c.query_opt(
                "SELECT 1 FROM key_history h LEFT JOIN users u ON u.id = h.owner_id
                  WHERE h.id = $1 AND ($2::boolean OR u.login IS NOT DISTINCT FROM $3::text)",
                &[&id, &any, &login],
            )?
            .is_some())
        })
    }

    pub fn list_keys(
        &self,
        scope: KeyScope<'_>,
        after: Option<(i64, &str)>,
        limit: usize,
    ) -> Result<Vec<KeyListItem>, String> {
        let (owner_login, only_superadmin, any) = match scope {
            KeyScope::All => (None, false, true),
            KeyScope::Owner(login) => (Some(login), false, false),
            KeyScope::Superadmin => (None, true, false),
        };
        let (after_created, after_id) = match after {
            Some((created, id)) => (Some(created), Some(id)),
            None => (None, None),
        };
        let (mut items, grants) = self.run(|c| {
            let rows = c.query(
                "SELECT id, login, name, hint, created_at, expires_at, last_check_at, check_count, revoked_at, kind, interaction, agents
                   FROM (
                     SELECT k.id AS id, u.login AS login, k.name AS name, k.hint AS hint,
                            k.created_at AS created_at, k.expires_at AS expires_at,
                            k.last_check_at AS last_check_at, k.check_count AS check_count,
                            NULL::bigint AS revoked_at, k.owner_id AS owner_id,
                            k.kind AS kind, k.interaction AS interaction, k.agents AS agents
                       FROM keys k LEFT JOIN users u ON u.id = k.owner_id
                     UNION ALL
                     SELECT h.id, u.login, h.name, ''::text, h.created_at, NULL::bigint,
                            h.last_check_at, h.check_count, h.revoked_at, h.owner_id, NULL::text, NULL::text, NULL::text
                       FROM key_history h LEFT JOIN users u ON u.id = h.owner_id
                   ) AS lines
                  WHERE ($1::boolean OR ($2::text IS NOT NULL AND login = $2::text)
                         OR ($3::boolean AND owner_id IS NULL))
                    AND ($4::bigint IS NULL OR created_at < $4 OR (created_at = $4 AND id < $5::text))
                  ORDER BY created_at DESC, id DESC
                  LIMIT $6",
                &[&any, &owner_login, &only_superadmin, &after_created, &after_id, &(limit as i64)],
            )?;
            let items: Vec<KeyListItem> = rows
                .into_iter()
                .map(|row| KeyListItem {
                    id: row.get(0),
                    owner: row.get(1),
                    name: row.get(2),
                    hint: row.get(3),
                    created_at: row.get(4),
                    expires_at: row.get(5),
                    last_check_at: row.get(6),
                    check_count: row.get(7),
                    revoked_at: row.get(8),
                    kind: row
                        .get::<_, Option<String>>(9)
                        .and_then(|name| crate::keys::KeyKind::from_name(&name)),
                    interaction: row
                        .get::<_, Option<String>>(10)
                        .and_then(|name| crate::keys::Interaction::from_name(&name)),
                    agents: row
                        .get::<_, Option<String>>(11)
                        .and_then(|name| crate::keys::AgentsMode::from_name(&name)),
                    grants: Vec::new(),
                })
                .collect();
            let ids: Vec<String> = items
                .iter()
                .filter(|i| i.revoked_at.is_none())
                .map(|i| i.id.clone())
                .collect();
            let grants = c
                .query(
                    "SELECT key_id, action, resource_type, resource_id, resource_path, limits, confirm
                       FROM key_grants WHERE key_id = ANY($1::text[])",
                    &[&ids],
                )?
                .into_iter()
                .map(|row| {
                    (
                        row.get::<_, String>(0),
                        (
                            [
                                row.get::<_, String>(1),
                                row.get::<_, String>(2),
                                row.get::<_, String>(3),
                                row.get::<_, String>(4),
                                row.get::<_, String>(5),
                            ],
                            row.get::<_, bool>(6),
                        ),
                    )
                })
                .collect::<Vec<_>>();
            Ok((items, grants))
        })?;
        for item in items.iter_mut().filter(|i| i.revoked_at.is_none()) {
            let rows: Vec<GrantRow> = grants
                .iter()
                .filter(|(key_id, _)| *key_id == item.id)
                .map(|(_, row)| row.clone())
                .collect();
            item.grants =
                grants_from(rows).map_err(|e| e.replacen("user_grants", "key_grants", 1))?;
            item.grants.sort();
        }
        Ok(items)
    }

    pub fn directory_version(&self) -> Result<i64, String> {
        self.run(|c| {
            Ok(
                c.query_one("SELECT version FROM directory_version WHERE id = 1", &[])?
                    .get(0),
            )
        })
    }

    /// The version, the users and the keys in one transaction with the isolation REPEATABLE READ
    /// (portal 10.7).
    pub fn load_directory(&self) -> Result<DirectorySnapshot, String> {
        let (version, user_rows, key_rows, grant_rows, externals) = self.run(|c| {
            let mut tx = c
                .build_transaction()
                .isolation_level(IsolationLevel::RepeatableRead)
                .read_only(true)
                .start()?;
            let version: i64 = tx
                .query_one("SELECT version FROM directory_version WHERE id = 1", &[])?
                .get(0);
            let users = Self::users_query(&mut tx)?;
            let keys = tx
                .query(
                    "SELECT k.id, u.login, k.name, k.digest, k.hint, k.created_at, k.expires_at,
                            k.last_check_at, k.check_count, k.kind, k.interaction, k.agents
                       FROM keys k LEFT JOIN users u ON u.id = k.owner_id
                      ORDER BY k.created_at, k.id",
                    &[],
                )?
                .into_iter()
                .map(|row| {
                    (
                        row.get::<_, String>(0),
                        row.get::<_, Option<String>>(1),
                        row.get::<_, String>(2),
                        row.get::<_, Vec<u8>>(3),
                        row.get::<_, String>(4),
                        row.get::<_, i64>(5),
                        row.get::<_, Option<i64>>(6),
                        row.get::<_, Option<i64>>(7),
                        row.get::<_, i64>(8),
                        row.get::<_, String>(9),
                        row.get::<_, String>(10),
                        row.get::<_, String>(11),
                    )
                })
                .collect::<Vec<_>>();
            let grants = tx
                .query(
                    "SELECT key_id, action, resource_type, resource_id, resource_path, limits, confirm
                       FROM key_grants",
                    &[],
                )?
                .into_iter()
                .map(|row| {
                    (
                        row.get::<_, String>(0),
                        (
                            [
                                row.get::<_, String>(1),
                                row.get::<_, String>(2),
                                row.get::<_, String>(3),
                                row.get::<_, String>(4),
                                row.get::<_, String>(5),
                            ],
                            row.get::<_, bool>(6),
                        ),
                    )
                })
                .collect::<Vec<_>>();
            let externals = Self::externals_query(&mut tx)?;
            tx.commit()?;
            Ok((version, users, keys, grants, externals))
        })?;
        let externals = externals
            .into_iter()
            .map(crate::store::ExternalRow::into_entry)
            .collect::<Result<Vec<_>, String>>()?;
        let users = Self::assemble_users(user_rows)?;
        let mut by_key: std::collections::HashMap<String, Vec<GrantRow>> =
            std::collections::HashMap::new();
        for (key_id, row) in grant_rows {
            by_key.entry(key_id).or_default().push(row);
        }
        let mut keys = Vec::with_capacity(key_rows.len());
        for (
            id,
            owner,
            name,
            digest,
            hint,
            created_at,
            expires_at,
            last_check_at,
            check_count,
            kind,
            interaction,
            agents,
        ) in key_rows
        {
            let kind = crate::keys::KeyKind::from_name(&kind)
                .ok_or_else(|| format!("keys: the kind {kind:?} of the key {id} is not known"))?;
            let interaction =
                crate::keys::Interaction::from_name(&interaction).ok_or_else(|| {
                    format!("keys: the interaction {interaction:?} of the key {id} is not known")
                })?;
            let agents = crate::keys::AgentsMode::from_name(&agents).ok_or_else(|| {
                format!("keys: the mode of the agents {agents:?} of the key {id} is not known")
            })?;
            let mut grants = grants_from(by_key.remove(&id).unwrap_or_default())
                .map_err(|e| e.replacen("user_grants", "key_grants", 1))?;
            grants.sort();
            keys.push(KeyRow {
                id,
                owner,
                name,
                digest: digest_of(digest)?,
                hint,
                created_at,
                expires_at,
                last_check_at,
                check_count,
                kind,
                interaction,
                agents,
                grants,
            });
        }
        Ok(DirectorySnapshot {
            version,
            users,
            keys,
            externals,
        })
    }

    pub fn insert_key(&self, key: &KeyRow, caps: &KeyCaps) -> Result<(), KeyWriteError> {
        self.run(|c| {
            let mut tx = c.transaction()?;
            bump(&mut tx)?;
            let owner_id: Option<i64> = match &key.owner {
                None => None,
                Some(login) => {
                    match tx.query_opt("SELECT id FROM users WHERE login = $1", &[login])? {
                        Some(row) => Some(row.get(0)),
                        None => return Ok(Err(KeyWriteError::UnknownOwner)),
                    }
                }
            };
            let active: i64 = tx
                .query_one(
                    "SELECT COUNT(*) FROM keys
                      WHERE owner_id IS NOT DISTINCT FROM $1::bigint
                        AND (expires_at IS NULL OR expires_at > $2)
                        AND ($3::bigint IS NULL OR created_at >= $3)",
                    &[&owner_id, &caps.now, &caps.created_not_before],
                )?
                .get(0);
            if active >= caps.max_per_user as i64 {
                return Ok(Err(KeyWriteError::TooManyKeys));
            }
            tx.execute(
                "INSERT INTO keys (id, owner_id, name, digest, hint, created_at, expires_at,
                                   last_check_at, check_count, kind, interaction, agents)
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12)",
                &[
                    &key.id,
                    &owner_id,
                    &key.name,
                    &key.digest.as_slice(),
                    &key.hint,
                    &key.created_at,
                    &key.expires_at,
                    &key.last_check_at,
                    &key.check_count,
                    &key.kind.name(),
                    &key.interaction.name(),
                    &key.agents.name(),
                ],
            )?;
            insert_key_grants(&mut tx, &key.id, &key.grants)?;
            insert_counter_rows(&mut tx, Subject::Key(&key.id), &key.grants)?;
            if !within_capacity(&mut tx, caps)? {
                return Ok(Err(KeyWriteError::Capacity));
            }
            tx.commit()?;
            Ok(Ok(()))
        })
        .map_err(KeyWriteError::Db)?
    }

    pub fn update_key(
        &self,
        id: &str,
        owner: Option<&str>,
        name: &str,
        modes: crate::store::KeyModes,
        grants: &[Grant],
        caps: &KeyCaps,
    ) -> Result<(), KeyWriteError> {
        self.run(|c| {
            let mut tx = c.transaction()?;
            bump(&mut tx)?;
            let found = tx.query_opt(
                "SELECT 1 FROM keys k LEFT JOIN users u ON u.id = k.owner_id
                  WHERE k.id = $1 AND u.login IS NOT DISTINCT FROM $2::text FOR UPDATE OF k",
                &[&id, &owner],
            )?;
            if found.is_none() {
                return Ok(Err(KeyWriteError::UnknownKey));
            }
            tx.execute(
                "UPDATE keys SET name = $2, interaction = $3, agents = $4 WHERE id = $1",
                &[&id, &name, &modes.interaction.name(), &modes.agents.name()],
            )?;
            tx.execute("DELETE FROM key_grants WHERE key_id = $1", &[&id])?;
            insert_key_grants(&mut tx, id, grants)?;
            insert_counter_rows(&mut tx, Subject::Key(id), grants)?;
            if !within_capacity(&mut tx, caps)? {
                return Ok(Err(KeyWriteError::Capacity));
            }
            tx.commit()?;
            Ok(Ok(()))
        })
        .map_err(KeyWriteError::Db)?
    }

    pub fn revoke_key(
        &self,
        id: &str,
        owner: Option<Option<&str>>,
        now: i64,
    ) -> Result<bool, String> {
        self.run(|c| {
            let mut tx = c.transaction()?;
            bump(&mut tx)?;
            let revoked = revoke(&mut tx, id, owner, now)?;
            if revoked {
                tx.commit()?;
            }
            // A revocation that finds no key changes nothing: the transaction ends with no commit.
            Ok(revoked)
        })
    }

    pub fn revoke_keys(&self, login: Option<&str>, now: i64) -> Result<usize, String> {
        self.run(|c| {
            let mut tx = c.transaction()?;
            bump(&mut tx)?;
            let ids: Vec<String> = match login {
                Some(login) => tx
                    .query(
                        "SELECT k.id FROM keys k JOIN users u ON u.id = k.owner_id
                          WHERE u.login = $1 ORDER BY k.id FOR UPDATE OF k",
                        &[&login],
                    )?
                    .into_iter()
                    .map(|r| r.get(0))
                    .collect(),
                None => tx
                    .query("SELECT id FROM keys ORDER BY id FOR UPDATE", &[])?
                    .into_iter()
                    .map(|r| r.get(0))
                    .collect(),
            };
            for id in &ids {
                revoke(&mut tx, id, None, now)?;
            }
            if !ids.is_empty() {
                tx.commit()?;
            }
            Ok(ids.len())
        })
    }

    pub fn write_key_checks(&self, checks: &[KeyCheck]) -> Result<(), String> {
        if checks.is_empty() {
            return Ok(());
        }
        self.run(|c| {
            let mut tx = c.transaction()?;
            // The same order on each instance, so two writes never deadlock.
            let mut sorted: Vec<&KeyCheck> = checks.iter().collect();
            sorted.sort_by(|a, b| a.id.cmp(&b.id));
            for check in sorted {
                tx.execute(
                    "UPDATE keys
                        SET last_check_at = GREATEST(COALESCE(last_check_at, 0), $2),
                            check_count = check_count + $3
                      WHERE id = $1",
                    &[&check.id, &check.last_check_at, &check.count],
                )?;
            }
            tx.commit()
        })
    }

    pub fn prune_keys(&self, expired_before: i64, history_before: i64) -> Result<(), String> {
        self.run(|c| {
            let mut tx = c.transaction()?;
            let deleted = tx.execute(
                "DELETE FROM keys WHERE expires_at IS NOT NULL AND expires_at < $1",
                &[&expired_before],
            )?;
            tx.execute(
                "DELETE FROM key_history WHERE revoked_at < $1",
                &[&history_before],
            )?;
            if deleted > 0 {
                bump(&mut tx)?;
            }
            tx.commit()
        })
    }

    #[cfg(test)]
    pub fn key_history(&self) -> Result<Vec<KeyHistoryRow>, String> {
        self.run(|c| {
            Ok(c.query(
                "SELECT h.id, u.login, h.name, h.created_at, h.revoked_at, h.last_check_at, h.check_count
                   FROM key_history h LEFT JOIN users u ON u.id = h.owner_id ORDER BY h.id",
                &[],
            )?
            .into_iter()
            .map(|row| KeyHistoryRow {
                id: row.get(0),
                owner: row.get(1),
                name: row.get(2),
                created_at: row.get(3),
                revoked_at: row.get(4),
                last_check_at: row.get(5),
                check_count: row.get(6),
            })
            .collect())
        })
    }
}

/// Deletes a key and writes its summary row. `owner` limits the key to one owner, as in
/// [`Pg::revoke_key`].
fn revoke(
    tx: &mut Transaction<'_>,
    id: &str,
    owner: Option<Option<&str>>,
    now: i64,
) -> Result<bool, postgres::Error> {
    let row = match owner {
        None => tx.query_opt(
            "DELETE FROM keys WHERE id = $1
             RETURNING owner_id, name, created_at, last_check_at, check_count",
            &[&id],
        )?,
        Some(login) => tx.query_opt(
            "DELETE FROM keys k USING (
                 SELECT k2.id FROM keys k2 LEFT JOIN users u ON u.id = k2.owner_id
                  WHERE k2.id = $1 AND u.login IS NOT DISTINCT FROM $2::text
               ) AS wanted
              WHERE k.id = wanted.id
             RETURNING k.owner_id, k.name, k.created_at, k.last_check_at, k.check_count",
            &[&id, &login],
        )?,
    };
    let Some(row) = row else {
        return Ok(false);
    };
    let owner_id: Option<i64> = row.get(0);
    let name: String = row.get(1);
    let created_at: i64 = row.get(2);
    let last_check_at: Option<i64> = row.get(3);
    let check_count: i64 = row.get(4);
    tx.execute(
        "INSERT INTO key_history (id, owner_id, name, created_at, revoked_at, last_check_at, check_count)
         VALUES ($1, $2, $3, $4, $5, $6, $7)
         ON CONFLICT (id) DO UPDATE SET revoked_at = EXCLUDED.revoked_at",
        &[&id, &owner_id, &name, &created_at, &now, &last_check_at, &check_count],
    )?;
    Ok(true)
}

// The helper traits come from `postgres`: this keeps the import used when `GenericClient` is the
// bound of `users_query`.
#[allow(unused)]
fn _generic<C: GenericClient>(_: &C) {}
