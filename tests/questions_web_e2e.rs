//! End-to-end tests of the web pages of the questions (lot L14): the panel "Open items", the page of one question, the
//! forms that answer, decline and ask the agent, the History and the route of the administrator. The rig is the rig of the
//! questions (L13): the agent asks through `/mcp`, and the owner uses the pages with a session. The pages have no script,
//! so each check reads the HTML. Run the tests on PostgreSQL with `INBOX_E2E_BACKEND=postgres`.

mod common;

use common::approvals::{hidden_in, item_path, Pep, INBOX, OWNER};
use common::{http_full, users_csrf, Server};
use serde_json::{json, Value};

const QUESTION: &str = "/auth/settings/inbox/items/question";
const REPLY: &str = "/auth/settings/inbox/items/reply";
const CANCEL: &str = "/auth/settings/inbox/items/cancel";
const ALLOW: &str = "/auth/settings/inbox/agents/allow";
const SECRET: &str = "Do not enter a password or a secret";

fn rig() -> Pep {
    Pep::with_questions(|_| {})
}

fn options() -> Value {
    json!([
        {"id": "go", "label": "Deploy now", "description": "Prod, 3 pods"},
        {"id": "wait", "label": "Wait until tomorrow"},
        {"id": "stage", "label": "Deploy to staging first"}
    ])
}

fn choice(extra: Value) -> Value {
    let mut args = json!({"title": "Deploy v5.1 to prod?", "lease_s": 900,
        "input": {"type": "choice", "options": options()}});
    for (k, v) in extra.as_object().unwrap() {
        args[k] = v.clone();
    }
    args
}

fn flags(mut args: Value, custom: bool, back: bool) -> Value {
    args["input"]["allow_custom"] = json!(custom);
    args["input"]["allow_ask_back"] = json!(back);
    args
}

fn ask(pep: &Pep, args: Value) -> String {
    let made = pep.tool("ask", args);
    assert_eq!(made["status"], "pending", "{made}");
    made["request_id"].as_str().unwrap().to_string()
}

fn answer_of(pep: &Pep, id: &str) -> Value {
    pep.tool("get_answer", json!({"request_id": id}))
}

fn csrf(pep: &Pep, cookie: &str, id: &str) -> String {
    let (_, _, page) = pep.page(cookie, &item_path(id));
    hidden_in(&page, QUESTION, "csrf")
}

/// The answer form of the page, as the owner uses it: the token comes from the page, `fields` are the fields of the blocks.
fn send(pep: &Pep, cookie: &str, id: &str, action: &str, fields: &str) -> (u16, String, String) {
    let token = csrf(pep, cookie, id);
    let mut body = format!("request_id={id}&action={action}");
    if !fields.is_empty() {
        body.push('&');
        body.push_str(fields);
    }
    pep.post(cookie, QUESTION, &token, &body, None)
}

fn detail(pep: &Pep, cookie: &str, id: &str) -> String {
    let (st, _, page) = pep.page(cookie, &item_path(id));
    assert_eq!(st, 200, "{page}");
    page
}

fn text_of(page: &str) -> String {
    Pep::text_of(page)
}

/// The number of rows "Other answer" in a page: one for each choice block that has the row.
fn other_rows(page: &str) -> usize {
    page.matches("class=\"opt is-other").count()
}

/// Whether the page has the row "Ask the agent": its field belongs to the form `qask`.
fn has_ask_row(page: &str) -> bool {
    page.contains("form=\"qask\"")
}

fn form_input(extra_questions: bool) -> Value {
    let mut questions = vec![
        json!({"id": "tables", "header": "Tables", "text": "Which tables should I archive?", "type": "choice",
               "multiple": true, "min": 1, "max": 3,
               "options": [{"id": "ev", "label": "events_2025", "description": "212 GB"},
                           {"id": "rf", "label": "refunds_2024"}, {"id": "ss", "label": "sessions_old"}]}),
        json!({"id": "keep", "header": "Retention", "text": "How long should the archive keep the data?",
               "type": "choice", "options": [{"id": "d30", "label": "30 days"}, {"id": "d90", "label": "90 days"}]}),
        json!({"id": "when", "header": "Run window", "text": "When may the job run?", "type": "text",
               "placeholder": "For example: every night"}),
    ];
    if extra_questions {
        questions.push(
            json!({"id": "note", "header": "Note", "text": "Anything else?", "type": "text"}),
        );
    }
    json!({"type": "form", "questions": questions})
}

fn form(extra: Value) -> Value {
    let mut args = json!({"title": "3 questions about the archive", "lease_s": 900, "input": form_input(false)});
    for (k, v) in extra.as_object().unwrap() {
        args[k] = v.clone();
    }
    args
}

// The matrix ---------------------------------------------------------------------------------------------

/// DIAL 1.4.2: the rows under the options come from the two flags, for every type of input and both kinds of key. The
/// server refuses a combination that the protocol forbids, so 17 of the 40 combinations make a page, as on the phone.
#[test]
fn rows_follow_flags_matrix_web() {
    let pep = rig();
    let cookie = pep.cookie(OWNER);
    let (_, shot) = pep.make_question_key("jev", "one_shot");
    let inputs = [
        ("choice", json!({"type": "choice", "options": options()})),
        (
            "multiple",
            json!({"type": "choice", "multiple": true, "min": 1, "max": 2, "options": options()}),
        ),
        ("confirm", json!({"type": "confirm"})),
        ("text", json!({"type": "text"})),
        ("none", json!({"type": "none"})),
        ("form", form_input(false)),
    ];
    let mut valid = 0;
    for (name, input) in &inputs {
        for custom in [false, true] {
            for back in [false, true] {
                for one_shot in [false, true] {
                    // The "multiple" choice is the same type as "choice": count it once in the total.
                    let mut args = json!({"title": "A question", "lease_s": 900, "input": input});
                    args["input"]["allow_custom"] = json!(custom);
                    args["input"]["allow_ask_back"] = json!(back);
                    let (is_error, made) = if one_shot {
                        pep.tool_with_key(&shot, "ask", args)
                    } else {
                        let id_key = pep.key_text.clone();
                        pep.tool_with_key(&id_key, "ask", args)
                    };
                    let case = format!("{name} custom={custom} back={back} one_shot={one_shot}");
                    let accepted = !is_error;
                    let expected = match (*name, custom, back, one_shot) {
                        (_, true, _, true) | (_, _, true, true) => false,
                        (n, _, _, true) => matches!(n, "choice" | "multiple" | "confirm"),
                        ("text", true, _, _) => false,
                        ("none", true, _, _) | ("none", _, true, _) => false,
                        _ => true,
                    };
                    assert_eq!(accepted, expected, "{case}: {made}");
                    if !accepted {
                        continue;
                    }
                    if *name != "multiple" {
                        valid += 1;
                    }
                    let id = made["request_id"].as_str().unwrap().to_string();
                    let page = detail(&pep, &cookie, &id);
                    let blocks = if *name == "form" { 2 } else { 1 };
                    let other =
                        if matches!(*name, "choice" | "multiple" | "confirm" | "form") && custom {
                            blocks
                        } else {
                            0
                        };
                    assert_eq!(other_rows(&page), other, "{case}: other rows");
                    assert_eq!(has_ask_row(&page), back && !one_shot, "{case}: ask row");
                    // The words of the escape rows appear only with the rows.
                    assert_eq!(
                        page.contains("This is not an answer. The question stays open"),
                        back && !one_shot,
                        "{case}"
                    );
                    // A message with no answer has no Send button and no field. Decline is always there.
                    assert!(page.contains("value=\"decline\""), "{case}");
                    assert_eq!(
                        page.contains("name=\"action\" value=\"accept\""),
                        *name != "none",
                        "{case}"
                    );
                    pep.tool_with_key(
                        if one_shot { &shot } else { &pep.key_text },
                        "cancel",
                        json!({"request_id": id}),
                    );
                }
            }
        }
    }
    assert_eq!(valid, 17, "17 of the 40 combinations are valid");
}

