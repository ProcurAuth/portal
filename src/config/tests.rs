use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};

const VARS: &[(&str, &str)] = &[
    ("PORTAL_LOGIN", "admin"),
    ("PORTAL_PASSWORD", "a password for the tests"),
    ("PORTAL_SESSION", "0123456789abcdef0123456789abcdef"),
    ("PORTAL_DB_PASSWORD", "database password"),
    ("API_HOST", "10.43.0.1"),
    ("API_PORT", "443"),
    ("PDP_DB", "a token of the server db, at least 32 bytes"),
    ("PDP_CHAT", "a token of the server chat, at least 32 bytes"),
    ("SHORT", "too short"),
    (
        "IDENTITY_KEY",
        "MC4CAQAwBQYDK2VwBCIEIAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8g",
    ),
];

/// A configuration with the two mandatory sections. Extra blocks come after it.
const MINIMAL: &str = r#"
    server { listen_address = "0.0.0.0:4181" }
    authentication {
      superadmin_login_env    = "PORTAL_LOGIN"
      superadmin_password_env = "PORTAL_PASSWORD"
    }
"#;

fn parse_with(raw: &str, vars: &[(&str, &str)]) -> Result<(Config, Secrets), String> {
    let vars: Vec<(String, String)> = vars
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
    Config::parse(raw, &move |name| {
        vars.iter().find(|(k, _)| k == name).map(|(_, v)| v.clone())
    })
}

fn parse(raw: &str) -> Result<(Config, Secrets), String> {
    parse_with(raw, VARS)
}

fn ok(raw: &str) -> (Config, Secrets) {
    parse(raw).unwrap_or_else(|e| panic!("{e}\n{raw}"))
}

fn err(raw: &str) -> String {
    match parse(raw) {
        Ok(_) => panic!("must be rejected:\n{raw}"),
        Err(e) => e,
    }
}

fn with(extra: &str) -> String {
    format!("{MINIMAL}\n{extra}")
}

