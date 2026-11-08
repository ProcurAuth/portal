//! The conformance cases of the series D (a PDP) and E (an edge) of the protocol, driven over HTTP.
//! The cases come from the public repository of the protocol. The tests read the path of its
//! directory `conformance/` in `PROCURAUTH_CONFORMANCE`. Without this variable, they do nothing.
//! The servers and the helpers come from `cluster_tests`.

use std::net::TcpStream;
use std::time::Duration;

use super::cluster_tests::*;
use super::pdp_rig::{app_of, seed_directory, InboxRig};
use super::*;
use crate::store::testing::{each_backend, Backend};

// The conformance cases of the series D (a PDP) --------------------------------------------------

/// The cases of the public repository of the protocol (`conformance/`). Each case loads its
/// directory in the portal, sends its steps to the PDP and checks the answers. The test reads the
/// path of the directory `conformance/` in `PROCURAUTH_CONFORMANCE`. Without this variable, the test
/// does nothing.
mod conformance_d {
    use super::*;
    use serde_json::Value;
    use sha2::{Digest, Sha256};

    fn conformance_dir() -> Option<std::path::PathBuf> {
        std::env::var_os("PROCURAUTH_CONFORMANCE").map(std::path::PathBuf::from)
    }

    fn read_json(path: &std::path::Path) -> Value {
        let text = std::fs::read_to_string(path).unwrap_or_else(|e| panic!("{path:?}: {e}"));
        serde_json::from_str(&text).unwrap_or_else(|e| panic!("{path:?}: {e}"))
    }

    /// The service ids of a directory, with their documents and the credential of each.
    struct Service {
        id: String,
        document: Option<Value>,
        credential: String,
    }

    /// The text of a credential: the one that a step of the case gives for this digest, or one that
    /// the harness makes.
    fn credential_of(id: &str, digest: Option<&str>, case: &Value) -> String {
        let made = format!("{id}-credential-harness-0123456789abcdef");
        let Some(digest) = digest else {
            return made;
        };
        let mut found = None;
        let mut visit = |value: &Value| {
            if let Some(text) = value
                .get("request")
                .and_then(|r| r.get("headers"))
                .and_then(|h| h.as_object())
                .and_then(|h| {
                    h.iter()
                        .find(|(name, _)| name.eq_ignore_ascii_case("authorization"))
                })
                .and_then(|(_, v)| v.as_str())
                .and_then(|v| v.strip_prefix("Bearer "))
            {
                let hex: String = Sha256::digest(text.as_bytes())
                    .iter()
                    .map(|b| format!("{b:02x}"))
                    .collect();
                if hex == digest {
                    found = Some(text.to_string());
                }
            }
        };
        if let Some(steps) = case.get("steps").and_then(Value::as_array) {
            steps.iter().for_each(&mut visit);
        }
        found.unwrap_or(made)
    }

    fn limits_text(limits: Option<&Value>) -> String {
        limits.map_or_else(|| "{}".to_string(), Value::to_string)
    }

    fn grants_of(subject: &Value) -> Vec<Grant> {
        let mut grants: Vec<Grant> = subject["grants"]
            .as_array()
            .map(Vec::as_slice)
            .unwrap_or(&[])
            .iter()
            .map(|g| {
                let text = format!(
                    "{} on {}",
                    g["action"].as_str().unwrap(),
                    g["resource"].as_str().unwrap()
                );
                Grant::parse(&text)
                    .unwrap_or_else(|e| panic!("the grant {text:?} of the directory: {e}"))
                    .with_confirm(g.get("confirm").and_then(Value::as_bool).unwrap_or(false))
                    .with_limits(
                        crate::limits::Limits::from_json(&limits_text(g.get("limits")))
                            .unwrap_or_else(|e| panic!("the limits of {text:?}: {e}")),
                    )
            })
            .collect();
        grants.sort();
        grants
    }

    /// The default service of a request: the service of its resource.
    fn service_of(body: &Value) -> Option<String> {
        let resource = body
            .get("resource")
            .or_else(|| body.get("evaluations")?.get(0)?.get("resource"))?;
        if resource["type"] == "service" {
            return resource["id"].as_str().map(str::to_string);
        }
        resource["properties"]["ancestors"][0]["id"]
            .as_str()
            .map(str::to_string)
    }

