//! What the pages of the Inbox tab need from the inbox: the overview, the alerts, the connection
//! log, the manual send of an alert and the revocation of a phone.
//!
//! The portal calls these functions from its own pages (`src/http/inbox_page.rs`), in process. It
//! checks the rights of the account first: this module does not know who asks.

use hyper::StatusCode;

use super::api::{accept_alert, Accepted, ApiError};
#[cfg(test)]
pub use super::db::DeliveryView;
use super::db::NewAlert;
pub use super::db::{AlertDetail, AlertSummary, ConnectionView, DeviceView};
use super::hub::LiveInfo;
use super::log;
use super::state::App;
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

/// One group of the file, with the number of phones that subscribe to it.
pub struct GroupRow {
    pub id: String,
    pub name: String,
    pub description: String,
    pub critical: bool,
    pub subscribed: usize,
}

/// The overview: the phones, the groups and the counts of the delivery.
pub struct Overview {
    pub devices: Vec<DeviceRow>,
    pub groups: Vec<GroupRow>,
    pub online: usize,
    pub enrolled: usize,
    pub unreachable: usize,
    pub pending_deliveries: i64,
    /// The age of the oldest pending delivery, in milliseconds.
    pub oldest_pending_ms: i64,
}

pub async fn overview(app: &App) -> Result<Overview, ApiError> {
    let (devices, (pending, oldest)) = app
        .db
        .overview(app.delivery_cutoff())
        .await
        .map_err(ApiError::storage)?;
    let live = app.live_sessions().await;
    let now = now_ms();
    let unreachable_after = app.config.devices.unreachable_after.as_millis() as i64;
    let (mut online, mut unreachable, mut enrolled) = (0, 0, 0);
    let mut subscribed: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();
    let rows: Vec<DeviceRow> = devices
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
            match state {
                DeviceState::Online => online += 1,
                DeviceState::Unreachable => unreachable += 1,
                _ => {}
            }
            if device.revoked_at.is_none() {
                enrolled += 1;
            }
            DeviceRow {
                device,
                live: l,
                state,
            }
        })
        .collect();
    for row in rows.iter().filter(|r| r.state != DeviceState::Revoked) {
        for group in &row.device.subscriptions {
            *subscribed.entry(group.as_str()).or_default() += 1;
        }
    }
    let groups = app
        .config
        .groups
        .iter()
        .map(|(id, g)| GroupRow {
            id: id.clone(),
            name: g.name.clone(),
            description: g.description.clone(),
            critical: g.critical,
            subscribed: subscribed.get(id.as_str()).copied().unwrap_or(0),
        })
        .collect();
    Ok(Overview {
        devices: rows,
        groups,
        online,
        enrolled,
        unreachable,
        pending_deliveries: pending,
        oldest_pending_ms: oldest,
    })
}

/// The most recent alerts, newest first, of the groups in `only` (`None`: all the groups).
pub async fn alerts(
    app: &App,
    limit: u32,
    only: Option<&[String]>,
) -> Result<Vec<AlertSummary>, ApiError> {
    let list = app
        .db
        .recent_alerts(limit.clamp(1, 500), 0, app.delivery_cutoff())
        .await
        .map_err(ApiError::storage)?;
    Ok(match only {
        Some(groups) => list
            .into_iter()
            .filter(|a| groups.contains(&a.group))
            .collect(),
        None => list,
    })
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

/// Sends an alert for an account of the portal. The portal checked `alert.send` on the group.
/// `idempotency_key` makes a double click of the form send one alert.
pub async fn send_alert(
    app: &App,
    group: String,
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
            group,
            title,
            source: "admin".into(),
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
