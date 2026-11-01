//! The AuthZEN PDP (protocol 1.3 and section 12, OpenID AuthZEN Authorization API 1.0), on a second
//! listener.
//!
//! The services of the deployment ask this PDP if a subject can do an action. Each service has its
//! own bearer token (a `pdp_client` block). The PDP answers only for the resources of the service
//! that asks.
//!
//! The subject has the type `user`. Its id is the id of a person, or the subject id of a key
//! (`key:<id>`, protocol 3.2). The decision for a person looks at the grants of the person. The
//! decision for a key has two sides, the grants of the key and the grants of its owner, and each
//! limit of both sides applies (see [`crate::decision`]).
//!
//! The resource is a node of the resource tree of the service (protocol 5.2). The service itself
//! is `{"type": "service", "id": "<name>"}`. A node below it has `properties.ancestors`: the chain
//! from the service to the parent of the node. The first ancestor must be the service that asks,
//! and the chain must follow the parents of the last known document of this service. Otherwise,
//! the answer is `false` with the reason `invalid_resource`. A node without `ancestors` is read as
//! `[service, node]`: this is correct only when its type has the parent `service`.
//!
//! A grant covers the node that it names and each node below it (prefix match, segment by
//! segment). With `context.match = "subtree"`, a grant on a node below the resource also gives
//! `true`: a service uses this answer only to show or hide a navigation element (protocol 4.4).
//! The default is `"node"`. A grant below the service applies only when the last known document
//! declares its chain and lists its type for its action. The PDP fails closed: with no known
//! document, the decision on a node below the service is `false`.
//!
//! A `true` answer to a question about a node has `context.limits`: the ceilings of the action
//! (protocol 7.3). A question with `context.usage` is one use: the PDP takes one unit from each
//! counter of the use, or none (protocol 6.5, see [`crate::counting`]). The answer has
//! `context.usage.units` (protocol 6.12). A counter without room gives `false` with the reason
//! `budget_exceeded` and `retry_after`. A count that fails gives the HTTP status 503 and never a
//! decision (protocol 6.9).
//!
//! Endpoints:
//!
//! - `POST /access/v1/evaluation`: one decision.
//! - `POST /access/v1/evaluations`: a batch of decisions, with the three AuthZEN semantics.
//! - `POST /access/v1/search/action`: the actions that a user can do on a resource.
//! - `GET /.well-known/authzen-configuration`: the metadata. It needs no token.
//!
//! The PDP fails closed: a request that it cannot read gets status `400`, a request without a
//! valid token gets status `401`, and an unknown subject or resource gets `false`. A bad member of
//! one evaluation gets `400`. In a batch, an item with a bad `context.match` or bad ancestors gets
//! `false` with the reason `invalid_request`, and the other items get their decisions. Each
//! decision is in the log with the service, the subject, the action, the resource (in the text
//! form of a grant path) and the decision. The PDP echoes `X-Request-ID`.
//!
//! The reasons of a `false` answer: `unknown_subject` (a person or a key that does not exist, a
//! revoked, expired or too-old key, a key whose owner cannot use keys, a subject of another type,
//! an id with an unknown prefix), `invalid_resource`, `invalid_request` and `budget_exceeded`. No
//! other denial has a reason, so that an answer never shows the grants of a user.
//!
//! An `invalid_resource` can come from a new tree of the service that the PDP does not know yet.
//! The PDP then asks for an early read of the document of the service, at most once in each
//! `access.actions_refresh` interval (see [`crate::actions::Registry::read_soon`]).

use std::sync::atomic::{AtomicU64, Ordering};

use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};

use super::*;
use crate::counting::{Outcome as CountOutcome, Unavailable};
use crate::decision::{self, Counter, CounterOwner, Matching, Owner, Question, Verdict};
use crate::grants::{
    check_action, check_resource_id, check_resource_type, encode_id, Grant, Segment, ACCESS,
    MAX_DEPTH, SERVICE_TYPE,
};
use crate::keyauth::{self, Cause, OwnerSide};
use crate::limits::{Limits, Value as LimitValue};
use crate::subject_id::SubjectId;

const EVALUATION_PATH: &str = "/access/v1/evaluation";
const EVALUATIONS_PATH: &str = "/access/v1/evaluations";
const SEARCH_ACTION_PATH: &str = "/access/v1/search/action";
const METADATA_PATH: &str = "/.well-known/authzen-configuration";
const REQUEST_ID: &str = "x-request-id";
const MAX_REQUEST_ID: usize = 128;
const MAX_ID_CHARS: usize = 256;
/// The reason in the log for a `user` subject that does not exist.
const NO_SUCH_USER: &str = "no such user";
/// The reason in the answer for the same case.
const UNKNOWN_SUBJECT: &str = "unknown_subject";
/// The reason in the answer for a resource that is not a correct node of the server.
const INVALID_RESOURCE: &str = "invalid_resource";
/// The reason in a batch item for a `context.match` that is not `"node"` or `"subtree"`, or for
/// ancestors that are not an array of objects with `type` and `id`.
const INVALID_REQUEST: &str = "invalid_request";
/// The reason in the answer for a counter without room (protocol 11.1).
const BUDGET_EXCEEDED: &str = "budget_exceeded";

/// How a grant matches the resource (`context.match`, contract 11.5).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Match {
    /// The default: a grant on the resource or on a node above it.
    Node,
    /// Also a grant on a node below the resource. Only for navigation.
    Subtree,
    /// A value that is not `"node"` or `"subtree"`: the decision is `false`.
    Invalid,
}

impl Match {
    /// The match of a `context` object. A missing `context` or a missing `match` is `"node"`.
    fn of(context: Option<&Value>) -> Self {
        match context.and_then(|c| c.get("match")) {
            None => Self::Node,
            Some(Value::String(text)) if text == "node" => Self::Node,
            Some(Value::String(text)) if text == "subtree" => Self::Subtree,
            Some(_) => Self::Invalid,
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Node => "node",
            Self::Subtree => "subtree",
            Self::Invalid => "invalid",
        }
    }

