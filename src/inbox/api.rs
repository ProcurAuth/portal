//! The API of the producers (`/api/v1/*`) and the path that accepts an alert. The admin calls
//! use the same path.

use std::collections::HashMap;

use hyper::body::{Bytes, Incoming};
use hyper::header::{HeaderMap, AUTHORIZATION};
use hyper::{Request, StatusCode};
use inbox_core::noise::MAX_PLAINTEXT;
use inbox_core::proto::{AlertMsg, Kind, ServerMsg, MAX_TITLE_CHARS};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use super::db::{InsertOutcome, NewAlert};
use super::http::{json, read_body, Resp};
use super::log;
use super::rights::{Decision, KeyCheck};
use super::spool::{self, Accepted as Spooled};
use super::state::{App, FileKey};
use super::util::{hex, is_hex_id, now_ms, truncate_chars};
use crate::util::parse_form;

pub struct ApiError {
    status: StatusCode,
    code: &'static str,
    message: String,
    /// The seconds in the header `Retry-After` of the answer, for a limit that has no room.
    retry_after: Option<u64>,
}

impl ApiError {
    pub fn new(status: StatusCode, code: &'static str, message: impl Into<String>) -> ApiError {
        ApiError {
            status,
            code,
            message: message.into(),
            retry_after: None,
        }
    }

    /// The same error, with the header `Retry-After`.
    pub fn retry_after(mut self, seconds: u64) -> ApiError {
        self.retry_after = Some(seconds);
        self
    }

    pub fn status(&self) -> StatusCode {
        self.status
    }

    /// One line for a log: the status, the code and the message.
    pub fn summary(&self) -> String {
        format!("{} {}: {}", self.status.as_u16(), self.code, self.message)
    }

    pub fn storage(e: impl std::fmt::Display) -> ApiError {
        log::error!(event = "storage_error", error = %e);
        ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "storage_unavailable",
            "storage error, retry",
        )
    }

    /// The JSON answer of the error.
    pub fn into_response(self) -> Resp {
        let body = json!({ "error": { "code": self.code, "message": self.message } });
        let mut response = json(self.status, &body);
        if let Some(seconds) = self.retry_after {
            if let Ok(value) = hyper::header::HeaderValue::from_str(&seconds.to_string()) {
                response
                    .headers_mut()
                    .insert(hyper::header::RETRY_AFTER, value);
            }
        }
        response
    }
}

/// Turns the result of a handler into the answer.
fn respond(result: Result<Resp, ApiError>) -> Resp {
    result.unwrap_or_else(ApiError::into_response)
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
    app: &App,
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
    let alert = NewAlert {
        title: title.to_string(),
        ..alert
    };

    // The encoded alert must fit in one encrypted frame, whatever the escaping.
    let probe = ServerMsg::Alert(AlertMsg {
        alert_id: "0".repeat(32),
        kind: alert.kind,
        group: alert.group.clone(),
        group_name: app.group_name(&alert.group),
        title: alert.title.clone(),
        created_at_ms: i64::MAX,
    });
    if serde_json::to_vec(&probe)
        .map(|v| v.len())
        .unwrap_or(usize::MAX)
        > MAX_PLAINTEXT
    {
        return Err(ApiError::new(
            StatusCode::PAYLOAD_TOO_LARGE,
            "too_large",
            "alert too large once encoded",
        ));
    }

    let outcome = match app.db.insert_alert(alert.clone()).await {
        Ok(outcome) => outcome,
        // PostgreSQL has no usable primary: the alert is kept in memory and copied to the other instances (`spool.rs`).
        Err(e) if e.is_unavailable() && app.db.spool().is_some() => {
            return accept_without_database(app, alert, source_kind).await;
        }
        Err(e) => return Err(ApiError::storage(e)),
    };
    match outcome {
        InsertOutcome::Created {
            alert_id, devices, ..
        } => {
            app.metrics.alert_accepted(source_kind, alert.kind);
            app.metrics.deliveries_created(devices.len() as u64);
            log::info!(event = "alert_accepted", alert.id = %alert_id, targets = devices.len());
            if devices.is_empty() {
                log::warn!(event = "alert_without_targets", alert.id = %alert_id);
            }
            replicate(app).await?;
            app.hub.wake(&devices);
            Ok(Accepted {
                alert_id,
                duplicate: false,
                targets: devices.len(),
            })
        }
        InsertOutcome::Duplicate { alert_id, devices } => {
            app.metrics.alert_duplicate(source_kind, alert.kind);
            // A retry after a failed copy must not be acknowledged before the copy exists.
            replicate(app).await?;
            app.hub.wake(&devices);
            Ok(Accepted {
                alert_id,
                duplicate: true,
                targets: devices.len(),
            })
        }
        InsertOutcome::Conflict => Err(ApiError::new(
            StatusCode::CONFLICT,
            "idempotency_conflict",
            "idempotency key already used for a different alert",
        )),
    }
}

