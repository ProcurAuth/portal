//! The API of the producers (`/api/v1/*`) and the path that accepts an alert. The admin calls
//! use the same path.

use hyper::body::{Bytes, Incoming};
use hyper::header::{HeaderMap, AUTHORIZATION};
use hyper::{Request, StatusCode, Uri};
use inbox_core::noise::MAX_PLAINTEXT;
use inbox_core::proto::{AlertMsg, Kind, ServerMsg, MAX_TITLE_CHARS};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use super::db::{same_title, InsertOutcome, NewAlert, WEBHOOK_SOURCE};
use super::http::{json, read_body, Resp};
use super::log;
use super::rights::{Decision, KeyCheck, Services};
use super::spool::{self, Accepted as Spooled};
use super::state::{App, FileKey};
use super::target::Target;
use super::util::{hex, is_hex_id, now_ms, truncate_chars};
use crate::util::percent_decode_form;

#[derive(Debug)]
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

    /// The short code of the error, as the JSON answer shows it.
    pub fn code(&self) -> &'static str {
        self.code
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
    match &alert.target {
        Target::Group(group) if !app.group_names.contains_key(group) => {
            return Err(ApiError::new(
                StatusCode::UNPROCESSABLE_ENTITY,
                "unknown_group",
                format!("unknown group {group:?}"),
            ));
        }
        Target::User(login) if login.is_empty() => {
            return Err(ApiError::new(
                StatusCode::UNPROCESSABLE_ENTITY,
                "unknown_user",
                "an alert for an account needs a login",
            ));
        }
        _ => {}
    }
    // Every store must accept the key: PostgreSQL refuses U+0000, and a spooled alert that the
    // database refuses can never be written.
    if !valid_idempotency_key(&alert.idempotency_key) {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "invalid_idempotency_key",
            "the idempotency key must be 1..128 bytes without control characters",
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
    let (frame_group, frame_group_name) = alert.target.frame(|id| app.group_name(id));
    let probe = ServerMsg::Alert(AlertMsg {
        alert_id: "0".repeat(32),
        kind: alert.kind,
        group: frame_group,
        group_name: frame_group_name,
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

    // An alert about a service goes only to the phones of the accounts that may see the service:
    // the rule of the launcher (see `rights::Services`).
    let audience: Option<Vec<String>> = alert
        .service
        .as_deref()
        .map(|service| super::rights::audience(app, service).into_iter().collect());
    let outcome = match app.db.insert_alert(alert.clone(), audience.clone()).await {
        Ok(outcome) => outcome,
        // PostgreSQL has no usable primary: the alert is kept in memory and copied to the other instances (`spool.rs`).
        Err(e) if e.is_unavailable() && app.db.spool().is_some() => {
            return accept_without_database(app, alert, audience, source_kind).await;
        }
        Err(e) => return Err(ApiError::storage(e)),
    };
    match outcome {
        InsertOutcome::Created {
            alert_id, devices, ..
        } => {
            app.metrics
                .alert_accepted(source_kind, alert.kind, alert.target.kind());
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
            app.metrics
                .alert_duplicate(source_kind, alert.kind, alert.target.kind());
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
        InsertOutcome::NoPhone => Err(no_phone()),
    }
}

/// Whether every store accepts the idempotency key: 1 to 128 bytes, and no control character.
/// The key is not limited to ASCII: the keys of the alerts of the portal hold UTF-8 service ids.
pub fn valid_idempotency_key(key: &str) -> bool {
    !key.is_empty() && key.len() <= 128 && !key.chars().any(char::is_control)
}

/// The answer for an alert to an account that has no phone to use. The alert is not stored.
fn no_phone() -> ApiError {
    ApiError::new(
        StatusCode::CONFLICT,
        "no_phone",
        "the account has no phone that can receive the alert: no phone is enrolled, or each phone is revoked",
    )
}

/// The alert is accepted although the database cannot take it: kept in the spool, copied to the other instances before the
/// answer, and delivered at once to the phones connected here. The database gets it when it has a primary again. With
/// `audience`, only the phones of these accounts get a delivery.
async fn accept_without_database(
    app: &App,
    alert: NewAlert,
    audience: Option<Vec<String>>,
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
        if known.target != alert.target || !same_title(&alert.source, &known.title, &alert.title) {
            return Err(conflict());
        }
        app.metrics
            .alert_duplicate(source_kind, alert.kind, alert.target.kind());
        let targets = pg.cached_targets(&alert.target, audience.as_deref()).len();
        return Ok(Accepted {
            alert_id: known.id,
            duplicate: true,
            targets,
        });
    }
    let devices = pg.cached_targets(&alert.target, audience.as_deref());
    if devices.is_empty() && matches!(alert.target, Target::User(_)) {
        return Err(no_phone());
    }
    let id = spool::alert_id(&alert.source, &alert.idempotency_key);
    let (entry, duplicate) = match pg.spool().accept(
        id,
        alert.kind,
        alert.target.clone(),
        alert.title,
        alert.source,
        alert.idempotency_key,
        alert.service,
        now_ms(),
        devices,
    ) {
        Spooled::Created(entry) => {
            app.metrics
                .alert_accepted(source_kind, entry.kind, entry.target.kind());
            app.metrics
                .alert_spooled(source_kind, entry.kind, entry.target.kind());
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
            app.metrics
                .alert_duplicate(source_kind, entry.kind, entry.target.kind());
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

/// The parameter `title` of the query of `POST /api/v1/notify/{group}`. It is the only parameter
/// that the API reads. The key of a sender is never in the query: a URL can go into the log of a
/// proxy. The API ignores each other parameter and never decodes its value, so no error and no log
/// can show it. A parameter `title` that occurs two times, or that has a bad encoding, gives `400`.
fn query_title(uri: &Uri) -> Result<Option<String>, ApiError> {
    let bad = |reason: &str| {
        ApiError::new(
            StatusCode::BAD_REQUEST,
            "invalid_query",
            format!("bad query: {reason}"),
        )
    };
    let mut title = None;
    for pair in uri.query().unwrap_or_default().split('&') {
        let Some(value) = pair
            .strip_prefix("title=")
            .or((pair == "title").then_some(""))
        else {
            continue;
        };
        if title.is_some() {
            return Err(bad("the parameter title occurs two times"));
        }
        title = Some(percent_decode_form(value).map_err(|e| bad(&e))?);
    }
    Ok(title)
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
/// one of them can be a key of the database. The key is only in the header `Authorization: Bearer`.
/// A request without this header gets `401`, also when its query holds a key.
fn sender<'a>(app: &'a App, headers: &HeaderMap) -> Result<Sender<'a>, ApiError> {
    let token = headers
        .get(AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
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

/// Lets the sender do `action` on the group. A key of the file needs the group in the list of the
/// action: the start kept the groups of the key where its owner holds `alert.send`, and the groups
/// where it holds `alert.read`. A key of the database needs the decision
/// of the portal. For `alert.send` the decision counts one use, except for
/// a critical group: a critical group never has a limit.
fn authorize(app: &App, sender: &Sender<'_>, action: &str, group: &str) -> Result<(), ApiError> {
    let count_use = action == crate::portal_actions::ALERT_SEND && !app.is_critical(group);
    authorize_at(app, sender, action, Some(group), count_use)
}

/// Lets the sender send an alert to an account (`alert.send_user`, flat on the service `portal`).
/// A key of the file needs `direct_alerts = true`: the start checked that its owner holds the
/// action. A key of the database needs the decision of the portal. With `count_use` the decision
/// counts one use of the budget `alerts`. A direct alert is never critical, so the budget always
/// applies. The caller asks first without `count_use`, then checks the target, then asks again with
/// `count_use`: an alert that is refused for its target does not use the budget.
fn authorize_user(app: &App, sender: &Sender<'_>, count_use: bool) -> Result<(), ApiError> {
    authorize_at(
        app,
        sender,
        crate::portal_actions::ALERT_SEND_USER,
        None,
        count_use,
    )
}

/// The decision for one action of the sender. `group` is `None` for an action on the service.
fn authorize_at(
    app: &App,
    sender: &Sender<'_>,
    action: &str,
    group: Option<&str>,
    count_use: bool,
) -> Result<(), ApiError> {
    let refused = || match group {
        Some(group) => ApiError::new(
            StatusCode::FORBIDDEN,
            "forbidden_group",
            format!("the sender has no permission {action:?} on the group {group:?}"),
        ),
        None => ApiError::new(
            StatusCode::FORBIDDEN,
            "forbidden_user",
            format!("the sender has no permission {action:?} on the service portal"),
        ),
    };
    match sender {
        Sender::FileKey(p) => {
            // The start kept one list for each action, from the grants of the owner.
            use crate::portal_actions::{ALERT_READ, ALERT_SEND, ALERT_SEND_USER};
            let allowed = match (action, group) {
                (ALERT_SEND, Some(group)) => p.groups.iter().any(|g| g == group),
                (ALERT_READ, Some(group)) => p.read_groups.iter().any(|g| g == group),
                (ALERT_SEND_USER, None) => p.direct_alerts,
                _ => false,
            };
            if allowed {
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
            let decision = match group {
                Some(group) => authorizer.decide(id, action, group, count_use),
                None => authorizer.decide_service(id, action, count_use),
            };
            match decision {
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

/// The services whose alerts the sender reads: the services that its owner may open. A key of the
/// file has them from the start. `None`: the portal cannot tell.
fn sender_services(app: &App, sender: &Sender<'_>) -> Option<Services> {
    match sender {
        Sender::FileKey(p) => Some(p.services.clone()),
        Sender::Key { id } => app.authorizer()?.key_services(id),
    }
}

/// Checks that the account exists. The portal knows its accounts: the inbox asks it. `404
/// unknown_user` for an account that the directory does not hold, `503` when the directory is not
/// ready.
fn check_user_exists(app: &App, login: &str) -> Result<(), ApiError> {
    let Some(authorizer) = app.authorizer() else {
        return Err(ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "directory_not_ready",
            "the portal cannot check accounts yet, retry in a moment",
        ));
    };
    match authorizer.user_exists(login) {
        Some(true) => Ok(()),
        Some(false) => Err(ApiError::new(
            StatusCode::NOT_FOUND,
            "unknown_user",
            format!("unknown user {login:?}"),
        )),
        None => Err(ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "directory_not_ready",
            "the portal cannot check accounts yet, retry in a moment",
        )),
    }
}

/// Reads a body that must be a JSON object. A derived `Deserialize` also reads a JSON array as a struct, so the form is
/// checked first.
fn parse_object<T: for<'de> Deserialize<'de>>(body: &Bytes) -> Result<T, ApiError> {
    let value: Value = parse_json(body)?;
    if !value.is_object() {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "invalid_json",
            "the body must be a JSON object",
        ));
    }
    serde_json::from_value(value)
        .map_err(|e| ApiError::new(StatusCode::BAD_REQUEST, "invalid_json", e.to_string()))
}

fn parse_json<T: for<'de> Deserialize<'de>>(body: &Bytes) -> Result<T, ApiError> {
    serde_json::from_slice(body)
        .map_err(|e| ApiError::new(StatusCode::BAD_REQUEST, "invalid_json", e.to_string()))
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AlertRequest {
    /// The id of a group. Exactly one of `group` and `user`.
    #[serde(default)]
    pub group: Option<String>,
    /// The login of an account. Exactly one of `group` and `user`.
    #[serde(default)]
    pub user: Option<String>,
    /// One line, at most 128 characters.
    pub title: String,
}

impl AlertRequest {
    /// The target of the request: exactly one of `group` and `user`. None, or both, is a bad
    /// request.
    pub fn target(&self) -> Result<Target, ApiError> {
        match (&self.group, &self.user) {
            (Some(group), None) => Ok(Target::Group(group.clone())),
            (None, Some(user)) => Ok(Target::User(user.clone())),
            (Some(_), Some(_)) => Err(ApiError::new(
                StatusCode::BAD_REQUEST,
                "invalid_target",
                "give one target: group or user, not both",
            )),
            (None, None) => Err(ApiError::new(
                StatusCode::BAD_REQUEST,
                "invalid_target",
                "give one target: the field group or the field user",
            )),
        }
    }
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

/// What a producer endpoint has read from a request: the headers, the sender and the body.
struct Call<'a> {
    headers: HeaderMap,
    who: Sender<'a>,
    body: Bytes,
}

/// Finds the sender, then reads the body. The key comes before the body: the inbox reads no body
/// for a request without a valid key. Thus an anonymous client cannot hold a connection of
/// `server.max_connections` for `server.body_read_timeout`, and ForwardAuth keeps its connections.
async fn read_call<'a>(app: &'a App, request: Request<Incoming>) -> Result<Call<'a>, ApiError> {
    let headers = request.headers().clone();
    let who = sender(app, &headers)?;
    let body = read_body(
        request,
        app.config.devices.max_request_bytes,
        app.http.body_read_timeout,
    )
    .await?;
    Ok(Call { headers, who, body })
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
        headers, who, body, ..
    } = read_call(app, request).await?;
    let req: AlertRequest = parse_json(&body)?;
    let target = req.target()?;
    match &target {
        Target::Group(group) => authorize(app, &who, crate::portal_actions::ALERT_SEND, group)?,
        Target::User(login) => {
            // The right first, without a count. Then the target. Then the count: a refused
            // target does not use the budget.
            authorize_user(app, &who, false)?;
            check_user_exists(app, login)?;
            let phones = app
                .db
                .active_phones_by_login()
                .await
                .map_err(ApiError::storage)?;
            if !phones.contains_key(login) {
                return Err(no_phone());
            }
            authorize_user(app, &who, true)?;
        }
    }
    let key = idempotency_key(&headers, &[&target.to_string(), &req.title])?;
    let accepted = accept_alert(
        app,
        NewAlert {
            kind: Kind::Alert,
            target,
            title: req.title,
            source: format!("producer:{}", who.name()),
            idempotency_key: key,
            service: None,
        },
        "producer",
    )
    .await?;
    Ok(created_or_ok(&accepted))
}

// ---------------------------------------------------------------- the agent channel (REST door)

/// The key of an agent on the REST door: `Authorization: Bearer api_...`, a key of the directory. A key of the kind `mcp`
/// gets the same `401` as a text with a bad form. The routes of the items need the block `inbox.items`: without it the
/// routes do not exist. The result holds what the transport shows about the agent: the header `Agent-Token` and the hints
/// (lot G1b). A token in the query string is refused, because a query goes to the logs of the proxies.
fn agent_key(
    app: &App,
    uri: &Uri,
    headers: &HeaderMap,
    ip: std::net::IpAddr,
) -> Result<super::approvals::Requester, ApiError> {
    use super::rights::{AgentCheck, Door};
    let unauthorized = || {
        ApiError::new(
            StatusCode::UNAUTHORIZED,
            "unauthorized",
            "missing or invalid bearer token",
        )
    };
    if app.config.items.is_none() {
        return Err(ApiError::new(
            StatusCode::NOT_FOUND,
            "not_found",
            "no such route",
        ));
    }
    let token = super::http::bearer_token(headers).ok_or_else(unauthorized)?;
    let not_ready = || {
        ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "directory_not_ready",
            "the portal cannot check keys yet, retry in a moment",
        )
    };
    let authorizer = app.authorizer().ok_or_else(not_ready)?;
    let who = match authorizer.check_agent_key(token, Door::Rest) {
        AgentCheck::Valid(key) => super::approvals::Requester {
            key_id: key.key_id,
            owner: key.owner,
            interaction: key.interaction,
            agent: super::agents::AgentCall {
                mode: key.agents,
                ..Default::default()
            },
        },
        AgentCheck::NotReady => return Err(not_ready()),
        AgentCheck::NotAKey | AgentCheck::Invalid => {
            app.metrics.auth_failure("agent_key");
            return Err(unauthorized());
        }
    };
    if token_in_query(uri) {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "invalid_query",
            "send the agent token in the header Agent-Token or in the body, never in the query string",
        ));
    }
    super::mcp_tools::with_transport(&who, headers, ip, None).map_err(agent_error)
}

/// Whether the query string has a parameter that looks like the token of an agent.
fn token_in_query(uri: &Uri) -> bool {
    uri.query().unwrap_or_default().split('&').any(|pair| {
        let name = pair
            .split('=')
            .next()
            .unwrap_or_default()
            .to_ascii_lowercase();
        matches!(name.as_str(), "agent_token" | "agent-token")
    })
}

/// The member `agent_token` of the body of a `POST` that has no other member that the portal reads (`keepalive` and
/// `supersede`). An empty body has none. A body that is not a JSON object has none either: the portal read no body for
/// these routes before. A member that is not a text is refused.
fn body_token(body: &[u8]) -> Result<Option<String>, ApiError> {
    let Ok(Value::Object(map)) = serde_json::from_slice::<Value>(body) else {
        return Ok(None);
    };
    match map.get("agent_token") {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(token)) => Ok(Some(token.clone())),
        Some(_) => Err(ApiError::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            "invalid_argument",
            "agent_token must be a text",
        )),
    }
}