#[test]
fn the_page_of_a_choice_has_the_boxes_the_zones_and_no_tick() {
    let pep = rig();
    let cookie = pep.cookie(OWNER);
    let id = ask(
        &pep,
        flags(
            choice(
                json!({"explanation": "It frees 40 GB.", "command": "kubectl -n prod set image deploy/api",
                          "command_label": "Shell", "language": "bash"}),
            ),
            true,
            true,
        ),
    );
    let page = detail(&pep, &cookie, &id);
    // The box "From agent", then the command, then the explanation, then the form.
    let at = |needle: &str| page.find(needle).unwrap_or_else(|| panic!("no {needle}"));
    assert!(at("From agent") < at("Command"));
    assert!(at("Command") < at("Agent explanation"));
    assert!(at("Agent explanation") < at("Your answer"));
    assert!(page.contains("Exact text") && page.contains("Not checked"));
    assert!(page.contains("data-copy-text=") && page.contains(" hidden>"));
    assert!(page.contains("Deploy now") && page.contains("Prod, 3 pods"));
    // Nothing is ticked, and the page has no script.
    assert!(!page.contains(" checked"), "no option is ticked");
    assert!(!page.contains("<script>") && !page.contains(" onclick="));
    assert!(page.contains(&format!("method=\"post\" action=\"{QUESTION}\"")));
    assert!(page.contains(&format!("action=\"{REPLY}\"")));
    assert!(page.contains("Decline to answer") && page.contains("Send answer"));
    let (_, head, _) = pep.page(&cookie, &item_path(&id));
    assert_eq!(
        common::approvals::header_of(&head, "cache-control").as_deref(),
        Some("no-store")
    );
}

#[test]
fn one_shot_page_has_no_chat_controls() {
    let pep = rig();
    let cookie = pep.cookie(OWNER);
    let (_, shot) = pep.make_question_key("jev", "one_shot");
    let (is_error, made) = pep.tool_with_key(
        &shot,
        "ask",
        json!({"title": "Which team handles \"double debit\"?", "lease_s": 900,
               "input": {"type": "choice", "options": [
                   {"id": "billing", "label": "billing", "description": "Charges and refunds"},
                   {"id": "support", "label": "support"}, {"id": "infra", "label": "infra"}]},
               "hint": {"suggested": "billing", "confidence": 0.62, "source": "jev"}}),
    );
    assert!(!is_error, "{made}");
    let id = made["request_id"].as_str().unwrap().to_string();
    let page = detail(&pep, &cookie, &id);
    let text = text_of(&page);
    assert!(
        text.contains("Automatic system (no conversation)"),
        "{text}"
    );
    assert!(text.contains("Last computed") && !text.contains("Agent active"));
    assert!(text.contains("No free text. No discussion."));
    assert!(page.contains("kind kind--auto"));
    // No chat control, no free field, no second form, no row.
    assert!(!has_ask_row(&page) && !page.contains(&format!("action=\"{REPLY}\"")));
    assert!(!page.contains("<textarea") && !page.contains("type=\"text\""));
    assert_eq!(other_rows(&page), 0);
    assert!(!page.contains("Ask the agent</b>") && !page.contains(">Other answer<"));
    assert!(!page.contains("Discussion"));
    // The server refuses a reply that a forged form sends.
    let token = csrf(&pep, &cookie, &id);
    let (st, head, _) = pep.post(
        &cookie,
        REPLY,
        &token,
        &format!("request_id={id}&text=why"),
        None,
    );
    assert_eq!(st, 303);
    assert!(common::location(&head).ends_with("?s=no_reply"), "{head}");
    // A forged free text is refused too, and the item stays open.
    let (st, _, body) = send(&pep, &cookie, &id, "accept", "b0=x&b0t=my+own");
    assert_eq!(st, 422, "{body}");
    let (_, got) = pep.tool_with_key(&shot, "get_answer", json!({"request_id": id}));
    assert_eq!(got["status"], "pending");
}

#[test]
fn hint_shown_above_options() {
    let pep = rig();
    let cookie = pep.cookie(OWNER);
    let id = ask(
        &pep,
        choice(json!({"hint": {"suggested": "wait", "confidence": 0.62, "source": "agent"}})),
    );
    let page = detail(&pep, &cookie, &id);
    let hint = page
        .find("Suggestion (not checked): Wait until tomorrow")
        .expect("the hint");
    let first_option = page.find("class=\"opt").expect("the options");
    assert!(hint < first_option, "the hint is above the options");
    assert!(page.contains("Confidence 62 %"), "{page}");
    assert!(page.contains("<meter"));
    // The hint never ticks an option.
    assert!(!page.contains(" checked"));
    // A hint with no confidence, and a hint that is a text and not an option.
    let id = ask(
        &pep,
        choice(
            json!({"hint": {"suggested": "after the freeze", "source": "agent"},
                      "idempotency_key": "two"}),
        ),
    );
    let page = detail(&pep, &cookie, &id);
    assert!(page.contains("Suggestion (not checked): after the freeze"));
    assert!(!page.contains("Confidence"));
}

#[test]
fn secret_warning_present() {
    let pep = rig();
    let cookie = pep.cookie(OWNER);
    // The row "Other answer", the box "Ask the agent" and a text question each carry the warning.
    let both = ask(&pep, flags(choice(json!({})), true, true));
    let page = detail(&pep, &cookie, &both);
    assert!(text_of(&page).matches(SECRET).count() >= 2, "{page}");
    let text = ask(
        &pep,
        json!({"title": "When?", "lease_s": 900, "input": {"type": "text", "placeholder": "Every night"}}),
    );
    let page = detail(&pep, &cookie, &text);
    assert!(text_of(&page).contains(SECRET));
    assert!(page.contains("placeholder=\"Every night\""));
    // A closed question has no free field and so no warning.
    let closed = ask(&pep, choice(json!({"idempotency_key": "closed"})));
    assert!(!text_of(&detail(&pep, &cookie, &closed)).contains(SECRET));
    // A form with a text block and a choice block with the row "Other".
    let f = ask(&pep, flags(form(json!({})), true, false));
    assert!(text_of(&detail(&pep, &cookie, &f)).matches(SECRET).count() >= 3);
}

