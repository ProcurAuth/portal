//! The count of the uses of the budgets (portal 7.2b, 7.6, 7.7).
//!
//! One use takes one unit from each of its counters, or none. The database does the count, so
//! several instances count each counter one time for all of them (protocol 6.8). This module adds
//! the refusal memo: after `budget_exceeded`, an instance refuses each use of the same counter at
//! once, with no statement, until the end of its window or the next version of the directory. The
//! memo counts nothing: it only refuses (portal 7.6).
//!
//! A failed read or write of a counter fails only the use that needs it: the PDP answers HTTP 503
//! and never a decision (protocol 6.9).

#![allow(dead_code)]

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use crate::decision::{Counter, CounterOwner};
use crate::grants::CounterId;
use crate::store::{CountOutcome, Store};

/// The most entries of the refusal memo, at any number of services (portal 7.6).
pub const MEMO_ENTRIES: usize = 10_000;

/// Why the PDP cannot count a use.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Unavailable {
    /// A read or a write of the database failed (portal 10.5, cause `store_error`).
    StoreError,
    /// A counter has no row (portal 7.2b, cause `row_missing`).
    RowMissing,
    /// The count did not end within 500 ms (portal 7.7).
    WaitExceeded,
}

/// The result of the count of one use.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// The count worked. `units` is the number of units that the PDP took from each counter: 0 when
    /// no budget applies to the subject, 1 otherwise (protocol 6.12).
    Counted {
        units: u8,
    },
    /// A counter has no room (protocol 6.5, 11.1). `retry_after` is a whole number of seconds,
    /// at least 1.
    Exceeded {
        retry_after: u64,
    },
    Unavailable(Unavailable),
}

/// The figures of the counts, for the metrics (portal 10.5).
#[derive(Default)]
pub struct Stats {
    pub counted: AtomicU64,
    pub refused: AtomicU64,
    pub store_error: AtomicU64,
    pub row_missing: AtomicU64,
    pub wait_exceeded: AtomicU64,
    /// The counter rows that the instance made (portal 7.2a).
    pub rows_made: AtomicU64,
    /// The statements that the instance sent to the database.
    pub statements: AtomicU64,
    /// The total time of these statements, in microseconds.
    pub statement_micros: AtomicU64,
}

#[derive(Default)]
pub struct Counting {
    memo: Mutex<HashMap<(CounterOwner, CounterId), Instant>>,
    pub stats: Stats,
    /// A fixed clock in whole seconds since the Unix epoch. 0 is no fixed clock. Only a test sets
    /// it: the program counts with the clock of the database (portal 7.2b).
    clock: std::sync::atomic::AtomicI64,
}

fn wait_exceeded(error: &str) -> bool {
    error.contains("no free database connection")
        || error.contains("statement timeout")
        || error.contains("canceling statement")
        || error.contains("database is locked")
}

impl Counting {
    /// Removes the memo. A new version of the directory can change the amounts (portal 7.6).
    pub fn clear(&self) {
        self.memo.lock().unwrap_or_else(|p| p.into_inner()).clear();
    }

    /// Fixes the clock of the count, or gives it back to the database with `None`.
    #[cfg(test)]
    pub fn set_clock(&self, now: Option<i64>) {
        self.clock.store(now.unwrap_or(0), Ordering::Relaxed);
    }

    fn fixed_clock(&self) -> Option<f64> {
        match self.clock.load(Ordering::Relaxed) {
            0 => None,
            now => Some(now as f64),
        }
    }

    pub fn memo_entries(&self) -> usize {
        self.memo.lock().unwrap_or_else(|p| p.into_inner()).len()
    }

    /// The seconds to the latest end among the memo entries that the use matches, or `None`.
    fn remembered(&self, counters: &[Counter], now: Instant) -> Option<f64> {
        let mut memo = self.memo.lock().unwrap_or_else(|p| p.into_inner());
        let mut latest: Option<f64> = None;
        for counter in counters {
            let key = (counter.owner.clone(), counter.id.clone());
            match memo.get(&key) {
                Some(end) if *end > now => {
                    let left = end.duration_since(now).as_secs_f64();
                    latest = Some(latest.map_or(left, |l| l.max(left)));
                }
                Some(_) => {
                    memo.remove(&key);
                }
                None => {}
            }
        }
        latest
    }

