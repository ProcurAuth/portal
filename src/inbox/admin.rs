//! The admin API of the inbox: the overview, the alerts, the connections, the sending of an
//! alert and the revocation of a device.
//!
//! The portal serves these calls under `/auth/inbox/api/` and lets only the session of the
//! superadmin use them (see `src/http/inbox_admin.rs`). This module does not check who asks.

use hyper::StatusCode;
use serde::Deserialize;
use serde_json::{json, Value};

use super::api::{accept_alert, ApiError};
use super::db::NewAlert;
use super::http::{json, Resp};
use super::log;
use super::state::App;
use super::util::{is_hex_id, now_ms};

pub async fn overview(app: &App) -> Result<Resp, ApiError> {
    let (devices, (pending, oldest)) = app
        .db
        .overview(app.delivery_cutoff())
        .await
        .map_err(ApiError::storage)?;
    let live = app.live_sessions().await;
    let now = now_ms();
    let unreachable_after = app.config.devices.unreachable_after.as_millis() as i64;
    let (mut online, mut unreachable, mut enrolled) = (0, 0, 0);
    let devices: Vec<_> = devices
        .into_iter()
        .map(|d| {
            let l = live.get(&d.id).cloned();
            let status = if d.revoked_at.is_some() {
                "revoked"
            } else if l.is_some() {
                "online"
            } else if !d.subscriptions.is_empty()
                && d.last_seen_at.is_none_or(|t| now - t > unreachable_after)
            {
                "unreachable"
            } else {
                "offline"
            };
            match status {
                "online" => online += 1,
                "unreachable" => unreachable += 1,
                _ => {}
            }
            if d.revoked_at.is_none() {
                enrolled += 1;
            }
            json!({ "device": d, "live": l, "status": status })
        })
        .collect();
    Ok(json(
        StatusCode::OK,
        &json!({
        "server": {
            "name": app.config.name,
            "version": super::VERSION,
            "time_ms": now,
            "unreachable_after_ms": unreachable_after,
            // A delivery younger than this is still in flight, not failed.
            "resend_after_ms": app.config.devices.resend_after.as_millis() as u64,
            "heartbeat_interval_ms": app.config.devices.heartbeat_interval.as_millis() as u64,
            // Port of the device and producer listener.
            "api_port": app.config.devices.listen.port(),
        },
        // Producers and their groups; tokens are never exposed.
        "producers": app.secrets.producers.iter().map(|p| json!({
            "name": p.name,
            "groups": p.groups,
            "webhook_send_resolved": p.webhook_send_resolved,
        })).collect::<Vec<_>>(),
        "groups": app.groups,
        "devices": devices,
        "stats": {
            "online": online,
            "enrolled": enrolled,
            "unreachable": unreachable,
            "pending_deliveries": pending,
            "oldest_pending_ms": oldest,
        },
        }),
    ))
}

/// The query of the lists. The values come from the query string of the request.
pub struct ListQuery {
    pub limit: Option<u32>,
    /// Only connections of this login (the user name of the device).
    pub user: Option<String>,
    /// Only alerts created at or after this Unix time in milliseconds.
    pub since_ms: Option<i64>,
}

impl ListQuery {
    /// Reads `limit`, `user` and `since_ms`. A value that is not a number is an error.
    pub fn parse(query: Option<&str>) -> Result<ListQuery, ApiError> {
        let bad = |what: &str| {
            ApiError::new(
                StatusCode::BAD_REQUEST,
                "invalid_query",
                format!("bad query: {what}"),
            )
        };
        let fields = match query {
            Some(raw) => crate::util::parse_form(raw.as_bytes()).map_err(|e| bad(&e))?,
            None => Default::default(),
        };
        Ok(ListQuery {
            limit: fields
                .get("limit")
                .map(|v| v.parse::<u32>().map_err(|_| bad("limit is not a number")))
                .transpose()?,
            user: fields.get("user").cloned(),
            since_ms: fields
                .get("since_ms")
                .map(|v| {
                    v.parse::<i64>()
                        .map_err(|_| bad("since_ms is not a number"))
                })
                .transpose()?,
        })
    }
}

pub async fn alerts(app: &App, q: ListQuery) -> Result<Resp, ApiError> {
    let limit = q.limit.unwrap_or(50).clamp(1, 500);
    let since = q.since_ms.unwrap_or(0).max(0);
    let list = app
        .db
        .recent_alerts(limit, since, app.delivery_cutoff())
        .await
        .map_err(ApiError::storage)?;
    Ok(json(StatusCode::OK, &json!({ "alerts": list })))
}

pub async fn alert(app: &App, id: &str) -> Result<Resp, ApiError> {
    let not_found = || ApiError::new(StatusCode::NOT_FOUND, "not_found", "no such alert");
    if !is_hex_id(id) {
        return Err(not_found());
    }
    let detail = app
        .db
        .alert_detail(id, app.delivery_cutoff())
        .await
        .map_err(ApiError::storage)?;
    Ok(json(
        StatusCode::OK,
        &serde_json::to_value(detail.ok_or_else(not_found)?).unwrap_or(Value::Null),
    ))
}

pub async fn connections(app: &App, q: ListQuery) -> Result<Resp, ApiError> {
    let limit = q.limit.unwrap_or(100).clamp(1, 1000);
    let user = q.user.filter(|u| !u.trim().is_empty());
    let list = app
        .db
        .recent_connections(limit, user)
        .await
        .map_err(ApiError::storage)?;
    Ok(json(StatusCode::OK, &json!({ "connections": list })))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AdminAlert {
    group: String,
    title: String,
    idempotency_key: String,
}

pub async fn send_alert(app: &App, body: &[u8]) -> Result<Resp, ApiError> {
    let req: AdminAlert = serde_json::from_slice(body)
        .map_err(|e| ApiError::new(StatusCode::BAD_REQUEST, "invalid_json", e.to_string()))?;
    if req.idempotency_key.is_empty() || req.idempotency_key.len() > 128 {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "invalid_idempotency_key",
            "idempotency_key must be 1..128 bytes",
        ));
    }
    let accepted = accept_alert(
        app,
        NewAlert {
            group: req.group,
            title: req.title,
            source: "admin".into(),
            idempotency_key: req.idempotency_key,
        },
        "admin",
    )
    .await?;
    let status = if accepted.duplicate {
        StatusCode::OK
    } else {
        StatusCode::CREATED
    };
    Ok(json(
        status,
        &serde_json::to_value(&accepted).unwrap_or(Value::Null),
    ))
}

pub async fn revoke(app: &App, id: &str) -> Result<Resp, ApiError> {
    if !is_hex_id(id) {
        return Err(ApiError::new(
            StatusCode::NOT_FOUND,
            "not_found",
            "no such device",
        ));
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
    Ok(json(StatusCode::OK, &json!({ "revoked": revoked })))
}
