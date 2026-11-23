//! The page of one question in the tab "Inbox": the title, the box "From agent", the command, the explanation of
//! the agent, the form of the answer by the type of the input, the rows "Other answer" and "Ask the agent", the
//! discussion, and the view of a question that is closed. The panel "Open items" and the History show a question with the
//! code of `items.rs`.
//!
//! The page works without script. A form is a POST with the token `csrf`. The box "Ask the agent" is a second form: its
//! field and its button name it with the attribute `form`, so the two forms never nest.
//!
//! Three kinds of text are on the page, and they never mix:
//!
//! - **Text of the server.** The words of the page, the times, the state, the name of the key.
//! - **Text of the agent.** The title, the explanation, the command, the labels of the options, the hint and the turns. It
//!   goes through [`html::escape`] (a hidden character has a visible mark), [`html::explanation`] or [`html::command`].
//! - **Text of the owner.** What the owner typed goes back into a field as it was typed, escaped and never marked.

use std::fmt::Write as _;

use inbox_core::proto::{
    Action, Command, ExplanationFormat, FormQuestionType, Hint, Input, InputOption, InputType,
    Interaction, ItemState,
};

use super::agents::{about_sender, sender_box, Sender};
use super::components::icon;
use super::items::{
    ago, b, code_of, detail_href, explanation_block, long_span, marks_banner, outcome, turns_html,
    when, word_of, zone_head, Mode, Turn, ITEMS, QUESTION, REPLY,
};
use super::markdown_html as html;
use super::questions::{
    rows, Block, BlockDraft, BlockKind, Draft, Rows, Sheet, ASK_NOTE, ASK_WORD, AUTOMATIC_HEADER,
    DECLINE_WORD, LOCK_LINE, MAX_TEXT_CHARS, NO_ANSWER_NOTE, NO_DISCUSSION, OTHER_NOTE, OTHER_WORD,
    SECRET_WARNING,
};
use super::settings::{fill_page, notice_html, Frame, Notice, Tab};
use crate::inbox::page_items::{Loaded, OwnerAnswer};
use crate::util::html_escape;

/// The content of a question, as the page shows it.
pub struct QText {
    pub title: String,
    pub explanation: Option<String>,
    pub format: ExplanationFormat,
    pub command: Option<Command>,
    pub input: Input,
    pub hint: Option<Hint>,
    pub as_of_ms: Option<i64>,
}

pub enum QBody {
    Shown(Box<QText>),
    /// The viewer reads metadata only.
    Hidden,
    /// The content is erased (`body_purge_after`).
    Purged,
    /// The seal does not open, or the body is not a question.
    Mismatch,
}

/// What the page of one question shows.
pub struct QDetail<'a> {
    pub loaded: &'a Loaded,
    pub body: QBody,
    /// The agent that asked: the box "From agent" and the lock of the answer.
    pub sender: Sender,
    /// The messages of the discussion, in order. Only turns of the agent and questions of the owner.
    pub turns: Vec<Turn>,
    /// The answer of the owner, when the item is answered and the answer can be read.
    pub answer: Option<OwnerAnswer>,
    pub now_ms: i64,
    pub stale_after_ms: i64,
    pub csrf: &'a str,
    pub mode: Mode,
    pub can_cancel: bool,
    /// The superadmin may open the content with `admin_body_read`.
    pub body_link: bool,
    /// The whole explanation, not the first 400 characters.
    pub full: bool,
    /// The entries of a form that the server refused: they go back into the fields.
    pub draft: Option<&'a Draft>,
    /// The request that replaced this one.
    pub next: Option<String>,
}

fn form_has_choice(input: &Input) -> bool {
    input
        .questions
        .iter()
        .flatten()
        .any(|q| q.kind == FormQuestionType::Choice)
}

/// The rows under the options of a question, from its flags.
pub fn rows_of(l: &Loaded, input: &Input) -> Rows {
    rows(
        input.kind,
        l.item.allow_custom,
        l.item.allow_ask_back,
        l.item.interaction,
        form_has_choice(input),
    )
}

/// The words of the server about the kind of answer: "3 choices", "Yes or no". They go in the row of a list.
pub fn kind_words(input: &Input) -> String {
    match input.kind {
        InputType::Choice if input.multiple => format!("choose several of {}", input.options.len()),
        InputType::Choice => format!("{} choices", input.options.len()),
        InputType::Confirm => "yes or no".to_string(),
        InputType::Text => "text answer".to_string(),
        InputType::None => "no answer needed".to_string(),
        InputType::Form => format!("{} questions", input.questions.as_ref().map_or(0, Vec::len)),
    }
}

/// The note of a row of a list: the kind of answer and the state of the discussion. Text of the server.
pub fn row_note(l: &Loaded, input: &Input) -> String {
    let i = &l.item;
    let mut parts = vec![kind_words(input)];
    if i.interaction == Interaction::OneShot {
        parts.push("automatic system, no chat".to_string());
    } else if i.last_seq > 0 {
        parts.push(format!(
            "open discussion, {} message{}",
            i.last_seq,
            if i.last_seq == 1 { "" } else { "s" }
        ));
    }
    parts.join(" · ")
}

// The head -------------------------------------------------------------------------------------------

fn automatic(l: &Loaded) -> bool {
    l.item.interaction == Interaction::OneShot
}

fn kind_chip(l: &Loaded) -> &'static str {
    if automatic(l) {
        r#"<span class="kind kind--auto">Automatic system</span>"#
    } else {
        r#"<span class="kind">Question</span>"#
    }
}