/// The alert is accepted although the database cannot take it: kept in the spool, copied to the other instances before the
/// answer, and delivered at once to the phones connected here. The database gets it when it has a primary again.
async fn accept_without_database(
    app: &App,
    alert: NewAlert,
    source_kind: &'static str,
) -> Result<Accepted, ApiError> {
    let pg = app.db.pg().expect("only PostgreSQL has a spool");
    let conflict = || {
        ApiError::new(
            StatusCode::CONFLICT,
            "idempotency_conflict",
            "idempotency key already used for a different alert",
        )
    };
    // Stored a moment ago: the phones know it under that id, a retry must not create another.
    if let Some(known) = pg.cached_alert(&alert.source, &alert.idempotency_key) {
        if known.group != alert.group || known.title != alert.title {
            return Err(conflict());
        }
        app.metrics.alert_duplicate(source_kind, alert.kind);
        let targets = pg.cached_targets(&alert.group).len();
        return Ok(Accepted {
            alert_id: known.id,
            duplicate: true,
            targets,
        });
    }
    let devices = pg.cached_targets(&alert.group);
    let id = spool::alert_id(&alert.source, &alert.idempotency_key);
    let (entry, duplicate) = match pg.spool().accept(
        id,
        alert.kind,
        alert.group,
        alert.title,
        alert.source,
        alert.idempotency_key,
        now_ms(),
        devices,
    ) {
        Spooled::Created(entry) => {
            app.metrics.alert_accepted(source_kind, entry.kind);
            app.metrics.alert_spooled(source_kind, entry.kind);
            app.metrics
                .deliveries_created(entry.deliveries.len() as u64);
            log::warn!(
                event = "alert_spooled",
                alert.id = %entry.id,
                targets = entry.deliveries.len()
            );
            (entry, false)
        }
        Spooled::Duplicate(entry) => {
            app.metrics.alert_duplicate(source_kind, entry.kind);
            (entry, true)
        }
        Spooled::Conflict => return Err(conflict()),
        Spooled::Full => {
            app.metrics.spool_rejected();
            log::error!(event = "spool_full", capacity = spool::MAX_SPOOLED);
            return Err(ApiError::new(
                StatusCode::SERVICE_UNAVAILABLE,
                "spool_full",
                "the database is away and too many alerts wait for it, retry",
            ));
        }
    };
    // The phones connected here first, then the copy: a peer that does not answer must not delay them.
    app.hub.wake(&entry.waiting());
    if app.cluster.replicate(&entry.wire()).await == 0 && app.cluster.enabled() {
        log::warn!(event = "spool_unreplicated", alert.id = %entry.id);
    }
    Ok(Accepted {
        alert_id: entry.id,
        duplicate,
        targets: entry.deliveries.len(),
    })
}

/// Waits for the shared copy that contains the alert just committed, before it is acknowledged or delivered: an
/// instance that takes over from a lost node then knows every alert that was acknowledged, and a device never
/// receives an alert that a producer retry would create a second time. With `sync_alerts = false` a failed copy
/// is only logged.
async fn replicate(app: &App) -> Result<(), ApiError> {
    let Err(error) = app.replica.sync().await else {
        return Ok(());
    };
    app.metrics.replica_sync_failure();
    log::warn!(event = "replica_sync_failed", error = %error);
    if app.replica.strict_alerts() {
        return Err(ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "replication_unavailable",
            "the alert could not be replicated, retry",
        ));
    }
    Ok(())
}

