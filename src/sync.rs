//! What keeps several instances of the portal in step. The instances share only the database. Each
//! instance reads the accounts again every few seconds. A change made on another instance then
//! shows up. Each instance also sweeps the sessions that ran out. Nothing here runs with a SQLite
//! file. One process owns a SQLite file.

use std::sync::Arc;
use std::thread;
use std::time::Duration;

use crate::http::AppState;
use crate::tracking::log_error;

/// The time between two rounds. An instance checks the changes that the other instances made.
pub const EVERY: Duration = Duration::from_secs(5);
/// The history rows of sessions that ended without a record are searched once in every
/// CLEANUP_EVERY_ROUNDS rounds.
const CLEANUP_EVERY_ROUNDS: u32 = 12;

/// Starts the thread. The thread starts only when the store is a shared one.
pub fn start(state: Arc<AppState>) -> Result<(), String> {
    if !state.store.is_shared() {
        return Ok(());
    }
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
    if let Err(error) = state.users.refresh(&state.store) {
        log_error("directory_refresh_failed", &error);
    }
    state.sessions.sweep();
    if round % CLEANUP_EVERY_ROUNDS == 0 {
        state.sessions.close_lost_history();
    }
}
