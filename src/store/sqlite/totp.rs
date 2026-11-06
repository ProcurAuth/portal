//! The second factor in SQLite: the sealed secrets, the recovery codes and the check value of the key.
//!
//! One instance is the only writer of the file, so a write needs no row lock: each write is one transaction.

use rusqlite::{params, Transaction};

use super::*;

/// Removes the rows of one account. The recovery codes go first: a database without enforced foreign keys keeps
/// no orphan.
pub(super) fn remove_rows(tx: &Transaction<'_>, login: &str) -> rusqlite::Result<usize> {
    tx.execute("DELETE FROM totp_recovery WHERE login = ?1", [login])?;
    tx.execute("DELETE FROM totp_credentials WHERE login = ?1", [login])
}

impl Sqlite {
    pub fn totp_bind_key(&self, check: &[u8; 32]) -> Result<bool, String> {
        let mut conn = self.conn();
        let tx = begin_write(&mut conn).map_err(db)?;
        tx.execute(
            "INSERT OR IGNORE INTO totp_key_check (id, value) VALUES (1, ?1)",
            [check.as_slice()],
        )
        .map_err(db)?;
        let stored: Vec<u8> = tx
            .query_row("SELECT value FROM totp_key_check WHERE id = 1", [], |row| {
                row.get(0)
            })
            .map_err(db)?;
        tx.commit().map_err(db)?;
        Ok(stored == check)
    }

    pub fn totp_get(&self, login: &str) -> Result<Option<TotpRow>, String> {
        self.conn()
            .query_row(
                "SELECT secret, created_at, last_step, failed, locked_until,
                        (SELECT COUNT(*) FROM totp_recovery r WHERE r.login = c.login)
                   FROM totp_credentials c WHERE login = ?1",
                [login],
                |row| {
                    Ok(TotpRow {
                        sealed: row.get(0)?,
                        created_at: row.get(1)?,
                        last_step: row.get(2)?,
                        failed: row.get(3)?,
                        locked_until: row.get(4)?,
                        recovery_left: row.get(5)?,
                    })
                },
            )
            .optional()
            .map_err(db)
    }

    pub fn totp_logins(&self) -> Result<Vec<String>, String> {
        self.conn()
            .prepare("SELECT login FROM totp_credentials ORDER BY login")
            .map_err(db)?
            .query_map([], |row| row.get(0))
            .map_err(db)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(db)
    }

    pub fn totp_enable(
        &self,
        login: &str,
        sealed: &[u8],
        recovery: &[[u8; 32]],
        step: i64,
        now: i64,
    ) -> Result<bool, String> {
        let mut conn = self.conn();
        let tx = begin_write(&mut conn).map_err(db)?;
        let inserted = tx
            .execute(
                "INSERT OR IGNORE INTO totp_credentials (login, secret, created_at, last_step)
                 VALUES (?1, ?2, ?3, ?4)",
                params![login, sealed, now, step],
            )
            .map_err(db)?;
        if inserted == 0 {
            return Ok(false);
        }
        for hash in recovery {
            tx.execute(
                "INSERT OR IGNORE INTO totp_recovery (login, hash) VALUES (?1, ?2)",
                params![login, hash.as_slice()],
            )
            .map_err(db)?;
        }
        tx.commit().map_err(db)?;
        Ok(true)
    }

    pub fn totp_accept_step(&self, login: &str, step: i64) -> Result<bool, String> {
        let changed = self
            .conn()
            .execute(
                "UPDATE totp_credentials SET last_step = ?2, failed = 0, failed_since = 0, locked_until = 0
                  WHERE login = ?1 AND last_step < ?2",
                params![login, step],
            )
            .map_err(db)?;
        Ok(changed == 1)
    }

    pub fn totp_use_recovery(&self, login: &str, hash: &[u8; 32]) -> Result<bool, String> {
        let mut conn = self.conn();
        let tx = begin_write(&mut conn).map_err(db)?;
        let used = tx
            .execute(
                "DELETE FROM totp_recovery WHERE login = ?1 AND hash = ?2",
                params![login, hash.as_slice()],
            )
            .map_err(db)?;
        if used == 1 {
            tx.execute(
                "UPDATE totp_credentials SET failed = 0, failed_since = 0, locked_until = 0 WHERE login = ?1",
                [login],
            )
            .map_err(db)?;
        }
        tx.commit().map_err(db)?;
        Ok(used == 1)
    }

    pub fn totp_replace_recovery(
        &self,
        login: &str,
        recovery: &[[u8; 32]],
    ) -> Result<bool, String> {
        let mut conn = self.conn();
        let tx = begin_write(&mut conn).map_err(db)?;
        let exists: Option<i64> = tx
            .query_row(
                "SELECT 1 FROM totp_credentials WHERE login = ?1",
                [login],
                |row| row.get(0),
            )
            .optional()
            .map_err(db)?;
        if exists.is_none() {
            return Ok(false);
        }
        tx.execute("DELETE FROM totp_recovery WHERE login = ?1", [login])
            .map_err(db)?;
        for hash in recovery {
            tx.execute(
                "INSERT OR IGNORE INTO totp_recovery (login, hash) VALUES (?1, ?2)",
                params![login, hash.as_slice()],
            )
            .map_err(db)?;
        }
        tx.commit().map_err(db)?;
        Ok(true)
    }

    pub fn totp_remove(&self, login: &str) -> Result<bool, String> {
        let mut conn = self.conn();
        let tx = begin_write(&mut conn).map_err(db)?;
        let removed = remove_rows(&tx, login).map_err(db)?;
        tx.commit().map_err(db)?;
        Ok(removed > 0)
    }

    pub fn totp_remove_all(&self) -> Result<usize, String> {
        let mut conn = self.conn();
        let tx = begin_write(&mut conn).map_err(db)?;
        tx.execute("DELETE FROM totp_recovery", []).map_err(db)?;
        let removed = tx.execute("DELETE FROM totp_credentials", []).map_err(db)?;
        tx.execute("DELETE FROM totp_key_check", []).map_err(db)?;
        tx.commit().map_err(db)?;
        Ok(removed)
    }

    pub fn totp_begin_attempt(
        &self,
        login: &str,
        now: i64,
        limits: TotpLimits,
    ) -> Result<Attempt, String> {
        let mut conn = self.conn();
        // A write transaction from the start: two attempts never read the same count.
        let tx = begin_write(&mut conn).map_err(db)?;
        let row: Option<(i64, i64, i64)> = tx
            .query_row(
                "SELECT failed, failed_since, locked_until FROM totp_credentials WHERE login = ?1",
                [login],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()
            .map_err(db)?;
        let Some((failed, since, locked_until)) = row else {
            return Ok(Attempt::NotEnrolled);
        };
        if locked_until > now {
            return Ok(Attempt::Locked {
                until: locked_until,
            });
        }
        let (failed, since, lock) = limits.after_failure(failed, since, now);
        tx.execute(
            "UPDATE totp_credentials SET failed = ?2, failed_since = ?3,
                    locked_until = MAX(locked_until, ?4) WHERE login = ?1",
            params![login, failed, since, lock.unwrap_or(0)],
        )
        .map_err(db)?;
        tx.commit().map_err(db)?;
        Ok(Attempt::Counted { locked_until: lock })
    }
}