/// The body of `POST .../supersede`: the token alone (an approval), or the new question. A body with another member than
/// `agent_token` is the new question.
fn supersede_body(
    body: &[u8],
) -> Result<(Option<String>, Option<super::questions::AskArgs>), ApiError> {
    let other = matches!(serde_json::from_slice::<Value>(body), Ok(Value::Object(map))
        if map.keys().any(|k| k != "agent_token"));
    if !other {
        return Ok((body_token(body)?, None));
    }
    let args: super::questions::AskArgs = parse_object(&Bytes::copy_from_slice(body))?;
    Ok((args.agent_token.clone(), Some(args)))
}

/// The error of the agent core as the REST door answers it.
fn agent_error(e: super::mcp_tools::AgentError) -> ApiError {
    let status = StatusCode::from_u16(e.status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
    let error = ApiError::new(status, e.code, e.message);
    match e.retry_after {
        Some(seconds) => error.retry_after(seconds),
        None => error,
    }
}

fn agent_reply(
    status: StatusCode,
    result: Result<Value, super::mcp_tools::AgentError>,
) -> Result<Resp, ApiError> {
    let value = result.map_err(agent_error)?;
    Ok(json(status, &super::mcp_tools::with_notice(value)))
}

/// The value of one number of the query, such as `wait=20`. A value that is not a whole number gives `400`.
fn query_number(uri: &Uri, name: &str) -> Result<Option<u64>, ApiError> {
    let mut found = None;
    for pair in uri.query().unwrap_or_default().split('&') {
        let Some(value) = pair
            .strip_prefix(name)
            .and_then(|rest| rest.strip_prefix('='))
        else {
            continue;
        };
        let number = value.parse::<u64>().map_err(|_| {
            ApiError::new(
                StatusCode::BAD_REQUEST,
                "invalid_query",
                format!("the parameter {name} is not a whole number of at most 18 digits"),
            )
        })?;
        if found.replace(number).is_some() {
            return Err(ApiError::new(
                StatusCode::BAD_REQUEST,
                "invalid_query",
                format!("the parameter {name} occurs two times"),
            ));
        }
    }
    Ok(found)
}

/// `POST /api/v1/approval-intents`
pub async fn post_intent(app: &App, ip: std::net::IpAddr, request: Request<Incoming>) -> Resp {
    respond(post_intent_inner(app, ip, request).await)
}

async fn post_intent_inner(
    app: &App,
    ip: std::net::IpAddr,
    request: Request<Incoming>,
) -> Result<Resp, ApiError> {
    let who = agent_key(app, request.uri(), request.headers(), ip)?;
    let body = read_body(
        request,
        app.config.devices.max_request_bytes,
        app.http.body_read_timeout,
    )
    .await?;
    let args: super::mcp_tools::IntentArgs = parse_object(&body)?;
    agent_reply(
        StatusCode::CREATED,
        super::mcp_tools::approval_intent(app, &who, args).await,
    )
}

/// `POST /api/v1/questions`: the agent asks a question to the owner of its key. `201` for a new question, `200` for a repeat
/// of an earlier call (the same `idempotency_key`). The header `Idempotency-Key` is the same as the member of the body: two
/// values that differ are refused.
pub async fn post_question(app: &App, ip: std::net::IpAddr, request: Request<Incoming>) -> Resp {
    respond(post_question_inner(app, ip, request).await)
}

async fn post_question_inner(
    app: &App,
    ip: std::net::IpAddr,
    request: Request<Incoming>,
) -> Result<Resp, ApiError> {
    if app.config.questions.is_none() {
        // The routes of the questions need the block `inbox.questions`: without it they do not exist.
        return Err(ApiError::new(
            StatusCode::NOT_FOUND,
            "not_found",
            "no such route",
        ));
    }
    let who = agent_key(app, request.uri(), request.headers(), ip)?;
    let header_key = request
        .headers()
        .get("idempotency-key")
        .map(|v| v.to_str().map(|t| t.trim().to_string()))
        .transpose()
        .map_err(|_| {
            ApiError::new(
                StatusCode::BAD_REQUEST,
                "invalid_idempotency_key",
                "Idempotency-Key must be 1..128 printable ASCII characters",
            )
        })?;
    let body = read_body(
        request,
        app.config.devices.max_request_bytes,
        app.http.body_read_timeout,
    )
    .await?;
    let mut args: super::questions::AskArgs = parse_object(&body)?;
    match (&header_key, &args.idempotency_key) {
        (Some(h), Some(b)) if h != b => {
            return Err(ApiError::new(
                StatusCode::UNPROCESSABLE_ENTITY,
                "invalid_argument",
                "the header Idempotency-Key and the member idempotency_key differ",
            ));
        }
        (Some(h), None) => args.idempotency_key = Some(h.clone()),
        _ => {}
    }
    let cap = app
        .config
        .items
        .as_ref()
        .map_or(std::time::Duration::ZERO, |i| i.wait_max);
    let (value, created) = super::questions::ask(app, &who, inbox_core::proto::Via::Api, args, cap)
        .await
        .map_err(agent_error)?;
    let status = if created {
        StatusCode::CREATED
    } else {
        StatusCode::OK
    };
    Ok(json(status, &super::mcp_tools::with_notice(value)))
}

/// `GET /api/v1/items/{id}`: the answer of an item of the key. With `wait` the call renews the lease and waits at most
/// `wait` seconds, and never more than `inbox.items.wait_max`. Without `wait` it renews nothing.
pub async fn get_item(
    app: &App,
    ip: std::net::IpAddr,
    request: Request<Incoming>,
    id: &str,
) -> Resp {
    respond(get_item_inner(app, ip, request, id).await)
}

async fn get_item_inner(
    app: &App,
    ip: std::net::IpAddr,
    request: Request<Incoming>,
    id: &str,
) -> Result<Resp, ApiError> {
    let who = agent_key(app, request.uri(), request.headers(), ip)?;
    let wait = query_number(request.uri(), "wait")?.map(|seconds| {
        let bound = app
            .config
            .items
            .as_ref()
            .map_or(std::time::Duration::ZERO, |i| i.wait_max);
        std::time::Duration::from_secs(seconds).min(bound)
    });
    let after_seq = query_number(request.uri(), "after_seq")?.unwrap_or(0);
    agent_reply(
        StatusCode::OK,
        super::mcp_tools::get_answer(app, &who, None, id, wait, after_seq).await,
    )
}

/// `DELETE /api/v1/items/{id}`: the agent withdraws its item.
pub async fn delete_item(
    app: &App,
    ip: std::net::IpAddr,
    request: Request<Incoming>,
    id: &str,
) -> Resp {
    respond(delete_item_inner(app, ip, request, id).await)
}

async fn delete_item_inner(
    app: &App,
    ip: std::net::IpAddr,
    request: Request<Incoming>,
    id: &str,
) -> Result<Resp, ApiError> {
    let who = agent_key(app, request.uri(), request.headers(), ip)?;
    agent_reply(
        StatusCode::OK,
        super::mcp_tools::cancel(app, &who, None, id).await,
    )
}

/// The body of `POST /api/v1/items/{id}/turns`.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct TurnRequest {
    text: String,
    #[serde(default)]
    explanation_format: Option<inbox_core::proto::ExplanationFormat>,
    #[serde(default)]
    expect_seq: Option<u64>,
    #[serde(default)]
    agent_token: Option<String>,
}

