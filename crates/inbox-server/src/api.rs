//! Producer API (`/api/v1/*`) and the alert acceptance path shared with the admin front.

use std::sync::Arc;

use axum::Json;
use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use opentelemetry::KeyValue;
use inbox_core::noise::MAX_PLAINTEXT;
use inbox_core::proto::{AlertMsg, MAX_TITLE_CHARS, ServerMsg};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tracing::Instrument;

use crate::db::{self, InsertOutcome, NewAlert};
use crate::state::{AppState, Producer};
use crate::util::{hex, is_hex_id, truncate_chars};

pub struct ApiError {
    status: StatusCode,
    code: &'static str,
    message: String,
}

impl ApiError {
    pub fn new(status: StatusCode, code: &'static str, message: impl Into<String>) -> ApiError {
        ApiError { status, code, message: message.into() }
    }

    pub fn status(&self) -> StatusCode {
        self.status
    }

    pub fn message(&self) -> &str {
        &self.message
    }

    pub fn storage(e: impl std::fmt::Display) -> ApiError {
        tracing::error!(event = "storage_error", error = %e);
        ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "storage_unavailable",
            "storage error, retry",
        )
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let body = json!({ "error": { "code": self.code, "message": self.message } });
        (self.status, [(header::CACHE_CONTROL, "no-store")], Json(body)).into_response()
    }
}

#[derive(Debug, Serialize)]
pub struct Accepted {
    pub alert_id: String,
    pub duplicate: bool,
    /// Devices that will receive the alert (subscribed when it was accepted).
    pub targets: usize,
}

/// Validates, stores and fans out an alert. Returns only once it is durable.
pub async fn accept_alert(
    app: &AppState,
    alert: NewAlert,
    source_kind: &'static str,
) -> Result<Accepted, ApiError> {
    if !app.group_names.contains_key(&alert.group) {
        return Err(ApiError::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            "unknown_group",
            format!("unknown group {:?}", alert.group),
        ));
    }
    let title = alert.title.trim();
    if title.is_empty() {
        return Err(ApiError::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            "missing_title",
            "an alert needs a title",
        ));
    }
    if title.chars().count() > MAX_TITLE_CHARS {
        return Err(ApiError::new(
            StatusCode::PAYLOAD_TOO_LARGE,
            "too_large",
            format!("title exceeds {MAX_TITLE_CHARS} characters"),
        ));
    }
    if title.chars().any(char::is_control) {
        return Err(ApiError::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            "invalid_text",
            "title must be a single line without control characters",
        ));
    }
    let alert = NewAlert { title: title.to_string(), ..alert };

    // The encoded alert must fit in one encrypted frame, whatever the escaping.
    let probe = ServerMsg::Alert(AlertMsg {
        alert_id: "0".repeat(32),
        group: alert.group.clone(),
        group_name: app.group_name(&alert.group),
        title: alert.title.clone(),
        created_at_ms: i64::MAX,
    });
    if serde_json::to_vec(&probe).map(|v| v.len()).unwrap_or(usize::MAX) > MAX_PLAINTEXT {
        return Err(ApiError::new(
            StatusCode::PAYLOAD_TOO_LARGE,
            "too_large",
            "alert too large once encoded",
        ));
    }

    let span = tracing::info_span!("alert.accept", alert.group = %alert.group, alert.source = %alert.source);
    let outcome = app
        .db
        .call(move |c| Ok(db::insert_alert(c, &alert)?))
        .instrument(span)
        .await
        .map_err(ApiError::storage)?;
    match outcome {
        InsertOutcome::Created { alert_id, devices, .. } => {
            app.metrics.alerts_accepted.add(1, &[KeyValue::new("source", source_kind)]);
            app.metrics.deliveries_created.add(devices.len() as u64, &[]);
            tracing::info!(event = "alert_accepted", alert.id = %alert_id, targets = devices.len());
            if devices.is_empty() {
                tracing::warn!(event = "alert_without_targets", alert.id = %alert_id);
            }
            app.hub.wake(&devices);
            Ok(Accepted { alert_id, duplicate: false, targets: devices.len() })
        }
        InsertOutcome::Duplicate { alert_id, devices } => {
            app.metrics.alerts_duplicate.add(1, &[KeyValue::new("source", source_kind)]);
            app.hub.wake(&devices);
            Ok(Accepted { alert_id, duplicate: true, targets: devices.len() })
        }
        InsertOutcome::Conflict => Err(ApiError::new(
            StatusCode::CONFLICT,
            "idempotency_conflict",
            "idempotency key already used for a different alert",
        )),
    }
}

