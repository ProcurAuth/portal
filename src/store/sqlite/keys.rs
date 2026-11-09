//! The keys, the directory version and the counter rows in SQLite (portal 10.4a).
//!
//! One instance is the only writer of the file, so a write needs no row lock: the connection is
//! behind a mutex, and each write is one transaction. Each write to the users, the grants or the
//! keys adds 1 to `directory_version` in its transaction (portal 10.7). A write that adds a limit
//! value makes the counter rows of the value in the same transaction (portal 7.2a, 10.4a).

use rusqlite::{params, Transaction};

use super::*;
use crate::decision::{Counter, CounterOwner};
use crate::grants::{counter_ids, CounterId};

/// Who owns the counters of a write.
pub(super) enum Subject<'a> {
    User(i64),
    Key(&'a str),
}

/// Adds 1 to the version of the directory.
pub(super) fn bump(tx: &Transaction<'_>) -> rusqlite::Result<()> {
    tx.execute(
        "UPDATE directory_version SET version = version + 1 WHERE id = 1",
        [],
    )
    .map(drop)
}

/// Makes the counter rows that the grants need and that the table does not hold yet.
pub(super) fn insert_counter_rows(
    tx: &Transaction<'_>,
    subject: Subject<'_>,
    grants: &[Grant],
) -> rusqlite::Result<()> {
    for CounterId {
        service,
        budget,
        window,
    } in counter_ids(grants)
    {
        match subject {
            Subject::User(id) => tx.execute(
                "INSERT OR IGNORE INTO budget_counters
                   (user_id, service, budget, \"window\", window_start, units)
                 VALUES (?1, ?2, ?3, ?4, 0, 0)",
                params![id, service, budget, window.name()],
            ),
            Subject::Key(id) => tx.execute(
                "INSERT OR IGNORE INTO budget_counters
                   (key_id, service, budget, \"window\", window_start, units)
                 VALUES (?1, ?2, ?3, ?4, 0, 0)",
                params![id, service, budget, window.name()],
            ),
        }?;
    }
    Ok(())
}

fn total(tx: &Transaction<'_>, sql: &str) -> rusqlite::Result<i64> {
    tx.query_row(sql, [], |row| row.get(0))
}

/// The bounds of portal 7.2f and 10.7d, checked after the write inserted its rows. The caller
/// drops the transaction when this answers `false`.
fn within_capacity(tx: &Transaction<'_>, caps: &KeyCaps) -> rusqlite::Result<bool> {
    let counters = total(tx, "SELECT COUNT(*) FROM budget_counters")?;
    let grants = total(
        tx,
        "SELECT (SELECT COUNT(*) FROM user_grants) + (SELECT COUNT(*) FROM key_grants)",
    )?;
    Ok(counters <= caps.max_counter_rows as i64 && grants <= caps.max_directory_grants as i64)
}

fn insert_key_grants(tx: &Transaction<'_>, key_id: &str, grants: &[Grant]) -> rusqlite::Result<()> {
    for grant in grants {
        let (kind, id, path, limits, confirm) = grant_columns(grant);
        tx.execute(
            "INSERT OR IGNORE INTO key_grants
               (key_id, action, resource_type, resource_id, resource_path, limits, confirm)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![key_id, grant.action, kind, id, path, limits, confirm],
        )?;
    }
    Ok(())
}

/// The grants of a key. `with_confirm`: the table has the column `confirm` (a file of version 8
/// that the approvals did not make has none: the import reads it as `false`).
fn key_grants_of(
    conn: &Connection,
    key_id: &str,
    with_confirm: bool,
) -> Result<Vec<Grant>, String> {
    let confirm = if with_confirm { "confirm" } else { "0" };
    let rows = conn
        .prepare_cached(&format!(
            "SELECT action, resource_type, resource_id, resource_path, limits, {confirm}
               FROM key_grants WHERE key_id = ?1"
        ))
        .map_err(db)?
        .query_map([key_id], |row| {
            Ok((
                [
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                ],
                row.get::<_, bool>(5)?,
            ))
        })
        .map_err(db)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(db)?;
    let mut grants = rows
        .iter()
        .map(|([action, kind, id, path, limits], confirm)| {
            grant_from_row(action, kind, id, path, limits, *confirm)
                .map_err(|e| e.replacen("user_grants", "key_grants", 1))
        })
        .collect::<Result<Vec<_>, _>>()?;
    grants.sort();
    Ok(grants)
}

fn digest_of(bytes: Vec<u8>) -> Result<[u8; 32], String> {
    bytes
        .try_into()
        .map_err(|_| "keys: a digest does not have 32 bytes".to_string())
}

/// The row of one counter: its id, the start of its window and its units.
fn counter_row(
    tx: &Transaction<'_>,
    owner: &CounterOwner,
    id: &CounterId,
) -> rusqlite::Result<Option<(i64, i64, i64)>> {
    let (subject, column) = match owner {
        CounterOwner::User(login) => (
            login.as_str(),
            "user_id = (SELECT id FROM users WHERE login = ?1)",
        ),
        CounterOwner::Key(key) => (key.as_str(), "key_id = ?1"),
        CounterOwner::File(file) => (file.as_str(), "file_subject = ?1"),
    };
    tx.query_row(
        &format!(
            "SELECT id, window_start, units FROM budget_counters
              WHERE {column} AND service = ?2 AND budget = ?3 AND \"window\" = ?4"
        ),
        params![subject, id.service, id.budget, id.window.name()],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    )
    .optional()
}

fn clock_seconds() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0.0, |d| d.as_secs_f64())
}

