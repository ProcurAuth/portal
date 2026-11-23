//! End-to-end tests of the preferences: the page "Inbox > Preferences", the frames `set_prefs` and `welcome` of
//! the phone, and the quiet flag of a request. The rig is the one of the approvals. The pages have no script, so each
//! check reads the HTML. The digest and its clock have unit tests that move the clock by hand
//! (`src/inbox/items_tests.rs`, `src/http/prefs_tests.rs`): a real server cannot.

mod common;

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use common::approvals::{command, hidden_in, next_request, Pep, INBOX, OWNER};
use common::{http_full, users_csrf, Conn, Recv};
use inbox_core::proto::{ClientMsg, DigestMode, DigestPrefs, Prefs, QuietPrefs, ServerMsg};

const PREFS: &str = "/auth/settings/inbox/prefs";
const WAIT: Duration = Duration::from_secs(10);

/// The session cookie that the superadmin gets when it opens a session as another account.
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

/// The anti-CSRF token of the page of the preferences, for a session that may use it.
fn csrf_of_prefs(pep: &Pep, cookie: &str) -> String {
    let (st, _, page) = pep.page(cookie, PREFS);
    assert_eq!(st, 200, "{page}");
    hidden_in(&page, PREFS, "csrf")
}

/// The token of any form of the main page: the session of "Log in as" has no page of preferences to read it from.
fn csrf_of_any_form(pep: &Pep, cookie: &str) -> String {
    let (_, _, page) = pep.page(cookie, INBOX);
    let at = page
        .find("action=\"/auth/settings/inbox/")
        .expect("a form of the page");
    let marker = "name=\"csrf\" value=\"";
    let at = at + page[at..].find(marker).unwrap() + marker.len();
    page[at..].split('"').next().unwrap().to_string()
}

fn save(pep: &Pep, cookie: &str, fields: &str) -> (u16, String, String) {
    let csrf = csrf_of_prefs(pep, cookie);
    pep.post(cookie, PREFS, &csrf, fields, None)
}

/// A phone that sends `set_prefs`. The frame after it proves that the server handled the first one.
async fn send_prefs(conn: &mut Conn, prefs: &Prefs) -> Option<(String, String)> {
    conn.send(&ClientMsg::SetPrefs(prefs.clone())).await;
    conn.send(&ClientMsg::Ping { nonce: 9 }).await;
    let mut refused = None;
    loop {
        match conn.recv(WAIT).await {
            Recv::Msg(ServerMsg::Pong { .. }) => return refused,
            Recv::Msg(ServerMsg::Error { code, message }) => refused = Some((code, message)),
            Recv::Msg(_) => {}
            Recv::Closed(code) => panic!("the session closed: {code:?}"),
            Recv::Timeout => panic!("no answer within {WAIT:?}"),
        }
    }
}

/// The preferences in the `welcome` of a new session of the phone.
async fn welcome_prefs(pep: &Pep, phone: &common::Phone) -> Prefs {
    let (_conn, welcome) = Conn::open(pep.s.port, phone).await.unwrap();
    match welcome {
        ServerMsg::Welcome { prefs, .. } => *prefs,
        other => panic!("{other:?}"),
    }
}

fn paris_quiet() -> QuietPrefs {
    QuietPrefs {
        from: "22:00".into(),
        to: "07:00".into(),
        tz: "Europe/Paris".into(),
        high_rings: true,
    }
}

#[test]
fn prefs_page_refused_for_impersonated_session() {
    let pep = Pep::new();
    let cookie = pep.cookie(OWNER);
    // The owner has the page. It has a form, no script, and the link is on the main page.
    let (st, head, page) = pep.page(&cookie, PREFS);
    assert_eq!(st, 200, "{page}");
    assert!(
        page.contains(&format!("method=\"post\" action=\"{PREFS}\"")),
        "{page}"
    );
    assert!(!page.contains("<script>") && !page.contains(" onclick="));
    // Only the decided options: no delegation, no escalation, no muted agent (SPEC 3.9.6).
    let words = page.to_lowercase();
    assert!(
        ["delegat", "escalat", "muted agent", "mute agent"]
            .iter()
            .all(|w| !words.contains(w)),
        "{page}"
    );
    assert!(common::approvals::header_of(&head, "cache-control")
        .is_some_and(|v| v.contains("no-store")));
    let (_, _, main) = pep.page(&cookie, INBOX);
    assert!(
        main.contains(&format!("href=\"{PREFS}\"")),
        "the link of the page"
    );
    // The session of "Log in as" has no page, no link, and no right to post.
    let as_owner = impersonate(&pep, OWNER);
    let (st, _, body) = pep.page(&as_owner, PREFS);
    assert_eq!(st, 403, "{body}");
    assert!(!body.contains("name=\"digest\""));
    let (_, _, main) = pep.page(&as_owner, INBOX);
    assert!(!main.contains(&format!("href=\"{PREFS}\"")));
    let csrf = csrf_of_any_form(&pep, &as_owner);
    let fields = "digest=every&hours=3&tz=UTC&from=01%3A00&to=02%3A00";
    assert_eq!(pep.post(&as_owner, PREFS, &csrf, fields, None).0, 403);
    // Without the token, the owner is refused too.
    assert_eq!(pep.post(&cookie, PREFS, "", fields, None).0, 403);
    // Nothing changed: the form still shows the default of the file (digest off).
    let (_, _, page) = pep.page(&cookie, PREFS);
    assert!(page.contains("value=\"off\" checked"), "{page}");
    assert!(!page.contains("value=\"3\""));
}

