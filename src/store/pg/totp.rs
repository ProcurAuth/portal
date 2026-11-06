//! The second factor in PostgreSQL: the sealed secrets, the recovery codes and the check value of the key.
//!
//! Every instance reads and writes the same rows. The last accepted step, the recovery codes and the counters of
//! failures are changed by one statement or by one transaction with a row lock, so two instances never accept the same
//! code or the same recovery code, and never lose a failure.

use postgres::Transaction;

use super::*;

/// Removes the rows of one account. The recovery codes go with the row of the account (`ON DELETE CASCADE`).
pub(super) fn remove_rows(tx: &mut Transaction<'_>, login: &str) -> Result<u64, postgres::Error> {
    tx.execute("DELETE FROM totp_credentials WHERE login = $1", &[&login])
}

impl Pg {
    pub fn totp_bind_key(&self, check: &[u8; 32]) -> Result<bool, String> {
        self.run(|c| {
            let mut tx = c.transaction()?;
            tx.execute(
                "INSERT INTO totp_key_check (id, value) VALUES (1, $1) ON CONFLICT (id) DO NOTHING",
                &[&check.as_slice()],
            )?;
            let stored: Vec<u8> = tx
                .query_one("SELECT value FROM totp_key_check WHERE id = 1", &[])?
                .get(0);
            tx.commit()?;
            Ok(stored == check)
        })
    }

    pub fn totp_get(&self, login: &str) -> Result<Option<TotpRow>, String> {
        self.run(|c| {
            Ok(c.query_opt(
                "SELECT secret, created_at, last_step, failed, locked_until,
                        (SELECT COUNT(*) FROM totp_recovery r WHERE r.login = c.login)::bigint
                   FROM totp_credentials c WHERE login = $1",
                &[&login],
            )?
            .map(|row| TotpRow {
                sealed: row.get(0),
                created_at: row.get(1),
                last_step: row.get(2),
                failed: row.get::<_, i32>(3) as u32,
                locked_until: row.get(4),
                recovery_left: row.get::<_, i64>(5) as u32,
            }))
        })
    }

    pub fn totp_logins(&self) -> Result<Vec<String>, String> {
        self.run(|c| {
            Ok(c.query(
                "SELECT login FROM totp_credentials ORDER BY login COLLATE \"C\"",
                &[],
            )?
            .into_iter()
            .map(|row| row.get(0))
            .collect())
        })
    }

    pub fn totp_enable(
        &self,
        login: &str,
        sealed: &[u8],
        recovery: &[[u8; 32]],
        step: i64,
        now: i64,
    ) -> Result<bool, String> {
        self.run(|c| {
            let mut tx = c.transaction()?;
            let inserted = tx.execute(
                "INSERT INTO totp_credentials (login, secret, created_at, last_step)
                 VALUES ($1, $2, $3, $4) ON CONFLICT (login) DO NOTHING",
                &[&login, &sealed, &now, &step],
            )?;
            if inserted == 0 {
                return Ok(false);
            }
            for hash in recovery {
                tx.execute(
                    "INSERT INTO totp_recovery (login, hash) VALUES ($1, $2) ON CONFLICT DO NOTHING",
                    &[&login, &hash.as_slice()],
                )?;
            }
            tx.commit()?;
            Ok(true)
        })
    }

    pub fn totp_accept_step(&self, login: &str, step: i64) -> Result<bool, String> {
        self.run(|c| {
            Ok(c.execute(
                "UPDATE totp_credentials SET last_step = $2, failed = 0, failed_since = 0, locked_until = 0
                  WHERE login = $1 AND last_step < $2",
                &[&login, &step],
            )? == 1)
        })
    }

    pub fn totp_use_recovery(&self, login: &str, hash: &[u8; 32]) -> Result<bool, String> {
        self.run(|c| {
            let mut tx = c.transaction()?;
            let used = tx.execute(
                "DELETE FROM totp_recovery WHERE login = $1 AND hash = $2",
                &[&login, &hash.as_slice()],
            )?;
            if used == 1 {
                tx.execute(
                    "UPDATE totp_credentials SET failed = 0, failed_since = 0, locked_until = 0 WHERE login = $1",
                    &[&login],
                )?;
            }
            tx.commit()?;
            Ok(used == 1)
        })
    }

    pub fn totp_replace_recovery(
        &self,
        login: &str,
        recovery: &[[u8; 32]],
    ) -> Result<bool, String> {
        self.run(|c| {
            let mut tx = c.transaction()?;
            if tx
                .query_opt(
                    "SELECT 1 FROM totp_credentials WHERE login = $1 FOR UPDATE",
                    &[&login],
                )?
                .is_none()
            {
                return Ok(false);
            }
            tx.execute("DELETE FROM totp_recovery WHERE login = $1", &[&login])?;
            for hash in recovery {
                tx.execute(
                    "INSERT INTO totp_recovery (login, hash) VALUES ($1, $2) ON CONFLICT DO NOTHING",
                    &[&login, &hash.as_slice()],
                )?;
            }
            tx.commit()?;
            Ok(true)
        })
    }

    pub fn totp_remove(&self, login: &str) -> Result<bool, String> {
        self.run(|c| {
            let mut tx = c.transaction()?;
            let removed = remove_rows(&mut tx, login)? > 0;
            tx.commit()?;
            Ok(removed)
        })
    }

    pub fn totp_remove_all(&self) -> Result<usize, String> {
        self.run(|c| {
            let mut tx = c.transaction()?;
            let removed = tx.execute("DELETE FROM totp_credentials", &[])?;
            tx.execute("DELETE FROM totp_key_check", &[])?;
            tx.commit()?;
            Ok(removed as usize)
        })
    }

    pub fn totp_begin_attempt(
        &self,
        login: &str,
        now: i64,
        limits: TotpLimits,
    ) -> Result<Attempt, String> {
        self.run(|c| {
            let mut tx = c.transaction()?;
            // The row lock serializes the attempts of every instance.
            let Some(row) = tx.query_opt(
                "SELECT failed, failed_since, locked_until FROM totp_credentials WHERE login = $1 FOR UPDATE",
                &[&login],
            )?
            else {
                return Ok(Attempt::NotEnrolled);
            };
            let (failed, since, locked_until) = (
                i64::from(row.get::<_, i32>(0)),
                row.get::<_, i64>(1),
                row.get::<_, i64>(2),
            );
            if locked_until > now {
                return Ok(Attempt::Locked {
                    until: locked_until,
                });
            }
            let (failed, since, lock) = limits.after_failure(failed, since, now);
            tx.execute(
                "UPDATE totp_credentials SET failed = $2, failed_since = $3,
                        locked_until = GREATEST(locked_until, $4) WHERE login = $1",
                &[&login, &(failed as i32), &since, &lock.unwrap_or(0)],
            )?;
            tx.commit()?;
            Ok(Attempt::Counted { locked_until: lock })
        })
    }
}
