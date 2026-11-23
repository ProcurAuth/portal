//! End-to-end tests of the web pages of the agents: the groups of the panel "Open items", the notice of a new agent,
//! the page and the list of the agents, the forms that name, allow, block, unblock and merge, the lock of an approval, and the
//! filter of the History. The rig is the one of the items: the service asks through the approval API, the agent gives its
//! texts through `/mcp`, and the owner uses the pages with a session. The pages have no script, so each check reads the HTML.
//!
//! The agents come from the real channel: the first call of an agent has no token, the portal makes the agent and
//! gives the token, and the next calls send it. The owner uses the forms of the pages. Only the cases that the channel
//! refuses to make are written in the database: text with hidden characters, an agent of another key or of another owner,
//! and the demo for the eyes. They test what the page does with data that it must never trust.

mod common;

use std::time::Duration;

use aws_lc_rs::aead::{Aad, LessSafeKey, Nonce, UnboundKey, AES_256_GCM};
use common::approvals::{command, hidden_in, item_path, Pep, INBOX, OWNER};
use common::{http_full, users_csrf};
use hmac::{Hmac, Mac};
use serde_json::json;
use sha2::{Digest, Sha256};

const ANSWER: &str = "/auth/settings/inbox/items/answer";
const AGENTS: &str = "/auth/settings/inbox/agents";
const ALLOW: &str = "/auth/settings/inbox/agents/allow";
const BLOCK: &str = "/auth/settings/inbox/agents/block";
const UNBLOCK: &str = "/auth/settings/inbox/agents/unblock";
const MERGE: &str = "/auth/settings/inbox/agents/merge";
const RENAME: &str = "/auth/settings/inbox/agents/rename";
/// The name of the key that the rig makes.
const KEY: &str = "agent-laptop";

// The rig ---------------------------------------------------------------------------------------------

fn enabled(lock_new: bool) -> Pep {
    Pep::with(|s| {
        s.add_to_inbox(&format!("  agents {{ lock_new = {lock_new} }}"));
    })
}

fn sql(pep: &Pep, statement: &str) {
    match &pep.s.pg {
        Some((admin, database)) => admin.exec(database, statement.to_string()),
        None => {
            let c = rusqlite::Connection::open(pep.s.dir.join("portal.db")).unwrap();
            c.busy_timeout(Duration::from_secs(10)).unwrap();
            c.execute_batch(statement).unwrap();
        }
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// The associated data of a sealed value, as `content_box::aad` writes it.
fn aad(id: &str, field: &str) -> Vec<u8> {
    let mut out = Vec::new();
    let mut part = |bytes: &[u8]| {
        out.extend_from_slice(&(bytes.len() as u32).to_be_bytes());
        out.extend_from_slice(bytes);
    };
    part(b"portal-content/v1");
    part(id.as_bytes());
    part(field.as_bytes());
    part(&0u64.to_be_bytes());
    part(b"");
    out
}

/// Seals `plain` with the content key of the server: AES-256-GCM with the key that HMAC-SHA-256 derives for the label.
fn seal(pep: &Pep, id: &str, plain: &[u8]) -> Vec<u8> {
    let text = std::fs::read_to_string(pep.s.dir.join("content_key")).unwrap();
    let key = inbox_core::b64::decode(text.trim()).unwrap();
    let mut mac = Hmac::<Sha256>::new_from_slice(&key).unwrap();
    mac.update(b"portal-content/seal/v1");
    let derived = mac.finalize().into_bytes();
    let sealing = LessSafeKey::new(UnboundKey::new(&AES_256_GCM, &derived).unwrap());
    let nonce: [u8; 12] = inbox_core::rng::Rng::os().bytes();
    let mut buffer = plain.to_vec();
    sealing
        .seal_in_place_append_tag(
            Nonce::assume_unique_for_key(nonce),
            Aad::from(aad(id, "agent_hints")),
            &mut buffer,
        )
        .unwrap();
    let mut out = vec![1, 2];
    out.extend_from_slice(b"k1");
    out.extend_from_slice(&nonce);
    out.extend_from_slice(&buffer);
    out
}

#[derive(Clone, Copy)]
struct Seed<'a> {
    id: &'a str,
    owner: &'a str,
    key_id: Option<&'a str>,
    name: Option<&'a str>,
    state: &'a str,
    ordinal: i64,
    says: Option<&'a str>,
    client: Option<&'a str>,
    net: Option<&'a str>,
    created_ago_s: i64,
}

impl<'a> Seed<'a> {
    fn new(id: &'a str, state: &'a str, ordinal: i64) -> Seed<'a> {
        Seed {
            id,
            owner: OWNER,
            key_id: None,
            name: None,
            state,
            ordinal,
            says: None,
            client: None,
            net: None,
            created_ago_s: 3600,
        }
    }
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64
}

/// Writes an agent as the engine does: the row, the sealed hints and the event `created`.
fn seed(pep: &Pep, a: Seed) {
    let key = a.key_id.unwrap_or(&pep.key_id).to_string();
    let now = now_ms();
    let hints = if a.says.is_some() || a.client.is_some() || a.net.is_some() {
        let mut o = serde_json::Map::new();
        if let Some(s) = a.says {
            o.insert("says".into(), json!(s));
            o.insert("says_first".into(), json!(s));
        }
        if let Some(c) = a.client {
            o.insert("client".into(), json!(c));
        }
        if let Some(n) = a.net {
            o.insert("net".into(), json!(n));
            o.insert("nets".into(), json!([n]));
        }
        Some(seal(
            pep,
            a.id,
            serde_json::Value::Object(o).to_string().as_bytes(),
        ))
    } else {
        None
    };
    let blob = |b: &[u8]| match &pep.s.pg {
        Some(_) => format!("decode('{}', 'hex')", hex(b)),
        None => format!("X'{}'", hex(b)),
    };
    let text = |t: Option<&str>| {
        t.map_or("NULL".to_string(), |t| {
            format!("'{}'", t.replace('\'', "''"))
        })
    };
    let name_key = a.name.map(|n| n.to_lowercase());
    let statement = format!(
        "INSERT INTO agents (agent_id, key_id, owner, kind, state, origin, ordinal, name, name_key, token_digest, merged_into, rev, created_at, named_at, last_seen_at, hints_kid, hints_ct) \
         VALUES ('{id}', '{key}', '{owner}', 'agent', '{state}', 'call', {ordinal}, {name}, {name_key}, '{digest}', {merged}, 1, {created}, {named}, {now}, {kid}, {ct}); \
         INSERT INTO agent_events (agent_id, at, event, actor, detail) VALUES ('{id}', {created}, 'created', 'agent', 'ordinal={ordinal}');",
        id = a.id,
        owner = a.owner,
        state = a.state,
        ordinal = a.ordinal,
        name = text(a.name),
        name_key = text(name_key.as_deref()),
        digest = hex(a.id.as_bytes()).repeat(2),
        merged = if a.state == "merged" { "'0000'" } else { "NULL" },
        created = now - a.created_ago_s * 1000,
        named = if a.name.is_some() { (now - a.created_ago_s * 1000 + 5000).to_string() } else { "NULL".into() },
        kid = hints.as_ref().map_or("NULL".to_string(), |_| "'k1'".into()),
        ct = hints.as_deref().map_or("NULL".to_string(), blob),
    );
    sql(pep, &statement);
}

