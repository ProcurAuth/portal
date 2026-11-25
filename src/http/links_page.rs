//! The personal links in the pages: the page "Links" of the settings, its three forms (add, change, delete) and the
//! icon of a link.
//!
//! A personal link is a tile that an account adds to its own launcher. Only its owner sees it, reads its icon and
//! changes it. The action `links.own.edit` of the service `portal` lets an account add and change links. An account
//! that lost the action keeps its links and can still delete them. A session of "Log in as" shows no personal link
//! and changes none: the superadmin who browses as an account does not read what the account keeps for itself.
//!
//! The portal never requests a link. It checks the link as it checks the link of an external service.

use super::*;
use crate::catalog::external::{self, ExternalEntry};
use crate::portal_actions as portal;
use crate::store::ExternalError;
use crate::ui::links as ui_links;

/// The purpose of the anti-forgery tokens of the forms of the page "Links".
pub(super) const LINKS_TOKEN: &str = "links";

/// The account holds the action `links.own.edit` now. A session of "Log in as" holds none.
fn can_edit(viewer: &Viewer) -> bool {
    if viewer.session.impersonated {
        return false;
    }
    match &viewer.grants {
        None => true,
        Some(grants) => portal::effective(grants, portal::LINKS_OWN_EDIT),
    }
}

/// The links of the account, sorted by name. A failure of the store gives no link and a log line.
pub(super) fn links_of(state: &AppState, viewer: &Viewer) -> Vec<ExternalEntry> {
    if viewer.session.impersonated {
        return Vec::new();
    }
    match crate::util::blocking(|| state.store.list_user_links(&viewer.login)) {
        Ok(mut links) => {
            links.sort_by(|a, b| {
                a.name
                    .to_lowercase()
                    .cmp(&b.name.to_lowercase())
                    .then(a.id.cmp(&b.id))
            });
            links
        }
        Err(error) => {
            log(
                state,
                LogLevel::Error,
                "personal_links_read_failed",
                &[("error", error)],
            );
            Vec::new()
        }
    }
}

/// Whether the viewer sees the tab "Links": the account holds the action, or it still has links.
pub(super) fn tab_visible(state: &AppState, viewer: &Viewer) -> bool {
    if viewer.session.impersonated {
        return false;
    }
    can_edit(viewer)
        || crate::util::blocking(|| state.store.count_user_links(&viewer.login))
            .is_ok_and(|held| held > 0)
}

/// The page "Links".
pub(super) async fn handle_get(state: &Arc<AppState>, uri: &str, viewer: &Viewer) -> HttpResponse {
    render(
        state,
        viewer,
        query_value(uri, "s").as_deref(),
        StatusCode::OK,
    )
    .await
}

async fn render(
    state: &Arc<AppState>,
    viewer: &Viewer,
    code: Option<&str>,
    status: StatusCode,
) -> HttpResponse {
    let (Ok(logout_token), Ok(token)) = (
        sign_action_token(&state.signing_secret, LOGOUT_TOKEN, &viewer.token),
        sign_action_token(&state.signing_secret, LINKS_TOKEN, &viewer.token),
    ) else {
        return text_response(StatusCode::INTERNAL_SERVER_ERROR, "authentication error\n");
    };
    let frame = ui::Frame {
        logout_token: &logout_token,
        keys: keys_page::list_visible(state, viewer),
        inbox: inbox_page::tab_visible(state, viewer),
        superadmin: is_superadmin(viewer),
        notify: inbox_page::notify_header(state, viewer).await,
        security: mfa_page::tab_visible(state, viewer),
        links: true,
        tab: state.tab_look(),
    };
    let links = links_of(state, viewer);
    let page = ui_links::links_page(
        &frame,
        &ui_links::LinksView {
            notice: code.and_then(ui_links::notice_for),
            token: &token,
            links: &links,
            can_edit: can_edit(viewer),
            max: state.config.ui.max_personal_links,
        },
    );
    html_response(status, page)
}

/// The response returns to the page "Links" with the outcome of an action, as a code from a fixed list.
fn redirect(code: &str) -> HttpResponse {
    redirect_response(
        StatusCode::SEE_OTHER,
        &format!("{}?s={code}", ui_links::PATH),
    )
}

