//! The AuthZEN PDP (contract 11.5, OpenID AuthZEN Authorization API 1.0), on a second listener.
//!
//! The servers of the deployment ask this PDP if a user can do an action. Each server has its own
//! bearer token (a `pdp_client` block). The PDP answers only for the resources of the server that
//! asks.
//!
//! The resource is a node of the resource tree of the server (contract 11.4). The server itself
//! is `{"type": "service", "id": "<name>"}`. A node below it has `properties.ancestors`: the chain
//! from the server to the parent of the node. The first ancestor must be the server that asks,
//! and the chain must follow the parents of the last known document of this server. Otherwise,
//! the answer is `false` with the reason `invalid_resource`. A node without `ancestors` is read as
//! `[server, node]`: this is correct only when its type has the parent `service`.
//!
//! A grant covers the node that it names and each node below it (prefix match, segment by
//! segment). With `context.match = "subtree"`, a grant on a node below the resource also gives
//! `true`: a server uses this answer only to show or hide a navigation element (contract 11.5).
//! The default is `"node"`. Another value gives `false` with the reason `invalid_request`. A grant below the service applies only when the last known document declares its
//! chain and lists its type for its action. The PDP fails closed: with no known document, the
//! decision on a node below the server is `false`.
//!
//! Endpoints:
//!
//! - `POST /access/v1/evaluation`: one decision.
//! - `POST /access/v1/evaluations`: a batch of decisions, with the three AuthZEN semantics.
//! - `POST /access/v1/search/action`: the actions that a user can do on a resource.
//! - `GET /.well-known/authzen-configuration`: the metadata. It needs no token.
//!
//! The PDP fails closed: a request that it cannot read gets status `400`, a request without a
//! valid token gets status `401`, and an unknown user, subject type or resource gets `false`.
//! Each decision is in the log with the server, the subject, the action, the resource (in the
//! text form of a grant path) and the decision. The PDP echoes `X-Request-ID`.
//!
//! Three denials have a reason in the answer. A `context.match` that is not `"node"` or
//! `"subtree"` gives `invalid_request`. When the subject is a `user` that does not exist, the
//! answer is `{"decision": false, "context": {"reason": "unknown_subject"}}`. When the resource is
//! not a correct node of the server, the reason is `invalid_resource`. No other denial has a
//! reason, so that an answer never shows the grants of a user.
//!
//! An `invalid_resource` can come from a new tree of the server that the PDP does not know yet.
//! The PDP then asks for an early read of the document of the server, at most once in each
//! `access.actions_refresh` interval (see [`crate::actions::Registry::read_soon`]).

use std::sync::atomic::{AtomicU64, Ordering};

use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};

use super::*;
use crate::grants::{
    check_action, check_resource_id, check_resource_type, covers, encode_id, Grant, Segment,
    ACCESS, MAX_DEPTH, SERVICE_TYPE,
};

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
/// The reason in the answer for a `context.match` that is not `"node"` or `"subtree"`.
const INVALID_REQUEST: &str = "invalid_request";

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

/// Counters for `/metrics`.
#[derive(Default)]
pub struct Stats {
    permit: AtomicU64,
    deny: AtomicU64,
    unauthorized: AtomicU64,
    bad_request: AtomicU64,
}

