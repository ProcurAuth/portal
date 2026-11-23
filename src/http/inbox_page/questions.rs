//! The web pages of the questions under `/auth/settings/inbox`: the page of one question and the form that
//! answers or declines it. The panel "Open items", the History and the form "Ask the agent" are in `items.rs`, which calls
//! this module for a row of kind `question`.
//!
//! The web page is a second surface to answer, with the phone. It uses the same engine: `Items::answer` runs the
//! check of the answer (`questions::check_answer`), the compare-and-set on the state, the lock of a new agent and the
//! rules of a closed item. The page adds three rules and no more:
//!
//! - It is for the owner of the question. Another account, and a question that does not exist, give the same answer.
//! - A session that the superadmin opens as another account answers nothing.
//! - It needs no biometric. The session and the token `csrf` bind the answer to the owner.
//!
//! A refusal of the engine is the page of the question with a notice and the status that fits: `409` for a closed
//! question or a lock, `422` for an answer that does not fit. The entries of the owner go back into the fields.

use inbox_core::proto::{Action, AnswerStatus, ExplanationFormat, Kind};
use serde_json::Value;

use super::*;
use crate::inbox::page_items::{
    check_answer, input_of, AgentTurn, Answer, AnswerVia, Answered, Caller, ItemError, Items,
    Loaded, OwnerAnswer, Served, TurnPlain,
};
use crate::ui::items::{self as ui_items, Mode, Title, Turn, TurnText};
use crate::ui::question_page::{self as page, QBody, QDetail, QText};
use crate::ui::questions::{Draft, Sheet, MAX_TEXT_CHARS};

type Fields = std::collections::HashMap<String, String>;

/// The content of a question from its unsealed body.
fn parse_text(plain: &[u8]) -> Option<QText> {
    let value: Value = serde_json::from_slice(plain).ok()?;
    let input = input_of(plain)?;
    let get = |name: &str| value.get(name).cloned();
    Some(QText {
        title: value.get("title")?.as_str()?.to_string(),
        explanation: value
            .get("explanation")
            .and_then(Value::as_str)
            .map(str::to_string),
        format: get("explanation_format")
            .and_then(|f| serde_json::from_value::<ExplanationFormat>(f).ok())
            .unwrap_or(ExplanationFormat::Plain),
        command: get("command").and_then(|c| serde_json::from_value(c).ok()),
        input,
        hint: get("hint").and_then(|h| serde_json::from_value(h).ok()),
        as_of_ms: value.get("as_of_ms").and_then(Value::as_i64),
    })
}

/// The content of a question for its owner.
async fn body_of(items: &Items<'_>, l: &Loaded) -> Result<QBody, ItemError> {
    Ok(match items.serve_body(l).await? {
        Served::Body(plain) => {
            parse_text(&plain).map_or(QBody::Mismatch, |t| QBody::Shown(Box::new(t)))
        }
        Served::Purged => QBody::Purged,
        Served::Mismatch => QBody::Mismatch,
        Served::NotFound => QBody::Hidden,
    })
}

/// What a row of a list shows of an item: the title, the start of the command (an approval), and the words about the
/// kind of answer (a question).
pub(super) async fn list_parts(
    items: &Items<'_>,
    l: &Loaded,
    with_command: bool,
) -> (Title, Option<String>, Option<String>) {
    if l.item.kind == Kind::Question {
        return match body_of(items, l).await {
            Ok(QBody::Shown(t)) => (
                Title::Text(t.title.clone()),
                None,
                Some(page::row_note(l, &t.input)),
            ),
            Ok(QBody::Purged) => (Title::Purged, None, None),
            _ => (Title::Hidden, None, None),
        };
    }
    match super::items::body_of(items, l).await {
        Ok(ui_items::Body::Shown(t)) => {
            let first = with_command.then(|| {
                t.command
                    .lines()
                    .next()
                    .unwrap_or_default()
                    .chars()
                    .take(200)
                    .collect()
            });
            (Title::Text(t.title), first, None)
        }
        Ok(ui_items::Body::Purged) => (Title::Purged, None, None),
        _ => (Title::Hidden, None, None),
    }
}