/// Control characters and runs of whitespace become single spaces.
fn one_line(text: &str) -> String {
    let spaced: String = text.chars().map(|c| if c.is_control() { ' ' } else { c }).collect();
    spaced.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Query parameters accepted by producer endpoints, for tools that cannot set
/// headers. A token in a URL may end up in proxy logs: prefer the header.
#[derive(Debug, Default, Deserialize)]
pub struct ProducerQuery {
    token: Option<String>,
    title: Option<String>,
}

fn producer<'a>(
    app: &'a AppState,
    headers: &HeaderMap,
    query: &ProducerQuery,
) -> Result<&'a Producer, ApiError> {
    let token = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .or(query.token.as_deref())
        .map(str::trim);
    let unauthorized = || {
        ApiError::new(StatusCode::UNAUTHORIZED, "unauthorized", "missing or invalid bearer token")
    };
    let token = token.ok_or_else(unauthorized)?;
    match app.producer_for_token(token) {
        Some(p) => Ok(p),
        None => {
            app.metrics.auth_failures.add(1, &[KeyValue::new("kind", "producer_token")]);
            Err(unauthorized())
        }
    }
}

fn check_group(p: &Producer, group: &str) -> Result<(), ApiError> {
    if p.groups.iter().any(|g| g == group) {
        Ok(())
    } else {
        Err(ApiError::new(
            StatusCode::FORBIDDEN,
            "forbidden_group",
            format!("producer may not alert group {group:?}"),
        ))
    }
}

fn parse_json<T: for<'de> Deserialize<'de>>(body: &Bytes) -> Result<T, ApiError> {
    serde_json::from_slice(body)
        .map_err(|e| ApiError::new(StatusCode::BAD_REQUEST, "invalid_json", e.to_string()))
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AlertRequest {
    pub group: String,
    /// One line, at most 128 characters.
    pub title: String,
}

/// The `Idempotency-Key` header, or, when the client sends none, a key derived
/// from the content and the current minute: an identical request retried within
/// the same minute is deduplicated.
pub fn idempotency_key(headers: &HeaderMap, content: &[&str]) -> Result<String, ApiError> {
    let Some(key) = headers.get("idempotency-key").and_then(|v| v.to_str().ok()).map(str::trim)
    else {
        let mut h = Sha256::new();
        for part in content {
            h.update((part.len() as u64).to_be_bytes());
            h.update(part.as_bytes());
        }
        h.update((crate::util::now_ms() / 60_000).to_be_bytes());
        return Ok(format!("auto:{}", hex(&h.finalize())));
    };
    if key.is_empty() || key.len() > 128 || !key.bytes().all(|b| b.is_ascii_graphic()) {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "invalid_idempotency_key",
            "Idempotency-Key must be 1..128 printable ASCII characters",
        ));
    }
    Ok(key.to_string())
}

/// `POST /api/v1/alerts`
pub async fn post_alert(
    State(app): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(query): Query<ProducerQuery>,
    body: Bytes,
) -> Result<Response, ApiError> {
    let p = producer(&app, &headers, &query)?;
    let req: AlertRequest = parse_json(&body)?;
    check_group(p, &req.group)?;
    let key = idempotency_key(&headers, &[&req.group, &req.title])?;
    let accepted = accept_alert(
        &app,
        NewAlert {
            group: req.group,
            title: req.title,
            source: format!("producer:{}", p.name),
            idempotency_key: key,
        },
        "producer",
    )
    .await?;
    let status = if accepted.duplicate { StatusCode::OK } else { StatusCode::CREATED };
    Ok((status, [(header::CACHE_CONTROL, "no-store")], Json(accepted)).into_response())
}

/// `GET /api/v1/alerts/{id}`: delivery status, for producers that escalate elsewhere.
pub async fn get_alert(
    State(app): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(query): Query<ProducerQuery>,
    Path(id): Path<String>,
) -> Result<Response, ApiError> {
    let p = producer(&app, &headers, &query)?;
    let not_found = || ApiError::new(StatusCode::NOT_FOUND, "not_found", "no such alert");
    if !is_hex_id(&id) {
        return Err(not_found());
    }
    let detail = app
        .db
        .call({
            let cutoff = app.delivery_cutoff();
            move |c| Ok(db::alert_detail(c, &id, cutoff)?)
        })
        .await
        .map_err(ApiError::storage)?;
    let detail = detail.ok_or_else(not_found)?;
    if !p.groups.contains(&detail.summary.group) {
        return Err(not_found());
    }
    Ok(([(header::CACHE_CONTROL, "no-store")], Json(detail)).into_response())
}