    /// Loads the directory in the node. `now` is the fixed clock.
    fn load(node: &Node, directory: &Value, counters: &[Value], now: i64) {
        let store = &node.state.store;
        let subjects = directory["subjects"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        // The persons first: each key needs its owner.
        for subject in subjects
            .iter()
            .filter(|s| !s["id"].as_str().unwrap().contains(':'))
        {
            let mut grants = grants_of(subject);
            // The portal lets a person use keys only with this right (portal 5.3b).
            grants.push(Grant::parse("keys.own.use on service:portal/key_kind:mcp").unwrap());
            grants.sort();
            store
                .insert_user(&crate::store::UserRow {
                    login: subject["id"].as_str().unwrap().to_string(),
                    password_hash: "h".into(),
                    grants,
                    created_at: 1,
                })
                .unwrap_or_else(|e| panic!("the person {}: {e:?}", subject["id"]));
        }
        for subject in subjects
            .iter()
            .filter(|s| s["id"].as_str().unwrap().contains(':'))
        {
            let id = subject["id"].as_str().unwrap();
            let local = id.strip_prefix("key:").unwrap();
            let owner = subject["owner"].as_str().unwrap();
            let state = subject["state"].as_str().unwrap_or("active");
            let owner_exists = subjects.iter().any(|s| s["id"] == owner);
            // A revoked key is deleted, and a key whose owner does not exist cannot be stored: the
            // PDP has no such key, so it is an unknown subject (protocol 11.1).
            if state == "revoked" || !owner_exists {
                continue;
            }
            store
                .insert_key(
                    &crate::store::KeyRow {
                        kind: crate::keys::KeyKind::Mcp,
                        interaction: crate::keys::Interaction::Conversational,
                        id: local.to_string(),
                        owner: Some(owner.to_string()),
                        name: local.to_string(),
                        digest: Sha256::digest(id.as_bytes()).into(),
                        hint: "ab12".into(),
                        created_at: unix_now() - 100,
                        expires_at: (state == "expired").then(|| unix_now() - 50),
                        last_check_at: None,
                        check_count: 0,
                        grants: grants_of(subject),
                    },
                    &key_caps(),
                )
                .unwrap_or_else(|e| panic!("the key {id}: {e:?}"));
        }
        node.state.users.refresh(&node.state.store).unwrap();
        for counter in counters {
            let subject = counter["subject"].as_str().unwrap();
            let window = counter["window"].as_str().unwrap();
            let length = crate::limits::Window::parse(window).unwrap().seconds() as i64;
            let escape = |text: &str| text.replace('\'', "''");
            let owner = match subject.strip_prefix("key:") {
                Some(local) => format!("key_id = '{}'", escape(local)),
                None => format!(
                    "user_id = (SELECT id FROM users WHERE login = '{}')",
                    escape(subject)
                ),
            };
            store.execute_raw(&format!(
                "UPDATE budget_counters SET units = {}, window_start = {}
                  WHERE {owner} AND budget = '{}' AND \"window\" = '{window}'",
                counter["used"].as_i64().unwrap(),
                now - now.rem_euclid(length),
                escape(counter["limit"].as_str().unwrap()),
            ));
        }
    }

    /// Runs one case against a fresh node. Gives the failures, as text.
    fn run_case(backend: &Backend, dir: &std::path::Path, case: &Value) -> Vec<String> {
        let id = case["id"].as_str().unwrap();
        let given = &case["given"];
        let directory = match &given["directory"] {
            Value::String(name) => read_json(&dir.join("directories").join(name)),
            inline => inline.clone(),
        };
        let now = given
            .get("clock")
            .or_else(|| directory.get("clock"))
            .and_then(|c| c["now"].as_i64())
            .unwrap_or(0);
        let services: Vec<Service> = directory["services"]
            .as_array()
            .unwrap()
            .iter()
            .map(|s| Service {
                id: s["id"].as_str().unwrap().to_string(),
                document: s.get("document").cloned(),
                credential: credential_of(
                    s["id"].as_str().unwrap(),
                    s.get("credential_sha256").and_then(Value::as_str),
                    case,
                ),
            })
            .collect();
        let persons = directory["subjects"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|s| !s["id"].as_str().unwrap().contains(':'))
            .count();
        let pdp_port = free_port();
        // A directory with approvals needs the inbox with its items and the block `inbox.approvals`, and each
        // server has `approvals = true`. The cases without approvals run as before, with no inbox.
        let with_approvals = directory.get("approvals").is_some();
        let rig = if with_approvals {
            match InboxRig::new(backend, "5m") {
                Some(rig) => Some(rig),
                None => {
                    eprintln!("INBOX_TEST_PG is not set: {id} is skipped on PostgreSQL");
                    return Vec::new();
                }
            }
        } else {
            None
        };
        let mut clients = String::new();
        let mut env = Vec::new();
        for (index, service) in services.iter().enumerate() {
            clients.push_str(&format!(
                "pdp_client \"{}\" {{ token_env = \"CONFORMANCE_TOKEN_{index}\" approvals = {with_approvals} }}\n",
                service.id
            ));
            env.push((
                format!("CONFORMANCE_TOKEN_{index}"),
                service.credential.clone(),
            ));
        }
        if let Some(rig) = &rig {
            env.extend(rig.env.iter().cloned());
        }
        let inbox_blocks = rig.as_ref().map_or("", |rig| rig.config.as_str());
        let node = start_custom(
            Arc::new(backend.fresh()),
            None,
            Custom {
                max_users: persons.max(1),
                env,
                boot_inbox: with_approvals,
                inbox_clock_ms: with_approvals.then_some(now * 1000),
                ..Custom::default()
            },
            &format!(
                r#"identity {{ issuer = "https://portal.test" signing_key_env = "TEST_IDENTITY_KEY" }}
                pdp {{ listen_address = "127.0.0.1:{pdp_port}" url = "http://pdp.test:{pdp_port}" plain_http = true max_evaluations = 64 max_body_bytes = "1MiB" }}
                keys {{}}
                {clients}
                {inbox_blocks}"#
            ),
        );
        // The catalogue holds each service, so the registry keeps its document.
        let listed_services: Vec<crate::catalog::Service> = services
            .iter()
            .map(|s| {
                let href = format!("/{}/", s.id);
                let mut labels = vec![
                    ("portal.name", s.id.as_str()),
                    ("portal.id", s.id.as_str()),
                    ("portal.href", href.as_str()),
                ];
                if s.document.is_some() {
                    labels.push(("portal.actions", DB_ACTIONS_URL));
                }
                listed(&labels)
            })
            .collect();
        *node.state.catalog.write().unwrap() =
            Arc::new(Catalog::build(listed_services, Vec::new()));
        let mut failures = Vec::new();
        for service in &services {
            if let Some(document) = &service.document {
                match crate::actions::parse(&service.id, document.to_string().as_bytes()) {
                    Ok(document) => node.state.actions.set_document(&service.id, document),
                    Err(e) => failures.push(format!(
                        "{id}: the reader of the portal refuses the document of {}: {e}",
                        service.id
                    )),
                }
            }
        }
        if now != 0 {
            node.state.counting.set_clock(Some(now));
        }
        let counters: Vec<Value> = given
            .get("counters")
            .and_then(Value::as_array)
            .cloned()
            .or_else(|| directory.get("counters").and_then(Value::as_array).cloned())
            .unwrap_or_default();
        load(&node, &directory, &counters, now);
        if with_approvals {
            // The clock of the inbox stops at the clock of the data, so the times of the approvals mean the same.
            let app = app_of(&node, None);
            seed_directory(&app, &directory, 300);
        }
        wait_until(Duration::from_secs(10), || {
            TcpStream::connect(("127.0.0.1", pdp_port)).is_ok()
        });
        for (number, step) in case["steps"].as_array().unwrap().iter().enumerate() {
            let name = step.get("name").and_then(Value::as_str).unwrap_or("");
            let label = format!("{id} step {} ({name})", number + 1);
            let request = &step["request"];
            let (method, path) = match request.get("endpoint").and_then(Value::as_str) {
                Some("evaluation") => ("POST", "/access/v1/evaluation".to_string()),
                Some("evaluations") => ("POST", "/access/v1/evaluations".to_string()),
                Some("metadata") => ("GET", "/.well-known/authzen-configuration".to_string()),
                _ => (
                    request["method"].as_str().unwrap_or("POST"),
                    request["path"].as_str().unwrap_or("/").to_string(),
                ),
            };
            let body = match (request.get("body"), request.get("body_text")) {
                (Some(body), _) => body.to_string(),
                (None, Some(text)) => text.as_str().unwrap().to_string(),
                _ => String::new(),
            };
            let default_service = request
                .get("body")
                .and_then(service_of)
                .and_then(|id| services.iter().find(|s| s.id == id))
                .or_else(|| services.first());
            let mut headers: Vec<(String, String)> =
                vec![("Content-Type".into(), "application/json".into())];
            let mut given_headers = request
                .get("headers")
                .and_then(Value::as_object)
                .cloned()
                .unwrap_or_default();
            if !given_headers
                .keys()
                .any(|k| k.eq_ignore_ascii_case("authorization"))
                && method == "POST"
            {
                if let Some(service) = default_service {
                    given_headers.insert(
                        "Authorization".into(),
                        Value::String(format!("Bearer {}", service.credential)),
                    );
                }
            }
            for (name, value) in &given_headers {
                headers.push((name.clone(), value.as_str().unwrap_or("").to_string()));
            }
            let header_refs: Vec<(&str, &str)> = headers
                .iter()
                .map(|(k, v)| (k.as_str(), v.as_str()))
                .collect();
            let reply = send(pdp_port, method, &path, &header_refs, &body);
            let expect = &step["expect"];
            if let Some(matcher) = expect.get("status") {
                check(
                    &mut failures,
                    &label,
                    "status",
                    &Value::from(reply.status),
                    matcher,
                );
            }
            if let Some(pointers) = expect.get("json").and_then(Value::as_object) {
                let parsed: Option<Value> = serde_json::from_str(&reply.body).ok();
                for (pointer, matcher) in pointers {
                    let found = parsed.as_ref().and_then(|v| v.pointer(pointer));
                    match found {
                        Some(value) => check(&mut failures, &label, pointer, value, matcher),
                        None => {
                            if matcher.get("absent").is_none() {
                                failures.push(format!(
                                    "{label}: {pointer} is absent; expected {matcher}; body {}",
                                    reply.body
                                ));
                            }
                        }
                    }
                }
            }
            if let Some(matcher) = expect.get("body_text") {
                check(
                    &mut failures,
                    &label,
                    "body_text",
                    &Value::from(reply.body.clone()),
                    matcher,
                );
            }
        }
        failures
    }

    fn check(failures: &mut Vec<String>, label: &str, what: &str, value: &Value, matcher: &Value) {
        if let Some(expected) = matcher.get("equals") {
            if value != expected {
                failures.push(format!("{label}: {what} is {value}, expected {expected}"));
            }
        } else if matcher.get("absent").is_some() {
            failures.push(format!("{label}: {what} is {value}, expected it absent"));
        } else if matcher.get("present").is_some() {
            // The value is there.
        } else if let Some(pattern) = matcher.get("not_regex").and_then(Value::as_str) {
            let text = value.as_str().unwrap_or("");
            if super::conformance_e::regex::is_match(pattern, text) {
                failures.push(format!("{label}: {what} matches {pattern}: {text}"));
            }
        } else {
            failures.push(format!(
                "{label}: the matcher {matcher} is not read by the harness"
            ));
        }
    }

    #[test]
    fn the_cases_of_the_series_d_pass() {
        let Some(dir) = conformance_dir() else {
            eprintln!("PROCURAUTH_CONFORMANCE is not set: the cases of the series D are skipped");
            return;
        };
        let mut paths: Vec<_> = std::fs::read_dir(dir.join("cases/D"))
            .unwrap()
            .map(|e| e.unwrap().path())
            .filter(|p| p.extension().is_some_and(|e| e == "json"))
            .collect();
        paths.sort();
        let only = std::env::var("PROCURAUTH_CASES").ok();
        let mut failures = Vec::new();
        let mut run = 0;
        each_backend(|backend| {
            let _ = backend;
        });
        for backend in crate::store::testing::backends() {
            for path in &paths {
                let case = read_json(path);
                let id = case["id"].as_str().unwrap().to_string();
                if only
                    .as_ref()
                    .is_some_and(|only| !only.split(',').any(|o| o == id))
                {
                    continue;
                }
                run += 1;
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    run_case(&backend, &dir, &case)
                }));
                match result {
                    Ok(found) => failures.extend(
                        found
                            .into_iter()
                            .map(|f| format!("[{}] {f}", backend.name())),
                    ),
                    Err(_) => failures.push(format!(
                        "[{}] {id}: the harness or the portal panicked",
                        backend.name()
                    )),
                }
            }
        }
        eprintln!("{run} cases run, {} failures", failures.len());
        assert!(failures.is_empty(), "\n{}", failures.join("\n"));
    }
}