    /// The match of the decision module, for a valid value.
    fn matching(self) -> Matching {
        match self {
            Self::Subtree => Matching::Subtree,
            Self::Node | Self::Invalid => Matching::Node,
        }
    }
}

/// The servers that can ask the PDP, by the SHA-256 of their token.
pub type Clients = std::collections::HashMap<[u8; 32], Client>;

/// Makes the map of the clients. Two clients with the same token stop the program at start
/// (portal 9.5a).
pub fn clients(list: Vec<Client>) -> Result<Clients, String> {
    let mut map = Clients::with_capacity(list.len());
    for client in list {
        let name = client.name.clone();
        if let Some(other) = map.insert(client.digest, client) {
            return Err(format!(
                "pdp_client {name:?} and pdp_client {:?} have the same token",
                other.name
            ));
        }
    }
    Ok(map)
}

/// The token of one server, as its SHA-256: the PDP does not keep the token itself.
pub struct Client {
    pub name: String,
    digest: [u8; 32],
}

impl Client {
    pub fn new(name: String, token: &[u8]) -> Self {
        Self {
            name,
            digest: Sha256::digest(token).into(),
        }
    }
}

/// The decisions of the PDP by result and reason, in the order of [`DECISIONS`].
const DECISIONS: [(bool, Option<&str>); 6] = [
    (true, None),
    (false, None),
    (false, Some(UNKNOWN_SUBJECT)),
    (false, Some(INVALID_RESOURCE)),
    (false, Some(INVALID_REQUEST)),
    (false, Some(BUDGET_EXCEEDED)),
];

/// Counters for `/metrics`.
#[derive(Default)]
pub struct Stats {
    decisions: [AtomicU64; 6],
    unauthorized: AtomicU64,
    bad_request: AtomicU64,
    /// The questions that got HTTP 503, by cause (portal 10.5). The wait that took more than 500
    /// ms has its own measure.
    unavailable_store: AtomicU64,
    unavailable_row: AtomicU64,
    /// The requests that got HTTP 400 for `context.usage` (portal 10.5).
    usage_rejected: AtomicU64,
}

impl Stats {
    /// Counts one decision.
    fn decided(&self, decision: bool, reason: Option<&'static str>) {
        if let Some(index) = DECISIONS
            .iter()
            .position(|(d, r)| *d == decision && *r == reason)
        {
            self.decisions[index].fetch_add(1, Ordering::Relaxed);
        }
    }

    pub fn render(&self) -> String {
        let read = |c: &AtomicU64| c.load(Ordering::Relaxed);
        let mut out = String::from(
            "# HELP portal_pdp_decisions_total Decisions of the PDP, by result and reason.\n# TYPE portal_pdp_decisions_total counter\n",
        );
        for ((decision, reason), count) in DECISIONS.iter().zip(&self.decisions) {
            out.push_str(&format!(
                "portal_pdp_decisions_total{{decision=\"{decision}\",reason=\"{}\"}} {}\n",
                reason.unwrap_or("none"),
                read(count)
            ));
        }
        out.push_str(&format!(
            "# HELP portal_pdp_refused_total PDP requests refused before a decision, by reason.\n# TYPE portal_pdp_refused_total counter\nportal_pdp_refused_total{{reason=\"unauthorized\"}} {}\nportal_pdp_refused_total{{reason=\"bad_request\"}} {}\n# HELP portal_usage_rejected_total Requests that got the HTTP status 400 for context.usage.\n# TYPE portal_usage_rejected_total counter\nportal_usage_rejected_total {}\n# HELP portal_pdp_unavailable_total Questions that got the HTTP status 503, by cause.\n# TYPE portal_pdp_unavailable_total counter\nportal_pdp_unavailable_total{{cause=\"store_error\"}} {}\nportal_pdp_unavailable_total{{cause=\"row_missing\"}} {}\n",
            read(&self.unauthorized),
            read(&self.bad_request),
            read(&self.usage_rejected),
            read(&self.unavailable_store),
            read(&self.unavailable_row),
        ));
        out
    }
}

/// The PDP listener: `listener` is bound to `pdp.listen_address`. Runs until the process ends.
pub async fn serve(state: Arc<AppState>, listener: TcpListener) {
    let Some(pdp) = state.config.pdp.clone() else {
        return;
    };
    if state.pdp_clients.is_empty() {
        log(
            &state,
            LogLevel::Warn,
            "pdp_without_client",
            &[(
                "detail",
                "no pdp_client block: the PDP refuses every request".into(),
            )],
        );
    }
    log(
        &state,
        LogLevel::Info,
        "pdp_started",
        &[
            ("listen", pdp.listen_address.to_string()),
            ("clients", state.pdp_clients.len().to_string()),
        ],
    );
    let connection_gate = Arc::new(Semaphore::new(state.config.server.max_connections));
    loop {
        let (stream, remote_addr) = match listener.accept().await {
            Ok(v) => v,
            Err(error) => {
                log(
                    &state,
                    LogLevel::Error,
                    "pdp_accept_error",
                    &[("error", error.to_string())],
                );
                tokio::time::sleep(Duration::from_millis(100)).await;
                continue;
            }
        };
        let Ok(permit) = Arc::clone(&connection_gate).try_acquire_owned() else {
            drop(stream);
            continue;
        };
        let connection_state = Arc::clone(&state);
        tokio::spawn(async move {
            let _permit = permit;
            let _live = connection_state.stop.enter();
            let _ = stream.set_nodelay(true);
            let io = TokioIo::new(stream);
            let service_state = Arc::clone(&connection_state);
            let service = service_fn(move |request| {
                let state = Arc::clone(&service_state);
                async move { Ok::<_, Infallible>(handle(&state, remote_addr, request).await) }
            });
            let mut builder = http1::Builder::new();
            builder
                .timer(TokioTimer::new())
                .header_read_timeout(connection_state.config.server.header_read_timeout)
                .max_headers(connection_state.config.server.max_headers)
                .max_buf_size(connection_state.config.server.max_header_bytes)
                .keep_alive(true);
            let _ = connection_state
                .stop
                .drive(builder.serve_connection(io, service))
                .await;
        });
    }
}

