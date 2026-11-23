//! The measures of the keys, the counters and the directory.
//!
//! No label names a user or a key. The labels `service` and `budget` are not used: a measure has
//! the same series at any number of services.

use std::sync::atomic::Ordering;

use crate::grants::Grant;
use crate::http::AppState;
use crate::users::KeyOwner;

/// The number of values of limits that the current documents of the services do not declare
///: a value on a grant of the database or of the file, for a limit that the document
/// of its service does not declare. A service with no known document counts no value.
pub fn undeclared_values(state: &AppState) -> u64 {
    let count = |grants: &[Grant]| -> u64 {
        let mut total = 0;
        for grant in grants.iter().filter(|g| !g.limits.is_empty()) {
            let Some(document) = state.actions.declared(&grant.service) else {
                continue;
            };
            total += grant
                .limits
                .iter()
                .filter(|(name, _)| !document.limits.iter().any(|d| d.name == *name))
                .count() as u64;
        }
        total
    };
    let mut total = 0;
    for (_, user) in state.users.list() {
        total += count(&user.grants);
    }
    for key in state.users.keys() {
        total += count(&key.grants);
    }
    for account in state.config.access.accounts.values() {
        total += count(&account.grants);
    }
    total
}

/// Reads the number again and keeps it for the measure. The call is for a new
/// directory and a new action document, not for each scrape.
pub fn refresh_undeclared(state: &AppState) {
    let total = undeclared_values(state);
    state.undeclared_values.store(total, Ordering::Relaxed);
}

/// The active keys: not expired and not too old.
fn active_keys(state: &AppState, now: i64) -> usize {
    state
        .users
        .keys()
        .iter()
        .filter(|key| {
            let max = match &key.owner {
                KeyOwner::User(login) => {
                    crate::keyauth::person_grants(state, login).and_then(|(grants, _)| {
                        crate::portal_actions::max_key_lifetime(&grants, key.kind)
                    })
                }
                _ => None,
            };
            !key.expired(now) && !key.too_old(now, max)
        })
        .count()
}

/// The text of the measures of this module, in the format of Prometheus.
pub fn render(state: &AppState) -> String {
    let now = crate::tracking::unix_now();
    let stats = &state.counting.stats;
    let read = |c: &std::sync::atomic::AtomicU64| c.load(Ordering::Relaxed);
    let mut out = String::new();
    if state.config.keys.is_some() {
        out.push_str(&state.key_stats.render());
        out.push_str(&format!(
            "# HELP portal_active_keys Keys that are not expired and not too old.\n# TYPE portal_active_keys gauge\nportal_active_keys {}\n",
            active_keys(state, now)
        ));
    }
    out.push_str(&format!(
        "# HELP portal_budget_units_total Uses counted, by result.\n# TYPE portal_budget_units_total counter\nportal_budget_units_total{{result=\"counted\"}} {}\nportal_budget_units_total{{result=\"refused\"}} {}\n",
        read(&stats.counted),
        read(&stats.refused)
    ));
    out.push_str(&format!(
        "# HELP portal_budget_refunds_total Counted uses whose units went back, by result. The approval of the use could not be consumed.\n# TYPE portal_budget_refunds_total counter\nportal_budget_refunds_total{{result=\"refunded\"}} {}\nportal_budget_refunds_total{{result=\"failed\"}} {}\n",
        read(&stats.refunded),
        read(&stats.refund_failed)
    ));
    out.push_str(&format!(
        "# HELP portal_counter_statement_seconds The time of the counter statements.\n# TYPE portal_counter_statement_seconds summary\nportal_counter_statement_seconds_sum {:.6}\nportal_counter_statement_seconds_count {}\n",
        read(&stats.statement_micros) as f64 / 1_000_000.0,
        read(&stats.statements)
    ));
    out.push_str(&format!(
        "# HELP portal_counter_wait_exceeded_total Counts that took more than 500 ms.\n# TYPE portal_counter_wait_exceeded_total counter\nportal_counter_wait_exceeded_total {}\n# HELP portal_counter_rows_made_total Counter rows that the instance made.\n# TYPE portal_counter_rows_made_total counter\nportal_counter_rows_made_total {}\n",
        read(&stats.wait_exceeded),
        read(&stats.rows_made)
    ));
    out.push_str(&format!(
        "# HELP portal_limit_values_undeclared Limit values that the documents of the services do not declare.\n# TYPE portal_limit_values_undeclared gauge\nportal_limit_values_undeclared {}\n",
        read(&state.undeclared_values)
    ));
    if let Ok(rows) = crate::util::blocking(|| state.store.counter_rows()) {
        out.push_str(&format!(
            "# HELP portal_counter_rows Rows of the table of the counters.\n# TYPE portal_counter_rows gauge\nportal_counter_rows {rows}\n"
        ));
    }
    let file_grants: usize = state
        .config
        .access
        .accounts
        .values()
        .map(|a| a.grants.len() + a.keys.iter().map(|k| k.grants.len()).sum::<usize>())
        .sum();
    out.push_str(&format!(
        "# HELP portal_directory_grants Grants of the users and the keys.\n# TYPE portal_directory_grants gauge\nportal_directory_grants {}\n",
        state.users.grant_count() + file_grants
    ));
    // With SQLite, the one instance is the one writer: the age of the directory is always 0.
    if !state.store.is_shared() {
        out.push_str("# HELP portal_directory_age_seconds Seconds since the last good read of the directory.\n# TYPE portal_directory_age_seconds gauge\nportal_directory_age_seconds 0\n");
    }
    let catalog = crate::http::current_catalog_of(state);
    let services: Vec<String> = catalog
        .groups()
        .iter()
        .flat_map(|g| g.services.iter())
        .filter(|s| !s.external)
        .map(|s| s.id.clone())
        .collect();
    out.push_str(&format!(
        "# HELP portal_actions_age_seconds The age of the oldest document of a service.\n# TYPE portal_actions_age_seconds gauge\nportal_actions_age_seconds {:.3}\n",
        state
            .actions
            .oldest_age(services.iter().map(String::as_str))
            .as_secs_f64()
    ));
    out
}
