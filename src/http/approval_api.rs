//! The approval API for a service (a PEP), on the PDP listener (protocol 17.4).
//!
//! | Request | Answer |
//! | --- | --- |
//! | `POST /approval/v1/requests` | `201 {request_id, code, verdict}`, or `200` with the same members for a request that still waits |
//! | `GET /approval/v1/requests/{id}?subject=&wait=&renew=` | `200 {verdict}` |
//! | `POST /approval/v1/requests/{id}/keepalive` with `{"subject": ...}` | `200 {verdict}` |
//!
//! The routes use the bearer token of any `pdp_client` block. The block `inbox.approvals` must exist. The API is narrow on purpose:
//! an answer holds `request_id`, `code` and `verdict`, and nothing else. The service never gets the explanation of
//! the agent, a comment of the owner, the owner, a device, or a time of an answer (protocol 17.12).
//!
//! # The status codes
//!
//! The reference PEP maps some codes to errors of its own tool and every other failure to its own HTTP 503. So the
//! portal keeps the codes below for the causes that the PEP maps, and it uses a code of 17.4 for a mistake of the
//! service.
//!
//! | Status | `error` | Cause | The PEP gives |
//! | --- | --- | --- | --- |
//! | 401, 403 | | no valid token; the file has no block `inbox.approvals` | its own 503 |
//! | 404 | `not_found` | an unknown request, of another service, or of another subject | `invalid_argument` |
//! | 409 | `no_device` | the owner has no phone and `web_approval` is false | `approval_unavailable` |
//! | 409 | `cannot_supersede` | the request to replace is consumed | its own 503 |
//! | 413 | `command_too_large` | `op` and `view` are above `max_command_bytes` | `invalid_argument` |
//! | 422 | `invalid_argument` | a bad member, a bad `approval_ref`, an unknown request to replace | `invalid_argument` |
//! | 422 | `invalid_lease`, `lease_required` | the wait is missing, double, or out of range | `invalid_argument` |
//! | 422 | `effect_mismatch`, `invalid_view`, `invalid_op`, `invalid_evaluation`, `explanation_required` | a mistake of the service | its own 503 |
//! | 429 | `rate_limited` and `retry_after` | a limit of requests, a mute, a deny time | `approval_rate_limited` |
//! | 503 | `storage_degraded`, `unavailable` | the inbox cannot write | its own 503 |

use hyper::body::Incoming;
use hyper::{Method, Request, StatusCode};
use inbox_core::effect::{is_effect, Op};
use serde::Deserialize;
use serde_json::{json, Value};

use super::pdp::{self, json_response, open_evaluation_id, unauthorized, Client};
use super::*;
use crate::inbox::approvals::{self, ApprovalError, Control, Requester, Submission, Verdict};
use crate::keyauth;
use crate::subject_id::SubjectId;

/// The path of the routes.
pub(super) const PREFIX: &str = "/approval/v1/requests";

/// True for a path that this module serves.
pub(super) fn owns(path: &str) -> bool {
    path == PREFIX
        || path
            .strip_prefix(PREFIX)
            .is_some_and(|rest| rest.starts_with('/'))
}