/// One request of the PDP listener.
async fn handle(
    state: &AppState,
    remote_addr: SocketAddr,
    request: Request<Incoming>,
) -> HttpResponse {
    let request_id = match unique_header_str(request.headers(), REQUEST_ID) {
        Ok(Some(id))
            if !id.is_empty()
                && id.len() <= MAX_REQUEST_ID
                && id.bytes().all(|b| b.is_ascii_graphic()) =>
        {
            Some(id.to_string())
        }
        Ok(None) => None,
        _ => {
            state.pdp_stats.bad_request.fetch_add(1, Ordering::Relaxed);
            return json_response(
                StatusCode::BAD_REQUEST,
                &json!({"error": "invalid X-Request-ID"}),
            );
        }
    };
    let method = request.method().clone();
    let path = request.uri().path().to_string();
    let response = match (method.clone(), path.as_str()) {
        (Method::GET, METADATA_PATH) => metadata(state),
        (Method::GET, HEALTH_PATH) => text_response(StatusCode::OK, "ok\n"),
        (Method::POST, EVALUATION_PATH | EVALUATIONS_PATH | SEARCH_ACTION_PATH) => {
            decide_request(state, &path, request, request_id.as_deref()).await
        }
        (_, METADATA_PATH | EVALUATION_PATH | EVALUATIONS_PATH | SEARCH_ACTION_PATH) => {
            json_response(
                StatusCode::METHOD_NOT_ALLOWED,
                &json!({"error": "method not allowed"}),
            )
        }
        _ => json_response(StatusCode::NOT_FOUND, &json!({"error": "not found"})),
    };
    let mut response = response;
    if let Some(id) = &request_id {
        let _ = set_dynamic_header(&mut response, HeaderName::from_static(REQUEST_ID), id);
    }
    log(
        state,
        LogLevel::Debug,
        "pdp_request",
        &[
            ("method", method.as_str().to_string()),
            ("path", path),
            ("status", response.status().as_u16().to_string()),
            ("remote_addr", remote_addr.to_string()),
            ("request_id", request_id.unwrap_or_default()),
        ],
    );
    response
}

/// The metadata of AuthZEN: the endpoints under `pdp.url`.
fn metadata(state: &AppState) -> HttpResponse {
    let base = state
        .config
        .pdp
        .as_ref()
        .map(|p| p.url.as_str())
        .unwrap_or("");
    json_response(
        StatusCode::OK,
        &json!({
            "policy_decision_point": base,
            "access_evaluation_endpoint": format!("{base}{EVALUATION_PATH}"),
            "access_evaluations_endpoint": format!("{base}{EVALUATIONS_PATH}"),
            "search_action_endpoint": format!("{base}{SEARCH_ACTION_PATH}"),
        }),
    )
}

/// The server that sent the request, from its bearer token.
fn client<'a>(state: &'a AppState, headers: &HeaderMap) -> Option<&'a Client> {
    let value = unique_header_str_name(headers, hyper::header::AUTHORIZATION).ok()??;
    // The scheme is case-insensitive (RFC 9110); the token is exact.
    let (scheme, token) = value.split_at_checked(7)?;
    if !scheme.eq_ignore_ascii_case("bearer ") {
        return None;
    }
    if token.is_empty() || token.len() > 1024 {
        return None;
    }
    let digest: [u8; 32] = Sha256::digest(token.as_bytes()).into();
    // One search by the digest, at any number of clients (portal 9.5a). The key is a SHA-256 of
    // the token, so the time of the search tells nothing about the token.
    state.pdp_clients.get(&digest)
}

async fn decide_request(
    state: &AppState,
    path: &str,
    request: Request<Incoming>,
    request_id: Option<&str>,
) -> HttpResponse {
    let Some(client) = client(state, request.headers()) else {
        state.pdp_stats.unauthorized.fetch_add(1, Ordering::Relaxed);
        let mut response =
            json_response(StatusCode::UNAUTHORIZED, &json!({"error": "unauthorized"}));
        response.headers_mut().insert(
            hyper::header::WWW_AUTHENTICATE,
            HeaderValue::from_static("Bearer realm=\"portal-pdp\""),
        );
        return response;
    };
    let max = state.config.pdp.as_ref().map_or(0, |p| p.max_body_bytes);
    let body = match read_limited_body(
        request.into_body(),
        max,
        state.config.server.body_read_timeout,
    )
    .await
    {
        Ok(body) => body,
        Err(BodyReadError::TooLarge) => return bad_request(state, "the body is too large"),
        Err(BodyReadError::Timeout) => return bad_request(state, "the body came too slowly"),
        Err(BodyReadError::Io) => return bad_request(state, "the body cannot be read"),
    };
    let Ok(Value::Object(request)) = serde_json::from_slice::<Value>(&body) else {
        return bad_request(state, "the body is not a JSON object");
    };
    let answer = match path {
        EVALUATION_PATH => evaluation(state, client, &request, request_id),
        EVALUATIONS_PATH => evaluations(state, client, &request, request_id),
        _ => search_action(state, client, &request, request_id),
    };
    match answer {
        Ok(value) => json_response(StatusCode::OK, &value),
        Err(PdpError::Bad(reason)) => bad_request(state, &reason),
        Err(PdpError::Usage(reason)) => {
            state
                .pdp_stats
                .usage_rejected
                .fetch_add(1, Ordering::Relaxed);
            json_response(StatusCode::BAD_REQUEST, &json!({ "error": reason }))
        }
        Err(PdpError::Unavailable(cause)) => {
            match cause {
                Unavailable::StoreError => {
                    state
                        .pdp_stats
                        .unavailable_store
                        .fetch_add(1, Ordering::Relaxed);
                }
                Unavailable::RowMissing => {
                    state
                        .pdp_stats
                        .unavailable_row
                        .fetch_add(1, Ordering::Relaxed);
                }
                // The wait that took more than 500 ms is counted by the counting module.
                Unavailable::WaitExceeded => {}
            }
            log(
                state,
                LogLevel::Warn,
                "pdp_unavailable",
                &[("cause", unavailable_name(cause).to_string())],
            );
            json_response(
                StatusCode::SERVICE_UNAVAILABLE,
                &json!({"error": "the count of the use did not work"}),
            )
        }
    }
}

