//! The API of the inbox in the portal, under `/auth/inbox/api/`.
//!
//! The admin calls need the session of the superadmin. A request without a session gets `401`,
//! and any other account gets `403`. Two calls are for any signed-in account: `GET csrf`, and
//! `POST enrollment-codes`, which makes a code for the login of the session only. A POST follows
//! the rules of the other forms of the portal: the same origin, a JSON body, and an anti-CSRF
//! token in the header `X-CSRF-Token`. The call `GET /auth/inbox/api/csrf` gives the token of the
//! session.
//!
//! This is a temporary place. A later version gives these calls their own rights and pages.

use super::*;
use crate::inbox::admin as inbox_admin;
use crate::inbox::admin::ListQuery;
use crate::inbox::ApiError;
use crate::inbox::App as InboxApp;

pub const PREFIX: &str = "/auth/inbox/api/";
/// The purpose of the anti-CSRF token of the admin API of the inbox.
const CSRF_PURPOSE: &str = "inbox";

fn answer(result: Result<HttpResponse, ApiError>) -> HttpResponse {
    let mut response = result.unwrap_or_else(ApiError::into_response);
    apply_secure_headers(&mut response, true);
    response
}

/// Handles one request under the prefix. The first checks are the session and the POST rules.
pub async fn handle(
    state: &Arc<AppState>,
    request: Request<Incoming>,
    client: &Client,
) -> HttpResponse {
    let Some(app) = state.inbox.app() else {
        return answer(Err(ApiError::new(
            StatusCode::NOT_FOUND,
            "not_found",
            "the inbox is not configured",
        )));
    };
    answer(run(state, &app, request, client).await)
}