/// Control characters and runs of whitespace become single spaces.
fn one_line(text: &str) -> String {
    let spaced: String = text
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    spaced.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Query parameters accepted by producer endpoints, for tools that cannot set
/// headers. A token in a URL may end up in proxy logs: prefer the header.
#[derive(Debug, Default)]
pub struct ProducerQuery {
    token: Option<String>,
    title: Option<String>,
}

impl ProducerQuery {
    /// Reads the query string of a request. An unknown parameter is ignored.
    fn read(request: &Request<Incoming>) -> Result<ProducerQuery, ApiError> {
        let Some(raw) = request.uri().query() else {
            return Ok(ProducerQuery::default());
        };
        let mut fields: HashMap<String, String> = parse_form(raw.as_bytes()).map_err(|e| {
            ApiError::new(
                StatusCode::BAD_REQUEST,
                "invalid_query",
                format!("bad query: {e}"),
            )
        })?;
        Ok(ProducerQuery {
            token: fields.remove("token"),
            title: fields.remove("title"),
        })
    }
}

/// Who sends an alert: a key of the file, or a key of the database.
enum Sender<'a> {
    FileKey(&'a FileKey),
    Key { id: String },
}

impl Sender<'_> {
    /// The name of the sender in the source of an alert.
    fn name(&self) -> String {
        match self {
            Sender::FileKey(p) => p.name.clone(),
            Sender::Key { id } => format!("key:{id}"),
        }
    }

    /// Whether a notification "resolved" of a webhook becomes an alert. A key of the database never
    /// sends them.
    fn sends_resolved(&self) -> bool {
        match self {
            Sender::FileKey(p) => p.webhook_send_resolved,
            Sender::Key { .. } => false,
        }
    }
}

/// Finds who sends. The API of the inbox verifies the keys of the kind `api` only (portal 8.3a).
/// The text gets its form checked first: the prefix, the length, the characters and the checksum,
/// before any lookup. A key of another kind gets the same answer as a text with a bad form: 401
/// with no detail. A key of the file comes next: it never depends on the portal. A key that is not
/// one of them can be a key of the database.
fn sender<'a>(
    app: &'a App,
    headers: &HeaderMap,
    query: &ProducerQuery,
) -> Result<Sender<'a>, ApiError> {
    let token = headers
        .get(AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .or(query.token.as_deref())
        .map(str::trim);
    let unauthorized = || {
        ApiError::new(
            StatusCode::UNAUTHORIZED,
            "unauthorized",
            "missing or invalid bearer token",
        )
    };
    let not_ready = || {
        ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "directory_not_ready",
            "the portal cannot check keys yet, retry in a moment",
        )
    };
    let token = token.ok_or_else(unauthorized)?;
    match crate::keys::check_format(token) {
        Ok(crate::keys::KeyKind::Api) => {}
        Ok(_) => {
            app.metrics.auth_failure("producer_kind");
            return Err(unauthorized());
        }
        Err(_) => {
            app.metrics.auth_failure("producer_token");
            return Err(unauthorized());
        }
    }
    if let Some(p) = app.file_key_for_token(token) {
        return Ok(Sender::FileKey(p));
    }
    let Some(authorizer) = app.authorizer() else {
        // Before the portal gives the authorizer, a key of the database cannot be checked.
        return Err(not_ready());
    };
    match authorizer.check_key(token) {
        KeyCheck::Valid { key_id } => Ok(Sender::Key { id: key_id }),
        KeyCheck::NotReady => Err(not_ready()),
        KeyCheck::NotAKey => {
            app.metrics.auth_failure("producer_token");
            Err(unauthorized())
        }
        KeyCheck::Invalid => {
            app.metrics.auth_failure("producer_key");
            Err(unauthorized())
        }
    }
}

