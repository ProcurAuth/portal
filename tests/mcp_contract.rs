//! Black-box tests of the MCP endpoint of the portal (`/mcp`), against the real binary. They follow the cases of
//! `mcp-server-contract/conformance/CASES.md` that apply to the agent channel: the transport (T), the keys (K) and the
//! tools, the results and the errors (R). The cases of the approval of actions (P) need a service and a PDP client: the
//! tests of the portal on the approval API cover them. Where the binding of ProcurAuth and the contract differ, the
//! binding applies: each refusal before the message has an empty body.
//!
//! The helpers that make a key and turn on the kind `mcp` are here and not in `tests/common`, because the other lots
//! of the inbox edit that file.

mod common;

use common::{form_value, http_full, portal_ok, Server};
use serde_json::{json, Value};

const MODERN: &str = "2026-07-28";
const META: &str = "io.modelcontextprotocol/protocolVersion";

/// Turns on the kind `mcp`: it needs the blocks `identity` and `pdp`.
fn enable_mcp(s: &Server) {
    std::fs::write(
        s.dir.join("identity.pem"),
        "-----BEGIN PRIVATE KEY-----\nMC4CAQAwBQYDK2VwBCIEIAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8g\n-----END PRIVATE KEY-----\n",
    )
    .unwrap();
    let text = std::fs::read_to_string(s.config()).unwrap();
    let from = "keys {\n  mcp = false\n  api = true\n}";
    assert!(text.contains(from));
    let mut text = text.replace(from, "keys {\n  mcp = true\n  api = true\n}");
    let pdp = common::free_port();
    text.push_str(&format!(
        "\nidentity {{\n  issuer = \"https://portal.e2e\"\n  signing_key_file = \"{}\"\n}}\npdp {{\n  listen_address = \"127.0.0.1:{pdp}\"\n  url = \"http://127.0.0.1:{pdp}\"\n  plain_http = true\n}}\n",
        s.dir.join("identity.pem").display()
    ));
    std::fs::write(s.config(), text).unwrap();
}

/// A server with the items, the approvals and the block `inbox.mcp` with `mcp` inside it. The kind `mcp` is on.
fn start_server(mcp: &str) -> Server {
    let mut s = Server::setup();
    let key = s.dir.join("content_key");
    portal_ok(&["inbox", "content-key", "--out", &key.to_string_lossy()]);
    s.add_to_inbox(&format!(
        r#"
  items {{
    max_lifetime = "90d"
    max_open_per_key = 5
    max_open_per_owner = 20
    max_open_total = 1000
    retention = "180d"
    body_purge_after = "30d"
    audit_retention = "365d"
    lease {{ min = "30s" max = "7d" grace = "2m" revive_window = "7d" }}
    digest {{ mode = "off" }}
  }}
  at_rest {{
    active_key = "k1"
    key "k1" {{ secret_file = "{}" }}
  }}
  approvals {{ consume_within = "5m" }}
  mcp {{ {mcp} }}
"#,
        key.display()
    ));
    enable_mcp(&s);
    s.start();
    s
}

/// A key that the superadmin makes on the page Keys. The answer is the text of the key, shown one time.
fn make_key(s: &Server, kind: &str, name: &str, interaction: &str) -> String {
    let cookie = s.session().cookie.clone();
    let host = format!("127.0.0.1:{}", s.port);
    let (st, _, page) = http_full(
        s.port,
        &host,
        "GET",
        &format!("/auth/keys/new?kind={kind}"),
        &[("Cookie", cookie.clone())],
        "",
    );
    assert_eq!(st, 200, "{page}");
    let form = page.find("id=\"key-form\"").expect("the form of a new key");
    let marker = "name=\"csrf\" value=\"";
    let start = form + page[form..].find(marker).unwrap() + marker.len();
    let csrf = page[start..].split('"').next().unwrap().to_string();
    let body = format!(
        "csrf={csrf}&kind={kind}&name={}&expires_at=&interaction={interaction}",
        form_value(name)
    );
    let (st, _, page) = http_full(
        s.port,
        &host,
        "POST",
        "/auth/keys/create",
        &[
            ("Content-Type", "application/x-www-form-urlencoded".into()),
            ("Origin", format!("http://{host}")),
            ("Cookie", cookie),
        ],
        &body,
    );
    assert_eq!(st, 200, "the key is not made: {page}");
    let marker = "id=\"key-text\" type=\"text\" readonly value=\"";
    let start = page.find(marker).expect("the text of the key") + marker.len();
    page[start..].split('"').next().unwrap().to_string()
}