impl Stats {
    pub fn render(&self) -> String {
        let read = |c: &AtomicU64| c.load(Ordering::Relaxed);
        format!(
            "# HELP portal_pdp_decisions_total Decisions of the PDP, by result.\n# TYPE portal_pdp_decisions_total counter\nportal_pdp_decisions_total{{decision=\"true\"}} {}\nportal_pdp_decisions_total{{decision=\"false\"}} {}\n# HELP portal_pdp_refused_total PDP requests refused before a decision, by reason.\n# TYPE portal_pdp_refused_total counter\nportal_pdp_refused_total{{reason=\"unauthorized\"}} {}\nportal_pdp_refused_total{{reason=\"bad_request\"}} {}\n",
            read(&self.permit),
            read(&self.deny),
            read(&self.unauthorized),
            read(&self.bad_request),
        )
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
            let _ = builder.serve_connection(io, service).await;
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
    // Compare with every client, so the time does not tell which one is near.
    let mut found = None;
    for client in &state.pdp_clients {
        if constant_time_eq(&client.digest, &digest) {
            found = Some(client);
        }
    }
    found
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
        Err(reason) => bad_request(state, &reason),
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
}

/// Read the resource of a request. `properties.ancestors` is optional. When it is present, it
/// must be an array of objects with `type` and `id`: else the request is bad (status `400`). The
/// chain itself is checked later, against the document of the server.
fn read_resource(value: Option<&Value>, what: &str) -> Result<Resource, String> {
    let node = entity(value, what)?;
    let properties = value
        .and_then(Value::as_object)
        .and_then(|object| object.get("properties"))
        .and_then(Value::as_object);
    let ancestors = match properties.and_then(|p| p.get("ancestors")) {
        None => None,
        Some(Value::Array(items)) => Some(
            items
                .iter()
                .enumerate()
                .map(|(index, item)| {
                    let at = format!("{what}.properties.ancestors[{index}]");
                    let object = item
                        .as_object()
                        .ok_or_else(|| format!("{at} must be an object"))?;
                    entity_of(object, &at)
                })
                .collect::<Result<Vec<_>, _>>()?,
        ),
        Some(_) => return Err(format!("{what}.properties.ancestors must be an array")),
    };
    Ok(Resource { node, ancestors })
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

/// Whether one of `grants` gives `action` on a node below the node `chain` of the service: its
/// path is longer than the chain and starts with it.
fn extends(grants: &[Grant], action: &str, service: &str, chain: &[Segment]) -> bool {
    grants.iter().any(|grant| {
        grant.action == action
            && grant.service == service
            && grant.path.len() > chain.len()
            && grant.path.starts_with(chain)
    })
}

/// The decision for one subject, action and resource, asked by `client`, with the reason for
/// the log and the reason for the answer. Fails closed.
fn decide(
    state: &AppState,
    client: &Client,
    subject: &Entity,
    action: &str,
    resource: &Resource,
    matching: Match,
) -> (bool, &'static str, Option<&'static str>) {
    if matching == Match::Invalid {
        return (
            false,
            "context.match is not node or subtree",
            Some(INVALID_REQUEST),
        );
    }
    if subject.kind != "user" {
        return (false, "the subject type is not user", None);
    }
    if check_action(action).is_err() {
        return (false, "the action name is not valid", None);
    }
    let chain = match chain_of(state, client, resource) {
        Ok(chain) => chain,
        Err(Unresolved::Invalid(why)) => return (false, why, Some(INVALID_RESOURCE)),
        Err(Unresolved::NoDocument) => {
            return (false, "the document of the service is not known", None)
        }
    };
    if constant_time_eq(subject.id.as_bytes(), state.superadmin_login.as_bytes()) {
        return (true, "superadmin", None);
    }
    let Some(stored) = stored_grants_of(state, &subject.id) else {
        return (false, NO_SUCH_USER, Some(UNKNOWN_SUBJECT));
    };
    let grants = state.policy.effective(&subject.id, &stored);
    // A grant below the service applies only when the last known document of the service
    // declares its chain and lists its type for its action. Without a known document, it does
    // not apply. A grant on the whole service needs no declaration.
    let document = state.actions.declared(&client.name);
    let applicable: Vec<Grant> = grants
        .iter()
        .filter(|grant| {
            grant.path.is_empty()
                || document
                    .as_ref()
                    .is_some_and(|d| d.check_grant(grant).is_ok())
        })
        .cloned()
        .collect();
    if covers(&applicable, action, &client.name, &chain) {
        (true, "grant", None)
    } else if matching == Match::Subtree && extends(&applicable, action, &client.name, &chain) {
        (true, "grant below the resource", None)
    } else if covers(&grants, action, &client.name, &chain)
        || (matching == Match::Subtree && extends(&grants, action, &client.name, &chain))
    {
        (
            false,
            "the grant does not follow the document of the service",
            None,
        )
    } else {
        (false, "no grant", None)
    }
}

/// The answer of one decision, with its reason when it has one.
struct Answer {
    decision: bool,
    reason: Option<&'static str>,
}

impl Answer {
    /// The AuthZEN member of the answer. Only an unknown subject and an invalid resource have a
    /// reason.
    fn json(&self) -> Value {
        match self.reason {
            Some(reason) => json!({ "decision": self.decision, "context": { "reason": reason } }),
            None => json!({ "decision": self.decision }),
        }
    }
}

/// Decide and write the decision in the log (contract 11.7).
fn decide_and_log(
    state: &AppState,
    client: &Client,
    subject: &Entity,
    action: &str,
    resource: &Resource,
    matching: Match,
    request_id: Option<&str>,
) -> Answer {
    let (decision, why, reason) = decide(state, client, subject, action, resource, matching);
    if reason == Some(INVALID_RESOURCE) {
        read_document_soon(state, client, request_id);
    }
    if decision {
        state.pdp_stats.permit.fetch_add(1, Ordering::Relaxed);
    } else {
        state.pdp_stats.deny.fetch_add(1, Ordering::Relaxed);
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
            ("decision", decision.to_string()),
            ("reason", why.to_string()),
            ("request_id", request_id.unwrap_or("").to_string()),
        ],
    );
    Answer { decision, reason }
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

fn evaluation(
    state: &AppState,
    client: &Client,
    request: &Map<String, Value>,
    request_id: Option<&str>,
) -> Result<Value, String> {
    let subject = entity(request.get("subject"), "subject")?;
    let action = action_name(request.get("action"))?;
    let resource = read_resource(request.get("resource"), "resource")?;
    check_context(request)?;
    let matching = Match::of(request.get("context"));
    Ok(decide_and_log(
        state, client, &subject, &action, &resource, matching, request_id,
    )
    .json())
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
) -> Result<Value, String> {
    let items = match request.get("evaluations") {
        None => return evaluation(state, client, request, request_id),
        Some(Value::Array(items)) if items.is_empty() => {
            return evaluation(state, client, request, request_id)
        }
        Some(Value::Array(items)) => items,
        Some(_) => return Err("evaluations must be an array".into()),
    };
    let max = state.config.pdp.as_ref().map_or(0, |p| p.max_evaluations);
    if items.len() > max {
        return Err(format!("more than {max} evaluations"));
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
        Some(_) => return Err("options.evaluations_semantic is not one of execute_all, deny_on_first_deny, permit_on_first_permit".into()),
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
            Some(_) => return Err(format!("evaluations[{index}].context must be an object")),
        }
        // The context of an item replaces the default context of the request (AuthZEN 1.0).
        let matching = Match::of(pick("context"));
        parsed.push((subject, action, resource, matching));
    }
    let mut out = Vec::with_capacity(parsed.len());
    for (subject, action, resource, matching) in &parsed {
        let answer = decide_and_log(
            state, client, subject, action, resource, *matching, request_id,
        );
        let decision = answer.decision;
        out.push(answer.json());
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

/// The actions that the subject can do on the resource: `access` and the actions that the server
/// publishes, kept when the decision is `true`. The resource has the same form as in an
/// evaluation, with its ancestors, and `context.match` has the same meaning. The answer has one
/// page.
fn search_action(
    state: &AppState,
    client: &Client,
    request: &Map<String, Value>,
    request_id: Option<&str>,
) -> Result<Value, String> {
    let subject = entity(request.get("subject"), "subject")?;
    let resource = read_resource(request.get("resource"), "resource")?;
    check_context(request)?;
    let matching = Match::of(request.get("context"));
    match request.get("page") {
        None | Some(Value::Object(_)) => {}
        Some(_) => return Err("page must be an object".into()),
    }
    let mut names = state.actions.action_names(&client.name);
    // Grants of actions that the document does not list now still count.
    if let Some(stored) = stored_grants_of(state, &subject.id) {
        for grant in state.policy.effective(&subject.id, &stored) {
            if grant.service == client.name && !names.contains(&grant.action) {
                names.push(grant.action);
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
                state, client, &subject, name, &resource, matching, request_id,
            )
            .decision
        })
        .map(|name| json!({ "name": name }))
        .collect();
    Ok(json!({ "results": results, "page": { "next_token": "" } }))
}

/// The stored grants of an account, read again from a shared database when the directory does not
/// know it yet. `None`: no such account.
fn stored_grants_of(state: &AppState, login: &str) -> Option<Vec<crate::grants::Grant>> {
    state.users.grants_of(login).or_else(|| {
        refresh_directory_if_due(state)
            .then(|| state.users.grants_of(login))
            .flatten()
    })
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