async fn run(
    state: &Arc<AppState>,
    app: &Arc<InboxApp>,
    request: Request<Incoming>,
    client: &Client,
) -> Result<HttpResponse, ApiError> {
    let Some(viewer) = current_viewer(state, request.headers(), client) else {
        return Err(ApiError::new(
            StatusCode::UNAUTHORIZED,
            "unauthorized",
            "sign in",
        ));
    };
    let method = request.method().clone();
    let path = request.uri().path().to_string();
    let rest = path.strip_prefix(PREFIX).unwrap_or_default().to_string();
    let query = request.uri().query().map(str::to_string);
    let segments: Vec<&str> = rest.split('/').collect();
    // These two calls are for any account. The others are for the superadmin.
    let own = matches!(segments.as_slice(), ["csrf"] | ["enrollment-codes"]);
    if !own && !is_superadmin(&viewer) {
        return Err(ApiError::new(
            StatusCode::FORBIDDEN,
            "forbidden",
            "only the superadmin can use this call",
        ));
    }
    let known_get = matches!(
        segments.as_slice(),
        ["csrf"] | ["overview"] | ["alerts"] | ["alerts", _] | ["connections"]
    );
    let known_post = matches!(
        segments.as_slice(),
        ["alerts"] | ["devices", _, "revoke"] | ["enrollment-codes"]
    );
    if method == Method::GET && known_get {
        return match segments.as_slice() {
            ["csrf"] => {
                let token = sign_action_token(&state.signing_secret, CSRF_PURPOSE, &viewer.token)
                    .map_err(|_| {
                    ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, "internal", "token error")
                })?;
                Ok(inbox_json(&serde_json::json!({ "csrf": token })))
            }
            ["overview"] => inbox_admin::overview(app).await,
            ["alerts"] => inbox_admin::alerts(app, ListQuery::parse(query.as_deref())?).await,
            ["alerts", id] => inbox_admin::alert(app, id).await,
            _ => inbox_admin::connections(app, ListQuery::parse(query.as_deref())?).await,
        };
    }
    if method == Method::POST && known_post {
        check_post(state, &viewer, request.headers())?;
        let body = read_limited_body(
            request.into_body(),
            state.config.server.max_body_bytes,
            state.config.server.body_read_timeout,
        )
        .await
        .map_err(|e| match e {
            BodyReadError::TooLarge => ApiError::new(
                StatusCode::PAYLOAD_TOO_LARGE,
                "too_large",
                "the body is too large",
            ),
            BodyReadError::Timeout => ApiError::new(
                StatusCode::REQUEST_TIMEOUT,
                "body_timeout",
                "the body did not arrive in time",
            ),
            BodyReadError::Io => ApiError::new(
                StatusCode::BAD_REQUEST,
                "invalid_body",
                "the body could not be read",
            ),
        })?;
        return match segments.as_slice() {
            ["enrollment-codes"] => {
                // The body is an empty object. The code is for the login of the session: there is
                // no parameter that names another account.
                let empty_object = matches!(
                    serde_json::from_slice::<serde_json::Value>(&body),
                    Ok(serde_json::Value::Object(ref fields)) if fields.is_empty()
                );
                if !empty_object {
                    return Err(ApiError::new(
                        StatusCode::BAD_REQUEST,
                        "invalid_json",
                        "the body must be {}",
                    ));
                }
                log(
                    state,
                    LogLevel::Info,
                    "inbox_enrollment_code",
                    &[("actor", viewer.login.clone())],
                );
                let made = state.inbox.create_enrollment_code(&viewer.login).await;
                match made {
                    Some(Ok(code)) => Ok(crate::inbox::json_response(
                        StatusCode::CREATED,
                        &serde_json::to_value(&code).unwrap_or(serde_json::Value::Null),
                    )),
                    Some(Err(e)) => Err(e),
                    None => Err(ApiError::new(
                        StatusCode::NOT_FOUND,
                        "not_found",
                        "the inbox is not configured",
                    )),
                }
            }
            ["alerts"] => {
                log(
                    state,
                    LogLevel::Info,
                    "inbox_admin_send",
                    &[("actor", viewer.login.clone())],
                );
                inbox_admin::send_alert(app, &body).await
            }
            ["devices", id, _] => {
                log(
                    state,
                    LogLevel::Info,
                    "inbox_admin_revoke",
                    &[("actor", viewer.login.clone()), ("device", id.to_string())],
                );
                inbox_admin::revoke(app, id).await
            }
            _ => Err(ApiError::new(
                StatusCode::NOT_FOUND,
                "not_found",
                "no such call",
            )),
        };
    }
    if known_get || known_post {
        return Err(ApiError::new(
            StatusCode::METHOD_NOT_ALLOWED,
            "method_not_allowed",
            "this method is not allowed for this call",
        ));
    }
    Err(ApiError::new(
        StatusCode::NOT_FOUND,
        "not_found",
        "no such call",
    ))
}

/// The rules of a POST: the same origin, a JSON body and the anti-CSRF token of the session.
fn check_post(state: &AppState, viewer: &Viewer, headers: &HeaderMap) -> Result<(), ApiError> {
    if !validate_post_origin(headers) {
        state.metrics.users_origin_failure();
        log(
            state,
            LogLevel::Warn,
            "origin_check_failed",
            &[("route", "inbox".to_string())],
        );
        return Err(ApiError::new(
            StatusCode::FORBIDDEN,
            "origin",
            "origin validation failed",
        ));
    }
    let json_body = match unique_header_str_name(headers, CONTENT_TYPE) {
        Ok(Some(v)) => v
            .split(';')
            .next()
            .is_some_and(|v| v.trim().eq_ignore_ascii_case("application/json")),
        _ => false,
    };
    if !json_body {
        return Err(ApiError::new(
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "content_type",
            "the body must have the content type application/json",
        ));
    }
    let token = match unique_header_str(headers, "x-csrf-token") {
        Ok(Some(v)) => v,
        _ => "",
    };
    if !verify_action_token(&state.signing_secret, CSRF_PURPOSE, &viewer.token, token) {
        return Err(ApiError::new(
            StatusCode::FORBIDDEN,
            "csrf",
            "the header X-CSRF-Token is missing or not valid",
        ));
    }
    Ok(())
}

fn inbox_json(value: &serde_json::Value) -> HttpResponse {
    crate::inbox::json_response(StatusCode::OK, value)
}
