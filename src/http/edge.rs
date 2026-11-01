//! The edge for the keys: `/auth/verify-key` (portal section 8, protocol 1.4, 8.3 and section 9).
//!
//! Traefik calls this route with ForwardAuth for each request on a route for the keys. The route
//! reads `Authorization` and the headers `X-Forwarded-*`. It never reads the body of the request.
//! It checks the key in memory, in the order of portal 8.3, and it answers:
//!
//! - 204 with the identity token of the key for the service, in the header that
//!   `identity.header` names. Traefik copies the token to the request and removes the header
//!   `Authorization` (portal 8.1).
//! - 401 with `WWW-Authenticate: Bearer` and an empty body for each failure. When the request has
//!   no text of a key, the header has no error. Otherwise it has `error="invalid_token"`. The
//!   answer never shows the cause (protocol 9.8). The cause is in the log and in the measures.
//!
//! A key check reads no database row and writes none (portal 8.3).

use super::*;
use crate::decision::{self, Matching, Owner, Question};
use crate::grants::{Grant, ACCESS};
use crate::keyauth::{self, Cause, OwnerSide};

/// The challenge of an answer 401 when the request has no text of a key.
const CHALLENGE: &str = "Bearer";
/// The challenge of an answer 401 when the request has a text that the edge refuses.
const CHALLENGE_INVALID: &str = "Bearer error=\"invalid_token\"";

pub(super) fn handle_verify_key(
    state: &AppState,
    headers: &HeaderMap,
    client: &Client,
) -> HttpResponse {
    match check(state, headers) {
        Ok(response) => response,
        Err(cause) => refuse(state, cause, client),
    }
}

/// The text of the key in `Authorization` (protocol 1.4): the scheme `Bearer` and the text. The
/// scheme has no case. A request with another scheme has no key.
fn key_text(headers: &HeaderMap) -> Result<&str, Cause> {
    let value = match unique_header_str(headers, "authorization") {
        Ok(Some(value)) => value,
        Ok(None) => return Err(Cause::NoKey),
        // Two headers, or a header that is not text: a request that the edge cannot read.
        Err(()) => return Err(Cause::BadFormat),
    };
    let Some((scheme, rest)) = value.split_once(' ') else {
        return if value.eq_ignore_ascii_case("bearer") {
            Err(Cause::BadFormat)
        } else {
            Err(Cause::NoKey)
        };
    };
    if !scheme.eq_ignore_ascii_case("bearer") {
        return Err(Cause::NoKey);
    }
    let text = rest.trim_start_matches(' ');
    if text.is_empty() {
        return Err(Cause::BadFormat);
    }
    Ok(text)
}

/// The services that the key can open: the services of its grants `access` whose decision for the
/// key is `true`, that is, the owner also has `access` on the service (protocol 3.5, 8.3).
fn open_services(state: &AppState, principal: &keyauth::Principal) -> Vec<Grant> {
    let key = &principal.key;
    let owner_counters = decision::CounterOwner::User(String::new());
    let mut services: Vec<&str> = key
        .grants
        .iter()
        .filter(|g| g.action == ACCESS && g.path.is_empty())
        .map(|g| g.service.as_str())
        .collect();
    services.sort_unstable();
    services.dedup();
    let mut out = Vec::new();
    for service in services {
        let question = Question {
            service,
            action: ACCESS,
            chain: &[],
            matching: Matching::Node,
            document: None,
        };
        let owner_grants: std::sync::Arc<[Grant]>;
        let owner = match &principal.owner {
            OwnerSide::Superadmin => Owner::Superadmin,
            OwnerSide::Person { login, .. } => {
                owner_grants = keyauth::person_grants_on(state, login, service)
                    .map(|(grants, _)| grants)
                    .unwrap_or_else(|| std::sync::Arc::from(Vec::new()));
                Owner::Person(&owner_grants)
            }
        };
        let verdict = decision::key(
            &key.counter_owner(),
            &key.grants,
            owner,
            &owner_counters,
            &question,
        );
        if verdict.allowed {
            if let Ok(grant) = Grant::new(ACCESS, service) {
                out.push(grant);
            }
        }
    }
    out
}