fn state_pill(d: &QDetail) -> String {
    let i = &d.loaded.item;
    match i.state {
        ItemState::Pending | ItemState::Replied if d.mode != Mode::Owner => {
            r#"<span class="pill">Open</span>"#.to_string()
        }
        ItemState::Replied => r#"<span class="pill">Agent is writing</span>"#.to_string(),
        ItemState::Pending if i.last_agent_turn_at.is_some() => {
            r#"<span class="pill pill--wait">Your turn</span>"#.to_string()
        }
        ItemState::Pending => r#"<span class="pill pill--wait">Waiting for you</span>"#.to_string(),
        ItemState::Answered if i.answer_action == Some(Action::Accept) => format!(
            r#"<span class="pill pill--ok">{}{}</span>"#,
            icon("circle-check"),
            word_of(d.loaded)
        ),
        _ => format!(r#"<span class="pill">{}</span>"#, word_of(d.loaded)),
    }
}

fn head(d: &QDetail, title_html: &str, stale: bool) -> String {
    let l = d.loaded;
    let i = &l.item;
    let old = if stale {
        format!(
            r#"<span class="pill pill--warn">{}Old question</span>"#,
            icon("clock")
        )
    } else {
        String::new()
    };
    let lease = match (i.lease_until, i.until_at, i.is_open()) {
        (Some(at), None, true) => {
            format!(r#"<span class="lease">Lease until {}</span>"#, b(&when(at)))
        }
        (_, Some(at), true) => {
            format!(r#"<span class="lease">Waits until {}</span>"#, b(&when(at)))
        }
        _ => String::new(),
    };
    let seen = if automatic(l) {
        format!(
            r#"<span class="lease">Last computed {}</span>"#,
            b(&ago(d.now_ms, i.agent_seen_at))
        )
    } else {
        format!(
            r#"<span class="lease">Agent active {}</span>"#,
            b(&ago(d.now_ms, i.agent_seen_at))
        )
    };
    let who = if automatic(l) {
        format!(
            r#"<span><b>{AUTOMATIC_HEADER}</b> <span class="muted">· key <b>{}</b> of <b>{}</b></span></span>"#,
            html::escape(&d.sender.agent.key_name),
            html_escape(&i.requester_owner)
        )
    } else {
        String::new()
    };
    let answered = match (i.answered_at, i.state) {
        (Some(at), ItemState::Answered) if d.mode == Mode::Owner => format!(
            r#"<span class="lease">You {} {}</span>"#,
            if i.answer_action == Some(Action::Accept) {
                "answered"
            } else {
                "declined"
            },
            b(&when(at))
        ),
        _ => String::new(),
    };
    format!(
        r#"<div class="head"><div class="chipStack">{}<span class="code">{}</span>{old}{}</div><h2>{title_html}</h2><div class="head__row">{who}<span class="lease">Asked {}</span>{seen}{lease}{answered}</div></div>{}"#,
        kind_chip(l),
        html_escape(&code_of(l)),
        state_pill(d),
        b(&ago(d.now_ms, i.created_at)),
        sender_box(&d.sender, &i.requester_owner),
    )
}

// The command and the hint -----------------------------------------------------------------------------

fn command_zone(d: &QDetail, c: &Command) -> String {
    let m = html::marks(&c.text);
    let lines = c.text.lines().count().max(1);
    let language = c.language.unwrap_or_default();
    let colors = if language == inbox_core::proto::Language::Plain {
        String::new()
    } else {
        format!(
            r#"<span class="kind kind--auto" title="The hint of the agent only sets the colors. It does not change the text.">{} colors</span>"#,
            language.as_str().to_uppercase()
        )
    };
    let copy = format!(
        r#"<button class="button button--small" type="button" data-copy-text="{}" hidden>{}Copy command</button>"#,
        inbox_core::b64::encode(c.text.as_bytes()),
        icon("copy")
    );
    let label = c.label.as_deref().unwrap_or("Command");
    let ascii = if m.non_ascii == 0 {
        "ASCII only".to_string()
    } else {
        format!(
            "{} non-ASCII character{}",
            m.non_ascii,
            if m.non_ascii == 1 { "" } else { "s" }
        )
    };
    let attention = if m.marked > 0 {
        format!(
            " · {}",
            b(&format!(
                "{} character{} need attention",
                m.marked,
                if m.marked == 1 { "" } else { "s" }
            ))
        )
    } else {
        String::new()
    };
    format!(
        r#"{}<section class="sec2 sec2--cmd" aria-labelledby="cmd-t">{}<div class="cmdscroll" tabindex="0" role="region" aria-label="{}">{}</div><div class="sec2__foot"><span>{lines} line{} · {} byte{} · {ascii}{attention}</span><span>Asked {}</span></div></section>"#,
        marks_banner(&m, "answer"),
        zone_head(
            "Command",
            "cmd-t",
            &format!(
                r#"<span class="trust trust--ok">{}Exact text</span>{colors}"#,
                icon("shield-check")
            ),
            &format!(
                "The agent wrote {}. It is shown as it is.",
                html::escape(label)
            ),
            &copy
        ),
        html::escape_attr(label),
        html::command(&c.text, language),
        if lines == 1 { "" } else { "s" },
        c.text.len(),
        if c.text.len() == 1 { "" } else { "s" },
        ago(d.now_ms, d.loaded.item.created_at),
    )
}

/// The suggestion of the agent or of the system. It is above the options and it never ticks an option.
fn hint_box(h: &Hint, sheet: Option<&Sheet>) -> String {
    let label = sheet
        .filter(|s| !s.is_form)
        .and_then(|s| s.blocks.first())
        .and_then(|bl| bl.options.iter().find(|o| o.id == h.suggested))
        .map_or_else(|| h.suggested.clone(), |o| o.label.clone());
    let conf = h
        .confidence
        .map(|c| (c * 100.0).round().clamp(0.0, 100.0) as u32);
    let meter = conf
        .map(|p| {
            format!(
                r#"<span class="muted">Confidence {p} %</span><meter class="meter" min="0" max="100" value="{p}" aria-label="Confidence {p} percent">{p} %</meter>"#
            )
        })
        .unwrap_or_default();
    format!(
        r#"<div class="auto" role="note"><div class="auto__row"><b>Suggestion (not checked): {}</b>{meter}<span class="muted">Source: {}</span></div><span class="note">Not checked. The sender suggests this answer. You decide.</span></div>"#,
        html::escape(&label),
        html::escape(&h.source),
    )
}

// The form -------------------------------------------------------------------------------------------

fn opt_text(o: &InputOption) -> String {
    let desc = o
        .description
        .as_deref()
        .map(|t| format!("<small>{}</small>", html::escape(t)))
        .unwrap_or_default();
    format!("<b>{}</b>{desc}", html::escape(&o.label))
}

fn checked(on: bool) -> &'static str {
    if on {
        " checked"
    } else {
        ""
    }
}

fn is_on(on: bool) -> &'static str {
    if on {
        " is-on"
    } else {
        ""
    }
}

/// The field of a free text. The text goes back as it was typed.
fn text_field(
    name: &str,
    text: &str,
    label: &str,
    placeholder: Option<&str>,
    rows_n: u8,
) -> String {
    let ph = html::escape_attr(placeholder.unwrap_or("Type your answer"));
    format!(
        r#"<textarea class="uiInput qtext" name="{name}" rows="{rows_n}" maxlength="{MAX_TEXT_CHARS}" placeholder="{ph}" aria-label="{}">
{}</textarea><span class="note hintHidden">{}{SECRET_WARNING} The agent reads this text.</span>"#,
        html::escape_attr(label),
        html_escape(text),
        icon("alert-triangle").replace(r#"class="icon""#, r#"class="icon icon--sm""#),
    )
}

fn miss(error: &Option<String>) -> String {
    error
        .as_deref()
        .map(|e| {
            format!(
                r#"<span class="miss" role="alert">{}</span>"#,
                html_escape(e)
            )
        })
        .unwrap_or_default()
}

fn type_words(bl: &Block) -> String {
    match bl.kind {
        BlockKind::Radio | BlockKind::Confirm => "Choose one.".to_string(),
        BlockKind::Text => "Write a text.".to_string(),
        BlockKind::Checkbox => {
            let (min, max) = (bl.min as usize, bl.max as usize);
            let n = bl.options.len();
            if min == max {
                format!("Choose {min}.")
            } else if max >= n && min >= 1 {
                format!("Choose at least {min}. No maximum.")
            } else if min == 0 {
                format!("Choose at most {max}.")
            } else {
                format!("Choose {min} to {max}.")
            }
        }
    }
}

fn block_html(n: usize, bl: &Block, sheet: &Sheet, d: &BlockDraft) -> String {
    let mut out = String::new();
    let hint = type_words(bl);
    if sheet.is_form {
        let state = if d.error.is_some() {
            format!(
                r#"<span class="qstate qstate--open">{}Needs an answer</span>"#,
                icon("point")
            )
        } else {
            String::new()
        };
        let _ = write!(
            out,
            r#"<div class="q"><div class="q__head"><span class="qnum">{}</span><span class="qchip">{}</span><span class="q__type">{}</span>{state}</div><h3 class="q__text">{}</h3>"#,
            n + 1,
            html::escape(bl.header.as_deref().unwrap_or_default()),
            html_escape(&hint),
            html::escape(bl.text.as_deref().unwrap_or_default()),
        );
    } else {
        let _ = write!(out, r#"<div class="q q--single">"#);
    }
    match bl.kind {
        BlockKind::Text => {
            let _ = write!(
                out,
                "{}{}",
                text_field(
                    &format!("b{n}t"),
                    &d.text,
                    bl.text.as_deref().unwrap_or("Your answer"),
                    bl.placeholder.as_deref(),
                    if sheet.is_form { 2 } else { 3 }
                ),
                miss(&d.error)
            );
        }
        _ => {
            let multiple = bl.kind == BlockKind::Checkbox;
            let group = if multiple { "group" } else { "radiogroup" };
            let _ = write!(
                out,
                r#"<div class="qopts opts" role="{group}" aria-label="Choices">"#
            );
            for o in &bl.options {
                let on = d.ticked.contains(&o.id);
                let input = if multiple {
                    format!(
                        r#"<input type="checkbox" name="b{n}o_{}" value="1"{}>"#,
                        html_escape(&o.id),
                        checked(on)
                    )
                } else {
                    format!(
                        r#"<input type="radio" name="b{n}" value="o:{}"{}>"#,
                        html_escape(&o.id),
                        checked(on)
                    )
                };
                let _ = write!(
                    out,
                    r#"<label class="opt{}">{input}<span>{}</span></label>"#,
                    is_on(on),
                    opt_text(o)
                );
            }
            if bl.other {
                let input = if multiple {
                    format!(
                        r#"<input type="checkbox" name="b{n}x" value="1"{}>"#,
                        checked(d.other_on)
                    )
                } else {
                    format!(
                        r#"<input type="radio" name="b{n}" value="x"{}>"#,
                        checked(d.other_on)
                    )
                };
                let note = if multiple {
                    "Your own answer. It replaces the ticks."
                } else if sheet.is_form {
                    "Your own answer. It replaces the choices."
                } else {
                    OTHER_NOTE
                };
                let _ = write!(
                    out,
                    r#"<label class="opt is-other{}">{input}<span><b>{OTHER_WORD}</b><span class="tagx">exclusive</span><small>{note}</small>{}{}</span></label>"#,
                    is_on(d.other_on),
                    text_field(
                        &format!("b{n}t"),
                        &d.other_text,
                        &format!("{OTHER_WORD}, question {}", n + 1),
                        None,
                        2
                    ),
                    miss(&d.error)
                );
            }
            out.push_str("</div>");
            if !bl.other {
                out.push_str(&miss(&d.error));
            }
        }
    }
    out.push_str("</div>");
    out
}

/// The row "Ask the agent": a text field and its own button. They belong to the form `qask`.
fn ask_row(d: &QDetail) -> String {
    let writing = d.loaded.item.state == ItemState::Replied;
    let off = if writing { " disabled" } else { "" };
    let text = d.draft.map(|x| x.ask_text.as_str()).unwrap_or_default();
    let error = d.draft.map(|x| x.ask_error.clone()).unwrap_or_default();
    let note = if writing {
        "The agent is writing its answer. The field opens when it replies. If the agent stays silent, the item becomes abandoned."
    } else {
        ASK_NOTE
    };
    format!(
        r#"<div class="escape"><div class="escape__row">{}<div><b>{ASK_WORD}</b><div class="note">{note}</div><textarea class="uiInput" name="text" form="qask" rows="2" maxlength="{}" placeholder="Your question to the agent" aria-label="Question to the agent"{off}>
{}</textarea>{}<span class="note hintHidden">{}{SECRET_WARNING} The agent reads this text.</span><div class="inline"><button class="button button--small" type="submit" form="qask"{off}>{}Send question</button></div></div></div></div>"#,
        icon("help-circle"),
        super::items::MAX_REPLY_CHARS,
        html_escape(text),
        miss(&error),
        icon("alert-triangle").replace(r#"class="icon""#, r#"class="icon icon--sm""#),
        icon("send"),
    )
}

fn hidden_fields(d: &QDetail) -> String {
    format!(
        r#"<input type="hidden" name="csrf" value="{}"><input type="hidden" name="request_id" value="{}">"#,
        html_escape(d.csrf),
        html_escape(&d.loaded.item.request_id)
    )
}

/// The form of the answer for the owner of an open question.
fn answer_form(d: &QDetail, t: &QText) -> String {
    let l = d.loaded;
    let rows = rows_of(l, &t.input);
    let sheet = Sheet::of(&t.input, &rows);
    let empty;
    let draft = match (d.draft, &sheet) {
        (Some(x), Some(s)) if x.blocks.len() == s.blocks.len() => x,
        (_, Some(s)) => {
            empty = Draft::empty(s);
            &empty
        }
        _ => {
            empty = Draft::default();
            &empty
        }
    };
    let mut body = String::new();
    if let Some(h) = &t.hint {
        body.push_str(&hint_box(h, sheet.as_ref()));
    }
    let mut notes = String::new();
    let locked = d.sender.locked();
    let send_word = match &sheet {
        Some(s) if s.is_form => "Send answers",
        _ => "Send answer",
    };
    let mut head_note = "Questions and choices come from the agent.".to_string();
    let mut trust = String::new();
    match &sheet {
        Some(s) => {
            if s.is_form {
                trust = format!(
                    r#"<span class="trust trust--no">{}Not checked</span>"#,
                    icon("alert-triangle")
                );
            } else {
                head_note = type_words(&s.blocks[0]);
            }
            for (n, bl) in s.blocks.iter().enumerate() {
                body.push_str(&block_html(n, bl, s, &draft.blocks[n]));
            }
        }
        None => {
            if let Some(why) = rows.invalid {
                notes = format!(
                    r#"<div class="banner banner--warn" role="alert">{}<p><b>This question is not valid.</b> {} The page does not answer it. You can decline it.</p></div>"#,
                    icon("alert-triangle"),
                    why.line()
                );
            } else {
                notes = format!(r#"<p class="note">{NO_ANSWER_NOTE}</p>"#);
            }
            head_note = String::new();
        }
    }
    let mut ask = String::new();
    if rows.ask_agent {
        ask = ask_row(d);
    }
    let lock_line = if locked {
        format!(
            r#"<div class="lockline">{}<span>{LOCK_LINE}</span></div>"#,
            icon("lock")
        )
    } else {
        String::new()
    };
    let send = match (&sheet, locked) {
        (None, _) => String::new(),
        (Some(_), true) => format!(
            r#"<button class="button button--approve button--off" type="button" disabled>{}{send_word}</button>"#,
            icon("lock")
        ),
        (Some(_), false) => format!(
            r#"<button class="button button--approve" type="submit" name="action" value="accept">{}{send_word}</button>"#,
            icon("send")
        ),
    };
    let side_note = if automatic(l) {
        NO_DISCUSSION
    } else if sheet.as_ref().is_some_and(|s| s.is_form) {
        "All answers go to the agent together."
    } else if rows.closed() && sheet.is_some() {
        "Only the choices answer. You can still decline."
    } else {
        "Your answer goes to the agent."
    };
    // The second form exists only with the row "Ask the agent": an automatic system has no control of a discussion.
    let ask_form = if rows.ask_agent {
        format!(
            r#"<form id="qask" method="post" action="{REPLY}">{}</form>"#,
            hidden_fields(d)
        )
    } else {
        String::new()
    };
    format!(
        r#"<form class="sec2 qform" method="post" action="{QUESTION}" aria-label="Your answer"><div class="sec2__head"><h2 class="title">Your answer</h2>{trust}<span class="sec2__note">{}</span></div>{body}{notes}{ask}<div class="qform__send">{lock_line}<div class="decide__row">{hidden}{send}<button class="button" type="submit" name="action" value="decline">{DECLINE_WORD}</button><span class="grow"></span><span class="note">{side_note}</span></div></div></form>{ask_form}"#,
        html_escape(&head_note),
        hidden = hidden_fields(d),
    )
}

// The view of a question that is closed ----------------------------------------------------------------

struct Chosen<'a> {
    block: &'a Block,
    chosen: Vec<&'a InputOption>,
    free: Option<String>,
}

fn chosen_blocks<'a>(sheet: &'a Sheet, a: &OwnerAnswer) -> Option<Vec<Chosen<'a>>> {
    let part = |bl: &'a Block, choice: Option<&Vec<String>>, text: Option<&String>| Chosen {
        block: bl,
        chosen: bl
            .options
            .iter()
            .filter(|o| choice.is_some_and(|c| c.contains(&o.id)))
            .collect(),
        free: text.cloned(),
    };
    if sheet.is_form {
        let list = a.answers.as_ref()?;
        sheet
            .blocks
            .iter()
            .map(|bl| {
                let x = list
                    .iter()
                    .find(|x| Some(&x.question_id) == bl.id.as_ref())?;
                Some(part(bl, x.choice.as_ref(), x.text.as_ref()))
            })
            .collect()
    } else {
        let bl = sheet.blocks.first()?;
        Some(vec![part(bl, a.choice.as_ref(), a.text.as_ref())])
    }
}

fn answer_view(d: &QDetail, t: &QText) -> String {
    let i = &d.loaded.item;
    if i.state != ItemState::Answered || d.mode != Mode::Owner {
        return String::new();
    }
    if i.answer_action != Some(Action::Accept) {
        return r#"<section class="sec2" aria-labelledby="ans-t"><div class="sec2__head"><h2 class="title" id="ans-t">Your answer</h2></div><div class="ans"><p class="ans__text">You declined to answer. The agent is told that you declined. You cannot change this.</p></div></section>"#.to_string();
    }
    let free_rows = Rows {
        other_answer: i.allow_custom,
        ask_agent: false,
        invalid: None,
    };
    let sheet = Sheet::of(&t.input, &free_rows);
    let blocks = match (&sheet, &d.answer) {
        (Some(s), Some(a)) => chosen_blocks(s, a),
        _ => None,
    };
    let Some(blocks) = blocks else {
        return r#"<section class="sec2" aria-labelledby="ans-t"><div class="sec2__head"><h2 class="title" id="ans-t">Your answer</h2></div><div class="ans"><p class="ans__note">You answered. The text of the answer is not available on this page.</p></div></section>"#.to_string();
    };
    let many = blocks.len() > 1;
    let mut out = String::new();
    for (n, c) in blocks.iter().enumerate() {
        let head = if many {
            format!(
                r#"<div class="q__head"><span class="qnum">{}</span><span class="qchip">{}</span></div><h3 class="q__text">{}</h3>"#,
                n + 1,
                html::escape(c.block.header.as_deref().unwrap_or_default()),
                html::escape(c.block.text.as_deref().unwrap_or_default())
            )
        } else {
            String::new()
        };
        let body = if c.block.kind == BlockKind::Text {
            format!(
                r#"<p class="ans__text">{}</p>"#,
                html_escape(c.free.as_deref().unwrap_or_default())
            )
        } else if let Some(free) = &c.free {
            format!(
                r#"<p class="ans__text">{}</p><p class="ans__note">Your own answer (Other).</p>"#,
                html_escape(free)
            )
        } else {
            let mut list = String::from(r#"<ul class="ans__list">"#);
            for o in &c.chosen {
                let _ = write!(
                    list,
                    r#"<li>{}<span>{}</span></li>"#,
                    icon("circle-check"),
                    opt_text(o)
                );
            }
            list.push_str("</ul>");
            if c.block.kind == BlockKind::Checkbox {
                let rest: Vec<String> = c
                    .block
                    .options
                    .iter()
                    .filter(|o| !c.chosen.iter().any(|x| x.id == o.id))
                    .map(|o| html::escape(&o.label))
                    .collect();
                if !rest.is_empty() {
                    let _ = write!(
                        list,
                        r#"<p class="ans__note">Not chosen: {}.</p>"#,
                        rest.join(", ")
                    );
                }
            }
            list
        };
        let _ = write!(out, r#"<div class="ans">{head}{body}</div>"#);
    }
    let at = i.answered_at.map(when).unwrap_or_default();
    format!(
        r#"<section class="sec2" aria-labelledby="ans-t"><div class="sec2__head"><h2 class="title" id="ans-t">Your answer{}</h2><span class="sec2__note">Sent to the agent at {at}. You cannot change {}.</span></div>{out}</section>"#,
        if many { "s" } else { "" },
        if many { "them" } else { "it" },
    )
}

/// The choices of a question that is closed or that the viewer cannot answer, as a list with no field.
fn options_list(t: &QText, i: &crate::inbox::page_items::Loaded) -> String {
    let rows = rows_of(i, &t.input);
    let Some(sheet) = Sheet::of(&t.input, &rows) else {
        return String::new();
    };
    let mut out = String::new();
    for bl in &sheet.blocks {
        if bl.kind == BlockKind::Text {
            continue;
        }
        let mut list = String::from(r#"<ul class="ans__list">"#);
        for o in &bl.options {
            let _ = write!(list, r#"<li><span>{}</span></li>"#, opt_text(o));
        }
        list.push_str("</ul>");
        let head = match (&bl.header, &bl.text) {
            (Some(h), Some(x)) => format!(
                r#"<div class="q__head"><span class="qchip">{}</span></div><h3 class="q__text">{}</h3>"#,
                html::escape(h),
                html::escape(x)
            ),
            _ => String::new(),
        };
        let _ = write!(out, r#"<div class="ans">{head}{list}</div>"#);
    }
    if out.is_empty() {
        return out;
    }
    format!(
        r#"<section class="sec2" aria-labelledby="opt-t"><div class="sec2__head"><h2 class="title" id="opt-t">Choices</h2><span class="sec2__note">Not open for an answer.</span></div>{out}</section>"#
    )
}

fn closed_banner(d: &QDetail) -> String {
    let i = &d.loaded.item;
    let (class, text) = match i.state {
        ItemState::Pending | ItemState::Replied | ItemState::Answered => return String::new(),
        ItemState::Abandoned => (
            "banner--warn",
            format!(
                "<b>The agent no longer answers.</b> Last sign {} ago. This is not a decline. You cannot answer now.",
                long_span(d.now_ms - i.agent_seen_at)
            ),
        ),
        ItemState::Superseded => {
            let link = d
                .next
                .as_deref()
                .map(|id| {
                    format!(
                        r#" <a href="{ITEMS}/{}">Open the new question</a>."#,
                        html_escape(id)
                    )
                })
                .unwrap_or_default();
            (
                "banner--info",
                format!("<b>The agent proposes something else.</b> This question is closed. A new question replaces it.{link}"),
            )
        }
        ItemState::Expired => (
            "banner--info",
            "<b>This question expired.</b> The time limit is reached. No decision was made. The question is closed."
                .to_string(),
        ),
        ItemState::Cancelled => (
            "banner--info",
            format!("<b>{}</b>", html_escape(&outcome(d.loaded, d.now_ms))),
        ),
    };
    format!(
        r#"<div class="banner {class}" role="status">{}<p>{text}</p></div>"#,
        icon("alert-triangle")
    )
}

fn discussion(d: &QDetail) -> String {
    if d.turns.is_empty() {
        return String::new();
    }
    let i = &d.loaded.item;
    let title = if i.state == ItemState::Answered {
        "You asked the agent before you answered"
    } else {
        "Discussion"
    };
    format!(
        r#"<section class="sec2" aria-labelledby="th-t"><div class="sec2__head"><h2 class="title" id="th-t">{title}</h2><span class="count">{} message{}</span></div><div class="thread">{}</div></section>"#,
        d.turns.len(),
        if d.turns.len() == 1 { "" } else { "s" },
        turns_html(&d.turns)
    )
}

fn side(d: &QDetail, t: Option<&QText>) -> String {
    let l = d.loaded;
    let i = &l.item;
    let mut req = format!(
        r#"<div><dt>Code</dt><dd><code>{}</code></dd></div><div><dt>Key</dt><dd>{}</dd></div>"#,
        html_escape(&code_of(l)),
        html::escape(&d.sender.agent.key_name)
    );
    if let Some(t) = t {
        let rows = rows_of(l, &t.input);
        let yes = |on: bool| if on { "Allowed" } else { "No" };
        let _ = write!(
            req,
            r#"<div><dt>Answers</dt><dd>{}</dd></div>"#,
            html_escape(&{
                let w = kind_words(&t.input);
                let mut c = w.chars();
                c.next()
                    .map(|f| f.to_uppercase().collect::<String>() + c.as_str())
                    .unwrap_or_default()
            }),
        );
        // An automatic system has neither row: the box below says so.
        if !automatic(l) {
            let _ = write!(
                req,
                r#"<div><dt>{OTHER_WORD}</dt><dd>{}</dd></div><div><dt>{ASK_WORD}</dt><dd>{}</dd></div>"#,
                yes(rows.other_answer),
                yes(rows.ask_agent),
            );
        }
    }
    let line = |class: &str, at: i64, text: &str| {
        format!(
            r#"<li class="{class}"><b>{}</b> {}</li>"#,
            when(at),
            html_escape(text)
        )
    };
    let mut tl = line("is-now", i.created_at, "Question asked");
    if let Some(at) = i.last_agent_turn_at {
        tl.push_str(&line("", at, "Last agent message"));
    } else if i.agent_seen_at > i.created_at {
        tl.push_str(&line(
            "",
            i.agent_seen_at,
            &format!("Agent active ({})", ago(d.now_ms, i.agent_seen_at)),
        ));
    }
    if let Some(at) = i.answered_at {
        tl.push_str(&line(
            "is-now",
            at,
            &format!("Answer: {}", word_of(l).to_lowercase()),
        ));
    }
    if let Some(at) = i.abandoned_at {
        tl.push_str(&line(
            "is-bad",
            at,
            "The agent no longer answered: abandoned",
        ));
    }
    if i.is_open() {
        if let Some(at) = i.lease_until {
            tl.push_str(&line("", at, "Lease ends"));
            tl.push_str(r#"<li>After that the item becomes <b>abandoned</b>.</li>"#);
        }
    }
    let who = if d.mode == Mode::Owner {
        "The agent reads your answer and your texts. No service reads them."
    } else {
        "The content is for the owner. An administrator reads metadata only."
    };
    let auto_box = if automatic(l) {
        r#"<div class="sidebox"><h3>Automatic system</h3><p>This key cannot read free text. It has no conversation. The choices are the only way to answer. The rows "Other answer" and "Ask the agent" never appear.</p></div>"#
    } else {
        ""
    };
    let about = about_sender(
        &d.sender,
        &i.requester_owner,
        &ago(d.now_ms, i.agent_seen_at),
    );
    format!(
        r#"<aside class="detail__side">{about}<div class="sidebox"><h3>This question</h3><dl class="facts">{req}</dl></div>{auto_box}<div class="sidebox"><h3>Time line</h3><ul class="timeline">{tl}</ul></div><div class="sidebox"><h3>Who sees what</h3><p>{who}</p></div></aside>"#
    )
}

/// The page of one question.
pub fn question_page(frame: &Frame, d: &QDetail, notice: Option<Notice>) -> String {
    let l = d.loaded;
    let i = &l.item;
    let code = code_of(l);
    let mut out = String::new();
    notice_html(&mut out, notice);
    let stale = i.is_open() && d.now_ms - i.created_at > d.stale_after_ms;
    let title = match &d.body {
        QBody::Shown(t) => html::escape(&t.title),
        _ => format!("Question {}", html_escape(&code)),
    };
    let mut main = head(d, &title, stale);
    let owner_open = d.mode == Mode::Owner && i.is_open();
    if stale && owner_open {
        let since = match &d.body {
            QBody::Shown(t) if t.as_of_ms.is_some() => format!(
                "The agent gave its data as of {}.",
                when(t.as_of_ms.unwrap_or_default())
            ),
            _ => format!("The question was asked {}.", when(i.created_at)),
        };
        let _ = write!(
            main,
            r#"<div class="banner banner--warn" role="alert">{}<p><b>This question is old.</b> The context may have changed. {since}</p></div>"#,
            icon("alert-triangle"),
        );
    }
    main.push_str(&closed_banner(d));
    match &d.body {
        QBody::Shown(t) => {
            if d.mode == Mode::AdminBody {
                let _ = write!(
                    main,
                    r#"<div class="banner banner--info">{}<p>You read this content as the administrator. The server wrote one line in the audit. The line has no content.</p></div>"#,
                    icon("shield-check")
                );
            }
            if let Some(c) = &t.command {
                main.push_str(&command_zone(d, c));
            }
            // A question has no explanation zone when the agent wrote none.
            if t.explanation.is_some() {
                main.push_str(&explanation_block(
                    t.explanation.as_deref(),
                    t.format,
                    d.full,
                    &detail_href(l),
                ));
            }
            main.push_str(&discussion(d));
            if owner_open {
                main.push_str(&answer_form(d, t));
            } else {
                main.push_str(&answer_view(d, t));
                if i.state != ItemState::Answered && d.mode != Mode::Admin {
                    main.push_str(&options_list(t, l));
                }
            }
        }
        other => {
            let text = match other {
                QBody::Purged => "The content of this question is deleted. The server keeps the metadata.",
                QBody::Mismatch => "The server cannot show this content: the seal does not open, or the body is not a question.",
                _ => "You read the metadata only. The content of a question is for its owner.",
            };
            let _ = write!(
                main,
                r#"<div class="banner banner--info">{}<p>{text}</p></div>"#,
                icon("shield-check")
            );
            if d.body_link {
                let _ = write!(
                    main,
                    r#"<p><a class="button button--small" href="{}/body">Read the content</a> <span class="note">The server writes one line in the audit.</span></p>"#,
                    html_escape(&detail_href(l))
                );
            }
        }
    }
    if d.can_cancel && i.is_open() {
        let _ = write!(
            main,
            r#"<form class="decide" method="post" action="{}"><div class="decide__body"><input type="hidden" name="csrf" value="{}"><input type="hidden" name="request_id" value="{}"><div class="decide__row"><button class="button" type="submit">{}Cancel this question</button><span class="note">The agent sees a closed question. This is not a refusal.</span></div></div></form>"#,
            super::items::CANCEL,
            html_escape(d.csrf),
            html_escape(&i.request_id),
            icon("ban"),
        );
    }
    let (back, back_word) = if i.is_open() {
        (super::inbox::PAGE, "Inbox")
    } else {
        (ITEMS, "History")
    };
    let word = if automatic(l) { "Decision" } else { "Question" };
    let t = match &d.body {
        QBody::Shown(t) => Some(t),
        _ => None,
    };
    let _ = write!(
        out,
        r#"<div class="page page--narrow"><nav class="crumbs" aria-label="Path"><a href="{back}">{}{back_word}</a><span>/</span><b>{word} {}</b></nav><div class="detail"><div class="detail__main">{main}</div>{}</div></div>"#,
        icon("arrow-left"),
        html_escape(&code),
        side(d, t.map(|v| &**v)),
    );
    fill_page(Tab::Inbox, frame, &format!("Inbox: question {code}"), &out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::inbox::agents::{AgentKind, AgentState};
    use crate::inbox::page_items::ItemRow;
    use crate::ui::agents::AgentView;
    use crate::ui::questions::BlockDraft;
    use inbox_core::proto::{FormAnswer, Kind, Urgency, Via};

    const NOW: i64 = 1_800_000_000_000;

    fn frame() -> Frame<'static> {
        Frame {
            logout_token: "out",
            keys: true,
            inbox: true,
            superadmin: false,
            notify: None,
            security: false,
            tab: crate::ui::TabLook::DEFAULT,
        }
    }

    fn item(state: ItemState) -> Loaded {
        Loaded {
            item: ItemRow {
                request_id: "0123456789abcdef0123456789abcdef".into(),
                kind: Kind::Question,
                chain_id: "c".into(),
                supersedes: None,
                superseded_by: None,
                previous_id: None,
                requester_key_id: "k1".into(),
                requester_owner: "alice".into(),
                agent_id: "a1".into(),
                via: Via::Mcp,
                interaction: Interaction::Conversational,
                urgency: Urgency::Normal,
                input_type: Some(InputType::Choice),
                state,
                settle_reason: None,
                body_kid: None,
                body_size: 0,
                body_purged_at: None,
                created_at: NOW - 60_000,
                chain_created_at: NOW - 60_000,
                lease_s: Some(60),
                lease_until: Some(NOW + 180_000),
                until_at: None,
                agent_seen_at: NOW - 30_000,
                last_agent_turn_at: None,
                abandoned_at: None,
                revivals: 0,
                last_seq: 0,
                allow_custom: false,
                allow_ask_back: false,
                answer_action: None,
                answered_at: None,
                answered_by: None,
                answered_via: None,
                final_at: None,
                idempotency_key: "i".into(),
            },
            approval: None,
        }
    }

    fn sender(lock: bool, state: AgentState) -> Sender {
        Sender {
            agent: AgentView {
                agent_id: "a1".into(),
                label: "ops-agent #1".into(),
                key_name: "ops-agent".into(),
                kind: AgentKind::Agent,
                state,
                named: false,
                created_at: NOW - 86_400_000,
                last_seen_at: NOW - 60_000,
                observed: None,
                plain: false,
            },
            lookalike: None,
            owner: true,
            lock,
            also: Vec::new(),
        }
    }

    fn input() -> Input {
        Input {
            kind: InputType::Choice,
            multiple: false,
            min: 1,
            max: 1,
            options: ["go", "wait"]
                .iter()
                .map(|id| InputOption {
                    id: (*id).into(),
                    label: id.to_uppercase(),
                    description: None,
                })
                .collect(),
            placeholder: None,
            questions: None,
        }
    }

    fn text(input: Input) -> QText {
        QText {
            title: "Deploy <b>now</b>?".into(),
            explanation: Some("Because.".into()),
            format: ExplanationFormat::Plain,
            command: None,
            input,
            hint: None,
            as_of_ms: None,
        }
    }

    fn render(l: &Loaded, t: QText, sender: Sender, f: impl FnOnce(&mut QDetail)) -> String {
        render_draft(l, t, sender, None, f)
    }

    fn render_draft(
        l: &Loaded,
        t: QText,
        sender: Sender,
        draft: Option<&Draft>,
        f: impl FnOnce(&mut QDetail),
    ) -> String {
        let mut d = QDetail {
            loaded: l,
            body: QBody::Shown(Box::new(t)),
            sender,
            turns: Vec::new(),
            answer: None,
            now_ms: NOW,
            stale_after_ms: 86_400_000,
            csrf: "tok",
            mode: Mode::Owner,
            can_cancel: false,
            body_link: false,
            full: false,
            draft,
            next: None,
        };
        f(&mut d);
        question_page(&frame(), &d, None)
    }

    fn open_page(l: &Loaded, t: QText) -> String {
        render(l, t, sender(false, AgentState::Known), |_| {})
    }

    #[test]
    fn the_title_is_escaped_and_no_option_is_ticked() {
        let l = item(ItemState::Pending);
        let page = open_page(&l, text(input()));
        assert!(page.contains("Deploy &lt;b&gt;now&lt;/b&gt;?") && !page.contains("<b>now</b>"));
        assert!(!page.contains("checked>"));
        assert!(page.contains("value=\"o:go\"") && page.contains("value=\"o:wait\""));
        assert!(page.contains("name=\"action\" value=\"accept\""));
        assert!(page.contains("name=\"action\" value=\"decline\""));
    }

    #[test]
    fn a_new_agent_with_the_lock_turns_send_off_and_keeps_decline_and_ask() {
        let mut l = item(ItemState::Pending);
        l.item.allow_ask_back = true;
        let page = render(&l, text(input()), sender(true, AgentState::New), |_| {});
        assert!(page.contains(LOCK_LINE) && page.contains("button--off"));
        assert!(!page.contains("name=\"action\" value=\"accept\""));
        assert!(page.contains("name=\"action\" value=\"decline\""));
        assert!(page.contains("form=\"qask\""));
    }

    #[test]
    fn a_mistake_of_a_block_is_shown_and_the_typed_text_is_kept_not_marked() {
        let mut l = item(ItemState::Pending);
        l.item.allow_custom = true;
        let draft = Draft {
            blocks: vec![BlockDraft {
                other_on: true,
                other_text: "a\u{200B}b <i>".into(),
                error: Some("Type your answer. An empty text is not an answer.".into()),
                ..BlockDraft::default()
            }],
            ..Draft::default()
        };
        let page = render_draft(
            &l,
            text(input()),
            sender(false, AgentState::Known),
            Some(&draft),
            |_| {},
        );
        assert!(page.contains("An empty text is not an answer."));
        assert!(
            page.contains("a\u{200B}b &lt;i&gt;"),
            "the owner text is not changed"
        );
        assert!(page.contains("value=\"x\" checked>"));
    }

    #[test]
    fn the_closed_states_have_their_words_and_no_field() {
        for (state, words) in [
            (ItemState::Abandoned, "This is not a decline."),
            (ItemState::Superseded, "The agent proposes something else."),
            (ItemState::Expired, "This question expired."),
            (ItemState::Cancelled, "Cancelled."),
        ] {
            let l = item(state);
            let page = render(&l, text(input()), sender(false, AgentState::Known), |d| {
                d.next = Some("f".repeat(32))
            });
            assert!(page.contains(words), "{state:?}");
            assert!(!page.contains("name=\"action\""), "{state:?}");
            assert!(page.contains("Not open for an answer."), "{state:?}");
            assert_eq!(
                page.contains(&format!("{ITEMS}/{}", "f".repeat(32))),
                state == ItemState::Superseded
            );
        }
    }

    #[test]
    fn an_answered_form_shows_each_block_the_free_text_and_what_was_not_chosen() {
        let mut l = item(ItemState::Answered);
        l.item.allow_custom = true;
        l.item.answer_action = Some(Action::Accept);
        l.item.answered_at = Some(NOW - 1000);
        let mut multi = input();
        multi.multiple = true;
        multi.max = 2;
        let form = Input {
            kind: InputType::Form,
            multiple: false,
            min: 0,
            max: 0,
            options: vec![],
            placeholder: None,
            questions: Some(vec![
                inbox_core::proto::FormQuestion {
                    id: "one".into(),
                    header: "Tables".into(),
                    text: "Which?".into(),
                    kind: FormQuestionType::Choice,
                    multiple: true,
                    min: 1,
                    max: 2,
                    options: multi.options.clone(),
                    placeholder: None,
                },
                inbox_core::proto::FormQuestion {
                    id: "two".into(),
                    header: "Window".into(),
                    text: "When?".into(),
                    kind: FormQuestionType::Text,
                    multiple: false,
                    min: 0,
                    max: 0,
                    options: vec![],
                    placeholder: None,
                },
            ]),
        };
        let answer = OwnerAnswer {
            answers: Some(vec![
                FormAnswer {
                    question_id: "one".into(),
                    choice: Some(vec!["go".into()]),
                    text: None,
                },
                FormAnswer {
                    question_id: "two".into(),
                    choice: None,
                    text: Some("every <b>night</b>".into()),
                },
            ]),
            ..OwnerAnswer::default()
        };
        let page = render(&l, text(form), sender(false, AgentState::Known), |d| {
            d.answer = Some(answer)
        });
        assert!(page.contains("Your answers") && page.contains("You cannot change them."));
        assert!(page.contains("Not chosen: WAIT."));
        assert!(page.contains("every &lt;b&gt;night&lt;/b&gt;"));
        assert!(!page.contains("name=\"action\""));
        assert!(page.contains("pill--ok"));
    }

    #[test]
    fn a_stale_question_has_a_banner_and_no_checkbox() {
        let l = item(ItemState::Pending);
        let page = render(&l, text(input()), sender(false, AgentState::Known), |d| {
            d.stale_after_ms = 1000
        });
        assert!(page.contains("This question is old.") && page.contains("Old question"));
        assert!(!page.contains("stale_ack") && !page.contains("type=\"checkbox\""));
    }

    #[test]
    fn a_viewer_that_is_not_the_owner_reads_no_content() {
        let l = item(ItemState::Pending);
        let mut d = QDetail {
            loaded: &l,
            body: QBody::Hidden,
            sender: sender(false, AgentState::Known),
            turns: Vec::new(),
            answer: None,
            now_ms: NOW,
            stale_after_ms: i64::MAX,
            csrf: "tok",
            mode: Mode::Admin,
            can_cancel: false,
            body_link: true,
            full: false,
            draft: None,
            next: None,
        };
        let page = question_page(&frame(), &d, None);
        assert!(page.contains("You read the metadata only.") && page.contains("Read the content"));
        assert!(!page.contains(QUESTION) && !page.contains("name=\"action\""));
        d.body_link = false;
        assert!(!question_page(&frame(), &d, None).contains("Read the content"));
    }

    #[test]
    fn the_words_of_a_row_say_the_kind_of_answer_and_the_discussion() {
        let mut l = item(ItemState::Pending);
        assert_eq!(row_note(&l, &input()), "2 choices");
        l.item.last_seq = 3;
        assert_eq!(
            row_note(&l, &input()),
            "2 choices · open discussion, 3 messages"
        );
        l.item.interaction = Interaction::OneShot;
        assert_eq!(
            row_note(&l, &input()),
            "2 choices · automatic system, no chat"
        );
    }
}