/// The values of the form of a link, checked. The name, the link and the description follow the rules of an external
/// service. The field of the icon is empty, or it holds a data URI.
struct Fields {
    name: String,
    href: String,
    description: String,
    icon: Option<Arc<crate::catalog::image::Image>>,
}

/// Reads the four fields. The error is the code of the refusal.
fn read_fields(
    state: &AppState,
    form: &mut std::collections::HashMap<String, String>,
) -> Result<Fields, &'static str> {
    let name = form.remove("name").unwrap_or_default();
    let href = form.remove("href").unwrap_or_default();
    let description = form.remove("description").unwrap_or_default();
    let icon = form.remove("icon").unwrap_or_default();
    let (name, _) = external::check_name(&name).map_err(|_| "link-bad-name")?;
    let href = external::check_href(&href).map_err(|_| "link-bad-link")?;
    let description =
        external::check_description(&description).map_err(|_| "link-bad-description")?;
    let icon = match icon.trim() {
        "" => None,
        uri => match crate::catalog::image::Image::from_data_uri(uri) {
            Ok(image) => Some(Arc::new(image)),
            Err(reason) => {
                log(
                    state,
                    LogLevel::Info,
                    "personal_link_icon_refused",
                    &[("reason", reason)],
                );
                return Err("link-bad-icon");
            }
        },
    };
    Ok(Fields {
        name,
        href,
        description,
        icon,
    })
}

/// The form of a link: the session, the origin, the anti-forgery token and the size of the body are checked. A
/// session of "Log in as" changes nothing.
#[allow(clippy::result_large_err)] // the error is the response, sent as is
async fn link_request(
    state: &Arc<AppState>,
    request: Request<Incoming>,
    client: &Client,
    action: &'static str,
) -> Result<(Viewer, std::collections::HashMap<String, String>), HttpResponse> {
    let (viewer, form) = form_request(
        state,
        request,
        client,
        action,
        LINKS_TOKEN,
        EXTERNAL_ICON_FORM_BYTES,
        true,
    )
    .await?;
    if viewer.session.impersonated {
        state.metrics.user_admin(action, "forbidden");
        return Err(text_response(StatusCode::FORBIDDEN, "Forbidden\n"));
    }
    Ok((viewer, form))
}

/// Adds a link. The account needs the action `links.own.edit`.
pub(super) async fn handle_add(
    state: &Arc<AppState>,
    request: Request<Incoming>,
    client: &Client,
) -> HttpResponse {
    let (viewer, mut form) = match link_request(state, request, client, "link_add").await {
        Ok(v) => v,
        Err(response) => return response,
    };
    if !can_edit(&viewer) {
        state.metrics.user_admin("link_add", "forbidden");
        return text_response(StatusCode::FORBIDDEN, "Forbidden\n");
    }
    let fields = read_fields(state, &mut form);
    if !form.is_empty() {
        return text_response(StatusCode::BAD_REQUEST, "Bad Request\n");
    }
    let rejected = |code: &str| {
        state.metrics.user_admin("link_add", "rejected");
        redirect(code)
    };
    let fields = match fields {
        Ok(fields) => fields,
        Err(code) => return rejected(code),
    };
    // The id is the slug of the name. Two links of one account cannot have the same id.
    let Ok((_, id)) = external::check_name(&fields.name) else {
        return rejected("link-bad-name");
    };
    let entry = ExternalEntry {
        id,
        name: fields.name,
        href: fields.href,
        description: fields.description,
        icon: fields.icon,
        created_at: unix_now(),
        from_file: false,
    };
    let store = Arc::clone(&state.store);
    let login = viewer.login.clone();
    let max = state.config.ui.max_personal_links;
    let row = entry.clone();
    match tokio::task::spawn_blocking(move || store.insert_user_link(&login, &row, max)).await {
        Ok(Ok(())) => {}
        Ok(Err(ExternalError::Exists)) => return rejected("link-exists"),
        Ok(Err(ExternalError::Full)) => return rejected("link-limit"),
        Ok(Err(ExternalError::Db(error))) => {
            log(
                state,
                LogLevel::Error,
                "personal_link_insert_failed",
                &[("error", error)],
            );
            return rejected("link-error");
        }
        Err(_) => return rejected("link-error"),
    }
    state.metrics.user_admin("link_add", "ok");
    redirect("link-added")
}