// The forms -------------------------------------------------------------------------------------------

#[test]
fn text_answer_post_requires_csrf_origin() {
    let pep = rig();
    let cookie = pep.cookie(OWNER);
    let id = ask(
        &pep,
        json!({"title": "When may I run it?", "lease_s": 900, "input": {"type": "text"}}),
    );
    let token = csrf(&pep, &cookie, &id);
    let fields = format!("request_id={id}&action=accept&b0t=tonight");
    // No token, a wrong token, no origin and another origin: refused, and nothing changes.
    assert_eq!(pep.post(&cookie, QUESTION, "", &fields, None).0, 403);
    assert_eq!(
        pep.post(&cookie, QUESTION, "not-the-token", &fields, None)
            .0,
        403
    );
    assert_eq!(
        pep.post(&cookie, QUESTION, &token, &fields, Some("")).0,
        403
    );
    assert_eq!(
        pep.post(
            &cookie,
            QUESTION,
            &token,
            &fields,
            Some("http://evil.example")
        )
        .0,
        403
    );
    // The token of another session does not fit. A GET on the form route is refused.
    assert_eq!(pep.page(&cookie, QUESTION).0, 405);
    assert_eq!(answer_of(&pep, &id)["status"], "pending");
    // The right token and the right origin work.
    let (st, head, _) = pep.post(&cookie, QUESTION, &token, &fields, None);
    assert_eq!(st, 303, "{head}");
    assert!(common::location(&head).ends_with("?s=q_answered"), "{head}");
    let got = answer_of(&pep, &id);
    assert_eq!(got["status"], "answered");
    assert_eq!(got["answer"]["untrusted"]["text"], "tonight");
    assert!(got["answer"]["by"].is_string());
    // The page says what happened.
    let (_, _, page) = pep.page(&cookie, &item_path(&id));
    assert!(text_of(&page).contains("tonight"));
    // An unknown field is refused.
    let id2 = ask(
        &pep,
        json!({"title": "When?", "lease_s": 900, "idempotency_key": "x", "input": {"type": "text"}}),
    );
    let token = csrf(&pep, &cookie, &id2);
    let (st, _, _) = pep.post(
        &cookie,
        QUESTION,
        &token,
        &format!("request_id={id2}&action=accept&b0t=a&extra=1"),
        None,
    );
    assert_eq!(st, 400);
    assert_eq!(answer_of(&pep, &id2)["status"], "pending");
}

#[test]
fn a_choice_a_confirm_a_multiple_choice_and_a_custom_text_reach_the_agent() {
    let pep = rig();
    let cookie = pep.cookie(OWNER);
    // One option of a radio.
    let id = ask(&pep, flags(choice(json!({})), true, false));
    let (st, head, body) = send(&pep, &cookie, &id, "accept", "b0=o%3Await");
    assert_eq!(st, 303, "{body}");
    assert!(common::location(&head).ends_with("?s=q_answered"));
    let got = answer_of(&pep, &id);
    assert_eq!(got["answer"]["choice"], json!(["wait"]));
    // The answered page shows what was chosen and that it cannot change.
    let page = detail(&pep, &cookie, &id);
    assert!(page.contains("Your answer") && page.contains("You cannot change it."));
    assert!(text_of(&page).contains("Wait until tomorrow") && page.contains("pill--ok"));
    assert!(!page.contains("name=\"action\" value=\"accept\""));
    // A free text, with the radio "Other answer" and with the text alone.
    let id = ask(
        &pep,
        flags(choice(json!({"idempotency_key": "c"})), true, false),
    );
    assert_eq!(
        send(&pep, &cookie, &id, "accept", "b0=x&b0t=not+before+18h").0,
        303
    );
    let got = answer_of(&pep, &id);
    assert_eq!(got["answer"]["choice"], json!([]));
    assert_eq!(got["answer"]["untrusted"]["text"], "not before 18h");
    let page = detail(&pep, &cookie, &id);
    assert!(text_of(&page).contains("not before 18h") && page.contains("Your own answer (Other)."));
    // A confirm.
    let id = ask(
        &pep,
        json!({"title": "Go?", "lease_s": 900, "input": {"type": "confirm"}}),
    );
    assert_eq!(send(&pep, &cookie, &id, "accept", "b0=o%3Ayes").0, 303);
    assert_eq!(answer_of(&pep, &id)["answer"]["choice"], json!(["yes"]));
    // A multiple choice: the ticks go in the order of the options.
    let id = ask(
        &pep,
        json!({"title": "Which?", "lease_s": 900, "input": {"type": "choice", "multiple": true,
               "min": 1, "max": 2, "options": options()}}),
    );
    let (st, _, body) = send(&pep, &cookie, &id, "accept", "b0o_stage=1&b0o_go=1");
    assert_eq!(st, 303, "{body}");
    assert_eq!(
        answer_of(&pep, &id)["answer"]["choice"],
        json!(["go", "stage"])
    );
    let page = detail(&pep, &cookie, &id);
    assert!(text_of(&page).contains("Not chosen: Wait until tomorrow."));
}

#[test]
fn a_wrong_choice_gets_the_page_again_with_the_ticks_and_the_text_that_were_typed() {
    let pep = rig();
    let cookie = pep.cookie(OWNER);
    let id = ask(
        &pep,
        json!({"title": "Which?", "lease_s": 900, "input": {"type": "choice", "multiple": true,
               "min": 2, "max": 2, "allow_custom": true, "options": options()}}),
    );
    // One tick of two: the mistake is named, and the tick stays.
    let (st, _, body) = send(&pep, &cookie, &id, "accept", "b0o_go=1");
    assert_eq!(st, 422, "{body}");
    assert!(body.contains("Choose 2."), "{body}");
    assert!(
        body.contains("name=\"b0o_go\" value=\"1\" checked"),
        "{body}"
    );
    // The ticks and a text together are two answers: refused, and both are kept.
    let (st, _, body) = send(
        &pep,
        &cookie,
        &id,
        "accept",
        "b0o_go=1&b0o_wait=1&b0x=1&b0t=mine+%3Cb%3E",
    );
    assert_eq!(st, 422);
    assert!(body.contains("replaces the choices"), "{body}");
    assert!(
        body.contains("mine &lt;b&gt;") && !body.contains("mine <b>"),
        "{body}"
    );
    assert_eq!(answer_of(&pep, &id)["status"], "pending");
}