/// The messages of the discussion and the answer of the owner, from the turns of the item.
fn split_turns(turns: Vec<TurnPlain>) -> (Vec<Turn>, Option<OwnerAnswer>) {
    let answer = turns
        .iter()
        .rev()
        .find(|t| t.row.kind == "answer")
        .and_then(|t| t.text.as_ref())
        .map(|plain| OwnerAnswer::from_plain(plain));
    let talk: Vec<TurnPlain> = turns
        .into_iter()
        .filter(|t| matches!(t.row.kind.as_str(), "say" | "reply"))
        .collect();
    let shown = talk
        .into_iter()
        .map(|t| {
            let agent = t.row.author == "agent";
            let text = match (&t.text, t.row.payload.is_some()) {
                (Some(plain), _) if agent => {
                    let turn = AgentTurn::from_plain(plain);
                    TurnText::Agent {
                        explanation: turn.explanation,
                        format: turn.explanation_format.unwrap_or(ExplanationFormat::Plain),
                        command: turn
                            .command
                            .map(|c| (c.text, c.language.unwrap_or_default())),
                    }
                }
                (Some(plain), _) => TurnText::Plain(String::from_utf8_lossy(plain).into_owned()),
                (None, true) => TurnText::Unreadable,
                (None, false) => TurnText::Empty,
            };
            Turn {
                agent,
                at: t.row.created_at,
                kind: t.row.kind.clone(),
                via: t.row.via.clone(),
                text,
            }
        })
        .collect();
    (shown, answer)
}

/// The page of one question. `mode` says what the viewer may do. `admin_body` is the route of the superadmin.
#[allow(clippy::too_many_arguments)]
pub(super) async fn detail(
    state: &Arc<AppState>,
    ctx: &Context<'_>,
    items: &Items<'_>,
    loaded: Loaded,
    mut mode: Mode,
    admin_body: bool,
    notice: Option<crate::ui::settings::Notice>,
    status: StatusCode,
    full: bool,
    draft: Option<&Draft>,
) -> HttpResponse {
    let id = loaded.item.request_id.clone();
    let body = if admin_body {
        match items.serve_body(&loaded).await {
            Ok(Served::NotFound) => return not_found(),
            Ok(Served::Body(plain)) => {
                // One audit line and one count for each read. The line holds no content.
                if items
                    .record_body_read(&id, loaded.item.kind, &ctx.viewer.login)
                    .await
                    .is_err()
                {
                    return unavailable();
                }
                mode = Mode::AdminBody;
                parse_text(&plain).map_or(QBody::Mismatch, |t| QBody::Shown(Box::new(t)))
            }
            Ok(Served::Purged) => QBody::Purged,
            Ok(Served::Mismatch) => QBody::Mismatch,
            Err(_) => return unavailable(),
        }
    } else if mode == Mode::Owner {
        match body_of(items, &loaded).await {
            Ok(b) => b,
            Err(_) => return unavailable(),
        }
    } else {
        QBody::Hidden
    };
    let (turns, answer) = if mode == Mode::Owner {
        match items.serve_turns(&id).await {
            Ok(t) => split_turns(t),
            Err(_) => return unavailable(),
        }
    } else {
        (Vec::new(), None)
    };
    let sender = super::agents::sender(state, ctx, items, &loaded, mode == Mode::Owner).await;
    let can_cancel = ctx.held.question_cancel;
    let body_link = !admin_body
        && mode == Mode::Admin
        && is_superadmin(&ctx.viewer)
        && ctx
            .app
            .config
            .at_rest
            .as_ref()
            .is_some_and(|a| a.admin_body_read);
    let d = QDetail {
        loaded: &loaded,
        body,
        sender,
        turns,
        answer,
        now_ms: unix_now() * 1000,
        stale_after_ms: ctx
            .app
            .config
            .items
            .as_ref()
            .map_or(i64::MAX, |c| c.stale_after.as_millis() as i64),
        csrf: &ctx.inbox_token,
        mode,
        can_cancel,
        body_link,
        full,
        draft,
        next: loaded.item.superseded_by.clone(),
    };
    let header = header_for(&ctx.app, &ctx.held, &ctx.viewer.login).await;
    let mut frame = ctx.frame(state);
    frame.notify = header;
    super::items::no_store(html_response(
        status,
        page::question_page(&frame, &d, notice),
    ))
}

