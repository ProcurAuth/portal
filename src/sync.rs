//! What keeps several instances of the portal in step. They share nothing but the database, so each
//! of them, every few seconds, reads the accounts again (a change made on another instance shows
//! up) and sweeps the sessions that ran out. Nothing here runs with a SQLite file, which one
//! process owns.

use std::sync::Arc;
use std::thread;
use std::time::Duration;

use crate::http::AppState;
use crate::tracking::log_error;

/// How often an instance looks at what the others did.
pub const EVERY: Duration = Duration::from_secs(5);
/// The history rows of sessions that ended unrecorded are looked for once every this many rounds.
const CLEANUP_EVERY_ROUNDS: u32 = 12;

/// Starts the thread, when the store is a shared one.
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

/// One round. A failure is counted and logged and the next round tries again.
pub fn run_round(state: &AppState, round: u32) {
    if let Err(error) = state.users.refresh(&state.store) {
        log_error("directory_refresh_failed", &error);
    }
    state.sessions.sweep();
    if round % CLEANUP_EVERY_ROUNDS == 0 {
        state.sessions.close_lost_history();
    }
}