/// The keys with their grants, oldest first.
pub(super) fn read_keys(conn: &Connection, with_kind: bool) -> Result<Vec<KeyRow>, String> {
    // A file of version 6 has no column `kind`: its keys are MCP keys. A file that the approvals
    // did not make has no column `interaction` and no column `confirm`: the keys get the default
    // of their kind, and no grant confirms.
    let kind = if with_kind { "k.kind" } else { "'mcp'" };
    let interaction = if has_column(conn, "keys", "interaction")? {
        "k.interaction"
    } else {
        "''"
    };
    // A file that the agents did not make has no column `agents`: the keys get the default of their kind.
    let agents = if has_column(conn, "keys", "agents")? {
        "k.agents"
    } else {
        "''"
    };
    let with_confirm = has_column(conn, "key_grants", "confirm")?;
    let rows = conn
        .prepare(&format!(
            "SELECT k.id, u.login, k.name, k.digest, k.hint, k.created_at, k.expires_at,
                        k.last_check_at, k.check_count, {kind}, {interaction}, {agents}
                   FROM keys k LEFT JOIN users u ON u.id = k.owner_id
                  ORDER BY k.created_at, k.id",
        ))
        .map_err(db)?
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, Option<String>>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, Vec<u8>>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, i64>(5)?,
                row.get::<_, Option<i64>>(6)?,
                row.get::<_, Option<i64>>(7)?,
                row.get::<_, i64>(8)?,
                row.get::<_, String>(9)?,
                row.get::<_, String>(10)?,
                row.get::<_, String>(11)?,
            ))
        })
        .map_err(db)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(db)?;
    let mut keys = Vec::with_capacity(rows.len());
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
    ) in rows
    {
        let kind = crate::keys::KeyKind::from_name(&kind)
            .ok_or_else(|| format!("keys: the kind {kind:?} of the key {id} is not known"))?;
        keys.push(KeyRow {
            interaction: if interaction.is_empty() {
                crate::keys::Interaction::default_for(kind)
            } else {
                crate::keys::Interaction::from_name(&interaction).ok_or_else(|| {
                    format!("keys: the interaction {interaction:?} of the key {id} is not known")
                })?
            },
            agents: if agents.is_empty() {
                crate::keys::AgentsMode::default_for(kind)
            } else {
                crate::keys::AgentsMode::from_name(&agents).ok_or_else(|| {
                    format!("keys: the mode of the agents {agents:?} of the key {id} is not known")
                })?
            },
            kind,
            grants: key_grants_of(conn, &id, with_confirm)?,
            id,
            owner,
            name,
            digest: digest_of(digest)?,
            hint,
            created_at,
            expires_at,
            last_check_at,
            check_count,
        });
    }
    Ok(keys)
}

