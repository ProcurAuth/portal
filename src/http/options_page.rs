//! `GET /auth/options`: the children of a node for the forms of grants (portal 3.1a).
//!
//! The editor of grants calls this address when a person opens a node that the action document
//! does not list. The portal asks the service for the children, with the identity token of this
//! person (protocol 5.6), and gives the answer as JSON. The portal makes the token only when the
//! person can open the service, so the form shows only what this person can see. A service that
//! publishes no list, or that does not answer, gives no option: `"unavailable": true`.

use super::*;
use crate::options::{origin_of, Query};

pub(super) const PATH: &str = "/auth/options";
const PARAMETERS: [&str; 5] = ["service", "type", "parent", "q", "after"];
/// The longest value of a parameter, in bytes.
const MAX_VALUE: usize = 1024;

fn bad_request() -> HttpResponse {
    text_response(StatusCode::BAD_REQUEST, "Bad Request\n")
}

/// The answer that lists nothing. `unavailable` tells the form that the service gave no list.
fn nothing(unavailable: bool) -> HttpResponse {
    json_response(
        StatusCode::OK,
        format!(r#"{{"items":[],"next":null,"unavailable":{unavailable}}}"#),
    )
}

pub(super) async fn handle_options(
    state: &Arc<AppState>,
    uri: &str,
    headers: &HeaderMap,
    client: &Client,
) -> HttpResponse {
    let Some(viewer) = current_viewer(state, headers, client) else {
        return text_response(StatusCode::UNAUTHORIZED, "Unauthorized\n");
    };
    if !keys_page::may_fill_forms(state, &viewer) {
        return text_response(StatusCode::FORBIDDEN, "Forbidden\n");
    }
    let mut params = std::collections::HashMap::new();
    if let Some((_, query)) = uri.split_once('?') {
        for pair in query.split('&').filter(|p| !p.is_empty()) {
            let (name, value) = pair.split_once('=').unwrap_or((pair, ""));
            let (Ok(name), Ok(value)) = (percent_decode_form(name), percent_decode_form(value))
            else {
                return bad_request();
            };
            if !PARAMETERS.contains(&name.as_str()) || value.len() > MAX_VALUE {
                return bad_request();
            }
            if params.insert(name, value).is_some() {
                return bad_request();
            }
        }
    }
    let (Some(service_id), Some(kind)) = (params.get("service"), params.get("type")) else {
        return bad_request();
    };
    let parent = params.get("parent").cloned().unwrap_or_default();
    let Ok(parent_path) = grants::parse_path(&parent) else {
        return bad_request();
    };
    let catalog = current_catalog(state);
    let service = catalog
        .groups()
        .iter()
        .flat_map(|g| g.services.iter())
        .find(|s| s.id == *service_id);
    let (Some(service), Some(document)) = (service, state.actions.declared(service_id)) else {
        return nothing(false);
    };
    // The type is a type of the document, and its parent is the type of the node (protocol 5.7).
    let parent_kind = parent_path
        .last()
        .map_or(grants::SERVICE_TYPE, |s| s.kind.as_str());
    let right_type = document
        .types
        .iter()
        .any(|t| t.name == *kind && t.parent == parent_kind);
    if !right_type {
        return bad_request();
    }
    if !may_open(viewer.grants.as_deref(), service_id) {
        return nothing(false);
    }
    let (Some(signer), Some(identity), Some(origin)) = (
        &state.signer,
        &state.config.identity,
        service.actions_url.as_deref().and_then(origin_of),
    ) else {
        return nothing(true);
    };
    let token = signer.token(viewer.session.sid, &viewer.login, service_id, unix_now());
    let query = Query {
        service: service_id.clone(),
        subject: viewer.login.clone(),
        parent,
        kind: kind.clone(),
        q: params.get("q").cloned().unwrap_or_default(),
        after: params.get("after").cloned().unwrap_or_default(),
    };
    match state
        .options
        .children(&origin, &identity.header, &token, &query)
        .await
    {
        Ok(answer) => {
            let items: Vec<serde_json::Value> = answer
                .items
                .iter()
                .map(|i| {
                    serde_json::json!({ "id": i.id, "label": i.label, "has_children": i.has_children })
                })
                .collect();
            json_response(
                StatusCode::OK,
                serde_json::json!({ "items": items, "next": answer.next, "unavailable": false })
                    .to_string(),
            )
        }
        Err(error) => {
            log(
                state,
                LogLevel::Debug,
                "options_failed",
                &[("service", service_id.clone()), ("error", error)],
            );
            nothing(true)
        }
    }
}
