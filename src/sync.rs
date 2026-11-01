//! What keeps the instances of the portal in step, and what runs on a timer.
//!
//! The instances share only the database. Each instance reads the directory of the users and the
//! keys again every few seconds (portal 10.7): when the version changed, a change made on another
//! instance shows up. Each instance also sweeps the sessions that ran out. With a SQLite file, one
//! process owns the file, so no round reads the directory.
//!
//! On both stores, the thread writes the checks of the keys that passed (portal 5.5a), and it
//! deletes the keys that expired long ago and the summary rows that are too old (portal 5.4).

use std::sync::Arc;
use std::thread;
use std::time::Duration;

use crate::http::AppState;
use crate::tracking::{log_error, unix_now};

/// The time between two rounds. An instance checks the changes that the other instances made.
pub const EVERY: Duration = Duration::from_secs(5);
/// The history rows of sessions that ended without a record are searched once in every
/// CLEANUP_EVERY_ROUNDS rounds.
const CLEANUP_EVERY_ROUNDS: u32 = 12;
/// The checks of the keys are written once in every KEY_CHECKS_EVERY_ROUNDS rounds: 15 seconds.
const KEY_CHECKS_EVERY_ROUNDS: u32 = 3;
/// The old keys and summary rows are deleted once in every PRUNE_EVERY_ROUNDS rounds: 5 minutes.
const PRUNE_EVERY_ROUNDS: u32 = 60;
/// The portal keeps an expired key for 30 days (portal 5.4).
const EXPIRED_KEY_DAYS: i64 = 30;

/// Starts the thread.
pub fn start(state: Arc<AppState>) -> Result<(), String> {
    thread::Builder::new()
        .name("portal-sync".into())
        .spawn(move || {
            let mut round = 0u32;
            loop {
                thread::sleep(EVERY);
                round = round.wrapping_add(1);
                run_round(&state, round);
            }
        })
        .map(drop)
        .map_err(|e| format!("cannot start the synchronisation thread: {e}"))
}

/// Runs one round. A failure is counted and logged. The next round tries again.
pub fn run_round(state: &AppState, round: u32) {
    directory_round(state);
    state.sessions.sweep();
    if round % CLEANUP_EVERY_ROUNDS == 0 {
        state.sessions.close_lost_history();
    }
    if round % KEY_CHECKS_EVERY_ROUNDS == 0 {
        write_key_checks(state);
    }
    if round % PRUNE_EVERY_ROUNDS == 0 {
        prune_keys(state);
    }
}

/// One round of the directory (portal 10.7): the instance reads the version, and it reads the
/// directory again when the version changed. A new directory clears the refusal memo and makes the
/// counter rows that it needs (portal 7.2a, 7.6).
pub fn directory_round(state: &AppState) {
    match state.users.refresh(&state.store) {
        Ok(true) => after_new_directory(state),
        Ok(false) => {}
        Err(error) => log_error("directory_refresh_failed", &error),
    }
}

/// What an instance does when it has a new directory.
pub fn after_new_directory(state: &AppState) {
    state.counting.clear();
    if let Err(error) = crate::bootstrap::ensure_new_counters(
        &state.config,
        &state.store,
        &state.users,
        &state.ensured_counters,
        &state.counting.stats.rows_made,
        unix_now(),
    ) {
        log_error("counter_rows_failed", &error);
    }
    crate::keys_metrics::refresh_undeclared(state);
}

/// Writes the checks that passed since the last write. A failed write keeps them for the next one.
fn write_key_checks(state: &AppState) {
    let checks = state.key_checks.take();
    if checks.is_empty() {
        return;
    }
    if let Err(error) = state.store.write_key_checks(&checks) {
        log_error("key_checks_write_failed", &error);
        state.key_checks.put_back(checks);
    }
}

/// Deletes the keys that expired more than 30 days ago and the summary rows older than
/// `keys.history_days` (portal 5.4).
fn prune_keys(state: &AppState) {
    let now = unix_now();
    let day = 86_400;
    let history = i64::from(state.config.key_limits.history_days) * day;
    if let Err(error) = state
        .store
        .prune_keys(now - EXPIRED_KEY_DAYS * day, now - history)
    {
        log_error("key_prune_failed", &error);
    }
}