#[test]
fn a_text_over_two_thousand_characters_is_refused_with_a_visible_error_and_the_text_is_kept() {
    let pep = rig();
    let cookie = pep.cookie(OWNER);
    let id = ask(
        &pep,
        json!({"title": "Explain", "lease_s": 900, "input": {"type": "text"}}),
    );
    let long = "é".repeat(2001);
    let (st, _, body) = send(&pep, &cookie, &id, "accept", &format!("b0t={long}"));
    assert_eq!(st, 422, "{}", &body[..body.len().min(500)]);
    assert!(body.contains("The text has 2001 characters. The limit is 2000."));
    // The text is in the field again, not cut and not changed.
    assert!(body.contains(&long), "the typed text is kept");
    assert_eq!(answer_of(&pep, &id)["status"], "pending");
    // 2000 characters are an answer.
    let exact = "é".repeat(2000);
    assert_eq!(
        send(&pep, &cookie, &id, "accept", &format!("b0t={exact}")).0,
        303
    );
    assert_eq!(
        answer_of(&pep, &id)["answer"]["untrusted"]["text"]
            .as_str()
            .unwrap()
            .chars()
            .count(),
        2000
    );
    // A text of only spaces is no answer.
    let id = ask(
        &pep,
        json!({"title": "Explain", "lease_s": 900, "idempotency_key": "b", "input": {"type": "text"}}),
    );
    let (st, _, body) = send(&pep, &cookie, &id, "accept", "b0t=+++");
    assert_eq!(st, 422);
    assert!(body.contains("An empty text is not an answer."));
    // A too long question to the agent keeps its text too.
    let id = ask(
        &pep,
        flags(choice(json!({"idempotency_key": "ask"})), false, true),
    );
    let token = csrf(&pep, &cookie, &id);
    let (st, _, body) = pep.post(
        &cookie,
        REPLY,
        &token,
        &format!("request_id={id}&text={long}"),
        None,
    );
    assert_eq!(st, 422);
    assert!(
        body.contains("The text has 2001 characters. The limit is 2000.") && body.contains(&long)
    );
}

#[test]
fn a_form_has_blocks_a_counter_per_question_and_an_answered_view() {
    let pep = rig();
    let cookie = pep.cookie(OWNER);
    let id = ask(&pep, flags(form(json!({})), true, true));
    let page = detail(&pep, &cookie, &id);
    // Three blocks with a number, a chip, the question and the rule of the count.
    assert_eq!(page.matches("class=\"q\"").count(), 3, "{page}");
    assert!(page.contains("class=\"qchip\">Tables<") && page.contains("Choose 1 to 3."));
    assert!(page.contains("Choose one.") && page.contains("Write a text."));
    assert!(
        page.contains("Send answers") && page.contains("All answers go to the agent together.")
    );
    assert_eq!(
        other_rows(&page),
        2,
        "the row Other is in each choice block"
    );
    assert!(has_ask_row(&page));
    assert!(page.contains("placeholder=\"For example: every night\""));
    // An incomplete form is refused: the blocks that have no answer are named, and the other entries stay.
    let (st, _, body) = send(&pep, &cookie, &id, "accept", "b0o_ev=1&b0o_ss=1&b1=o%3Ad90");
    assert_eq!(st, 422);
    assert!(body.contains("Type your answer. An empty text is not an answer."));
    assert!(body.contains("name=\"b0o_ev\" value=\"1\" checked"));
    assert_eq!(answer_of(&pep, &id)["status"], "pending");
    // Ask the agent first, then answer with a free text in the second block.
    let token = csrf(&pep, &cookie, &id);
    let (st, _, _) = pep.post(
        &cookie,
        REPLY,
        &token,
        &format!("request_id={id}&text=Does+Finance+read+it"),
        None,
    );
    assert_eq!(st, 303);
    let (st, _, body) = send(
        &pep,
        &cookie,
        &id,
        "accept",
        "b0o_ev=1&b0o_ss=1&b1=x&b1t=45+days&b2t=Every+night%2C+02%3A00",
    );
    assert_eq!(st, 303, "{body}");
    let got = answer_of(&pep, &id);
    let answers = got["answer"]["answers"]
        .as_array()
        .unwrap_or_else(|| panic!("{got}"));
    assert_eq!(answers.len(), 3, "{got}");
    // The answered view shows each block, the free text and the discussion.
    let page = detail(&pep, &cookie, &id);
    let text = text_of(&page);
    assert!(text.contains("events_2025") && text.contains("Not chosen: refunds_2024."));
    assert!(text.contains("45 days") && text.contains("Your own answer (Other)."));
    assert!(text.contains("Every night, 02:00"));
    assert!(
        text.contains("You asked the agent before you answered")
            && text.contains("Does Finance read it")
    );
    assert!(!page.contains("name=\"action\" value=\"accept\""));
    // A form of four questions has four blocks.
    let id = ask(
        &pep,
        json!({"title": "Four", "lease_s": 900, "idempotency_key": "four", "input": form_input(true)}),
    );
    assert_eq!(detail(&pep, &cookie, &id).matches("class=\"q\"").count(), 4);
}

#[test]
fn a_decline_carries_nothing_and_the_agent_reads_declined() {
    let pep = rig();
    let cookie = pep.cookie(OWNER);
    let id = ask(&pep, flags(choice(json!({})), true, false));
    // The fields of the blocks are dropped: a decline has no content.
    let (st, head, _) = send(&pep, &cookie, &id, "decline", "b0=x&b0t=ignored");
    assert_eq!(st, 303);
    assert!(common::location(&head).ends_with("?s=q_declined"));
    let got = answer_of(&pep, &id);
    assert_eq!(got["answer"]["action"], "decline");
    assert!(got["answer"]["untrusted"].is_null() || got["answer"]["untrusted"]["text"].is_null());
    let text = text_of(&detail(&pep, &cookie, &id));
    assert!(text.contains("You declined to answer."));
    // A message with no answer is closed with Decline only.
    let id = ask(
        &pep,
        json!({"title": "FYI: the job ended", "lease_s": 900, "input": {"type": "none"}}),
    );
    let page = detail(&pep, &cookie, &id);
    assert!(text_of(&page).contains("This message takes no answer."));
    assert!(!page.contains("name=\"action\" value=\"accept\""));
    assert_eq!(send(&pep, &cookie, &id, "accept", "").0, 422);
    assert_eq!(send(&pep, &cookie, &id, "decline", "").0, 303);
}