/// Why the PDP answers with a status, not with a decision.
enum PdpError {
    /// A request that the PDP cannot read: HTTP 400.
    Bad(String),
    /// A request with a bad `context.usage` (protocol 6.4, 6.11): HTTP 400, counted apart.
    Usage(String),
    /// The count of a use did not work: HTTP 503 (protocol 6.9).
    Unavailable(Unavailable),
}

impl From<String> for PdpError {
    fn from(reason: String) -> Self {
        Self::Bad(reason)
    }
}

impl From<&str> for PdpError {
    fn from(reason: &str) -> Self {
        Self::Bad(reason.to_string())
    }
}

fn unavailable_name(cause: Unavailable) -> &'static str {
    match cause {
        Unavailable::StoreError => "store_error",
        Unavailable::RowMissing => "row_missing",
        Unavailable::WaitExceeded => "counter_wait_exceeded",
    }
}

fn bad_request(state: &AppState, reason: &str) -> HttpResponse {
    state.pdp_stats.bad_request.fetch_add(1, Ordering::Relaxed);
    json_response(StatusCode::BAD_REQUEST, &json!({ "error": reason }))
}

/// A subject, an action or a resource of a request, read with its required members.
struct Entity {
    kind: String,
    id: String,
}

fn entity(value: Option<&Value>, what: &str) -> Result<Entity, String> {
    let object = value
        .and_then(Value::as_object)
        .ok_or_else(|| format!("{what} must be an object"))?;
    check_properties(object, what)?;
    entity_of(object, what)
}

/// The members `type` and `id` of an object of a resource: two strings of any length. A bad type
/// or a bad id makes the resource invalid, not the request (protocol 4.1, 11.1): `chain_of`
/// checks them.
fn resource_entity_of(object: &Map<String, Value>, what: &str) -> Result<Entity, String> {
    let text = |name: &str| -> Result<String, String> {
        match object.get(name) {
            Some(Value::String(text)) => Ok(text.clone()),
            _ => Err(format!("{what}.{name} must be a string")),
        }
    };
    Ok(Entity {
        kind: text("type")?,
        id: text("id")?,
    })
}

/// The members `type` and `id` of an object.
fn entity_of(object: &Map<String, Value>, what: &str) -> Result<Entity, String> {
    let text = |name: &str| -> Result<String, String> {
        match object.get(name) {
            Some(Value::String(text))
                if !text.is_empty() && text.chars().count() <= MAX_ID_CHARS =>
            {
                Ok(text.clone())
            }
            _ => Err(format!(
                "{what}.{name} must be a string of 1 to {MAX_ID_CHARS} characters"
            )),
        }
    };
    Ok(Entity {
        kind: text("type")?,
        id: text("id")?,
    })
}

/// The resource of a request: the node and, when the request gives them, its ancestors from the
/// server to the parent of the node.
struct Resource {
    node: Entity,
    ancestors: Option<Vec<Entity>>,
    /// `properties.ancestors` is not an array of objects with `type` and `id`. Only a batch item
    /// can hold such a resource: it gets the reason `invalid_request`.
    invalid: bool,
}

/// Read the resource of a request. `properties.ancestors` is optional. When it is present, it
/// must be an array of objects with `type` and `id`. Otherwise the resource has the mark `invalid`:
/// the caller refuses a request with one evaluation (status `400`), and a batch item gets the
/// reason `invalid_request`. The chain itself is checked later, against the document of the
/// service.
fn read_resource(value: Option<&Value>, what: &str) -> Result<Resource, String> {
    let object = value
        .and_then(Value::as_object)
        .ok_or_else(|| format!("{what} must be an object"))?;
    check_properties(object, what)?;
    let node = resource_entity_of(object, what)?;
    let properties = value
        .and_then(Value::as_object)
        .and_then(|object| object.get("properties"))
        .and_then(Value::as_object);
    let mut invalid = false;
    let ancestors = match properties.and_then(|p| p.get("ancestors")) {
        None => None,
        Some(Value::Array(items)) => {
            let mut chain = Vec::with_capacity(items.len());
            for item in items {
                match item
                    .as_object()
                    .map(|object| resource_entity_of(object, what))
                {
                    Some(Ok(entity)) => chain.push(entity),
                    _ => {
                        invalid = true;
                        break;
                    }
                }
            }
            if invalid {
                None
            } else {
                Some(chain)
            }
        }
        Some(_) => {
            invalid = true;
            None
        }
    };
    Ok(Resource {
        node,
        ancestors,
        invalid,
    })
}

impl Resource {
    /// The text form of the resource for the log: `service:<id>`, then `/<type>:<id>` for each
    /// node below, with the ids encoded. The text shows what the request gave, also when it is
    /// not correct.
    fn log_text(&self) -> String {
        let mut parts: Vec<String> = Vec::new();
        if let Some(ancestors) = &self.ancestors {
            parts.extend(
                ancestors
                    .iter()
                    .map(|a| format!("{}:{}", a.kind, encode_id(&a.id))),
            );
        }
        parts.push(format!("{}:{}", self.node.kind, encode_id(&self.node.id)));
        parts.join("/")
    }
}