    fn remember(&self, counter: &Counter, seconds_left: f64, now: Instant) {
        let mut memo = self.memo.lock().unwrap_or_else(|p| p.into_inner());
        let key = (counter.owner.clone(), counter.id.clone());
        if memo.len() >= MEMO_ENTRIES && !memo.contains_key(&key) {
            // Drop the ended entries first. When the memo is still full, it adds no entry.
            memo.retain(|_, end| *end > now);
            if memo.len() >= MEMO_ENTRIES {
                return;
            }
        }
        let end = now + Duration::from_secs_f64(seconds_left.max(0.0));
        memo.insert(key, end);
    }

    /// Counts one use on its counters. The call blocks: callers run it with
    /// [`crate::util::blocking`].
    pub fn count(&self, store: &Store, counters: &[Counter]) -> Outcome {
        if counters.is_empty() {
            return Outcome::Counted { units: 0 };
        }
        let now = Instant::now();
        if let Some(left) = self.remembered(counters, now) {
            self.stats.refused.fetch_add(1, Ordering::Relaxed);
            return Outcome::Exceeded {
                retry_after: retry_after(left),
            };
        }
        let started = Instant::now();
        let result = store.count_use(counters, self.fixed_clock());
        self.stats.statements.fetch_add(1, Ordering::Relaxed);
        self.stats.statement_micros.fetch_add(
            u64::try_from(started.elapsed().as_micros()).unwrap_or(u64::MAX),
            Ordering::Relaxed,
        );
        match result {
            Ok(CountOutcome::Counted) => {
                self.stats.counted.fetch_add(1, Ordering::Relaxed);
                Outcome::Counted { units: 1 }
            }
            Ok(CountOutcome::Refused(refusals)) => {
                self.stats.refused.fetch_add(1, Ordering::Relaxed);
                let now = Instant::now();
                let mut latest = 0f64;
                for refusal in &refusals {
                    if let Some(counter) = counters.get(refusal.index) {
                        self.remember(counter, refusal.seconds_left, now);
                    }
                    latest = latest.max(refusal.seconds_left);
                }
                Outcome::Exceeded {
                    retry_after: retry_after(latest),
                }
            }
            Ok(CountOutcome::RowMissing) => {
                self.stats.row_missing.fetch_add(1, Ordering::Relaxed);
                Outcome::Unavailable(Unavailable::RowMissing)
            }
            Err(error) if wait_exceeded(&error) => {
                self.stats.wait_exceeded.fetch_add(1, Ordering::Relaxed);
                Outcome::Unavailable(Unavailable::WaitExceeded)
            }
            Err(_) => {
                self.stats.store_error.fetch_add(1, Ordering::Relaxed);
                Outcome::Unavailable(Unavailable::StoreError)
            }
        }
    }
}

