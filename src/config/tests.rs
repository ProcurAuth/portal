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
    server { listen_address = "0.0.0.0:4181" trusted_proxies = [] }
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
    assert_eq!(
        secrets.superadmin_password.as_bytes(),
        b"a password for the tests"
    );
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
    assert_eq!(
        line(&cfg, "authentication.superadmin_login").source,
        "env:PORTAL_LOGIN"
    );
    assert_eq!(
        line(&cfg, "authentication.superadmin_login").value,
        "<hidden>"
    );
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
    assert!(cfg
        .effective
        .iter()
        .all(|l| !l.key.starts_with("postgres.")));
    assert!(cfg.effective.iter().all(|l| l.key != "session.secret"));
    assert!(cfg
        .effective
        .iter()
        .all(|l| l.key != "discovery.kubernetes_host_env"));
    assert!(cfg
        .effective
        .iter()
        .all(|l| l.key != "discovery.docker_socket"));
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
        r#"server {{ listen_address = "0.0.0.0:4181" trusted_proxies = [] }}
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
    assert_eq!(
        secrets.superadmin_password.as_bytes(),
        b"secret\n",
        "only the last line end goes"
    );
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
            r#"server {{ listen_address = "0.0.0.0:4181" trusted_proxies = [] }}
            authentication {{ superadmin_login_{form} superadmin_password_env = "PORTAL_PASSWORD" }}"#
        ));
        assert_eq!(secrets.superadmin_login, "0123456789abcdef0123456789abcdef");
        let (_, secrets) = ok(&format!(
            r#"server {{ listen_address = "0.0.0.0:4181" trusted_proxies = [] }}
            authentication {{ superadmin_login_env = "PORTAL_LOGIN" superadmin_password_{form} }}"#
        ));
        assert_eq!(secrets.superadmin_password.len(), 32);
        let (_, secrets) = ok(&with(&format!(
            r#"session {{ secret_mode = "provided" secret_{form} }}"#
        )));
        assert_eq!(secrets.session_secret.unwrap().len(), 32);
        let block = POSTGRES_BLOCK.replace(
            r#"password_env = "PORTAL_DB_PASSWORD""#,
            &format!("password_{form}"),
        );
        let (_, secrets) = ok(&format!("{}\n{block}", shared()));
        assert_eq!(secrets.postgres_password.unwrap().len(), 32);
    }
    fs::remove_file(file).ok();
}

#[test]
fn a_secret_in_clear_text_in_the_file_is_refused() {
    for (from, to) in [
        (
            r#"superadmin_password_env = "PORTAL_PASSWORD""#,
            r#"superadmin_password = "hunter2hunter2""#,
        ),
        (
            r#"superadmin_login_env    = "PORTAL_LOGIN""#,
            r#"superadmin_login = "admin""#,
        ),
    ] {
        let message = err(&MINIMAL.replace(from, to));
        assert!(
            message.contains("must not be written in the file"),
            "{message}"
        );
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
        POSTGRES_BLOCK.replace(
            "password_env = \"PORTAL_DB_PASSWORD\"",
            "password = \"clear\""
        )
    ));
    assert!(message.contains("postgres.password"), "{message}");
}

#[test]
fn a_secret_needs_exactly_one_form() {
    let none = MINIMAL.replace(r#"superadmin_password_env = "PORTAL_PASSWORD""#, "");
    let message = err(&none);
    assert!(
        message.contains("authentication.superadmin_password_env"),
        "{message}"
    );
    assert!(
        message.contains("authentication.superadmin_password_file"),
        "{message}"
    );

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
    let message = parse_with(MINIMAL, &[("PORTAL_LOGIN", "admin")])
        .err()
        .unwrap();
    assert!(message.contains("PORTAL_PASSWORD"), "{message}");
    assert!(message.contains("superadmin_password_env"), "{message}");
    // The variable is empty.
    let message = parse_with(
        MINIMAL,
        &[("PORTAL_LOGIN", "admin"), ("PORTAL_PASSWORD", "")],
    )
    .err()
    .unwrap();
    assert!(
        message.contains("PORTAL_PASSWORD") && message.contains("empty"),
        "{message}"
    );
    // The file does not exist.
    let raw = MINIMAL.replace(
        r#"superadmin_password_env = "PORTAL_PASSWORD""#,
        r#"superadmin_password_file = "/nonexistent/portal-password""#,
    );
    let message = err(&raw);
    assert!(
        message.contains("/nonexistent/portal-password"),
        "{message}"
    );
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
    assert!(
        message.contains("session.secret_mode is ephemeral"),
        "{message}"
    );
    // The old mode name is gone.
    assert!(err(&with(
        r#"session { secret_mode = "file" secret_env = "PORTAL_SESSION" }"#
    ))
    .contains("session.secret_mode"));
    // At least 32 bytes.
    let raw = with(r#"session { secret_mode = "provided" secret_env = "SHORT" }"#);
    let message = parse_with(
        &raw,
        &[
            ("PORTAL_LOGIN", "a"),
            ("PORTAL_PASSWORD", "p"),
            ("SHORT", "abc"),
        ],
    )
    .err()
    .unwrap();
    assert!(
        message.contains("minimum is 32") && message.contains("env:SHORT"),
        "{message}"
    );
    assert!(!message.contains("abc"), "{message}");
}

// The colon is the separator in the id of a key: no login can have it.

#[test]
fn a_superadmin_login_with_a_colon_is_refused() {
    for bad in ["access:x", "admin:", "xx:yy", "a:b"] {
        let message = parse_with(MINIMAL, &[("PORTAL_LOGIN", bad), ("PORTAL_PASSWORD", "p")])
            .err()
            .unwrap_or_else(|| panic!("{bad:?} must be refused"));
        assert!(
            message.contains("authentication.superadmin_login"),
            "{message}"
        );
        assert!(message.contains("ids of keys"), "{message}");
        assert!(
            !message.contains(bad),
            "the login stays out of the error: {message}"
        );
    }
}

// PTL-CFG-3: the login is refused, not changed.

#[test]
fn a_superadmin_login_with_an_upper_case_letter_or_a_space_is_refused() {
    for bad in [
        "Admin",
        "ADMIN",
        "ad min",
        " admin",
        "admin ",
        "ad\tmin",
        "adm\u{e9}N",
        "ad\u{0}min",
    ] {
        let message = parse_with(MINIMAL, &[("PORTAL_LOGIN", bad), ("PORTAL_PASSWORD", "p")])
            .err()
            .unwrap_or_else(|| panic!("{bad:?} must be refused"));
        assert!(
            message.contains("authentication.superadmin_login"),
            "{message}"
        );
        assert!(
            !message.contains(bad),
            "the login stays out of the error: {message}"
        );
    }
    for good in ["admin", "root-1", "a.b_c@example.org", "\u{e9}l\u{e8}ve"] {
        let (_, secrets) =
            parse_with(MINIMAL, &[("PORTAL_LOGIN", good), ("PORTAL_PASSWORD", "p")]).unwrap();
        assert_eq!(secrets.superadmin_login, good);
    }
    let long = "a".repeat(65);
    assert!(parse_with(
        MINIMAL,
        &[("PORTAL_LOGIN", &long), ("PORTAL_PASSWORD", "p")]
    )
    .is_err());
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
        Some(KubernetesApi {
            host: "10.43.0.1".into(),
            port: 443
        })
    );
    assert_eq!(
        line(&cfg, "discovery.kubernetes_host_env").value,
        "\"API_HOST\""
    );
    assert_eq!(
        line(&cfg, "discovery.kubernetes_host").source,
        "env:API_HOST"
    );
    assert_eq!(line(&cfg, "discovery.kubernetes_port").value, "443");

    // An IPv6 address gets its brackets for the URL.
    let (cfg, _) = parse_with(
        &raw,
        &[
            ("PORTAL_LOGIN", "a"),
            ("PORTAL_PASSWORD", "p"),
            ("API_HOST", "fd00::1"),
            ("API_PORT", "6443"),
        ],
    )
    .unwrap();
    assert_eq!(cfg.discovery.kubernetes.unwrap().host, "[fd00::1]");
}

#[test]
fn the_kubernetes_variables_are_mandatory_and_checked_at_start() {
    let names = r#"kubernetes_host_env = "API_HOST" kubernetes_port_env = "API_PORT""#;
    // The names are missing.
    let message = err(&with("discovery { kubernetes = true }"));
    assert!(
        message.contains("discovery.kubernetes_host_env"),
        "{message}"
    );
    // The names are present but the switch is off.
    let message = err(&with(&format!("discovery {{ {names} }}")));
    assert!(
        message.contains("discovery.kubernetes_host_env must not be set"),
        "{message}"
    );
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
        (
            "security.login_rate_limit_enabled",
            r#"security { login_rate_limit_enabled = "true" }"#,
        ),
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
    ok(&with(
        "users { max_users = 5 } security { login_rate_limit_enabled = false }",
    ));
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
        (
            r#"header_read_timeout = "2m""#,
            "server.header_read_timeout",
            "\"2m\"",
        ),
        (
            r#"header_read_timeout = "ten""#,
            "server.header_read_timeout",
            "\"ten\"",
        ),
        (
            r#"max_body_bytes = "8GiB""#,
            "server.max_body_bytes",
            "\"8GiB\"",
        ),
        (
            r#"listen_address = "nowhere""#,
            "server.listen_address",
            "\"nowhere\"",
        ),
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
        (
            r#"session { same_site = "None" }"#,
            "session.same_site",
            "\"None\"",
        ),
        (
            r#"session { cookie_name = "portal" }"#,
            "session.cookie_name",
            "\"portal\"",
        ),
        (
            r#"metrics { path = "metrics" }"#,
            "metrics.path",
            "\"metrics\"",
        ),
        (
            r#"discovery { docker_socket = "docker.sock" }"#,
            "discovery.docker_socket",
            "\"docker.sock\"",
        ),
        (
            r#"access { grants_mode = "Static" }"#,
            "access.grants_mode",
            "\"Static\"",
        ),
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
    assert!(
        message.contains("2h") && message.contains("1h"),
        "{message}"
    );
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
        "login_attempts_per_ip",
        "login_window",
        "login_lockout",
        "global_login_attempts",
        "global_login_window",
        "global_login_lockout",
        "max_rate_limit_entries",
    ] {
        assert!(
            cfg.effective
                .iter()
                .all(|l| l.key != format!("security.{key}")),
            "{key}"
        );
    }
    // One limiter on keeps the shared table size.
    ok(&with(
        "security { login_rate_limit_enabled = false max_rate_limit_entries = 100 }",
    ));
}