/// Why a resource gives no chain.
enum Unresolved {
    /// The resource is not a correct node of the server: the answer has `invalid_resource`.
    Invalid(&'static str),
    /// The PDP has no known document of the server: it cannot check the chain. The answer is
    /// `false` with no reason.
    NoDocument,
}

/// The chain of a resource below the server that asks: empty for the server itself. Checks the
/// first ancestor, the types and the ids, and the parents of the last known document.
fn chain_of(
    state: &AppState,
    client: &Client,
    resource: &Resource,
) -> Result<Vec<Segment>, Unresolved> {
    let node = &resource.node;
    if node.kind == SERVICE_TYPE {
        if resource.ancestors.as_ref().is_some_and(|a| !a.is_empty()) {
            return Err(Unresolved::Invalid("the service has ancestors"));
        }
        if node.id != client.name {
            return Err(Unresolved::Invalid(
                "the resource is a service of another server",
            ));
        }
        return Ok(Vec::new());
    }
    let below: &[Entity] = match &resource.ancestors {
        // Compatibility: a node without ancestors is a node of depth 1.
        None => &[],
        Some(ancestors) => {
            let Some((first, rest)) = ancestors.split_first() else {
                return Err(Unresolved::Invalid(
                    "the ancestors do not start with the service",
                ));
            };
            if first.kind != SERVICE_TYPE || first.id != client.name {
                return Err(Unresolved::Invalid(
                    "the first ancestor is not the service of the server that asks",
                ));
            }
            rest
        }
    };
    if below.len() + 1 > MAX_DEPTH {
        return Err(Unresolved::Invalid("the chain is too deep"));
    }
    let mut chain = Vec::with_capacity(below.len() + 1);
    for entity in below.iter().chain(std::iter::once(node)) {
        if entity.kind == SERVICE_TYPE
            || check_resource_type(&entity.kind).is_err()
            || check_resource_id(&entity.id).is_err()
        {
            return Err(Unresolved::Invalid(
                "a type or an id of the chain is not valid",
            ));
        }
        chain.push(Segment {
            kind: entity.kind.clone(),
            id: entity.id.clone(),
        });
    }
    let Some(document) = state.actions.declared(&client.name) else {
        return Err(Unresolved::NoDocument);
    };
    if !document.follows(chain.iter().map(|s| s.kind.as_str())) {
        return Err(Unresolved::Invalid(
            "the chain does not follow the types of the service",
        ));
    }
    Ok(chain)
}

fn action_name(value: Option<&Value>) -> Result<String, String> {
    let object = value
        .and_then(Value::as_object)
        .ok_or_else(|| "action must be an object".to_string())?;
    check_properties(object, "action")?;
    match object.get("name") {
        Some(Value::String(name)) if !name.is_empty() && name.len() <= MAX_ID_CHARS => {
            Ok(name.clone())
        }
        _ => Err(format!(
            "action.name must be a string of 1 to {MAX_ID_CHARS} characters"
        )),
    }
}

/// `properties` is optional and must be an object. The PDP reads only `ancestors` of a resource.
fn check_properties(object: &Map<String, Value>, what: &str) -> Result<(), String> {
    match object.get("properties") {
        None | Some(Value::Object(_)) => Ok(()),
        Some(_) => Err(format!("{what}.properties must be an object")),
    }
}

fn check_context(request: &Map<String, Value>) -> Result<(), String> {
    match request.get("context") {
        None | Some(Value::Object(_)) => Ok(()),
        Some(_) => Err("context must be an object".into()),
    }
}

/// The decision for one question and what the answer needs.
struct Outcome {
    decision: bool,
    /// The reason for the log.
    why: &'static str,
    /// The reason of the answer (protocol 11.1).
    reason: Option<&'static str>,
    /// The ceilings of `context.limits`, for a `true` answer.
    ceilings: Limits,
    /// The units that the PDP took, for a `true` answer to a question with `context.usage`.
    units: Option<u8>,
    retry_after: Option<u64>,
    /// The count of the use did not work: the request gets HTTP 503.
    unavailable: Option<Unavailable>,
}

impl Outcome {
    fn deny(why: &'static str, reason: Option<&'static str>) -> Self {
        Self {
            decision: false,
            why,
            reason,
            ceilings: Limits::new(),
            units: None,
            retry_after: None,
            unavailable: None,
        }
    }

    fn allow(why: &'static str) -> Self {
        Self {
            decision: true,
            ..Self::deny(why, None)
        }
    }

    /// The members of `context` of the answer.
    fn json(&self) -> Value {
        let mut context = Map::new();
        if self.decision {
            if !self.ceilings.is_empty() {
                let limits: Map<String, Value> = self
                    .ceilings
                    .iter()
                    .filter_map(|(name, value)| match value {
                        LimitValue::Ceiling(n) => Some((name.to_string(), json!(n))),
                        LimitValue::Budget(_) => None,
                    })
                    .collect();
                context.insert("limits".into(), Value::Object(limits));
            }
            if let Some(units) = self.units {
                context.insert("usage".into(), json!({ "units": units }));
            }
        } else {
            if let Some(reason) = self.reason {
                context.insert("reason".into(), json!(reason));
            }
            if let Some(retry_after) = self.retry_after {
                context.insert("retry_after".into(), json!(retry_after));
            }
        }
        if context.is_empty() {
            json!({ "decision": self.decision })
        } else {
            json!({ "decision": self.decision, "context": context })
        }
    }
}

/// The chain of the question, the service and the document, for the decision module.
fn question<'a>(
    client: &'a Client,
    action: &'a str,
    chain: &'a [Segment],
    matching: Match,
    document: Option<&'a crate::actions::Document>,
) -> Question<'a> {
    Question {
        service: &client.name,
        action,
        chain,
        matching: matching.matching(),
        document,
    }
}