/// The summary rows of the revoked keys.
pub(super) fn read_history(conn: &Connection) -> Result<Vec<KeyHistoryRow>, String> {
    let mut statement = conn
        .prepare(
            "SELECT h.id, u.login, h.name, h.created_at, h.revoked_at, h.last_check_at, h.check_count
               FROM key_history h LEFT JOIN users u ON u.id = h.owner_id ORDER BY h.id",
        )
        .map_err(db)?;
    let rows = statement
        .query_map([], |row| {
            Ok(KeyHistoryRow {
                id: row.get(0)?,
                owner: row.get(1)?,
                name: row.get(2)?,
                created_at: row.get(3)?,
                revoked_at: row.get(4)?,
                last_check_at: row.get(5)?,
                check_count: row.get(6)?,
            })
        })
        .map_err(db)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(db);
    rows
}

impl Sqlite {
    pub fn count_use(
        &self,
        counters: &[Counter],
        clock: Option<f64>,
    ) -> Result<CountOutcome, String> {
        let mut conn = self.conn();
        // The write lock of the file replaces the row locks (portal 7.2b, 10.4a).
        let tx = conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .map_err(db)?;
        let now = clock.unwrap_or_else(clock_seconds);
        let mut rows = Vec::with_capacity(counters.len());
        for counter in counters {
            let Some(row) = counter_row(&tx, &counter.owner, &counter.id).map_err(db)? else {
                return Ok(CountOutcome::RowMissing);
            };
            rows.push(row);
        }
        let mut refused = Vec::new();
        let mut updates = Vec::with_capacity(counters.len());
        for (index, (counter, (row_id, start, units))) in counters.iter().zip(&rows).enumerate() {
            let length = counter.id.window.seconds() as f64;
            let window_start = (now / length).floor() * length;
            let current = if (*start as f64) < window_start {
                0
            } else {
                *units
            };
            if current + 1 > counter.amount as i64 {
                refused.push(Refusal {
                    index,
                    seconds_left: window_start + length - now,
                });
            }
            updates.push((*row_id, window_start as i64, current + 1));
        }
        if !refused.is_empty() {
            return Ok(CountOutcome::Refused(refused));
        }
        for (row_id, start, units) in updates {
            tx.execute(
                "UPDATE budget_counters SET window_start = ?2, units = ?3 WHERE id = ?1",
                params![row_id, start, units],
            )
            .map_err(db)?;
        }
        tx.commit().map_err(db)?;
        Ok(CountOutcome::Counted)
    }