/// What the endpoint answered.
struct Answer {
    status: u16,
    head: String,
    body: String,
}

impl Answer {
    fn header(&self, name: &str) -> Option<String> {
        self.head.lines().skip(1).find_map(|line| {
            let (key, value) = line.split_once(':')?;
            key.eq_ignore_ascii_case(name)
                .then(|| value.trim().to_string())
        })
    }

    fn json(&self) -> Value {
        serde_json::from_str(&self.body).unwrap_or_else(|e| panic!("{e}: {:?}", self.body))
    }

    fn error_code(&self) -> i64 {
        self.json()["error"]["code"].as_i64().unwrap()
    }
}

/// One request to the endpoint. Each answer must have `Cache-Control: no-store` (T13).
fn request(s: &Server, method: &str, headers: &[(&str, String)], body: &str) -> Answer {
    let host = format!("127.0.0.1:{}", s.port);
    let (status, head, body) = http_full(s.port, &host, method, "/mcp", headers, body);
    let answer = Answer { status, head, body };
    assert_eq!(
        answer.header("cache-control").as_deref(),
        Some("no-store"),
        "T13: {method} gave {status} with no `no-store`: {}",
        answer.head
    );
    answer
}

fn bearer(token: &str) -> (&'static str, String) {
    ("Authorization", format!("Bearer {token}"))
}

/// A legacy request: no `_meta`, no version header.
fn legacy(s: &Server, token: &str, method: &str, params: Value) -> Answer {
    let body = json!({"jsonrpc": "2.0", "id": 1, "method": method, "params": params});
    request(s, "POST", &[bearer(token)], &body.to_string())
}

/// A request of the version `2026-07-28` with the headers that agree with the message.
fn modern(s: &Server, token: &str, method: &str, name: Option<&str>, mut params: Value) -> Answer {
    params["_meta"] = json!({META: MODERN});
    let body = json!({"jsonrpc": "2.0", "id": 1, "method": method, "params": params});
    let mut headers = vec![
        bearer(token),
        ("MCP-Protocol-Version", MODERN.to_string()),
        ("Mcp-Method", method.to_string()),
    ];
    if let Some(name) = name {
        headers.push(("Mcp-Name", name.to_string()));
    }
    request(s, "POST", &headers, &body.to_string())
}

fn call_tool(s: &Server, token: &str, name: &str, args: Value) -> Answer {
    legacy(
        s,
        token,
        "tools/call",
        json!({"name": name, "arguments": args}),
    )
}

/// The result of a tool: the flag, and the structured content. The text is the same object (R04).
fn tool_result(a: &Answer) -> (bool, Value) {
    assert_eq!(a.status, 200, "{}", a.body);
    let body = a.json();
    let result = &body["result"];
    let text = result["content"][0]["text"].as_str().expect("a text");
    assert_eq!(
        serde_json::from_str::<Value>(text).unwrap(),
        result["structuredContent"],
        "R04"
    );
    (
        result["isError"] == true,
        result["structuredContent"].clone(),
    )
}