/// The decision for one subject, action and resource, asked by `client`. `usage` is true when the
/// question has `context.usage`: the question is a use. Fails closed.
fn decide(
    state: &AppState,
    client: &Client,
    subject: &Entity,
    action: &str,
    resource: &Resource,
    matching: Match,
    usage: bool,
) -> Outcome {
    if matching == Match::Invalid {
        return Outcome::deny(
            "context.match is not node or subtree",
            Some(INVALID_REQUEST),
        );
    }
    if resource.invalid {
        return Outcome::deny("the ancestors are not valid", Some(INVALID_REQUEST));
    }
    if subject.kind != "user" {
        return Outcome::deny("the subject type is not user", Some(UNKNOWN_SUBJECT));
    }
    if check_action(action).is_err() {
        return Outcome::deny("the action name is not valid", None);
    }
    let chain = match chain_of(state, client, resource) {
        Ok(chain) => chain,
        Err(Unresolved::Invalid(why)) => return Outcome::deny(why, Some(INVALID_RESOURCE)),
        Err(Unresolved::NoDocument) => {
            return Outcome::deny("the document of the service is not known", None)
        }
    };
    let document = state.actions.declared(&client.name);
    let q = question(client, action, &chain, matching, document.as_deref());
    match SubjectId::parse(&subject.id) {
        SubjectId::User(login) => decide_person(state, login, &q, usage),
        SubjectId::Key(id) => decide_key(state, id, &q, usage),
        SubjectId::Unknown => Outcome::deny(NO_SUCH_USER, Some(UNKNOWN_SUBJECT)),
    }
}

/// The decision for a person: the superadmin, an account of the file or an account of the
/// database.
fn decide_person(state: &AppState, login: &str, q: &Question<'_>, usage: bool) -> Outcome {
    // The superadmin has every right and no limit. The PDP counts no use of the superadmin
    // (portal 9.1).
    if constant_time_eq(login.as_bytes(), state.superadmin_login.as_bytes()) {
        let mut outcome = Outcome::allow("superadmin");
        if usage {
            outcome.units = Some(0);
        }
        return outcome;
    }
    let Some((grants, from_file)) = known_person_grants(state, login, q.service) else {
        return Outcome::deny(NO_SUCH_USER, Some(UNKNOWN_SUBJECT));
    };
    let verdict = if from_file {
        decision::file_person(login, &grants, q)
    } else {
        decision::person(login, &grants, q)
    };
    finish(state, q, verdict, usage)
}

/// The grants of a person on one service and whether the account comes from the file. When the
/// directory does not know the login, the PDP reads the directory again at most once each second:
/// the account can be a few seconds old (portal 10.7c). `None`: no such person.
fn known_person_grants(
    state: &AppState,
    login: &str,
    service: &str,
) -> Option<(std::sync::Arc<[Grant]>, bool)> {
    keyauth::person_grants_on(state, login, service).or_else(|| {
        refresh_directory_if_due(state)
            .then(|| keyauth::person_grants_on(state, login, service))
            .flatten()
    })
}

/// The decision for a key (protocol 3.5).
fn decide_key(state: &AppState, id: &str, q: &Question<'_>, usage: bool) -> Outcome {
    if state.config.keys.is_none() {
        return Outcome::deny("keys are not enabled", Some(UNKNOWN_SUBJECT));
    }
    let key = state.users.key_by_id(id).or_else(|| {
        if refresh_directory_if_due(state) {
            state.users.key_by_id(id)
        } else {
            None
        }
    });
    let Some(key) = key else {
        return Outcome::deny("no such key", Some(UNKNOWN_SUBJECT));
    };
    let principal = match keyauth::resolve(state, &key, crate::tracking::unix_now()) {
        Ok(principal) => principal,
        // The copy of the directory is too old: the PDP answers `false` for each key (protocol 3.9).
        Err(Cause::DirectoryTooOld) => return Outcome::deny("the directory is too old", None),
        Err(_) => return Outcome::deny("the key is not valid", Some(UNKNOWN_SUBJECT)),
    };
    // A key never holds an action of the service `portal` (portal 6.5): the PDP ignores one.
    let key_grants: Vec<Grant> = key
        .grants
        .iter()
        .filter(|g| g.service != crate::portal_actions::SERVICE || g.action == ACCESS)
        .cloned()
        .collect();
    let owner_grants: std::sync::Arc<[Grant]>;
    let (owner, owner_counters) = match &principal.owner {
        OwnerSide::Superadmin => (Owner::Superadmin, CounterOwner::User(String::new())),
        OwnerSide::Person { login, from_file } => {
            // The owner side reads only the grants of the service of the question.
            let Some((grants, _)) = keyauth::person_grants_on(state, login, q.service) else {
                return Outcome::deny("the key is not valid", Some(UNKNOWN_SUBJECT));
            };
            owner_grants = grants;
            (
                Owner::Person(&owner_grants),
                if *from_file {
                    CounterOwner::File(login.clone())
                } else {
                    CounterOwner::User(login.clone())
                },
            )
        }
    };
    let verdict = decision::key(&key.counter_owner(), &key_grants, owner, &owner_counters, q);
    finish(state, q, verdict, usage)
}

/// The answer from a verdict: for a use, the count of the units.
fn finish(state: &AppState, q: &Question<'_>, verdict: Verdict, usage: bool) -> Outcome {
    if !verdict.allowed {
        return Outcome::deny("no grant", None);
    }
    let mut outcome = Outcome::allow("grant");
    outcome.ceilings = verdict.ceilings;
    if !usage {
        return outcome;
    }
    debug_assert!(q.matching == Matching::Node);
    match count(state, &verdict.counters) {
        CountOutcome::Counted { units } => outcome.units = Some(units),
        CountOutcome::Exceeded { retry_after } => {
            outcome = Outcome::deny("budget exceeded", Some(BUDGET_EXCEEDED));
            outcome.retry_after = Some(retry_after);
        }
        CountOutcome::Unavailable(cause) => outcome.unavailable = Some(cause),
    }
    outcome
}

/// Counts one use on a thread that may block (portal 7.7).
fn count(state: &AppState, counters: &[Counter]) -> CountOutcome {
    crate::util::blocking(|| state.counting.count(&state.store, counters))
}