#[test]
fn an_answer_on_a_closed_item_gives_a_409_page() {
    let pep = rig();
    let cookie = pep.cookie(OWNER);
    let id = ask(&pep, choice(json!({})));
    let token = csrf(&pep, &cookie, &id);
    // The agent withdraws the question while the page is open.
    pep.tool("cancel", json!({"request_id": id}));
    let (st, _, body) = pep.post(
        &cookie,
        QUESTION,
        &token,
        &format!("request_id={id}&action=accept&b0=o%3Ago"),
        None,
    );
    assert_eq!(st, 409, "{body}");
    assert!(text_of(&body).contains("The question is closed. The agent did not get your answer."));
    assert!(!body.contains("name=\"action\" value=\"accept\""));
    // A second answer after the first one: the page says that another answer came first.
    let id = ask(&pep, choice(json!({"idempotency_key": "two"})));
    let token = csrf(&pep, &cookie, &id);
    let fields = format!("request_id={id}&action=accept&b0=o%3Ago");
    assert_eq!(pep.post(&cookie, QUESTION, &token, &fields, None).0, 303);
    let (st, _, body) = pep.post(&cookie, QUESTION, &token, &fields, None);
    assert_eq!(st, 409);
    assert!(
        text_of(&body).contains("Too late") || text_of(&body).contains("Another answer came first")
    );
    assert_eq!(answer_of(&pep, &id)["answer"]["choice"], json!(["go"]));
}

#[test]
fn a_foreign_owner_is_answered_as_an_unknown_question_and_an_impersonated_session_does_nothing() {
    let pep = rig();
    let id = ask(&pep, choice(json!({})));
    let cookie = pep.cookie(OWNER);
    // Another account that owns keys has the pages, and no question of dana.
    let other = pep
        .s
        .account_with(
            "erin",
            &[
                "access on service:portal",
                "keys.own.read on service:portal",
                "keys.own.use on service:portal/key_kind:mcp",
            ],
        )
        .cookie
        .clone();
    let (st, _, _) = pep.page(&other, &item_path(&id));
    let unknown = "0".repeat(32);
    assert_eq!(st, pep.page(&other, &item_path(&unknown)).0);
    assert_eq!(st, 404);
    let token = pep.inbox_csrf(&other);
    let fields = |id: &str| format!("request_id={id}&action=decline");
    let (st1, head1, _) = pep.post(&other, QUESTION, &token, &fields(&id), None);
    let (st2, head2, _) = pep.post(&other, QUESTION, &token, &fields(&unknown), None);
    assert_eq!(
        (st1, common::location(&head1)),
        (st2, common::location(&head2))
    );
    assert!(common::location(&head1).ends_with("?s=gone"));
    assert_eq!(answer_of(&pep, &id)["status"], "pending");
    // The superadmin that opens a session as dana answers nothing.
    let host = format!("127.0.0.1:{}", pep.s.port);
    let body = format!(
        "csrf={}&login={OWNER}",
        common::form_value(&users_csrf(&pep.s))
    );
    let (st, head, text) = http_full(
        pep.s.port,
        &host,
        "POST",
        "/auth/users/impersonate",
        &[
            ("Content-Type", "application/x-www-form-urlencoded".into()),
            ("Origin", format!("http://{host}")),
            ("Cookie", pep.s.session().cookie.clone()),
        ],
        &body,
    );
    assert_eq!(st, 303, "{text}");
    let session = head
        .lines()
        .filter(|l| l.to_ascii_lowercase().starts_with("set-cookie:"))
        .filter_map(|l| {
            l.split_once(':')
                .map(|(_, v)| v.trim().split(';').next().unwrap().to_string())
        })
        .find(|c| c.starts_with("portal="))
        .expect("the cookie of the impersonation");
    let (st, _, page) = pep.page(&session, &item_path(&id));
    assert_eq!(
        st, 403,
        "an impersonated session does not read a question: {page}"
    );
    let token = pep.inbox_csrf(&session);
    assert_eq!(
        pep.post(&session, QUESTION, &token, &fields(&id), None).0,
        403
    );
    assert_eq!(
        pep.post(
            &session,
            REPLY,
            &token,
            &format!("request_id={id}&text=hi"),
            None
        )
        .0,
        403
    );
    assert_eq!(answer_of(&pep, &id)["status"], "pending");
    drop(cookie);
}

// The panel, the pages and the texts of the agent ----------------------------------------------------------

#[test]
fn the_panel_lists_the_questions_by_agent_and_is_hidden_when_nothing_waits() {
    let pep = rig();
    let cookie = pep.cookie(OWNER);
    let (_, _, page) = pep.page(&cookie, INBOX);
    assert!(
        !page.contains("id=\"oi-t\""),
        "the panel is hidden when it is empty"
    );
    let a = ask(&pep, flags(choice(json!({"explanation": "x"})), true, true));
    let b = ask(
        &pep,
        json!({"title": "A text", "lease_s": 900, "input": {"type": "text"}}),
    );
    let (st, head, page) = pep.page(&cookie, INBOX);
    assert_eq!(st, 200);
    assert_eq!(
        common::approvals::header_of(&head, "cache-control").as_deref(),
        Some("no-store")
    );
    assert!(page.contains("id=\"oi-t\"") && page.contains("Deploy v5.1 to prod?"));
    assert!(page.contains(&format!("href=\"{}\"", item_path(&a))));
    assert!(page.contains(&format!("href=\"{}\"", item_path(&b))));
    assert!(page.contains("class=\"kind\">Question<") && page.contains(">Answer<"));
    assert!(page.contains("3 choices") && page.contains("text answer"));
    assert!(
        page.contains("nbadge--ask"),
        "a question counts in the badge"
    );
    // The alerts stay above the items.
    let oi = page.find("id=\"oi-t\"").unwrap();
    assert_eq!(
        page.matches("class=\"agrp").count(),
        1,
        "one group for the agent"
    );
    assert!(oi > 0);
    // The panel gives the item back after the answer.
    assert_eq!(send(&pep, &cookie, &a, "decline", "").0, 303);
    assert_eq!(send(&pep, &cookie, &b, "decline", "").0, 303);
    let (_, _, page) = pep.page(&cookie, INBOX);
    assert!(!page.contains("id=\"oi-t\"") && !page.contains("nbadge--ask"));
}

#[test]
fn an_open_discussion_shows_the_turns_and_blocks_the_field_while_the_agent_writes() {
    let pep = rig();
    let cookie = pep.cookie(OWNER);
    let id = ask(&pep, flags(choice(json!({})), true, true));
    let token = csrf(&pep, &cookie, &id);
    let (st, head, _) = pep.post(
        &cookie,
        REPLY,
        &token,
        &format!("request_id={id}&text=How+big+is+it"),
        None,
    );
    assert_eq!(st, 303);
    assert!(common::location(&head).ends_with("?s=replied"));
    let page = detail(&pep, &cookie, &id);
    let text = text_of(&page);
    assert!(
        text.contains("How big is it") && text.contains("Discussion") && text.contains("1 message")
    );
    assert!(text.contains("The agent is writing its answer."));
    assert!(
        page.contains("disabled>"),
        "the field is closed while the agent writes"
    );
    assert!(text.contains("Agent is writing"));
    // The agent talks back, and the field opens.
    std::thread::sleep(std::time::Duration::from_millis(2100));
    pep.tool(
        "say",
        json!({"request_id": id, "text": "It is **212 GB**.", "explanation_format": "markdown"}),
    );
    let page = detail(&pep, &cookie, &id);
    let text = text_of(&page);
    assert!(text.contains("Agent text, not checked") && text.contains("212 GB"));
    assert!(
        page.contains("<strong>212 GB</strong>"),
        "the safe Markdown shows"
    );
    assert!(text.contains("Your turn"));
    assert!(!page.contains("The agent is writing its answer."));
    let (_, _, panel) = pep.page(&cookie, INBOX);
    assert!(
        text_of(&panel).contains("open discussion, 2 messages")
            && text_of(&panel).contains("Your turn")
    );
}