fn tool_names(s: &Server, token: &str) -> Vec<String> {
    let a = legacy(s, token, "tools/list", json!({}));
    assert_eq!(a.status, 200, "{}", a.body);
    a.json()["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap().to_string())
        .collect()
}

// ---------------------------------------------------------------- transport

#[test]
fn the_transport_cases_t01_to_t13() {
    let s = start_server(
        r#"allowed_origins = ["https://app.example"] max_unauthenticated_per_minute = 100"#,
    );
    let key = make_key(&s, "mcp", "agent", "conversational");
    assert!(key.starts_with("mcp_"), "{key}");

    // T01, T02, T05: `server/discover` for the new version, with the name and the version in `_meta`.
    let a = modern(&s, &key, "server/discover", None, json!({}));
    assert_eq!(a.status, 200, "T01 {}", a.body);
    let result = a.json()["result"].clone();
    assert_eq!(result["resultType"], "complete", "T05");
    let info = &result["_meta"]["io.modelcontextprotocol/serverInfo"];
    assert_eq!(info["name"], "portal", "T02");
    assert!(
        info["version"].as_str().is_some_and(|v| !v.is_empty()),
        "T02"
    );
    assert!(result["supportedVersions"]
        .as_array()
        .unwrap()
        .contains(&json!(MODERN)));
    assert_eq!(
        a.header("content-type").as_deref(),
        Some("application/json")
    );

    // T02, T06: `initialize` of a legacy client. The server echoes a known version, gives `serverInfo`, makes no session,
    // and ignores the session id that the client sends.
    for (asked, expected) in [
        ("2025-06-18", "2025-06-18"),
        ("2025-11-25", "2025-11-25"),
        ("1999-01-01", "2025-11-25"),
    ] {
        let body = json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {
            "protocolVersion": asked, "capabilities": {}, "clientInfo": {"name": "t", "version": "1"}}});
        let a = request(
            &s,
            "POST",
            &[bearer(&key), ("Mcp-Session-Id", "client-made-id".into())],
            &body.to_string(),
        );
        assert_eq!(a.status, 200, "{}", a.body);
        let result = a.json()["result"].clone();
        assert_eq!(result["protocolVersion"], expected);
        assert_eq!(result["serverInfo"]["name"], "portal", "T02");
        assert!(
            result.get("resultType").is_none(),
            "a legacy result has no resultType"
        );
        assert!(a.header("mcp-session-id").is_none(), "T06");
    }

    // T03: a request of the new version without `Mcp-Method` gets 400 and -32020. Each other mismatch too.
    let meta = json!({"_meta": {META: MODERN}});
    let body = json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list", "params": meta});
    let a = request(
        &s,
        "POST",
        &[bearer(&key), ("MCP-Protocol-Version", MODERN.into())],
        &body.to_string(),
    );
    assert_eq!((a.status, a.error_code()), (400, -32020), "T03 {}", a.body);
    let mismatches: [Vec<(&str, String)>; 4] = [
        vec![
            ("MCP-Protocol-Version", MODERN.into()),
            ("Mcp-Method", "tools/call".into()),
        ],
        vec![
            ("MCP-Protocol-Version", "2025-11-25".into()),
            ("Mcp-Method", "tools/list".into()),
        ],
        vec![("Mcp-Method", "tools/list".into())],
        vec![],
    ];
    for extra in mismatches {
        let mut headers = vec![bearer(&key)];
        headers.extend(extra);
        let a = request(&s, "POST", &headers, &body.to_string());
        assert_eq!((a.status, a.error_code()), (400, -32020), "T03 {}", a.body);
    }
    // The name of the tool must agree too.
    let call = json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {
        "name": "get_answer", "arguments": {}, "_meta": {META: MODERN}}});
    for name in [Some("cancel"), None] {
        let mut headers = vec![
            bearer(&key),
            ("MCP-Protocol-Version", MODERN.to_string()),
            ("Mcp-Method", "tools/call".to_string()),
        ];
        if let Some(name) = name {
            headers.push(("Mcp-Name", name.to_string()));
        }
        let a = request(&s, "POST", &headers, &call.to_string());
        assert_eq!((a.status, a.error_code()), (400, -32020), "T03 {name:?}");
    }

    // T04: a version that the server does not support gets 400, -32022 and `data.supported`.
    let body = json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list", "params": {"_meta": {META: "2099-01-01"}}});
    let a = request(
        &s,
        "POST",
        &[
            bearer(&key),
            ("MCP-Protocol-Version", "2099-01-01".into()),
            ("Mcp-Method", "tools/list".into()),
        ],
        &body.to_string(),
    );
    assert_eq!((a.status, a.error_code()), (400, -32022), "T04 {}", a.body);
    let supported = a.json()["error"]["data"]["supported"].clone();
    assert!(
        supported.as_array().unwrap().contains(&json!(MODERN)),
        "T04 {supported}"
    );
    // A legacy client with a header that names an unknown version gets the same.
    let a = request(
        &s,
        "POST",
        &[bearer(&key), ("MCP-Protocol-Version", "1999-01-01".into())],
        &json!({"jsonrpc": "2.0", "id": 1, "method": "ping"}).to_string(),
    );
    assert_eq!((a.status, a.error_code()), (400, -32022), "T04");

    // T05: each result of the new version has `resultType`.
    for (method, name, params) in [
        ("ping", None, json!({})),
        ("tools/list", None, json!({})),
        (
            "tools/call",
            Some("get_answer"),
            json!({"name": "get_answer", "arguments": {"request_id": "x"}}),
        ),
    ] {
        let a = modern(&s, &key, method, name, params);
        assert_eq!(a.status, 200, "T05 {method}: {}", a.body);
        assert_eq!(a.json()["result"]["resultType"], "complete", "T05 {method}");
    }

    // T07: an unknown method is a usual answer with -32601. A method of the other version too.
    for method in [
        "resources/list",
        "prompts/list",
        "nope/nope",
        "completion/complete",
    ] {
        let a = legacy(&s, &key, method, json!({}));
        assert_eq!((a.status, a.error_code()), (200, -32601), "T07 {method}");
    }
    let a = legacy(&s, &key, "server/discover", json!({}));
    assert_eq!(
        (a.status, a.error_code()),
        (200, -32601),
        "a legacy request cannot discover"
    );

    // T08: a batch, a body that is not JSON and a body that is not JSON-RPC 2.0 get 400.
    let a = request(
        &s,
        "POST",
        &[bearer(&key)],
        r#"[{"jsonrpc":"2.0","id":1,"method":"ping"}]"#,
    );
    assert_eq!((a.status, a.error_code()), (400, -32600), "T08 batch");
    let a = request(&s, "POST", &[bearer(&key)], "this is not json");
    assert_eq!((a.status, a.error_code()), (400, -32700), "T08 json");
    for body in [
        r#"{"jsonrpc":"1.0","id":1,"method":"ping"}"#,
        r#"{"id":1,"method":"ping"}"#,
        r#"{"jsonrpc":"2.0","id":null,"method":"ping"}"#,
        r#"{"jsonrpc":"2.0","id":1}"#,
        "5",
    ] {
        let a = request(&s, "POST", &[bearer(&key)], body);
        assert_eq!((a.status, a.error_code()), (400, -32600), "T08 {body}");
    }

    // A notification and a response of the client get 202 with no body.
    for body in [
        r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
        r#"{"jsonrpc":"2.0","id":9,"result":{}}"#,
    ] {
        let a = request(&s, "POST", &[bearer(&key)], body);
        assert_eq!(a.status, 202, "{body}");
        assert!(a.body.is_empty(), "{body}");
    }
    // A notification of the new version with a header that disagrees is refused as a request is.
    let body = r#"{"jsonrpc":"2.0","method":"notifications/initialized","params":{"_meta":{"io.modelcontextprotocol/protocolVersion":"2026-07-28"}}}"#;
    let a = request(&s, "POST", &[bearer(&key)], body);
    assert_eq!((a.status, a.error_code()), (400, -32020));
    let a = request(
        &s,
        "POST",
        &[
            bearer(&key),
            ("MCP-Protocol-Version", MODERN.into()),
            ("Mcp-Method", "notifications/initialized".into()),
        ],
        body,
    );
    assert_eq!(a.status, 202);

    // T09: any method but POST gets 405 with `Allow: POST`, with a key and without one.
    for method in ["GET", "DELETE", "PUT", "PATCH"] {
        for headers in [vec![bearer(&key)], vec![]] {
            let a = request(&s, method, &headers, "");
            assert_eq!(a.status, 405, "T09 {method}");
            assert_eq!(a.header("allow").as_deref(), Some("POST"), "T09 {method}");
            assert!(a.body.is_empty());
        }
    }

    // T10: a wrong `Content-Type` gets 415, and a body above `max_body_bytes` gets 413.
    for content_type in [
        "text/plain",
        "application/x-www-form-urlencoded",
        "application/jsonx",
    ] {
        let a = request(
            &s,
            "POST",
            &[bearer(&key), ("Content-Type", content_type.into())],
            r#"{"jsonrpc":"2.0","id":1,"method":"ping"}"#,
        );
        assert_eq!(a.status, 415, "T10 {content_type}");
        assert!(a.body.is_empty());
    }
    let a = request(
        &s,
        "POST",
        &[
            bearer(&key),
            ("Content-Type", "application/json; charset=utf-8".into()),
        ],
        r#"{"jsonrpc":"2.0","id":1,"method":"ping"}"#,
    );
    assert_eq!(a.status, 200, "a charset is allowed");
    let big = format!(
        r#"{{"jsonrpc":"2.0","id":1,"method":"ping","params":{{"x":"{}"}}}}"#,
        "a".repeat(70 * 1024)
    );
    let a = request(&s, "POST", &[bearer(&key)], &big);
    assert_eq!(a.status, 413, "T10");
    assert!(a.body.is_empty());

    // T11, T12: no `Origin` passes. An origin that is not on the list gets 403, before the key is checked, and an origin
    // on the list passes.
    let ping = json!({"jsonrpc": "2.0", "id": 1, "method": "ping"}).to_string();
    assert_eq!(
        request(&s, "POST", &[bearer(&key)], &ping).status,
        200,
        "T11"
    );
    let a = request(
        &s,
        "POST",
        &[bearer(&key), ("Origin", "https://evil.example".into())],
        &ping,
    );
    assert_eq!(a.status, 403, "T11");
    let a = request(
        &s,
        "POST",
        &[("Origin", "https://evil.example".into())],
        &ping,
    );
    assert_eq!(a.status, 403, "T12: 403, not 401");
    let a = request(
        &s,
        "POST",
        &[bearer(&key), ("Origin", "https://app.example".into())],
        &ping,
    );
    assert_eq!(a.status, 200, "an origin of the list passes");
    let a = request(
        &s,
        "POST",
        &[bearer(&key), ("Origin", "https://app.example:8443".into())],
        &ping,
    );
    assert_eq!(a.status, 403, "an origin is compared as a whole");
}