/// Decide and write the decision in the log.
#[allow(clippy::too_many_arguments)]
fn decide_and_log(
    state: &AppState,
    client: &Client,
    subject: &Entity,
    action: &str,
    resource: &Resource,
    matching: Match,
    usage: bool,
    request_id: Option<&str>,
) -> Outcome {
    let outcome = decide(state, client, subject, action, resource, matching, usage);
    if outcome.reason == Some(INVALID_RESOURCE) {
        read_document_soon(state, client, request_id);
    }
    if outcome.unavailable.is_none() {
        state.pdp_stats.decided(outcome.decision, outcome.reason);
    }
    log(
        state,
        LogLevel::Info,
        "pdp_decision",
        &[
            ("client", client.name.clone()),
            ("subject", subject.id.clone()),
            ("action", action.to_string()),
            ("resource", resource.log_text()),
            ("match", matching.as_str().to_string()),
            ("usage", usage.to_string()),
            ("decision", outcome.decision.to_string()),
            ("reason", outcome.why.to_string()),
            ("request_id", request_id.unwrap_or("").to_string()),
        ],
    );
    outcome
}

/// Asks for an early read of the document of the server after an `invalid_resource`. The log has
/// one line when the read is scheduled.
fn read_document_soon(state: &AppState, client: &Client, request_id: Option<&str>) {
    let refresh = state.config.access.actions_refresh;
    if state
        .actions
        .read_soon(&client.name, refresh, std::time::Instant::now())
    {
        log(
            state,
            LogLevel::Info,
            "actions_read_early",
            &[
                ("service", client.name.clone()),
                ("request_id", request_id.unwrap_or("").to_string()),
            ],
        );
    }
}

/// `context.usage` of a question: `None` without it. The only valid value is `{"units": 1}`
/// (protocol 6.4); any other value gets HTTP 400.
fn read_usage(context: Option<&Value>) -> Result<bool, String> {
    let Some(usage) = context.and_then(|c| c.get("usage")) else {
        return Ok(false);
    };
    match usage.as_object() {
        Some(object)
            if object.len() == 1 && object.get("units").and_then(Value::as_u64) == Some(1) =>
        {
            Ok(true)
        }
        _ => Err("context.usage must be {\"units\": 1}".into()),
    }
}

/// Whether a `context` object has the member `usage`, with any value.
fn has_usage(value: Option<&Value>) -> bool {
    value
        .and_then(|c| c.get("context"))
        .and_then(Value::as_object)
        .is_some_and(|c| c.contains_key("usage"))
}

