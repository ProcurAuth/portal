//! End-to-end tests of the web pages of the items (lot L10): the panel "Open items", the History, the page of one
//! request, the forms that answer, reply and cancel, the route of the administrator and the limits. The rig is the one of
//! the approvals: the service asks through the approval API, the agent gives its texts through `/mcp`, and the owner
//! uses the pages with a session. The pages have no script, so each check reads the HTML.

mod common;

use std::time::Duration;

use common::approvals::{
    command, command_with, header_of, hidden_in, item_path, next_request, next_result, Pep, INBOX,
    OWNER,
};
use common::{http_full, users_csrf, Server, ACCOUNT_PASSWORD};
use inbox_core::proto::{Action, AnswerMsg, AnswerStatus, ClientMsg};
use serde_json::json;

const ANSWER: &str = "/auth/settings/inbox/items/answer";
const REPLY: &str = "/auth/settings/inbox/items/reply";
const CANCEL: &str = "/auth/settings/inbox/items/cancel";

fn edit_config(s: &Server, from: &str, to: &str) {
    let text = std::fs::read_to_string(s.config()).unwrap();
    assert!(text.contains(from), "{from} is not in the file");
    std::fs::write(s.config(), text.replacen(from, to, 1)).unwrap();
}

/// An account that owns keys, so it has the pages of the items, and nothing else.
fn other_owner(pep: &Pep, login: &str) -> String {
    pep.s
        .account_with(
            login,
            &[
                "access on service:portal",
                "keys.own.read on service:portal",
                "keys.own.use on service:portal/key_kind:mcp",
            ],
        )
        .cookie
        .clone()
}

/// An account with the rights `approval.read` and, with `cancel`, `approval.cancel`.
fn admin_account(pep: &Pep, login: &str, cancel: bool) -> String {
    let mut grants = vec![
        "access on service:portal",
        "approval.read on service:portal",
    ];
    if cancel {
        grants.push("approval.cancel on service:portal");
    }
    pep.s.account_with(login, &grants).cookie.clone()
}