    pub fn ensure_counter_rows(&self, rows: &[(CounterOwner, CounterId)]) -> Result<usize, String> {
        let mut conn = self.conn();
        let tx = super::begin_write(&mut conn).map_err(db)?;
        let mut made = 0;
        for (owner, id) in rows {
            let (column, subject) = match owner {
                CounterOwner::User(login) => (
                    "user_id",
                    tx.query_row("SELECT id FROM users WHERE login = ?1", [login], |r| {
                        r.get::<_, i64>(0).map(|id| id.to_string())
                    })
                    .optional()
                    .map_err(db)?,
                ),
                CounterOwner::Key(key) => (
                    "key_id",
                    tx.query_row("SELECT id FROM keys WHERE id = ?1", [key], |r| r.get(0))
                        .optional()
                        .map_err(db)?,
                ),
                CounterOwner::File(file) => ("file_subject", Some(file.clone())),
            };
            let Some(subject) = subject else {
                continue;
            };
            // `user_id` is a number: SQLite converts the text of its id.
            let subject: rusqlite::types::Value = if column == "user_id" {
                rusqlite::types::Value::Integer(subject.parse().unwrap_or(0))
            } else {
                rusqlite::types::Value::Text(subject)
            };
            made += tx
                .execute(
                    &format!(
                        "INSERT OR IGNORE INTO budget_counters
                           ({column}, service, budget, \"window\", window_start, units)
                         VALUES (?1, ?2, ?3, ?4, 0, 0)"
                    ),
                    params![subject, id.service, id.budget, id.window.name()],
                )
                .map_err(db)?;
        }
        tx.commit().map_err(db)?;
        Ok(made)
    }

    pub fn delete_file_counters(&self, keep: &[String]) -> Result<usize, String> {
        let mut conn = self.conn();
        let tx = super::begin_write(&mut conn).map_err(db)?;
        let mut stale: Vec<String> = tx
            .prepare(
                "SELECT DISTINCT file_subject FROM budget_counters WHERE file_subject IS NOT NULL",
            )
            .map_err(db)?
            .query_map([], |r| r.get(0))
            .map_err(db)?
            .collect::<Result<_, _>>()
            .map_err(db)?;
        stale.retain(|subject| !keep.contains(subject));
        let mut deleted = 0;
        for subject in &stale {
            deleted += tx
                .execute(
                    "DELETE FROM budget_counters WHERE file_subject = ?1",
                    [subject],
                )
                .map_err(db)?;
        }
        tx.commit().map_err(db)?;
        Ok(deleted)
    }

    pub fn counter_rows(&self) -> Result<i64, String> {
        self.conn()
            .query_row("SELECT COUNT(*) FROM budget_counters", [], |r| r.get(0))
            .map_err(db)
    }

    pub fn has_key_history(&self, id: &str, owner: Option<Option<&str>>) -> Result<bool, String> {
        let (any, login) = match owner {
            None => (true, None),
            Some(login) => (false, login),
        };
        self.conn()
            .query_row(
                "SELECT 1 FROM key_history h LEFT JOIN users u ON u.id = h.owner_id
                  WHERE h.id = ?1 AND (?2 OR u.login IS ?3)",
                params![id, any, login],
                |_| Ok(()),
            )
            .optional()
            .map(|found| found.is_some())
            .map_err(db)
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
        let conn = self.conn();
        let mut statement = conn
            .prepare(
                "SELECT id, login, name, hint, created_at, expires_at, last_check_at, check_count, revoked_at, kind, interaction, agents
                   FROM (
                     SELECT k.id AS id, u.login AS login, k.name AS name, k.hint AS hint,
                            k.created_at AS created_at, k.expires_at AS expires_at,
                            k.last_check_at AS last_check_at, k.check_count AS check_count,
                            NULL AS revoked_at, k.owner_id AS owner_id, k.kind AS kind,
                            k.interaction AS interaction, k.agents AS agents
                       FROM keys k LEFT JOIN users u ON u.id = k.owner_id
                     UNION ALL
                     SELECT h.id, u.login, h.name, '', h.created_at, NULL, h.last_check_at,
                            h.check_count, h.revoked_at, h.owner_id, NULL, NULL, NULL
                       FROM key_history h LEFT JOIN users u ON u.id = h.owner_id
                   )
                  WHERE (?1 OR (?2 IS NOT NULL AND login = ?2) OR (?3 AND owner_id IS NULL))
                    AND (?4 IS NULL OR created_at < ?4 OR (created_at = ?4 AND id < ?5))
                  ORDER BY created_at DESC, id DESC
                  LIMIT ?6",
            )
            .map_err(db)?;
        let mut items: Vec<KeyListItem> = statement
            .query_map(
                params![
                    any,
                    owner_login,
                    only_superadmin,
                    after_created,
                    after_id,
                    limit as i64
                ],
                |row| {
                    Ok(KeyListItem {
                        id: row.get(0)?,
                        owner: row.get(1)?,
                        name: row.get(2)?,
                        hint: row.get(3)?,
                        created_at: row.get(4)?,
                        expires_at: row.get(5)?,
                        last_check_at: row.get(6)?,
                        check_count: row.get(7)?,
                        revoked_at: row.get(8)?,
                        kind: row
                            .get::<_, Option<String>>(9)?
                            .and_then(|name| crate::keys::KeyKind::from_name(&name)),
                        interaction: row
                            .get::<_, Option<String>>(10)?
                            .and_then(|name| crate::keys::Interaction::from_name(&name)),
                        agents: row
                            .get::<_, Option<String>>(11)?
                            .and_then(|name| crate::keys::AgentsMode::from_name(&name)),
                        grants: Vec::new(),
                    })
                },
            )
            .map_err(db)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(db)?;
        for item in items.iter_mut().filter(|i| i.revoked_at.is_none()) {
            item.grants = key_grants_of(&conn, &item.id, true)?;
        }
        Ok(items)
    }

    pub fn directory_version(&self) -> Result<i64, String> {
        self.conn()
            .query_row(
                "SELECT version FROM directory_version WHERE id = 1",
                [],
                |r| r.get(0),
            )
            .map_err(db)
    }

    pub fn load_directory(&self) -> Result<DirectorySnapshot, String> {
        let conn = self.conn();
        let version: i64 = conn
            .query_row(
                "SELECT version FROM directory_version WHERE id = 1",
                [],
                |r| r.get(0),
            )
            .map_err(db)?;
        let users = Self::load_users_with(&conn)?;
        let keys = read_keys(&conn, true)?;
        let externals = Self::load_externals_with(&conn)?;
        Ok(DirectorySnapshot {
            version,
            users,
            keys,
            externals,
        })
    }

    pub fn insert_key(&self, key: &KeyRow, caps: &KeyCaps) -> Result<(), KeyWriteError> {
        let db = |e| KeyWriteError::Db(db(e));
        let mut conn = self.conn();
        let tx = conn.transaction().map_err(db)?;
        bump(&tx).map_err(db)?;
        let owner_id: Option<i64> = match &key.owner {
            None => None,
            Some(login) => Some(
                tx.query_row("SELECT id FROM users WHERE login = ?1", [login], |r| {
                    r.get(0)
                })
                .optional()
                .map_err(db)?
                .ok_or(KeyWriteError::UnknownOwner)?,
            ),
        };
        let active: i64 = tx
            .query_row(
                "SELECT COUNT(*) FROM keys
                  WHERE owner_id IS ?1 AND (expires_at IS NULL OR expires_at > ?2)
                    AND (?3 IS NULL OR created_at >= ?3)",
                params![owner_id, caps.now, caps.created_not_before],
                |r| r.get(0),
            )
            .map_err(db)?;
        if active >= caps.max_per_user as i64 {
            return Err(KeyWriteError::TooManyKeys);
        }
        tx.execute(
            "INSERT INTO keys (id, owner_id, name, digest, hint, created_at, expires_at,
                               last_check_at, check_count, kind, interaction, agents)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
            params![
                key.id,
                owner_id,
                key.name,
                key.digest.as_slice(),
                key.hint,
                key.created_at,
                key.expires_at,
                key.last_check_at,
                key.check_count,
                key.kind.name(),
                key.interaction.name(),
                key.agents.name()
            ],
        )
        .map_err(db)?;
        insert_key_grants(&tx, &key.id, &key.grants).map_err(db)?;
        insert_counter_rows(&tx, Subject::Key(&key.id), &key.grants).map_err(db)?;
        if !within_capacity(&tx, caps).map_err(db)? {
            return Err(KeyWriteError::Capacity);
        }
        tx.commit().map_err(db)
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
        let db = |e| KeyWriteError::Db(db(e));
        let mut conn = self.conn();
        let tx = conn.transaction().map_err(db)?;
        bump(&tx).map_err(db)?;
        let found: Option<i64> = tx
            .query_row(
                "SELECT 1 FROM keys k LEFT JOIN users u ON u.id = k.owner_id
                  WHERE k.id = ?1 AND u.login IS ?2",
                params![id, owner],
                |r| r.get(0),
            )
            .optional()
            .map_err(db)?;
        if found.is_none() {
            return Err(KeyWriteError::UnknownKey);
        }
        tx.execute(
            "UPDATE keys SET name = ?2, interaction = ?3, agents = ?4 WHERE id = ?1",
            params![id, name, modes.interaction.name(), modes.agents.name()],
        )
        .map_err(db)?;
        tx.execute("DELETE FROM key_grants WHERE key_id = ?1", [id])
            .map_err(db)?;
        insert_key_grants(&tx, id, grants).map_err(db)?;
        insert_counter_rows(&tx, Subject::Key(id), grants).map_err(db)?;
        if !within_capacity(&tx, caps).map_err(db)? {
            return Err(KeyWriteError::Capacity);
        }
        tx.commit().map_err(db)
    }