#[test]
fn hidden_and_bidi_characters_of_the_agent_are_marked_and_markup_is_text() {
    let pep = rig();
    let cookie = pep.cookie(OWNER);
    let id = ask(
        &pep,
        json!({"title": "Run <b>it</b>\u{200B}now?", "lease_s": 900,
               "explanation": "Safe\u{202E} to run <script>alert(1)</script>",
               "command": "DELETE FROM p\u{430}yments WHERE x = '\u{200B}1'",
               "input": {"type": "choice", "options": [
                   {"id": "a", "label": "Yes\u{200B}", "description": "<i>fine</i>\u{202E}"},
                   {"id": "b", "label": "No"}]},
               "hint": {"suggested": "a\u{200B}", "source": "sys\u{202E}tem"}}),
    );
    let page = detail(&pep, &cookie, &id);
    assert!(
        !page.contains('\u{200B}') && !page.contains('\u{202E}'),
        "no raw hidden character"
    );
    assert!(page.contains("U+200B") && page.contains("U+202E"));
    assert!(
        !page.contains("<script>alert")
            && !page.contains("<b>it</b>")
            && !page.contains("<i>fine</i>")
    );
    assert!(page.contains("&lt;script&gt;"));
    // The command has a hidden character and a look-alike letter: the banner counts them.
    assert!(text_of(&page).contains("This command has hidden or look-alike characters."));
    assert!(text_of(&page).contains("Look at each mark before you answer."));
    let (_, _, panel) = pep.page(&cookie, INBOX);
    assert!(!panel.contains('\u{200B}') && panel.contains("U+200B"));
}

#[test]
fn the_agent_token_is_never_in_a_page_and_no_page_names_the_product() {
    let pep = Pep::with_questions(|s| s.add_to_inbox("agents { lock_new = false }"));
    let cookie = pep.cookie(OWNER);
    let made = pep.tool(
        "ask",
        flags(choice(json!({"agent_label": "billing-bot"})), true, true),
    );
    let token = made["agent_token"]
        .as_str()
        .expect("the first call gives the token")
        .to_string();
    let id = made["request_id"].as_str().unwrap().to_string();
    assert_eq!(send(&pep, &cookie, &id, "decline", "").0, 303);
    let id2 = ask(
        &pep,
        flags(
            choice(json!({"agent_token": token, "idempotency_key": "two"})),
            true,
            true,
        ),
    );
    let root = pep.s.session().cookie.clone();
    for (who, path) in [
        (&cookie, INBOX.to_string()),
        (&cookie, item_path(&id)),
        (&cookie, item_path(&id2)),
        (&cookie, format!("{INBOX}/items")),
        (&cookie, format!("{INBOX}/agents")),
        (&root, format!("{INBOX}/items")),
    ] {
        let (st, _, page) = pep.page(who, &path);
        assert_eq!(st, 200, "{path}");
        assert!(!page.contains(&token), "{path} has the token of the agent");
        let text = Pep::text_of(&page);
        for word in ["portal", "inbox", "inbox", "circular"] {
            assert!(!text.contains(word), "{path} has the word {word}");
        }
    }
    // The answered page too, and the page of a form.
    let f = ask(
        &pep,
        flags(form(json!({"idempotency_key": "f"})), true, true),
    );
    let page = detail(&pep, &cookie, &f);
    for word in ["portal", "inbox", "inbox", "circular"] {
        assert!(!Pep::text_of(&page).contains(word));
    }
}