/// What the phone sets shows on the page, and what the page saves comes back in the next `welcome`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn set_prefs_from_phone_synced_to_page() {
    let pep = Pep::new();
    let phone = pep.phone("pixel").await;
    let cookie = pep.cookie(OWNER);
    let (mut conn, _) = Conn::open(pep.s.port, &phone).await.unwrap();
    let from_phone = Prefs {
        quiet: Some(paris_quiet()),
        digest: Some(DigestPrefs {
            mode: DigestMode::Every,
            at: None,
            tz: None,
            every_h: Some(6),
        }),
    };
    assert_eq!(send_prefs(&mut conn, &from_phone).await, None);
    let (_, _, page) = pep.page(&cookie, PREFS);
    assert!(page.contains("value=\"every\" checked"), "{page}");
    assert!(page.contains("name=\"hours\" value=\"6\""), "{page}");
    assert!(
        page.contains("name=\"from\" value=\"22:00\"")
            && page.contains("name=\"to\" value=\"07:00\"")
    );
    assert!(page.contains("name=\"high\" value=\"1\" checked"));
    assert!(
        page.contains("<option value=\"Europe/Paris\" selected>"),
        "the zone of the phone"
    );

    // The page saves other values. The phone reads them in its next welcome.
    let (st, head, body) = save(
        &pep,
        &cookie,
        "digest=daily&at=08%3A15&tz=Asia%2FTokyo&hours=4&from=23%3A00&to=06%3A30",
    );
    assert_eq!(st, 303, "{body}");
    assert!(head.contains("prefs?s=saved"), "{head}");
    let (_, _, page) = pep.page(&cookie, &format!("{PREFS}?s=saved"));
    assert!(page.contains("Your preferences are saved"), "{page}");
    assert!(
        page.contains("value=\"daily\" checked") && page.contains("name=\"at\" value=\"08:15\"")
    );
    drop(conn);
    let stored = welcome_prefs(&pep, &phone).await;
    assert_eq!(
        stored,
        Prefs {
            quiet: Some(QuietPrefs {
                from: "23:00".into(),
                to: "06:30".into(),
                tz: "Asia/Tokyo".into(),
                high_rings: false,
            }),
            digest: Some(DigestPrefs {
                mode: DigestMode::Daily,
                at: Some("08:15".into()),
                tz: Some("Asia/Tokyo".into()),
                every_h: None,
            }),
        }
    );

    // The button "defaults of the file" removes the values.
    let (st, head, body) = save(&pep, &cookie, "reset=1");
    assert_eq!(st, 303, "{body}");
    assert!(head.contains("prefs?s=reset"));
    assert_eq!(welcome_prefs(&pep, &phone).await, Prefs::default());
}

