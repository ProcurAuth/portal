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
    ("PORTAL_TOTP_KEY", TOTP_KEY),
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
      totp_key_env            = "PORTAL_TOTP_KEY"
    }
"#;

/// The key of the second factor that the tests give to every set of variables. A test of this key sets its own.
const TOTP_KEY: &str = "00112233445566778899aabbccddeeff00112233445566778899aabbccddeeff";

fn parse_with(raw: &str, vars: &[(&str, &str)]) -> Result<(Config, Secrets), String> {
    let mut vars: Vec<(String, String)> = vars
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
    if !vars.iter().any(|(k, _)| k == "PORTAL_TOTP_KEY") {
        vars.push(("PORTAL_TOTP_KEY".into(), TOTP_KEY.into()));
    }
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
    assert_eq!(cfg.session.ttl, Duration::from_secs(24 * 3600));
    assert_eq!(cfg.session.idle_timeout, Duration::from_secs(30 * 60));
    assert_eq!(
        cfg.session.remember,
        Some(Remember {
            ttl: Duration::from_secs(30 * 86_400),
            idle_timeout: Duration::from_secs(7 * 86_400),
        })
    );
    assert_eq!(cfg.session.bind, SessionBind::IpUserAgent);
    assert_eq!(cfg.session.max_active_sessions, 4096);
    assert_eq!(cfg.session.cookie_name, "__Host-portal");
    assert!(cfg.metrics.enabled);
    assert_eq!(cfg.metrics.path, "/metrics");
    assert!(cfg.discovery.kubernetes.is_none());
    assert!(cfg.discovery.docker_socket.is_none());
    assert_eq!(cfg.discovery.interval, Duration::from_secs(15));
    assert_eq!(cfg.ui.icons_dir, PathBuf::from("/icons"));
    assert_eq!(cfg.ui.favicon, Favicon::Default);
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
        authentication {{ superadmin_login_file = "{}" superadmin_password_file = "{}" totp_key_env = "PORTAL_TOTP_KEY" }}"#,
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
            authentication {{ superadmin_login_{form} superadmin_password_env = "PORTAL_PASSWORD" totp_key_env = "PORTAL_TOTP_KEY" }}"#
        ));
        assert_eq!(secrets.superadmin_login, "0123456789abcdef0123456789abcdef");
        let (_, secrets) = ok(&format!(
            r#"server {{ listen_address = "0.0.0.0:4181" trusted_proxies = [] }}
            authentication {{ superadmin_login_env = "PORTAL_LOGIN" superadmin_password_{form} totp_key_env = "PORTAL_TOTP_KEY" }}"#
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
        r#"ui { favicon = "None" }"#,
        r#"ui { favicon = "off" }"#,
        "ui { favicon = none }",
        "ui { favicon = false }",
    ] {
        err(&with(bad));
    }
    let (cfg, _) = ok(&with(r#"ui { favicon = "none" }"#));
    assert_eq!(cfg.ui.favicon, Favicon::None);
    let message = err(&with(r#"ui { favicon = "off" }"#));
    assert!(
        message.contains("ui.favicon") && message.contains("default") && message.contains("off"),
        "{message}"
    );
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
        r#"idle_timeout = "25h""#,
        r#"bind = "ip""#,
    ] {
        err(&with(&format!("session {{ {extra} }}")));
    }
}

#[test]
fn the_remember_keys_have_documented_defaults_and_show_in_the_effective_configuration() {
    let (cfg, _) = ok(MINIMAL);
    for (key, value) in [
        ("session.ttl", "\"24h\""),
        ("session.remember", "true"),
        ("session.remember_ttl", "\"30d\""),
        ("session.remember_idle_timeout", "\"7d\""),
    ] {
        let found = line(&cfg, key);
        assert_eq!(
            (found.value.as_str(), found.source.as_str()),
            (value, "default"),
            "{key}"
        );
    }
    let (cfg, _) = ok(&with(
        r#"session { remember_ttl = "90d" remember_idle_timeout = "14d" ttl = "7d" }"#,
    ));
    assert_eq!(
        cfg.session.remember,
        Some(Remember {
            ttl: Duration::from_secs(90 * 86_400),
            idle_timeout: Duration::from_secs(14 * 86_400),
        })
    );
    assert_eq!(line(&cfg, "session.remember_ttl").source, "file");
    assert!(cfg
        .render_effective()
        .contains("session.remember_ttl = \"90d\""));
}

#[test]
fn the_remember_keys_refuse_values_out_of_range() {
    for (extra, key) in [
        (r#"remember_ttl = "91d""#, "session.remember_ttl"),
        (r#"remember_ttl = "30s""#, "session.remember_ttl"),
        (
            r#"remember_idle_timeout = "91d""#,
            "session.remember_idle_timeout",
        ),
        (
            r#"remember_idle_timeout = "30s""#,
            "session.remember_idle_timeout",
        ),
        (r#"remember = "true""#, "session.remember"),
        (r#"remember = 1"#, "session.remember"),
    ] {
        let message = err(&with(&format!("session {{ {extra} }}")));
        assert!(message.contains(key), "{extra}: {message}");
    }
    let message = err(&with(r#"session { remember_ttl = "91d" }"#));
    assert!(
        message.contains("91d") && message.contains("90d"),
        "{message}"
    );
}

#[test]
fn the_remember_keys_contradict_the_other_session_keys() {
    // remember_ttl is not shorter than ttl.
    let message = err(&with(
        r#"session { ttl = "7d" remember_ttl = "3d" remember_idle_timeout = "1d" }"#,
    ));
    assert!(
        message.contains("session.remember_ttl")
            && message.contains("3d")
            && message.contains("7d"),
        "{message}"
    );
    ok(&with(
        r#"session { ttl = "3d" remember_ttl = "3d" remember_idle_timeout = "1d" }"#,
    ));
    // remember_idle_timeout is not longer than remember_ttl. The default 7d is too long for 2d.
    let message = err(&with(r#"session { remember_ttl = "2d" }"#));
    assert!(
        message.contains("session.remember_idle_timeout")
            && message.contains("7d")
            && message.contains("2d"),
        "{message}"
    );
    ok(&with(
        r#"session { remember_ttl = "2d" remember_idle_timeout = "2d" }"#,
    ));
    // remember_idle_timeout is not shorter than idle_timeout.
    let message = err(&with(
        r#"session { ttl = "7d" idle_timeout = "2d" remember_idle_timeout = "1d" }"#,
    ));
    assert!(
        message.contains("session.remember_idle_timeout")
            && message.contains("session.idle_timeout")
            && message.contains("1d")
            && message.contains("2d"),
        "{message}"
    );
}

#[test]
fn session_remember_false_turns_the_option_off_and_refuses_its_values() {
    let (cfg, _) = ok(&with("session { remember = false }"));
    assert_eq!(cfg.session.remember, None);
    assert_eq!(line(&cfg, "session.remember").value, "false");
    for key in ["session.remember_ttl", "session.remember_idle_timeout"] {
        assert!(cfg.effective.iter().all(|l| l.key != key), "{key}");
    }
    for body in [
        r#"remember = false remember_ttl = "30d""#,
        r#"remember = false remember_idle_timeout = "7d""#,
    ] {
        let message = err(&with(&format!("session {{ {body} }}")));
        assert!(
            message.contains("must not be set when session.remember is false"),
            "{body}: {message}"
        );
    }
    // With the option off, the values of the option do not stop the start.
    ok(&with(
        r#"session { remember = false ttl = "7d" idle_timeout = "7d" }"#,
    ));
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
            users {{ allow_creation = {} }}
            user "alice" {{ grants = ["query.run.read on service:db", "access on service:db"] }}
            user "bob" {{ grants = [] }}"#,
            mode != "static"
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

/// `session.max_sessions_per_account`: the documented default, a value of the file, the range, and the contradiction
/// with `session.max_active_sessions`.
#[test]
fn one_account_has_a_bound_of_sessions_that_is_at_most_the_global_bound() {
    let (cfg, _) = ok(&with(""));
    assert_eq!(cfg.session.max_sessions_per_account, 16);
    assert_eq!(line(&cfg, "session.max_sessions_per_account").value, "16");
    let (cfg, _) = ok(&with("session { max_sessions_per_account = 2 }"));
    assert_eq!(cfg.session.max_sessions_per_account, 2);
    let message = err(&with("session { max_sessions_per_account = 0 }"));
    assert!(
        message.contains("session.max_sessions_per_account"),
        "{message}"
    );
    let message = err(&with("session { max_active_sessions = 8 }"));
    assert!(
        message.contains("session.max_sessions_per_account")
            && message.contains("session.max_active_sessions")
            && message.contains("16")
            && message.contains('8'),
        "{message}"
    );
    let (cfg, _) = ok(&with(
        "session { max_active_sessions = 8 max_sessions_per_account = 8 }",
    ));
    assert_eq!(cfg.session.max_sessions_per_account, 8);
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
    let text = crate::keys::generate(crate::keys::KeyKind::Mcp)
        .unwrap()
        .to_string();
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
        (r#"keys {}"#, "the block identity is absent"),
        (
            r#"identity { issuer = "i" signing_key_env = "IDENTITY_KEY" } keys {}"#,
            "the block pdp is absent",
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
            { grant = "keys.own.use on service:portal/key_kind:mcp", limits = { max_key_lifetime = 2592000 } },
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
            r#""keys.own.create on service:portal/key_kind:mcp", "keys.own.read on service:portal""#,
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
          grants = ["access on service:datastore", "keys.own.use on service:portal/key_kind:mcp"]
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
    let other = crate::keys::generate(crate::keys::KeyKind::Mcp)
        .unwrap()
        .to_string();
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
    // The last character is the end of the checksum. The new one differs from it, or the key stays good in 1 run of 62.
    let wrong_last = if text.ends_with('x') { 'y' } else { 'x' };
    vars.push(("BAD_FORM".into(), format!("{}{wrong_last}", &text[..52])));
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
            "cannot hold",
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

/// An operator can point `key_sha256_env` or `key_sha256_file` at the text of a key by mistake. The
/// error names the attribute and the source, and it never shows the value.
#[test]
fn a_key_text_in_key_sha256_never_appears_in_the_error() {
    let (vars, text) = vars_with_key();
    let file = temp_file(text.as_bytes());
    let not_utf8 = temp_file(&[0xff, 0xfe, b'a']);
    for (attribute, source) in [
        (
            r#"key_sha256_env = "CAROL_KEY""#.to_string(),
            "env:CAROL_KEY".to_string(),
        ),
        (
            format!(r#"key_sha256_file = "{}""#, file.display()),
            format!("file:{}", file.display()),
        ),
        (
            format!(r#"key_sha256_file = "{}""#, not_utf8.display()),
            format!("file:{}", not_utf8.display()),
        ),
    ] {
        let raw = with_keys(&format!(
            r#"user "carol" {{ grants = [] key "k" {{ {attribute} grants = [] }} }}"#
        ));
        let message = parse_keys(&raw, &vars).err().unwrap();
        assert!(message.contains("user.carol.key.k.key_sha256"), "{message}");
        assert!(message.contains(&source), "{message}");
        assert!(message.contains("the value is hidden"), "{message}");
        assert!(!message.contains(&text), "the text of the key: {message}");
        assert!(
            !message.contains(&text[4..20]),
            "a part of the key: {message}"
        );
    }
    let _ = fs::remove_file(file);
    let _ = fs::remove_file(not_utf8);
}

#[test]
fn a_user_has_at_most_max_per_user_blocks_key_and_each_key_at_most_512_grants() {
    let (mut vars, _) = vars_with_key();
    let second = crate::keys::generate(crate::keys::KeyKind::Mcp)
        .unwrap()
        .to_string();
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

const INBOX_VARS: &[(&str, &str)] = &[("INBOX_KEY", "the text of a server key")];

/// The texts of two API keys, made once for the tests.
fn api_texts() -> &'static [String; 2] {
    static TEXTS: std::sync::OnceLock<[String; 2]> = std::sync::OnceLock::new();
    TEXTS.get_or_init(|| {
        [(); 2].map(|_| {
            crate::keys::generate(crate::keys::KeyKind::Api)
                .unwrap()
                .to_string()
        })
    })
}

fn inbox_parse(raw: &str) -> Result<(Config, Secrets), String> {
    let texts = api_texts();
    let vars: Vec<(&str, &str)> = VARS
        .iter()
        .chain(INBOX_VARS)
        .copied()
        .chain([
            ("INBOX_TOKEN", texts[0].as_str()),
            ("INBOX_TOKEN_2", texts[1].as_str()),
        ])
        .collect();
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
    public_address = "inbox.example.org:8080"
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
  api_key "grafana" { owner = "ci" secret_env = "INBOX_TOKEN" groups = ["infra"] }
}"#;

/// The blocks that an API key of the file needs: the kind `api` on, an account of the file that
/// owns the key, and the rights of this account.
const API_ACCESS: &str = r#"
access { grants_mode = "static" }
users { allow_creation = false }
keys { mcp = false api = true }
user "ci" {
  grants = [
    "keys.own.use on service:portal/key_kind:api",
    "alert.send on service:portal/inbox_group:infra",
    "alert.send on service:portal/inbox_group:oncall",
  ]
}
"#;

fn with_inbox(inbox: &str) -> String {
    format!("{MINIMAL}\n{API_ACCESS}\n{inbox}")
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
    assert_eq!(config.server.name, "Portal");
    assert_eq!(inbox.devices.max_devices, 1000);
    assert_eq!(inbox.devices.max_per_account, 10);
    assert_eq!(inbox.devices.max_auth_failures, 5);
    assert_eq!(inbox.devices.auth_failure_window, Duration::from_secs(600));
    assert_eq!(inbox.devices.max_handshakes_per_address, 16);
    assert_eq!(
        line(&config, "inbox.devices.max_per_account").source,
        "default"
    );
    assert_eq!(inbox.devices.max_request_bytes, 64 * 1024);
    assert_eq!(inbox.devices.heartbeat_interval, Duration::from_secs(20));
    assert_eq!(inbox.devices.max_inflight, 32);
    assert_eq!(inbox.storage.delivery_ttl, Duration::from_secs(7 * 86_400));
    assert_eq!(inbox.storage.pool_size, 8);
    assert!(inbox.replica.is_none() && inbox.cluster.is_none());
    assert_eq!(inbox.groups.len(), 2);
    assert_eq!(inbox.groups["oncall"].description, "");
    assert!(!inbox.groups["infra"].critical && !inbox.groups["oncall"].critical);
    assert_eq!(
        line(&config, "inbox.group.infra.critical").source,
        "default"
    );
    assert_eq!(inbox.api_keys["grafana"].groups, ["infra"]);
    assert_eq!(inbox.api_keys["grafana"].owner, "ci");
    assert!(!inbox.api_keys["grafana"].webhook_send_resolved);
    let secrets = secrets.inbox.unwrap();
    assert_eq!(secrets.server_key.as_bytes(), b"the text of a server key");
    assert_eq!(secrets.api_keys.len(), 1);
    // The effective configuration shows the source and hides the secrets.
    assert_eq!(line(&config, "server.name").source, "default");
    assert!(config
        .effective
        .iter()
        .all(|l| l.key != "inbox.devices.listen"));
    assert_eq!(line(&config, "inbox.server_key").value, "<hidden>");
    assert_eq!(line(&config, "inbox.server_key").source, "env:INBOX_KEY");
    assert_eq!(
        line(&config, "inbox.devices.public_address").value,
        "\"inbox.example.org:8080\""
    );
    assert_eq!(inbox.devices.public_address, "inbox.example.org:8080");
    assert_eq!(inbox.enrollment.code_ttl, Duration::from_secs(600));
    assert_eq!(inbox.enrollment.max_active_per_account, 5);
    assert_eq!(line(&config, "inbox.enrollment.code_ttl").source, "default");
    assert!(
        config
            .effective
            .iter()
            .all(|l| l.key != "inbox.devices.credential"),
        "the shared password does not exist any more"
    );
    assert_eq!(
        line(&config, "inbox.api_key.grafana.secret").value,
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
fn a_group_can_be_critical_and_the_value_must_be_a_boolean() {
    let text = with_inbox(&INBOX.replace(
        "group \"oncall\" { name = \"On call\" }",
        "group \"oncall\" { name = \"On call\" critical = true }",
    ));
    let (config, _) = inbox_ok(&text);
    let inbox = config.inbox.as_ref().unwrap();
    assert!(inbox.groups["oncall"].critical);
    assert!(!inbox.groups["infra"].critical);
    assert_eq!(line(&config, "inbox.group.oncall.critical").value, "true");
    assert_eq!(line(&config, "inbox.group.oncall.critical").source, "file");
    let text = with_inbox(&INBOX.replace(
        "group \"oncall\" { name = \"On call\" }",
        "group \"oncall\" { name = \"On call\" critical = \"yes\" }",
    ));
    let error = inbox_err(&text);
    assert!(error.contains("inbox.group.oncall.critical"), "{error}");
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
            r#"api_key "grafana" { owner = "ci" secret_env = "INBOX_TOKEN" groups = ["infra"] }"#,
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
fn the_public_address_is_required_and_follows_the_rules_of_the_app() {
    let without: String = INBOX
        .lines()
        .filter(|l| !l.trim_start().starts_with("public_address "))
        .map(|l| format!("{l}\n"))
        .collect();
    let message = inbox_err(&with_inbox(&without));
    assert!(
        message.contains("missing required configuration key")
            && message.contains("inbox.devices.public_address"),
        "{message}"
    );
    for good in [
        "inbox.example.org",
        "inbox.example.org:8080",
        "10.0.0.5:8080",
        "[::1]:8080",
        "http://inbox.example.org",
        "https://inbox.example.org:8443/prefix",
    ] {
        let raw = INBOX.replace("inbox.example.org:8080", good);
        let (config, _) = inbox_ok(&with_inbox(&raw));
        assert_eq!(config.inbox.unwrap().devices.public_address, good);
    }
    let too_long = format!("{}.example", "a".repeat(512));
    for bad in [
        "",
        "inbox example",
        "inbox.example.org:0",
        "inbox.example.org:65536",
        "inbox.example.org:",
        ":8080",
        "ftp://inbox.example.org",
        "https://user@inbox.example.org",
        "https://inbox.example.org/?x=1",
        "https://inbox.example.org/#top",
        "inbox.example.org:80:90",
        "[::1",
        "[not-an-address]:80",
        "inbox.exa\u{7}mple.org",
        too_long.as_str(),
    ] {
        let raw = INBOX.replace("inbox.example.org:8080", bad);
        let message = inbox_err(&with_inbox(&raw));
        assert!(
            message.contains("inbox.devices.public_address"),
            "{bad:?}: {message}"
        );
    }
}

#[test]
fn the_limits_of_the_phones_take_the_values_of_the_file() {
    let raw = INBOX.replacen(
        "max_inflight = 32",
        "max_inflight = 32 max_devices = 5 max_per_account = 5 max_handshakes_per_address = 2",
        1,
    );
    let (config, _) = inbox_ok(&with_inbox(&raw));
    let devices = &config.inbox.as_ref().unwrap().devices;
    assert_eq!(
        (
            devices.max_devices,
            devices.max_per_account,
            devices.max_handshakes_per_address
        ),
        (5, 5, 2)
    );
    assert_eq!(
        line(&config, "inbox.devices.max_handshakes_per_address").source,
        "file"
    );
}

#[test]
fn the_enrollment_block_changes_the_defaults() {
    let raw = INBOX.replace(
        "  storage {",
        "  enrollment { code_ttl = \"1h\" max_active_per_account = 2 }\n  storage {",
    );
    let (config, _) = inbox_ok(&with_inbox(&raw));
    let inbox = config.inbox.as_ref().unwrap();
    assert_eq!(inbox.enrollment.code_ttl, Duration::from_secs(3600));
    assert_eq!(inbox.enrollment.max_active_per_account, 2);
    assert_eq!(line(&config, "inbox.enrollment.code_ttl").source, "file");
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
            "max_inflight = 32",
            "max_inflight = 32 max_per_account = 0",
            "inbox.devices.max_per_account",
        ),
        (
            "max_inflight = 32",
            "max_inflight = 32 max_handshakes_per_address = 0",
            "inbox.devices.max_handshakes_per_address",
        ),
        (
            "max_inflight = 32",
            "max_inflight = 32 max_handshakes_per_address = \"16\"",
            "inbox.devices.max_handshakes_per_address",
        ),
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
            "inbox.api_key.grafana.groups",
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
            "public_address = \"inbox.example.org:8080\"",
            "credential_env = \"INBOX_KEY\"",
            "unknown configuration key: inbox.devices.credential",
        ),
        (
            "  storage {",
            "  enrollment { code_ttl = \"30s\" }\n  storage {",
            "inbox.enrollment.code_ttl",
        ),
        (
            "  storage {",
            "  enrollment { code_ttl = \"25h\" }\n  storage {",
            "inbox.enrollment.code_ttl",
        ),
        (
            "  storage {",
            "  enrollment { max_active_per_account = 0 }\n  storage {",
            "inbox.enrollment.max_active_per_account",
        ),
        (
            "  storage {",
            "  enrollment { max_active_per_account = 101 }\n  storage {",
            "inbox.enrollment.max_active_per_account",
        ),
        (
            "  storage {",
            "  enrollment { colour = 1 }\n  storage {",
            "unknown configuration key: inbox.enrollment.colour",
        ),
        (
            "secret_env = \"INBOX_TOKEN\"",
            "secret_env = \"NOT_SET\"",
            "NOT_SET",
        ),
        (
            "server_key_env = \"INBOX_KEY\"",
            "name = \"X\" server_key_env = \"INBOX_KEY\"",
            "unknown configuration key: inbox.name",
        ),
        (
            "public_address = \"inbox.example.org:8080\"",
            "public_address = \"inbox.example.org:8080\" trusted_proxies2 = []",
            "trusted_proxies2",
        ),
        // The phones use the listener of the portal: the key of the old phone port does not exist,
        // and it has no alias.
        (
            "public_address = \"inbox.example.org:8080\"",
            "listen = \"0.0.0.0:8080\" public_address = \"inbox.example.org:8080\"",
            "unknown configuration key: inbox.devices.listen",
        ),
    ] {
        assert!(INBOX.contains(from), "{from}");
        let message = inbox_err(&with_inbox(&INBOX.replacen(from, to, 1)));
        assert!(message.contains(expected), "{to}: {message}");
    }
}

#[test]
fn the_inbox_has_no_list_of_trusted_proxies_of_its_own() {
    // The phones use `server.trusted_proxies`. The old key is unknown, with no alias.
    let raw = INBOX.replacen(
        "public_address",
        "trusted_proxies = [\"10.0.0.0/8\"] public_address",
        1,
    );
    let message = inbox_err(&with_inbox(&raw));
    assert!(
        message.contains("unknown configuration key: inbox.devices.trusted_proxies"),
        "{message}"
    );
}

#[test]
fn the_metrics_path_cannot_hide_a_route_of_the_inbox() {
    for path in [
        "/readyz",
        "/api/version",
        "/api/v1",
        "/api/v1/alerts",
        "/v1",
        "/v1/session",
        "/healthz",
        "/auth/x",
    ] {
        let message = err(&with(&format!(
            r#"metrics {{ enabled = true path = "{path}" }}"#
        )));
        assert!(message.contains("metrics.path"), "{path}: {message}");
    }
    // A path that only starts with the same letters is free.
    ok(&with(r#"metrics { enabled = true path = "/v10/x" }"#));
    ok(&with(r#"metrics { enabled = true path = "/api/v10" }"#));
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
        (
            // The default of max_per_account (10) is above this max_devices.
            "max_inflight = 32",
            "max_inflight = 32 max_devices = 5",
            "inbox.devices.max_per_account must be at most inbox.devices.max_devices: max_per_account is 10, max_devices is 5",
        ),
        (
            "max_inflight = 32",
            "max_inflight = 32 max_devices = 5 max_per_account = 6",
            "inbox.devices.max_per_account must be at most inbox.devices.max_devices: max_per_account is 6, max_devices is 5",
        ),
    ] {
        let message = inbox_err(&with_inbox(&INBOX.replacen(from, to, 1)));
        assert!(message.contains(expected), "{to}: {message}");
        assert!(
            message.contains("is "),
            "the error holds the values: {message}"
        );
    }
}

#[test]
fn two_api_keys_have_their_own_texts_and_options() {
    let two = INBOX.replace(
        r#"api_key "grafana""#,
        r#"api_key "ci" { owner = "ci" secret_env = "INBOX_TOKEN_2" groups = ["oncall"] webhook_send_resolved = true }
  api_key "grafana""#,
    );
    let (config, secrets) = inbox_ok(&with_inbox(&two));
    assert!(config.inbox.as_ref().unwrap().api_keys["ci"].webhook_send_resolved);
    assert_eq!(secrets.inbox.unwrap().api_keys.len(), 2);
}

#[test]
fn a_key_of_the_file_reads_only_the_groups_of_its_block_where_its_owner_holds_alert_read() {
    let two = INBOX.replace(r#"groups = ["infra"]"#, r#"groups = ["infra", "oncall"]"#);
    // The owner sends to both groups and holds no alert.read: the key reads nothing.
    let (config, _) = inbox_ok(&with_inbox(&two));
    let key = &config.inbox.as_ref().unwrap().api_keys["grafana"];
    assert_eq!(key.groups, ["infra", "oncall"]);
    assert!(key.read_groups.is_empty(), "{:?}", key.read_groups);
    // With alert.read on oncall, the key reads oncall only.
    let reader = with_inbox(&two).replace(
        "    \"alert.send on service:portal/inbox_group:oncall\",\n",
        "    \"alert.send on service:portal/inbox_group:oncall\",\n    \"alert.read on service:portal/inbox_group:oncall\",\n",
    );
    let (config, _) = inbox_ok(&reader);
    let key = &config.inbox.as_ref().unwrap().api_keys["grafana"];
    assert_eq!(key.read_groups, ["oncall"]);
}

/// The audit finding portal/watch/group-alert-names-services-hidden-from-receivers: a key of the
/// file reads an alert about a service only when its owner may open the service.
#[test]
fn a_key_of_the_file_reads_the_alerts_about_the_services_that_its_owner_may_open() {
    use crate::inbox::rights::Services;
    let (config, _) = inbox_ok(&with_inbox(INBOX));
    let key = &config.inbox.as_ref().unwrap().api_keys["grafana"];
    assert_eq!(key.services, Services::Only(Default::default()));
    let opener = with_inbox(INBOX).replace(
        "    \"alert.send on service:portal/inbox_group:oncall\",\n",
        "    \"alert.send on service:portal/inbox_group:oncall\",\n    \"access on service:notes\",\n",
    );
    let (config, _) = inbox_ok(&opener);
    let key = &config.inbox.as_ref().unwrap().api_keys["grafana"];
    assert!(key.services.sees(Some("notes")));
    assert!(!key.services.sees(Some("vault")));
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

/// The inbox with a block `watch` that holds `keys`.
fn with_watch(keys: &str) -> String {
    with_inbox(&INBOX.replace(
        r#"  group "infra""#,
        &format!("  watch {{\n{keys}\n  }}\n  group \"infra\""),
    ))
}

#[test]
fn the_inbox_sends_no_alert_about_the_portal_without_the_block() {
    let (config, _) = inbox_ok(&with_inbox(INBOX));
    assert!(config.inbox.as_ref().unwrap().watch.is_none());
    assert!(config
        .effective
        .iter()
        .all(|l| !l.key.starts_with("inbox.watch.")));
}

#[test]
fn the_block_watch_has_documented_defaults() {
    let (config, _) = inbox_ok(&with_watch("group = \"infra\""));
    let a = config.inbox.as_ref().unwrap().watch.as_ref().unwrap();
    assert_eq!(a.group, "infra");
    assert!(a.service_unhealthy && a.sign_in_burst && a.recovered);
    assert_eq!(a.unhealthy_after, Duration::from_secs(120));
    assert_eq!(a.sign_in_failures, 10);
    assert_eq!(a.sign_in_window, Duration::from_secs(300));
    assert_eq!(a.repeat_after, Duration::from_secs(3600));
    assert_eq!(line(&config, "inbox.watch.group").source, "file");
    for (key, value) in [
        ("service_unhealthy", "true"),
        ("unhealthy_after", "\"2m\""),
        ("sign_in_burst", "true"),
        ("sign_in_failures", "10"),
        ("sign_in_window", "\"5m\""),
        ("repeat_after", "\"1h\""),
        ("recovered", "true"),
    ] {
        let l = line(&config, &format!("inbox.watch.{key}"));
        assert_eq!(
            (l.value.as_str(), l.source.as_str()),
            (value, "default"),
            "{key}"
        );
    }
    assert!(config
        .render_effective()
        .contains("inbox.watch.repeat_after = \"1h\"  # default"));
}

#[test]
fn the_block_watch_takes_the_values_of_the_file() {
    let (config, _) = inbox_ok(&with_watch(
        r#"group = "oncall"
    service_unhealthy = false
    unhealthy_after = "10s"
    sign_in_failures = 2
    sign_in_window = "24h"
    repeat_after = "7d"
    recovered = false"#,
    ));
    let a = config.inbox.as_ref().unwrap().watch.as_ref().unwrap();
    assert_eq!(a.group, "oncall");
    assert!(!a.service_unhealthy && !a.recovered && a.sign_in_burst);
    assert_eq!(a.unhealthy_after, Duration::from_secs(10));
    assert_eq!(a.sign_in_failures, 2);
    assert_eq!(a.sign_in_window, Duration::from_secs(86_400));
    assert_eq!(a.repeat_after, Duration::from_secs(7 * 86_400));
    assert_eq!(line(&config, "inbox.watch.repeat_after").source, "file");
}

#[test]
fn the_block_watch_stops_the_start_on_a_bad_value() {
    for (keys, key, value) in [
        (
            "unhealthy_after = \"9s\"",
            "inbox.watch.unhealthy_after",
            "9s",
        ),
        (
            "unhealthy_after = \"25h\"",
            "inbox.watch.unhealthy_after",
            "25h",
        ),
        ("sign_in_failures = 1", "inbox.watch.sign_in_failures", "1"),
        (
            "sign_in_failures = 10001",
            "inbox.watch.sign_in_failures",
            "10001",
        ),
        (
            "sign_in_failures = \"10\"",
            "inbox.watch.sign_in_failures",
            "10",
        ),
        (
            "sign_in_window = \"9s\"",
            "inbox.watch.sign_in_window",
            "9s",
        ),
        (
            "sign_in_window = \"25h\"",
            "inbox.watch.sign_in_window",
            "25h",
        ),
        ("repeat_after = \"59s\"", "inbox.watch.repeat_after", "59s"),
        ("repeat_after = \"8d\"", "inbox.watch.repeat_after", "8d"),
        ("recovered = \"yes\"", "inbox.watch.recovered", "yes"),
        (
            "service_unhealthy = 1",
            "inbox.watch.service_unhealthy",
            "1",
        ),
        (
            "sign_in_burst = \"true\"",
            "inbox.watch.sign_in_burst",
            "true",
        ),
    ] {
        let message = inbox_err(&with_watch(&format!("group = \"infra\"\n{keys}")));
        assert!(message.contains(key), "{message}");
        assert!(message.contains(value), "{message}");
    }
    // An unknown key and a bad id.
    let message = inbox_err(&with_watch("group = \"infra\"\nwebhook = \"x\""));
    assert!(message.contains("inbox.watch.webhook"), "{message}");
    let message = inbox_err(&with_watch("group = \"Infra Prod\""));
    assert!(message.contains("inbox.watch.group"), "{message}");
}

#[test]
fn the_block_watch_needs_an_existing_group() {
    let message = inbox_err(&with_watch("sign_in_burst = true"));
    assert!(
        message.contains("inbox.watch.group"),
        "the group is mandatory: {message}"
    );
    let message = inbox_err(&with_watch("group = \"missing\""));
    assert!(
        message.contains("inbox.watch.group")
            && message.contains("infra, oncall")
            && message.contains("\"missing\""),
        "{message}"
    );
}

#[test]
fn the_block_watch_with_both_alerts_off_is_a_contradiction() {
    let message = inbox_err(&with_watch(
        "group = \"infra\"\nservice_unhealthy = false\nsign_in_burst = false",
    ));
    assert!(
        message.contains("inbox.watch.service_unhealthy")
            && message.contains("inbox.watch.sign_in_burst"),
        "{message}"
    );
}

#[test]
fn the_block_watch_is_not_allowed_without_the_inbox() {
    let message = err(&with("watch { group = \"infra\" }"));
    assert!(message.contains("watch"), "{message}");
    let message = err(&with("inbox_watch { group = \"infra\" }"));
    assert!(message.contains("inbox_watch"), "{message}");
}

/// The old names of the feature. Two parts build each one, so that the repository holds no copy
/// of the old name.
fn old_block_name() -> String {
    ["pa", "ger"].concat()
}

#[test]
fn the_old_block_of_the_feature_is_an_unknown_block() {
    let old = old_block_name();
    let message = inbox_err(&with_inbox(&INBOX.replacen(
        "inbox {",
        &format!("{old} {{"),
        1,
    )));
    assert_eq!(message, format!("unknown configuration block: {old}"));
    // A block with the old name is refused even when the new block exists too.
    let message = inbox_err(&format!("{}\n{old} {{ }}", with_inbox(INBOX)));
    assert_eq!(message, format!("unknown configuration block: {old}"));
}

#[test]
fn the_old_block_of_the_alerts_of_the_portal_is_an_unknown_block() {
    let old = ["portal", "_alerts"].concat();
    let message = inbox_err(&with_inbox(&INBOX.replace(
        "group \"infra\"",
        &format!("{old} {{ group = \"infra\" }}\n  group \"infra\""),
    )));
    assert!(message.contains(&format!("inbox.{old}")), "{message}");
}

#[test]
fn the_name_of_the_instance_has_a_default_and_a_check() {
    let (config, _) = ok(MINIMAL);
    assert_eq!(config.server.name, "Portal");
    assert_eq!(line(&config, "server.name").source, "default");
    assert_eq!(line(&config, "server.name").value, "\"Portal\"");
    let (config, _) = ok(&with_server("name = \"Prod\"", ""));
    assert_eq!(config.server.name, "Prod");
    assert_eq!(line(&config, "server.name").source, "file");
    for bad in [
        "name = \"\"".to_string(),
        "name = \"   \"".to_string(),
        format!("name = \"{}\"", "a".repeat(65)),
        "name = \"a\\nb\"".to_string(),
        "name = 5".to_string(),
    ] {
        let message = err(&with_server(&bad, ""));
        assert!(message.contains("server.name"), "{bad}: {message}");
    }
    let (config, _) = ok(&with_server(&format!("name = \"{}\"", "a".repeat(64)), ""));
    assert_eq!(config.server.name.len(), 64);
}

// The switches of the features, and the keys of the file -----------------------------------------

#[test]
fn the_switches_have_documented_defaults() {
    let (config, _) = ok(&with_keys(""));
    let keys = config.keys.as_ref().unwrap();
    assert!(keys.mcp && !keys.api, "mcp is on and api is off");
    assert_eq!(keys.kinds(), [crate::keys::KeyKind::Mcp]);
    assert_eq!(line(&config, "keys.mcp").value, "true");
    assert_eq!(line(&config, "keys.mcp").source, "default");
    assert_eq!(line(&config, "keys.api").value, "false");
    assert_eq!(line(&config, "keys.api").source, "default");
    assert!(config.users.allow_creation);
    assert_eq!(line(&config, "users.allow_creation").value, "true");
    assert_eq!(line(&config, "users.allow_creation").source, "default");
    // The defaults are in the generated reference.
    let reference = keys::reference();
    for expected in [
        "| `mcp` | `true` or `false` | `true` |",
        "| `api` | `true` or `false` | `false` |",
        "| `allow_creation` | `true` or `false` | `true` |",
    ] {
        assert!(reference.contains(expected), "{expected}");
    }
    // The switches take a boolean, written without quotes.
    for bad in ["mcp = \"true\"", "api = 1", "api = \"yes\""] {
        let message = err(&with(&format!(
            "{}\n{}",
            KEYS_NEEDS.replace("keys {}", ""),
            format_args!("keys {{ {bad} }}")
        )));
        assert!(message.contains("keys."), "{bad}: {message}");
    }
    let message = err(&with("users { allow_creation = \"no\" }"));
    assert!(message.contains("users.allow_creation"), "{message}");
}

#[test]
fn both_kinds_can_be_on_and_each_kind_can_be_off() {
    let (config, _) = inbox_ok(&with(&format!(
        "{}\n{}\n{}",
        KEYS_NEEDS.replace("keys {}", ""),
        "keys { api = true }",
        INBOX.replace(
            r#"  api_key "grafana" { owner = "ci" secret_env = "INBOX_TOKEN" groups = ["infra"] }"#,
            ""
        )
    )));
    assert_eq!(
        config.keys.as_ref().unwrap().kinds(),
        [crate::keys::KeyKind::Mcp, crate::keys::KeyKind::Api]
    );
    let (config, _) = inbox_ok(&with_inbox(INBOX));
    assert_eq!(
        config.keys.as_ref().unwrap().kinds(),
        [crate::keys::KeyKind::Api],
        "api only, with no identity and no pdp"
    );
    assert!(config.identity.is_none() && config.pdp.is_none());
}

#[test]
fn the_five_contradictions_of_the_switches_stop_the_start() {
    let no_keys_block = KEYS_NEEDS.replace("keys {}", "");
    for (what, raw, expected) in [
        (
            "keys.api = true without the block inbox",
            with(&format!("{no_keys_block}\nkeys {{ api = true }}")),
            vec![
                "keys.api",
                "the block inbox is absent",
                "expected keys.api = false",
            ],
        ),
        (
            "keys.mcp = true without the block identity",
            with(
                r#"pdp { listen_address = "0.0.0.0:4182" url = "https://portal-pdp.test:4182" } keys { mcp = true }"#,
            ),
            vec![
                "keys.mcp",
                "the block identity is absent",
                "expected keys.mcp = false",
            ],
        ),
        (
            "keys.mcp = true without the block pdp",
            with(r#"identity { issuer = "i" signing_key_env = "IDENTITY_KEY" } keys {}"#),
            vec!["keys.mcp", "the block pdp is absent"],
        ),
        (
            "a block keys with both kinds off",
            with("keys { mcp = false api = false }"),
            vec!["keys.mcp is false", "keys.api is false", "both kinds off"],
        ),
        (
            "an api_key block and keys.api = false, the kind mcp on",
            format!(
                "{MINIMAL}\n{}\n{}\n{}",
                r#"access { grants_mode = "hybrid" }"#, KEYS_NEEDS, INBOX
            ),
            vec![
                "inbox.api_key",
                "needs keys.api = true",
                "keys.api is false",
            ],
        ),
        (
            "an api_key block and no block keys",
            format!("{MINIMAL}\naccess {{ grants_mode = \"hybrid\" }}\n{INBOX}"),
            vec!["inbox.api_key", "needs keys.api = true"],
        ),
        (
            "grants_mode static and users.allow_creation true",
            with(r#"access { grants_mode = "static" }"#),
            vec![
                "users.allow_creation",
                "access.grants_mode",
                "expected users.allow_creation = false",
                "\"static\"",
            ],
        ),
        (
            "grants_mode static and users.allow_creation written true",
            with(r#"access { grants_mode = "static" } users { allow_creation = true }"#),
            vec!["users.allow_creation is true"],
        ),
    ] {
        let message = inbox_err(&raw);
        for part in expected {
            assert!(message.contains(part), "{what}: {part}\n{message}");
        }
    }
    // The static mode with no creation starts.
    ok(&with(
        r#"access { grants_mode = "static" } users { allow_creation = false }"#,
    ));
    // The other modes start with creation on or off.
    for mode in ["dynamic", "hybrid"] {
        for creation in [true, false] {
            ok(&with(&format!(
                r#"access {{ grants_mode = "{mode}" }} users {{ allow_creation = {creation} }}"#
            )));
        }
    }
}

#[test]
fn a_user_block_cannot_name_a_feature_that_the_file_turns_off() {
    let user = |extra: &str, grant: &str| {
        inbox_err(&with(&format!(
            "access {{ grants_mode = \"hybrid\" }}\n{extra}\nuser \"carol\" {{ grants = [\"{grant}\"] }}"
        )))
    };
    // No inbox block: no action of the inbox.
    let message = user("", "alert.send on service:portal/inbox_group:infra");
    assert!(
        message.contains("carol") && message.contains("no action"),
        "{message}"
    );
    let message = user("", "inbox.receive on service:portal/inbox_group:infra");
    assert!(message.contains("no action"), "{message}");
    let message = user("", "phone.read on service:portal");
    assert!(message.contains("no action"), "{message}");
    // No block keys: no action of the keys.
    let message = user("", "keys.own.read on service:portal");
    assert!(message.contains("no action"), "{message}");
    // A kind that is off is not a node.
    let message = user(KEYS_NEEDS, "keys.own.use on service:portal/key_kind:api");
    assert!(
        message.contains("does not exist") && message.contains("key_kind:mcp"),
        "{message}"
    );
    // A group that does not exist is not a node.
    let message = inbox_err(&format!(
        "{}\nuser \"dan\" {{ grants = [\"alert.send on service:portal/inbox_group:nowhere\"] }}",
        with_inbox(INBOX).replace("user \"ci\"", "user \"ci2\"")
    ));
    assert!(
        message.contains("dan") && message.contains("does not exist"),
        "{message}"
    );
}

#[test]
fn a_file_key_alerts_an_account_only_with_direct_alerts_and_an_owner_that_holds_the_right() {
    let block =
        r#"api_key "grafana" { owner = "ci" secret_env = "INBOX_TOKEN" groups = ["infra"] }"#;
    let direct = r#"api_key "grafana" { owner = "ci" secret_env = "INBOX_TOKEN" groups = ["infra"] direct_alerts = true }"#;
    let only_direct = r#"api_key "grafana" { owner = "ci" secret_env = "INBOX_TOKEN" groups = [] direct_alerts = true }"#;
    let empty = r#"api_key "grafana" { owner = "ci" secret_env = "INBOX_TOKEN" groups = [] }"#;
    let with_right = |key: &str| {
        with_inbox(&INBOX.replace(block, key)).replace(
            "\"keys.own.use on service:portal/key_kind:api\",",
            "\"keys.own.use on service:portal/key_kind:api\", \"alert.send_user on service:portal\",",
        )
    };
    // The default is `false`.
    let (config, _) = inbox_ok(&with_inbox(INBOX));
    assert!(!config.inbox.as_ref().unwrap().api_keys["grafana"].direct_alerts);
    // With the right, the key can have it, with a group or with none.
    for key in [direct, only_direct] {
        let (config, _) = inbox_ok(&with_right(key));
        assert!(config.inbox.as_ref().unwrap().api_keys["grafana"].direct_alerts);
    }
    // The owner has no `alert.send_user`: the start stops and names the key and the right.
    let message = inbox_err(&with_inbox(&INBOX.replace(block, direct)));
    assert!(
        message.contains("inbox.api_key.grafana.direct_alerts")
            && message.contains("alert.send_user")
            && message.contains("\"ci\""),
        "{message}"
    );
    // The right without `direct_alerts` changes nothing: the key sends to its groups only.
    let (config, _) = inbox_ok(&with_right(block));
    assert!(!config.inbox.as_ref().unwrap().api_keys["grafana"].direct_alerts);
    // A key with no group and no direct alert can do nothing.
    let message = inbox_err(&with_right(empty));
    assert!(
        message.contains("inbox.api_key.grafana.groups") && message.contains("direct_alerts"),
        "{message}"
    );
    // The right has no node: a grant on a group is refused.
    let message = inbox_err(&with_inbox(INBOX).replace(
        "\"keys.own.use on service:portal/key_kind:api\",",
        "\"keys.own.use on service:portal/key_kind:api\", \"alert.send_user on service:portal/inbox_group:infra\",",
    ));
    assert!(message.contains("alert.send_user"), "{message}");
}

#[test]
fn an_api_key_block_follows_its_owner_and_checks_its_text_at_start() {
    let good = inbox_ok(&with_inbox(INBOX));
    assert_eq!(
        good.0.inbox.as_ref().unwrap().api_keys["grafana"].owner,
        "ci"
    );
    let block =
        r#"api_key "grafana" { owner = "ci" secret_env = "INBOX_TOKEN" groups = ["infra"] }"#;
    let changed = |to: &str| with_inbox(&INBOX.replace(block, to));
    // The owner is a login of a file account.
    let message = inbox_err(&changed(
        r#"api_key "grafana" { owner = "nobody" secret_env = "INBOX_TOKEN" groups = ["infra"] }"#,
    ));
    assert!(
        message.contains("inbox.api_key.grafana.owner") && message.contains("\"nobody\""),
        "{message}"
    );
    let message = inbox_err(&changed(
        r#"api_key "grafana" { secret_env = "INBOX_TOKEN" groups = ["infra"] }"#,
    ));
    assert!(message.contains("inbox.api_key.grafana.owner"), "{message}");
    // A superadmin login is not an account of the file.
    let message = inbox_err(&changed(
        r#"api_key "grafana" { owner = "admin" secret_env = "INBOX_TOKEN" groups = ["infra"] }"#,
    ));
    assert!(message.contains("inbox.api_key.grafana.owner"), "{message}");
    // The owner must hold the right to use the keys of the kind, and the right on each group.
    let no_use = with_inbox(INBOX).replace("\"keys.own.use on service:portal/key_kind:api\",", "");
    let message = inbox_err(&no_use);
    assert!(
        message.contains("inbox.api_key.grafana") && message.contains("keys.own.use"),
        "{message}"
    );
    let no_send =
        with_inbox(INBOX).replace("\"alert.send on service:portal/inbox_group:infra\",", "");
    let message = inbox_err(&no_send);
    assert!(
        message.contains("inbox.api_key.grafana.groups") && message.contains("alert.send"),
        "{message}"
    );
    // A group of the key that the owner may not use, with a group that it may use.
    let message = inbox_err(&changed(
        r#"api_key "grafana" { owner = "ci" secret_env = "INBOX_TOKEN" groups = ["infra", "ghost"] }"#,
    ));
    assert!(message.contains("ghost"), "{message}");
    // A secret is never in the file, and one form only.
    for (what, bad, expected) in [
        (
            "inline",
            r#"api_key "grafana" { owner = "ci" secret = "api_x" groups = ["infra"] }"#,
            "must not be written in the file",
        ),
        (
            "two forms",
            r#"api_key "grafana" { owner = "ci" secret_env = "INBOX_TOKEN" secret_file = "/x/y" groups = ["infra"] }"#,
            "exactly one",
        ),
        (
            "no form",
            r#"api_key "grafana" { owner = "ci" groups = ["infra"] }"#,
            "exactly one",
        ),
        (
            "unknown key",
            r#"api_key "grafana" { owner = "ci" secret_env = "INBOX_TOKEN" groups = ["infra"] token_env = "INBOX_TOKEN" }"#,
            "unknown configuration key: inbox.api_key.grafana.token_env",
        ),
    ] {
        let message = inbox_err(&changed(bad));
        assert!(message.contains(expected), "{what}: {message}");
    }
    // The text has the form of an API key: the error names the key, the form, and never the value.
    let texts = api_texts();
    let mcp = crate::keys::generate(crate::keys::KeyKind::Mcp)
        .unwrap()
        .to_string();
    let mut bad_sum = texts[0].clone();
    let last = bad_sum.pop().unwrap();
    bad_sum.push(if last == 'a' { 'b' } else { 'a' });
    for (what, text) in [
        ("an MCP key", mcp.clone()),
        ("a bad checksum", bad_sum.clone()),
        ("a short text", "api_short".to_string()),
        (
            "a token of 32 characters",
            "0123456789abcdef0123456789abcdef".to_string(),
        ),
        ("a reserved prefix", format!("whk_{}", &texts[0][4..])),
    ] {
        let mut vars: Vec<(&str, &str)> = VARS.iter().chain(INBOX_VARS).copied().collect();
        vars.push(("INBOX_TOKEN", text.as_str()));
        let message = parse_with(&with_inbox(INBOX), &vars)
            .err()
            .unwrap_or_else(|| panic!("{what} must stop the start"));
        assert!(
            message.contains("inbox.api_key.grafana.secret")
                && message.contains("expected an API key with the prefix api_")
                && message.contains("env:INBOX_TOKEN"),
            "{what}: {message}"
        );
        assert!(
            !message.contains(&text),
            "{what}: the value is in the message"
        );
    }
    // Two keys cannot have the same text.
    let two = with_inbox(&INBOX.replace(
        block,
        &format!(
            r#"{block}
  api_key "again" {{ owner = "ci" secret_env = "INBOX_TOKEN" groups = ["oncall"] }}"#
        ),
    ));
    let message = inbox_err(&two);
    assert!(
        message.contains("inbox.api_key.grafana.secret")
            || message.contains("inbox.api_key.again.secret"),
        "{message}"
    );
    assert!(
        message.contains("the same text") && !message.contains(&texts[0]),
        "{message}"
    );
    // The old block is gone, with no alias.
    let message = inbox_err(&changed(
        r#"producer "grafana" { token_env = "INBOX_TOKEN" groups = ["infra"] }"#,
    ));
    assert!(message.contains("producer"), "{message}");
    // The secret can come from a file, and the effective configuration shows its source.
    let file = temp_file(texts[0].as_bytes());
    let (config, _) = inbox_ok(&changed(&format!(
        r#"api_key "grafana" {{ owner = "ci" secret_file = "{}" groups = ["infra"] }}"#,
        file.display()
    )));
    let shown = line(&config, "inbox.api_key.grafana.secret");
    assert_eq!(shown.value, "<hidden>");
    assert_eq!(shown.source, format!("file:{}", file.display()));
    fs::remove_file(file).ok();
}

#[test]
fn a_block_key_of_an_account_of_the_file_holds_an_mcp_key_only() {
    let (mut vars, _) = vars_with_key();
    let api = crate::keys::generate(crate::keys::KeyKind::Api)
        .unwrap()
        .to_string();
    vars.push(("API_KEY".into(), api.clone()));
    let raw = with_keys(
        r#"user "carol" {
          grants = []
          key "svc" { key_env = "API_KEY" grants = [] }
        }"#,
    );
    let message = parse_keys(&raw, &vars).err().unwrap();
    assert!(
        message.contains("carol") && message.contains("MCP key") && message.contains("api_key"),
        "{message}"
    );
    assert!(!message.contains(&api));
    // With the kind mcp off, a block key stops the start.
    let raw = format!(
        "{MINIMAL}\n{}\n{}\n{}",
        r#"access { grants_mode = "hybrid" }"#,
        r#"users { allow_creation = true }"#,
        r#"user "carol" { grants = [] key "k" { key_env = "CAROL_KEY" grants = [] } }"#
    );
    let message = parse_keys(&raw, &vars).err().unwrap();
    assert!(message.contains("keys.mcp = true"), "{message}");
}

// The second factor: `authentication.mfa` and `authentication.totp_key`.

#[test]
fn the_second_factor_is_optional_by_default_and_needs_its_key() {
    let (cfg, secrets) = ok(MINIMAL);
    assert_eq!(cfg.authentication.mfa, MfaPolicy::Optional);
    assert_eq!(line(&cfg, "authentication.mfa").source, "default");
    assert_eq!(line(&cfg, "authentication.mfa").value, "\"optional\"");
    assert_eq!(secrets.totp_key.as_ref().unwrap().len(), 64);
    // The key is in the effective configuration with its source, and its value is hidden.
    let key = line(&cfg, "authentication.totp_key");
    assert_eq!(key.source, "env:PORTAL_TOTP_KEY");
    assert_eq!(key.value, "<hidden>");
    assert!(!cfg.render_effective().contains(TOTP_KEY));
    for (value, policy) in [
        ("off", MfaPolicy::Off),
        ("optional", MfaPolicy::Optional),
        ("required", MfaPolicy::Required),
    ] {
        let mut raw = MINIMAL.replace(
            "authentication {",
            &format!(r#"authentication {{ mfa = "{value}""#),
        );
        if value == "off" {
            // The key does not apply with `off`.
            raw = raw.replace(r#"totp_key_env            = "PORTAL_TOTP_KEY""#, "");
        }
        assert_eq!(ok(&raw).0.authentication.mfa, policy, "{value}");
    }
}

#[test]
fn a_policy_that_is_not_off_without_a_key_stops_the_start_and_says_why() {
    let no_key = MINIMAL.replace(r#"totp_key_env            = "PORTAL_TOTP_KEY""#, "");
    for policy in ["", r#"mfa = "optional""#, r#"mfa = "required""#] {
        let raw = no_key.replace("authentication {", &format!("authentication {{ {policy}"));
        let message = err(&raw);
        assert!(
            message.contains("authentication.totp_key_env")
                && message.contains("authentication.totp_key_file")
                && message.contains("authentication.mfa"),
            "{policy:?}: {message}"
        );
        assert!(
            message.contains(r#"authentication.mfa = "off""#),
            "{message}"
        );
    }
    // With `off`, the key is not needed, and it must not be written: the two keys contradict.
    let off = no_key.replace("authentication {", r#"authentication { mfa = "off""#);
    let (cfg, secrets) = ok(&off);
    assert_eq!(cfg.authentication.mfa, MfaPolicy::Off);
    assert!(secrets.totp_key.is_none());
    assert!(cfg
        .effective
        .iter()
        .all(|l| l.key != "authentication.totp_key"));
    let both = MINIMAL.replace("authentication {", r#"authentication { mfa = "off""#);
    let message = err(&both);
    assert!(
        message.contains(
            "authentication.totp_key_env and authentication.totp_key_file must not be set"
        ) && message.contains(r#"authentication.mfa is "off""#),
        "{message}"
    );
}

#[test]
fn the_key_of_the_second_factor_is_checked_at_start_and_never_shown() {
    let bad = |value: &str| {
        let message = parse_with(
            MINIMAL,
            &[
                ("PORTAL_LOGIN", "admin"),
                ("PORTAL_PASSWORD", "a password for the tests"),
                ("PORTAL_TOTP_KEY", value),
            ],
        )
        .err()
        .unwrap_or_else(|| panic!("{value:?} must be refused"));
        assert!(message.contains("authentication.totp_key"), "{message}");
        assert!(message.contains("env:PORTAL_TOTP_KEY"), "{message}");
        assert!(
            !message.contains(value) || value.is_empty(),
            "the value stays out of the error: {message}"
        );
        message
    };
    assert!(bad("0011").contains("expected 64 hexadecimal characters"));
    assert!(bad(&"zz".repeat(32)).contains("not hexadecimal"));
    assert!(bad(&"ab".repeat(32)).contains("all bytes"));
    assert!(bad(&format!("{TOTP_KEY}00")).contains("66 characters"));
    // A key from a file loses its last line end, as every secret.
    let file = temp_file(format!("{TOTP_KEY}\n").as_bytes());
    let raw = MINIMAL.replace(
        r#"totp_key_env            = "PORTAL_TOTP_KEY""#,
        &format!(r#"totp_key_file = "{}""#, file.display()),
    );
    let (cfg, _) = ok(&raw);
    assert_eq!(
        line(&cfg, "authentication.totp_key").source,
        format!("file:{}", file.display())
    );
    fs::remove_file(file).ok();
    // The key is never in clear in the file, and the other values are closed.
    let clear = MINIMAL.replace(
        r#"totp_key_env            = "PORTAL_TOTP_KEY""#,
        &format!(r#"totp_key = "{TOTP_KEY}""#),
    );
    assert!(err(&clear).contains("must not be written in the file"));
    let wrong = MINIMAL.replace("authentication {", r#"authentication { mfa = "always""#);
    let message = err(&wrong);
    assert!(
        message.contains("authentication.mfa")
            && message.contains("\"off\", \"optional\", \"required\""),
        "{message}"
    );
}

// ---------------------------------------------------------------- inbox.items and inbox.at_rest

/// The block `inbox.items` with the values of the examples of the design (all mandatory keys).
const ITEMS: &str = r#"
  items {
    max_lifetime = "90d"
    max_open_per_key = 5
    max_open_per_owner = 20
    max_open_total = 1000
    retention = "180d"
    body_purge_after = "30d"
    audit_retention = "365d"
    lease {
      min = "30s"
      max = "7d"
      grace = "2m"
      revive_window = "7d"
    }
    digest { mode = "daily" at = "08:30" timezone = "Europe/Paris" }
  }
"#;

/// The block `inbox.at_rest`: the key `k2` seals and the key `k1` only reads.
const AT_REST: &str = r#"
  at_rest {
    active_key = "k2"
    key "k1" { secret_env = "CONTENT_K1" }
    key "k2" { secret_file = "/run/secrets/content_k2" }
  }
"#;

/// The text of a content key made of `seed` and the position.
fn content_text(seed: u8) -> String {
    let bytes: Vec<u8> = (0..32u8)
        .map(|i| seed.wrapping_add(i.wrapping_mul(7)))
        .collect();
    inbox_core::b64::encode(&bytes)
}

/// The test file of the second key. It lives in the temporary directory, so the block that names it
/// is rewritten to its path.
fn content_file() -> PathBuf {
    temp_file(format!("{}\n", content_text(90)).as_bytes())
}

/// The configuration of the inbox with `items` and `at_rest` inside the block `inbox`.
fn items_config(items: &str, at_rest: &str) -> (String, PathBuf) {
    let file = content_file();
    let at_rest = at_rest.replace("/run/secrets/content_k2", &file.display().to_string());
    let inbox = INBOX.replacen(
        "  group \"infra\"",
        &format!("{items}{at_rest}  group \"infra\""),
        1,
    );
    (with_inbox(&inbox), file)
}

fn items_parse(raw: &str) -> Result<(Config, Secrets), String> {
    let text = content_text(10);
    let texts = api_texts();
    let vars: Vec<(&str, &str)> = VARS
        .iter()
        .chain(INBOX_VARS)
        .copied()
        .chain([
            ("INBOX_TOKEN", texts[0].as_str()),
            ("INBOX_TOKEN_2", texts[1].as_str()),
            ("CONTENT_K1", text.as_str()),
        ])
        .collect();
    parse_with(raw, &vars)
}

fn items_ok(items: &str, at_rest: &str) -> (Config, Secrets) {
    let (raw, file) = items_config(items, at_rest);
    let result = items_parse(&raw).unwrap_or_else(|e| panic!("{e}\n{raw}"));
    fs::remove_file(file).ok();
    result
}

fn items_err(items: &str, at_rest: &str) -> String {
    let (raw, file) = items_config(items, at_rest);
    let result = items_parse(&raw);
    fs::remove_file(file).ok();
    match result {
        Ok(_) => panic!("must be rejected:\n{raw}"),
        Err(e) => e,
    }
}

#[test]
fn the_blocks_items_and_at_rest_give_typed_values_with_documented_defaults() {
    let (config, secrets) = items_ok(ITEMS, AT_REST);
    let inbox = config.inbox.as_ref().unwrap();
    let items = inbox.items.as_ref().unwrap();
    let day = |n: u64| Duration::from_secs(n * 86_400);
    assert_eq!(items.max_lifetime, day(90));
    assert_eq!(
        (
            items.max_open_per_key,
            items.max_open_per_owner,
            items.max_open_total
        ),
        (5, 20, 1000)
    );
    assert_eq!(
        (
            items.retention,
            items.body_purge_after,
            items.audit_retention
        ),
        (day(180), day(30), day(365))
    );
    assert_eq!(items.lease.min, Duration::from_secs(30));
    assert_eq!(items.lease.max, day(7));
    assert_eq!(items.lease.grace, Duration::from_secs(120));
    assert_eq!(items.lease.revive_window, day(7));
    // The documented defaults.
    assert_eq!(items.lease.max_revivals, 3);
    assert_eq!((items.max_chain, items.max_turns), (10, 40));
    assert_eq!(items.stale_after, Duration::from_secs(24 * 3600));
    assert_eq!(items.max_body_bytes, 16 * 1024);
    assert_eq!(items.wait_max, Duration::from_secs(25));
    assert_eq!(
        items.digest,
        DigestConfig::Daily {
            hour: 8,
            minute: 30,
            timezone: "Europe/Paris".into()
        }
    );
    let at_rest = inbox.at_rest.as_ref().unwrap();
    assert_eq!(at_rest.active_key, "k2");
    assert_eq!(at_rest.keys, ["k1", "k2"]);
    assert!(!at_rest.admin_body_read, "the default is false");
    let secrets = secrets.inbox.as_ref().unwrap();
    assert_eq!(secrets.content_keys.len(), 2);
    assert_eq!(secrets.content_keys[0].0, "k1");
    assert_eq!(
        secrets.content_keys[0].1.as_bytes(),
        content_text(10).as_bytes()
    );
    // The other modes of the digest.
    let off = ITEMS.replace(
        r#"digest { mode = "daily" at = "08:30" timezone = "Europe/Paris" }"#,
        r#"digest { mode = "off" }"#,
    );
    let (config, _) = items_ok(&off, AT_REST);
    assert_eq!(
        config.inbox.unwrap().items.unwrap().digest,
        DigestConfig::Off
    );
    let every = ITEMS.replace(
        r#"digest { mode = "daily" at = "08:30" timezone = "Europe/Paris" }"#,
        r#"digest { mode = "every" every = "6h" }"#,
    );
    let (config, _) = items_ok(&every, AT_REST);
    assert_eq!(
        config.inbox.unwrap().items.unwrap().digest,
        DigestConfig::Every(Duration::from_secs(6 * 3600))
    );
}

#[test]
fn print_config_masks_secret_shows_kid_and_source() {
    let (config, _) = items_ok(ITEMS, AT_REST);
    let shown = config.render_effective();
    let file_line = shown
        .lines()
        .find(|l| l.starts_with("inbox.at_rest.key.k2.secret"))
        .unwrap();
    assert!(
        file_line.contains("<hidden>") && file_line.contains("# file:/"),
        "{file_line}"
    );
    let env_line = shown
        .lines()
        .find(|l| l.starts_with("inbox.at_rest.key.k1.secret"))
        .unwrap();
    assert!(
        env_line.contains("<hidden>") && env_line.contains("# env:CONTENT_K1"),
        "{env_line}"
    );
    assert!(
        shown.contains("inbox.at_rest.key.k2.use = \"active\""),
        "{shown}"
    );
    assert!(
        shown.contains("inbox.at_rest.key.k1.use = \"read-only\""),
        "{shown}"
    );
    assert!(
        shown.contains("inbox.at_rest.active_key = \"k2\"  # file"),
        "{shown}"
    );
    assert!(
        shown.contains("inbox.items.lease.max_revivals = 3  # default"),
        "{shown}"
    );
    assert!(
        shown.contains("inbox.items.digest.at = \"08:30\"  # file"),
        "{shown}"
    );
    // No line holds the text of a key.
    for seed in [10u8, 90] {
        assert!(!shown.contains(&content_text(seed)), "the secret is hidden");
    }
}

#[test]
fn no_default_for_mandatory_keys_message_names_key_range_value() {
    // Each mandatory key of the two blocks stops the start when it is missing.
    for (line, key, range) in [
        (
            "    max_lifetime = \"90d\"\n",
            "inbox.items.max_lifetime",
            "from 1d to 365d",
        ),
        (
            "    max_open_per_key = 5\n",
            "inbox.items.max_open_per_key",
            "from 1 to 1000",
        ),
        (
            "    max_open_per_owner = 20\n",
            "inbox.items.max_open_per_owner",
            "from 1 to 10000",
        ),
        (
            "    max_open_total = 1000\n",
            "inbox.items.max_open_total",
            "from 1 to 1000000",
        ),
        (
            "    retention = \"180d\"\n",
            "inbox.items.retention",
            "from 1d to 3650d",
        ),
        (
            "    body_purge_after = \"30d\"\n",
            "inbox.items.body_purge_after",
            "from 0s to 3650d",
        ),
        (
            "    audit_retention = \"365d\"\n",
            "inbox.items.audit_retention",
            "from 1d to 3650d",
        ),
        (
            "      min = \"30s\"\n",
            "inbox.items.lease.min",
            "from 5s to 1h",
        ),
        (
            "      max = \"7d\"\n",
            "inbox.items.lease.max",
            "from 5s to 30d",
        ),
        (
            "      grace = \"2m\"\n",
            "inbox.items.lease.grace",
            "from 0s to 1h",
        ),
        (
            "      revive_window = \"7d\"\n",
            "inbox.items.lease.revive_window",
            "from 0s to 365d",
        ),
    ] {
        assert!(ITEMS.contains(line), "{line}");
        let message = items_err(&ITEMS.replacen(line, "", 1), AT_REST);
        assert!(
            message.contains("missing required configuration key")
                && message.contains(key)
                && message.contains(range),
            "{key}: {message}"
        );
    }
    let message = items_err(
        &ITEMS.replace(
            r#"digest { mode = "daily" at = "08:30" timezone = "Europe/Paris" }"#,
            "",
        ),
        AT_REST,
    );
    assert!(message.contains("inbox.items.digest.mode"), "{message}");
    let message = items_err(ITEMS, &AT_REST.replace("    active_key = \"k2\"\n", ""));
    assert!(
        message.contains("missing required configuration key: inbox.at_rest.active_key")
            && message.contains("1 to 16 characters"),
        "{message}"
    );
    // A written value outside the range names the key, the range and the value.
    let message = items_err(
        &ITEMS.replace("max_open_per_key = 5", "max_open_per_key = 1001"),
        AT_REST,
    );
    assert!(
        message.contains("inbox.items.max_open_per_key")
            && message.contains("from 1 to 1000")
            && message.contains("1001"),
        "{message}"
    );
    // A value is never changed to fit: a number in quotes, a duration without unit.
    for (from, to, key) in [
        (
            "max_open_per_key = 5",
            "max_open_per_key = \"5\"",
            "inbox.items.max_open_per_key",
        ),
        (
            "retention = \"180d\"",
            "retention = \"180\"",
            "inbox.items.retention",
        ),
        (
            "max_lifetime = \"90d\"",
            "max_lifetime = \"400d\"",
            "inbox.items.max_lifetime",
        ),
    ] {
        let message = items_err(&ITEMS.replacen(from, to, 1), AT_REST);
        assert!(
            message.contains(key) && message.contains(to.split(" = ").nth(1).unwrap()),
            "{message}"
        );
    }
    // The optional keys have their range too.
    for (extra, key) in [
        ("max_chain = 0", "inbox.items.max_chain"),
        ("max_chain = 51", "inbox.items.max_chain"),
        ("max_turns = 1", "inbox.items.max_turns"),
        ("stale_after = \"30s\"", "inbox.items.stale_after"),
        ("max_body_bytes = \"512B\"", "inbox.items.max_body_bytes"),
        ("max_body_bytes = \"33KiB\"", "inbox.items.max_body_bytes"),
        ("wait_max = \"61s\"", "inbox.items.wait_max"),
    ] {
        let message = items_err(
            &ITEMS.replacen("  items {", &format!("  items {{\n    {extra}"), 1),
            AT_REST,
        );
        assert!(
            message.contains(key) && message.contains("expected"),
            "{extra}: {message}"
        );
    }
}

#[test]
fn a_contradiction_of_the_items_stops_the_start_with_the_two_keys_and_values() {
    for (from, to, expected) in [
        // lease.min > lease.max
        ("max = \"7d\"", "max = \"20s\"", "inbox.items.lease.min must be at most inbox.items.lease.max: min is 30s, max is 20s"),
        // revive_window > max_lifetime
        ("revive_window = \"7d\"", "revive_window = \"100d\"", "inbox.items.lease.revive_window must be at most inbox.items.max_lifetime: revive_window is 100d, max_lifetime is 90d"),
        // retention < max_lifetime + revive_window
        ("retention = \"180d\"", "retention = \"96d\"", "inbox.items.retention must be at least inbox.items.max_lifetime plus inbox.items.lease.revive_window: retention is 96d, max_lifetime is 90d, revive_window is 7d"),
        // audit_retention < retention
        ("audit_retention = \"365d\"", "audit_retention = \"179d\"", "inbox.items.audit_retention must be at least inbox.items.retention: audit_retention is 179d, retention is 180d"),
        // body_purge_after > retention
        ("body_purge_after = \"30d\"", "body_purge_after = \"181d\"", "inbox.items.body_purge_after must be at most inbox.items.retention: body_purge_after is 181d, retention is 180d"),
    ] {
        let message = items_err(&ITEMS.replacen(from, to, 1), AT_REST);
        assert!(message.contains(expected), "{to}: {message}");
    }
    // lease.max > max_lifetime: the table allows 30d for the lease, so lower max_lifetime.
    let short_life = ITEMS
        .replacen("max_lifetime = \"90d\"", "max_lifetime = \"20d\"", 1)
        .replacen("max = \"7d\"", "max = \"30d\"", 1);
    let message = items_err(&short_life, AT_REST);
    assert!(
        message.contains("inbox.items.lease.max must be at most inbox.items.max_lifetime: lease.max is 30d, max_lifetime is 20d"),
        "{message}"
    );
    // A limit at the border is valid.
    items_ok(
        &ITEMS.replacen("retention = \"180d\"", "retention = \"97d\"", 1),
        AT_REST,
    );
    items_ok(
        &ITEMS.replacen(
            "body_purge_after = \"30d\"",
            "body_purge_after = \"180d\"",
            1,
        ),
        AT_REST,
    );
    items_ok(&ITEMS.replacen("min = \"30s\"", "min = \"1h\"", 1), AT_REST);
    items_ok(&ITEMS.replacen("max = \"7d\"", "max = \"30s\"", 1), AT_REST);
}

#[test]
fn the_digest_mode_decides_which_keys_apply() {
    let daily = r#"digest { mode = "daily" at = "08:30" timezone = "Europe/Paris" }"#;
    for (digest, expected) in [
        (r#"digest { mode = "daily" timezone = "UTC" }"#, "inbox.items.digest.at"),
        (r#"digest { mode = "daily" at = "08:30" }"#, "inbox.items.digest.timezone"),
        (r#"digest { mode = "every" }"#, "inbox.items.digest.every"),
        (r#"digest { mode = "off" at = "08:30" }"#, "inbox.items.digest.at must not be set when inbox.items.digest.mode is not \"daily\""),
        (r#"digest { mode = "every" every = "6h" timezone = "UTC" }"#, "inbox.items.digest.timezone must not be set"),
        (r#"digest { mode = "daily" at = "08:30" timezone = "UTC" every = "6h" }"#, "inbox.items.digest.every must not be set when inbox.items.digest.mode is not \"every\""),
        (r#"digest { mode = "daily" at = "24:00" timezone = "UTC" }"#, "inbox.items.digest.at"),
        (r#"digest { mode = "daily" at = "8:30" timezone = "UTC" }"#, "HH:MM"),
        (r#"digest { mode = "daily" at = "08:30" timezone = "Paris time" }"#, "inbox.items.digest.timezone"),
        (r#"digest { mode = "every" every = "30m" }"#, "from 1h to 7d"),
        (r#"digest { mode = "weekly" }"#, "inbox.items.digest.mode"),
    ] {
        let message = items_err(&ITEMS.replacen(daily, digest, 1), AT_REST);
        assert!(message.contains(expected), "{digest}: {message}");
    }
}

#[test]
fn the_blocks_items_and_at_rest_need_each_other() {
    let message = items_err(ITEMS, "");
    assert!(
        message.contains("the block inbox.items needs the block inbox.at_rest"),
        "{message}"
    );
    let message = items_err("", AT_REST);
    assert!(
        message.contains("the block inbox.at_rest needs the block inbox.items"),
        "{message}"
    );
    // Without both blocks, the inbox runs as before.
    let (config, secrets) = inbox_ok(&with_inbox(INBOX));
    let inbox = config.inbox.unwrap();
    assert!(inbox.items.is_none() && inbox.at_rest.is_none());
    assert!(secrets.inbox.unwrap().content_keys.is_empty());
}

#[test]
fn plain_secret_in_file_stops_start() {
    let plain = AT_REST.replace(
        r#"key "k1" { secret_env = "CONTENT_K1" }"#,
        &format!(r#"key "k1" {{ secret = "{}" }}"#, content_text(10)),
    );
    let message = items_err(ITEMS, &plain);
    assert!(
        message.contains("inbox.at_rest.key.k1.secret must not be written in the file")
            && message.contains("secret_env")
            && message.contains("secret_file"),
        "{message}"
    );
    assert!(
        !message.contains(&content_text(10)),
        "the error never shows the secret"
    );
    // Two forms, and no form, stop the start too.
    let two = AT_REST.replace(
        r#"key "k1" { secret_env = "CONTENT_K1" }"#,
        r#"key "k1" { secret_env = "CONTENT_K1" secret_file = "/run/secrets/x" }"#,
    );
    assert!(items_err(ITEMS, &two).contains("exactly one of inbox.at_rest.key.k1.secret_env"));
    let none = AT_REST.replace(
        r#"key "k1" { secret_env = "CONTENT_K1" }"#,
        r#"key "k1" { }"#,
    );
    let message = items_err(ITEMS, &none);
    assert!(
        message.contains("missing required configuration key")
            && message.contains("inbox.at_rest.key.k1.secret"),
        "{message}"
    );
    // A variable that does not exist, an empty variable and a file that does not exist.
    let missing = AT_REST.replace("CONTENT_K1", "CONTENT_NOT_SET");
    let message = items_err(ITEMS, &missing);
    assert!(
        message.contains("inbox.at_rest.key.k1.secret_env") && message.contains("CONTENT_NOT_SET"),
        "{message}"
    );
    let nofile = AT_REST.replace("/run/secrets/content_k2", "/nonexistent/content_k2");
    let (raw, file) = items_config(ITEMS, &nofile);
    let message = items_parse(&raw).err().unwrap();
    fs::remove_file(file).ok();
    assert!(
        message.contains("inbox.at_rest.key.k2.secret_file")
            && message.contains("/nonexistent/content_k2"),
        "{message}"
    );
}

#[test]
fn active_key_unknown_stops_start() {
    let message = items_err(
        ITEMS,
        &AT_REST.replace("active_key = \"k2\"", "active_key = \"k9\""),
    );
    assert!(
        message.contains("inbox.at_rest.active_key")
            && message.contains("\"k9\"")
            && message.contains("\"k1\", \"k2\""),
        "{message}"
    );
    // The form of the id: 1 to 16 characters of a-z, 0-9, '-' and '_'.
    for bad in ["", "K1", "a-very-long-key-id-of-more-than-16", "k 1", "é"] {
        let message = items_err(
            ITEMS,
            &AT_REST.replace("active_key = \"k2\"", &format!("active_key = {bad:?}")),
        );
        assert!(
            message.contains("inbox.at_rest.active_key"),
            "{bad:?}: {message}"
        );
        let label = AT_REST.replace(r#"key "k1""#, &format!("key {bad:?}"));
        let message = items_err(ITEMS, &label);
        assert!(
            message.contains("key id") || message.contains("inbox.at_rest.key"),
            "{bad:?}: {message}"
        );
    }
    // At least one and at most eight keys.
    let none = r#"
  at_rest {
    active_key = "k1"
  }
"#;
    assert!(items_err(ITEMS, none).contains("1 to 8 blocks key"));
    let many: String = (1..=9)
        .map(|i| format!("    key \"k{i}\" {{ secret_env = \"CONTENT_K1\" }}\n"))
        .collect();
    let nine = format!("  at_rest {{\n    active_key = \"k1\"\n{many}  }}\n");
    assert!(items_err(ITEMS, &nine).contains("1 to 8 blocks key"));
}

#[test]
fn duplicate_key_id_or_secret_stops_start() {
    // The reader refuses a second block with the same id.
    let two = AT_REST.replace(
        r#"key "k2" { secret_file = "/run/secrets/content_k2" }"#,
        r#"key "k1" { secret_file = "/run/secrets/content_k2" }"#,
    );
    let message = items_err(ITEMS, &two);
    assert!(
        message.contains("duplicate configuration block") && message.contains("k1"),
        "{message}"
    );
    // The schema of the block key is closed.
    let other = AT_REST.replace(
        r#"{ secret_env = "CONTENT_K1" }"#,
        r#"{ secret_env = "CONTENT_K1" label = "x" }"#,
    );
    assert!(
        items_err(ITEMS, &other).contains("unknown configuration key: inbox.at_rest.key.k1.label")
    );
    // A key block outside the block at_rest, and a block in a block that does not hold it.
    let outside = INBOX.replacen(
        "  group \"infra\"",
        "  key \"k1\" { secret_env = \"CONTENT_K1\" }\n  group \"infra\"",
        1,
    );
    assert!(items_parse(&with_inbox(&outside)).is_err());
    let nested = ITEMS.replacen(
        "  items {",
        "  items {\n    key \"k1\" { secret_env = \"CONTENT_K1\" }",
        1,
    );
    assert!(items_err(&nested, AT_REST).contains("nested HCL blocks are not supported"));
    // Two keys with the same text are refused where the program decodes them (see the module state).
}