fn failure(error: &ApprovalError) -> HttpResponse {
    let status = StatusCode::from_u16(error.status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
    let mut response = json_response(status, &error.to_value());
    if let Some(seconds) = error.retry_after {
        let _ = set_dynamic_header(
            &mut response,
            HeaderName::from_static("retry-after"),
            &seconds.to_string(),
        );
    }
    response
}

fn code(status: StatusCode, code: &str) -> HttpResponse {
    json_response(status, &json!({ "error": code }))
}

fn verdict_response(verdict: Verdict) -> HttpResponse {
    json_response(StatusCode::OK, &json!({ "verdict": verdict.as_str() }))
}

/// One request of the approval API.
pub(super) async fn handle(
    state: &AppState,
    client_hint: Option<&Client>,
    request: Request<Incoming>,
    path: &str,
) -> HttpResponse {
    let Some(client) = client_hint else {
        return unauthorized(state);
    };
    if !pdp::approvals_configured(state) {
        return code(StatusCode::FORBIDDEN, "forbidden");
    }
    let Some(app) = state.inbox.app() else {
        return failure(&ApprovalError::new(503, "unavailable"));
    };
    let rest = path.strip_prefix(PREFIX).unwrap_or("");
    let segments: Vec<&str> = match rest {
        "" => Vec::new(),
        _ => match rest.strip_prefix('/') {
            Some(tail) => tail.split('/').collect(),
            None => return code(StatusCode::NOT_FOUND, "not_found"),
        },
    };
    let allowed = match segments.as_slice() {
        [] => Method::POST,
        [id] if !id.is_empty() => Method::GET,
        [id, "keepalive"] if !id.is_empty() => Method::POST,
        _ => return code(StatusCode::NOT_FOUND, "not_found"),
    };
    if request.method() != allowed {
        let mut response = code(StatusCode::METHOD_NOT_ALLOWED, "method_not_allowed");
        let _ = set_dynamic_header(
            &mut response,
            HeaderName::from_static("allow"),
            allowed.as_str(),
        );
        return response;
    }
    match segments.as_slice() {
        [] => submit(state, &app, client, request).await,
        [id] => read(&app, client, request.uri().query().unwrap_or(""), id).await,
        [id, _] => keepalive(state, &app, client, request, id).await,
        _ => code(StatusCode::NOT_FOUND, "not_found"),
    }
}

fn is_json(request: &Request<Incoming>) -> bool {
    request
        .headers()
        .get(hyper::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.split(';').next())
        .is_some_and(|t| t.trim().eq_ignore_ascii_case("application/json"))
}

/// What the body of a submission holds. A member that is not in the list stops the request.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Body {
    evaluation_id: String,
    effect: String,
    op: Op,
    view: Value,
    approval_ref: Option<String>,
    lease_s: Option<Value>,
    until: Option<Value>,
    supersedes: Option<String>,
}

/// A submission after the shape checks.
struct Parsed {
    evaluation_id: String,
    effect: String,
    op: Op,
    view: Value,
    control: Control,
    supersedes: Option<String>,
}

fn text(value: String, max: usize) -> Result<String, &'static str> {
    if value.is_empty() || value.len() > max {
        Err("invalid_argument")
    } else {
        Ok(value)
    }
}

/// Reads the body of a submission. The error is the code of 17.4.
fn read_submission(body: &[u8]) -> Result<Parsed, &'static str> {
    let body: Body = match serde_json::from_slice(body) {
        Ok(body) => body,
        Err(e) => {
            // The strict reader of `op` writes these texts. A member of the envelope writes none of them.
            let reason = e.to_string();
            return Err(
                if [
                    "float",
                    "range",
                    "duplicate at",
                    "depth",
                    "op is not an object",
                ]
                .iter()
                .any(|p| reason.starts_with(p))
                    || reason.contains("surrogate")
                    || reason.contains("hex escape")
                {
                    "invalid_op"
                } else {
                    "invalid_argument"
                },
            );
        }
    };
    let evaluation_id = text(body.evaluation_id, 1024)?;
    if !is_effect(&body.effect) {
        return Err("invalid_argument");
    }
    let supersedes = body.supersedes.map(|s| text(s, 1024)).transpose()?;
    let reference = body.approval_ref.map(|s| text(s, 1024)).transpose()?;
    let given = usize::from(reference.is_some())
        + usize::from(body.lease_s.is_some())
        + usize::from(body.until.is_some());
    // A value that is not a whole number of seconds, or not a time, makes the wait invalid (17.5).
    let lease_s = match &body.lease_s {
        None => None,
        Some(v) => Some(v.as_i64().ok_or("invalid_lease")?),
    };
    let until = match &body.until {
        None => None,
        Some(Value::String(s)) => Some(approvals::parse_time(s).ok_or("invalid_lease")?),
        Some(_) => return Err("invalid_lease"),
    };
    let control = match (given, reference, lease_s, until) {
        (0, ..) => return Err("lease_required"),
        (1, Some(r), ..) => Control::Reference(r),
        (1, None, Some(n), _) => Control::LeaseSeconds(n),
        (1, None, None, Some(ms)) => Control::UntilMs(ms),
        _ => return Err("invalid_lease"),
    };
    Ok(Parsed {
        evaluation_id,
        effect: body.effect,
        op: body.op,
        view: body.view,
        control,
        supersedes,
    })
}