/// A name that is not in the time zone database gets 422, and nothing is stored or changed: no other case, no guess.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bad_timezone_name_refused_422_no_coercion() {
    let pep = Pep::new();
    let phone = pep.phone("pixel").await;
    let cookie = pep.cookie(OWNER);
    for tz in [
        "Mars%2FBase",
        "europe%2Fparis",
        "Paris",
        "Europe%2FParis%00",
    ] {
        let (st, _, page) = save(&pep, &cookie, &format!("digest=daily&at=09%3A00&tz={tz}"));
        assert_eq!(st, 422, "{tz}: {page}");
        assert!(
            page.contains("Nothing is saved") && page.contains("time zone"),
            "{tz}: {page}"
        );
    }
    // A zone that no block uses is checked too, and a bad hour or period is refused.
    for fields in [
        "digest=off&tz=Mars%2FBase",
        "digest=every&hours=0&tz=UTC",
        "digest=every&hours=169&tz=UTC",
        "digest=every&hours=abc&tz=UTC",
        "digest=off&tz=UTC&from=22%3A00",
        "digest=off&tz=UTC&from=22%3A00&to=22%3A00",
        "digest=off&tz=UTC&from=24%3A00&to=07%3A00",
        "digest=weekly&tz=UTC",
    ] {
        let (st, _, page) = save(&pep, &cookie, fields);
        assert_eq!(st, 422, "{fields}: {page}");
    }
    // The refused page keeps what the person typed.
    let (_, _, page) = save(&pep, &cookie, "digest=every&hours=169&tz=UTC");
    assert!(page.contains("name=\"hours\" value=\"169\""), "{page}");
    assert_eq!(welcome_prefs(&pep, &phone).await, Prefs::default());

    // The phone gets the same answer, and nothing is stored.
    let (mut conn, _) = Conn::open(pep.s.port, &phone).await.unwrap();
    let bad = Prefs {
        quiet: Some(QuietPrefs {
            tz: "Mars/Base".into(),
            ..paris_quiet()
        }),
        digest: None,
    };
    let (code, message) = send_prefs(&mut conn, &bad).await.expect("an error frame");
    assert_eq!(code, "invalid_prefs");
    assert!(
        message.contains("quiet.tz") && message.contains("Mars/Base"),
        "{message}"
    );
    let low_case = Prefs {
        quiet: Some(QuietPrefs {
            tz: "europe/paris".into(),
            ..paris_quiet()
        }),
        digest: None,
    };
    assert!(send_prefs(&mut conn, &low_case).await.is_some());
    drop(conn);
    assert_eq!(welcome_prefs(&pep, &phone).await, Prefs::default());
    // A good value is stored: the refusals did not break the path.
    let (st, _, body) = save(&pep, &cookie, "digest=every&hours=168&tz=UTC");
    assert_eq!(st, 303, "{body}");
    assert_eq!(
        welcome_prefs(&pep, &phone)
            .await
            .digest
            .and_then(|d| d.every_h),
        Some(168)
    );
}

/// The time of the day of `unix_s`, in UTC, as `HH:MM`.
fn hhmm(unix_s: u64) -> String {
    let day = unix_s % 86_400;
    format!("{:02}:{:02}", day / 3600, day % 3600 / 60)
}

/// A request that arrives inside the quiet hours is delivered at once with the flag `quiet`. One that arrives outside has
/// no flag. The window comes from the real clock: one hour before to one hour after now, in UTC.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn quiet_flag_set_in_window_request_still_delivered() {
    let pep = Pep::new();
    let phone = pep.phone("pixel").await;
    let (mut conn, _) = Conn::open(pep.s.port, &phone).await.unwrap();
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let window = |from: u64, to: u64| Prefs {
        quiet: Some(QuietPrefs {
            from: hhmm(from),
            to: hhmm(to),
            tz: "UTC".into(),
            high_rings: false,
        }),
        digest: None,
    };
    assert_eq!(
        send_prefs(&mut conn, &window(now - 3600, now + 3600)).await,
        None
    );
    let (first, _) = pep.submit(&command(1), 600);
    let req = next_request(&mut conn, WAIT).await;
    conn.receipt(&req.id).await;
    assert_eq!(req.request_id, first, "the request is delivered");
    assert!(req.quiet, "inside the quiet hours");

    assert_eq!(
        send_prefs(&mut conn, &window(now + 7200, now + 10_800)).await,
        None
    );
    let (second, _) = pep.submit(&command(2), 600);
    let req = next_request(&mut conn, WAIT).await;
    conn.receipt(&req.id).await;
    assert_eq!(req.request_id, second);
    assert!(!req.quiet, "outside the quiet hours");
}

/// Starts a server and waits until the file `stop` appears in the directory that `INBOX_DEMO_OUT` names. The test is for a
/// look at the page with a browser (`tools/dev/ui-check-prefs.js`). It writes `demo.json` with the port, the login and
/// the password. Run it with `cargo test --test prefs_e2e prefs_demo_for_the_eyes -- --ignored`.
#[test]
#[ignore = "starts a server for a look at the page"]
fn prefs_demo_for_the_eyes() {
    let out = std::path::PathBuf::from(std::env::var("INBOX_DEMO_OUT").expect("INBOX_DEMO_OUT"));
    let pep = Pep::with(|s| {
        let text = std::fs::read_to_string(s.config()).unwrap();
        let from = "digest { mode = \"off\" }";
        assert!(text.contains(from));
        std::fs::write(
            s.config(),
            text.replacen(
                from,
                "digest { mode = \"daily\" at = \"09:00\" timezone = \"Europe/Paris\" }",
                1,
            ),
        )
        .unwrap();
    });
    std::fs::write(
        out.join("demo.json"),
        serde_json::json!({"port": pep.s.port, "owner": OWNER, "password": common::ACCOUNT_PASSWORD})
            .to_string(),
    )
    .unwrap();
    while !out.join("stop").exists() {
        std::thread::sleep(Duration::from_millis(300));
    }
}