/// The whole seconds of `retry_after`: a time of 11.2 seconds gives 12, and the least value is 1
/// (protocol 11.1).
pub fn retry_after(seconds: f64) -> u64 {
    (seconds.ceil().max(1.0)) as u64
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::grants::Grant;
    use crate::limits::{Limits, Window};
    use crate::store::{KeyCaps, UserRow};

    fn counter(owner: CounterOwner, window: Window, amount: u64) -> Counter {
        Counter {
            owner,
            id: CounterId {
                service: "db".into(),
                budget: "requests".into(),
                window,
            },
            amount,
        }
    }

    fn store_with_alice() -> Store {
        let store = Store::open_in_memory();
        let budget = Limits::from_json(
            r#"{"requests":[{"amount":5,"window":"minute"},{"amount":9,"window":"day"}]}"#,
        )
        .unwrap();
        store
            .insert_user(&UserRow {
                login: "alice".into(),
                password_hash: "h".into(),
                grants: vec![Grant::new("access", "db").unwrap().with_limits(budget)],
                created_at: 1,
            })
            .unwrap();
        let _ = KeyCaps {
            max_per_user: 1,
            max_counter_rows: 1,
            max_directory_grants: 1,
            now: 0,
            created_not_before: None,
        };
        store
    }

    #[test]
    fn a_use_without_counters_counts_no_unit() {
        let counting = Counting::default();
        let store = Store::open_in_memory();
        assert_eq!(counting.count(&store, &[]), Outcome::Counted { units: 0 });
        assert_eq!(counting.stats.statements.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn the_retry_after_is_a_whole_number_of_seconds_and_at_least_1() {
        assert_eq!(retry_after(11.2), 12);
        assert_eq!(retry_after(11.0), 11);
        assert_eq!(retry_after(0.01), 1);
        assert_eq!(retry_after(0.0), 1);
        assert_eq!(retry_after(86_399.5), 86_400);
    }

    #[test]
    fn after_a_refusal_the_memo_refuses_the_next_uses_with_no_statement() {
        let counting = Counting::default();
        let store = store_with_alice();
        let alice = CounterOwner::User("alice".into());
        let one = vec![counter(alice.clone(), Window::Minute, 1)];
        assert_eq!(counting.count(&store, &one), Outcome::Counted { units: 1 });
        let Outcome::Exceeded { retry_after } = counting.count(&store, &one) else {
            panic!("the second use is refused");
        };
        assert!((1..=60).contains(&retry_after), "{retry_after}");
        assert_eq!(counting.memo_entries(), 1);
        let statements = counting.stats.statements.load(Ordering::Relaxed);
        assert_eq!(statements, 2);
        // The memo refuses with no statement, also for a use with another amount.
        let again = vec![counter(alice.clone(), Window::Minute, 100)];
        assert!(matches!(
            counting.count(&store, &again),
            Outcome::Exceeded { .. }
        ));
        assert_eq!(
            counting.stats.statements.load(Ordering::Relaxed),
            statements
        );
        // The memo counts nothing: no unit was taken by the refusals.
        counting.clear();
        assert_eq!(
            counting.count(&store, &again),
            Outcome::Counted { units: 1 }
        );
        // A counter that is not in the memo is not refused.
        let other = vec![counter(alice, Window::Day, 9)];
        assert_eq!(
            counting.count(&store, &other),
            Outcome::Counted { units: 1 }
        );
    }

    #[test]
    fn a_counter_without_a_row_and_a_failed_store_make_the_use_unavailable() {
        let counting = Counting::default();
        let store = store_with_alice();
        let nobody = vec![counter(
            CounterOwner::User("nobody".into()),
            Window::Minute,
            1,
        )];
        assert_eq!(
            counting.count(&store, &nobody),
            Outcome::Unavailable(Unavailable::RowMissing)
        );
        assert_eq!(counting.stats.row_missing.load(Ordering::Relaxed), 1);
        assert!(wait_exceeded("no free database connection"));
        assert!(wait_exceeded(
            "db error: ERROR: canceling statement due to statement timeout"
        ));
        assert!(!wait_exceeded("connection refused"));
    }

    #[test]
    fn the_memo_holds_at_most_10000_entries() {
        let counting = Counting::default();
        let now = Instant::now();
        for n in 0..MEMO_ENTRIES + 50 {
            let c = counter(CounterOwner::User(format!("u{n}")), Window::Hour, 1);
            counting.remember(&c, 600.0, now);
        }
        assert_eq!(counting.memo_entries(), MEMO_ENTRIES);
        // An entry that exists can still be refreshed when the memo is full.
        let known = counter(CounterOwner::User("u0".into()), Window::Hour, 1);
        counting.remember(&known, 1_200.0, now);
        assert_eq!(counting.memo_entries(), MEMO_ENTRIES);
        let left = counting.remembered(&[known], now).unwrap();
        assert!(left > 1_000.0);
    }
}