/// The key that the subject of a question names, with its owner. `None` for a person, an unknown key, a revoked key
/// or a key that cannot be used now: the evaluation is then not valid for a submission.
fn requester(state: &AppState, subject: &str) -> Option<Requester> {
    let SubjectId::Key(id) = SubjectId::parse(subject) else {
        return None;
    };
    let key = state.users.key_by_id(id)?;
    let principal = keyauth::resolve(state, &key, crate::tracking::unix_now()).ok()?;
    Some(Requester {
        key_id: key.id.clone(),
        owner: principal.owner_login(&state.superadmin_login).to_string(),
        interaction: match key.interaction {
            crate::keys::Interaction::Conversational => {
                inbox_core::proto::Interaction::Conversational
            }
            crate::keys::Interaction::OneShot => inbox_core::proto::Interaction::OneShot,
        },
        // A service never names an agent: a request with no intent belongs to the bucket of the key.
        agent: Default::default(),
    })
}

async fn submit(
    state: &AppState,
    app: &crate::inbox::App,
    client: &Client,
    request: Request<Incoming>,
) -> HttpResponse {
    if !is_json(&request) {
        return code(StatusCode::UNSUPPORTED_MEDIA_TYPE, "unsupported_media_type");
    }
    let Some(config) = app.config.approvals.as_ref() else {
        return failure(&ApprovalError::new(503, "unavailable"));
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
        Err(BodyReadError::TooLarge) => {
            return failure(&ApprovalError::new(413, "command_too_large"))
        }
        Err(_) => return code(StatusCode::BAD_REQUEST, "invalid_argument"),
    };
    let parsed = match read_submission(&body) {
        Ok(parsed) => parsed,
        Err(error) => return code(StatusCode::UNPROCESSABLE_ENTITY, error),
    };
    if approvals::command_bytes(parsed.op.as_value(), &parsed.view) > config.max_command_bytes {
        return failure(&ApprovalError::new(413, "command_too_large"));
    }
    let invalid_evaluation = || code(StatusCode::UNPROCESSABLE_ENTITY, "invalid_evaluation");
    let Some(content) = app.secrets.content.as_ref() else {
        return failure(&ApprovalError::new(503, "unavailable"));
    };
    let facts = match open_evaluation_id(
        content,
        crate::inbox::Clock::now_ms(&*app.clock),
        config.evaluation_ttl.as_secs(),
        &parsed.evaluation_id,
        &client.name,
        &parsed.effect,
    ) {
        Ok(facts) => facts,
        Err(_) => return invalid_evaluation(),
    };
    // The action and the resource come from the sealed `evaluation_id`, never from the body: the portal sealed them
    // when the PDP answered `approval_required` for this service, subject, action, resource and effect.
    let Some(who) = requester(state, &facts.subject) else {
        return invalid_evaluation();
    };
    let submission = Submission {
        service: client.name.clone(),
        action: facts.action,
        resource: facts.resource,
        evaluation_id: parsed.evaluation_id,
        effect: parsed.effect,
        op: parsed.op,
        view: parsed.view,
        control: parsed.control,
        supersedes: parsed.supersedes,
    };
    match approvals::submit(app, &who, submission).await {
        Ok(done) => json_response(
            if done.created {
                StatusCode::CREATED
            } else {
                StatusCode::OK
            },
            &json!({
                "request_id": done.request_id,
                "code": done.code,
                "verdict": Verdict::Pending.as_str()
            }),
        ),
        Err(error) => failure(&error),
    }
}