/// `POST /api/v1/items/{id}/keepalive`, `/turns` and `/supersede`.
pub async fn item_action(
    app: &App,
    ip: std::net::IpAddr,
    request: Request<Incoming>,
    id: &str,
    action: &str,
) -> Resp {
    respond(item_action_inner(app, ip, request, id, action).await)
}

async fn item_action_inner(
    app: &App,
    ip: std::net::IpAddr,
    request: Request<Incoming>,
    id: &str,
    action: &str,
) -> Result<Resp, ApiError> {
    let who = agent_key(app, request.uri(), request.headers(), ip)?;
    let body = read_body(
        request,
        app.config.devices.max_request_bytes,
        app.http.body_read_timeout,
    )
    .await?;
    match action {
        "keepalive" => agent_reply(
            StatusCode::OK,
            super::mcp_tools::keepalive(app, &who, body_token(&body)?.as_deref(), id).await,
        ),
        "turns" => {
            let turn: TurnRequest = parse_object(&body)?;
            agent_reply(
                StatusCode::CREATED,
                super::mcp_tools::say(
                    app,
                    &who,
                    turn.agent_token.as_deref(),
                    id,
                    &turn.text,
                    turn.explanation_format,
                    turn.expect_seq,
                )
                .await,
            )
        }
        "supersede" => {
            // The body of an approval has the token at most. The body of a question is the new question.
            let (token, ask) = supersede_body(&body)?;
            let cap = app
                .config
                .items
                .as_ref()
                .map_or(std::time::Duration::ZERO, |i| i.wait_max);
            agent_reply(
                StatusCode::OK,
                super::mcp_tools::supersede(
                    app,
                    &who,
                    token.as_deref(),
                    id,
                    ask,
                    inbox_core::proto::Via::Api,
                    cap,
                )
                .await,
            )
        }
        _ => Err(ApiError::new(
            StatusCode::NOT_FOUND,
            "not_found",
            "no such route",
        )),
    }
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
    let who = sender(app, request.headers())?;
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
    // A sender that may not read the alert sees the same answer as for an alert that is not there.
    // An alert for a group needs `alert.read` on the group. An alert for an account has no group:
    // the key that sent it reads it. The account that is the target and the superadmin read it in
    // the page of the Inbox.
    match &detail.summary.target {
        Target::Group(group) => {
            if let Err(error) = authorize(app, &who, crate::portal_actions::ALERT_READ, group) {
                return Err(if error.status() == StatusCode::FORBIDDEN {
                    not_found()
                } else {
                    error
                });
            }
        }
        Target::User(_) => {
            if detail.summary.source != format!("producer:{}", who.name()) {
                return Err(not_found());
            }
        }
    }
    // An alert about a service needs the service in the services of the owner of the key: the
    // rule of the launcher. When the portal cannot tell, the key reads nothing.
    if let Some(service) = detail.summary.service.as_deref() {
        match sender_services(app, &who) {
            Some(seen) if seen.sees(Some(service)) => {}
            Some(_) => return Err(not_found()),
            None => {
                return Err(ApiError::new(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "directory_not_ready",
                    "the portal cannot check keys yet, retry in a moment",
                ))
            }
        }
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
/// deduplicated on group + fingerprint + status + start time, so Alertmanager repeats and
/// retries do not page twice. A repeat with a new summary keeps the first title. The count
/// `refused` holds each alert that the Inbox refuses: such an alert does not stop the others.
pub async fn alertmanager(app: &App, request: Request<Incoming>, group: &str) -> Resp {
    respond(alertmanager_inner(app, request, group).await)
}

/// The idempotency key of an alert of the webhook: a digest of the group, the fingerprint, the
/// status and the start time. The key has the form of an `Idempotency-Key` header, so every store
/// accepts it, and the same alert sent to two groups makes one alert in each group.
fn webhook_key(group: &str, fingerprint: &str, status: &str, starts_at: &str) -> String {
    let mut h = Sha256::new();
    for part in [group, fingerprint, status, starts_at] {
        h.update((part.len() as u64).to_be_bytes());
        h.update(part.as_bytes());
    }
    format!("am:{}", hex(&h.finalize()))
}

async fn alertmanager_inner(
    app: &App,
    request: Request<Incoming>,
    group: &str,
) -> Result<Resp, ApiError> {
    let Call { who, body, .. } = read_call(app, request).await?;
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
    // The group is the same for every alert of the payload: an unknown group refuses the payload.
    if !app.group_names.contains_key(group) {
        return Err(ApiError::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            "unknown_group",
            format!("unknown group {group:?}"),
        ));
    }

    let (mut created, mut duplicates, mut skipped, mut refused) = (0usize, 0usize, 0usize, 0usize);
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
        let mut title = format!("[{}] {}", one_line(&status.to_uppercase()), one_line(&name));
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
        let result = accept_alert(
            app,
            NewAlert {
                kind: Kind::Alert,
                target: Target::Group(group.to_string()),
                title: truncate_chars(&title, MAX_TITLE_CHARS),
                source: format!("{WEBHOOK_SOURCE}{}", who.name()),
                idempotency_key: webhook_key(group, &fingerprint, status, &starts_at),
                service: None,
            },
            "webhook",
        )
        .await;
        match result {
            Ok(accepted) if accepted.duplicate => duplicates += 1,
            Ok(_) => created += 1,
            // A retry cannot change this answer: the loop continues with the other alerts.
            Err(e) if e.status().is_client_error() => {
                refused += 1;
                app.metrics.alert_refused("webhook");
                log::warn!(event = "webhook_alert_refused", group = %group, error = %e.summary());
            }
            // The storage or the copy failed: the sender sends the whole payload again, and the
            // keys merge the alerts that are stored.
            Err(e) => return Err(e),
        }
    }
    Ok(json(
        StatusCode::OK,
        &json!({ "created": created, "duplicates": duplicates, "skipped": skipped, "refused": refused }),
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
    let uri = request.uri().clone();
    let Call { headers, who, body } = read_call(app, request).await?;
    authorize(app, &who, crate::portal_actions::ALERT_SEND, group)?;
    let query_title = query_title(&uri)?;
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
    let title = query_title
        .or(header_title)
        .or(body_title)
        .map(|t| one_line(&truncate_chars(&one_line(&t), MAX_TITLE_CHARS)))
        .unwrap_or_default();
    let key = idempotency_key(&headers, &[group, &title])?;
    let accepted = accept_alert(
        app,
        NewAlert {
            kind: Kind::Alert,
            target: Target::Group(group.to_string()),
            title,
            source: format!("notify:{}", who.name()),
            idempotency_key: key,
            service: None,
        },
        "notify",
    )
    .await?;
    Ok(created_or_ok(&accepted))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(text: &str) -> Result<AlertRequest, ApiError> {
        parse_json(&Bytes::from(text.to_string()))
    }

    #[test]
    fn a_request_has_exactly_one_target() {
        let group = request(r#"{"group":"infra","title":"t"}"#).unwrap();
        assert_eq!(group.target().unwrap(), Target::Group("infra".into()));
        let user = request(r#"{"user":"alice","title":"t"}"#).unwrap();
        assert_eq!(user.target().unwrap(), Target::User("alice".into()));
        for body in [
            r#"{"group":"infra","user":"alice","title":"t"}"#,
            r#"{"title":"t"}"#,
        ] {
            let error = request(body).unwrap().target().unwrap_err();
            assert_eq!(error.status(), StatusCode::BAD_REQUEST, "{body}");
            assert_eq!(error.code(), "invalid_target", "{body}");
        }
        // An unknown field, or a missing title, is a bad JSON as before.
        for body in [
            r#"{"group":"infra","title":"t","bogus":1}"#,
            r#"{"user":"alice"}"#,
            r#"{"user":7,"title":"t"}"#,
        ] {
            assert_eq!(
                request(body).err().unwrap().code(),
                "invalid_json",
                "{body}"
            );
        }
    }

    #[test]
    fn the_idempotency_key_of_two_targets_with_the_same_id_differs() {
        let headers = HeaderMap::new();
        let group = idempotency_key(&headers, &[&Target::Group("a".into()).to_string(), "t"]);
        let user = idempotency_key(&headers, &[&Target::User("a".into()).to_string(), "t"]);
        assert_ne!(group.unwrap(), user.unwrap());
    }

    #[test]
    fn every_store_accepts_a_valid_key_and_a_watch_key_with_utf8_is_valid() {
        for key in [
            "k",
            "auto:00ff",
            "portal:unhealthy:médias-plex:498055",
            &"x".repeat(128),
        ] {
            assert!(valid_idempotency_key(key), "{key}");
        }
        for key in ["", "fp\u{0}nul", "a\nb", "bell\u{7}", &"x".repeat(129)] {
            assert!(!valid_idempotency_key(key), "{key:?}");
        }
    }

    #[test]
    fn the_key_of_a_webhook_alert_names_the_group_and_has_the_form_of_a_header() {
        let key = webhook_key("infra", "fp\u{0}nul", "firing", "t");
        assert!(
            key.len() <= 128 && key.bytes().all(|b| b.is_ascii_graphic()),
            "{key}"
        );
        assert!(valid_idempotency_key(&key));
        assert_eq!(key, webhook_key("infra", "fp\u{0}nul", "firing", "t"));
        // The same alert to another group is another alert. The parts cannot shift.
        assert_ne!(key, webhook_key("other", "fp\u{0}nul", "firing", "t"));
        assert_ne!(
            webhook_key("infra", "ab", "c", ""),
            webhook_key("infra", "a", "bc", "")
        );
        assert_ne!(key, webhook_key("infra", "fp\u{0}nul", "resolved", "t"));
    }

    #[test]
    fn the_query_gives_only_the_title_and_never_reads_a_key() {
        let title = |text: &str| query_title(&text.parse::<Uri>().unwrap());
        assert_eq!(title("/api/v1/notify/infra").unwrap(), None);
        assert_eq!(
            title("/api/v1/notify/infra?title=Backup%20done+now").unwrap(),
            Some("Backup done now".to_string())
        );
        // The parameter `token` is not a parameter of the API: its value is never decoded, so a
        // bad encoding in it is no error, and nothing of it comes back.
        let key = "api_AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";
        assert_eq!(
            title(&format!("/api/v1/notify/infra?token={key}&title=t")).unwrap(),
            Some("t".to_string())
        );
        assert_eq!(
            title(&format!("/api/v1/notify/infra?token={key}%zz")).unwrap(),
            None
        );
        // A repeated title and a title with a bad encoding are refused. The message holds no value
        // of the query.
        for text in [
            format!("/api/v1/notify/infra?token={key}&title=a&title=b"),
            format!("/api/v1/notify/infra?token={key}&title=%zz"),
        ] {
            let error = title(&text).unwrap_err();
            assert_eq!(error.status(), StatusCode::BAD_REQUEST, "{text}");
            assert_eq!(error.code(), "invalid_query", "{text}");
            assert!(!error.summary().contains("api_"), "{}", error.summary());
        }
    }

    #[test]
    fn no_phone_is_a_conflict_with_its_own_code() {
        let error = no_phone();
        assert_eq!(error.status(), StatusCode::CONFLICT);
        assert_eq!(error.code(), "no_phone");
    }
}