// ---------------------------------------------------------------- keys, tools, results, errors

#[test]
fn the_key_and_tool_cases_k01_k02_k03_r01_to_r09() {
    let s = start_server(r#"max_unauthenticated_per_minute = 100"#);
    let conversational = make_key(&s, "mcp", "agent", "conversational");
    let shot = make_key(&s, "mcp", "system", "one_shot");
    let api = make_key(&s, "api", "script", "one_shot");
    let ping = json!({"jsonrpc": "2.0", "id": 1, "method": "ping"}).to_string();

    // K01: no key, an unknown key, a good key of the other kind and a text that is not a key get the same 401, with an
    // empty body and a challenge.
    let unknown = format!("mcp_{}", "A".repeat(43 + 6));
    let seen: Vec<(u16, Option<String>, String)> = [
        vec![],
        vec![bearer(&unknown)],
        vec![bearer(&api)],
        vec![bearer("not-a-key")],
        vec![("Authorization", "Basic abc".to_string())],
        vec![("Authorization", "Bearer".to_string())],
    ]
    .iter()
    .map(|headers| {
        let a = request(&s, "POST", headers, &ping);
        (a.status, a.header("www-authenticate"), a.body)
    })
    .collect();
    for one in &seen {
        assert_eq!(one, &seen[0], "K01");
    }
    assert_eq!(seen[0].0, 401);
    assert!(
        seen[0]
            .1
            .as_deref()
            .is_some_and(|h| h.starts_with("Bearer realm=")),
        "K01 {:?}",
        seen[0].1
    );
    assert!(seen[0].2.is_empty(), "K01: the body is empty");

    // R01, R02, K02: the tools of a key, in one order, with the hints. A key that is not conversational does not see
    // `say` and `supersede`.
    let all = [
        "approval_intent",
        "get_answer",
        "keepalive",
        "say",
        "supersede",
        "cancel",
    ];
    assert_eq!(tool_names(&s, &conversational), all, "K02");
    assert_eq!(
        tool_names(&s, &conversational),
        all,
        "R01: the same sequence each time"
    );
    assert_eq!(
        tool_names(&s, &shot),
        ["approval_intent", "get_answer", "keepalive", "cancel"],
        "K02"
    );
    let list = legacy(&s, &conversational, "tools/list", json!({})).json();
    for tool in list["result"]["tools"].as_array().unwrap() {
        let name = tool["name"].as_str().unwrap();
        assert!(
            name.len() <= 64 && name.bytes().all(|b| b.is_ascii_lowercase() || b == b'_'),
            "R01 {name}"
        );
        let a = &tool["annotations"];
        for hint in ["readOnlyHint", "idempotentHint", "openWorldHint"] {
            assert!(a[hint].is_boolean(), "R02 {name} {hint}");
        }
        if a["readOnlyHint"] == false {
            assert_eq!(a["destructiveHint"], false, "R02 {name}");
        }
        assert_eq!(tool["inputSchema"]["type"], "object", "R-schema {name}");
        assert!(tool["outputSchema"].is_object(), "R-schema {name}");
        let description = tool["description"].as_str().unwrap();
        assert!(
            !description.contains("canary"),
            "no text of a third party in a description"
        );
    }
    // K03: a tool that the key does not have is answered as a tool that does not exist.
    let unknown_tool = call_tool(&s, &conversational, "no_such_tool", json!({}));
    let hidden = call_tool(&s, &shot, "say", json!({"request_id": "x", "text": "x"}));
    assert_eq!(
        (unknown_tool.status, unknown_tool.error_code()),
        (200, -32602)
    );
    assert_eq!(unknown_tool.body, hidden.body, "K03: the same answer");

    // R04: a result has `structuredContent` and the same object as text. R08, R09: a tool error has the three members.
    let ok = call_tool(
        &s,
        &conversational,
        "approval_intent",
        json!({"lease_s": 900, "explanation": "why"}),
    );
    let (is_error, value) = tool_result(&ok);
    assert!(!is_error, "{value}");
    assert_eq!(value["approval_ref"].as_str().unwrap().len(), 43);
    assert!(value["valid_for_s"].as_i64().unwrap() > 0);
    for (args, code) in [
        (json!({}), "lease_required"),
        (json!({"lease_s": 5}), "invalid_lease"),
        (
            json!({"lease_s": 900, "until": "2099-01-01T00:00:00Z"}),
            "invalid_lease",
        ),
        (json!({"lease_s": "900"}), "invalid_argument"),
        (json!({"lease_s": 900, "no_such": 1}), "invalid_argument"),
        (
            json!({"lease_s": 900, "explanation": "x".repeat(2001)}),
            "explanation_too_long",
        ),
    ] {
        let (is_error, value) = tool_result(&call_tool(
            &s,
            &conversational,
            "approval_intent",
            args.clone(),
        ));
        assert!(is_error, "R08 {args}");
        let error = &value["error"];
        assert_eq!(error["code"], code, "R08 R09 {args}: {value}");
        assert!(error["message"].as_str().is_some_and(|m| !m.is_empty()));
        assert_eq!(error["retryable"], false, "{args}");
        assert!(
            !error["message"].as_str().unwrap().contains("xxxx"),
            "the message copies no value"
        );
    }
    // A one_shot key cannot ask for a talk.
    let (is_error, value) = tool_result(&call_tool(
        &s,
        &shot,
        "approval_intent",
        json!({"lease_s": 900, "allow_ask_back": true}),
    ));
    assert!(is_error);
    assert_eq!(value["error"]["code"], "interaction_not_allowed");
    // R09: an argument that is absent or of the wrong type.
    for (tool, args) in [
        ("get_answer", json!({})),
        ("get_answer", json!({"request_id": 5})),
        ("get_answer", json!({"request_id": "x", "wait_s": -1})),
        ("keepalive", json!({"request_id": "x", "extra": true})),
        ("say", json!({"request_id": "x"})),
        ("cancel", json!({})),
    ] {
        let (is_error, value) = tool_result(&call_tool(&s, &conversational, tool, args.clone()));
        assert!(
            is_error && value["error"]["code"] == "invalid_argument",
            "R09 {tool} {args}: {value}"
        );
    }
    // An item that does not exist: `not_found`, not retryable.
    for tool in ["get_answer", "keepalive", "cancel", "supersede"] {
        let (is_error, value) = tool_result(&call_tool(
            &s,
            &conversational,
            tool,
            json!({"request_id": "0123456789abcdef0123456789abcdef"}),
        ));
        assert!(is_error, "{tool}");
        assert_eq!(
            value["error"],
            json!({"code": "not_found", "message": "no such item", "retryable": false}),
            "{tool}"
        );
    }
}

#[test]
fn the_rate_cases_k07_and_k08() {
    let s = start_server(r#"rate_limit_per_minute = 4 max_unauthenticated_per_minute = 3"#);
    let key = make_key(&s, "mcp", "agent", "conversational");
    let other = make_key(&s, "mcp", "other", "conversational");
    let ping = json!({"jsonrpc": "2.0", "id": 1, "method": "ping"}).to_string();
    // K07: each request counts, `tools/list` and `ping` too. The key over its limit gets 429 and `Retry-After`.
    for n in 0..4 {
        assert_eq!(
            request(&s, "POST", &[bearer(&key)], &ping).status,
            200,
            "{n}"
        );
    }
    let a = request(&s, "POST", &[bearer(&key)], &ping);
    assert_eq!(a.status, 429, "K07");
    assert!(
        a.header("retry-after")
            .and_then(|v| v.parse::<u64>().ok())
            .is_some_and(|v| v >= 1),
        "K07"
    );
    assert!(a.body.is_empty());
    // Another key has its own bucket.
    assert_eq!(request(&s, "POST", &[bearer(&other)], &ping).status, 200);
    // K08: a request with no valid key is counted for the client address, before the key is checked.
    let bad = format!("mcp_{}", "B".repeat(43 + 6));
    for n in 0..3 {
        assert_eq!(
            request(&s, "POST", &[bearer(&bad)], &ping).status,
            401,
            "K08 {n}"
        );
    }
    let a = request(&s, "POST", &[bearer(&bad)], &ping);
    assert_eq!(a.status, 429, "K08");
    assert!(a
        .header("retry-after")
        .and_then(|v| v.parse::<u64>().ok())
        .is_some_and(|v| v >= 1));
    // A good key from the same address waits too: the address is over its limit.
    assert_eq!(
        request(&s, "POST", &[bearer(&other)], &ping).status,
        429,
        "K08"
    );
    // A request that fails a check before the key (a bad method) is not counted.
    assert_eq!(request(&s, "GET", &[], "").status, 405);
}