    pub fn revoke_key(
        &self,
        id: &str,
        owner: Option<Option<&str>>,
        now: i64,
    ) -> Result<bool, String> {
        let mut conn = self.conn();
        let tx = conn.transaction().map_err(db)?;
        let revoked = revoke(&tx, id, owner, now).map_err(db)?;
        if revoked {
            bump(&tx).map_err(db)?;
        }
        tx.commit().map_err(db)?;
        Ok(revoked)
    }

    pub fn revoke_keys(&self, login: Option<&str>, now: i64) -> Result<usize, String> {
        let mut conn = self.conn();
        let tx = super::begin_write(&mut conn).map_err(db)?;
        let ids: Vec<String> = match login {
            Some(login) => tx
                .prepare(
                    "SELECT k.id FROM keys k JOIN users u ON u.id = k.owner_id WHERE u.login = ?1",
                )
                .map_err(db)?
                .query_map([login], |r| r.get(0))
                .map_err(db)?
                .collect::<Result<_, _>>()
                .map_err(db)?,
            None => tx
                .prepare("SELECT id FROM keys")
                .map_err(db)?
                .query_map([], |r| r.get(0))
                .map_err(db)?
                .collect::<Result<_, _>>()
                .map_err(db)?,
        };
        for id in &ids {
            revoke(&tx, id, None, now).map_err(db)?;
        }
        if !ids.is_empty() {
            bump(&tx).map_err(db)?;
        }
        tx.commit().map_err(db)?;
        Ok(ids.len())
    }

