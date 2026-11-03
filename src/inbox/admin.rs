//! What the page of the Inbox tab needs from the inbox: the phones, the alerts, the connection
//! log, the manual send of an alert and the revocation of a phone.
//!
//! The portal calls these functions from its own page (`src/http/inbox_page.rs`), in process. It
//! checks the rights of the account first: this module does not know who asks.

use hyper::StatusCode;

use super::api::{accept_alert, Accepted, ApiError};
#[cfg(test)]
pub use super::db::DeliveryView;
use inbox_core::proto::Kind;

use super::db::NewAlert;
pub use super::db::{AlertCount, AlertDetail, AlertSummary, ConnectionView, DeviceView};
use super::hub::LiveInfo;
use super::log;
use super::state::App;
pub use super::target::{account_of_source, key_of_source, Target};
use super::util::{is_hex_id, now_ms};

/// The state of a phone, as the pages show it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeviceState {
    /// A session is open.
    Online,
    /// No session, and the phone was seen not long ago or has no subscription.
    Offline,
    /// The phone has a subscription and was not seen for `inbox.devices.unreachable_after`.
    Unreachable,
    Revoked,
}

impl DeviceState {
    pub fn word(self) -> &'static str {
        match self {
            Self::Online => "online",
            Self::Offline => "offline",
            Self::Unreachable => "unreachable",
            Self::Revoked => "revoked",
        }
    }
}

/// One phone with its state.
pub struct DeviceRow {
    pub device: DeviceView,
    pub live: Option<LiveInfo>,
    pub state: DeviceState,
}

/// The phones with their state.
pub async fn devices(app: &App) -> Result<Vec<DeviceRow>, ApiError> {
    let (devices, _) = app
        .db
        .overview(app.delivery_cutoff())
        .await
        .map_err(ApiError::storage)?;
    let live = app.live_sessions().await;
    let now = now_ms();
    let unreachable_after = app.config.devices.unreachable_after.as_millis() as i64;
    Ok(devices
        .into_iter()
        .map(|device| {
            let l = live.get(&device.id).cloned();
            let state = if device.revoked_at.is_some() {
                DeviceState::Revoked
            } else if l.is_some() {
                DeviceState::Online
            } else if !device.subscriptions.is_empty()
                && device
                    .last_seen_at
                    .is_none_or(|t| now - t > unreachable_after)
            {
                DeviceState::Unreachable
            } else {
                DeviceState::Offline
            };
            DeviceRow {
                device,
                live: l,
                state,
            }
        })
        .collect())
}

/// The most recent alerts since `since_ms`, newest first, that `may_read` keeps. The second value
/// is `true` when the store gave `limit` alerts: older ones can exist. The store gives all the
/// alerts and the page decides which ones the account may see: an alert for a group needs
/// `alert.read` on the group, an alert for an account is for its sender, its target and the
/// superadmin.
pub async fn alerts(
    app: &App,
    limit: u32,
    since_ms: i64,
    may_read: impl Fn(&AlertSummary) -> bool,
) -> Result<(Vec<AlertSummary>, bool), ApiError> {
    let limit = limit.clamp(1, 500);
    let list = app
        .db
        .recent_alerts(limit, since_ms, app.delivery_cutoff())
        .await
        .map_err(ApiError::storage)?;
    let truncated = list.len() >= limit as usize;
    Ok((
        list.into_iter().filter(|a| may_read(a)).collect(),
        truncated,
    ))
}

/// The number of phones that are not revoked, for each login that has one at least. An account
/// that has no phone is not in the map: a direct alert to it answers 409 `no_phone`.
pub async fn active_phone_counts(
    app: &App,
) -> Result<std::collections::BTreeMap<String, u32>, ApiError> {
    app.db
        .active_phones_by_login()
        .await
        .map_err(ApiError::storage)
}

/// The alerts since `since_ms`, counted for each bucket of `bucket_ms` and each group: the data of
/// the graph. The page keeps the groups that the account may read.
pub async fn alert_counts(
    app: &App,
    since_ms: i64,
    bucket_ms: i64,
) -> Result<Vec<AlertCount>, ApiError> {
    app.db
        .alert_counts(since_ms, bucket_ms)
        .await
        .map_err(ApiError::storage)
}

/// One alert with its deliveries. `None`: no such alert.
pub async fn alert(app: &App, id: &str) -> Result<Option<AlertDetail>, ApiError> {
    if !is_hex_id(id) {
        return Ok(None);
    }
    app.db
        .alert_detail(id, app.delivery_cutoff())
        .await
        .map_err(ApiError::storage)
}

/// The connection log, newest first, for one login when `login` is given.
pub async fn connections(
    app: &App,
    limit: u32,
    login: Option<String>,
) -> Result<Vec<ConnectionView>, ApiError> {
    let login = login.filter(|l| !l.trim().is_empty());
    app.db
        .recent_connections(limit.clamp(1, 1000), login)
        .await
        .map_err(ApiError::storage)
}

/// Sends an alert for an account of the portal. The portal checked the right of the sender:
/// `alert.send` on the group, or `alert.send_user` for an account. The portal checked that the
/// account of the target exists. `sender` is the login of the account that uses the form: it is
/// in the source of the alert, so the alert has its sender. `idempotency_key` makes a double
/// click of the form send one alert. An account with no phone gives 409 `no_phone`.
pub async fn send_alert(
    app: &App,
    sender: &str,
    target: Target,
    title: String,
    idempotency_key: String,
) -> Result<Accepted, ApiError> {
    if idempotency_key.is_empty() || idempotency_key.len() > 128 {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "invalid_idempotency_key",
            "idempotency_key must be 1..128 bytes",
        ));
    }
    accept_alert(
        app,
        NewAlert {
            kind: Kind::Alert,
            target,
            title,
            source: format!("admin:{sender}"),
            idempotency_key,
        },
        "admin",
    )
    .await
}

/// The login of the account that owns the phone. `None`: no such phone.
pub async fn owner_of(app: &App, id: &str) -> Result<Option<String>, ApiError> {
    if !is_hex_id(id) {
        return Ok(None);
    }
    Ok(app
        .db
        .find_device(id)
        .await
        .map_err(ApiError::storage)?
        .map(|d| d.user_name))
}

/// Revokes a phone. The session of the phone ends with the code 4403. The result is `true` when
/// the phone was not revoked before.
pub async fn revoke(app: &App, id: &str) -> Result<bool, ApiError> {
    if !is_hex_id(id) {
        return Ok(false);
    }
    let revoked = app.db.revoke_device(id).await.map_err(ApiError::storage)?;
    if revoked {
        if let Err(e) = app.replica.sync().await {
            app.metrics.replica_sync_failure();
            log::warn!(event = "replica_sync_failed", error = %e);
        }
        app.hub.kick(id, inbox_core::proto::close::REVOKED);
        log::info!(event = "device_revoked", device.id = %id);
    }
    Ok(revoked)
}
