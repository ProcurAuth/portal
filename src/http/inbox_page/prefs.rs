//! The page "Inbox > Preferences" at `/auth/settings/inbox/prefs`: the digest and the quiet hours of the account.
//!
//! Only the account itself uses the page. A session that the superadmin opens as the account gets `403`, for the page and
//! for the form, so no one changes the preferences of another account. The values go to the table `account_prefs`, the
//! table that `set_prefs` of the phone writes. A phone reads them in `welcome`. The page reads the same row, so a change
//! of the phone shows here at once.
//!
//! The server refuses a bad value with status 422 and stores nothing. It never changes a value to make it fit.

use inbox_core::proto::Prefs;

use super::items::allowed;
use super::*;
use crate::inbox::page_items::ItemError;
use crate::ui::prefs::{self as ui_prefs, Form, PrefsPage};

fn forbidden_page(state: &Arc<AppState>, ctx: &Context<'_>) -> HttpResponse {
    html_response(
        StatusCode::FORBIDDEN,
        ui_inbox::status_page(&ctx.frame(state), "Forbidden"),
    )
}

fn no_store(mut response: HttpResponse) -> HttpResponse {
    response.headers_mut().insert(
        HeaderName::from_static("cache-control"),
        HeaderValue::from_static("no-store"),
    );
    response
}

fn file_default(ctx: &Context<'_>) -> crate::config::DigestConfig {
    ctx.app
        .config
        .items
        .as_ref()
        .map(|i| i.digest.clone())
        .unwrap_or(crate::config::DigestConfig::Off)
}

/// Draws the page with `form`. A refusal gives the status 422.
async fn draw(
    state: &Arc<AppState>,
    ctx: &Context<'_>,
    form: &Form,
    has_own: bool,
    notice: Option<crate::ui::settings::Notice>,
    error: Option<&crate::inbox::prefs::PrefsError>,
) -> HttpResponse {
    let zones = crate::inbox::prefs::zone_names();
    let file = file_default(ctx);
    let header = header_for(&ctx.app, &ctx.held, &ctx.viewer.login).await;
    let mut frame = ctx.frame(state);
    frame.notify = header;
    let html = ui_prefs::prefs_page(
        &frame,
        &PrefsPage {
            csrf: &ctx.inbox_token,
            form,
            zones: &zones,
            file: &file,
            has_own,
            notice,
            error,
        },
    );
    let status = if error.is_some() {
        StatusCode::UNPROCESSABLE_ENTITY
    } else {
        StatusCode::OK
    };
    no_store(html_response(status, html))
}

/// `GET /auth/settings/inbox/prefs`.
pub(super) async fn page(state: &Arc<AppState>, ctx: &Context<'_>) -> HttpResponse {
    if !allowed(ctx) {
        return forbidden_page(state, ctx);
    }
    let Some(items) = ctx.app.items() else {
        return forbidden_page(state, ctx);
    };
    let Some(pairs) = query_pairs(ctx.uri) else {
        return bad_request();
    };
    let code = pairs.iter().find(|(name, _)| name == "s").map(|(_, v)| v);
    let notice = code.and_then(|code| ui_prefs::notice_for(code));
    if code.is_some() && notice.is_none() {
        return bad_request();
    }
    let prefs = match items.prefs(&ctx.viewer.login).await {
        Ok(p) => p,
        Err(_) => return unavailable(),
    };
    let form = Form::from_prefs(&prefs, &file_default(ctx));
    let has_own = prefs.quiet.is_some() || prefs.digest.is_some();
    draw(state, ctx, &form, has_own, notice, None).await
}

fn back(code: &str) -> HttpResponse {
    back_with(ui_prefs::PREFS, code)
}

/// `POST /auth/settings/inbox/prefs`: `digest` (`daily`, `every` or `off`), `at`, `tz`, `hours`, `from`, `to`, `high`. With
/// `reset`, the values of the account are removed and the defaults of the file apply again.
pub(super) async fn save(
    state: &Arc<AppState>,
    ctx: &Context<'_>,
    fields: std::collections::HashMap<String, String>,
) -> HttpResponse {
    if !allowed(ctx) {
        return forbidden_page(state, ctx);
    }
    let Some(items) = ctx.app.items() else {
        return forbidden_page(state, ctx);
    };
    let login = &ctx.viewer.login;
    if fields.contains_key("reset") {
        return match items.set_prefs(login, &Prefs::default()).await {
            Ok(()) => back("reset"),
            Err(ItemError::Degraded | ItemError::Replica) => unavailable(),
            Err(_) => back("failed"),
        };
    }
    let form = Form::from_fields(&fields);
    let prefs = match form.to_prefs(&fields) {
        Ok(prefs) => prefs,
        Err(error) => {
            let has_own = items
                .prefs(login)
                .await
                .map(|p| p.quiet.is_some() || p.digest.is_some())
                .unwrap_or(false);
            return draw(state, ctx, &form, has_own, None, Some(&error)).await;
        }
    };
    match items.set_prefs(login, &prefs).await {
        Ok(()) => back("saved"),
        Err(ItemError::Prefs(error)) => draw(state, ctx, &form, false, None, Some(&error)).await,
        Err(ItemError::Degraded | ItemError::Replica) => unavailable(),
        Err(_) => back("failed"),
    }
}