/// Lets the sender do `action` on the group. A key of the file needs the group in its list: the
/// start kept only the groups that its owner may send to. A key of the database needs the decision
/// of the portal. For `alert.send` the decision counts one use, except for
/// a critical group: a critical group never has a limit.
fn authorize(app: &App, sender: &Sender<'_>, action: &str, group: &str) -> Result<(), ApiError> {
    let refused = || {
        ApiError::new(
            StatusCode::FORBIDDEN,
            "forbidden_group",
            format!("the sender has no permission {action:?} on the group {group:?}"),
        )
    };
    match sender {
        Sender::FileKey(p) => {
            if p.groups.iter().any(|g| g == group) {
                Ok(())
            } else {
                Err(refused())
            }
        }
        Sender::Key { id } => {
            let Some(authorizer) = app.authorizer() else {
                return Err(ApiError::new(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "directory_not_ready",
                    "the portal cannot check keys yet, retry in a moment",
                ));
            };
            let count_use = action == crate::portal_actions::ALERT_SEND && !app.is_critical(group);
            match authorizer.decide(id, action, group, count_use) {
                Decision::Allowed => Ok(()),
                Decision::Refused {
                    reason: "key_not_valid",
                } => {
                    app.metrics.auth_failure("producer_key");
                    Err(ApiError::new(
                        StatusCode::UNAUTHORIZED,
                        "unauthorized",
                        "missing or invalid bearer token",
                    ))
                }
                Decision::Refused { .. } => {
                    app.metrics.auth_failure("producer_rights");
                    Err(refused())
                }
                Decision::Limited { retry_after } => Err(ApiError::new(
                    StatusCode::TOO_MANY_REQUESTS,
                    "rate_limited",
                    "the limit of alerts of this key is reached, retry later",
                )
                .retry_after(retry_after)),
                Decision::NotReady => Err(ApiError::new(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "directory_not_ready",
                    "the portal cannot check keys yet, retry in a moment",
                )),
                Decision::Unavailable => Err(ApiError::new(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "rights_unavailable",
                    "the portal cannot count the use of this key now, retry in a moment",
                )),
            }
        }
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
    let Some(key) = headers
        .get("idempotency-key")
        .and_then(|v| v.to_str().ok())
        .map(str::trim)
    else {
        let mut h = Sha256::new();
        for part in content {
            h.update((part.len() as u64).to_be_bytes());
            h.update(part.as_bytes());
        }
        h.update((now_ms() / 60_000).to_be_bytes());
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

/// What a producer endpoint has read from a request before it checks the token.
struct Call {
    query: ProducerQuery,
    headers: HeaderMap,
    body: Bytes,
}

/// Reads the query and the body, in this order, as the endpoints always did.
async fn read_call(app: &App, request: Request<Incoming>) -> Result<Call, ApiError> {
    let query = ProducerQuery::read(&request)?;
    let headers = request.headers().clone();
    let body = read_body(
        request,
        app.config.devices.max_request_bytes,
        app.http.body_read_timeout,
    )
    .await?;
    Ok(Call {
        query,
        headers,
        body,
    })
}

fn created_or_ok(accepted: &Accepted) -> Resp {
    let status = if accepted.duplicate {
        StatusCode::OK
    } else {
        StatusCode::CREATED
    };
    json(
        status,
        &serde_json::to_value(accepted).unwrap_or(Value::Null),
    )
}

/// `POST /api/v1/alerts`
pub async fn post_alert(app: &App, request: Request<Incoming>) -> Resp {
    respond(post_alert_inner(app, request).await)
}

async fn post_alert_inner(app: &App, request: Request<Incoming>) -> Result<Resp, ApiError> {
    let Call {
        query,
        headers,
        body,
    } = read_call(app, request).await?;
    let who = sender(app, &headers, &query)?;
    let req: AlertRequest = parse_json(&body)?;
    authorize(app, &who, crate::portal_actions::ALERT_SEND, &req.group)?;
    let key = idempotency_key(&headers, &[&req.group, &req.title])?;
    let accepted = accept_alert(
        app,
        NewAlert {
            kind: Kind::Alert,
            group: req.group,
            title: req.title,
            source: format!("producer:{}", who.name()),
            idempotency_key: key,
        },
        "producer",
    )
    .await?;
    Ok(created_or_ok(&accepted))
}

/// `GET /api/v1/alerts/{id}`: delivery status, for producers that escalate elsewhere.
pub async fn get_alert(app: &App, request: Request<Incoming>, id: &str) -> Resp {
    respond(get_alert_inner(app, request, id).await)
}

async fn get_alert_inner(
    app: &App,
    request: Request<Incoming>,
    id: &str,
) -> Result<Resp, ApiError> {
    let query = ProducerQuery::read(&request)?;
    let who = sender(app, request.headers(), &query)?;
    let not_found = || ApiError::new(StatusCode::NOT_FOUND, "not_found", "no such alert");
    if !is_hex_id(id) {
        return Err(not_found());
    }
    let detail = app
        .db
        .alert_detail(id, app.delivery_cutoff())
        .await
        .map_err(ApiError::storage)?;
    let detail = detail.ok_or_else(not_found)?;
    // A sender that may not read the group sees the same answer as for an alert that is not there.
    if let Err(error) = authorize(
        app,
        &who,
        crate::portal_actions::ALERT_READ,
        &detail.summary.group,
    ) {
        return Err(if error.status() == StatusCode::FORBIDDEN {
            not_found()
        } else {
            error
        });
    }
    Ok(json(
        StatusCode::OK,
        &serde_json::to_value(detail).unwrap_or(Value::Null),
    ))
}

/// `POST /api/v1/webhooks/alertmanager/{group}`
///
/// Accepts the Prometheus Alertmanager webhook payload, which Grafana's webhook
/// contact point also emits. Each alert of the payload becomes one Inbox alert,
/// deduplicated on fingerprint + status + start time, so Alertmanager repeats and
/// retries do not page twice.
pub async fn alertmanager(app: &App, request: Request<Incoming>, group: &str) -> Resp {
    respond(alertmanager_inner(app, request, group).await)
}

async fn alertmanager_inner(
    app: &App,
    request: Request<Incoming>,
    group: &str,
) -> Result<Resp, ApiError> {
    let Call {
        query,
        headers,
        body,
    } = read_call(app, request).await?;
    let who = sender(app, &headers, &query)?;
    authorize(app, &who, crate::portal_actions::ALERT_SEND, group)?;
    let payload: Value = parse_json(&body)?;
    let alerts = payload
        .get("alerts")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            ApiError::new(
                StatusCode::BAD_REQUEST,
                "invalid_payload",
                "missing alerts array",
            )
        })?;

    let (mut created, mut duplicates, mut skipped) = (0usize, 0usize, 0usize);
    for a in alerts {
        let status = a.get("status").and_then(Value::as_str).unwrap_or("firing");
        if status == "resolved" && !who.sends_resolved() {
            skipped += 1;
            continue;
        }
        let labels = a.get("labels").cloned().unwrap_or(Value::Null);
        let annotations = a.get("annotations").cloned().unwrap_or(Value::Null);
        let text = |v: &Value, k: &str| v.get(k).and_then(Value::as_str).map(str::to_string);
        let name = text(&labels, "alertname").unwrap_or_else(|| "alert".into());
        // "[FIRING] alertname: summary", on one line.
        let mut title = format!("[{}] {}", status.to_uppercase(), name);
        if let Some(summary) = text(&annotations, "summary")
            .map(|t| one_line(&t))
            .filter(|t| !t.is_empty())
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
        let key = if key.len() > 128 {
            hex(&Sha256::digest(key.as_bytes()))
        } else {
            key
        };
        let accepted = accept_alert(
            app,
            NewAlert {
                kind: Kind::Alert,
                group: group.to_string(),
                title: truncate_chars(&title, MAX_TITLE_CHARS),
                source: format!("webhook:{}", who.name()),
                idempotency_key: key,
            },
            "webhook",
        )
        .await?;
        if accepted.duplicate {
            duplicates += 1
        } else {
            created += 1
        }
    }
    Ok(json(
        StatusCode::OK,
        &json!({ "created": created, "duplicates": duplicates, "skipped": skipped }),
    ))
}

/// `POST /api/v1/notify/{group}`: generic webhook for any alerting tool.
///
/// The title comes from `?title=`, the `X-Title` header or the `title` field of a
/// JSON body; otherwise the body itself (plain text, or any other JSON as compact
/// text) is the title. It is put on one line and truncated to the limit rather
/// than refused: the sending tool usually cannot adapt.
pub async fn notify(app: &App, request: Request<Incoming>, group: &str) -> Resp {
    respond(notify_inner(app, request, group).await)
}

async fn notify_inner(
    app: &App,
    request: Request<Incoming>,
    group: &str,
) -> Result<Resp, ApiError> {
    let Call {
        query,
        headers,
        body,
    } = read_call(app, request).await?;
    let who = sender(app, &headers, &query)?;
    authorize(app, &who, crate::portal_actions::ALERT_SEND, group)?;
    let header_title = headers
        .get("x-title")
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
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
    let key = idempotency_key(&headers, &[group, &title])?;
    let accepted = accept_alert(
        app,
        NewAlert {
            kind: Kind::Alert,
            group: group.to_string(),
            title,
            source: format!("notify:{}", who.name()),
            idempotency_key: key,
        },
        "notify",
    )
    .await?;
    Ok(created_or_ok(&accepted))
}