/// The parameters of a query, decoded. The first value of a name counts. `None` for a bad escape.
fn query_params(query: &str) -> Option<Vec<(String, String)>> {
    let mut out: Vec<(String, String)> = Vec::new();
    for pair in query.split('&').filter(|p| !p.is_empty()) {
        let (name, value) = pair.split_once('=').unwrap_or((pair, ""));
        let name = crate::util::percent_decode_form(name).ok()?;
        let value = crate::util::percent_decode_form(value).ok()?;
        if !out.iter().any(|(n, _)| *n == name) {
            out.push((name, value));
        }
    }
    Some(out)
}

async fn read(app: &crate::inbox::App, client: &Client, query: &str, id: &str) -> HttpResponse {
    let invalid = || code(StatusCode::UNPROCESSABLE_ENTITY, "invalid_argument");
    let Ok(id) = crate::util::percent_decode_form(id) else {
        return code(StatusCode::NOT_FOUND, "not_found");
    };
    let Some(params) = query_params(query) else {
        return invalid();
    };
    let param = |name: &str| {
        params
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, v)| v.as_str())
    };
    let Some(subject) = param("subject").filter(|s| !s.is_empty()) else {
        return invalid();
    };
    let renew = match param("renew") {
        None | Some("false") => false,
        Some("true") => true,
        Some(_) => return invalid(),
    };
    // The wait is bound by `inbox.items.wait_max`, and it is never an error.
    let wait = match param("wait") {
        None => 0,
        Some(t) if !t.is_empty() && t.bytes().all(|b| b.is_ascii_digit()) => {
            t.parse::<u64>().unwrap_or(u64::MAX)
        }
        Some(_) => return invalid(),
    };
    match approvals::wait_verdict(app, &client.name, subject, &id, renew, wait).await {
        Ok(verdict) => verdict_response(verdict),
        Err(error) => failure(&error),
    }
}