// The conformance cases of the series E (an edge) -------------------------------------------------

/// The cases of the series E drive an edge: Traefik with its ForwardAuth middleware for the keys,
/// and the portal behind it. The test acts as Traefik: it sends the headers of the request to
/// `/auth/verify-key`, it returns a refusal as it is, and it forwards an accepted request to the
/// service with the header of the identity token from the answer and without `Authorization`
/// (portal 8.1, 8.1a). The test also acts as the service: it records the request that arrives.
mod conformance_e {
    use super::*;
    use serde_json::Value;

    /// A small matcher of regular expressions for the patterns of the cases: anchors, literals,
    /// escapes, classes, groups, `?`, `*`, `+` and `{n,m}`.
    pub(super) mod regex {
        #[derive(Debug, Clone)]
        enum Node {
            Char(char),
            Any,
            Class(bool, Vec<(char, char)>),
            Start,
            End,
            Group(Vec<Node>),
            Repeat(Box<Node>, usize, usize),
        }

        fn escape_class(c: char) -> Option<(bool, Vec<(char, char)>)> {
            match c {
                's' => Some((
                    false,
                    vec![(' ', ' '), ('\t', '\t'), ('\n', '\n'), ('\r', '\r')],
                )),
                'S' => Some((
                    true,
                    vec![(' ', ' '), ('\t', '\t'), ('\n', '\n'), ('\r', '\r')],
                )),
                'd' => Some((false, vec![('0', '9')])),
                'w' => Some((false, vec![('a', 'z'), ('A', 'Z'), ('0', '9'), ('_', '_')])),
                _ => None,
            }
        }