fn check(state: &AppState, headers: &HeaderMap) -> Result<HttpResponse, Cause> {
    let text = key_text(headers)?;
    if state.config.keys.is_none() {
        return Err(Cause::KeysDisabled);
    }
    crate::keys::check_format(text).map_err(|_| Cause::BadFormat)?;
    let digest = crate::keys::digest(text);
    let key = state.users.key_by_digest(&digest).or_else(|| {
        // The key can come from another instance, a few seconds ago (portal 10.7c).
        refresh_directory_if_due(state)
            .then(|| state.users.key_by_digest(&digest))
            .flatten()
    });
    let Some(key) = key else {
        return Err(Cause::UnknownKey);
    };
    let now = unix_now();
    let principal = keyauth::resolve(state, &key, now)?;
    let uri = match unique_header_str(headers, "x-forwarded-uri") {
        Ok(Some(uri))
            if validate_return_uri(uri, state.config.security.max_forwarded_uri_bytes) =>
        {
            uri
        }
        _ => return Err(Cause::BadForwardedHeader),
    };
    // The service is the one that owns the longest prefix of the path, as `/auth/verify` finds it.
    let catalog = current_catalog(state);
    let opened = open_services(state, &principal);
    let service = match access::decide(&catalog, Some(&opened), uri) {
        Decision::Allow {
            service: Some(service),
        } => service,
        // A path that no service owns: the key opens nothing.
        Decision::Allow { service: None } => return Err(Cause::NoService),
        Decision::Deny => {
            return Err(if access::path_readings(uri).is_none() {
                Cause::BadForwardedHeader
            } else if path_has_owner(&catalog, uri) {
                Cause::NoAccess
            } else {
                Cause::NoService
            })
        }
    };
    if state.actions.too_old(&service) {
        return Err(Cause::ActionsTooOld);
    }
    let (Some(signer), Some(identity)) = (&state.signer, &state.config.identity) else {
        // The block `keys` needs the block `identity`: the start checks it.
        return Err(Cause::KeysDisabled);
    };
    let owner = principal.owner_login(&state.superadmin_login).to_string();
    let (token, fresh) = signer.key_token(&key.id, &key.subject(), &owner, &service, now);
    let mut response = empty_response(StatusCode::NO_CONTENT);
    let Ok(name) = HeaderName::from_bytes(identity.header.as_bytes()) else {
        return Err(Cause::KeysDisabled);
    };
    if set_dynamic_header(&mut response, name, &token).is_err() {
        return Err(Cause::KeysDisabled);
    }
    state.key_checks.record(&key, now);
    state.key_stats.passed();
    if fresh {
        // The edge logs a check that passed only when it signs a new token (portal 10.5a).
        log(
            state,
            LogLevel::Info,
            "key_check",
            &[
                ("key", key.id.clone()),
                ("owner", owner),
                ("service", service),
                ("result", "ok".into()),
            ],
        );
    }
    Ok(response)
}

/// Whether a service owns the path, whatever the grants of the key are.
fn path_has_owner(catalog: &Catalog, uri: &str) -> bool {
    access::path_readings(uri).is_some_and(|readings| {
        readings
            .iter()
            .any(|reading| catalog.owner(reading).is_some())
    })
}

/// The answer 401 for each failure, with its measure and its line of the log (portal 10.5, 10.5d).
fn refuse(state: &AppState, cause: Cause, client: &Client) -> HttpResponse {
    state.key_stats.failed(cause);
    let address = client.ip.map(|ip| ip.to_string()).unwrap_or_default();
    if state
        .key_failure_log
        .due(&address, std::time::Instant::now())
    {
        let not_logged = state.key_failure_log.take_unlogged();
        let mut fields = vec![("source", address), ("cause", cause.as_str().to_string())];
        if not_logged > 0 {
            fields.push(("not_logged", not_logged.to_string()));
        }
        log(state, LogLevel::Info, "key_check_failed", &fields);
    }
    let mut response = empty_response(StatusCode::UNAUTHORIZED);
    let challenge = if cause == Cause::NoKey {
        CHALLENGE
    } else {
        CHALLENGE_INVALID
    };
    response.headers_mut().insert(
        hyper::header::WWW_AUTHENTICATE,
        HeaderValue::from_static(challenge),
    );
    response
}