/// Changes the name, the link, the description and the icon of a link. The account needs the action
/// `links.own.edit`. An empty field of the icon keeps the icon, and the box `remove_icon` removes it.
pub(super) async fn handle_edit(
    state: &Arc<AppState>,
    request: Request<Incoming>,
    client: &Client,
) -> HttpResponse {
    let (viewer, mut form) = match link_request(state, request, client, "link_edit").await {
        Ok(v) => v,
        Err(response) => return response,
    };
    if !can_edit(&viewer) {
        state.metrics.user_admin("link_edit", "forbidden");
        return text_response(StatusCode::FORBIDDEN, "Forbidden\n");
    }
    let id = form.remove("id").unwrap_or_default();
    let remove_icon = form.remove("remove_icon").is_some_and(|v| v == "1");
    let fields = read_fields(state, &mut form);
    if !form.is_empty() {
        return text_response(StatusCode::BAD_REQUEST, "Bad Request\n");
    }
    let rejected = |code: &str| {
        state.metrics.user_admin("link_edit", "rejected");
        redirect(code)
    };
    let fields = match fields {
        Ok(fields) => fields,
        Err(code) => return rejected(code),
    };
    let Some(current) = links_of(state, &viewer).into_iter().find(|e| e.id == id) else {
        return rejected("link-gone");
    };
    let icon = if remove_icon {
        None
    } else {
        fields.icon.or(current.icon.clone())
    };
    let entry = ExternalEntry {
        name: fields.name,
        href: fields.href,
        description: fields.description,
        icon,
        ..current
    };
    let store = Arc::clone(&state.store);
    let login = viewer.login.clone();
    match tokio::task::spawn_blocking(move || store.update_user_link(&login, &entry)).await {
        Ok(Ok(true)) => {}
        Ok(Ok(false)) => return rejected("link-gone"),
        Ok(Err(error)) => {
            log(
                state,
                LogLevel::Error,
                "personal_link_update_failed",
                &[("error", error)],
            );
            return rejected("link-error");
        }
        Err(_) => return rejected("link-error"),
    }
    state.metrics.user_admin("link_edit", "ok");
    redirect("link-updated")
}

/// Deletes a link. Any account deletes its own links, also an account that lost the action.
pub(super) async fn handle_delete(
    state: &Arc<AppState>,
    request: Request<Incoming>,
    client: &Client,
) -> HttpResponse {
    let (viewer, mut form) = match link_request(state, request, client, "link_delete").await {
        Ok(v) => v,
        Err(response) => return response,
    };
    let id = form.remove("id").unwrap_or_default();
    if !form.is_empty() {
        return text_response(StatusCode::BAD_REQUEST, "Bad Request\n");
    }
    let store = Arc::clone(&state.store);
    let login = viewer.login.clone();
    match tokio::task::spawn_blocking(move || store.remove_user_link(&login, &id)).await {
        Ok(Ok(true)) => {
            state.metrics.user_admin("link_delete", "ok");
            redirect("link-deleted")
        }
        Ok(Ok(false)) => {
            state.metrics.user_admin("link_delete", "rejected");
            redirect("link-gone")
        }
        Ok(Err(error)) => {
            log(
                state,
                LogLevel::Error,
                "personal_link_delete_failed",
                &[("error", error)],
            );
            redirect("link-error")
        }
        Err(_) => redirect("link-error"),
    }
}

/// `/auth/my-icon/<id>`: the own icon of a personal link, for its owner only. Any other request gets the answer of
/// an unknown path: a stranger, a session of "Log in as", a link of another account and a link without icon look the
/// same. The image is served as the icon of an external service is.
pub(super) fn handle_icon(
    state: &AppState,
    path: &str,
    uri: &str,
    headers: &HeaderMap,
    client: &Client,
) -> HttpResponse {
    let Some(viewer) = current_viewer_passive(state, headers, client) else {
        return not_found_response(state, &Method::GET, uri, || false);
    };
    let image = path
        .strip_prefix(external::PERSONAL_ICON_PREFIX)
        .and_then(|raw| percent_decode_form(raw).ok())
        .and_then(|id| {
            links_of(state, &viewer)
                .into_iter()
                .find(|entry| entry.id == id)
        })
        .and_then(|entry| entry.icon);
    let Some(image) = image else {
        return not_found_response(state, &Method::GET, uri, || true);
    };
    icon_response(&image, headers)
}