async fn keepalive(
    state: &AppState,
    app: &crate::inbox::App,
    client: &Client,
    request: Request<Incoming>,
    id: &str,
) -> HttpResponse {
    if !is_json(&request) {
        return code(StatusCode::UNSUPPORTED_MEDIA_TYPE, "unsupported_media_type");
    }
    let Ok(id) = crate::util::percent_decode_form(id) else {
        return code(StatusCode::NOT_FOUND, "not_found");
    };
    let max = state.config.pdp.as_ref().map_or(0, |p| p.max_body_bytes);
    let Ok(body) = read_limited_body(
        request.into_body(),
        max,
        state.config.server.body_read_timeout,
    )
    .await
    else {
        return code(StatusCode::BAD_REQUEST, "invalid_argument");
    };
    // The body has the member `subject` and no other.
    let subject = match serde_json::from_slice::<Value>(&body) {
        Ok(Value::Object(map)) if map.len() == 1 => match map.get("subject") {
            Some(Value::String(s)) if !s.is_empty() => Some(s.clone()),
            _ => None,
        },
        _ => None,
    };
    let Some(subject) = subject else {
        return code(StatusCode::UNPROCESSABLE_ENTITY, "invalid_argument");
    };
    match approvals::verdict(app, &client.name, &subject, &id, true).await {
        Ok(verdict) => verdict_response(verdict),
        Err(error) => failure(&error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const EFFECT: &str = "sha256:0000000000000000000000000000000000000000000000000000000000000000";

    fn body(extra: &str) -> Vec<u8> {
        format!(r#"{{"evaluation_id":"ev","effect":"{EFFECT}","op":{{"a":1}},"view":{{}}{extra}}}"#)
            .into_bytes()
    }

    #[test]
    fn the_body_of_a_submission_is_read_with_the_codes_of_17_4() {
        let control = |extra: &str| read_submission(&body(extra)).map(|p| p.control);
        assert_eq!(control(r#","lease_s":60"#), Ok(Control::LeaseSeconds(60)));
        assert_eq!(
            control(r#","until":"2030-01-01T00:00:00Z""#),
            Ok(Control::UntilMs(1_893_456_000_000))
        );
        assert_eq!(
            control(r#","approval_ref":"r1""#),
            Ok(Control::Reference("r1".into()))
        );
        assert_eq!(control(""), Err("lease_required"));
        assert_eq!(
            control(r#","lease_s":60,"until":"2030-01-01T00:00:00Z""#),
            Err("invalid_lease")
        );
        assert_eq!(
            control(r#","lease_s":60,"approval_ref":"r""#),
            Err("invalid_lease")
        );
        assert_eq!(control(r#","lease_s":1.5"#), Err("invalid_lease"));
        assert_eq!(control(r#","lease_s":"60""#), Err("invalid_lease"));
        assert_eq!(control(r#","until":"tomorrow""#), Err("invalid_lease"));
        assert_eq!(control(r#","until":5"#), Err("invalid_lease"));
        assert_eq!(
            control(r#","extra":1,"lease_s":60"#),
            Err("invalid_argument")
        );
        assert_eq!(control(r#","approval_ref":5"#), Err("invalid_argument"));
        assert_eq!(control(r#","approval_ref":"""#), Err("invalid_argument"));
        assert_eq!(
            control(r#","supersedes":7,"lease_s":60"#),
            Err("invalid_argument")
        );
    }

    #[test]
    fn a_bad_op_is_invalid_op_and_a_bad_envelope_is_invalid_argument() {
        let with_op = |op: &str| {
            format!(
                r#"{{"evaluation_id":"e","effect":"{EFFECT}","op":{op},"view":{{}},"lease_s":60}}"#
            )
        };
        for op in [
            r#"{"a":1,"a":2}"#,
            r#"{"a":1.5}"#,
            r#"{"a":9007199254740992}"#,
            r#"{"a":1e2}"#,
            r#"[1]"#,
            r#"5"#,
            r#"{"a":"\ud800"}"#,
        ] {
            assert_eq!(
                read_submission(with_op(op).as_bytes()).map(|_| ()),
                Err("invalid_op"),
                "{op}"
            );
        }
        for bad in [
            "[]".to_string(),
            "{".to_string(),
            r#"{"evaluation_id":"e","effect":"x","op":{},"view":{},"lease_s":60}"#.to_string(),
            r#"{"effect":"x","op":{},"view":{},"lease_s":60}"#.to_string(),
            r#"{"evaluation_id":"e","evaluation_id":"f","effect":"sha256:00","op":{},"view":{},"lease_s":60}"#.to_string(),
        ] {
            assert_eq!(
                read_submission(bad.as_bytes()).map(|_| ()),
                Err("invalid_argument"),
                "{bad}"
            );
        }
        assert!(read_submission(with_op(r#"{"a":[1,"x",null,true,{"b":-3}]}"#).as_bytes()).is_ok());
    }

    #[test]
    fn a_query_is_decoded_and_the_first_value_of_a_name_counts() {
        let params = query_params("subject=key%3Aabc&wait=5&subject=other&renew=true").unwrap();
        assert_eq!(
            params,
            vec![
                ("subject".to_owned(), "key:abc".to_owned()),
                ("wait".to_owned(), "5".to_owned()),
                ("renew".to_owned(), "true".to_owned()),
            ]
        );
        assert!(query_params("subject=%zz").is_none());
        assert_eq!(query_params("").unwrap(), Vec::new());
    }

    #[test]
    fn the_routes_of_the_api_are_the_three_of_17_4() {
        assert!(owns("/approval/v1/requests"));
        assert!(owns("/approval/v1/requests/abc"));
        assert!(owns("/approval/v1/requests/abc/keepalive"));
        assert!(!owns("/approval/v1/requestsx"));
        assert!(!owns("/approval/v1"));
        assert!(!owns("/access/v1/evaluation"));
    }
}