#[test]
fn metrics_path_is_refused_when_the_metrics_are_off() {
    let message = err(&with(r#"metrics { enabled = false path = "/metrics" }"#));
    assert!(
        message.contains("metrics.path must not be set when metrics.enabled is false"),
        "{message}"
    );
    let (cfg, _) = ok(&with("metrics { enabled = false }"));
    assert!(!cfg.metrics.enabled);
    assert!(cfg.effective.iter().all(|l| l.key != "metrics.path"));
    ok(&with(r#"metrics { enabled = true path = "/prom" }"#));
}

// The closed schema.

#[test]
fn rejects_an_unknown_key_block_or_duplicate() {
    assert!(err(&with(r#"logging { magic = "yes" }"#))
        .contains("unknown configuration key: logging.magic"));
    assert!(err(&with("magic { }")).contains("magic"));
    assert!(err(&with(r#"logging { level = "info" level = "warn" }"#)).contains("duplicate"));
    assert!(err(&with(
        r#"logging { level = "info" } logging { level = "warn" }"#
    ))
    .contains("duplicate"));
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
    assert_eq!(
        cfg.discovery.docker_socket,
        Some(PathBuf::from("/var/run/docker.sock"))
    );
    assert_eq!(cfg.discovery.probe_timeout, Duration::from_millis(200));
    for body in [
        r#"interval = "1s""#,
        r#"docker_socket = "docker.sock""#,
        r#"kubernetes = "maybe""#,
        r#"docker_socket = "/a/../b""#,
    ] {
        err(&with(&format!("discovery {{ {body} }}")));
    }
}

#[test]
fn parses_idle_timeout_and_binding_and_refuses_bad_ones() {
    let (cfg, _) = ok(&with(
        r#"session { idle_timeout = "10m" bind = "user_agent" }"#,
    ));
    assert_eq!(cfg.session.idle_timeout, Duration::from_secs(600));
    assert_eq!(cfg.session.bind, SessionBind::UserAgent);
    for extra in [
        r#"idle_timeout = "10s""#,
        r#"idle_timeout = "9h""#,
        r#"bind = "ip""#,
    ] {
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
    let raw = format!(
        "{}\nstorage {{ retention_days = 30 }}\n{POSTGRES_BLOCK}",
        shared()
    );
    let (cfg, secrets) = ok(&raw);
    let StorageBackend::Postgres(pg) = &cfg.storage.backend else {
        panic!("expected the postgres backend");
    };
    assert_eq!(pg.host, "postgresql-rw.data.svc.cluster.local");
    assert_eq!((pg.port, pg.pool_size), (5432, 4));
    assert_eq!(pg.connect_timeout, Duration::from_secs(5));
    assert_eq!(
        secrets.postgres_password.unwrap().as_bytes(),
        b"database password"
    );
    assert_eq!(cfg.storage.retention_days, 30, "retention applies to both");
    assert_eq!(line(&cfg, "postgres.password").value, "<hidden>");

    let tuned =
        POSTGRES_BLOCK.replace("}", r#"port = 6432 pool_size = 8 connect_timeout = "2s" }"#);
    let (cfg, _) = ok(&format!("{}\n{tuned}", shared()));
    let StorageBackend::Postgres(pg) = &cfg.storage.backend else {
        panic!("expected the postgres backend");
    };
    assert_eq!(
        (pg.port, pg.pool_size, pg.connect_timeout),
        (6432, 8, Duration::from_secs(2))
    );
}

#[test]
fn the_postgres_block_and_the_sqlite_file_exclude_each_other() {
    let raw = format!(
        "{}\nstorage {{ database = \"/data/p.db\" }}\n{POSTGRES_BLOCK}",
        shared()
    );
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
    let start = readme
        .find(begin)
        .expect("README.md has no config-reference:begin marker")
        + begin.len();
    let stop = readme
        .find(end)
        .expect("README.md has no config-reference:end marker");
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
    assert!(
        message.contains("unknown configuration block: roles"),
        "{message}"
    );
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
fn a_user_block_gives_grants_on_sub_resources() {
    let (cfg, _) = ok(&with(
        r#"access { grants_mode = "hybrid" }
        user "alice" { grants = [
          "query.run.write on service:clickhouse-dash/database:analytics",
          "query.run.read on service:clickhouse-dash",
        ] }"#,
    ));
    let alice = &cfg.access.configured_grants["alice"];
    assert_eq!(
        alice.iter().map(ToString::to_string).collect::<Vec<_>>(),
        [
            "query.run.read on service:clickhouse-dash",
            "query.run.write on service:clickhouse-dash/database:analytics"
        ]
    );
    assert_eq!(alice[1].path[0].id, "analytics");
}

#[test]
fn a_user_block_gives_grants_on_several_levels_with_encoded_ids() {
    let (cfg, _) = ok(&with(
        r#"access { grants_mode = "hybrid" }
        user "alice" { grants = [
          "query.run.read on service:clickhouse-dash/host:local",
          "query.run.write on service:clickhouse-dash/host:local/database:a%2Fb",
        ] }"#,
    ));
    let alice = &cfg.access.configured_grants["alice"];
    assert_eq!(alice[0].path.len(), 1);
    assert_eq!(alice[1].path[1].id, "a/b", "the id is decoded");
    assert_eq!(
        alice[1].to_string(),
        "query.run.write on service:clickhouse-dash/host:local/database:a%2Fb"
    );
    for (grant, expected) in [
        (
            "query.run.write on service:db/host:local/database:a%2fb",
            "upper-case hex",
        ),
        ("query.run.write on service:db/host:%6Cocal", "as it is"),
        ("query.run.write on service:db/host:a/b", "segment"),
        (
            "query.run.write on service:db/a:1/b:2/c:3/d:4/e:5",
            "the maximum is 4",
        ),
    ] {
        let message = err(&with(&format!(
            r#"access {{ grants_mode = "hybrid" }} user "alice" {{ grants = ["{grant}"] }}"#
        )));
        assert!(message.contains("user.alice.grants"), "{message}");
        assert!(message.contains(expected), "{grant}: {message}");
    }
}

#[test]
fn a_user_block_is_refused_in_the_dynamic_mode_and_a_bad_one_always() {
    let message = err(&with(r#"user "alice" { grants = [] }"#));
    assert!(message.contains("dynamic"), "{message}");
    let hybrid = |block: &str| {
        err(&with(&format!(
            r#"access {{ grants_mode = "hybrid" }} {block}"#
        )))
    };
    for (block, expected) in [
        (
            r#"user "alice" { grants = ["admin"] }"#,
            "user.alice.grants",
        ),
        (
            r#"user "alice" { grants = ["access on service:db", "access on service:db"] }"#,
            "two times",
        ),
        (
            r#"user "alice" { grants = ["access on service:db/database:a"] }"#,
            "no grant on a sub-resource",
        ),
        (
            r#"user "alice" { grants = ["query on service:db/database:a b"] }"#,
            "user.alice.grants",
        ),
        (
            r#"user "alice" { grants = ["query on service:db/Database:a"] }"#,
            "resource type",
        ),
        (
            r#"user "alice" { }"#,
            "missing required configuration key: user.alice.grants",
        ),
        (
            r#"user "alice" { grants = [] roles = [] }"#,
            "unknown configuration key: user.alice.roles",
        ),
        (r#"user "Alice" { grants = [] }"#, "expected a login"),
        (r#"user "admin" { grants = [] }"#, "superadmin"),
        (
            r#"user "a" { grants = [] } user "a" { grants = [] }"#,
            "duplicate",
        ),
        (r#"user { grants = [] }"#, "needs a label"),
        (
            r#"user "alice" { grants = "access on service:db" }"#,
            "list",
        ),
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
        (
            r#"identity { signing_key_env = "IDENTITY_KEY" }"#,
            "identity.issuer",
        ),
        (r#"identity { issuer = "i" }"#, "identity.signing_key"),
        (
            r#"identity { issuer = "i" signing_key = "x" signing_key_env = "IDENTITY_KEY" }"#,
            "must not be written",
        ),
        (
            r#"identity { issuer = "i" signing_key_env = "IDENTITY_KEY" token_ttl = "2h" }"#,
            "identity.token_ttl",
        ),
        (
            r#"identity { issuer = "i" signing_key_env = "IDENTITY_KEY" header = "Cookie" }"#,
            "identity.header",
        ),
        (
            r#"identity { issuer = "a b" signing_key_env = "IDENTITY_KEY" }"#,
            "identity.issuer",
        ),
    ] {
        let message = err(&with(block));
        assert!(message.contains(expected), "{block}: {message}");
    }
}

#[test]
fn the_pdp_block_and_its_clients() {
    let (cfg, secrets) = ok(&with(
        r#"pdp { listen_address = "0.0.0.0:4182" url = "http://portal-pdp.edge:4182/" plain_http = true }
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
    let pdp =
        r#"pdp { listen_address = "0.0.0.0:4182" url = "http://pdp:4182" plain_http = true }"#;
    for (extra, expected) in [
        (
            r#"pdp_client "db" { token_env = "PDP_DB" }"#.to_string(),
            "needs the pdp block",
        ),
        (
            format!(
                r#"{pdp} pdp_client "db" {{ token_env = "PDP_DB" }} pdp_client "chat" {{ token_env = "PDP_DB" }}"#
            ),
            "the same token",
        ),
        (
            format!(r#"{pdp} pdp_client "db" {{ token_env = "SHORT" }}"#),
            "the minimum is 32",
        ),
        (
            format!(r#"{pdp} pdp_client "db" {{ token = "x" }}"#),
            "must not be written",
        ),
        (
            format!(r#"{pdp} pdp_client "Db" {{ token_env = "PDP_DB" }}"#),
            "service id",
        ),
        (
            r#"pdp { listen_address = "0.0.0.0:4181" url = "http://pdp:4181" plain_http = true }"#
                .to_string(),
            "must not use the port of server.listen_address",
        ),
        (
            r#"pdp { listen_address = "0.0.0.0:4182" url = "http://pdp/x" plain_http = true }"#
                .to_string(),
            "pdp.url",
        ),
        (
            r#"pdp { url = "http://pdp" plain_http = true }"#.to_string(),
            "pdp.listen_address",
        ),
    ] {
        let message = err(&with(&extra));
        assert!(message.contains(expected), "{extra}: {message}");
    }
}

// Security regressions -------------------------------------------------------------------------

/// The superadmin password and the session secret of the published examples (`.env.example`,
/// the quick start of the README). Anybody who reads the repository knows them.
#[test]
fn the_published_example_secrets_stop_the_start_and_are_never_printed() {
    for password in ["change-me-use-a-long-random-secret", "test1234"] {
        let message = parse_with(
            MINIMAL,
            &[("PORTAL_LOGIN", "admin"), ("PORTAL_PASSWORD", password)],
        )
        .err()
        .unwrap_or_else(|| panic!("the example password {password:?} must be refused"));
        assert!(
            message.contains("authentication.superadmin_password"),
            "{message}"
        );
        assert!(message.contains("env:PORTAL_PASSWORD"), "{message}");
        assert!(
            !message.contains(password),
            "the value is hidden: {message}"
        );
    }
    let message = parse_with(
        &with(r#"session { secret_mode = "provided" secret_env = "PORTAL_SESSION" }"#),
        &[
            ("PORTAL_LOGIN", "admin"),
            ("PORTAL_PASSWORD", "a password for the tests"),
            ("PORTAL_SESSION", "change-me-with-a-long-random-value"),
        ],
    )
    .err()
    .expect("the example session secret must be refused");
    assert!(message.contains("session.secret"), "{message}");
    assert!(
        !message.contains("change-me"),
        "the value is hidden: {message}"
    );
}

#[test]
fn the_trusted_proxies_are_mandatory_and_written_exactly() {
    let message = err(&MINIMAL.replace(" trusted_proxies = []", ""));
    assert!(message.contains("server.trusted_proxies"), "{message}");
    let (cfg, _) = ok(&MINIMAL.replace(
        "trusted_proxies = []",
        r#"trusted_proxies = ["10.42.0.0/16", "2001:db8::/32"]"#,
    ));
    assert_eq!(cfg.server.trusted_proxies.len(), 2);
    assert_eq!(
        line(&cfg, "server.trusted_proxies").value,
        r#"["10.42.0.0/16", "2001:db8::/32"]"#
    );
    for bad in ["10.42.0.1/16", "10.42.0.0", "proxy"] {
        let message = err(&MINIMAL.replace(
            "trusted_proxies = []",
            &format!(r#"trusted_proxies = ["{bad}"]"#),
        ));
        assert!(message.contains("server.trusted_proxies"), "{message}");
        assert!(message.contains(bad), "{message}");
    }
}

#[test]
fn the_sign_in_bodies_must_leave_connections_for_forward_auth() {
    let (cfg, _) = ok(&with(""));
    assert_eq!(cfg.server.max_login_body_reads, 64);
    let message = err(&with_server("max_connections = 64", ""));
    assert!(message.contains("server.max_login_body_reads"), "{message}");
    assert!(message.contains("server.max_connections"), "{message}");
    let (cfg, _) = ok(&with_server(
        "max_connections = 8 max_login_body_reads = 2",
        "",
    ));
    assert_eq!(cfg.server.max_login_body_reads, 2);
}

// Keys: the block `keys`, the accounts of the file and their keys ---------------------------------

/// The blocks that the block `keys` needs.
const KEYS_NEEDS: &str = r#"
    identity { issuer = "https://portal.test" signing_key_env = "IDENTITY_KEY" }
    pdp { listen_address = "0.0.0.0:4182" url = "https://portal-pdp.test:4182" }
    keys {}
"#;

fn with_keys(extra: &str) -> String {
    with(&format!(
        "access {{ grants_mode = \"hybrid\" }}\n{KEYS_NEEDS}\n{extra}"
    ))
}

/// The variables of the tests and the text of one new key.
fn vars_with_key() -> (Vec<(String, String)>, String) {
    let text = crate::keys::generate().unwrap().to_string();
    let mut vars: Vec<(String, String)> = VARS
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
    vars.push(("CAROL_KEY".into(), text.clone()));
    (vars, text)
}

fn parse_keys(raw: &str, vars: &[(String, String)]) -> Result<(Config, Secrets), String> {
    let vars = vars.to_vec();
    Config::parse(raw, &move |name| {
        vars.iter().find(|(k, _)| k == name).map(|(_, v)| v.clone())
    })
}

#[test]
fn the_keys_block_gives_its_defaults_and_needs_the_identity_and_pdp_blocks() {
    let (cfg, _) = ok(&with_keys(""));
    let keys = cfg.keys.clone().unwrap();
    assert_eq!(keys.default_lifetime, Duration::from_secs(30 * 86_400));
    assert_eq!(keys.directory_max_age, Duration::from_secs(30));
    assert_eq!(keys.max_per_user, 100);
    assert_eq!(keys.max_counter_rows, 2_000_000);
    assert_eq!(keys.max_directory_grants, 4_000_000);
    assert_eq!(keys.history_days, 90);
    assert_eq!(cfg.users.max_grants, 512);
    assert_eq!(line(&cfg, "keys.max_per_user").source, "default");
    assert!(ok(MINIMAL).0.keys.is_none(), "no block, no key");
    for (extra, expected) in [
        (r#"keys {}"#, "needs the block identity"),
        (
            r#"identity { issuer = "i" signing_key_env = "IDENTITY_KEY" } keys {}"#,
            "needs the block pdp",
        ),
    ] {
        let message = err(&with(extra));
        assert!(message.contains(expected), "{extra}: {message}");
    }
    for (attribute, value) in [
        ("default_lifetime", r#""12h""#),
        ("directory_max_age", r#""14s""#),
        ("directory_max_age", r#""301s""#),
        ("max_per_user", "0"),
        ("max_per_user", "10001"),
        ("max_counter_rows", "0"),
        ("max_directory_grants", "100000001"),
        ("history_days", "0"),
        ("history_days", "3651"),
    ] {
        let raw = with(&format!(
            "{}\n{}",
            KEYS_NEEDS.replace("keys {}", ""),
            format_args!("keys {{ {attribute} = {value} }}")
        ));
        let message = err(&raw);
        assert!(message.contains(&format!("keys.{attribute}")), "{message}");
        assert!(
            message.contains(value.trim_matches('"')),
            "the value: {message}"
        );
    }
    assert!(parse(&with(&format!(
        "{}\nkeys {{ unknown = 1 }}",
        KEYS_NEEDS.replace("keys {}", "")
    )))
    .is_err());
}

#[test]
fn a_http_url_of_the_pdp_needs_plain_http_and_plain_http_needs_a_http_url() {
    let base = |pdp: &str| {
        with(&format!(
            "identity {{ issuer = \"i\" signing_key_env = \"IDENTITY_KEY\" }}\n{pdp}"
        ))
    };
    let message = err(&base(
        r#"pdp { listen_address = "0.0.0.0:4182" url = "http://pdp:4182" }"#,
    ));
    assert!(message.contains("pdp.plain_http"), "{message}");
    let message = err(&base(
        r#"pdp { listen_address = "0.0.0.0:4182" url = "https://pdp:4182" plain_http = true }"#,
    ));
    assert!(message.contains("contradict"), "{message}");
    let (cfg, _) = ok(&base(
        r#"pdp { listen_address = "0.0.0.0:4182" url = "http://pdp:4182" plain_http = true counter_connections = 8 }"#,
    ));
    let pdp = cfg.pdp.unwrap();
    assert!(pdp.plain_http);
    assert_eq!(pdp.counter_connections, 8);
    let (cfg, _) = ok(&base(
        r#"pdp { listen_address = "0.0.0.0:4182" url = "https://pdp:4182" }"#,
    ));
    assert_eq!(cfg.pdp.unwrap().counter_connections, 2);
}

#[test]
fn the_age_of_a_document_is_longer_than_the_time_between_two_reads() {
    let (cfg, _) = ok(MINIMAL);
    assert_eq!(cfg.access.actions_max_age, Duration::from_secs(3600));
    assert_eq!(cfg.access.start_read_deadline, Duration::from_secs(5));
    assert_eq!(
        (
            cfg.access.bounds.max_actions,
            cfg.access.bounds.max_resources,
            cfg.access.bounds.max_document_bytes
        ),
        (1024, 4096, 262_144)
    );
    let message = err(&with(
        r#"access { actions_refresh = "10m" actions_max_age = "10m" }"#,
    ));
    assert!(message.contains("access.actions_max_age"), "{message}");
    assert!(parse(&with(
        r#"access { actions_refresh = "10m" actions_max_age = "11m" }"#
    ))
    .is_ok());
    for bad in [
        r#"access { actions_max_age = "30s" }"#,
        r#"access { actions_max_age = "8d" }"#,
        r#"access { start_read_deadline = "0s" }"#,
        r#"access { start_read_deadline = "301s" }"#,
        r#"access { max_actions = 63 }"#,
        r#"access { max_resources = 255 }"#,
        r#"access { max_document_bytes = 65535 }"#,
    ] {
        assert!(parse(&with(bad)).is_err(), "{bad}");
    }
}

#[test]
fn a_grant_of_a_user_block_can_carry_limit_values() {
    let (cfg, _) = ok(&with_keys(
        r#"user "carol" {
          grants = [
            { grant = "access on service:datastore", limits = { requests = [ { amount = 300, window = "minute" }, { amount = 10, window = "second" } ] } },
            { grant = "data.read on service:datastore/host:prod/database:test1", limits = { max_rows = 1000 } },
            "keys.own.read on service:portal",
            { grant = "keys.own.use on service:portal", limits = { max_key_lifetime = 2592000 } },
          ]
        }"#,
    ));
    let carol = &cfg.access.accounts["carol"];
    let by_action = |action: &str| carol.grants.iter().find(|g| g.action == action).unwrap();
    assert_eq!(
        by_action("access").limits.to_json(),
        r#"{"requests":[{"amount":10,"window":"second"},{"amount":300,"window":"minute"}]}"#
    );
    assert_eq!(
        by_action("data.read").limits.to_json(),
        r#"{"max_rows":1000}"#
    );
    assert_eq!(
        by_action("keys.own.use").limits.to_json(),
        r#"{"max_key_lifetime":2592000}"#
    );
    assert!(by_action("keys.own.read").limits.is_empty());
    assert_eq!(cfg.access.configured_grants["carol"], carol.grants);
    assert!(carol.password_hash.is_none());
    assert!(carol.keys.is_empty());
}

#[test]
fn a_user_block_with_a_bad_limit_a_bad_right_or_too_many_grants_stops_the_start() {
    let user = |grants: &str| {
        err(&with_keys(&format!(
            r#"user "carol" {{ grants = [ {grants} ] }}"#
        )))
    };
    for (grants, expected) in [
        (
            r#"{ grant = "data.read on service:db/host:x", limits = { requests = [ { amount = 1, window = "minute" } ] } }"#,
            "a budget value is only on the grant access",
        ),
        (
            r#"{ grant = "access on service:db", limits = { a = 1, b = 2, c = 3, d = 4, e = 5, f = 6, g = 7, h = 8, i = 9 } }"#,
            "at most 8",
        ),
        (
            r#"{ grant = "data.read on service:db", limits = { max_rows = 1000000001 } }"#,
            "1000000000",
        ),
        (
            r#"{ grant = "access on service:db", limits = { requests = [ { amount = 0, window = "minute" } ] } }"#,
            "from 1",
        ),
        (
            r#"{ grant = "access on service:db", limits = { requests = [ { amount = 1, window = "week" } ] } }"#,
            "week",
        ),
        (
            r#"{ grant = "access on service:db", limits = { requests = [ { amount = 1, window = "minute" }, { amount = 2, window = "minute" } ] } }"#,
            "two times",
        ),
        (
            r#"{ grant = "access on service:db", limits = { requests = [ { amount = 1, window = "minute", burst = 2 } ] } }"#,
            "burst",
        ),
        (
            r#"{ grant = "access on service:db", limits = 3 }"#,
            "limits must be an object",
        ),
        (r#"{ limits = {} }"#, "needs the member grant"),
        (
            r#"{ grant = "access on service:db", colour = "red" }"#,
            "colour",
        ),
        (
            r#""keys.own.create on service:portal", "keys.own.read on service:portal""#,
            "keys.own.create",
        ),
        (
            r#""keys.own.edit on service:portal""#,
            "needs the action keys.own.read",
        ),
        (
            r#""keys.all.read on service:portal""#,
            "needs the action users.read",
        ),
        (
            r#""keys.own.stop on service:portal", "keys.own.read on service:portal""#,
            "no action",
        ),
        (
            r#""access on service:db", "access on service:db""#,
            "two times",
        ),
    ] {
        let message = user(grants);
        assert!(message.contains(expected), "{grants}: {message}");
        assert!(
            message.contains("carol"),
            "the error names the login: {message}"
        );
    }
    // More grants than `users.max_grants`.
    let message = err(&with_keys(
        r#"users { max_grants = 2 }
           user "carol" { grants = ["access on service:a", "access on service:b", "access on service:c"] }"#,
    ));
    assert!(
        message.contains("carol") && message.contains("3 grants") && message.contains("is 2"),
        "{message}"
    );
}

#[test]
fn a_user_block_can_hold_the_digest_of_a_password() {
    let hash = crate::crypto::hash_password(b"a long password for carol").unwrap();
    let mut vars: Vec<(String, String)> = VARS
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
    vars.push(("CAROL_HASH".into(), hash.clone()));
    vars.push(("BAD_HASH".into(), "plaintext".into()));
    let raw =
        |attribute: &str| with_keys(&format!(r#"user "carol" {{ grants = [] {attribute} }}"#));
    let (cfg, _) = parse_keys(&raw(r#"password_hash_env = "CAROL_HASH""#), &vars).unwrap();
    assert_eq!(
        cfg.access.accounts["carol"].password_hash.as_deref(),
        Some(hash.as_str())
    );
    assert_eq!(line(&cfg, "user.carol.password_hash").value, "<hidden>");
    assert_eq!(
        line(&cfg, "user.carol.password_hash").source,
        "env:CAROL_HASH"
    );
    let message = parse_keys(&raw(r#"password_hash_env = "BAD_HASH""#), &vars)
        .err()
        .unwrap();
    assert!(
        message.contains("user.carol.password_hash") && message.contains("PHC"),
        "{message}"
    );
    assert!(
        !message.contains("plaintext"),
        "the value stays hidden: {message}"
    );
    let message = parse_keys(&raw(r#"password_hash = "x""#), &vars)
        .err()
        .unwrap();
    assert!(message.contains("must not be written"), "{message}");
}

#[test]
fn a_key_of_the_file_gives_its_digest_its_id_and_its_grants() {
    let (vars, text) = vars_with_key();
    let digest = crate::keys::digest(&text);
    let hex: String = digest.iter().map(|b| format!("{b:02X}")).collect();
    let mut vars = vars;
    vars.push(("CAROL_SUM".into(), hex));
    let raw = with_keys(
        r#"user "carol" {
          grants = ["access on service:datastore", "keys.own.use on service:portal"]
          key "nightly report" {
            key_env = "CAROL_KEY"
            grants = [
              { grant = "access on service:datastore", limits = { requests = [ { amount = 60, window = "minute" } ] } },
              "data.read on service:datastore/host:prod/database:test1",
            ]
          }
          key "backup check" {
            key_sha256_env = "CAROL_SUM"
            grants = ["access on service:datastore"]
          }
        }"#,
    );
    // Two keys cannot have the same digest: the second block takes the digest of the first.
    let message = parse_keys(&raw, &vars).err().unwrap();
    assert!(
        message.contains("backup check") && message.contains("digest"),
        "{message}"
    );
    assert!(
        !message.contains(&text),
        "the text of the key stays hidden: {message}"
    );
    let other = crate::keys::generate().unwrap().to_string();
    vars.push((
        "OTHER_SUM".into(),
        crate::keys::digest(&other)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect(),
    ));
    let raw = raw.replace("CAROL_SUM", "OTHER_SUM");
    let (cfg, _) = parse_keys(&raw, &vars).unwrap();
    let carol = &cfg.access.accounts["carol"];
    assert_eq!(carol.keys.len(), 2);
    let nightly = carol
        .keys
        .iter()
        .find(|k| k.name == "nightly report")
        .unwrap();
    assert_eq!(nightly.digest, digest);
    assert_eq!(nightly.id, crate::keys::file_id("carol", "nightly report"));
    assert_eq!(nightly.grants.len(), 2);
    let backup = carol
        .keys
        .iter()
        .find(|k| k.name == "backup check")
        .unwrap();
    assert_eq!(backup.digest, crate::keys::digest(&other));
    assert_eq!(
        line(&cfg, "user.carol.key.nightly report.key").value,
        "<hidden>"
    );
    assert_eq!(
        line(&cfg, "user.carol.key.nightly report.key").source,
        "env:CAROL_KEY"
    );
    assert_eq!(
        line(&cfg, "user.carol.key.backup check.key_sha256").source,
        "env:OTHER_SUM"
    );
    assert!(!cfg.render_effective().contains(&text));
}

#[test]
fn a_bad_block_key_stops_the_start_and_never_shows_the_text_of_a_key() {
    let (mut vars, text) = vars_with_key();
    vars.push(("BAD_FORM".into(), format!("{}x", &text[..52])));
    vars.push(("BAD_SUM".into(), "not hexadecimal".into()));
    vars.push(("SHORT_SUM".into(), "ab".repeat(31)));
    let key = |attributes: &str, grants: &str| {
        with_keys(&format!(
            r#"user "carol" {{ grants = [] key "k" {{ {attributes} grants = [{grants}] }} }}"#
        ))
    };
    for (raw, expected) in [
        (key(r#"key_env = "BAD_FORM""#, ""), "user.carol.key.k.key"),
        (
            key(r#"key_sha256_env = "BAD_SUM""#, ""),
            "64 hexadecimal characters",
        ),
        (
            key(r#"key_sha256_env = "SHORT_SUM""#, ""),
            "64 hexadecimal characters",
        ),
        (
            key(r#"key_env = "CAROL_KEY" key_sha256_env = "SHORT_SUM""#, ""),
            "not two",
        ),
        (key("", ""), "exactly one"),
        (key(r#"key = "mcp_x""#, ""), "must not be written"),
        (key(r#"key_sha256 = "ab""#, ""), "must not be written"),
        (
            key(
                r#"key_env = "CAROL_KEY""#,
                r#""keys.own.read on service:portal""#,
            ),
            "never holds one",
        ),
        (
            key(r#"key_env = "MISSING_VARIABLE""#, ""),
            "MISSING_VARIABLE",
        ),
    ] {
        let message = parse_keys(&raw, &vars)
            .err()
            .unwrap_or_else(|| panic!("must stop:\n{raw}"));
        assert!(message.contains(expected), "{expected}: {message}");
        assert!(
            message.contains("carol"),
            "the error names the login: {message}"
        );
        assert!(
            !message.contains(&text),
            "the text of the key is in the message: {message}"
        );
    }
    // A name of 65 characters, and a control character.
    let long = "n".repeat(65);
    let raw = with_keys(&format!(
        r#"user "carol" {{ grants = [] key "{long}" {{ key_env = "CAROL_KEY" grants = [] }} }}"#
    ));
    assert!(parse_keys(&raw, &vars).is_err());
    // Without the block `keys`, a block `key` stops the program.
    let raw = with(
        r#"access { grants_mode = "hybrid" } user "carol" { grants = [] key "k" { key_env = "CAROL_KEY" grants = [] } }"#,
    );
    let message = parse_keys(&raw, &vars).err().unwrap();
    assert!(message.contains("needs the block keys"), "{message}");
}

#[test]
fn a_user_has_at_most_max_per_user_blocks_key_and_each_key_at_most_512_grants() {
    let (mut vars, _) = vars_with_key();
    let second = crate::keys::generate().unwrap().to_string();
    vars.push(("SECOND_KEY".into(), second));
    let raw = with(&format!(
        r#"access {{ grants_mode = "hybrid" }}
        {}
        keys {{ max_per_user = 1 }}
        user "carol" {{ grants = []
          key "a" {{ key_env = "CAROL_KEY" grants = [] }}
          key "b" {{ key_env = "SECOND_KEY" grants = [] }}
        }}"#,
        KEYS_NEEDS.replace("keys {}", "")
    ));
    let message = parse_keys(&raw, &vars).err().unwrap();
    assert!(
        message.contains("carol") && message.contains("max_per_user"),
        "{message}"
    );
    let many: Vec<String> = (0..513)
        .map(|i| format!("\"access on service:s{i}\""))
        .collect();
    let raw = with(&format!(
        r#"access {{ grants_mode = "hybrid" }}
        {KEYS_NEEDS}
        users {{ max_grants = 600 }}
        user "carol" {{ grants = [] key "a" {{ key_env = "CAROL_KEY" grants = [{}] }} }}"#,
        many.join(", ")
    ));
    let message = parse_keys(&raw, &vars).err().unwrap();
    assert!(
        message.contains("too many items") && message.contains("512"),
        "{message}"
    );
    assert!(
        message.len() < 2_000,
        "the error is cut: {} bytes",
        message.len()
    );
}

// The block `inbox`.

const INBOX_VARS: &[(&str, &str)] = &[
    ("INBOX_KEY", "the text of a server key"),
    ("INBOX_CRED", "the text of a credential record"),
    (
        "INBOX_TOKEN",
        "a token of the producer grafana of 32 bytes or more",
    ),
    (
        "INBOX_TOKEN_2",
        "another token of the producer ci of 32 bytes or more",
    ),
];

fn inbox_parse(raw: &str) -> Result<(Config, Secrets), String> {
    let vars: Vec<(&str, &str)> = VARS.iter().chain(INBOX_VARS).copied().collect();
    parse_with(raw, &vars)
}

fn inbox_ok(raw: &str) -> (Config, Secrets) {
    inbox_parse(raw).unwrap_or_else(|e| panic!("{e}\n{raw}"))
}

fn inbox_err(raw: &str) -> String {
    match inbox_parse(raw) {
        Ok(_) => panic!("must be rejected:\n{raw}"),
        Err(e) => e,
    }
}

const INBOX: &str = r#"inbox {
  server_key_env = "INBOX_KEY"
  devices {
    listen = "0.0.0.0:8080"
    credential_env = "INBOX_CRED"
    trusted_proxies = []
    heartbeat_interval = "20s"
    session_timeout = "60s"
    resend_after = "15s"
    max_inflight = 32
    unreachable_after = "2m"
    offline_warning_after = "3m"
  }
  storage {
    delivery_ttl = "7d"
    alert_retention = "30d"
    connection_log_retention = "90d"
  }
  group "infra" { name = "Infra prod" description = "Production outages" }
  group "oncall" { name = "On call" }
  producer "grafana" { token_env = "INBOX_TOKEN" groups = ["infra"] }
}"#;

fn with_inbox(inbox: &str) -> String {
    format!("{MINIMAL}\n{inbox}")
}

#[test]
fn the_inbox_is_off_without_its_block() {
    let (config, secrets) = ok(MINIMAL);
    assert!(config.inbox.is_none());
    assert!(secrets.inbox.is_none());
    assert!(config
        .effective
        .iter()
        .all(|l| !l.key.starts_with("inbox.")));
}

#[test]
fn the_inbox_block_gives_typed_values_with_documented_defaults() {
    let (config, secrets) = inbox_ok(&with_inbox(INBOX));
    let inbox = config.inbox.as_ref().unwrap();
    assert_eq!(inbox.name, "Inbox");
    assert_eq!(inbox.devices.listen, "0.0.0.0:8080".parse().unwrap());
    assert_eq!(inbox.devices.max_devices, 1000);
    assert_eq!(inbox.devices.max_auth_failures, 5);
    assert_eq!(inbox.devices.auth_failure_window, Duration::from_secs(600));
    assert_eq!(inbox.devices.max_request_bytes, 64 * 1024);
    assert_eq!(inbox.devices.heartbeat_interval, Duration::from_secs(20));
    assert_eq!(inbox.devices.max_inflight, 32);
    assert_eq!(inbox.storage.delivery_ttl, Duration::from_secs(7 * 86_400));
    assert_eq!(inbox.storage.pool_size, 8);
    assert!(inbox.replica.is_none() && inbox.cluster.is_none());
    assert_eq!(inbox.groups.len(), 2);
    assert_eq!(inbox.groups["oncall"].description, "");
    assert_eq!(inbox.producers["grafana"].groups, ["infra"]);
    assert!(!inbox.producers["grafana"].webhook_send_resolved);
    let secrets = secrets.inbox.unwrap();
    assert_eq!(secrets.server_key.as_bytes(), b"the text of a server key");
    assert_eq!(
        secrets.credential.as_bytes(),
        b"the text of a credential record"
    );
    assert_eq!(secrets.producers.len(), 1);
    // The effective configuration shows the source and hides the secrets.
    assert_eq!(line(&config, "inbox.name").source, "default");
    assert_eq!(line(&config, "inbox.devices.listen").source, "file");
    assert_eq!(line(&config, "inbox.server_key").value, "<hidden>");
    assert_eq!(line(&config, "inbox.server_key").source, "env:INBOX_KEY");
    assert_eq!(line(&config, "inbox.devices.credential").value, "<hidden>");
    assert_eq!(
        line(&config, "inbox.producer.grafana.token").value,
        "<hidden>"
    );
    assert_eq!(
        line(&config, "inbox.group.infra.name").value,
        "\"Infra prod\""
    );
    assert_eq!(
        line(&config, "inbox.group.oncall.description").source,
        "default"
    );
    assert!(config
        .effective
        .iter()
        .all(|l| !l.value.contains("server key")));
    assert!(config
        .render_effective()
        .contains("inbox.devices.resend_after = \"15s\"  # file"));
}

#[test]
fn the_delivery_values_have_no_default() {
    for key in [
        "heartbeat_interval",
        "session_timeout",
        "resend_after",
        "max_inflight",
        "unreachable_after",
        "offline_warning_after",
        "delivery_ttl",
        "alert_retention",
        "connection_log_retention",
    ] {
        let without: String = INBOX
            .lines()
            .filter(|l| !l.trim_start().starts_with(&format!("{key} ")))
            .map(|l| format!("{l}\n"))
            .collect();
        let message = inbox_err(&with_inbox(&without));
        assert!(
            message.contains("missing required configuration key") && message.contains(key),
            "{key}: {message}"
        );
    }
}

#[test]
fn the_inbox_block_needs_its_inner_blocks_and_one_group() {
    let without_devices = INBOX.replacen("devices {", "devicez {", 1);
    let message = inbox_err(&with_inbox(&without_devices));
    assert!(message.contains("devicez"), "{message}");
    let no_group = INBOX
        .replace(
            r#"group "infra" { name = "Infra prod" description = "Production outages" }"#,
            "",
        )
        .replace(r#"group "oncall" { name = "On call" }"#, "")
        .replace(
            r#"producer "grafana" { token_env = "INBOX_TOKEN" groups = ["infra"] }"#,
            "",
        );
    let message = inbox_err(&with_inbox(&no_group));
    assert!(message.contains("at least one block group"), "{message}");
    // An inner block of the inbox is not a block of the top of the file.
    let message = inbox_err(&format!(
        "{MINIMAL}\ndevices {{ listen = \"0.0.0.0:1\" }}\n{INBOX}"
    ));
    assert!(
        message.contains("unknown configuration block: devices"),
        "{message}"
    );
    let message = inbox_err(&format!(
        "{MINIMAL}\ngroup \"x\" {{ name = \"X\" }}\n{INBOX}"
    ));
    assert!(
        message.contains("unknown configuration block: group"),
        "{message}"
    );
    let twice = INBOX.replacen("  storage {", "  storage { }\n  storage {", 1);
    let message = inbox_err(&with_inbox(&twice));
    assert!(
        message.contains("duplicate configuration block: inbox.storage"),
        "{message}"
    );
}

#[test]
fn the_inbox_schema_is_closed_and_strict() {
    for (from, to, expected) in [
        (
            "max_inflight = 32",
            "max_inflight = 32 admin_listen = \"127.0.0.1:1\"",
            "unknown configuration key: inbox.devices.admin_listen",
        ),
        (
            "max_inflight = 32",
            "max_inflight = \"32\"",
            "inbox.devices.max_inflight",
        ),
        ("max_inflight = 32", "max_inflight = 2000", "out of range"),
        (
            "resend_after = \"15s\"",
            "resend_after = \"15\"",
            "inbox.devices.resend_after",
        ),
        (
            "name = \"On call\"",
            "name = \"On call\" colour = \"red\"",
            "unknown configuration key: inbox.group.oncall.colour",
        ),
        (
            "groups = [\"infra\"]",
            "groups = [\"infra\", \"infra\"]",
            "written two times",
        ),
        ("groups = [\"infra\"]", "groups = [\"missing\"]", "missing"),
        ("groups = [\"infra\"]", "groups = []", "at least one group"),
        (
            "groups = [\"infra\"]",
            "groups = [\"Infra\"]",
            "inbox.producer.grafana.groups",
        ),
        (
            "server_key_env = \"INBOX_KEY\"",
            "server_key = \"abc\"",
            "must not be written in the file",
        ),
        (
            "server_key_env = \"INBOX_KEY\"",
            "server_key_env = \"INBOX_KEY\" server_key_file = \"/x/y\"",
            "exactly one",
        ),
        (
            "credential_env = \"INBOX_CRED\"",
            "credential_env = \"NOT_SET\"",
            "NOT_SET",
        ),
        (
            "token_env = \"INBOX_TOKEN\"",
            "token_env = \"NOT_SET\"",
            "NOT_SET",
        ),
        (
            "server_key_env = \"INBOX_KEY\"",
            "name = \"\" server_key_env = \"INBOX_KEY\"",
            "inbox.name",
        ),
        (
            "listen = \"0.0.0.0:8080\"",
            "listen = \"0.0.0.0:8080\" trusted_proxies2 = []",
            "trusted_proxies2",
        ),
    ] {
        assert!(INBOX.contains(from), "{from}");
        let message = inbox_err(&with_inbox(&INBOX.replacen(from, to, 1)));
        assert!(message.contains(expected), "{to}: {message}");
    }
}

#[test]
fn the_inbox_values_that_contradict_each_other_stop_the_start() {
    for (from, to, expected) in [
        (
            "session_timeout = \"60s\"",
            "session_timeout = \"30s\"",
            "inbox.devices.session_timeout must be at least two times",
        ),
        (
            "unreachable_after = \"2m\"",
            "unreachable_after = \"30s\"",
            "inbox.devices.unreachable_after must be at least",
        ),
        (
            "offline_warning_after = \"3m\"",
            "offline_warning_after = \"30s\"",
            "inbox.devices.offline_warning_after must be at least two times",
        ),
        (
            "alert_retention = \"30d\"",
            "alert_retention = \"1d\"",
            "inbox.storage.alert_retention must be at least",
        ),
    ] {
        let message = inbox_err(&with_inbox(&INBOX.replacen(from, to, 1)));
        assert!(message.contains(expected), "{to}: {message}");
        assert!(
            message.contains("is "),
            "the error holds the values: {message}"
        );
    }
    // The listener of the phones is not the listener of the portal.
    let message = inbox_err(&with_inbox(
        &INBOX.replace("0.0.0.0:8080", "127.0.0.1:4181"),
    ));
    assert!(
        message.contains("inbox.devices.listen must not use the port of server.listen_address"),
        "{message}"
    );
}

#[test]
fn two_producers_have_their_own_tokens_and_options() {
    let two = INBOX.replace(
        r#"producer "grafana""#,
        r#"producer "ci" { token_env = "INBOX_TOKEN_2" groups = ["oncall"] webhook_send_resolved = true }
  producer "grafana""#,
    );
    let (config, secrets) = inbox_ok(&with_inbox(&two));
    assert!(config.inbox.as_ref().unwrap().producers["ci"].webhook_send_resolved);
    assert_eq!(secrets.inbox.unwrap().producers.len(), 2);
}

#[test]
fn the_replica_block_is_for_sqlite_and_the_cluster_block_is_for_postgres() {
    let replica = INBOX.replace(
        r#"  group "infra""#,
        "  replica { dir = \"/replica\" }\n  group \"infra\"",
    );
    let (config, _) = inbox_ok(&with_inbox(&replica));
    let r = config.inbox.as_ref().unwrap().replica.as_ref().unwrap();
    assert_eq!(r.dir, std::path::PathBuf::from("/replica"));
    assert_eq!(r.snapshot_interval, Duration::from_secs(10));
    assert_eq!(r.lease_ttl, Duration::from_secs(30));
    assert_eq!(r.sync_timeout, Duration::from_secs(5));
    assert!(r.sync_alerts, "the safe value is the default");
    assert_eq!(line(&config, "inbox.replica.sync_alerts").source, "default");
    // Not with PostgreSQL.
    let message = inbox_err(&format!(
        "{}\n{POSTGRES_BLOCK}",
        with_inbox(&replica).replace(MINIMAL, &shared())
    ));
    assert!(
        message.contains("inbox.replica is for the SQLite file"),
        "{message}"
    );
    // Not in the directory of the SQLite file.
    let same = format!(
        "{}\nstorage {{ database = \"/replica/portal.db\" }}",
        with_inbox(&replica)
    );
    let message = inbox_err(&same);
    assert!(
        message.contains("must not be the directory of storage.database"),
        "{message}"
    );
    for bad in [
        "dir = \"replica\"",
        "dir = \"/replica\" lease_ttl = \"1s\"",
        "dir = \"/replica\" sync_timeout = \"0s\"",
        "dir = \"/replica\" sync_alerts = \"true\"",
    ] {
        let broken = replica.replace("dir = \"/replica\"", bad);
        inbox_err(&with_inbox(&broken));
    }
    inbox_err(&with_inbox(&replica.replace("dir = \"/replica\"", "")));

    let cluster = INBOX.replace(
        r#"  group "infra""#,
        "  cluster { listen = \"0.0.0.0:8082\" peers = [\"inbox-peers.apps.svc.cluster.local:8082\"] }\n  group \"infra\"",
    );
    // PostgreSQL needs the shared signing key.
    let with_postgres = format!(
        "{}\n{POSTGRES_BLOCK}",
        with_inbox(&cluster).replace(MINIMAL, &shared())
    );
    let (config, _) = inbox_ok(&with_postgres);
    let c = config.inbox.as_ref().unwrap().cluster.as_ref().unwrap();
    assert_eq!(c.peers, ["inbox-peers.apps.svc.cluster.local:8082"]);
    assert_eq!(config.inbox.as_ref().unwrap().storage.pool_size, 8);
    let message = inbox_err(&with_inbox(&cluster));
    assert!(
        message.contains("inbox.cluster needs the block postgres"),
        "{message}"
    );
    for (from, to) in [
        ("0.0.0.0:8082", "0.0.0.0:8080"),
        ("0.0.0.0:8082", "0.0.0.0:4181"),
        (
            "inbox-peers.apps.svc.cluster.local:8082",
            "inbox-peers.apps.svc.cluster.local",
        ),
        ("inbox-peers.apps.svc.cluster.local:8082", "inbox-peers:0"),
        (
            "peers = [\"inbox-peers.apps.svc.cluster.local:8082\"]",
            "peers = []",
        ),
    ] {
        inbox_err(&with_postgres.replacen(from, to, 1));
    }
}

#[test]
fn the_pool_size_of_the_inbox_applies_only_with_postgres() {
    let sized = INBOX.replace(
        "connection_log_retention = \"90d\"",
        "connection_log_retention = \"90d\" pool_size = 4",
    );
    let message = inbox_err(&with_inbox(&sized));
    assert!(
        message.contains("inbox.storage.pool_size must not be set"),
        "{message}"
    );
    let with_postgres = format!(
        "{}\n{POSTGRES_BLOCK}",
        with_inbox(&sized).replace(MINIMAL, &shared())
    );
    let (config, _) = inbox_ok(&with_postgres);
    assert_eq!(config.inbox.as_ref().unwrap().storage.pool_size, 4);
}