        fn parse_seq(chars: &[char], i: &mut usize, in_group: bool) -> Vec<Node> {
            let mut out: Vec<Node> = Vec::new();
            while *i < chars.len() {
                let c = chars[*i];
                if c == ')' && in_group {
                    return out;
                }
                *i += 1;
                let node = match c {
                    '^' => Node::Start,
                    '$' => Node::End,
                    '.' => Node::Any,
                    '(' => {
                        let inner = parse_seq(chars, i, true);
                        *i += 1; // the closing parenthesis
                        Node::Group(inner)
                    }
                    '[' => {
                        let negate = chars.get(*i) == Some(&'^');
                        if negate {
                            *i += 1;
                        }
                        let mut items = Vec::new();
                        while chars[*i] != ']' {
                            let mut lo = chars[*i];
                            if lo == '\\' {
                                *i += 1;
                                lo = chars[*i];
                            }
                            if chars.get(*i + 1) == Some(&'-') && chars.get(*i + 2) != Some(&']') {
                                items.push((lo, chars[*i + 2]));
                                *i += 3;
                            } else {
                                items.push((lo, lo));
                                *i += 1;
                            }
                        }
                        *i += 1;
                        Node::Class(negate, items)
                    }
                    '\\' => {
                        let e = chars[*i];
                        *i += 1;
                        match escape_class(e) {
                            Some((negate, items)) => Node::Class(negate, items),
                            None => Node::Char(e),
                        }
                    }
                    other => Node::Char(other),
                };
                // A quantifier applies to the node before it.
                let (min, max) = match chars.get(*i) {
                    Some('?') => (0, 1),
                    Some('*') => (0, usize::MAX),
                    Some('+') => (1, usize::MAX),
                    Some('{') => {
                        let close = chars[*i..].iter().position(|c| *c == '}').unwrap() + *i;
                        let body: String = chars[*i + 1..close].iter().collect();
                        *i = close;
                        match body.split_once(',') {
                            Some((a, b)) => (
                                a.parse().unwrap(),
                                if b.is_empty() {
                                    usize::MAX
                                } else {
                                    b.parse().unwrap()
                                },
                            ),
                            None => (body.parse().unwrap(), body.parse().unwrap()),
                        }
                    }
                    _ => {
                        out.push(node);
                        continue;
                    }
                };
                *i += 1;
                out.push(Node::Repeat(Box::new(node), min, max));
            }
            out
        }