/// Moves a request to an agent.
fn give(pep: &Pep, request: &str, agent: &str) {
    sql(
        pep,
        &format!("UPDATE items SET agent_id = '{agent}' WHERE request_id = '{request}'"),
    );
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

/// The token `csrf` of any form of the page of the Inbox for a session.
fn csrf_of(pep: &Pep, cookie: &str) -> String {
    let (_, _, page) = pep.page(cookie, INBOX);
    let at = page
        .find("action=\"/auth/settings/inbox/")
        .expect("a form of the page");
    let marker = "name=\"csrf\" value=\"";
    let at = at + page[at..].find(marker).unwrap() + marker.len();
    common::unescape(page[at..].split('"').next().unwrap())
}

/// The fields of a valid form of the action, on the new agent.
fn fields_for(action: &str, a1: &str, a2: &str) -> String {
    match action {
        ALLOW | RENAME => format!("agent_id={a2}&name=Mine"),
        MERGE => format!("agent_id={a2}&into={a1}"),
        _ => format!("agent_id={a2}"),
    }
}

/// The text of the page that a person reads, with the entities decoded.
fn text(page: &str) -> String {
    let main = &page[page.find("<main").unwrap_or(0)..];
    let mut out = String::new();
    let mut in_tag = false;
    for c in main.chars() {
        match c {
            '<' => in_tag = true,
            '>' => in_tag = false,
            c if !in_tag => out.push(c),
            _ => {}
        }
    }
    common::unescape(&out)
}

const A1: &str = "a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1";
const A2: &str = "a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2";
const A3: &str = "a3a3a3a3a3a3a3a3a3a3a3a3a3a3a3a3";

/// What the real agents of `trio` gave.
struct Trio {
    /// The requests: one of the known agent, one of the new agent, one with no agent (the bucket of the key).
    one: String,
    two: String,
    three: String,
    /// The ids of the two agents, as the engine made them.
    a1: String,
    a2: String,
    /// The tokens of the two agents, as the channel gave them once.
    tok1: String,
    tok2: String,
}

/// The `User-Agent` of the two agents: the client that the owner reads.
const UA: &str = "claude-code 2.1";

/// The agent that a request belongs to, as the engine wrote it.
fn agent_of(pep: &Pep, request: &str) -> String {
    pep.s
        .scalar(&format!(
            "SELECT agent_id FROM items WHERE request_id = '{request}'"
        ))
        .expect("the request has an agent")
}

/// Three requests of one key, made through the real channel: no SQL writes an agent. The first call of an agent has
/// no token, so the portal makes the agent and gives its token. The owner allows the first agent with a name through the
/// form of the page. The second agent stays new. The third request has no intent, so it goes to the bucket of the key.
fn trio(pep: &Pep) -> Trio {
    let first = pep.agent_request(None, UA, &command(1), json!({"agent_label": "billing-bot"}));
    assert_eq!(first.status, "new", "{first:?}");
    let tok1 = first
        .token
        .clone()
        .expect("the call that made the agent gives its token");
    let a1 = agent_of(pep, &first.request_id);
    let cookie = pep.cookie(OWNER);
    let csrf = csrf_of(pep, &cookie);
    let (st, head, _) = pep.post(
        &cookie,
        ALLOW,
        &csrf,
        &format!("agent_id={a1}&name=Billing+bot"),
        None,
    );
    assert_eq!(st, 303, "{head}");
    let second = pep.agent_request(None, UA, &command(2), json!({"agent_label": "billing-bot"}));
    assert_eq!(second.status, "new", "{second:?}");
    let tok2 = second.token.clone().expect("a token");
    let a2 = agent_of(pep, &second.request_id);
    assert_ne!(a1, a2, "two calls with no token make two agents");
    // The agent that knows its token makes a request of its own: the same agent, no new one.
    let (three, _) = pep.submit(&command(3), 600);
    assert_eq!(
        pep.s.scalar(&format!(
            "SELECT kind FROM agents WHERE agent_id = (SELECT agent_id FROM items WHERE request_id = '{three}')"
        )),
        Some("bucket".to_string()),
        "a submission with no intent goes to the bucket"
    );
    Trio {
        one: first.request_id,
        two: second.request_id,
        three,
        a1,
        a2,
        tok1,
        tok2,
    }
}

// The panel -------------------------------------------------------------------------------------------

#[test]
fn two_agents_with_one_key_have_two_groups_new_first_bucket_last() {
    let pep = enabled(false);
    let cookie = pep.cookie(OWNER);
    let Trio {
        one, two, three, ..
    } = trio(&pep);
    let (st, head, page) = pep.page(&cookie, INBOX);
    assert_eq!(st, 200, "{page}");
    assert_eq!(
        common::approvals::header_of(&head, "cache-control").as_deref(),
        Some("no-store")
    );
    // One group for each agent. The new agent comes first, then the known one, and the bucket is last.
    assert_eq!(page.matches("<details class=\"agrp").count(), 3, "{page}");
    let at = |name: &str| {
        page.find(&format!("<span class=\"agrp__name\">{name}</span>"))
            .unwrap_or_else(|| {
                panic!(
                    "no group {name}: {}",
                    page.split("agrp__name")
                        .skip(1)
                        .map(|x| &x[..x.len().min(60)])
                        .collect::<Vec<_>>()
                        .join(" | ")
                )
            })
    };
    let order = [
        at(&format!("{KEY} #2")),
        at("Billing bot"),
        at(&format!("Unattributed · {KEY}")),
    ];
    assert!(order.windows(2).all(|w| w[0] < w[1]), "{order:?}");
    // Each request is in the group of its agent.
    let group = |id: &str| -> usize { page.find(&item_path(id)).unwrap() };
    assert!(order[0] < group(&two) && group(&two) < order[1]);
    assert!(order[1] < group(&one) && group(&one) < order[2]);
    assert!(order[2] < group(&three));
    // The hints of the agents are in the line "Observed, not verified", not in the title.
    assert!(page.contains("<q>billing-bot</q>") && page.contains("From 127.0.0.0/24"));
    assert!(page.contains("Client claude-code 2.1"));
    assert!(page.contains("Which agent asked is not known."));
    // The row names the service and the action: the key is in the header of the group.
    assert!(page.contains("db · <code>data.write</code>"));
    // The page needs no script.
    assert!(
        !page.contains("<script>") && !page.contains(" onclick=") && !page.contains("javascript:")
    );
    // The tab counts the requests, not the agents.
    assert!(page.contains("nbadge--ask") && page.contains("Requests to answer"));
    assert!(
        page.contains(&format!("href=\"{AGENTS}\"")),
        "the segmented control has Agents"
    );
}

#[test]
fn a_key_without_agents_is_one_group_with_the_name_of_the_key() {
    // The block `inbox.agents` is absent: each key is one agent.
    let pep = Pep::new();
    let cookie = pep.cookie(OWNER);
    let (id, code) = pep.submit_intent(&command(4), json!({}), 600);
    let (_, _, page) = pep.page(&cookie, INBOX);
    assert_eq!(page.matches("<details class=\"agrp").count(), 1);
    assert!(page.contains(&format!("<span class=\"agrp__name\">{KEY}</span>")));
    assert!(
        !page.contains("keychip")
            && !page.contains("pill--un")
            && !page.contains("Observed, not verified")
    );
    assert!(!page.contains("agrp--bucket") && !page.contains("firstuse"));
    assert!(page.contains(&item_path(&id)) && page.contains(&code));
    // The pages of the agents do not exist without the block.
    assert_eq!(pep.page(&cookie, AGENTS).0, 404);
    assert_eq!(pep.page(&cookie, &format!("{AGENTS}/{A1}")).0, 404);
    assert!(!page.contains(&format!("href=\"{AGENTS}\"")));
}

#[test]
fn the_first_use_notice_names_the_choices_and_has_no_locked_row_by_default() {
    let pep = enabled(false);
    let cookie = pep.cookie(OWNER);
    let Trio { two, a2, .. } = trio(&pep);
    let (_, _, page) = pep.page(&cookie, INBOX);
    assert!(page.contains("A new agent is using the key"), "{page}");
    // The forms: name and allow, allow as it is, block, merge. Each one has the token and the id of the agent.
    for action in [ALLOW, BLOCK, MERGE] {
        assert_eq!(hidden_in(&page, action, "agent_id"), a2, "{action}");
        assert!(!hidden_in(&page, action, "csrf").is_empty());
    }
    assert!(
        page.contains("Name and allow") && page.contains("Block") && page.contains("Merge into it")
    );
    assert!(page.contains(&format!("Allow as &quot;{KEY} #2&quot;")));
    // The text "billing-bot" is not the name "Billing bot" after the fold (the hyphen stays): no warning, no false claim.
    assert!(!page.contains("is the same as the name of your agent"));
    // The new agent is not locked: no locked row, no locked text, and the owner can approve.
    assert!(!page.contains("lockline") && !page.contains("Approve is locked"));
    let (_, _, detail) = pep.page(&cookie, &item_path(&two));
    assert!(detail.contains(r#"name="action" value="accept""#) && !detail.contains("lockline"));
    assert!(detail.contains("From agent") && detail.contains("New, not named"));
    assert!(detail.contains(&format!(r#"<span class="sender__name">{KEY} #2</span>"#)));
    assert!(detail.contains("Says it is: <q>billing-bot</q>") && detail.contains("Not verified"));
    let (st, head, _) = pep.answer_form(&cookie, &two, "accept", "");
    assert_eq!(st, 303, "{head}");
    assert!(common::location(&head).ends_with("?s=approved"));
}

#[test]
fn with_lock_new_the_row_the_page_and_the_server_refuse_approve_and_decline_stays_open() {
    let pep = enabled(true);
    let cookie = pep.cookie(OWNER);
    let Trio { two, a2, tok2, .. } = trio(&pep);
    let (_, _, page) = pep.page(&cookie, INBOX);
    assert!(page.contains("Approve is locked until you allow this agent. You can decline now."));
    assert!(
        page.contains("Approve is locked.")
            && page.contains("Approve stays locked until you allow it.")
    );
    let (_, _, detail) = pep.page(&cookie, &item_path(&two));
    assert!(detail.contains("Approve is locked. This agent is new."));
    assert!(!detail.contains(r#"name="action" value="accept""#));
    assert!(detail.contains("button--off") && detail.contains(r#"name="action" value="decline""#));
    // A form that forges the accept is refused by the server: 409, the page that says why, and nothing changes.
    let csrf = hidden_in(&detail, ANSWER, "csrf");
    let effect = hidden_in(&detail, ANSWER, "shown_effect");
    let forged = format!(
        "request_id={two}&action=accept&shown_effect={}",
        common::form_value(&effect)
    );
    let (st, _, body) = pep.post(&cookie, ANSWER, &csrf, &forged, None);
    assert_eq!(st, 409, "{body}");
    assert!(body.contains("Approve is locked until you allow this agent. You can decline now."));
    assert_eq!(pep.verdict(&two)["verdict"], "pending");
    // A decline is open. After the owner allows the agent, the approval works for the next request.
    let (st, head, _) = pep.answer_form(&cookie, &two, "decline", "");
    assert_eq!(st, 303, "{head}");
    assert_eq!(
        pep.verdict_settles(&two, Duration::from_secs(10))["verdict"],
        "denied"
    );
    // The agent sends its next request with its token: the same agent, still locked.
    let again = pep.agent_request(Some(&tok2), UA, &command(5), json!({}));
    assert_eq!(
        (again.status.as_str(), again.token.is_none()),
        ("new", true)
    );
    let next = again.request_id;
    assert_eq!(agent_of(&pep, &next), a2);
    assert_eq!(pep.answer_form(&cookie, &next, "accept", "").0, 409);
    let (st, head, _) = pep.post(
        &cookie,
        ALLOW,
        &csrf,
        &format!("agent_id={a2}&name=Refund+cleaner"),
        None,
    );
    assert_eq!(st, 303, "{head}");
    assert!(common::location(&head).ends_with("?s=agent_allowed"));
    let (st, head, _) = pep.answer_form(&cookie, &next, "accept", "");
    assert_eq!(st, 303, "{head}");
    assert_eq!(pep.verdict(&next)["verdict"], "approved");
}

#[test]
fn hints_are_escaped_and_label_injection_is_rendered_as_text() {
    let pep = enabled(false);
    let cookie = pep.cookie(OWNER);
    seed(
        &pep,
        Seed {
            name: Some("Billing bot"),
            ..Seed::new(A1, "known", 1)
        },
    );
    // The new agent copies the name of the known agent. It hides characters, injects markup and uses a Cyrillic letter.
    seed(
        &pep,
        Seed {
            says: Some("Billing\u{202E} b\u{43e}t <script>alert(1)</script>"),
            client: Some("evil\u{200B}\"><img src=x onerror=alert(2)>"),
            net: Some("203.0.113.0/24"),
            ..Seed::new(A2, "new", 2)
        },
    );
    let (id, _) = pep.submit_intent(&command(6), json!({"explanation": "Run it"}), 600);
    give(&pep, &id, A2);
    for path in [INBOX.to_string(), item_path(&id), format!("{AGENTS}/{A2}")] {
        let (st, _, page) = pep.page(&cookie, &path);
        assert_eq!(st, 200, "{path}");
        for raw in ['\u{202E}', '\u{200B}'] {
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
        assert!(
            page.contains("U+202E") && page.contains("U+200B") && page.contains("U+043E"),
            "{path}"
        );
        assert!(
            page.contains("&lt;script&gt;alert(1)&lt;/script&gt;"),
            "{path}"
        );
        // The text of the agent is never the title of the agent.
        let title = format!("<span class=\"agrp__name\">{KEY} #2</span>");
        assert!(path != INBOX || page.contains(&title));
        assert!(!page.contains("<h2>Billing"), "{path}");
    }
    // The Cyrillic letter does not make the text equal to the name: no false claim. The owner sees the mark.
    let (_, _, page) = pep.page(&cookie, INBOX);
    assert!(!page.contains("is the same as the name of your agent"));
}

#[test]
fn a_text_that_folds_to_the_name_of_a_known_agent_is_flagged() {
    let pep = enabled(false);
    let cookie = pep.cookie(OWNER);
    // The known agent has a name that the owner gave. The new agent says the same text with other letters and spaces.
    let known = pep.agent_request(None, UA, &command(7), json!({"agent_label": "my-reports"}));
    let a1 = agent_of(&pep, &known.request_id);
    let csrf = csrf_of(&pep, &cookie);
    let (st, head, _) = pep.post(
        &cookie,
        ALLOW,
        &csrf,
        &format!("agent_id={a1}&name=Billing+bot"),
        None,
    );
    assert_eq!(st, 303, "{head}");
    let copy = pep.agent_request(
        None,
        UA,
        &command(8),
        json!({"agent_label": "BILLING  Bot"}),
    );
    let (_, _, page) = pep.page(&cookie, INBOX);
    assert!(
        page.contains(
            "is the same as the name of your agent <b>Billing bot</b>. Same client and same address range too."
        ),
        "{page}"
    );
    let (_, _, detail) = pep.page(&cookie, &item_path(&copy.request_id));
    assert!(detail.contains("Same text as the name of your agent Billing bot. This can be a copy."));
}

/// The notice of a new agent names the known agent that it looks like, with the sentence that hints can be copied
///. The text is the same as the text of the known agent, or the client and the range are the same.
/// Nothing merges by itself.
#[test]
fn the_first_use_notice_names_a_similar_known_agent_and_nothing_merges() {
    let pep = enabled(false);
    let cookie = pep.cookie(OWNER);
    // No known agent yet: the first new agent looks like nothing.
    let first = pep.agent_request(None, UA, &command(9), json!({"agent_label": "billing-bot"}));
    let (_, _, page) = pep.page(&cookie, INBOX);
    assert!(page.contains("A new agent is using the key"));
    assert!(!page.contains("Looks like your agent"), "{page}");
    let a1 = agent_of(&pep, &first.request_id);
    let csrf = csrf_of(&pep, &cookie);
    let (st, _, _) = pep.post(
        &cookie,
        ALLOW,
        &csrf,
        &format!("agent_id={a1}&name=Nightly+ETL"),
        None,
    );
    assert_eq!(st, 303);
    // The same text with other letters: the notice names the known agent, by its name and with a link, and says why.
    let copy = pep.agent_request(
        None,
        UA,
        &command(10),
        json!({"agent_label": "Billing-Bot"}),
    );
    let a2 = agent_of(&pep, &copy.request_id);
    let (_, _, page) = pep.page(&cookie, INBOX);
    assert!(
        page.contains(&format!(
            "Looks like your agent <a href=\"{AGENTS}/{a1}\">Nightly ETL</a>: it says the same text. Hints can be copied. Nothing merges by itself."
        )),
        "{page}"
    );
    // The notice is the only place: the page of the known agent has no such line.
    let (_, _, known_page) = pep.page(&cookie, &format!("{AGENTS}/{a1}"));
    assert!(!known_page.contains("Looks like your agent"));
    // The client and the range are the same, and the text is not: the line says that.
    let by_client = pep.agent_request(None, UA, &command(11), json!({"agent_label": "other-name"}));
    let a3 = agent_of(&pep, &by_client.request_id);
    let (_, _, page) = pep.page(&cookie, INBOX);
    assert_eq!(page.matches("Looks like your agent").count(), 2, "{page}");
    assert!(page.contains("it has the same client and the same address range"));
    // Nothing merged: both are still new, and the known agent still holds its own request.
    for id in [&a2, &a3] {
        assert_eq!(
            pep.s
                .scalar(&format!("SELECT state FROM agents WHERE agent_id = '{id}'")),
            Some("new".to_string())
        );
    }
    assert_eq!(agent_of(&pep, &first.request_id), a1);
    // A new agent with another text, another client and no range in common has no line: the user agent differs.
    let stranger = pep.agent_request(
        None,
        "curl/8.5",
        &command(12),
        json!({"agent_label": "report-writer"}),
    );
    assert_eq!(stranger.status, "new");
    let (_, _, page) = pep.page(&cookie, INBOX);
    // The range is the same in this rig (one machine), so the client decides: a different client gives no line.
    assert_eq!(page.matches("Looks like your agent").count(), 2, "{page}");
}

// The agent page and the list -------------------------------------------------------------------------

#[test]
fn the_agent_page_shows_the_agent_and_the_list_reaches_every_agent() {
    let pep = enabled(false);
    let cookie = pep.cookie(OWNER);
    let Trio {
        one, a1, a2, tok1, ..
    } = trio(&pep);
    // The agent says another text and then the first one again: the portal records the two changes as events. The text
    // that stays is the first one.
    for says in ["billing-agent", "billing-bot"] {
        let again = pep.agent_request(Some(&tok1), UA, &command(40), json!({"agent_label": says}));
        assert_eq!(again.status, "known");
    }
    let (st, head, page) = pep.page(&cookie, &format!("{AGENTS}/{a1}"));
    assert_eq!(st, 200, "{page}");
    assert_eq!(
        common::approvals::header_of(&head, "cache-control").as_deref(),
        Some("no-store")
    );
    let t = text(&page);
    for needle in [
        "Billing bot",
        "Known",
        KEY,
        "Open requests",
        "Observed, not verified",
        "Says it is",
        "billing-bot",
        "claude-code 2.1",
        "127.0.0.0/24",
        "Anyone who holds the key can send these texts. They are no proof.",
        "Events",
        "Made by a first call of the key",
        "The text that the agent says about itself changed",
        "Rename",
        "Merge",
        "Block this agent",
        "Agents on it",
    ] {
        assert!(t.contains(needle), "{needle} is not in {t}");
    }
    assert!(
        page.contains(&item_path(&one)),
        "the table of the open requests has a Review link"
    );
    for action in [RENAME, MERGE, BLOCK] {
        assert_eq!(hidden_in(&page, action, "agent_id"), a1);
    }
    assert!(page.contains(&format!("/auth/settings/inbox/items?agent={a1}")));
    // The list shows the three agents, the new one first, and links to each page.
    let (st, _, list) = pep.page(&cookie, AGENTS);
    assert_eq!(st, 200);
    assert!(list.contains(&format!("{AGENTS}/{a2}")) && list.contains(&format!("{AGENTS}/{a1}")));
    assert!(list.find(&format!("{KEY} #2")).unwrap() < list.find("Billing bot").unwrap());
    assert!(
        !text(&list).contains("billing-bot"),
        "the list never shows what an agent said"
    );
    // The page of the bucket has no form.
    let bucket = bucket_id(&pep, &cookie);
    let (_, _, bpage) = pep.page(&cookie, &format!("{AGENTS}/{bucket}"));
    assert!(bpage.contains("This is not an agent.") && !bpage.contains(RENAME));
}

/// The id of the bucket of the key, from its link on the list of the agents.
fn bucket_id(pep: &Pep, cookie: &str) -> String {
    let (_, _, list) = pep.page(cookie, AGENTS);
    let at = list
        .find(&format!("Unattributed · {KEY}"))
        .or_else(|| list.find(&format!("<a href=\"{AGENTS}/")))
        .unwrap();
    let head = &list[..at];
    let link = head.rfind(&format!("{AGENTS}/")).unwrap() + AGENTS.len() + 1;
    list[link..link + 32].to_string()
}

#[test]
fn owner_sees_only_own_agents_and_forms_refuse_foreign_agents() {
    let pep = enabled(false);
    let cookie = pep.cookie(OWNER);
    let Trio { two, a1, a2, .. } = trio(&pep);
    let erin = other_owner(&pep, "erin");
    // Erin sees nothing of dana.
    let (st, _, page) = pep.page(&erin, INBOX);
    assert_eq!(st, 200);
    assert!(!page.contains("agrp") && !page.contains("Billing bot") && !page.contains("firstuse"));
    assert_eq!(pep.page(&erin, &format!("{AGENTS}/{a1}")).0, 404);
    assert_eq!(
        pep.page(&erin, &format!("{AGENTS}/{}", "0".repeat(32))).0,
        404
    );
    assert_eq!(pep.page(&erin, &format!("{AGENTS}/not-an-id")).0, 404);
    let (st, _, list) = pep.page(&erin, AGENTS);
    assert_eq!(st, 200);
    assert!(!list.contains("Billing bot") && list.contains("No agent yet."));
    // Her forms on the agents of dana change nothing and answer as for an agent that does not exist.
    let csrf = csrf_of(&pep, &erin);
    for (action, fields) in [
        (ALLOW, format!("agent_id={a2}&name=Mine")),
        (BLOCK, format!("agent_id={a2}")),
        (UNBLOCK, format!("agent_id={a1}")),
        (RENAME, format!("agent_id={a1}&name=Mine")),
        (MERGE, format!("agent_id={a2}&into={a1}")),
    ] {
        let (st, head, _) = pep.post(&erin, action, &csrf, &fields, None);
        assert_eq!(st, 303, "{action}");
        assert!(
            common::location(&head).ends_with("?s=agent_gone"),
            "{action}: {head}"
        );
    }
    // The agents of dana are as they were: the second one is new, and its request waits.
    let (_, _, page) = pep.page(&cookie, INBOX);
    assert!(page.contains("A new agent is using the key") && page.contains("Billing bot"));
    assert_eq!(pep.verdict(&two)["verdict"], "pending");
    // A merge of an agent of dana into an agent of erin is refused in the same way.
    seed(
        &pep,
        Seed {
            owner: "erin",
            key_id: Some("erin-key"),
            name: Some("Hers"),
            ..Seed::new(A3, "known", 1)
        },
    );
    let csrf = csrf_of(&pep, &cookie);
    let (st, head, _) = pep.post(
        &cookie,
        MERGE,
        &csrf,
        &format!("agent_id={a2}&into={A3}"),
        None,
    );
    assert_eq!(st, 303);
    assert!(common::location(&head).ends_with("?s=agent_gone"), "{head}");
}

#[test]
fn forms_need_csrf_and_origin_and_an_impersonated_session_does_nothing() {
    let pep = enabled(false);
    let cookie = pep.cookie(OWNER);
    let Trio { two, a1, a2, .. } = trio(&pep);
    let csrf = csrf_of(&pep, &cookie);
    for action in [ALLOW, BLOCK, UNBLOCK, RENAME, MERGE] {
        let fields = fields_for(action, &a1, &a2);
        // No token, a wrong token, no origin and another origin: refused.
        assert_eq!(
            pep.post(&cookie, action, "", &fields, None).0,
            403,
            "{action}"
        );
        assert_eq!(
            pep.post(&cookie, action, "wrong", &fields, None).0,
            403,
            "{action}"
        );
        assert_eq!(
            pep.post(&cookie, action, &csrf, &fields, Some("")).0,
            403,
            "{action}"
        );
        assert_eq!(
            pep.post(&cookie, action, &csrf, &fields, Some("http://evil.example"))
                .0,
            403,
            "{action}"
        );
        // A GET of a form route is not a page.
        assert_eq!(pep.page(&cookie, action).0, 405, "{action}");
        // A field that the form does not have, and an id that is not an id: 400.
        let extra = format!("{fields}&extra=1");
        assert_eq!(
            pep.post(&cookie, action, &csrf, &extra, None).0,
            400,
            "{action}"
        );
        assert_eq!(
            pep.post(&cookie, action, &csrf, "agent_id=short&name=x&into=y", None)
                .0,
            400,
            "{action}"
        );
    }
    // The session of "Log in as" reads no agent and acts on none.
    let as_dana = impersonate(&pep, OWNER);
    assert_eq!(pep.page(&as_dana, &format!("{AGENTS}/{a2}")).0, 403);
    assert_eq!(pep.page(&as_dana, AGENTS).0, 403);
    let (st, _, page) = pep.page(&as_dana, INBOX);
    assert_eq!(st, 200);
    assert!(!page.contains("firstuse") && !page.contains("agrp") && !page.contains("Billing bot"));
    let csrf = csrf_of(&pep, &as_dana);
    for action in [ALLOW, BLOCK, UNBLOCK, RENAME, MERGE] {
        assert_eq!(
            pep.post(&as_dana, action, &csrf, &fields_for(action, &a1, &a2), None)
                .0,
            403,
            "{action}"
        );
    }
    // Nothing changed: the agent is still new, and its request still waits.
    let (_, _, page) = pep.page(&cookie, INBOX);
    assert!(page.contains("A new agent is using the key"));
    assert_eq!(pep.verdict(&two)["verdict"], "pending");
}

#[test]
fn rename_allow_block_unblock_merge_work_and_refuse_what_they_must() {
    let pep = enabled(false);
    let cookie = pep.cookie(OWNER);
    let Trio {
        one, two, a1, a2, ..
    } = trio(&pep);
    let csrf = csrf_of(&pep, &cookie);
    let post = |action: &str, fields: String| pep.post(&cookie, action, &csrf, &fields, None);
    // A name that is not valid: 422 with the page of the agent and the name that was typed. Nothing is saved.
    let (st, _, page) = post(RENAME, format!("agent_id={a1}&name=bad%0Aname"));
    assert_eq!(st, 422, "{page}");
    assert!(page.contains("A name has 1 to 32 characters on one line."));
    let (st, _, page) = post(RENAME, format!("agent_id={a1}&name={}", "x".repeat(33)));
    assert_eq!(st, 422);
    assert!(page.contains("A name has 1 to 32 characters"));
    let (st, _, page) = post(RENAME, format!("agent_id={a1}&name=a%E2%80%AEb"));
    assert_eq!(st, 422);
    assert!(!page.contains('\u{202E}'));
    // A name that another agent of the key has, after the fold: 422.
    let (st, _, page) = post(ALLOW, format!("agent_id={a2}&name=BILLING+bot"));
    assert_eq!(st, 422, "{page}");
    assert!(page.contains("Another agent of this key has this name."));
    // The rename works and shows the new name everywhere at once.
    let (st, head, _) = post(RENAME, format!("agent_id={a1}&name=Biller"));
    assert_eq!(st, 303);
    assert!(common::location(&head).ends_with(&format!("{a1}?s=agent_renamed")));
    let (_, _, page) = pep.page(&cookie, INBOX);
    assert!(
        page.contains("<span class=\"agrp__name\">Biller</span>") && !page.contains("Billing bot")
    );
    // The allow with an empty name keeps the number. The new agent leaves the notice.
    let (st, head, _) = post(ALLOW, format!("agent_id={a2}&name="));
    assert_eq!(st, 303);
    assert!(common::location(&head).ends_with("?s=agent_allowed"));
    let (_, _, page) = pep.page(&cookie, INBOX);
    assert!(
        !page.contains("firstuse")
            && page.contains(&format!("<span class=\"agrp__name\">{KEY} #2</span>"))
    );
    assert!(!page.contains("pill--new"));
    // The same action again is a success: it changes nothing.
    assert_eq!(post(ALLOW, format!("agent_id={a2}")).0, 303);
    // A merge across two keys is refused with 422, and a merge into itself with 409.
    seed(
        &pep,
        Seed {
            key_id: Some("other-key"),
            name: Some("Elsewhere"),
            ..Seed::new(A3, "known", 1)
        },
    );
    let (st, _, page) = post(MERGE, format!("agent_id={a2}&into={A3}"));
    assert_eq!(st, 422, "{page}");
    assert!(page.contains("You can merge only agents of the same key."));
    let (st, _, _) = post(MERGE, format!("agent_id={a2}&into={a2}"));
    assert_eq!(st, 409);
    let (st, _, page) = post(MERGE, format!("agent_id={a2}&into=nothing"));
    assert_eq!(st, 422);
    assert!(page.contains("Choose the agent to merge into."));
    // The merge works: the request moves, the agent is removed, and the page of the target opens.
    let (st, head, _) = post(MERGE, format!("agent_id={a2}&into={a1}"));
    assert_eq!(st, 303, "{head}");
    assert!(common::location(&head).ends_with(&format!("{a1}?s=agent_merged")));
    let (_, _, page) = pep.page(&cookie, INBOX);
    assert_eq!(page.matches("<details class=\"agrp").count(), 2);
    let target = page
        .find("<span class=\"agrp__name\">Biller</span>")
        .unwrap();
    assert!(
        page.find(&item_path(&two)).unwrap() > target
            && page.find(&item_path(&one)).unwrap() > target
    );
    let (_, _, merged) = pep.page(&cookie, &format!("{AGENTS}/{a2}"));
    assert!(merged.contains("merged into") && !merged.contains(RENAME));
    // A merged agent takes no other action.
    let (st, _, _) = post(RENAME, format!("agent_id={a2}&name=Again"));
    assert_eq!(st, 409);
}

#[test]
fn a_blocked_agent_loses_its_open_requests_and_can_be_unblocked() {
    let pep = enabled(false);
    let cookie = pep.cookie(OWNER);
    let Trio {
        one,
        two,
        three,
        a1,
        a2,
        ..
    } = trio(&pep);
    let csrf = csrf_of(&pep, &cookie);
    let (st, head, _) = pep.post(&cookie, BLOCK, &csrf, &format!("agent_id={a1}"), None);
    assert_eq!(st, 303, "{head}");
    assert!(common::location(&head).ends_with("?s=agent_blocked"));
    // The requests of the agent are cancelled, and the others wait.
    assert_eq!(pep.verdict(&one)["verdict"], "void");
    assert_eq!(pep.verdict(&two)["verdict"], "pending");
    assert_eq!(pep.verdict(&three)["verdict"], "pending");
    let (st, _, page) = pep.page(&cookie, &format!("{INBOX}?s=agent_blocked"));
    assert_eq!(st, 200);
    assert!(page.contains("The agent is blocked. Its open requests are cancelled."));
    assert!(
        !page.contains("Billing bot"),
        "a group with no open request is not shown"
    );
    // The History says why, and the agent column leads to the history of the agent.
    let (_, _, history) = pep.page(&cookie, &format!("{INBOX}/items"));
    assert!(history.contains("Cancelled: the owner blocked the agent."));
    assert!(
        history.contains(&format!("{INBOX}/items?agent={a1}")) && history.contains("Billing bot")
    );
    // The page of the blocked agent shows the state, the count in the events and Unblock only.
    let (_, _, agent) = pep.page(&cookie, &format!("{AGENTS}/{a1}"));
    assert!(agent.contains("Blocked") && agent.contains("1 open request cancelled."));
    assert!(
        agent.contains(&format!("action=\"{UNBLOCK}\""))
            && !agent.contains(RENAME)
            && !agent.contains(BLOCK)
    );
    // Unblock gives a known agent again. It revives nothing.
    let (st, head, _) = pep.post(&cookie, UNBLOCK, &csrf, &format!("agent_id={a1}"), None);
    assert_eq!(st, 303);
    assert!(common::location(&head).ends_with(&format!("{a1}?s=agent_unblocked")));
    let (_, _, agent) = pep.page(&cookie, &format!("{AGENTS}/{a1}"));
    assert!(agent.contains(&format!("action=\"{BLOCK}\"")) && agent.contains("Unblocked by"));
    assert_eq!(pep.verdict(&one)["verdict"], "void");
    // A new agent can be blocked too: its request is cancelled.
    assert_eq!(
        pep.post(&cookie, BLOCK, &csrf, &format!("agent_id={a2}"), None)
            .0,
        303
    );
    assert_eq!(pep.verdict(&two)["verdict"], "void");
    // Unblock gives a known agent. The same action again is a success that changes nothing.
    assert_eq!(
        pep.post(&cookie, UNBLOCK, &csrf, &format!("agent_id={a2}"), None)
            .0,
        303
    );
    assert_eq!(
        pep.post(&cookie, UNBLOCK, &csrf, &format!("agent_id={a2}"), None)
            .0,
        303
    );
}

// The History -----------------------------------------------------------------------------------------

#[test]
fn history_filter_by_agent_and_the_agent_column() {
    let pep = enabled(false);
    let cookie = pep.cookie(OWNER);
    let Trio {
        one,
        two,
        three,
        a1,
        a2,
        ..
    } = trio(&pep);
    for id in [&one, &two, &three] {
        assert_eq!(pep.answer_form(&cookie, id, "decline", "").0, 303);
    }
    let (st, _, page) = pep.page(&cookie, &format!("{INBOX}/items"));
    assert_eq!(st, 200);
    // The column names the agent, with a link to its filter, and the key after it.
    assert!(
        page.contains(&format!(
            "<a href=\"{INBOX}/items?agent={a1}\">Billing bot</a>"
        )),
        "{page}"
    );
    assert!(page.contains(&format!(
        "<a href=\"{INBOX}/items?agent={a2}\">{KEY} #2</a>"
    )));
    assert!(page.contains(&format!("Unattributed · {KEY}")));
    for id in [&one, &two, &three] {
        assert!(page.contains(&item_path(id)));
    }
    // The filter keeps the requests of one agent, shows a chip with its name and a link that removes it.
    let (st, _, page) = pep.page(&cookie, &format!("{INBOX}/items?agent={a1}"));
    assert_eq!(st, 200);
    assert!(
        page.contains(&item_path(&one))
            && !page.contains(&item_path(&two))
            && !page.contains(&item_path(&three))
    );
    assert!(page.contains("histAgent") && page.contains("Agent <b>Billing bot</b>"));
    assert!(
        page.contains(&format!("name=\"agent\" value=\"{a1}\"")),
        "the search form keeps the filter"
    );
    // A bad id is a bad request. The agent of another owner shows nothing and no name.
    assert_eq!(
        pep.page(&cookie, &format!("{INBOX}/items?agent=abc")).0,
        400
    );
    let erin = other_owner(&pep, "erin");
    let (st, _, page) = pep.page(&erin, &format!("{INBOX}/items?agent={a1}"));
    assert_eq!(st, 200);
    assert!(
        page.contains("No request matches.")
            && page.contains("Agent <b>unknown</b>")
            && !page.contains("Billing bot")
    );
    // A merged agent gives its target: the old link still works.
    let csrf = csrf_of(&pep, &cookie);
    assert_eq!(
        pep.post(
            &cookie,
            MERGE,
            &csrf,
            &format!("agent_id={a2}&into={a1}"),
            None
        )
        .0,
        303
    );
    let (_, _, page) = pep.page(&cookie, &format!("{INBOX}/items?agent={a2}"));
    assert!(
        page.contains(&item_path(&one))
            && page.contains(&item_path(&two))
            && page.contains("Agent <b>Billing bot</b>")
    );
}

// The product name ------------------------------------------------------------------------------------

#[test]
fn pages_of_the_agents_never_name_the_product_and_carry_no_store() {
    let pep = enabled(true);
    let cookie = pep.cookie(OWNER);
    let Trio { two, a1, a2, .. } = trio(&pep);
    let root = pep.s.session().cookie.clone();
    for path in [
        INBOX.to_string(),
        AGENTS.to_string(),
        format!("{AGENTS}/{a1}"),
        format!("{AGENTS}/{a2}"),
        item_path(&two),
        format!("{INBOX}/items"),
        format!("{INBOX}/items?agent={a1}"),
    ] {
        let (st, head, page) = pep.page(&cookie, &path);
        assert_eq!(st, 200, "{path}");
        assert_eq!(
            common::approvals::header_of(&head, "cache-control").as_deref(),
            Some("no-store"),
            "{path}"
        );
        let body = Pep::text_of(&page);
        assert!(!body.contains("portal"), "{path} has the word portal");
        assert!(!page.contains("<script>"), "{path}");
    }
    // The superadmin has no agent: the list is empty, and the agents of dana are not found.
    assert_eq!(pep.page(&root, &format!("{AGENTS}/{a1}")).0, 404);
}

// The mode `enrolled` -------------------------------------------------------------------------

const ENROLL: &str = "/auth/settings/inbox/agents/enroll";
const RESET: &str = "/auth/settings/inbox/agents/reset";

/// A server whose key is in the mode `enrolled`. `max` is `inbox.agents.max_per_key`.
fn enrolled_rig(max: u32) -> Pep {
    Pep::with_mode(
        |s| {
            s.add_to_inbox(&format!(
                "  agents {{ lock_new = true max_per_key = {max} }}"
            ));
        },
        Some("enrolled"),
    )
}

/// The value of the input `id` of a page.
fn input_value(page: &str, id: &str) -> String {
    let marker = format!("id=\"{id}\" type=\"text\" readonly value=\"");
    let at = page.find(&marker).unwrap_or_else(|| panic!("no {id}")) + marker.len();
    common::unescape(page[at..].split('"').next().unwrap())
}

/// A call of the agent that makes an item. The result is the flag `isError` and the answer.
fn intent_with(pep: &Pep, token: Option<&str>) -> (bool, serde_json::Value) {
    pep.tool_as(token, UA, "approval_intent", json!({"lease_s": 600}))
}

#[test]
fn enrolled_cycle_enroll_call_with_and_without_the_token_reset_old_token_dead_new_works() {
    let pep = enrolled_rig(8);
    let cookie = pep.cookie(OWNER);
    // The list has the form "Add agent" with the key of the owner, and no agent.
    let (st, _, list) = pep.page(&cookie, AGENTS);
    assert_eq!(st, 200);
    assert!(list.contains(&format!("action=\"{ENROLL}\"")), "{list}");
    assert!(list.contains(&format!("<option value=\"{}\">{KEY}</option>", pep.key_id)));
    assert!(text(&list).contains("shows it one time"));
    // A call with no token is refused. It makes no agent.
    let (is_error, e) = intent_with(&pep, None);
    assert!(
        is_error && e["error"]["code"] == "agent_token_required",
        "{e}"
    );
    assert_eq!(
        pep.s
            .scalar("SELECT COUNT(*) FROM agents WHERE kind = 'agent'"),
        Some("0".into())
    );

    // Enroll: the page shows the token one time, in a box with a copy button, and the header line.
    let (st, head, page) = pep.enroll_via_web(&cookie, "Refund cleaner");
    assert_eq!(st, 200, "{page}");
    assert_eq!(
        common::approvals::header_of(&head, "cache-control").as_deref(),
        Some("no-store")
    );
    let referrer = common::approvals::header_of(&head, "referrer-policy").unwrap_or_default();
    assert!(
        referrer == "same-origin" || referrer == "no-referrer",
        "{referrer}"
    );
    let token = Pep::token_of(&page);
    assert!(token.starts_with("agt_") && token.len() == 47, "{token}");
    assert_eq!(
        input_value(&page, "agent-header"),
        format!("Agent-Token: {token}")
    );
    assert!(
        page.contains("data-copy=\"agent-token\"") && page.contains("data-copy=\"agent-header\"")
    );
    let t = text(&page);
    for needle in [
        "Refund cleaner",
        "shows it one time",
        "Copy its token now",
        "agent_token",
        "A call with no token, or with a wrong token, is refused.",
    ] {
        assert!(t.contains(needle), "{needle} is not in {t}");
    }
    assert!(!Pep::text_of(&page).contains("portal"), "the word portal");
    assert!(!page.contains("<script>"));
    // The row: known, made by the owner, named, with a digest and no token.
    let row = |col: &str| {
        pep.s.scalar(&format!(
            "SELECT {col} FROM agents WHERE kind = 'agent' AND name = 'Refund cleaner'"
        ))
    };
    assert_eq!(row("state"), Some("known".into()));
    assert_eq!(row("origin"), Some("owner".into()));
    let id = row("agent_id").unwrap();
    assert_ne!(row("token_digest").unwrap(), token);
    assert_eq!(row("token_digest").unwrap().len(), 64);

    // The call with the token works as that agent. It is known: no notice, no lock, no first-use text.
    let first = pep.agent_request(
        Some(&token),
        UA,
        &command(1),
        json!({"agent_label": "refunds"}),
    );
    assert_eq!(first.status, "known", "{first:?}");
    assert_eq!(agent_of(&pep, &first.request_id), id);
    let (_, _, inbox) = pep.page(&cookie, INBOX);
    assert!(
        inbox.contains("<span class=\"agrp__name\">Refund cleaner</span>"),
        "{inbox}"
    );
    assert!(!inbox.contains("firstuse") && !inbox.contains("A new agent is using"));
    assert!(!text(&inbox).contains("Approve is locked"));
    // The owner can approve at once, although `lock_new` is on: the agent never was new.
    let (st, head, _) = pep.answer_form(&cookie, &first.request_id, "accept", "");
    assert_eq!(st, 303, "{head}");
    assert_eq!(pep.verdict(&first.request_id)["verdict"], "approved");
    // A call with no token, and a call with a token of another form or another agent, are refused.
    let (is_error, e) = intent_with(&pep, None);
    assert!(
        is_error && e["error"]["code"] == "agent_token_required",
        "{e}"
    );
    let (is_error, e) = intent_with(&pep, Some("agt_not-a-token"));
    assert!(is_error && e["error"]["code"] == "agent_unknown", "{e}");
    // The agent page has the button "Reset token" and the warning.
    let (st, _, agent_page) = pep.page(&cookie, &format!("{AGENTS}/{id}"));
    assert_eq!(st, 200);
    assert!(agent_page.contains(&format!("action=\"{RESET}\"")));
    assert_eq!(hidden_in(&agent_page, RESET, "agent_id"), id);
    let t = text(&agent_page);
    assert!(t.contains("Reset token") && t.contains("The old token stops at once."));
    assert!(t.contains(&format!("Added by {OWNER}")), "{t}");

    // Reset: the page shows the new token one time. The old token is dead at once.
    let (st, head, page) = pep.reset_via_web(&cookie, &id);
    assert_eq!(st, 200, "{page}");
    assert_eq!(
        common::approvals::header_of(&head, "cache-control").as_deref(),
        Some("no-store")
    );
    let fresh = Pep::token_of(&page);
    assert_ne!(fresh, token);
    assert!(text(&page).contains("The old token stopped at once"));
    let (is_error, e) = intent_with(&pep, Some(&token));
    assert!(
        is_error && e["error"]["code"] == "agent_unknown",
        "the old token: {e}"
    );
    let second = pep.agent_request(Some(&fresh), UA, &command(2), json!({}));
    assert_eq!(second.status, "known");
    assert_eq!(
        agent_of(&pep, &second.request_id),
        id,
        "the same agent after the reset"
    );
    // The agent keeps its name, its state and its request.
    let (_, _, agent_page) = pep.page(&cookie, &format!("{AGENTS}/{id}"));
    let t = text(&agent_page);
    assert!(t.contains("Refund cleaner") && t.contains("Known"));
    assert!(
        t.contains("Token reset by") && t.contains("The old token stopped at once."),
        "{t}"
    );

    // The tokens are on no other page, in no history, in no event, in no row of the database and in no log line.
    for path in [
        INBOX.to_string(),
        AGENTS.to_string(),
        format!("{AGENTS}/{id}"),
        item_path(&first.request_id),
        item_path(&second.request_id),
        format!("{INBOX}/items"),
        format!("{INBOX}/items?agent={id}"),
        "/auth/settings".to_string(),
    ] {
        let (_, head, page) = pep.page(&cookie, &path);
        for t in [&token, &fresh] {
            assert!(!page.contains(&t[4..]), "{path} holds a token");
        }
        assert!(!head.contains(&token[4..]));
    }
    let database = pep.s.database_text();
    let log = pep.s.log();
    for t in [&token, &fresh] {
        assert!(!database.contains(&t[4..]), "the database holds a token");
        assert!(!log.contains(&t[4..]), "a log line holds a token");
    }
    assert!(log.contains("\"event\":\"agent_enrolled\""));
    assert!(log.contains("\"op\":\"reset_token\""));
    // The events hold ids and numbers. The reset has no detail.
    assert_eq!(
        pep.s.scalar(&format!(
            "SELECT COUNT(*) FROM agent_events WHERE agent_id = '{id}' AND event = 'token_reset' AND detail IS NULL AND actor = '{OWNER}'"
        )),
        Some("1".into())
    );
}

#[test]
fn the_service_at_the_end_of_the_chain_never_sees_the_token_of_an_enrolled_agent() {
    let pep = enrolled_rig(8);
    let cookie = pep.cookie(OWNER);
    let (_, _, page) = pep.enroll_via_web(&cookie, "Refund cleaner");
    let token = Pep::token_of(&page);
    // The agent calls with the token in the header and in the argument; the service asks the PDP and submits the request.
    // What the service reads in each answer holds no token, no `agt_` and no word of the agents.
    let (is_error, intent) = pep.tool_as(
        Some(&token),
        UA,
        "approval_intent",
        json!({"lease_s": 600, "agent_label": "canary-label-7c1e", "agent_token": token}),
    );
    assert!(!is_error, "{intent}");
    assert_eq!(intent["agent_status"], "known");
    let reference = intent["approval_ref"].as_str().unwrap();
    let evaluation = pep.evaluation(&command(5).effect, false).to_string();
    let reply = pep.submit_with(&command(5), json!({"approval_ref": reference}));
    assert!(reply.status == 200 || reply.status == 201, "{}", reply.body);
    let verdict = pep
        .verdict(reply.json()["request_id"].as_str().unwrap())
        .to_string();
    for seen in [evaluation, reply.body.clone(), verdict] {
        for needle in [
            &token[4..],
            "agt_",
            "canary-label-7c1e",
            "Refund cleaner",
            "agent_token",
            "agent_id",
        ] {
            assert!(!seen.contains(needle), "the service saw {needle:?}: {seen}");
        }
    }
}

#[test]
fn also_asked_by_names_the_other_agents_that_asked_the_same_effect_with_text_of_the_server() {
    let pep = enrolled_rig(8);
    let cookie = pep.cookie(OWNER);
    let enroll = |name: &str| {
        let (st, _, page) = pep.enroll_via_web(&cookie, name);
        assert_eq!(st, 200, "{page}");
        Pep::token_of(&page)
    };
    let (ta, tb, tc) = (
        enroll("Night ETL"),
        enroll("Report writer"),
        enroll("Third one"),
    );
    let first = pep.agent_request(
        Some(&ta),
        UA,
        &command(9),
        json!({"agent_label": "etl-says"}),
    );
    // Two other agents ask for the same effect while the request is open. They get the same request.
    let by_b = pep.agent_request(
        Some(&tb),
        UA,
        &command(9),
        json!({"agent_label": "SNEAKY-<b>says</b>"}),
    );
    let by_c = pep.agent_request(
        Some(&tc),
        UA,
        &command(9),
        json!({"agent_label": "third-says"}),
    );
    assert_eq!(
        (&by_b.request_id, &by_c.request_id),
        (&first.request_id, &first.request_id)
    );
    let (st, _, page) = pep.page(&cookie, &item_path(&first.request_id));
    assert_eq!(st, 200);
    assert!(page.contains("class=\"sender__also\""), "{page}");
    let t = text(&page);
    assert!(t.contains("Also asked byReport writer, Third one"), "{t}");
    // The sender stays the first agent. The text of the other agents is on no part of the page.
    assert!(t.contains("Night ETL"));
    assert!(page.contains("etl-says"));
    for words in ["SNEAKY", "third-says"] {
        assert!(!page.contains(words), "{words} is on the page");
    }
    // The names link to the agent pages of the owner.
    let id_of = |name: &str| {
        pep.s
            .scalar(&format!(
                "SELECT agent_id FROM agents WHERE name = '{name}'"
            ))
            .unwrap()
    };
    assert!(page.contains(&format!(
        "href=\"{AGENTS}/{}\">Report writer</a>",
        id_of("Report writer")
    )));
    // A request that nobody else asked for has no line.
    let alone = pep.agent_request(Some(&ta), UA, &command(10), json!({}));
    let (_, _, page) = pep.page(&cookie, &item_path(&alone.request_id));
    assert!(!page.contains("sender__also") && !text(&page).contains("Also asked by"));
}

#[test]
fn enroll_and_reset_need_csrf_origin_the_owner_and_a_key_in_the_mode_enrolled() {
    let pep = enrolled_rig(2);
    let cookie = pep.cookie(OWNER);
    let (side_id, _) = pep.make_key_in_mode("side-key", "discover");
    let (st, _, page) = pep.enroll_via_web(&cookie, "Mine");
    assert_eq!(st, 200, "{page}");
    let id = pep
        .s
        .scalar("SELECT agent_id FROM agents WHERE name = 'Mine'")
        .unwrap();
    let csrf = csrf_of(&pep, &cookie);
    let enroll_fields = format!("key={}&name=Other", pep.key_id);
    let reset_fields = format!("agent_id={id}");
    for (action, fields) in [(ENROLL, &enroll_fields), (RESET, &reset_fields)] {
        assert_eq!(
            pep.post(&cookie, action, "", fields, None).0,
            403,
            "{action}"
        );
        assert_eq!(pep.post(&cookie, action, "wrong", fields, None).0, 403);
        assert_eq!(pep.post(&cookie, action, &csrf, fields, Some("")).0, 403);
        assert_eq!(
            pep.post(&cookie, action, &csrf, fields, Some("http://evil.example"))
                .0,
            403
        );
        assert_eq!(pep.page(&cookie, action).0, 405, "a GET is not a page");
        assert_eq!(
            pep.post(&cookie, action, &csrf, &format!("{fields}&extra=1"), None)
                .0,
            400
        );
    }
    assert_eq!(pep.post(&cookie, ENROLL, &csrf, "name=x", None).0, 400);
    assert_eq!(
        pep.post(&cookie, RESET, &csrf, "agent_id=short", None).0,
        400
    );
    // Nothing changed after the refusals: one agent.
    let count = || {
        pep.s
            .scalar("SELECT COUNT(*) FROM agents WHERE kind = 'agent'")
    };
    assert_eq!(count(), Some("1".into()));

    // The session of "Log in as" reads no form and acts on none. Erin is made first: the root session is used for it.
    let erin = other_owner(&pep, "erin");
    let as_dana = impersonate(&pep, OWNER);
    let icsrf = csrf_of(&pep, &as_dana);
    assert_eq!(
        pep.post(&as_dana, ENROLL, &icsrf, &enroll_fields, None).0,
        403
    );
    assert_eq!(
        pep.post(&as_dana, RESET, &icsrf, &reset_fields, None).0,
        403
    );
    assert_eq!(count(), Some("1".into()));

    // Another owner: the key, the agent and the pages are the same as unknown ones.
    let ecsrf = csrf_of(&pep, &erin);
    let (st, _, foreign) = pep.post(&erin, ENROLL, &ecsrf, &enroll_fields, None);
    let (st_unknown, _, unknown) = pep.post(
        &erin,
        ENROLL,
        &ecsrf,
        &format!("key={}&name=Other", "9".repeat(32)),
        None,
    );
    assert_eq!((st, st_unknown), (422, 422));
    assert!(text(&foreign).contains("Choose a key in the mode Enrolled."));
    assert!(!foreign.contains("id=\"agent-token\"") && !unknown.contains("id=\"agent-token\""));
    let (st, head, _) = pep.post(&erin, RESET, &ecsrf, &reset_fields, None);
    assert_eq!(st, 303, "{head}");
    assert!(head.contains("s=agent_gone"), "{head}");
    let (st_missing, head_missing, _) = pep.post(
        &erin,
        RESET,
        &ecsrf,
        &format!("agent_id={}", "8".repeat(32)),
        None,
    );
    assert_eq!(
        (st_missing, head_missing.contains("s=agent_gone")),
        (303, true)
    );
    assert_eq!(count(), Some("1".into()));
    // Erin has no key in the mode `enrolled`: her list has no form.
    assert!(!pep.page(&erin, AGENTS).2.contains(ENROLL));
    // The old token of Mine still works: the foreign reset changed nothing.
    let digest = pep
        .s
        .scalar(&format!(
            "SELECT token_digest FROM agents WHERE agent_id = '{id}'"
        ))
        .unwrap();
    let token = Pep::token_of(&page);
    assert_eq!(digest, hex(&Sha256::digest(token.as_bytes())));

    // A key of another mode of the owner: the same answer as an unknown key.
    let (st, _, page) = pep.post(
        &cookie,
        ENROLL,
        &csrf,
        &format!("key={side_id}&name=Nope"),
        None,
    );
    assert_eq!(st, 422, "{page}");
    assert!(text(&page).contains("Choose a key in the mode Enrolled."));
    // The name rules of a rename apply, and the cap of the key applies (here two agents).
    for bad in ["", "%20x", "a%09b", "a%E2%80%AEb", "x%E2%80%8Bx"] {
        let (st, _, page) = pep.post(
            &cookie,
            ENROLL,
            &csrf,
            &format!("key={}&name={bad}", pep.key_id),
            None,
        );
        assert_eq!(st, 422, "{bad}");
        assert!(
            text(&page).contains("A name has 1 to 32 characters"),
            "{bad}"
        );
    }
    let (st, _, page) = pep.post(
        &cookie,
        ENROLL,
        &csrf,
        &format!("key={}&name=MINE", pep.key_id),
        None,
    );
    assert_eq!(st, 422);
    assert!(text(&page).contains("Another agent of this key has this name"));
    // The refused name comes back in the form.
    assert!(page.contains("value=\"MINE\""));
    let (st, _, _) = pep.post(
        &cookie,
        ENROLL,
        &csrf,
        &format!("key={}&name=Second", pep.key_id),
        None,
    );
    assert_eq!(st, 200);
    let (st, _, page) = pep.post(
        &cookie,
        ENROLL,
        &csrf,
        &format!("key={}&name=Third", pep.key_id),
        None,
    );
    assert_eq!(st, 409, "{page}");
    assert!(text(&page).contains("the most agents that the file allows"));
    assert_eq!(count(), Some("2".into()));
}

#[test]
fn a_key_that_is_not_enrolled_has_no_form_add_agent_and_no_reset() {
    let pep = enabled(false);
    let cookie = pep.cookie(OWNER);
    let Trio { a1, .. } = trio(&pep);
    let (_, _, list) = pep.page(&cookie, AGENTS);
    assert!(!list.contains(ENROLL) && !text(&list).contains("Add agent"));
    let (_, _, page) = pep.page(&cookie, &format!("{AGENTS}/{a1}"));
    assert!(!page.contains(RESET) && !text(&page).contains("Reset token"));
    // The server refuses the forms too.
    let csrf = csrf_of(&pep, &cookie);
    let (st, _, page) = pep.post(
        &cookie,
        ENROLL,
        &csrf,
        &format!("key={}&name=Mine", pep.key_id),
        None,
    );
    assert_eq!(st, 422, "{page}");
    assert!(!page.contains("id=\"agent-token\""));
    // An agent that a call made has no token to reset, and a key that is not enrolled does not reset any.
    let (st, _, page) = pep.post(&cookie, RESET, &csrf, &format!("agent_id={a1}"), None);
    assert_eq!(st, 409, "{page}");
    assert!(!page.contains("id=\"agent-token\""));
    // Without the block `inbox.agents` the form does not exist.
    let plain = Pep::new();
    let cookie = plain.cookie(OWNER);
    assert_eq!(plain.page(&cookie, AGENTS).0, 404);
    let csrf = csrf_of(&plain, &cookie);
    let (st, _, _) = plain.post(
        &cookie,
        ENROLL,
        &csrf,
        &format!("key={}&name=Mine", plain.key_id),
        None,
    );
    assert_eq!(st, 403);
}

// The eyes --------------------------------------------------------------------------------------------

/// Starts a server with the agents in every state and waits until the file `stop` appears in the directory that
/// `INBOX_DEMO_OUT` names. The test is for a look at the pages with a browser (`tools/dev/ui-check-agents.js`). It writes
/// `demo.json` with the port, the logins, the passwords and the ids. Run it with
/// `cargo test --test agents_web_e2e agents_demo_for_the_eyes -- --ignored`.
#[test]
#[ignore = "starts a server for a look at the pages"]
fn agents_demo_for_the_eyes() {
    let out = std::path::PathBuf::from(std::env::var("INBOX_DEMO_OUT").expect("INBOX_DEMO_OUT"));
    let lock = std::env::var("INBOX_DEMO_LOCK").is_ok_and(|v| v == "1");
    let pep = enabled(lock);
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
    for (n, title) in ["Disk 86 % on db-2", "Backup finished"].iter().enumerate() {
        let (st, _) = common::post_alert(
            &pep.s,
            &format!("demo-{n}"),
            json!({"group": "infra", "title": title}),
        );
        assert_eq!(st, 201);
    }
    // Three agents that the owner knows, one new agent that copies a name, and the bucket.
    let b = "b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1";
    let c = "c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1";
    let d = "d1d1d1d1d1d1d1d1d1d1d1d1d1d1d1d1";
    let e = "e1e1e1e1e1e1e1e1e1e1e1e1e1e1e1e1";
    seed(
        &pep,
        Seed {
            name: Some("Billing bot"),
            says: Some("billing-bot"),
            client: Some("claude-code 2.1"),
            net: Some("203.0.113.0/24"),
            created_ago_s: 7 * 86_400,
            ..Seed::new(A1, "known", 1)
        },
    );
    seed(
        &pep,
        Seed {
            name: Some("Night ETL"),
            says: Some("etl-nightly"),
            client: Some("python-httpx 0.28"),
            net: Some("198.51.100.0/24"),
            created_ago_s: 20 * 86_400,
            ..Seed::new(b, "known", 2)
        },
    );
    seed(
        &pep,
        Seed {
            name: Some("Report writer"),
            says: Some("report-writer"),
            client: Some("curl 8.5"),
            net: Some("192.0.2.0/24"),
            created_ago_s: 30 * 86_400,
            ..Seed::new(c, "known", 3)
        },
    );
    seed(
        &pep,
        Seed {
            says: Some("Billing Bot"),
            client: Some("claude-code 2.1"),
            net: Some("203.0.113.0/24"),
            created_ago_s: 90,
            ..Seed::new(A2, "new", 4)
        },
    );
    seed(
        &pep,
        Seed {
            name: Some("Retired job"),
            says: Some("old-cron"),
            client: Some("curl 8.5"),
            net: Some("192.0.2.0/24"),
            created_ago_s: 40 * 86_400,
            ..Seed::new(d, "blocked", 5)
        },
    );
    seed(
        &pep,
        Seed {
            says: Some("Billing\u{202E} b\u{43e}t <i>x</i>"),
            client: Some("evil\u{200B}client"),
            net: Some("203.0.113.0/24"),
            created_ago_s: 60,
            ..Seed::new(e, "new", 6)
        },
    );
    let mut ids = serde_json::Map::new();
    let one = |n: u32, title: &str, agent: Option<&str>| {
        let (id, _) = pep.submit_intent(
            &common::approvals::command_with(
                &format!("DELETE FROM payments.refunds WHERE batch = {n}\n  AND status = 'settled'"),
                title,
            ),
            json!({"explanation": "Clean up refunds older than 6 months, as asked in ticket 4412.", "language": "sql"}),
            3000,
        );
        if let Some(agent) = agent {
            give(&pep, &id, agent);
        }
        id
    };
    ids.insert(
        "billing1".into(),
        json!(one(
            1,
            "Run a write command on ClickHouse (payments.refunds)",
            Some(A1)
        )),
    );
    ids.insert(
        "billing2".into(),
        json!(one(
            2,
            "Run a write command on ClickHouse (payments.ledger)",
            Some(A1)
        )),
    );
    ids.insert(
        "night".into(),
        json!(one(
            3,
            "Run a write command on ClickHouse (staging.events_tmp)",
            Some(b)
        )),
    );
    ids.insert(
        "report".into(),
        json!(one(
            4,
            "Run a write command on ClickHouse (reports.daily)",
            Some(c)
        )),
    );
    ids.insert(
        "new".into(),
        json!(one(
            5,
            "Run a write command on ClickHouse (payments.refunds)",
            Some(A2)
        )),
    );
    ids.insert(
        "copy".into(),
        json!(one(
            6,
            "Run a write command on ClickHouse (payments.audit)",
            Some(e)
        )),
    );
    ids.insert(
        "bucket".into(),
        json!(one(
            7,
            "Run a write command on ClickHouse (analytics.sessions)",
            None
        )),
    );
    ids.insert(
        "history".into(),
        json!(one(
            8,
            "Run a write command on ClickHouse (old.table)",
            Some(A1)
        )),
    );
    let owner = pep.cookie(OWNER);
    assert_eq!(
        pep.answer_form(&owner, ids["history"].as_str().unwrap(), "decline", "")
            .0,
        303
    );
    // The ids of the agents are in `ids` too: `tools/dev/ui-check.sh` passes one object to the script.
    for (name, id) in [
        ("agent_billing", A1),
        ("agent_new", A2),
        ("agent_night", b),
        ("agent_report", c),
        ("agent_blocked", d),
        ("agent_copy", e),
    ] {
        ids.insert(name.into(), json!(id));
    }
    let ids = serde_json::Value::Object(ids);
    let ann = "ann";
    pep.s.account_with(
        ann,
        &[
            "access on service:portal",
            "approval.read on service:portal",
            "approval.cancel on service:portal",
        ],
    );
    std::fs::write(
        out.join("demo.json"),
        json!({"port": pep.s.port, "owner": OWNER, "password": common::ACCOUNT_PASSWORD,
               "root_password": common::SUPERADMIN_PASSWORD, "admin": ann, "lock": lock, "ids": ids})
        .to_string(),
    )
    .unwrap();
    while !out.join("stop").exists() {
        std::thread::sleep(Duration::from_millis(300));
    }
}

/// Starts a server whose key is in the mode `enrolled`, with two agents that the owner made and a request that both agents asked
/// for, and waits until the file `stop` appears in the directory that `INBOX_DEMO_OUT` names. The test is for a look at the
/// pages with a browser (`tools/dev/ui-check-agents-enrolled.js`). It writes `demo.json` with the port, the login, the password
/// and the ids. Run it with `cargo test --test agents_web_e2e agents_enrolled_demo_for_the_eyes -- --ignored`.
#[test]
#[ignore = "starts a server for a look at the pages"]
fn agents_enrolled_demo_for_the_eyes() {
    let out = std::path::PathBuf::from(std::env::var("INBOX_DEMO_OUT").expect("INBOX_DEMO_OUT"));
    let pep = enrolled_rig(12);
    let cookie = pep.cookie(OWNER);
    let enroll = |name: &str| {
        let (st, _, page) = pep.enroll_via_web(&cookie, name);
        assert_eq!(st, 200, "{page}");
        Pep::token_of(&page)
    };
    let (night, report) = (enroll("Night ETL"), enroll("Report writer"));
    let first = pep.agent_request(Some(&night), UA, &command(9), json!({"agent_label": "etl"}));
    let same = pep.agent_request(Some(&report), UA, &command(9), json!({"agent_label": "rw"}));
    assert_eq!(first.request_id, same.request_id);
    let id_of = |name: &str| {
        pep.s
            .scalar(&format!(
                "SELECT agent_id FROM agents WHERE name = '{name}'"
            ))
            .unwrap()
    };
    std::fs::write(
        out.join("demo.json"),
        json!({"port": pep.s.port, "owner": OWNER, "password": common::ACCOUNT_PASSWORD,
               "ids": {"night": id_of("Night ETL"), "report": id_of("Report writer"),
                       "request": first.request_id, "key": pep.key_id}})
        .to_string(),
    )
    .unwrap();
    while !out.join("stop").exists() {
        std::thread::sleep(Duration::from_millis(300));
    }
}
