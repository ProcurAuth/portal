//! The alerts that the portal makes about itself. They use the same path as the alerts of the
//! API: one transaction for the alert and its deliveries, the idempotency key, the copy of the
//! replica and the spool of the degraded mode. They have no HTTP request and no token.

use hyper::StatusCode;

use super::api::{accept_alert, ApiError};
use inbox_core::proto::Kind;

use super::db::NewAlert;
use super::log;
use super::state::App;
use super::target::Target;

/// The source of these alerts. A producer has the source `producer:<name>`: no conflict.
const SOURCE: &str = "portal";

/// Accepts one alert for the target. The result is `true` when the key was already in use: another
/// instance, or an earlier try, sent it. A key that another title already uses (two instances
/// with two configurations, during an update) counts the same way: the phones have an alert for
/// this event, and a new try would fail again.
pub async fn send_alert(
    app: &App,
    target: Target,
    title: String,
    key: String,
) -> Result<bool, ApiError> {
    let result = accept_alert(
        app,
        NewAlert {
            kind: Kind::Alert,
            target,
            title,
            source: SOURCE.into(),
            idempotency_key: key,
        },
        SOURCE,
    )
    .await;
    match result {
        Ok(accepted) => Ok(accepted.duplicate),
        Err(e) if e.status() == StatusCode::CONFLICT => {
            log::warn!(event = "watch_key_in_use", error = %e.summary());
            Ok(true)
        }
        Err(e) => Err(e),
    }
}