        /// The end positions after the node, from `pos`.
        fn run(node: &Node, text: &[char], pos: usize) -> Vec<usize> {
            match node {
                Node::Char(c) => (text.get(pos) == Some(c))
                    .then_some(pos + 1)
                    .into_iter()
                    .collect(),
                Node::Any => (pos < text.len()).then_some(pos + 1).into_iter().collect(),
                Node::Class(negate, items) => match text.get(pos) {
                    Some(c) => {
                        let inside = items.iter().any(|(lo, hi)| lo <= c && c <= hi);
                        (inside != *negate).then_some(pos + 1).into_iter().collect()
                    }
                    None => Vec::new(),
                },
                Node::Start => (pos == 0).then_some(pos).into_iter().collect(),
                Node::End => (pos == text.len()).then_some(pos).into_iter().collect(),
                Node::Group(nodes) => run_seq(nodes, text, pos),
                Node::Repeat(inner, min, max) => {
                    let mut results = Vec::new();
                    let mut frontier = vec![pos];
                    let mut count = 0;
                    if *min == 0 {
                        results.push(pos);
                    }
                    while count < *max && !frontier.is_empty() && count < text.len() + 2 {
                        let mut next = Vec::new();
                        for p in &frontier {
                            for end in run(inner, text, *p) {
                                if !next.contains(&end) {
                                    next.push(end);
                                }
                            }
                        }
                        count += 1;
                        if count >= *min {
                            for p in &next {
                                if !results.contains(p) {
                                    results.push(*p);
                                }
                            }
                        }
                        frontier = next;
                    }
                    results
                }
            }
        }

        fn run_seq(nodes: &[Node], text: &[char], pos: usize) -> Vec<usize> {
            let mut positions = vec![pos];
            for node in nodes {
                let mut next = Vec::new();
                for p in positions {
                    for end in run(node, text, p) {
                        if !next.contains(&end) {
                            next.push(end);
                        }
                    }
                }
                positions = next;
            }
            positions
        }

        /// Whether the pattern matches somewhere in the text. `^` and `$` anchor it.
        pub fn is_match(pattern: &str, text: &str) -> bool {
            let chars: Vec<char> = pattern.chars().collect();
            let nodes = parse_seq(&chars, &mut 0, false);
            let text: Vec<char> = text.chars().collect();
            (0..=text.len()).any(|start| !run_seq(&nodes, &text, start).is_empty())
        }