#[test]
fn with_lock_new_the_page_refuses_the_answer_and_decline_and_ask_stay_open() {
    let pep = Pep::with_questions(|s| s.add_to_inbox("agents { lock_new = true }"));
    let cookie = pep.cookie(OWNER);
    let made = pep.tool(
        "ask",
        flags(choice(json!({"agent_label": "billing-bot"})), true, true),
    );
    let id = made["request_id"].as_str().unwrap().to_string();
    let agent = made["agent_status"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    assert_eq!(agent, "new", "{made}");
    let page = detail(&pep, &cookie, &id);
    let text = text_of(&page);
    assert!(text.contains(
        "Send is off: this agent is new. Name it or allow it first. You can decline now."
    ));
    assert!(page.contains("button--off") && !page.contains("name=\"action\" value=\"accept\""));
    assert!(page.contains("name=\"action\" value=\"decline\"") && has_ask_row(&page));
    let (_, _, panel) = pep.page(&cookie, INBOX);
    assert!(text_of(&panel).contains("Send is locked until you allow this agent."));
    // A forged form is refused by the server: 409 with the page that says why. Nothing changes.
    let (st, _, body) = send(&pep, &cookie, &id, "accept", "b0=o%3Ago");
    assert_eq!(st, 409, "{body}");
    assert!(text_of(&body).contains("Send is locked until you allow this agent."));
    assert_eq!(answer_of(&pep, &id)["status"], "pending");
    // A question to the agent works, and a decline works.
    let token = csrf(&pep, &cookie, &id);
    assert_eq!(
        pep.post(
            &cookie,
            REPLY,
            &token,
            &format!("request_id={id}&text=who+are+you"),
            None
        )
        .0,
        303
    );
    // The owner allows the agent: the answer works.
    let agent_id = pep
        .s
        .scalar(&format!(
            "SELECT agent_id FROM items WHERE request_id = '{id}'"
        ))
        .expect("the agent");
    let (st, head, _) = pep.post(
        &cookie,
        ALLOW,
        &token,
        &format!("agent_id={agent_id}&name=Billing+bot"),
        None,
    );
    assert_eq!(st, 303, "{head}");
    assert_eq!(send(&pep, &cookie, &id, "accept", "b0=o%3Ago").0, 303);
    assert_eq!(answer_of(&pep, &id)["answer"]["choice"], json!(["go"]));
    // A second question of a new agent: decline works while locked.
    let made = pep.tool(
        "ask",
        flags(
            choice(json!({"agent_label": "other-bot", "idempotency_key": "two"})),
            false,
            false,
        ),
    );
    let id2 = made["request_id"].as_str().unwrap().to_string();
    assert_eq!(send(&pep, &cookie, &id2, "decline", "").0, 303);
}

// History, rights and the route of the administrator --------------------------------------------------------

#[test]
fn the_history_lists_the_states_of_the_questions_with_their_own_words() {
    let pep = rig();
    let cookie = pep.cookie(OWNER);
    let answered = ask(&pep, choice(json!({"title": "Answered one"})));
    let declined = ask(
        &pep,
        choice(json!({"title": "Declined one", "idempotency_key": "d"})),
    );
    let cancelled = ask(
        &pep,
        choice(json!({"title": "Cancelled one", "idempotency_key": "c"})),
    );
    let old = ask(
        &pep,
        choice(json!({"title": "Old one", "idempotency_key": "o"})),
    );
    assert_eq!(send(&pep, &cookie, &answered, "accept", "b0=o%3Ago").0, 303);
    assert_eq!(send(&pep, &cookie, &declined, "decline", "").0, 303);
    pep.tool("cancel", json!({"request_id": cancelled}));
    // A new question that replaces the old one.
    let made = pep.tool(
        "supersede",
        json!({"request_id": old, "title": "Old one, new", "lease_s": 900,
               "input": {"type": "choice", "options": options()}}),
    );
    let new_id = made["request_id"].as_str().unwrap().to_string();
    let (st, _, page) = pep.page(&cookie, &format!("{INBOX}/items"));
    assert_eq!(st, 200);
    let text = text_of(&page);
    assert!(text.contains("Answered by dana (web)"), "{text}");
    assert!(text.contains("Declined by dana (web)"));
    assert!(text.contains("Cancelled by the agent."));
    assert!(text.contains("Replaced by a new request."));
    assert!(
        text.contains("Answered 1") && text.contains("Declined 1") && text.contains("Replaced 1")
    );
    // The chip "Answered" filters the rows.
    let (_, _, only) = pep.page(&cookie, &format!("{INBOX}/items?st=answered"));
    assert!(only.contains("Answered one") && !only.contains("Declined one"));
    // The closed pages: the banner of each state, and no field.
    let page = detail(&pep, &cookie, &old);
    let text = text_of(&page);
    assert!(text.contains("The agent proposes something else."));
    assert!(
        page.contains(&format!("href=\"{INBOX}/items/{new_id}\"")),
        "the link to the new question"
    );
    assert!(!page.contains("name=\"action\" value=\"accept\""));
    let text = text_of(&detail(&pep, &cookie, &cancelled));
    assert!(text.contains("Cancelled by the agent."));
    let text = text_of(&detail(&pep, &cookie, &declined));
    assert!(
        text.contains("You declined to answer. You cannot change this.")
            || text.contains("You declined to answer.")
    );
}

#[test]
fn an_account_with_question_read_sees_the_metadata_and_one_with_question_cancel_cancels() {
    let pep = rig();
    let id = ask(&pep, choice(json!({"explanation": "canary-expl-9f3a"})));
    let reader = pep
        .s
        .account_with(
            "rita",
            &[
                "access on service:portal",
                "question.read on service:portal",
            ],
        )
        .cookie
        .clone();
    let (st, _, page) = pep.page(&reader, &item_path(&id));
    assert_eq!(st, 200, "{page}");
    assert!(!page.contains("canary-expl-9f3a") && !page.contains("Deploy now"));
    assert!(text_of(&page).contains("You read the metadata only."));
    assert!(!page.contains("name=\"action\" value=\"accept\"") && !page.contains(CANCEL));
    let (_, _, history) = pep.page(&reader, &format!("{INBOX}/items"));
    assert!(!history.contains("canary-expl-9f3a"));
    // An approval-only reader has no question.
    let approver = pep
        .s
        .account_with(
            "abe",
            &[
                "access on service:portal",
                "approval.read on service:portal",
            ],
        )
        .cookie
        .clone();
    assert_eq!(pep.page(&approver, &item_path(&id)).0, 404);
    // `question.cancel` withdraws an open question.
    let canceller = pep
        .s
        .account_with(
            "cal",
            &[
                "access on service:portal",
                "question.read on service:portal",
                "question.cancel on service:portal",
            ],
        )
        .cookie
        .clone();
    let page = detail(&pep, &canceller, &id);
    assert!(
        page.contains(&format!("action=\"{CANCEL}\"")) && page.contains("Cancel this question")
    );
    let token = hidden_in(&page, CANCEL, "csrf");
    // The reader cannot, because it lacks the right.
    let reader_token = pep.inbox_csrf(&reader);
    assert_eq!(
        pep.post(
            &reader,
            CANCEL,
            &reader_token,
            &format!("request_id={id}"),
            None
        )
        .0,
        403
    );
    assert_eq!(answer_of(&pep, &id)["status"], "pending");
    let (st, head, _) = pep.post(
        &canceller,
        CANCEL,
        &token,
        &format!("request_id={id}"),
        None,
    );
    assert_eq!(st, 303, "{head}");
    assert_eq!(answer_of(&pep, &id)["status"], "cancelled");
}

fn edit_config(s: &Server, from: &str, to: &str) {
    let text = std::fs::read_to_string(s.config()).unwrap();
    assert!(text.contains(from), "{from} is not in the file");
    std::fs::write(s.config(), text.replacen(from, to, 1)).unwrap();
}

#[test]
fn the_body_of_a_question_is_read_by_the_superadmin_only_behind_the_switch() {
    let pep = rig();
    let id = ask(&pep, choice(json!({"explanation": "canary-expl-77c1"})));
    let root = pep.s.session().cookie.clone();
    // The switch is off: the route does not exist, and the root reads the metadata.
    assert_eq!(pep.page(&root, &format!("{}/body", item_path(&id))).0, 404);
    let (st, _, page) = pep.page(&root, &item_path(&id));
    assert_eq!(st, 200);
    assert!(!page.contains("canary-expl-77c1") && !page.contains("/body"));
    drop(pep);
    let pep = Pep::with_questions(|s| {
        edit_config(
            s,
            "active_key = \"k1\"",
            "active_key = \"k1\"\n    admin_body_read = true",
        );
    });
    let id = ask(&pep, choice(json!({"explanation": "canary-expl-77c1"})));
    let root = pep.s.session().cookie.clone();
    let (_, _, meta) = pep.page(&root, &item_path(&id));
    assert!(!meta.contains("canary-expl-77c1"));
    assert!(meta.contains(&format!("{}/body", item_path(&id))));
    let (st, _, page) = pep.page(&root, &format!("{}/body", item_path(&id)));
    assert_eq!(st, 200);
    assert!(page.contains("canary-expl-77c1") && page.contains("Deploy now"));
    assert!(page.contains("You read this content as the administrator."));
    // The administrator sees no form to answer.
    assert!(!page.contains(QUESTION) && !page.contains("name=\"action\" value=\"accept\""));
    // A plain account has no such route.
    let reader = pep
        .s
        .account_with(
            "rita",
            &[
                "access on service:portal",
                "question.read on service:portal",
            ],
        )
        .cookie
        .clone();
    assert_eq!(
        pep.page(&reader, &format!("{}/body", item_path(&id))).0,
        404
    );
}

/// Starts a server with questions in each state and waits until the file `stop` appears in the directory that
/// `INBOX_DEMO_OUT` names. The test is for a look at the pages with a browser (`tools/dev/ui-check-items.js`). It writes
/// `demo-questions.json` with the port, the login, the password and the ids. Run it with
/// `cargo test --test questions_web_e2e questions_demo_for_the_eyes -- --ignored`.
#[test]
#[ignore = "starts a server for a look at the pages"]
fn questions_demo_for_the_eyes() {
    let out = std::path::PathBuf::from(std::env::var("INBOX_DEMO_OUT").expect("INBOX_DEMO_OUT"));
    let pep = Pep::with_questions(|s| {
        edit_config(
            s,
            "active_key = \"k1\"",
            "active_key = \"k1\"\n    admin_body_read = true",
        );
        edit_config(
            s,
            "max_lifetime = \"90d\"",
            "max_lifetime = \"90d\"\n    stale_after = \"1m\"",
        );
    });
    let cookie = pep.cookie(OWNER);
    let (_, shot) = pep.make_question_key("jev", "one_shot");
    // The old question is the first one: it is old when the others are made.
    let old = ask(
        &pep,
        flags(
            choice(
                json!({"title": "Purge the staging of November?", "idempotency_key": "old",
                          "explanation": "The purge frees 40 GB. The tests passed at 14:00.",
                          "command": "DELETE FROM staging.events\nWHERE ts < '2026-11-01';",
                          "command_label": "SQL", "language": "sql"}),
            ),
            true,
            true,
        ),
    );
    let answered = ask(
        &pep,
        choice(json!({"title": "Which region first?", "idempotency_key": "a"})),
    );
    let declined = ask(
        &pep,
        choice(json!({"title": "Rotate the key now?", "idempotency_key": "d"})),
    );
    let cancelled = ask(
        &pep,
        choice(json!({"title": "Archive the logs?", "idempotency_key": "c"})),
    );
    assert_eq!(send(&pep, &cookie, &answered, "accept", "b0=o%3Ago").0, 303);
    assert_eq!(send(&pep, &cookie, &declined, "decline", "").0, 303);
    pep.tool("cancel", json!({"request_id": cancelled}));
    std::thread::sleep(std::time::Duration::from_secs(65));
    let both = ask(
        &pep,
        flags(
            choice(
                json!({"title": "Deploy v5.1 to prod?", "idempotency_key": "both",
                          "explanation": "The change fixes the **double debit** bug.\n\n- Tests passed at 14:00\n- The freeze starts at 18:00",
                          "explanation_format": "markdown",
                          "command": "kubectl -n prod set image deploy/api \\\n  api=registry.local/api:v5.1",
                          "command_label": "Shell", "language": "bash",
                          "hint": {"suggested": "wait", "confidence": 0.62, "source": "agent"}}),
            ),
            true,
            true,
        ),
    );
    let closed = ask(
        &pep,
        choice(json!({"title": "Closed choice", "idempotency_key": "closed"})),
    );
    let multiple = ask(
        &pep,
        json!({"title": "Which tables?", "lease_s": 900, "idempotency_key": "m",
               "input": {"type": "choice", "multiple": true, "min": 1, "max": 2, "allow_custom": true, "options": options()}}),
    );
    let text = ask(
        &pep,
        json!({"title": "When may the job run?", "lease_s": 900, "idempotency_key": "t",
               "input": {"type": "text", "placeholder": "For example: every night", "allow_ask_back": true}}),
    );
    let confirm = ask(
        &pep,
        json!({"title": "Go ahead?", "lease_s": 900, "idempotency_key": "y", "input": {"type": "confirm"}}),
    );
    let none = ask(
        &pep,
        json!({"title": "The job ended", "lease_s": 900, "idempotency_key": "n", "input": {"type": "none"}}),
    );
    let discussion = ask(
        &pep,
        flags(
            choice(json!({"title": "Which table first?", "idempotency_key": "disc"})),
            true,
            true,
        ),
    );
    let token = csrf(&pep, &cookie, &discussion);
    assert_eq!(
        pep.post(
            &cookie,
            REPLY,
            &token,
            &format!("request_id={discussion}&text=How+big+is+each+one"),
            None
        )
        .0,
        303
    );
    std::thread::sleep(std::time::Duration::from_millis(2100));
    pep.tool("say", json!({"request_id": discussion, "text": "events_2025 is **212 GB**.", "explanation_format": "markdown"}));
    let form_id = ask(
        &pep,
        flags(form(json!({"idempotency_key": "form"})), true, true),
    );
    let form_done = ask(
        &pep,
        flags(
            form(json!({"idempotency_key": "form2", "title": "Archive plan"})),
            true,
            false,
        ),
    );
    assert_eq!(
        send(
            &pep,
            &cookie,
            &form_done,
            "accept",
            "b0o_ev=1&b0o_ss=1&b1=x&b1t=45+days&b2t=Every+night"
        )
        .0,
        303
    );
    let (_, jev) = pep.tool_with_key(
        &shot,
        "ask",
        json!({"title": "Which team handles \"double debit\"?", "lease_s": 900,
               "input": {"type": "choice", "options": [
                   {"id": "billing", "label": "billing", "description": "Charges and refunds"},
                   {"id": "support", "label": "support", "description": "Customer help"},
                   {"id": "infra", "label": "infra", "description": "Servers and network"}]},
               "hint": {"suggested": "billing", "confidence": 0.62, "source": "jev"}}),
    );
    let jev = jev["request_id"].as_str().unwrap().to_string();
    let marks = ask(
        &pep,
        flags(
            choice(
                json!({"title": "Run it\u{200B} now?", "idempotency_key": "marks",
                          "explanation": "Safe to run\u{202E}.",
                          "command": "DELETE FROM p\u{430}yments\nWHERE status = 'settled\u{200B}'"}),
            ),
            true,
            false,
        ),
    );
    let ids = json!({"old": old, "answered": answered, "declined": declined, "cancelled": cancelled, "both": both,
                     "closed": closed, "multiple": multiple, "text": text, "confirm": confirm, "none": none,
                     "discussion": discussion, "form": form_id, "form_done": form_done, "jev": jev, "marks": marks});
    std::fs::write(
        out.join("demo-questions.json"),
        json!({"port": pep.s.port, "owner": OWNER, "password": common::ACCOUNT_PASSWORD,
               "root_password": common::SUPERADMIN_PASSWORD, "ids": ids})
        .to_string(),
    )
    .unwrap();
    while !out.join("stop").exists() {
        std::thread::sleep(std::time::Duration::from_millis(300));
    }
}