/// The minimal configuration with more keys in the `server` block.
fn with_server(server_keys: &str, extra: &str) -> String {
    MINIMAL.replace(
        r#"listen_address = "0.0.0.0:4181""#,
        &format!(r#"listen_address = "0.0.0.0:4181" {server_keys}"#),
    ) + "\n"
        + extra
}

/// The configuration for PostgreSQL: it needs the shared signing key.
fn shared() -> String {
    with(r#"session { secret_mode = "provided" secret_env = "PORTAL_SESSION" }"#)
}

const POSTGRES_BLOCK: &str = r#"postgres {
    host = "postgresql-rw.data.svc.cluster.local"
    database = "portal"
    user = "portal"
    password_env = "PORTAL_DB_PASSWORD"
}"#;

fn temp_file(content: &[u8]) -> PathBuf {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let path = std::env::temp_dir().join(format!(
        "portal-config-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    fs::write(&path, content).unwrap();
    path
}

fn line<'a>(config: &'a Config, key: &str) -> &'a Effective {
    config
        .effective
        .iter()
        .find(|l| l.key == key)
        .unwrap_or_else(|| panic!("{key} is not in the effective configuration"))
}

#[test]
fn parses_the_minimal_configuration_with_the_documented_defaults() {
    let (cfg, secrets) = ok(MINIMAL);
    assert_eq!(secrets.superadmin_login, "admin");
    assert_eq!(secrets.superadmin_password.as_bytes(), b"a password for the tests");
    assert!(secrets.session_secret.is_none());
    assert!(secrets.postgres_password.is_none());
    assert_eq!(cfg.access.grants_mode, Mode::Dynamic);
    assert!(cfg.access.configured_grants.is_empty());
    assert_eq!(cfg.access.actions_timeout, Duration::from_secs(2));
    assert_eq!(cfg.access.actions_refresh, Duration::from_secs(300));
    assert!(cfg.identity.is_none() && secrets.identity_key.is_none());
    assert!(cfg.pdp.is_none() && secrets.pdp_tokens.is_empty());
    assert_eq!(cfg.users.max_users, 64);
    assert!(matches!(
        &cfg.storage.backend,
        StorageBackend::Sqlite(path) if path == &PathBuf::from("/var/lib/portal/portal.db")
    ));
    assert_eq!(cfg.storage.retention_days, 90);
    assert_eq!(cfg.server.workers, 4);
    assert_eq!(cfg.server.max_body_bytes, 32 * 1024);
    assert_eq!(cfg.server.max_connections, 1024);
    assert_eq!(cfg.server.max_headers, 64);
    assert_eq!(cfg.server.max_header_bytes, 32 * 1024);
    assert_eq!(cfg.server.header_read_timeout, Duration::from_secs(10));
    assert_eq!(cfg.session.ttl, Duration::from_secs(8 * 3600));
    assert_eq!(cfg.session.idle_timeout, Duration::from_secs(30 * 60));
    assert_eq!(cfg.session.bind, SessionBind::IpUserAgent);
    assert_eq!(cfg.session.max_active_sessions, 4096);
    assert_eq!(cfg.session.cookie_name, "__Host-portal");
    assert!(cfg.metrics.enabled);
    assert_eq!(cfg.metrics.path, "/metrics");
    assert!(cfg.discovery.kubernetes.is_none());
    assert!(cfg.discovery.docker_socket.is_none());
    assert_eq!(cfg.discovery.interval, Duration::from_secs(15));
    assert_eq!(cfg.ui.icons_dir, PathBuf::from("/icons"));
    assert_eq!(cfg.logging.level, LogLevel::Info);
}

#[test]
fn every_default_of_the_table_is_a_valid_value() {
    // The minimal file uses every default. A bad default makes this parse fail.
    ok(MINIMAL);
    for key in keys::KEYS {
        if let keys::Presence::Default(text) = key.presence {
            if !key.is_secret() {
                resolve::check_default(key, text).unwrap_or_else(|e| panic!("{e}"));
            }
        }
    }
}

// PTL-CFG-7: effective configuration with sources.

#[test]
fn the_effective_configuration_gives_the_source_of_each_value_and_hides_the_secrets() {
    let file = temp_file(b"0123456789abcdef0123456789abcdef\n");
    let raw = with_server(
        "workers = 8",
        &format!(
            r#"session {{ secret_mode = "provided" secret_file = "{}" }}"#,
            file.display()
        ),
    );
    let (cfg, _secrets) = ok(&raw);
    assert_eq!(line(&cfg, "server.workers").source, "file");
    assert_eq!(line(&cfg, "server.workers").value, "8");
    assert_eq!(line(&cfg, "server.max_connections").source, "default");
    assert_eq!(line(&cfg, "server.max_connections").value, "1024");
    assert_eq!(line(&cfg, "server.max_body_bytes").value, "\"32KiB\"");
    assert_eq!(line(&cfg, "logging.level").value, "\"info\"");
    assert_eq!(line(&cfg, "authentication.superadmin_login").source, "env:PORTAL_LOGIN");
    assert_eq!(line(&cfg, "authentication.superadmin_login").value, "<hidden>");
    assert_eq!(
        line(&cfg, "authentication.superadmin_password").source,
        "env:PORTAL_PASSWORD"
    );
    assert_eq!(line(&cfg, "session.secret").value, "<hidden>");
    assert_eq!(
        line(&cfg, "session.secret").source,
        format!("file:{}", file.display())
    );
    let text = cfg.render_effective();
    assert!(!text.contains("a password for the tests"));
    assert!(!text.contains("0123456789abcdef"));
    assert!(!text.contains("= admin"), "the login is hidden too");
    assert!(text.contains("server.workers = 8  # file\n"));
    fs::remove_file(file).ok();
}

#[test]
fn a_key_that_does_not_apply_is_not_in_the_effective_configuration() {
    let (cfg, _) = ok(MINIMAL);
    assert!(cfg.effective.iter().all(|l| !l.key.starts_with("postgres.")));
    assert!(cfg.effective.iter().all(|l| l.key != "session.secret"));
    assert!(cfg.effective.iter().all(|l| l.key != "discovery.kubernetes_host_env"));
    assert!(cfg.effective.iter().all(|l| l.key != "discovery.docker_socket"));
    let (cfg, _) = ok(&format!("{}\n{POSTGRES_BLOCK}", shared()));
    assert!(cfg.effective.iter().any(|l| l.key == "postgres.host"));
    assert!(cfg.effective.iter().all(|l| l.key != "storage.database"));
}

// PTL-CFG-5: _env and _file on the four secrets.

#[test]
fn a_secret_comes_from_a_variable_or_from_a_file_and_the_file_loses_one_line_end() {
    let login = temp_file(b"root\n");
    let password = temp_file(b"pass word\r\n");
    let raw = format!(
        r#"server {{ listen_address = "0.0.0.0:4181" }}
        authentication {{ superadmin_login_file = "{}" superadmin_password_file = "{}" }}"#,
        login.display(),
        password.display()
    );
    let (cfg, secrets) = ok(&raw);
    assert_eq!(secrets.superadmin_login, "root");
    assert_eq!(secrets.superadmin_password.as_bytes(), b"pass word");
    assert_eq!(
        line(&cfg, "authentication.superadmin_login").source,
        format!("file:{}", login.display())
    );
    fs::remove_file(login).ok();
    fs::remove_file(password).ok();

    let two_ends = temp_file(b"secret\n\n");
    let raw = MINIMAL.replace(
        r#"superadmin_password_env = "PORTAL_PASSWORD""#,
        &format!(r#"superadmin_password_file = "{}""#, two_ends.display()),
    );
    let (_, secrets) = ok(&raw);
    assert_eq!(secrets.superadmin_password.as_bytes(), b"secret\n", "only the last line end goes");
    fs::remove_file(two_ends).ok();
}

#[test]
fn all_four_secrets_accept_both_forms() {
    let file = temp_file(b"0123456789abcdef0123456789abcdef");
    let forms = [
        r#"env = "PORTAL_SESSION""#.to_string(),
        format!(r#"file = "{}""#, file.display()),
    ];
    for form in &forms {
        let (_, secrets) = ok(&format!(
            r#"server {{ listen_address = "0.0.0.0:4181" }}
            authentication {{ superadmin_login_{form} superadmin_password_env = "PORTAL_PASSWORD" }}"#
        ));
        assert_eq!(secrets.superadmin_login, "0123456789abcdef0123456789abcdef");
        let (_, secrets) = ok(&format!(
            r#"server {{ listen_address = "0.0.0.0:4181" }}
            authentication {{ superadmin_login_env = "PORTAL_LOGIN" superadmin_password_{form} }}"#
        ));
        assert_eq!(secrets.superadmin_password.len(), 32);
        let (_, secrets) = ok(&with(&format!(r#"session {{ secret_mode = "provided" secret_{form} }}"#)));
        assert_eq!(secrets.session_secret.unwrap().len(), 32);
        let block = POSTGRES_BLOCK.replace(r#"password_env = "PORTAL_DB_PASSWORD""#, &format!("password_{form}"));
        let (_, secrets) = ok(&format!("{}\n{block}", shared()));
        assert_eq!(secrets.postgres_password.unwrap().len(), 32);
    }
    fs::remove_file(file).ok();
}

#[test]
fn a_secret_in_clear_text_in_the_file_is_refused() {
    for (from, to) in [
        (r#"superadmin_password_env = "PORTAL_PASSWORD""#, r#"superadmin_password = "hunter2hunter2""#),
        (r#"superadmin_login_env    = "PORTAL_LOGIN""#, r#"superadmin_login = "admin""#),
    ] {
        let message = err(&MINIMAL.replace(from, to));
        assert!(message.contains("must not be written in the file"), "{message}");
        assert!(!message.contains("hunter2"), "{message}");
    }
    let message = err(&with(
        r#"session { secret_mode = "provided" secret = "0123456789abcdef0123456789abcdef" }"#,
    ));
    assert!(message.contains("session.secret"), "{message}");
    assert!(!message.contains("0123456789"), "{message}");
    let message = err(&format!(
        "{}\n{}",
        shared(),
        POSTGRES_BLOCK.replace("password_env = \"PORTAL_DB_PASSWORD\"", "password = \"clear\"")
    ));
    assert!(message.contains("postgres.password"), "{message}");
}

#[test]
fn a_secret_needs_exactly_one_form() {
    let none = MINIMAL.replace(r#"superadmin_password_env = "PORTAL_PASSWORD""#, "");
    let message = err(&none);
    assert!(message.contains("authentication.superadmin_password_env"), "{message}");
    assert!(message.contains("authentication.superadmin_password_file"), "{message}");

    let both = MINIMAL.replace(
        r#"superadmin_password_env = "PORTAL_PASSWORD""#,
        r#"superadmin_password_env = "PORTAL_PASSWORD" superadmin_password_file = "/run/secrets/p""#,
    );
    let message = err(&both);
    assert!(message.contains("exactly one"), "{message}");
}

#[test]
fn a_missing_empty_or_unreadable_source_stops_the_start() {
    // The variable does not exist.
    let message = parse_with(MINIMAL, &[("PORTAL_LOGIN", "admin")]).err().unwrap();
    assert!(message.contains("PORTAL_PASSWORD"), "{message}");
    assert!(message.contains("superadmin_password_env"), "{message}");
    // The variable is empty.
    let message = parse_with(MINIMAL, &[("PORTAL_LOGIN", "admin"), ("PORTAL_PASSWORD", "")])
        .err()
        .unwrap();
    assert!(message.contains("PORTAL_PASSWORD") && message.contains("empty"), "{message}");
    // The file does not exist.
    let raw = MINIMAL.replace(
        r#"superadmin_password_env = "PORTAL_PASSWORD""#,
        r#"superadmin_password_file = "/nonexistent/portal-password""#,
    );
    let message = err(&raw);
    assert!(message.contains("/nonexistent/portal-password"), "{message}");
    assert!(message.contains("superadmin_password_file"), "{message}");
    // The file is empty, or holds only a line end.
    for content in [&b""[..], &b"\n"[..]] {
        let empty = temp_file(content);
        let raw = MINIMAL.replace(
            r#"superadmin_password_env = "PORTAL_PASSWORD""#,
            &format!(r#"superadmin_password_file = "{}""#, empty.display()),
        );
        let message = err(&raw);
        assert!(message.contains("empty"), "{message}");
        fs::remove_file(empty).ok();
    }
    // The file path is relative, or the variable name is not valid.
    let raw = MINIMAL.replace(
        r#"superadmin_password_env = "PORTAL_PASSWORD""#,
        r#"superadmin_password_file = "relative/password""#,
    );
    assert!(err(&raw).contains("absolute"));
    let raw = MINIMAL.replace(r#""PORTAL_PASSWORD""#, r#""1BAD NAME""#);
    assert!(err(&raw).contains("variable"));
    // A secret that is too large.
    let big = "x".repeat(300);
    let message = parse_with(MINIMAL, &[("PORTAL_LOGIN", &big), ("PORTAL_PASSWORD", "p")])
        .err()
        .unwrap();
    assert!(message.contains("256"), "{message}");
}

#[test]
fn the_password_is_bounded_by_the_security_limit() {
    let raw = with("security { max_password_bytes = 8 }");
    let message = err(&raw);
    assert!(message.contains("max_password_bytes"), "{message}");
}

#[test]
fn the_session_secret_modes() {
    let (_, secrets) = ok(&with(
        r#"session { secret_mode = "provided" secret_env = "PORTAL_SESSION" }"#,
    ));
    assert_eq!(secrets.session_secret.unwrap().len(), 32);
    // `provided` needs a source.
    let message = err(&with(r#"session { secret_mode = "provided" }"#));
    assert!(message.contains("session.secret_env"), "{message}");
    // `ephemeral` refuses one.
    let message = err(&with(r#"session { secret_env = "PORTAL_SESSION" }"#));
    assert!(message.contains("session.secret_mode is ephemeral"), "{message}");
    // The old mode name is gone.
    assert!(err(&with(r#"session { secret_mode = "file" secret_env = "PORTAL_SESSION" }"#))
        .contains("session.secret_mode"));
    // At least 32 bytes.
    let raw = with(r#"session { secret_mode = "provided" secret_env = "SHORT" }"#);
    let message = parse_with(&raw, &[("PORTAL_LOGIN", "a"), ("PORTAL_PASSWORD", "p"), ("SHORT", "abc")])
        .err()
        .unwrap();
    assert!(message.contains("minimum is 32") && message.contains("env:SHORT"), "{message}");
    assert!(!message.contains("abc"), "{message}");
}

// PTL-CFG-3: the login is refused, not changed.

#[test]
fn a_superadmin_login_with_an_upper_case_letter_or_a_space_is_refused() {
    for bad in ["Admin", "ADMIN", "ad min", " admin", "admin ", "ad\tmin", "adm\u{e9}N", "ad\u{0}min"] {
        let message = parse_with(MINIMAL, &[("PORTAL_LOGIN", bad), ("PORTAL_PASSWORD", "p")])
            .err()
            .unwrap_or_else(|| panic!("{bad:?} must be refused"));
        assert!(message.contains("authentication.superadmin_login"), "{message}");
        assert!(!message.contains(bad), "the login stays out of the error: {message}");
    }
    for good in ["admin", "root-1", "a.b_c@example.org", "\u{e9}l\u{e8}ve"] {
        let (_, secrets) = parse_with(MINIMAL, &[("PORTAL_LOGIN", good), ("PORTAL_PASSWORD", "p")]).unwrap();
        assert_eq!(secrets.superadmin_login, good);
    }
    let long = "a".repeat(65);
    assert!(parse_with(MINIMAL, &[("PORTAL_LOGIN", &long), ("PORTAL_PASSWORD", "p")]).is_err());
    let file = temp_file(b"Admin\n");
    let raw = MINIMAL.replace(
        r#"superadmin_login_env    = "PORTAL_LOGIN""#,
        &format!(r#"superadmin_login_file = "{}""#, file.display()),
    );
    assert!(err(&raw).contains("upper-case"));
    fs::remove_file(file).ok();
}

// PTL-CFG-4: the Kubernetes API variables are named in the file.

#[test]
fn the_kubernetes_api_comes_from_the_variables_that_the_file_names() {
    let raw = with(
        r#"discovery { kubernetes = true kubernetes_host_env = "API_HOST" kubernetes_port_env = "API_PORT" }"#,
    );
    let (cfg, _) = ok(&raw);
    assert_eq!(
        cfg.discovery.kubernetes,
        Some(KubernetesApi { host: "10.43.0.1".into(), port: 443 })
    );
    assert_eq!(line(&cfg, "discovery.kubernetes_host_env").value, "\"API_HOST\"");
    assert_eq!(line(&cfg, "discovery.kubernetes_host").source, "env:API_HOST");
    assert_eq!(line(&cfg, "discovery.kubernetes_port").value, "443");

    // An IPv6 address gets its brackets for the URL.
    let (cfg, _) = parse_with(
        &raw,
        &[("PORTAL_LOGIN", "a"), ("PORTAL_PASSWORD", "p"), ("API_HOST", "fd00::1"), ("API_PORT", "6443")],
    )
    .unwrap();
    assert_eq!(cfg.discovery.kubernetes.unwrap().host, "[fd00::1]");
}

#[test]
fn the_kubernetes_variables_are_mandatory_and_checked_at_start() {
    let names = r#"kubernetes_host_env = "API_HOST" kubernetes_port_env = "API_PORT""#;
    // The names are missing.
    let message = err(&with("discovery { kubernetes = true }"));
    assert!(message.contains("discovery.kubernetes_host_env"), "{message}");
    // The names are present but the switch is off.
    let message = err(&with(&format!("discovery {{ {names} }}")));
    assert!(message.contains("discovery.kubernetes_host_env must not be set"), "{message}");
    // A variable does not exist, is empty or is not a port.
    let raw = with(&format!("discovery {{ kubernetes = true {names} }}"));
    let base = [("PORTAL_LOGIN", "a"), ("PORTAL_PASSWORD", "p")];
    let with_api = |host: &'static str, port: &'static str| {
        let mut vars = base.to_vec();
        vars.push(("API_HOST", host));
        vars.push(("API_PORT", port));
        parse_with(&raw, &vars).err().unwrap()
    };
    assert!(parse_with(&raw, &base).err().unwrap().contains("API_HOST"));
    assert!(with_api("", "443").contains("empty"));
    assert!(with_api("10.0.0.1", "").contains("API_PORT"));
    assert!(with_api("10.0.0.1", "https").contains("port number"));
    assert!(with_api("10.0.0.1", "0").contains("port number"));
    assert!(with_api("10.0.0.1", "70000").contains("port number"));
    assert!(with_api("10.0.0.1 x", "443").contains("host name"));
}

// PTL-CFG-2: strict types.

#[test]
fn a_number_in_quotes_and_a_word_without_quotes_are_refused() {
    for (key, bad) in [
        ("server.workers", r#"workers = "4""#),
        ("server.max_body_bytes", "max_body_bytes = 8192"),
        ("server.max_body_bytes", "max_body_bytes = 8KiB"),
        ("server.header_read_timeout", "header_read_timeout = 10"),
        ("server.header_read_timeout", "header_read_timeout = 10s"),
        ("server.header_read_timeout", "header_read_timeout = ten"),
    ] {
        let message = err(&with_server(bad, ""));
        assert!(message.contains(key), "{bad}: {message}");
    }
    for (key, raw) in [
        ("security.login_rate_limit_enabled", r#"security { login_rate_limit_enabled = "true" }"#),
        ("session.bind", "session { bind = none }"),
        ("session.same_site", "session { same_site = Strict }"),
        ("logging.level", "logging { level = info }"),
        ("metrics.enabled", "metrics { enabled = yes }"),
        ("users.max_users", "users { max_users = 05 }"),
        ("users.max_users", "users { max_users = 5.0 }"),
        ("users.max_users", "users { max_users = 5x }"),
        ("users.max_users", r#"users { max_users = "5" }"#),
    ] {
        let message = err(&with(raw));
        assert!(message.contains(key), "{raw}: {message}");
    }
    // A path without quotes is not even a scalar.
    err(&with("metrics { path = /metrics }"));
    // The good forms.
    ok(&with("users { max_users = 5 } security { login_rate_limit_enabled = false }"));
}

#[test]
fn letter_case_is_exact_for_booleans_and_enums() {
    for bad in [
        "security { login_rate_limit_enabled = True }",
        "security { login_rate_limit_enabled = TRUE }",
        r#"session { same_site = "strict" }"#,
        r#"session { same_site = "LAX" }"#,
        r#"session { bind = "IP_USER_AGENT" }"#,
        r#"session { secret_mode = "Ephemeral" }"#,
        r#"logging { level = "INFO" }"#,
        r#"logging { level = "Debug" }"#,
        r#"logging { level = "warning" }"#,
    ] {
        err(&with(bad));
    }
    let (cfg, _) = ok(&with(
        r#"session { same_site = "Lax" bind = "none" } logging { level = "warn" }"#,
    ));
    assert_eq!(cfg.logging.level, LogLevel::Warn);
    assert_eq!(cfg.session.bind, SessionBind::None);
    assert_eq!(cfg.session.same_site.as_str(), "Lax");
}

#[test]
fn a_size_needs_its_unit() {
    let message = err(&with_server(r#"max_body_bytes = "8192""#, ""));
    assert!(message.contains("unit"), "{message}");
    let (cfg, _) = ok(&with_server(r#"max_body_bytes = "8192B""#, ""));
    assert_eq!(cfg.server.max_body_bytes, 8192);
    err(&with_server(r#"max_body_bytes = "8kib""#, ""));
}

// PTL-CFG-1: the error holds the key and the value.

#[test]
fn an_error_names_the_key_what_it_accepts_and_the_value() {
    let server_cases = [
        ("workers = 0", "server.workers", "0"),
        ("workers = 65", "server.workers", "65"),
        (r#"header_read_timeout = "2m""#, "server.header_read_timeout", "\"2m\""),
        (r#"header_read_timeout = "ten""#, "server.header_read_timeout", "\"ten\""),
        (r#"max_body_bytes = "8GiB""#, "server.max_body_bytes", "\"8GiB\""),
        (r#"listen_address = "nowhere""#, "server.listen_address", "\"nowhere\""),
    ];
    for (body, key, value) in server_cases {
        let raw = MINIMAL.replace(r#"listen_address = "0.0.0.0:4181""#, body);
        let raw = if body.starts_with("listen_address") {
            raw
        } else {
            with_server(body, "")
        };
        let message = err(&raw);
        assert!(message.contains(key), "{key}: {message}");
        assert!(message.contains(value), "{value}: {message}");
        assert!(message.contains("expected"), "{message}");
    }
    let cases = [
        (r#"session { same_site = "None" }"#, "session.same_site", "\"None\""),
        (r#"session { cookie_name = "portal" }"#, "session.cookie_name", "\"portal\""),
        (r#"metrics { path = "metrics" }"#, "metrics.path", "\"metrics\""),
        (r#"discovery { docker_socket = "docker.sock" }"#, "discovery.docker_socket", "\"docker.sock\""),
        (r#"access { grants_mode = "Static" }"#, "access.grants_mode", "\"Static\""),
        (r#"logging { level = "loud" }"#, "logging.level", "\"loud\""),
    ];
    for (block, key, value) in cases {
        let message = err(&with(block));
        assert!(message.contains(key), "{key}: {message}");
        assert!(message.contains(value), "{value}: {message}");
        assert!(message.contains("expected"), "{message}");
    }
}

#[test]
fn a_cross_key_error_holds_the_values() {
    let message = err(&with(r#"session { ttl = "1h" idle_timeout = "2h" }"#));
    assert!(message.contains("2h") && message.contains("1h"), "{message}");
}

// PTL-CFG-8: a value of something that is off.

#[test]
fn the_values_of_a_limiter_that_is_off_are_refused() {
    for body in [
        "login_rate_limit_enabled = false login_attempts_per_ip = 3",
        r#"login_rate_limit_enabled = false login_window = "1m""#,
        r#"login_rate_limit_enabled = false login_lockout = "1m""#,
        "global_login_rate_limit_enabled = false global_login_attempts = 3",
        r#"global_login_rate_limit_enabled = false global_login_window = "1m""#,
        r#"global_login_rate_limit_enabled = false global_login_lockout = "1m""#,
        "login_rate_limit_enabled = false global_login_rate_limit_enabled = false max_rate_limit_entries = 100",
    ] {
        let message = err(&with(&format!("security {{ {body} }}")));
        assert!(message.contains("must not be set when"), "{body}: {message}");
    }
    // Off with no value is fine, and the limiter values are not in the effective configuration.
    let (cfg, _) = ok(&with(
        "security { login_rate_limit_enabled = false global_login_rate_limit_enabled = false }",
    ));
    assert!(!cfg.security.login_rate_limit_enabled);
    for key in [
        "login_attempts_per_ip", "login_window", "login_lockout", "global_login_attempts",
        "global_login_window", "global_login_lockout", "max_rate_limit_entries",
    ] {
        assert!(cfg.effective.iter().all(|l| l.key != format!("security.{key}")), "{key}");
    }
    // One limiter on keeps the shared table size.
    ok(&with("security { login_rate_limit_enabled = false max_rate_limit_entries = 100 }"));
}

#[test]
fn metrics_path_is_refused_when_the_metrics_are_off() {
    let message = err(&with(r#"metrics { enabled = false path = "/metrics" }"#));
    assert!(message.contains("metrics.path must not be set when metrics.enabled is false"), "{message}");
    let (cfg, _) = ok(&with("metrics { enabled = false }"));
    assert!(!cfg.metrics.enabled);
    assert!(cfg.effective.iter().all(|l| l.key != "metrics.path"));
    ok(&with(r#"metrics { enabled = true path = "/prom" }"#));
}

// The closed schema.

#[test]
fn rejects_an_unknown_key_block_or_duplicate() {
    assert!(err(&with(r#"logging { magic = "yes" }"#)).contains("unknown configuration key: logging.magic"));
    assert!(err(&with("magic { }")).contains("magic"));
    assert!(err(&with(r#"logging { level = "info" level = "warn" }"#)).contains("duplicate"));
    assert!(err(&with(r#"logging { level = "info" } logging { level = "warn" }"#)).contains("duplicate"));
    // The old attribute names.
    assert!(err(&MINIMAL.replace("superadmin_login_env", "login_file")).contains("unknown"));
    assert!(err(&with(r#"session { secret_file_x = "/p" }"#)).contains("unknown"));
}

#[test]
fn an_attribute_that_is_not_a_secret_does_not_accept_env_or_file() {
    for raw in [
        with_server(r#"workers_env = "WORKERS""#, ""),
        with(r#"logging { level_file = "/etc/level" }"#),
        with(r#"storage { database_file = "/etc/db" }"#),
    ] {
        let message = err(&raw);
        assert!(message.contains("does not accept"), "{message}");
    }
}

#[test]
fn the_listen_address_is_mandatory() {
    let message = err(&MINIMAL.replace(r#"listen_address = "0.0.0.0:4181""#, "workers = 4"));
    assert!(message.contains("server.listen_address"), "{message}");
}

// The rest of the keys.

#[test]
fn discovery_is_off_by_default_and_parses_its_keys() {
    let (cfg, _) = ok(&with(
        r#"discovery { interval = "30s" probe_timeout = "200ms" docker_socket = "/var/run/docker.sock" }"#,
    ));
    assert!(cfg.discovery.kubernetes.is_none());
    assert_eq!(cfg.discovery.docker_socket, Some(PathBuf::from("/var/run/docker.sock")));
    assert_eq!(cfg.discovery.probe_timeout, Duration::from_millis(200));
    for body in [r#"interval = "1s""#, r#"docker_socket = "docker.sock""#, r#"kubernetes = "maybe""#, r#"docker_socket = "/a/../b""#] {
        err(&with(&format!("discovery {{ {body} }}")));
    }
}

#[test]
fn parses_idle_timeout_and_binding_and_refuses_bad_ones() {
    let (cfg, _) = ok(&with(r#"session { idle_timeout = "10m" bind = "user_agent" }"#));
    assert_eq!(cfg.session.idle_timeout, Duration::from_secs(600));
    assert_eq!(cfg.session.bind, SessionBind::UserAgent);
    for extra in [r#"idle_timeout = "10s""#, r#"idle_timeout = "9h""#, r#"bind = "ip""#] {
        err(&with(&format!("session {{ {extra} }}")));
    }
}

#[test]
fn parses_users_and_storage() {
    let (cfg, _) = ok(&with(
        r#"users { max_users = 5 } storage { database = "/data/p.db" retention_days = 30 }"#,
    ));
    assert_eq!(cfg.users.max_users, 5);
    assert!(matches!(
        &cfg.storage.backend,
        StorageBackend::Sqlite(path) if path == &PathBuf::from("/data/p.db")
    ));
    assert_eq!(cfg.storage.retention_days, 30);
}

#[test]
fn rejects_bad_users_and_storage_values() {
    for block in [
        "users { max_users = 0 }",
        "users { max_users = 5000 }",
        r#"storage { database = "relative.db" }"#,
        "storage { retention_days = 0 }",
        "storage { retention_days = 9999 }",
    ] {
        err(&format!("{}\n{block}", shared()));
    }
}

#[test]
fn the_postgres_block_replaces_the_sqlite_file() {
    let raw = format!("{}\nstorage {{ retention_days = 30 }}\n{POSTGRES_BLOCK}", shared());
    let (cfg, secrets) = ok(&raw);
    let StorageBackend::Postgres(pg) = &cfg.storage.backend else {
        panic!("expected the postgres backend");
    };
    assert_eq!(pg.host, "postgresql-rw.data.svc.cluster.local");
    assert_eq!((pg.port, pg.pool_size), (5432, 4));
    assert_eq!(pg.connect_timeout, Duration::from_secs(5));
    assert_eq!(secrets.postgres_password.unwrap().as_bytes(), b"database password");
    assert_eq!(cfg.storage.retention_days, 30, "retention applies to both");
    assert_eq!(line(&cfg, "postgres.password").value, "<hidden>");

    let tuned = POSTGRES_BLOCK.replace(
        "}",
        r#"port = 6432 pool_size = 8 connect_timeout = "2s" }"#,
    );
    let (cfg, _) = ok(&format!("{}\n{tuned}", shared()));
    let StorageBackend::Postgres(pg) = &cfg.storage.backend else {
        panic!("expected the postgres backend");
    };
    assert_eq!((pg.port, pg.pool_size, pg.connect_timeout), (6432, 8, Duration::from_secs(2)));
}

#[test]
fn the_postgres_block_and_the_sqlite_file_exclude_each_other() {
    let raw = format!("{}\nstorage {{ database = \"/data/p.db\" }}\n{POSTGRES_BLOCK}", shared());
    let message = err(&raw);
    assert!(message.contains("storage.database"), "{message}");
}

#[test]
fn postgres_needs_the_shared_signing_key() {
    let raw = format!("{MINIMAL}\n{POSTGRES_BLOCK}");
    let message = err(&raw);
    assert!(message.contains("session.secret_mode"), "{message}");
}

#[test]
fn an_empty_postgres_block_is_a_postgres_block() {
    let message = err(&format!("{}\npostgres {{ }}", shared()));
    assert!(message.contains("postgres.host"), "{message}");
}

#[test]
fn rejects_bad_postgres_values() {
    for block in [
        r#"postgres { host = "db" database = "portal" user = "portal" }"#,
        r#"postgres { host = "db" database = "portal" password_env = "PORTAL_DB_PASSWORD" }"#,
        r#"postgres { database = "portal" user = "portal" password_env = "PORTAL_DB_PASSWORD" }"#,
        r#"postgres { host = "db host" database = "portal" user = "portal" password_env = "PORTAL_DB_PASSWORD" }"#,
        r#"postgres { host = "db" database = "po rtal" user = "portal" password_env = "PORTAL_DB_PASSWORD" }"#,
        r#"postgres { host = "db" database = "portal" user = "portal" password_file = "p" }"#,
        r#"postgres { host = "db" database = "portal" user = "portal" password_env = "PORTAL_DB_PASSWORD" port = 0 }"#,
        r#"postgres { host = "db" database = "portal" user = "portal" password_env = "PORTAL_DB_PASSWORD" port = 70000 }"#,
        r#"postgres { host = "db" database = "portal" user = "portal" password_env = "PORTAL_DB_PASSWORD" port = "5432" }"#,
        r#"postgres { host = "db" database = "portal" user = "portal" password_env = "PORTAL_DB_PASSWORD" pool_size = 0 }"#,
        r#"postgres { host = "db" database = "portal" user = "portal" password_env = "PORTAL_DB_PASSWORD" connect_timeout = "0s" }"#,
        r#"postgres { host = "db" database = "portal" user = "portal" password_env = "PORTAL_DB_PASSWORD" sslmode = "require" }"#,
    ] {
        err(&format!("{}\n{block}", shared()));
    }
}

#[test]
fn the_postgres_password_must_be_valid_utf8() {
    let file = temp_file(&[0xff, 0xfe, b'x']);
    let block = POSTGRES_BLOCK.replace(
        "password_env = \"PORTAL_DB_PASSWORD\"",
        &format!("password_file = \"{}\"", file.display()),
    );
    let message = err(&format!("{}\n{block}", shared()));
    assert!(message.contains("UTF-8"), "{message}");
    fs::remove_file(file).ok();
}

#[test]
fn the_configuration_reference_in_the_readme_is_the_generated_one() {
    let readme = fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/README.md"))
        .expect("README.md must be next to Cargo.toml");
    let begin = "<!-- config-reference:begin -->\n";
    let end = "<!-- config-reference:end -->";
    let start = readme.find(begin).expect("README.md has no config-reference:begin marker") + begin.len();
    let stop = readme.find(end).expect("README.md has no config-reference:end marker");
    assert_eq!(
        &readme[start..stop],
        format!("{}\n", keys::reference().trim_end_matches('\n')),
        "the reference in README.md is out of date: run `portal config-reference` and paste the result"
    );
}

// PTL-AUZ: no roles, grants, identity token and PDP.

#[test]
fn the_old_access_block_does_not_exist() {
    let message = err(&with(r#"roles { names = "ops" }"#));
    assert!(message.contains("unknown configuration block: roles"), "{message}");
}

#[test]
fn user_blocks_give_grants_in_the_static_and_hybrid_modes() {
    for mode in ["static", "hybrid"] {
        let (cfg, _) = ok(&with(&format!(
            r#"access {{ grants_mode = "{mode}" }}
            user "alice" {{ grants = ["query.run.read on service:db", "access on service:db"] }}
            user "bob" {{ grants = [] }}"#
        )));
        assert_eq!(cfg.access.grants_mode, Mode::parse(mode).unwrap());
        let alice = &cfg.access.configured_grants["alice"];
        assert_eq!(
            alice.iter().map(ToString::to_string).collect::<Vec<_>>(),
            ["access on service:db", "query.run.read on service:db"],
            "sorted"
        );
        assert!(cfg.access.configured_grants["bob"].is_empty());
        assert_eq!(
            line(&cfg, "user.alice.grants").value,
            r#"["query.run.read on service:db", "access on service:db"]"#
        );
        assert_eq!(line(&cfg, "user.alice.grants").source, "file");
    }
}

#[test]
fn a_user_block_is_refused_in_the_dynamic_mode_and_a_bad_one_always() {
    let message = err(&with(r#"user "alice" { grants = [] }"#));
    assert!(message.contains("dynamic"), "{message}");
    let hybrid = |block: &str| err(&with(&format!(r#"access {{ grants_mode = "hybrid" }} {block}"#)));
    for (block, expected) in [
        (r#"user "alice" { grants = ["admin"] }"#, "user.alice.grants"),
        (r#"user "alice" { grants = ["access on service:db", "access on service:db"] }"#, "two times"),
        (r#"user "alice" { }"#, "missing required configuration key: user.alice.grants"),
        (r#"user "alice" { grants = [] roles = [] }"#, "unknown configuration key: user.alice.roles"),
        (r#"user "Alice" { grants = [] }"#, "expected a login"),
        (r#"user "admin" { grants = [] }"#, "superadmin"),
        (r#"user "a" { grants = [] } user "a" { grants = [] }"#, "duplicate"),
        (r#"user { grants = [] }"#, "needs a label"),
        (r#"user "alice" { grants = "access on service:db" }"#, "list"),
    ] {
        let message = hybrid(block);
        assert!(message.contains(expected), "{block}: {message}");
    }
}

#[test]
fn the_identity_block_reads_its_key_and_gives_its_defaults() {
    let (cfg, secrets) = ok(&with(
        r#"identity { issuer = "https://portal.test" signing_key_env = "IDENTITY_KEY" }"#,
    ));
    let identity = cfg.identity.unwrap();
    assert_eq!(identity.header, "x-identity-token");
    assert_eq!(identity.issuer, "https://portal.test");
    assert_eq!(identity.token_ttl, Duration::from_secs(300));
    assert!(secrets.identity_key.is_some());
    for (block, expected) in [
        (r#"identity { signing_key_env = "IDENTITY_KEY" }"#, "identity.issuer"),
        (r#"identity { issuer = "i" }"#, "identity.signing_key"),
        (r#"identity { issuer = "i" signing_key = "x" signing_key_env = "IDENTITY_KEY" }"#, "must not be written"),
        (r#"identity { issuer = "i" signing_key_env = "IDENTITY_KEY" token_ttl = "2h" }"#, "identity.token_ttl"),
        (r#"identity { issuer = "i" signing_key_env = "IDENTITY_KEY" header = "Cookie" }"#, "identity.header"),
        (r#"identity { issuer = "a b" signing_key_env = "IDENTITY_KEY" }"#, "identity.issuer"),
    ] {
        let message = err(&with(block));
        assert!(message.contains(expected), "{block}: {message}");
    }
}

#[test]
fn the_pdp_block_and_its_clients() {
    let (cfg, secrets) = ok(&with(
        r#"pdp { listen_address = "0.0.0.0:4182" url = "http://portal-pdp.edge:4182/" }
        pdp_client "db" { token_env = "PDP_DB" }
        pdp_client "chat" { token_env = "PDP_CHAT" }"#,
    ));
    let pdp = cfg.pdp.clone().unwrap();
    assert_eq!(pdp.listen_address.port(), 4182);
    assert_eq!(pdp.url, "http://portal-pdp.edge:4182", "without the last /");
    assert_eq!((pdp.max_evaluations, pdp.max_body_bytes), (64, 64 * 1024));
    let names: Vec<&str> = secrets.pdp_tokens.iter().map(|(n, _)| n.as_str()).collect();
    assert_eq!(names, ["db", "chat"]);
    assert_eq!(line(&cfg, "pdp_client.db.token").value, "<hidden>");
    assert_eq!(line(&cfg, "pdp_client.db.token").source, "env:PDP_DB");
    assert!(!cfg.render_effective().contains("a token of the server"));
}

#[test]
fn a_bad_pdp_configuration_stops_the_start() {
    let pdp = r#"pdp { listen_address = "0.0.0.0:4182" url = "http://pdp:4182" }"#;
    for (extra, expected) in [
        (r#"pdp_client "db" { token_env = "PDP_DB" }"#.to_string(), "needs the pdp block"),
        (
            format!(r#"{pdp} pdp_client "db" {{ token_env = "PDP_DB" }} pdp_client "chat" {{ token_env = "PDP_DB" }}"#),
            "the same token",
        ),
        (format!(r#"{pdp} pdp_client "db" {{ token_env = "SHORT" }}"#), "the minimum is 32"),
        (format!(r#"{pdp} pdp_client "db" {{ token = "x" }}"#), "must not be written"),
        (format!(r#"{pdp} pdp_client "Db" {{ token_env = "PDP_DB" }}"#), "service id"),
        (
            r#"pdp { listen_address = "0.0.0.0:4181" url = "http://pdp:4181" }"#.to_string(),
            "must not use the port of server.listen_address",
        ),
        (r#"pdp { listen_address = "0.0.0.0:4182" url = "http://pdp/x" }"#.to_string(), "pdp.url"),
        (r#"pdp { url = "http://pdp" }"#.to_string(), "pdp.listen_address"),
    ] {
        let message = err(&with(&extra));
        assert!(message.contains(expected), "{extra}: {message}");
    }
}