fn evaluation(
    state: &AppState,
    client: &Client,
    request: &Map<String, Value>,
    request_id: Option<&str>,
) -> Result<Value, PdpError> {
    let subject = entity(request.get("subject"), "subject")?;
    let action = action_name(request.get("action"))?;
    let resource = read_resource(request.get("resource"), "resource")?;
    check_context(request)?;
    // In a request with one evaluation, a bad member gets HTTP 400 (protocol 11.1).
    if resource.invalid {
        return Err(PdpError::Bad(
            "resource.properties.ancestors must be an array of objects with type and id".into(),
        ));
    }
    let matching = Match::of(request.get("context"));
    if matching == Match::Invalid {
        return Err(PdpError::Bad(
            "context.match must be \"node\" or \"subtree\"".into(),
        ));
    }
    let usage = read_usage(request.get("context")).map_err(PdpError::Usage)?;
    if usage && matching == Match::Subtree {
        return Err(PdpError::Usage(
            "context.usage and context.match subtree cannot be in one request".into(),
        ));
    }
    let outcome = decide_and_log(
        state, client, &subject, &action, &resource, matching, usage, request_id,
    );
    if let Some(cause) = outcome.unavailable {
        return Err(PdpError::Unavailable(cause));
    }
    Ok(outcome.json())
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Semantic {
    ExecuteAll,
    DenyOnFirstDeny,
    PermitOnFirstPermit,
}

/// The batch of AuthZEN: the members at the top are the defaults of each evaluation.
fn evaluations(
    state: &AppState,
    client: &Client,
    request: &Map<String, Value>,
    request_id: Option<&str>,
) -> Result<Value, PdpError> {
    let items = match request.get("evaluations") {
        None => return evaluation(state, client, request, request_id),
        Some(Value::Array(items)) if items.is_empty() => {
            return evaluation(state, client, request, request_id)
        }
        Some(Value::Array(items)) => items,
        Some(_) => return Err("evaluations must be an array".to_string().into()),
    };
    // A request with items takes no `context.usage` at any level (protocol 6.11).
    if has_usage(Some(&Value::Object(request.clone())))
        || items.iter().any(|item| has_usage(Some(item)))
    {
        return Err(PdpError::Usage(
            "context.usage is only for a request with one evaluation".into(),
        ));
    }
    let max = state.config.pdp.as_ref().map_or(0, |p| p.max_evaluations);
    if items.len() > max {
        return Err(format!("more than {max} evaluations").into());
    }
    let semantic = match request
        .get("options")
        .map(|o| o.as_object().ok_or("options must be an object"))
        .transpose()?
        .and_then(|o| o.get("evaluations_semantic"))
    {
        None => Semantic::ExecuteAll,
        Some(Value::String(s)) if s == "execute_all" => Semantic::ExecuteAll,
        Some(Value::String(s)) if s == "deny_on_first_deny" => Semantic::DenyOnFirstDeny,
        Some(Value::String(s)) if s == "permit_on_first_permit" => Semantic::PermitOnFirstPermit,
        Some(_) => return Err("options.evaluations_semantic is not one of execute_all, deny_on_first_deny, permit_on_first_permit".to_string().into()),
    };
    check_context(request)?;
    // Check every item before the first decision: a bad batch gets no decision at all.
    let mut parsed = Vec::with_capacity(items.len());
    for (index, item) in items.iter().enumerate() {
        let item = item
            .as_object()
            .ok_or_else(|| format!("evaluations[{index}] must be an object"))?;
        let pick = |name: &str| item.get(name).or_else(|| request.get(name));
        let subject = entity(pick("subject"), &format!("evaluations[{index}].subject"))?;
        let action =
            action_name(pick("action")).map_err(|e| format!("evaluations[{index}].{e}"))?;
        let resource = read_resource(pick("resource"), &format!("evaluations[{index}].resource"))?;
        match item.get("context") {
            None | Some(Value::Object(_)) => {}
            Some(_) => return Err(format!("evaluations[{index}].context must be an object").into()),
        }
        // The context of an item replaces the default context of the request (AuthZEN 1.0).
        let matching = Match::of(pick("context"));
        parsed.push((subject, action, resource, matching));
    }
    let mut out = Vec::with_capacity(parsed.len());
    for (subject, action, resource, matching) in &parsed {
        let outcome = decide_and_log(
            state, client, subject, action, resource, *matching, false, request_id,
        );
        let decision = outcome.decision;
        out.push(outcome.json());
        let stop = match semantic {
            Semantic::ExecuteAll => false,
            Semantic::DenyOnFirstDeny => !decision,
            Semantic::PermitOnFirstPermit => decision,
        };
        if stop {
            break;
        }
    }
    Ok(json!({ "evaluations": out }))
}

/// The actions that the subject can do on the resource: `access` and the actions that the service
/// publishes, kept when the decision is `true`. The resource has the same form as in an
/// evaluation, with its ancestors, and `context.match` has the same meaning. The answer has one
/// page. The request takes no `context.usage` (protocol 6.11).
fn search_action(
    state: &AppState,
    client: &Client,
    request: &Map<String, Value>,
    request_id: Option<&str>,
) -> Result<Value, PdpError> {
    let subject = entity(request.get("subject"), "subject")?;
    let resource = read_resource(request.get("resource"), "resource")?;
    check_context(request)?;
    if has_usage(Some(&Value::Object(request.clone()))) {
        return Err(PdpError::Usage(
            "context.usage is only for a request with one evaluation".into(),
        ));
    }
    if resource.invalid {
        return Err(PdpError::Bad(
            "resource.properties.ancestors must be an array of objects with type and id".into(),
        ));
    }
    let matching = Match::of(request.get("context"));
    if matching == Match::Invalid {
        return Err(PdpError::Bad(
            "context.match must be \"node\" or \"subtree\"".into(),
        ));
    }
    match request.get("page") {
        None | Some(Value::Object(_)) => {}
        Some(_) => return Err("page must be an object".to_string().into()),
    }
    let mut names = state.actions.action_names(&client.name);
    // Grants of actions that the document does not list now still count.
    if let SubjectId::User(login) = SubjectId::parse(&subject.id) {
        if let Some((grants, _)) = keyauth::person_grants_on(state, login, &client.name) {
            for grant in grants.iter() {
                if !names.contains(&grant.action) {
                    names.push(grant.action.clone());
                }
            }
        }
    }
    if !names.iter().any(|n| n == ACCESS) {
        names.insert(0, ACCESS.to_string());
    }
    let results: Vec<Value> = names
        .into_iter()
        .filter(|name| {
            decide_and_log(
                state, client, &subject, name, &resource, matching, false, request_id,
            )
            .decision
        })
        .map(|name| json!({ "name": name }))
        .collect();
    Ok(json!({ "results": results, "page": { "next_token": "" } }))
}

fn json_response(status: StatusCode, value: &Value) -> HttpResponse {
    let mut response = response_with_body(status, Bytes::from(value.to_string()));
    response
        .headers_mut()
        .insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    response.headers_mut().insert(
        HeaderName::from_static("cache-control"),
        HeaderValue::from_static("no-store"),
    );
    response
}

#[cfg(test)]
mod tests {
    use super::*;

    fn client(name: &str) -> Client {
        Client::new(name.to_string(), b"a token of the tests, 32 bytes long")
    }

    fn user(id: &str) -> Entity {
        Entity {
            kind: "user".into(),
            id: id.into(),
        }
    }

    #[test]
    fn two_clients_with_one_token_stop_the_start_and_the_map_finds_a_client_by_its_digest() {
        let error = match clients(vec![client("db"), client("chat")]) {
            Err(error) => error,
            Ok(_) => panic!("the same token twice must fail"),
        };
        assert!(error.contains("db") && error.contains("chat"), "{error}");
        let map = clients(vec![
            Client::new("a".into(), b"token a"),
            Client::new("b".into(), b"token b"),
        ])
        .unwrap();
        let digest: [u8; 32] = Sha256::digest(b"token b").into();
        assert_eq!(map.get(&digest).map(|c| c.name.as_str()), Some("b"));
        let other: [u8; 32] = Sha256::digest(b"token c").into();
        assert!(!map.contains_key(&other));
    }

    fn resource(kind: &str, id: &str) -> Entity {
        Entity {
            kind: kind.into(),
            id: id.into(),
        }
    }

    #[test]
    fn entities_need_their_members_and_refuse_other_shapes() {
        let read =
            |text: &str| entity(serde_json::from_str::<Value>(text).ok().as_ref(), "subject");
        assert!(read(r#"{"type":"user","id":"alice"}"#).is_ok());
        assert!(read(r#"{"type":"user","id":"alice","properties":{"x":1}}"#).is_ok());
        for bad in [
            r#"{"type":"user"}"#,
            r#"{"id":"alice"}"#,
            r#"{"type":"user","id":""}"#,
            r#"{"type":"user","id":7}"#,
            r#"{"type":"user","id":"a","properties":[]}"#,
            r#""alice""#,
        ] {
            assert!(read(bad).is_err(), "{bad}");
        }
        assert!(action_name(Some(&json!({"name": "query.run.read"}))).is_ok());
        assert!(action_name(Some(&json!({"name": ""}))).is_err());
        assert!(action_name(None).is_err());
    }

    #[test]
    fn a_client_is_kept_as_the_hash_of_its_token() {
        let c = client("db");
        assert_ne!(c.digest, [0u8; 32]);
        assert_eq!(
            c.digest,
            <[u8; 32]>::from(Sha256::digest(b"a token of the tests, 32 bytes long"))
        );
        // The unit tests of the decisions are in the cluster tests, with a whole server.
        let _ = (user("a"), resource("service", "db"));
    }
}