    pub fn write_key_checks(&self, checks: &[KeyCheck]) -> Result<(), String> {
        let mut conn = self.conn();
        let tx = conn.transaction().map_err(db)?;
        for check in checks {
            tx.execute(
                "UPDATE keys
                    SET last_check_at = max(coalesce(last_check_at, 0), ?2),
                        check_count = check_count + ?3
                  WHERE id = ?1",
                params![check.id, check.last_check_at, check.count],
            )
            .map_err(db)?;
        }
        tx.commit().map_err(db)
    }

    pub fn prune_keys(&self, expired_before: i64, history_before: i64) -> Result<(), String> {
        let mut conn = self.conn();
        let tx = conn.transaction().map_err(db)?;
        let deleted = tx
            .execute(
                "DELETE FROM keys WHERE expires_at IS NOT NULL AND expires_at < ?1",
                [expired_before],
            )
            .map_err(db)?;
        tx.execute(
            "DELETE FROM key_history WHERE revoked_at < ?1",
            [history_before],
        )
        .map_err(db)?;
        if deleted > 0 {
            bump(&tx).map_err(db)?;
        }
        tx.commit().map_err(db)
    }

    #[cfg(test)]
    pub fn key_history(&self) -> Result<Vec<KeyHistoryRow>, String> {
        let conn = self.conn();
        let mut statement = conn
            .prepare(
                "SELECT h.id, u.login, h.name, h.created_at, h.revoked_at, h.last_check_at, h.check_count
                   FROM key_history h LEFT JOIN users u ON u.id = h.owner_id ORDER BY h.id",
            )
            .map_err(db)?;
        let rows = statement
            .query_map([], |row| {
                Ok(KeyHistoryRow {
                    id: row.get(0)?,
                    owner: row.get(1)?,
                    name: row.get(2)?,
                    created_at: row.get(3)?,
                    revoked_at: row.get(4)?,
                    last_check_at: row.get(5)?,
                    check_count: row.get(6)?,
                })
            })
            .map_err(db)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(db);
        rows
    }
}

/// Deletes a key and writes its summary row. `owner` limits the key to one owner, as in
/// [`Sqlite::revoke_key`].
fn revoke(
    tx: &Transaction<'_>,
    id: &str,
    owner: Option<Option<&str>>,
    now: i64,
) -> rusqlite::Result<bool> {
    let sql = match owner {
        None => {
            "SELECT k.owner_id, k.name, k.created_at, k.last_check_at, k.check_count
               FROM keys k WHERE k.id = ?1 AND ?2 IS NULL"
        }
        Some(_) => {
            "SELECT k.owner_id, k.name, k.created_at, k.last_check_at, k.check_count
               FROM keys k LEFT JOIN users u ON u.id = k.owner_id
              WHERE k.id = ?1 AND u.login IS ?2"
        }
    };
    let login: Option<&str> = owner.flatten();
    let row = tx
        .query_row(sql, params![id, login], |r| {
            Ok((
                r.get::<_, Option<i64>>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, i64>(2)?,
                r.get::<_, Option<i64>>(3)?,
                r.get::<_, i64>(4)?,
            ))
        })
        .optional()?;
    let Some((owner_id, name, created_at, last_check_at, check_count)) = row else {
        return Ok(false);
    };
    tx.execute(
        "INSERT OR REPLACE INTO key_history
           (id, owner_id, name, created_at, revoked_at, last_check_at, check_count)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            id,
            owner_id,
            name,
            created_at,
            now,
            last_check_at,
            check_count
        ],
    )?;
    tx.execute("DELETE FROM keys WHERE id = ?1", [id])?;
    Ok(true)
}