/// The superadmin opens a session as another account ("Log in as"). The result is the cookie.
fn impersonate(pep: &Pep, login: &str) -> String {
    let host = format!("127.0.0.1:{}", pep.s.port);
    let body = format!(
        "csrf={}&login={login}",
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
    head.lines()
        .filter(|l| l.to_ascii_lowercase().starts_with("set-cookie:"))
        .filter_map(|l| {
            l.split_once(':')
                .map(|(_, v)| v.trim().split(';').next().unwrap().to_string())
        })
        .find(|c| c.starts_with("portal="))
        .expect("the session cookie of the impersonation")
}

/// The text that a reader sees: the tags are removed and the entities of the portal are decoded.
fn plain(html: &str) -> String {
    let mut out = String::new();
    let mut in_tag = false;
    for c in html.chars() {
        match c {
            '<' => in_tag = true,
            '>' => in_tag = false,
            c if !in_tag => out.push(c),
            _ => {}
        }
    }
    common::unescape(&out)
}

const EXPLANATION: &str = "canary-explanation-52f0: copy the old rows, then check the count.";

fn one(pep: &Pep, n: u32) -> (String, String) {
    pep.submit_intent(&command(n), json!({"explanation": EXPLANATION}), 600)
}

#[test]
fn open_items_zone_renders_with_javascript_disabled() {
    let pep = Pep::new();
    let cookie = pep.cookie(OWNER);
    let (st, _, page) = pep.page(&cookie, INBOX);
    assert_eq!(st, 200);
    assert!(
        !page.contains("id=\"oi-t\""),
        "the panel is absent when nothing waits"
    );

    let (id, code) = one(&pep, 1);
    let (st, _, page) = pep.page(&cookie, INBOX);
    assert_eq!(st, 200);
    assert!(
        page.contains("id=\"oi-t\"") && page.contains(&code),
        "{page}"
    );
    assert!(page.contains(&format!("href=\"{}\"", item_path(&id))));
    assert!(page.contains("Write to ClickHouse") && page.contains("Waiting for you"));
    // The start of the command is in the row, as plain text.
    assert!(page.contains("INSERT INTO analytics.events SELECT 1"));
    // No script is needed: no inline code and no handler attribute, only links and forms.
    assert!(
        !page.contains("<script>") && !page.contains(" onclick=") && !page.contains("javascript:")
    );
    // The detail page is a real form that posts to the server.
    let (_, _, detail) = pep.page(&cookie, &item_path(&id));
    assert!(
        detail.contains(&format!("method=\"post\" action=\"{ANSWER}\"")),
        "{detail}"
    );
    assert!(
        detail.contains("data-copy-text=") && detail.contains(" hidden>"),
        "the copy button waits for the script"
    );
    // The badge of the tab counts the requests to answer.
    assert!(page.contains("nbadge--ask") && page.contains("Requests to answer"));

    // The answer takes the request out of the panel.
    let (st, head, _) = pep.answer_form(&cookie, &id, "accept", "");
    assert_eq!(st, 303, "{head}");
    assert!(common::location(&head).ends_with("?s=approved"));
    let (_, _, page) = pep.page(&cookie, INBOX);
    assert!(!page.contains("id=\"oi-t\"") && !page.contains("nbadge--ask"));
    assert_eq!(pep.verdict(&id)["verdict"], "approved");
}

#[test]
fn alerts_come_before_the_open_items_and_the_phones_after() {
    let pep = Pep::new();
    let s = &pep.s;
    // The owner reads the alerts of the group infra and has a phone: the page has the zones Sent and Phones.
    let grants = [
        "access on service:db",
        "data.write on service:db/host:prod",
        "access on service:portal",
        "inbox.receive on service:portal/inbox_group:infra",
        "alert.read on service:portal/inbox_group:infra",
        "keys.own.read on service:portal",
        "keys.own.create on service:portal/key_kind:mcp",
        "keys.own.use on service:portal/key_kind:mcp",
    ];
    let mut fields = format!("login={OWNER}&grants=1");
    for (n, g) in grants.iter().enumerate() {
        fields.push_str(&format!("&grant{n}={}", common::form_value(g)));
    }
    common::users_form(s, "/auth/users/update", &fields);
    let cookie = pep.cookie(OWNER);
    one(&pep, 2);
    let (_, _, page) = pep.page(&cookie, INBOX);
    let sent = page.find("id=\"sent-title\"").expect("the zone Sent");
    let items = page.find("id=\"oi-t\"").expect("the panel");
    let phones = page.find("id=\"phones-title\"").expect("the zone Phones");
    assert!(
        sent < items && items < phones,
        "alerts first, then the items, then the rest"
    );
}

#[test]
fn command_above_explanation_and_explanation_labelled_unverified() {
    let pep = Pep::new();
    let cookie = pep.cookie(OWNER);
    let (id, _) = pep.submit_intent(
        &command(3),
        json!({"explanation": EXPLANATION, "language": "sql"}),
        600,
    );
    let (st, _, page) = pep.page(&cookie, &item_path(&id));
    assert_eq!(st, 200);
    let command_at = page.find("id=\"cmd-t\"").unwrap();
    let explanation_at = page.find("id=\"exp-t\"").unwrap();
    let decision_at = page.find("Your decision").unwrap();
    assert!(command_at < explanation_at && explanation_at < decision_at);
    // The command is exact text, with its colors; the explanation says that nobody checked it.
    assert!(plain(&page).contains("INSERT INTO analytics.events SELECT 3"));
    assert!(page.contains("tok-keyword"), "the hint sql gives colors");
    assert!(page.contains("Text of the agent, not verified.") && page.contains("Not checked"));
    assert!(page.contains(EXPLANATION));
    assert!(page.contains("This text does not change what will run."));
    assert!(page.contains("cannot check that you read the whole command"));
    assert!(page.contains("From the service"));
    // The short hash and the view fields are in the zone of the command.
    assert!(
        page.contains("sha256:")
            && page.contains("<dt>Host</dt>")
            && page.contains("<dd><code>prod</code></dd>")
    );
}

#[test]
fn xss_bidi_control_zero_width_rendered_visible() {
    let pep = Pep::new();
    let cookie = pep.cookie(OWNER);
    let sql =
        "DELETE FROM p\u{430}yments -- \u{202E}swor wen\u{200B}<script>alert(1)</script>\u{7}";
    let c = command_with(sql, "Write <img src=x onerror=alert(1)> to \u{202E}X");
    let explanation = "Safe<script>alert(2)</script>\u{202E}\u{200B}\n\n[x](javascript:alert(3))";
    let (id, _) = pep.submit_intent(
        &c,
        json!({"explanation": explanation, "explanation_format": "markdown", "language": "sql"}),
        600,
    );
    for path in [item_path(&id), INBOX.to_string(), format!("{INBOX}/items")] {
        let (st, _, page) = pep.page(&cookie, &path);
        assert_eq!(st, 200, "{path}");
        for raw in ['\u{202E}', '\u{200B}', '\u{7}'] {
            assert!(
                !page.contains(raw),
                "{path} has the raw character {:04X}",
                raw as u32
            );
        }
        assert!(
            !page.contains("<script>alert") && !page.contains("<img src=x"),
            "{path}"
        );
        assert!(!page.contains("href=\"javascript"), "{path}");
    }
    let (_, _, page) = pep.page(&cookie, &item_path(&id));
    for mark in ["U+202E", "U+200B", "U+0007"] {
        assert!(page.contains(mark), "the mark {mark} is visible");
    }
    assert!(
        page.contains("md-na") && page.contains("U+0430"),
        "the Cyrillic letter has its mark and its code"
    );
    assert!(page.contains("hidden or look-alike characters"));
    assert!(page.contains("&lt;script&gt;alert(1)&lt;/script&gt;"));
    assert!(page.contains("non-ASCII character"));
    // The copy button holds the exact bytes, hidden characters included.
    let at = page.find("data-copy-text=\"").unwrap() + "data-copy-text=\"".len();
    let bytes = inbox_core::b64::decode(page[at..].split('"').next().unwrap()).unwrap();
    assert_eq!(bytes, sql.as_bytes());
}

#[test]
fn no_store_header_on_pages_with_bodies() {
    let pep = Pep::new();
    let cookie = pep.cookie(OWNER);
    let (id, _) = one(&pep, 4);
    for path in [INBOX.to_string(), item_path(&id), format!("{INBOX}/items")] {
        let (st, head, _) = pep.page(&cookie, &path);
        assert_eq!(st, 200, "{path}");
        assert_eq!(
            header_of(&head, "cache-control").as_deref(),
            Some("no-store"),
            "{path}"
        );
    }
    // The limits page carries no body, and a refused page carries none either.
    let (st, head, _) = pep.page(&cookie, &format!("{INBOX}/items/{}", "0".repeat(32)));
    assert_eq!(st, 404);
    assert!(header_of(&head, "cache-control").as_deref() != Some("public"));
}

#[test]
fn pages_of_the_items_never_name_the_product() {
    let pep = Pep::new();
    let cookie = pep.cookie(OWNER);
    let (id, _) = one(&pep, 5);
    let root = pep.s.session().cookie.clone();
    for (who, path) in [
        (&cookie, INBOX.to_string()),
        (&cookie, item_path(&id)),
        (&cookie, format!("{INBOX}/items")),
        (&root, format!("{INBOX}/items")),
    ] {
        let (st, _, page) = pep.page(who, &path);
        assert_eq!(st, 200, "{path}");
        let text = Pep::text_of(&page);
        for word in ["portal", "inbox", "inbox", "circular"] {
            assert!(!text.contains(word), "{path} has the word {word}");
        }
    }
}

#[test]
fn csrf_and_origin_required_on_answer_and_reply() {
    let pep = Pep::new();
    let cookie = pep.cookie(OWNER);
    let (id, _) = pep.submit_intent(&command(6), json!({"allow_ask_back": true}), 600);
    let (_, _, page) = pep.page(&cookie, &item_path(&id));
    let csrf = hidden_in(&page, ANSWER, "csrf");
    let effect = hidden_in(&page, ANSWER, "shown_effect");
    let fields = format!(
        "request_id={id}&action=accept&shown_effect={}",
        common::form_value(&effect)
    );
    // No token, a wrong token, no origin and another origin: refused, and nothing changes.
    assert_eq!(pep.post(&cookie, ANSWER, "", &fields, None).0, 403);
    assert_eq!(
        pep.post(&cookie, ANSWER, "not-the-token", &fields, None).0,
        403
    );
    assert_eq!(pep.post(&cookie, ANSWER, &csrf, &fields, Some("")).0, 403);
    assert_eq!(
        pep.post(&cookie, ANSWER, &csrf, &fields, Some("http://evil.example"))
            .0,
        403
    );
    let reply = format!("request_id={id}&text=why+this+database");
    assert_eq!(pep.post(&cookie, REPLY, "", &reply, None).0, 403);
    assert_eq!(pep.post(&cookie, REPLY, "x", &reply, None).0, 403);
    assert_eq!(
        pep.post(&cookie, REPLY, &csrf, &reply, Some("http://evil.example"))
            .0,
        403
    );
    assert_eq!(pep.verdict(&id)["verdict"], "pending");
    let (_, _, page) = pep.page(&cookie, &item_path(&id));
    assert!(!page.contains("why this database"), "no turn was stored");
    // A GET on a form route is refused, and the token of another purpose does not fit.
    assert_eq!(pep.page(&cookie, ANSWER).0, 405);
    // The right token and origin work.
    assert_eq!(pep.post(&cookie, REPLY, &csrf, &reply, None).0, 303);
    assert_eq!(pep.post(&cookie, ANSWER, &csrf, &fields, None).0, 303);
    assert_eq!(pep.verdict(&id)["verdict"], "approved");
}

#[test]
fn an_answer_takes_the_stored_effect_and_a_page_that_shows_another_one_is_refused() {
    let pep = Pep::new();
    let cookie = pep.cookie(OWNER);
    let c = command(7);
    let (id, _) = pep.submit(&c, 600);
    let (_, _, page) = pep.page(&cookie, &item_path(&id));
    let csrf = hidden_in(&page, ANSWER, "csrf");
    assert_eq!(hidden_in(&page, ANSWER, "shown_effect"), c.effect);
    let other = common::approvals::command(8).effect;
    let (st, head, _) = pep.post(
        &cookie,
        ANSWER,
        &csrf,
        &format!(
            "request_id={id}&action=accept&shown_effect={}",
            common::form_value(&other)
        ),
        None,
    );
    assert_eq!(st, 303);
    assert!(common::location(&head).ends_with("?s=changed"), "{head}");
    assert_eq!(pep.verdict(&id)["verdict"], "pending");
    // An approval carries no text, a field that the form does not know is refused.
    let fields = format!(
        "request_id={id}&action=accept&shown_effect={}",
        common::form_value(&c.effect)
    );
    assert_eq!(
        pep.post(&cookie, ANSWER, &csrf, &format!("{fields}&text=x"), None)
            .0,
        422
    );
    assert_eq!(
        pep.post(&cookie, ANSWER, &csrf, &format!("{fields}&extra=1"), None)
            .0,
        400
    );
    assert_eq!(pep.verdict(&id)["verdict"], "pending");
    assert_eq!(pep.post(&cookie, ANSWER, &csrf, &fields, None).0, 303);
    assert_eq!(pep.resume(&c)["decision"], true);
    assert_eq!(
        pep.resume(&c)["decision"],
        false,
        "one approval, one execution"
    );
}

#[test]
fn owner_sees_only_own_items() {
    let pep = Pep::new();
    let dana = pep.cookie(OWNER);
    let (id, code) = one(&pep, 9);
    let eve = other_owner(&pep, "eve");
    // Eve has the pages, and none of the request of dana.
    let (st, _, page) = pep.page(&eve, &item_path(&id));
    assert_eq!(st, 404, "{page}");
    let (st, _, page) = pep.page(&eve, INBOX);
    assert_eq!(st, 200);
    assert!(!page.contains(&code) && !page.contains("id=\"oi-t\""));
    let (st, _, page) = pep.page(&eve, &format!("{INBOX}/items"));
    assert_eq!(st, 200);
    assert!(!page.contains(&code));
    // Her answer, her reply and her cancel change nothing.
    let (_, _, mine) = pep.page(&dana, &item_path(&id));
    let csrf = hidden_in(&mine, ANSWER, "csrf");
    let effect = hidden_in(&mine, ANSWER, "shown_effect");
    let fields = format!(
        "request_id={id}&action=accept&shown_effect={}",
        common::form_value(&effect)
    );
    let (_, _, hers) = pep.page(&eve, INBOX);
    let eve_csrf = {
        // The token of her session: any form of her page has it.
        let at = hers.find("name=\"csrf\" value=\"").map(|at| at + 20);
        match at {
            Some(at) => hers[at..].split('"').next().unwrap().to_string(),
            None => csrf.clone(),
        }
    };
    let (st, head, _) = pep.post(&eve, ANSWER, &eve_csrf, &fields, None);
    assert!(
        st == 303 && common::location(&head).ends_with("?s=gone") || st == 403,
        "{st} {head}"
    );
    assert_eq!(
        pep.post(&eve, CANCEL, &eve_csrf, &format!("request_id={id}"), None)
            .0,
        403
    );
    assert_eq!(pep.verdict(&id)["verdict"], "pending");
}

#[test]
fn admin_body_route_404_when_switch_false() {
    let pep = Pep::new();
    let (id, _) = one(&pep, 10);
    let root = pep.s.session().cookie.clone();
    for cookie in [root, pep.cookie(OWNER)] {
        let (st, _, page) = pep.page(&cookie, &format!("{}/body", item_path(&id)));
        assert_eq!(st, 404, "{page}");
    }
    let (_, _, metrics) = {
        let (st, body) = common::http(pep.s.port, "GET", "/metrics", &[], "");
        (st, 0, body)
    };
    assert!(!metrics.contains("portal_item_body_reads_total 1"));
    assert!(!pep.s.database_text().contains("body_read"));
}

#[test]
fn admin_body_read_true_superadmin_only_writes_audit_and_counter_no_body() {
    let pep = Pep::with(|s| {
        edit_config(
            s,
            "active_key = \"k1\"",
            "active_key = \"k1\"\n    admin_body_read = true",
        );
    });
    let (id, _) = one(&pep, 11);
    let root = pep.s.session().cookie.clone();
    let path = format!("{}/body", item_path(&id));
    // An owner and an account with the right `approval.read` get the answer of a route that does not exist.
    assert_eq!(pep.page(&pep.cookie(OWNER), &path).0, 404);
    let reader = admin_account(&pep, "ann", false);
    assert_eq!(pep.page(&reader, &path).0, 404);
    let (_, _, metrics) = {
        let (st, body) = common::http(pep.s.port, "GET", "/metrics", &[], "");
        (st, 0, body)
    };
    assert!(
        !metrics.contains("portal_item_body_reads_total 1"),
        "a refused read is not counted"
    );

    // The superadmin reads it: the page shows the content, the audit has one row and the counter is 1.
    let (st, head, page) = pep.page(&root, &path);
    assert_eq!(st, 200, "{page}");
    assert_eq!(
        header_of(&head, "cache-control").as_deref(),
        Some("no-store")
    );
    assert!(page.contains("INSERT INTO analytics.events SELECT 11") && page.contains(EXPLANATION));
    assert!(page.contains("You read this content as the administrator"));
    assert!(
        !page.contains(ANSWER),
        "the route of the administrator has no form of decision"
    );
    let (_, _, metrics) = {
        let (st, body) = common::http(pep.s.port, "GET", "/metrics", &[], "");
        (st, 0, body)
    };
    assert!(
        metrics.contains("portal_item_body_reads_total 1"),
        "{metrics}"
    );
    let database = pep.s.database_text();
    assert_eq!(
        database.matches("body_read").count(),
        1,
        "one audit row for one read"
    );
    assert!(database.contains("user:root"));
    // The audit and the log hold no content.
    for canary in [EXPLANATION, "INSERT INTO analytics.events SELECT 11"] {
        assert!(
            !database.contains(canary),
            "the database holds the content in clear"
        );
        assert!(!pep.s.log().contains(canary), "the log holds the content");
    }
    // The metadata page of an administrator offers the link, only to the superadmin.
    let (_, _, meta) = pep.page(&root, &item_path(&id));
    assert!(
        meta.contains(&format!("{}/body", item_path(&id)))
            || meta.contains("Your decision")
            || meta.contains("Command")
    );
    let (_, _, hers) = pep.page(&reader, &item_path(&id));
    assert!(!hers.contains("/body\""), "{hers}");
    // The superadmin that opens a session as an account of the right does not read either.
    assert_eq!(pep.page(&impersonate(&pep, "ann"), &path).0, 404);
}

#[test]
fn impersonated_session_cannot_answer() {
    let pep = Pep::new();
    let (id, _) = one(&pep, 12);
    let cookie = pep.cookie(OWNER);
    let (_, _, page) = pep.page(&cookie, &item_path(&id));
    let effect = hidden_in(&page, ANSWER, "shown_effect");
    let as_dana = impersonate(&pep, OWNER);
    // The session of "Log in as" reads no content and answers nothing.
    let (st, _, body) = pep.page(&as_dana, &item_path(&id));
    assert_eq!(st, 403, "{body}");
    assert!(!body.contains(EXPLANATION));
    let (st, _, body) = pep.page(&as_dana, INBOX);
    assert_eq!(st, 200);
    assert!(!body.contains("id=\"oi-t\"") && !body.contains("Write to ClickHouse"));
    assert_eq!(pep.page(&as_dana, &format!("{INBOX}/items")).0, 403);
    let (_, _, any) = pep.page(&as_dana, INBOX);
    let csrf = {
        let at = any
            .find("action=\"/auth/settings/inbox/")
            .expect("a form of the page");
        let marker = "name=\"csrf\" value=\"";
        let at = at + any[at..].find(marker).unwrap() + marker.len();
        any[at..].split('"').next().unwrap().to_string()
    };
    let fields = format!(
        "request_id={id}&action=accept&shown_effect={}",
        common::form_value(&effect)
    );
    assert_eq!(pep.post(&as_dana, ANSWER, &csrf, &fields, None).0, 403);
    assert_eq!(
        pep.post(
            &as_dana,
            REPLY,
            &csrf,
            &format!("request_id={id}&text=x"),
            None
        )
        .0,
        403
    );
    assert_eq!(pep.verdict(&id)["verdict"], "pending");
}

#[test]
fn approval_read_right_shows_metadata_not_body() {
    let pep = Pep::new();
    let (id, code) = one(&pep, 13);
    let ann = admin_account(&pep, "ann", false);
    let (st, _, page) = pep.page(&ann, &format!("{INBOX}/items?p=7"));
    assert_eq!(st, 200);
    // An open request is in no History: answer it first (the owner declines).
    assert!(!page.contains(&code));
    let owner = pep.cookie(OWNER);
    assert_eq!(pep.answer_form(&owner, &id, "decline", "").0, 303);
    let (_, _, page) = pep.page(&ann, &format!("{INBOX}/items"));
    assert!(
        page.contains(&code) && page.contains("content hidden"),
        "{page}"
    );
    assert!(page.contains("dana"), "the owner is named");
    assert!(!page.contains("Write to ClickHouse"));
    let (st, _, detail) = pep.page(&ann, &item_path(&id));
    assert_eq!(st, 200, "{detail}");
    assert!(detail.contains("metadata only") && detail.contains("sha256:"));
    for secret in [
        EXPLANATION,
        "INSERT INTO analytics.events",
        "Write to ClickHouse",
    ] {
        assert!(!detail.contains(secret), "{secret}");
    }
    assert!(!detail.contains("id=\"cmd-t\"") && !detail.contains(ANSWER));
    // Without the right, an account sees nothing of the others.
    let eve = other_owner(&pep, "eve");
    assert_eq!(pep.page(&eve, &item_path(&id)).0, 404);
    // The limits are for the accounts with the right.
    assert_eq!(pep.page(&ann, &format!("{INBOX}/limits")).0, 200);
    assert_eq!(pep.page(&eve, &format!("{INBOX}/limits")).0, 403);
}

#[test]
fn an_account_with_approval_cancel_cancels_an_open_request_and_nobody_else_does() {
    let pep = Pep::new();
    let (id, _) = one(&pep, 14);
    let reader = admin_account(&pep, "ann", false);
    let (_, _, page) = pep.page(&reader, &item_path(&id));
    assert!(!page.contains(CANCEL), "no button without the right");
    assert_eq!(
        pep.post(&reader, CANCEL, "x", &format!("request_id={id}"), None)
            .0,
        403
    );
    let canceller = admin_account(&pep, "cam", true);
    let (_, _, page) = pep.page(&canceller, &item_path(&id));
    let csrf = hidden_in(&page, CANCEL, "csrf");
    assert_eq!(pep.verdict(&id)["verdict"], "pending");
    let (st, head, _) = pep.post(&canceller, CANCEL, &csrf, &format!("request_id={id}"), None);
    assert_eq!(st, 303, "{head}");
    assert!(common::location(&head).ends_with("?s=cancelled"));
    assert_eq!(
        pep.verdict(&id)["verdict"],
        "void",
        "the service sees a closed request"
    );
    let (_, _, page) = pep.page(&canceller, &item_path(&id));
    assert!(page.contains("Cancelled by an administrator."));
    // The owner cannot answer it any more.
    let owner = pep.cookie(OWNER);
    let (_, _, page) = pep.page(&owner, &item_path(&id));
    assert!(!page.contains("value=\"accept\""));
}

#[test]
fn a_decline_with_a_comment_reaches_the_agent_and_a_reply_makes_a_turn() {
    let pep = Pep::new();
    let owner = pep.cookie(OWNER);
    let (id, _) = pep.submit_intent(
        &command(15),
        json!({"allow_custom": true, "allow_ask_back": true}),
        600,
    );
    // A question: the request stays open and the thread shows it.
    let (_, _, page) = pep.page(&owner, &item_path(&id));
    let csrf = hidden_in(&page, REPLY, "csrf");
    let (st, head, _) = pep.post(
        &owner,
        REPLY,
        &csrf,
        &format!("request_id={id}&text=Why+this+table%3F%0A%3Cb%3Ex"),
        None,
    );
    assert_eq!(st, 303, "{head}");
    assert!(common::location(&head).ends_with("?s=replied"));
    let (_, _, page) = pep.page(&owner, &item_path(&id));
    assert!(
        page.contains("Why this table?") && page.contains("&lt;b&gt;x"),
        "{page}"
    );
    assert_eq!(pep.verdict(&id)["verdict"], "pending");
    let (_, head, _) = pep.post(
        &owner,
        REPLY,
        &csrf,
        &format!("request_id={id}&text=+"),
        None,
    );
    assert!(head.contains("s=no_text_given"), "{head}");
    // The decline with a comment ends the request. The agent reads the text as a text of a third party.
    let (st, _, _) = pep.answer_form(
        &owner,
        &id,
        "decline",
        "text=use+staging_b&intent=change_request",
    );
    assert_eq!(st, 303);
    assert_eq!(pep.verdict(&id)["verdict"], "denied");
    let answer = pep.tool("get_answer", json!({"request_id": id, "wait_s": 0}));
    assert!(answer.to_string().contains("use staging_b"), "{answer}");
    assert!(answer.to_string().contains("untrusted"));
    // A comment on a request that does not allow it is refused.
    let (id2, _) = pep.submit(&command(16), 600);
    let (st, _, page) = pep.answer_form(&owner, &id2, "decline", "text=hello");
    assert_eq!(st, 303, "{page}");
    assert_eq!(pep.verdict(&id2)["verdict"], "pending");
}

#[test]
fn web_approval_false_lists_only() {
    let pep = Pep::with(|s| {
        edit_config(
            s,
            "consume_within = \"5m\"",
            "consume_within = \"5m\"\n    web_approval = false",
        );
    });
    let owner = pep.cookie(OWNER);
    let rt = tokio::runtime::Runtime::new().unwrap();
    // The service needs a phone when the web does not decide.
    let _phone = rt.block_on(pep.phone("pixel"));
    let (id, code) = one(&pep, 17);
    let (_, _, page) = pep.page(&owner, INBOX);
    assert!(page.contains(&code), "the panel lists the request");
    let (st, _, detail) = pep.page(&owner, &item_path(&id));
    assert_eq!(st, 200);
    assert!(
        detail.contains("INSERT INTO analytics.events SELECT 17"),
        "the content shows"
    );
    assert!(detail.contains("answers on the phone only"));
    assert!(!detail.contains("value=\"accept\"") && !detail.contains(ANSWER));
    // A form that someone posts anyway decides nothing.
    let csrf = {
        let at = page.find("action=\"/auth/settings/inbox/").unwrap();
        let marker = "name=\"csrf\" value=\"";
        let at = at + page[at..].find(marker).unwrap() + marker.len();
        page[at..].split('"').next().unwrap().to_string()
    };
    let fields = format!(
        "request_id={id}&action=accept&shown_effect={}",
        common::form_value(&command(17).effect)
    );
    let (st, head, _) = pep.post(&owner, ANSWER, &csrf, &fields, None);
    assert_eq!(st, 303);
    assert!(common::location(&head).ends_with("?s=phone_only"), "{head}");
    assert_eq!(pep.verdict(&id)["verdict"], "pending");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn web_and_phone_answer_race_one_accepted() {
    let pep = Pep::new();
    let phone = pep.phone("pixel").await;
    let owner = pep.cookie(OWNER);
    let c = command(18);
    let (id, _) = pep.submit(&c, 600);
    let (mut conn, _) = common::Conn::open(pep.s.port, &phone)
        .await
        .expect("the session opens");
    let req = next_request(&mut conn, Duration::from_secs(10)).await;
    conn.receipt(&req.id).await;
    // The page gave a form: the web wins with it, and the same form is used again after.
    let (_, _, page) = pep.page(&owner, &item_path(&id));
    let csrf = hidden_in(&page, ANSWER, "csrf");
    let fields = |action: &str| {
        format!(
            "request_id={id}&action={action}&shown_effect={}",
            common::form_value(&c.effect)
        )
    };
    let (st, _, _) = pep.post(&owner, ANSWER, &csrf, &fields("accept"), None);
    assert_eq!(st, 303);
    conn.send(&ClientMsg::Answer(AnswerMsg {
        request_id: id.clone(),
        client_msg_id: "m-1".into(),
        action: Action::Decline,
        shown_effect: None,
        choice: None,
        text: None,
        intent: None,
        answers: None,
    }))
    .await;
    let result = next_result(&mut conn, Duration::from_secs(10)).await;
    assert_eq!(result.status, AnswerStatus::AlreadyAnswered);
    assert_eq!(pep.verdict(&id)["verdict"], "approved");
    // A second web answer is refused too.
    let (st, head, _) = pep.post(&owner, ANSWER, &csrf, &fields("decline"), None);
    assert_eq!(st, 303);
    assert!(common::location(&head).ends_with("?s=taken"), "{head}");
    assert_eq!(pep.verdict(&id)["verdict"], "approved");
    assert_eq!(pep.resume(&c)["decision"], true);
    assert_eq!(pep.resume(&c)["decision"], false);
}

#[test]
fn stale_request_needs_checkbox() {
    let pep = Pep::with(|s| {
        edit_config(
            s,
            "max_lifetime = \"90d\"",
            "max_lifetime = \"90d\"\n    stale_after = \"1m\"",
        );
    });
    let owner = pep.cookie(OWNER);
    let c = command(19);
    let (id, _) = pep.submit(&c, 600);
    let (_, _, page) = pep.page(&owner, &item_path(&id));
    assert!(!page.contains("stale_ack"), "a young request has no box");
    std::thread::sleep(Duration::from_secs(62));
    let (_, _, page) = pep.page(&owner, &item_path(&id));
    assert!(page.contains("This request is old.") && page.contains("name=\"stale_ack\""));
    assert!(page.contains("I know this request is 1 minute old."));
    let (_, _, main) = pep.page(&owner, INBOX);
    assert!(main.contains("Old request"));
    // Without the box the approval is refused, and the page says why. A refusal needs no box.
    let (st, _, body) = pep.answer_form(&owner, &id, "accept", "");
    assert_eq!(st, 422, "{body}");
    assert!(body.contains("Tick the box to approve an old request."));
    assert_eq!(pep.verdict(&id)["verdict"], "pending");
    let (st, _, _) = pep.answer_form(&owner, &id, "accept", "stale_ack=1");
    assert_eq!(st, 303);
    assert_eq!(pep.verdict(&id)["verdict"], "approved");
}

#[test]
fn history_filters_chains_and_hides_the_open_items() {
    let pep = Pep::new();
    let owner = pep.cookie(OWNER);
    let (first, code1) = pep.submit(&command(20), 600);
    // The second request replaces the first: the history chains them.
    let c2 = command(21);
    let reply = pep.submit_with(&c2, json!({"lease_s": 600, "supersedes": first}));
    assert!(reply.status == 201 || reply.status == 200, "{}", reply.body);
    let second = reply.json()["request_id"].as_str().unwrap().to_string();
    let code2 = reply.json()["code"].as_str().unwrap().to_string();
    let (third, code3) = pep.submit(&command(22), 600);
    let history = format!("{INBOX}/items");
    let (_, _, page) = pep.page(&owner, &history);
    assert!(
        page.contains(&code1),
        "the replaced request is in the history"
    );
    assert!(
        !page.contains(&code2) && !page.contains(&code3),
        "an open request is not"
    );
    assert!(page.contains("Replaced by a new request."));
    assert_eq!(pep.answer_form(&owner, &second, "accept", "").0, 303);
    assert_eq!(pep.answer_form(&owner, &third, "decline", "").0, 303);
    let (_, _, page) = pep.page(&owner, &history);
    assert!(page.contains(&code1) && page.contains(&code2) && page.contains(&code3));
    // The request that follows another one stands right after it, as a chained row.
    let at1 = page.find(&format!(">{code1}<")).unwrap();
    let at2 = page.find(&format!(">{code2}<")).unwrap();
    assert!(
        at1 < at2 && page[at1..at2].contains("class=\"chain\""),
        "{page}"
    );
    // The state filter, the key filter and the period.
    let (_, _, only) = pep.page(&owner, &format!("{history}?st=declined"));
    assert!(only.contains(&code3) && !only.contains(&code2) && !only.contains(&code1));
    let (_, _, only) = pep.page(&owner, &format!("{history}?st=replaced"));
    assert!(only.contains(&code1) && !only.contains(&code3));
    let (_, _, only) = pep.page(&owner, &format!("{history}?k=agent-lap"));
    assert!(only.contains(&code1) && only.contains(&code3));
    let (_, _, none) = pep.page(&owner, &format!("{history}?k=nobody"));
    assert!(!none.contains(&code1) && none.contains("No request matches."));
    let (_, _, none) = pep.page(&owner, &format!("{history}?svc=other"));
    assert!(!none.contains(&code1));
    let (_, _, some) = pep.page(&owner, &format!("{history}?svc=db&p=7"));
    assert!(some.contains(&code1));
    // A parameter that the page does not know is refused.
    assert_eq!(pep.page(&owner, &format!("{history}?x=1")).0, 400);
    assert_eq!(pep.page(&owner, &format!("{history}?st=open")).0, 400);
    // A final request has its outcome and no form.
    let (_, _, detail) = pep.page(&owner, &item_path(&third));
    assert!(detail.contains("Declined by dana (web)") && !detail.contains("value=\"accept\""));
}

#[test]
fn the_limits_page_shows_the_values_the_sources_and_no_secret() {
    let pep = Pep::new();
    let root = pep.s.session().cookie.clone();
    let (st, _, page) = pep.page(&root, &format!("{INBOX}/limits"));
    assert_eq!(st, 200, "{page}");
    for key in [
        "inbox.items.max_lifetime",
        "inbox.items.stale_after",
        "inbox.approvals.consume_within",
        "inbox.approvals.web_approval",
        "inbox.at_rest.admin_body_read",
        "inbox.at_rest.active_key",
        "inbox.at_rest.key.k1.secret",
    ] {
        assert!(page.contains(key), "{key} is not in the page");
    }
    assert!(page.contains("value hidden"));
    assert!(page.contains(">file<") || page.contains(">file:"));
    assert!(page.contains(">default<"));
    assert!(page.contains("Administrators cannot read the content of items."));
    let secret = std::fs::read_to_string(pep.s.dir.join("content_key")).unwrap();
    assert!(!page.contains(secret.trim()));
    // The owner has no right to it. The password of the test accounts is not a secret of the page either.
    assert_eq!(
        pep.page(&pep.cookie(OWNER), &format!("{INBOX}/limits")).0,
        403
    );
    assert!(!page.contains(ACCOUNT_PASSWORD));
}

/// Starts a server with requests in each state and waits until the file `stop` appears in the directory that
/// `INBOX_DEMO_OUT` names. The test is for a look at the pages with a browser (`tools/dev/ui-check-items.js`). It writes
/// `demo.json` with the port, the logins and the passwords. Run it with
/// `cargo test --test items_web_e2e items_demo_for_the_eyes -- --ignored`.
#[test]
#[ignore = "starts a server for a look at the pages"]
fn items_demo_for_the_eyes() {
    let out = std::path::PathBuf::from(std::env::var("INBOX_DEMO_OUT").expect("INBOX_DEMO_OUT"));
    let pep = Pep::with(|s| {
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
    let rt = tokio::runtime::Runtime::new().unwrap();
    // The owner reads the alerts and has phones, so that the page shows every zone.
    let grants = [
        "access on service:db",
        "data.write on service:db/host:prod",
        "access on service:portal",
        "inbox.receive on service:portal/inbox_group:infra",
        "alert.read on service:portal/inbox_group:infra",
        "keys.own.read on service:portal",
        "keys.own.create on service:portal/key_kind:mcp",
        "keys.own.use on service:portal/key_kind:mcp",
    ];
    let mut fields = format!("login={OWNER}&grants=1");
    for (n, g) in grants.iter().enumerate() {
        fields.push_str(&format!("&grant{n}={}", common::form_value(g)));
    }
    common::users_form(&pep.s, "/auth/users/update", &fields);
    let _phones = (
        rt.block_on(pep.phone("Pixel 9 of dana")),
        rt.block_on(pep.phone("Pixel 6a of dana")),
    );
    for (n, title) in [
        "Disk 86 % on db-2",
        "Backup finished",
        "Certificate renews in 9 d",
    ]
    .iter()
    .enumerate()
    {
        let (st, _) = common::post_alert(
            &pep.s,
            &format!("demo-{n}"),
            json!({"group": "infra", "title": title}),
        );
        assert_eq!(st, 201);
    }
    let owner = pep.cookie(OWNER);
    // The old request is the first one: it is old when the others are made.
    let (old, _) = pep.submit_intent(
        &command_with("DELETE FROM payments.refunds\nWHERE created_at < '2026-05-01 00:00:00'\n  AND status = 'settled'", "Run a write command on ClickHouse (payments.refunds)"),
        json!({"explanation": "Clean up refunds older than 6 months, as asked in ticket 4412.", "language": "sql"}),
        3000,
    );
    // The history: a refusal with a comment, a replaced request and its successor, an approval.
    let (declined, _) = pep.submit_intent(&command(31), json!({"allow_custom": true}), 3000);
    assert_eq!(
        pep.answer_form(
            &owner,
            &declined,
            "decline",
            "text=use+staging_b&intent=comment"
        )
        .0,
        303
    );
    let (first, _) = pep.submit(&command(32), 3000);
    let again = pep.submit_with(&command(33), json!({"lease_s": 3000, "supersedes": first}));
    let second = again.json()["request_id"].as_str().unwrap().to_string();
    assert_eq!(pep.answer_form(&owner, &second, "accept", "").0, 303);
    let (cancelled, _) = pep.submit(&command(34), 3000);
    pep.tool("cancel", json!({"request_id": cancelled}));
    std::thread::sleep(Duration::from_secs(65));
    let md = "## Why this step\n\nThe prod replica disk is at 86 %. This step copies old rows to `archive.events_2025`. The [runbook](https://wiki.example.org/runbook) has the rollback.\n\n- Row count is the same on both sides\n- Space after the step:\n  - prod replica: frees about 212 GB\n\n- [x] Dry run on staging\n- [ ] Drop the old partitions\n\n> I do not drop anything in this step.\n\n```sql\nSELECT count() AS rows, min(ts)\nFROM prod.events_2025\n```\n";
    let (md_id, _) = pep.submit_intent(
        &command_with("-- archive events older than 90 days\nINSERT INTO archive.events_2025\nSELECT event_id, toDateTime(ts) AS ts, 100.50 AS amount\nFROM prod.events_2025\nWHERE ts < now() - INTERVAL 90 DAY AND status IN ('closed', 'settled')\nLIMIT 5000000", "Copy old events to the archive"),
        json!({"explanation": md, "explanation_format": "markdown", "language": "sql", "allow_custom": true, "allow_ask_back": true}),
        3000,
    );
    pep.tool("say", json!({"request_id": md_id, "text": "Ask me if the **count** is not the same.\n\n```diff\n-old\n+new\n```", "explanation_format": "markdown"}));
    let (marks, _) = pep.submit_intent(
        &command_with("DELETE FROM p\u{430}yments.refunds\nWHERE status = 'settled\u{200B}'\n-- keep new rows \u{202E}swor wen peek", "Run a write command on ClickHouse"),
        json!({"explanation": "Clean up old refunds. Safe to approve\u{202E}."}),
        3000,
    );
    let (long, _) = pep.submit_intent(
        &command(35),
        json!({"explanation": "A long explanation. ".repeat(40)}),
        3000,
    );
    let admin_login = "ann";
    admin_account(&pep, admin_login, true);
    let ids = json!({"old": old, "markdown": md_id, "marks": marks, "long": long, "declined": declined,
                     "replaced": first, "cancelled": cancelled});
    std::fs::write(
        out.join("demo.json"),
        json!({"port": pep.s.port, "owner": OWNER, "password": ACCOUNT_PASSWORD, "root_password": common::SUPERADMIN_PASSWORD,
               "admin": admin_login, "ids": ids})
        .to_string(),
    )
    .unwrap();
    while !out.join("stop").exists() {
        std::thread::sleep(Duration::from_millis(300));
    }
}