        #[test]
        fn the_matcher_reads_the_patterns_of_the_cases() {
            assert!(is_match("^2[0-9]{2}$", "204"));
            assert!(!is_match("^2[0-9]{2}$", "2044"));
            assert!(!is_match("^2[0-9]{2}$", "404"));
            let invalid = r#"^Bearer\s+(realm="[^"]*",\s*)?error="invalid_token"(,\s*error_description="[^"]*")?$"#;
            assert!(is_match(invalid, r#"Bearer error="invalid_token""#));
            assert!(is_match(
                invalid,
                r#"Bearer realm="x", error="invalid_token", error_description="y z""#
            ));
            assert!(!is_match(invalid, "Bearer"));
            assert!(!is_match(invalid, r#"Bearer error="invalid_request""#));
            let none = r#"^Bearer( realm="[^"]*")?$"#;
            assert!(is_match(none, "Bearer"));
            assert!(is_match(none, r#"Bearer realm="r""#));
            assert!(!is_match(none, r#"Bearer error="invalid_token""#));
            let jwt = r"^[A-Za-z0-9_-]{16,}\.[A-Za-z0-9_-]{16,}\.[A-Za-z0-9_-]{16,}$";
            assert!(is_match(
                jwt,
                "aaaaaaaaaaaaaaaaaa.bbbbbbbbbbbbbbbbbb.cccccccccccccccccc"
            ));
            assert!(!is_match(jwt, "forged.forged.forged"));
            assert!(is_match("mcp_", "xxmcp_yy") && !is_match("^mcp_", "xxmcp_"));
        }
    }

    fn conformance_dir() -> Option<std::path::PathBuf> {
        std::env::var_os("PROCURAUTH_CONFORMANCE").map(std::path::PathBuf::from)
    }

    /// What the service behind the edge got.
    #[derive(Default)]
    struct Upstream {
        requests: Vec<Vec<(String, String)>>,
    }

    struct Edge<'a> {
        node: &'a Node,
        upstream: std::sync::Mutex<Upstream>,
        token_header: String,
    }

    struct Answer {
        status: u16,
        headers: Vec<(String, String)>,
        body: String,
    }

    impl Edge<'_> {
        /// One request through the edge: the ForwardAuth call, then the service.
        fn request(
            &self,
            method: &str,
            path: &str,
            headers: &[(String, String)],
            body: &str,
        ) -> Answer {
            let mut auth: Vec<(&str, &str)> = vec![
                ("X-Forwarded-Method", method),
                ("X-Forwarded-Proto", "https"),
                ("X-Forwarded-Host", "portal.test"),
                ("X-Forwarded-For", "203.0.113.9"),
            ];
            auth.push(("X-Forwarded-Uri", path));
            for (name, value) in headers {
                if name.eq_ignore_ascii_case("authorization") {
                    auth.push(("Authorization", value));
                }
            }
            let reply = send(self.node.port, "GET", "/auth/verify-key", &auth, "");
            if !(200..300).contains(&reply.status) {
                // A refusal: Traefik returns the answer of the portal as it is.
                return Answer {
                    status: reply.status,
                    headers: reply.headers.clone(),
                    body: reply.body.clone(),
                };
            }
            let mut forwarded: Vec<(String, String)> = headers
                .iter()
                .filter(|(name, _)| {
                    !name.eq_ignore_ascii_case("authorization")
                        && !name.eq_ignore_ascii_case(&self.token_header)
                })
                .cloned()
                .collect();
            if let Some(token) = reply.header(&self.token_header) {
                forwarded.push((self.token_header.clone(), token.to_string()));
            }
            let _ = body;
            self.upstream.lock().unwrap().requests.push(forwarded);
            Answer {
                status: 200,
                headers: Vec::new(),
                body: String::new(),
            }
        }
    }

    fn replace_variables(text: &str, variables: &[(String, String)]) -> String {
        let mut out = text.to_string();
        for (name, value) in variables {
            out = out.replace(&format!("{{{{{name}}}}}"), value);
        }
        out
    }

    fn variables_in(value: &Value, variables: &[(String, String)]) -> Value {
        match value {
            Value::String(text) => Value::String(replace_variables(text, variables)),
            Value::Array(items) => {
                Value::Array(items.iter().map(|v| variables_in(v, variables)).collect())
            }
            Value::Object(map) => Value::Object(
                map.iter()
                    .map(|(k, v)| (replace_variables(k, variables), variables_in(v, variables)))
                    .collect(),
            ),
            other => other.clone(),
        }
    }

    fn matches(value: Option<&Value>, matcher: &Value) -> Result<(), String> {
        if let Some(expected) = matcher.get("equals") {
            return if value == Some(expected) {
                Ok(())
            } else {
                Err(format!("is {value:?}, expected {expected}"))
            };
        }
        if matcher.get("absent").is_some() {
            return if value.is_none() {
                Ok(())
            } else {
                Err(format!("is {value:?}, expected it absent"))
            };
        }
        if matcher.get("present").is_some() {
            return if value.is_some() {
                Ok(())
            } else {
                Err("is absent, expected it present".into())
            };
        }
        let text = value.map(|v| v.as_str().map_or_else(|| v.to_string(), str::to_string));
        if let Some(pattern) = matcher.get("regex").and_then(Value::as_str) {
            return match &text {
                Some(text) if regex::is_match(pattern, text) => Ok(()),
                other => Err(format!("is {other:?}, expected a match of {pattern}")),
            };
        }
        if let Some(pattern) = matcher.get("not_regex").and_then(Value::as_str) {
            return match &text {
                Some(text) if regex::is_match(pattern, text) => {
                    Err(format!("matches {pattern}: {text}"))
                }
                _ => Ok(()),
            };
        }
        if let Some(options) = matcher.get("one_of").and_then(Value::as_array) {
            return if value.is_some_and(|v| options.contains(v)) {
                Ok(())
            } else {
                Err(format!("is {value:?}, expected one of {options:?}"))
            };
        }
        Err(format!("the matcher {matcher} is not read by the harness"))
    }

    fn decode_part(token: &str, index: usize) -> Value {
        use base64ct::{Base64UrlUnpadded, Encoding};
        let part = token.split('.').nth(index).unwrap_or("");
        Base64UrlUnpadded::decode_vec(part)
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or(Value::Null)
    }

    fn run_case(backend: &Backend, case: &Value) -> Vec<String> {
        let id = case["id"].as_str().unwrap();
        let pdp_port = free_port();
        let node = start_with(
            Arc::new(backend.fresh()),
            None,
            &format!(
                r#"identity {{ issuer = "https://portal.test" signing_key_env = "TEST_IDENTITY_KEY" token_ttl = "5m" }}
                pdp {{ listen_address = "127.0.0.1:{pdp_port}" url = "http://pdp.test:{pdp_port}" plain_http = true }}
                keys {{}}
                pdp_client "probe" {{ token_env = "TEST_PDP_DB" }}"#
            ),
        );
        let catalog = Catalog::build(
            vec![listed(&[
                ("portal.name", "Probe"),
                ("portal.id", "probe"),
                ("portal.href", "/probe/"),
                ("portal.actions", DB_ACTIONS_URL),
            ])],
            Vec::new(),
        );
        *node.state.catalog.write().unwrap() = Arc::new(catalog);
        node.state.actions.set_document(
            "probe",
            crate::actions::parse(
                "probe",
                br#"{"server":"probe","resource_types":[],"actions":[{"name":"probe.read","resource_types":["service"],"description":"Read."}]}"#,
            )
            .unwrap(),
        );
        node.state
            .store
            .insert_user(&crate::store::UserRow {
                login: "ana".into(),
                password_hash: "h".into(),
                grants: vec![
                    Grant::parse("access on service:probe").unwrap(),
                    Grant::parse("keys.own.use on service:portal/key_kind:mcp").unwrap(),
                ],
                created_at: 1,
            })
            .unwrap();
        let make = |key_id: &str, byte: u8, grants: Vec<Grant>| -> String {
            let text = crate::keys::generate(crate::keys::KeyKind::Mcp)
                .unwrap()
                .to_string();
            let _ = byte;
            node.state
                .store
                .insert_key(
                    &crate::store::KeyRow {
                        kind: crate::keys::KeyKind::Mcp,
                        interaction: crate::keys::Interaction::Conversational,
                        id: key_id.into(),
                        owner: Some("ana".into()),
                        name: key_id.into(),
                        digest: crate::keys::digest(&text),
                        hint: crate::keys::hint(&text),
                        created_at: unix_now() - 10,
                        expires_at: None,
                        last_check_at: None,
                        check_count: 0,
                        grants,
                    },
                    &key_caps(),
                )
                .unwrap();
            text
        };
        let valid = make(
            "9b1c04e2aa71f0c3",
            1,
            vec![Grant::parse("access on service:probe").unwrap()],
        );
        let no_access = make("4d2e8f6a10b37c95", 2, Vec::new());
        // The grant of `access` has `confirm`: an edge asks only `access`, so it ignores `confirm` (protocol 17.8).
        let confirm_access = make(
            "6e5a1c3f9b2d7048",
            3,
            vec![Grant::parse("access on service:probe")
                .unwrap()
                .with_confirm(true)],
        );
        node.state.users.refresh(&node.state.store).unwrap();
        let unknown = crate::keys::generate(crate::keys::KeyKind::Mcp)
            .unwrap()
            .to_string();
        let token_header = node.state.config.identity.as_ref().unwrap().header.clone();
        let variables: Vec<(String, String)> = vec![
            ("valid_key_text".into(), valid),
            ("unknown_key_text".into(), unknown),
            ("no_access_key_text".into(), no_access),
            ("confirm_access_key_text".into(), confirm_access),
            ("valid_key_subject".into(), "key:9b1c04e2aa71f0c3".into()),
            ("valid_key_owner".into(), "ana".into()),
            ("token_header".into(), token_header.clone()),
            ("service".into(), "probe".into()),
            ("mcp_path".into(), "/probe/mcp".into()),
        ];
        let edge = Edge {
            node: &node,
            upstream: std::sync::Mutex::new(Upstream::default()),
            token_header,
        };
        let mut failures = Vec::new();
        for (number, step) in case["steps"].as_array().unwrap().iter().enumerate() {
            let label = format!(
                "{id} step {} ({})",
                number + 1,
                step.get("name").and_then(Value::as_str).unwrap_or("")
            );
            let request = variables_in(&step["request"], &variables);
            let method = request["method"].as_str().unwrap_or("GET").to_string();
            let path = request["path"].as_str().unwrap().to_string();
            let headers: Vec<(String, String)> = request
                .get("headers")
                .and_then(Value::as_object)
                .map(|h| {
                    h.iter()
                        .map(|(k, v)| (k.clone(), v.as_str().unwrap_or("").to_string()))
                        .collect()
                })
                .unwrap_or_default();
            let body = match (request.get("body"), request.get("body_text")) {
                (Some(body), _) => body.to_string(),
                (None, Some(text)) => text.as_str().unwrap_or("").to_string(),
                _ => String::new(),
            };
            let before = edge.upstream.lock().unwrap().requests.len();
            let answer = edge.request(&method, &path, &headers, &body);
            let received: Vec<Vec<(String, String)>> =
                edge.upstream.lock().unwrap().requests[before..].to_vec();
            let expect = variables_in(&step["expect"], &variables);
            let mut record = |what: &str, result: Result<(), String>| {
                if let Err(reason) = result {
                    failures.push(format!("[{}] {label}: {what} {reason}", backend.name()));
                }
            };
            if let Some(matcher) = expect.get("status") {
                record(
                    "status",
                    matches(Some(&Value::from(answer.status)), matcher),
                );
            }
            if let Some(headers) = expect.get("headers").and_then(Value::as_object) {
                for (name, matcher) in headers {
                    let value = answer
                        .headers
                        .iter()
                        .find(|(n, _)| n.eq_ignore_ascii_case(name))
                        .map(|(_, v)| Value::String(v.clone()));
                    record(&format!("header {name}"), matches(value.as_ref(), matcher));
                }
            }
            if let Some(matcher) = expect.get("body_text") {
                record(
                    "body_text",
                    matches(Some(&Value::String(answer.body.clone())), matcher),
                );
            }
            if let Some(upstream) = expect.get("upstream") {
                if let Some(matcher) = upstream.get("count") {
                    record(
                        "upstream count",
                        matches(Some(&Value::from(received.len())), matcher),
                    );
                }
                let first = received.first();
                if let Some(headers) = upstream.get("headers").and_then(Value::as_object) {
                    for (name, matcher) in headers {
                        let value = first.and_then(|h| {
                            h.iter()
                                .find(|(n, _)| n.eq_ignore_ascii_case(name))
                                .map(|(_, v)| Value::String(v.clone()))
                        });
                        record(
                            &format!("upstream header {name}"),
                            matches(value.as_ref(), matcher),
                        );
                    }
                }
                let token = first.and_then(|h| {
                    h.iter()
                        .find(|(n, _)| n.eq_ignore_ascii_case(&edge.token_header))
                        .map(|(_, v)| v.clone())
                });
                for (member, part) in [("token_header", 0), ("token_claims", 1)] {
                    if let Some(pointers) = upstream.get(member).and_then(Value::as_object) {
                        let decoded = token.as_ref().map(|t| decode_part(t, part));
                        for (pointer, matcher) in pointers {
                            let value = decoded.as_ref().and_then(|d| d.pointer(pointer));
                            record(&format!("{member} {pointer}"), matches(value, matcher));
                        }
                    }
                }
            }
        }
        failures
    }

    #[test]
    fn the_cases_of_the_series_e_pass() {
        let Some(dir) = conformance_dir() else {
            eprintln!("PROCURAUTH_CONFORMANCE is not set: the cases of the series E are skipped");
            return;
        };
        let mut paths: Vec<_> = std::fs::read_dir(dir.join("cases/E"))
            .unwrap()
            .map(|e| e.unwrap().path())
            .filter(|p| p.extension().is_some_and(|e| e == "json"))
            .collect();
        paths.sort();
        let mut failures = Vec::new();
        let mut run = 0;
        for backend in crate::store::testing::backends() {
            for path in &paths {
                let case: Value =
                    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
                run += 1;
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    run_case(&backend, &case)
                }));
                match result {
                    Ok(found) => failures.extend(found),
                    Err(_) => failures.push(format!(
                        "[{}] {}: the harness or the portal panicked",
                        backend.name(),
                        case["id"]
                    )),
                }
            }
        }
        eprintln!("{run} cases run, {} failures", failures.len());
        assert!(failures.is_empty(), "\n{}", failures.join("\n"));
    }
}