/// `POST /api/v1/webhooks/alertmanager/{group}`
///
/// Accepts the Prometheus Alertmanager webhook payload, which Grafana's webhook
/// contact point also emits. Each alert of the payload becomes one Inbox alert,
/// deduplicated on fingerprint + status + start time, so Alertmanager repeats and
/// retries do not page twice.
pub async fn alertmanager(
    State(app): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(query): Query<ProducerQuery>,
    Path(group): Path<String>,
    body: Bytes,
) -> Result<Response, ApiError> {
    let p = producer(&app, &headers, &query)?;
    check_group(p, &group)?;
    let payload: Value = parse_json(&body)?;
    let alerts = payload.get("alerts").and_then(Value::as_array).ok_or_else(|| {
        ApiError::new(StatusCode::BAD_REQUEST, "invalid_payload", "missing alerts array")
    })?;

    let (mut created, mut duplicates, mut skipped) = (0usize, 0usize, 0usize);
    for a in alerts {
        let status = a.get("status").and_then(Value::as_str).unwrap_or("firing");
        if status == "resolved" && !p.webhook_send_resolved {
            skipped += 1;
            continue;
        }
        let labels = a.get("labels").cloned().unwrap_or(Value::Null);
        let annotations = a.get("annotations").cloned().unwrap_or(Value::Null);
        let text = |v: &Value, k: &str| v.get(k).and_then(Value::as_str).map(str::to_string);
        let name = text(&labels, "alertname").unwrap_or_else(|| "alert".into());
        // "[FIRING] alertname: summary", on one line.
        let mut title = format!("[{}] {}", status.to_uppercase(), name);
        if let Some(summary) =
            text(&annotations, "summary").map(|t| one_line(&t)).filter(|t| !t.is_empty())
        {
            title.push_str(": ");
            title.push_str(&summary);
        }
        let fingerprint = text(a, "fingerprint").unwrap_or_else(|| {
            // serde_json maps are ordered, so this hash is stable for equal label sets.
            hex(&Sha256::digest(labels.to_string().as_bytes()))
        });
        let starts_at = text(a, "startsAt").unwrap_or_default();
        let key = format!("{}:{}:{}", fingerprint, status, starts_at);
        let key = if key.len() > 128 { hex(&Sha256::digest(key.as_bytes())) } else { key };
        let accepted = accept_alert(
            &app,
            NewAlert {
                group: group.clone(),
                title: truncate_chars(&title, MAX_TITLE_CHARS),
                source: format!("webhook:{}", p.name),
                idempotency_key: key,
            },
            "webhook",
        )
        .await?;
        if accepted.duplicate { duplicates += 1 } else { created += 1 }
    }
    Ok(Json(json!({ "created": created, "duplicates": duplicates, "skipped": skipped }))
        .into_response())
}

/// `POST /api/v1/notify/{group}`: generic webhook for any alerting tool.
///
/// The title comes from `?title=`, the `X-Title` header or the `title` field of a
/// JSON body; otherwise the body itself (plain text, or any other JSON as compact
/// text) is the title. It is put on one line and truncated to the limit rather
/// than refused: the sending tool usually cannot adapt.
pub async fn notify(
    State(app): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(query): Query<ProducerQuery>,
    Path(group): Path<String>,
    body: Bytes,
) -> Result<Response, ApiError> {
    let p = producer(&app, &headers, &query)?;
    check_group(p, &group)?;
    let header_title = headers.get("x-title").and_then(|v| v.to_str().ok()).map(str::to_string);
    let text = String::from_utf8_lossy(&body);
    let body_title = match serde_json::from_slice::<Value>(&body) {
        Ok(Value::Object(map)) if map.get("title").is_some_and(Value::is_string) => {
            map.get("title").and_then(Value::as_str).map(str::to_string)
        }
        Ok(Value::String(s)) => Some(s),
        Ok(Value::Null) | Err(_) if text.trim().is_empty() => None,
        Ok(other) => Some(other.to_string()),
        Err(_) => Some(text.to_string()),
    };
    let title = query
        .title
        .clone()
        .or(header_title)
        .or(body_title)
        .map(|t| one_line(&truncate_chars(&one_line(&t), MAX_TITLE_CHARS)))
        .unwrap_or_default();
    let key = idempotency_key(&headers, &[&group, &title])?;
    let accepted = accept_alert(
        &app,
        NewAlert { group, title, source: format!("notify:{}", p.name), idempotency_key: key },
        "notify",
    )
    .await?;
    let status = if accepted.duplicate { StatusCode::OK } else { StatusCode::CREATED };
    Ok((status, [(header::CACHE_CONTROL, "no-store")], Json(accepted)).into_response())
}