/// The page of the question with a notice, for an answer that the server refuses.
async fn show(
    state: &Arc<AppState>,
    ctx: &Context<'_>,
    items: &Items<'_>,
    loaded: &Loaded,
    code: &'static str,
    status: StatusCode,
    draft: Option<Draft>,
) -> HttpResponse {
    detail(
        state,
        ctx,
        items,
        loaded.clone(),
        Mode::Owner,
        false,
        ui_items::notice_for(code),
        status,
        false,
        draft.as_ref(),
    )
    .await
}

/// `POST /auth/settings/inbox/items/question`: `request_id`, `action` (`accept` or `decline`), and the fields of the
/// blocks (see `ui::questions`). A decline takes no content: the page drops the fields.
pub(super) async fn answer(
    state: &Arc<AppState>,
    ctx: &Context<'_>,
    mut f: Fields,
) -> HttpResponse {
    let (Some(id), Some(action)) = (f.remove("request_id"), f.remove("action")) else {
        return bad_request();
    };
    if !super::items::id_ok(&id) {
        return bad_request();
    }
    let action = match action.as_str() {
        "accept" => Action::Accept,
        "decline" => Action::Decline,
        _ => return bad_request(),
    };
    if ctx.viewer.session.impersonated || !super::items::allowed(ctx) {
        return forbidden();
    }
    let Some(items) = ctx.app.items() else {
        return not_found();
    };
    let login = ctx.viewer.login.clone();
    let loaded = match items.get(&id, &Caller::Owner(login.clone())).await {
        Ok(Some(l)) if l.item.kind == Kind::Question => l,
        Ok(_) => return super::items::back(&id, "gone"),
        Err(_) => return super::items::back(&id, "failed"),
    };
    let mut payload = None;
    let mut draft = None;
    if action == Action::Accept {
        let t = match body_of(&items, &loaded).await {
            Ok(QBody::Shown(t)) => t,
            Ok(_) => {
                return show(
                    state,
                    ctx,
                    &items,
                    &loaded,
                    "q_no_body",
                    StatusCode::UNPROCESSABLE_ENTITY,
                    None,
                )
                .await
            }
            Err(_) => return super::items::back(&id, "failed"),
        };
        let rows = page::rows_of(&loaded, &t.input);
        let Some(sheet) = Sheet::of(&t.input, &rows) else {
            return show(
                state,
                ctx,
                &items,
                &loaded,
                "q_invalid",
                StatusCode::UNPROCESSABLE_ENTITY,
                None,
            )
            .await;
        };
        let mut d = Draft::read(&sheet, &mut f);
        // A field that the form of the page never makes is not an answer.
        if !f.is_empty() {
            return bad_request();
        }
        let Some(given) = d.answer(&sheet) else {
            return show(
                state,
                ctx,
                &items,
                &loaded,
                "q_invalid",
                StatusCode::UNPROCESSABLE_ENTITY,
                Some(d),
            )
            .await;
        };
        // The same check as the engine runs. A mistake here is a bug of the page, and the page says so.
        if check_answer(&t.input, loaded.item.allow_custom, action, &given).is_err() {
            return show(
                state,
                ctx,
                &items,
                &loaded,
                "q_invalid",
                StatusCode::UNPROCESSABLE_ENTITY,
                Some(d),
            )
            .await;
        }
        payload = Some(zeroize::Zeroizing::new(given.to_plain()));
        draft = Some(d);
    }
    let consume_within = ctx
        .app
        .config
        .approvals
        .as_ref()
        .map_or(std::time::Duration::from_secs(300), |a| a.consume_within);
    let given = Answer {
        login: login.clone(),
        via: AnswerVia::Web,
        action,
        shown_effect: None,
        client_msg_id: None,
        payload,
        consume_within,
    };
    match items.answer(&id, given).await {
        Ok(Answered::Accepted { .. }) => {
            log(
                state,
                LogLevel::Info,
                "inbox_web_answer",
                &[
                    ("actor", login),
                    ("request", id.clone()),
                    (
                        "action",
                        if action == Action::Accept {
                            "accept"
                        } else {
                            "decline"
                        }
                        .to_string(),
                    ),
                ],
            );
            super::items::back(
                &id,
                if action == Action::Accept {
                    "q_answered"
                } else {
                    "q_declined"
                },
            )
        }
        Ok(Answered::Refused { status, .. }) => {
            let (code, st) = match status {
                AnswerStatus::AlreadyAnswered => ("q_taken", StatusCode::CONFLICT),
                AnswerStatus::Abandoned => ("q_abandoned", StatusCode::CONFLICT),
                AnswerStatus::Expired => ("q_expired", StatusCode::CONFLICT),
                AnswerStatus::CustomNotAllowed => ("q_no_custom", StatusCode::UNPROCESSABLE_ENTITY),
                _ => ("q_closed", StatusCode::CONFLICT),
            };
            show(
                state,
                ctx,
                &items,
                &loaded,
                code,
                st,
                if st == StatusCode::CONFLICT {
                    None
                } else {
                    draft
                },
            )
            .await
        }
        // The agent is new and `lock_new` is true: the server refuses the answer. The page says why. A decline and a
        // question to the agent stay open.
        Ok(Answered::AgentLocked(_)) => {
            show(
                state,
                ctx,
                &items,
                &loaded,
                "q_locked",
                StatusCode::CONFLICT,
                draft,
            )
            .await
        }
        Ok(Answered::Invalid(_)) => {
            show(
                state,
                ctx,
                &items,
                &loaded,
                "q_invalid",
                StatusCode::UNPROCESSABLE_ENTITY,
                draft,
            )
            .await
        }
        Ok(Answered::NotFound) => super::items::back(&id, "gone"),
        Err(_) => super::items::back(&id, "failed"),
    }
}

/// The form "Ask the agent" that the server refuses before it asks the engine: the page of the question with the text
/// that the owner typed. `None`: the item is not a question of this owner, and the caller redirects as for an approval.
pub(super) async fn ask_refused(
    state: &Arc<AppState>,
    ctx: &Context<'_>,
    id: &str,
    text: &str,
    code: &'static str,
) -> Option<HttpResponse> {
    let items = ctx.app.items()?;
    let loaded = items
        .get(id, &Caller::Owner(ctx.viewer.login.clone()))
        .await
        .ok()
        .flatten()
        .filter(|l| l.item.kind == Kind::Question)?;
    let n = text.chars().count();
    let error = if code == "too_long" {
        format!("The text has {n} characters. The limit is {MAX_TEXT_CHARS}.")
    } else {
        "Type a text.".to_string()
    };
    let draft = Draft {
        ask_text: text.to_string(),
        ask_error: Some(error),
        ..Draft::default()
    };
    Some(
        detail(
            state,
            ctx,
            &items,
            loaded,
            Mode::Owner,
            false,
            ui_items::notice_for(code),
            StatusCode::UNPROCESSABLE_ENTITY,
            false,
            Some(&draft),
        )
        .await,
    )
}
