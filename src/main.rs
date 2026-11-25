#![forbid(unsafe_code)]

mod access;
mod actions;
mod bootstrap;
mod catalog;
mod config;
mod counting;
mod crypto;
mod decision;
mod discovery;
mod grants;
mod http;
mod icons;
mod identity;
mod inbox;
mod key_forms;
mod keyauth;
mod keys;
mod keys_metrics;
mod limits;
mod metrics;
mod mfa;
mod options;
mod portal_actions;
mod rate_limit;
mod secretbox;
mod session;
mod shutdown;
mod store;
mod subject_id;
mod sync;
mod totp;
mod tracking;
mod transport;
mod ui;
mod users;
mod util;
mod watch;

use std::env;
use std::io::{self, Read};
use std::net::{IpAddr, SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;

use config::{Config, Secrets, StorageBackend};
use crypto::{generate_session_secret, hash_password};
use http::{AccessSecrets, SuperAdmin};
use store::Store;
use tracking::Tracker;
use zeroize::Zeroize;

fn main() {
    if let Err(e) = real_main() {
        eprintln!("portal: {e}");
        std::process::exit(1);
    }
}

fn real_main() -> Result<(), String> {
    let mut args = env::args().skip(1);
    let command = args.next().unwrap_or_else(|| "help".into());
    match command.as_str() {
        "run" => {
            let config_path = parse_config_arg(&args.collect::<Vec<_>>())?;
            let (config, mut secrets) = Config::load(&config_path)?;
            inbox::log::set_level(config.logging.level);
            let signing_secret = take_session_secret(&mut secrets)?;
            let superadmin = load_superadmin(&secrets, &signing_secret)?;
            // With a replica, the inbox waits for its lease and restores the SQLite file here,
            // before the portal opens the file. A signal ends the wait.
            let terminated = config
                .inbox
                .as_ref()
                .is_some_and(|inbox| inbox.replica.is_some())
                .then(shutdown::terminated_flag);
            let is_terminated = || {
                terminated
                    .as_ref()
                    .is_some_and(|t| t.load(Ordering::SeqCst))
            };
            let terminated_now = terminated.clone();
            let booted = match inbox::boot(&config, &mut secrets, &is_terminated)? {
                inbox::Boot::Off => None,
                inbox::Boot::Stopped => return Ok(()),
                inbox::Boot::Ready(booted) => Some(booted),
            };
            let store = Arc::new(open_store(&config, &secrets)?);
            // A command that writes to the SQLite file reads this mark.
            if let StorageBackend::Sqlite(path) = &config.storage.backend {
                store::mark_sqlite_instance(path)?;
            }
            // The inbox opens its storage on the same database.
            let inbox_start = match booted {
                Some(booted) => {
                    let password = secrets
                        .postgres_password
                        .as_ref()
                        .map(|p| {
                            std::str::from_utf8(p.as_bytes())
                                .map(|text| zeroize::Zeroizing::new(text.to_string()))
                                .map_err(|_| "postgres.password is not valid UTF-8".to_string())
                        })
                        .transpose()?;
                    Some(booted.start(&config, password)?)
                }
                None => None,
            };
            let access = AccessSecrets {
                identity_key: secrets.identity_key.take(),
                pdp_tokens: std::mem::take(&mut secrets.pdp_tokens),
                totp_key: secrets.totp_key.take(),
            };
            // The program erases the secrets that it still holds from memory here.
            drop(secrets);
            let tracker = Tracker::start(
                Arc::clone(&store),
                tracking::FLUSH_EVERY,
                config.storage.retention_days,
            )?;
            let state = Arc::new(http::AppState::new(
                config,
                superadmin,
                signing_secret,
                store,
                tracker,
                access,
            )?);
            sync::start(Arc::clone(&state))?;
            let lifecycle = match inbox_start {
                Some((init, lifecycle)) => {
                    state.inbox.prepare(init);
                    Some(lifecycle)
                }
                None => None,
            };
            if is_terminated() {
                return Ok(());
            }
            // The process stops when a signal comes, and when the inbox loses its replica lease.
            let fence = lifecycle.as_ref().map(inbox::Lifecycle::fence);
            let also_stop = async move {
                tokio::select! {
                    _ = async {
                        match &fence {
                            Some(fence) => fence.wait().await,
                            None => std::future::pending().await,
                        }
                    } => {}
                    _ = async {
                        // Only an inbox with a replica has the flag of the signals.
                        match &terminated_now {
                            Some(flag) => {
                                while !flag.load(Ordering::SeqCst) {
                                    tokio::time::sleep(Duration::from_millis(100)).await;
                                }
                            }
                            None => std::future::pending().await,
                        }
                    } => {}
                }
            };
            let served = http::serve(state, also_stop);
            // The runtime is gone. The last snapshot holds all the data of the file.
            let finished = lifecycle.map(inbox::Lifecycle::finish).transpose();
            served?;
            finished?;
            Ok(())
        }
        // Copies the accounts and the history of a SQLite file into the (empty) PostgreSQL database.
        // The configuration names the database. The file is only read.
        "import-sqlite" => {
            let (config_path, from) = parse_import_args(&args.collect::<Vec<_>>())?;
            let (config, secrets) = Config::load(&config_path)?;
            if !matches!(config.storage.backend, StorageBackend::Postgres(_)) {
                return Err("import-sqlite needs a configuration with a postgres block".into());
            }
            let store = open_store(&config, &secrets)?;
            let report = store.import_sqlite(&from)?;
            println!(
                "imported {} accounts, {} keys, {} revoked keys, {} external services, {} personal links, {} sessions, {} service accesses, {} audit events, {} sign-in attempts",
                report.users,
                report.keys,
                report.key_history,
                report.externals,
                report.user_links,
                report.sessions,
                report.accesses,
                report.audit,
                report.logins
            );
            Ok(())
        }
        // The incident command. It ends every session of the shared database, the superadmin sessions too.
        // A running portal stops accepting a session within 5 s. With a SQLite file the command fails.
        "end-sessions" => {
            let config_path = parse_config_arg(&args.collect::<Vec<_>>())?;
            let (config, secrets) = Config::load(&config_path)?;
            require_shared_sessions(&config)?;
            let store = open_store(&config, &secrets)?;
            let ended = store.end_all_sessions(tracking::unix_now())?;
            println!("ended {ended} sessions");
            Ok(())
        }
        // Revokes the keys of the database of one user, or all the keys of the database (portal
        // 5.6). It makes no key and does not revoke a key of the file. A running portal takes the
        // change within one round of the directory. With a SQLite file, the command writes only when
        // the portal does not run.
        "revoke-keys" => {
            let (config_path, who) = parse_revoke_args(&args.collect::<Vec<_>>())?;
            let (config, secrets) = Config::load(&config_path)?;
            if let StorageBackend::Sqlite(path) = &config.storage.backend {
                if store::sqlite_instance_running(path) {
                    return Err(format!(
                        "the portal runs and holds the SQLite file {}: stop it first, or use PostgreSQL",
                        path.display()
                    ));
                }
            }
            let store = open_store(&config, &secrets)?;
            let revoked = store.revoke_keys(who.as_deref(), tracking::unix_now())?;
            println!("revoked {revoked} keys");
            Ok(())
        }
        // Removes the second factor of one account, or of every account and the check value of the key. This is the
        // way back for the superadmin, who has no account above it, and for a new `authentication.totp_key`. With a
        // SQLite file, the command writes only when the portal does not run. With PostgreSQL, it ends the sessions of
        // the account too. The command writes one audit event, with the actor `cli`.
        "reset-two-factor" => {
            let (config_path, who) = parse_revoke_args(&args.collect::<Vec<_>>())?;
            let (config, secrets) = Config::load(&config_path)?;
            if let StorageBackend::Sqlite(path) = &config.storage.backend {
                if store::sqlite_instance_running(path) {
                    return Err(format!(
                        "the portal runs and holds the SQLite file {}: stop it first, or use PostgreSQL",
                        path.display()
                    ));
                }
            }
            let store = open_store(&config, &secrets)?;
            let removed = match &who {
                Some(login) => {
                    let removed = usize::from(store.totp_remove(login)?);
                    if removed > 0 && store.is_shared() {
                        // The sessions of an account of the file, and of the superadmin, are not in the list of a user.
                        let _ = store.sessions_delete_user(login);
                    }
                    removed
                }
                None => store.totp_remove_all()?,
            };
            if removed > 0 {
                let mut batch = tracking::Batch::default();
                batch
                    .events
                    .push(tracking::Event::Audit(tracking::AuditEvent {
                        kind: tracking::AuditKind::TotpReset,
                        login: who.clone(),
                        actor: Some("cli".into()),
                        ip: None,
                        user_agent: "portal reset-two-factor".into(),
                        at: tracking::unix_now(),
                    }));
                store.apply(&batch)?;
            }
            println!("removed the second factor of {removed} accounts");
            Ok(())
        }
        // Check the file, the variables and the files that it names.
        // Then print the value that applies to each key, with its source.
        // The secrets are hidden.
        "check-config" => {
            let config_path = parse_config_arg(&args.collect::<Vec<_>>())?;
            let (config, signer) = load_and_check(&config_path)?;
            print!("{}", config.render_effective());
            if let Some(signer) = signer {
                println!(
                    "identity.key_id = {:?}  # derived from identity.signing_key",
                    signer.kid()
                );
            }
            println!("configuration ok");
            Ok(())
        }
        // The same checks as `check-config`. The output is only the effective configuration, one
        // line for each key, so a script can read it.
        "print-config" => {
            let config_path = parse_config_arg(&args.collect::<Vec<_>>())?;
            let (config, _signer) = load_and_check(&config_path)?;
            print!("{}", config.render_effective());
            Ok(())
        }
        // The public key of the identity tokens. Each service uses it in `identity.public_keys_file`.
        "identity-public-key" => {
            let config_path = parse_config_arg(&args.collect::<Vec<_>>())?;
            let (config, mut secrets) = Config::load(&config_path)?;
            let signer = identity_signer(&config, &mut secrets)?
                .ok_or("the configuration has no identity block")?;
            print!("{}", signer.jwk_set());
            Ok(())
        }
        // The configuration reference in Markdown. It is generated from the table of keys.
        "config-reference" => {
            if args.next().is_some() {
                return Err("config-reference takes no argument".into());
            }
            print!("{}", config::keys::reference());
            Ok(())
        }
        "healthcheck" => {
            let config_path = parse_config_arg(&args.collect::<Vec<_>>())?;
            healthcheck(&config_path)
        }
        "hash-password" => hash_password_command(),
        // The commands of the inbox: keys, secrets, import and health check.
        "inbox" => inbox::cli::run(&args.collect::<Vec<_>>()),
        // Prints a synthetic page on stdout. Use it to look at the design without a runtime or a login.
        "render-demo" => {
            let icons = match args.next() {
                Some(dir) if dir != "-" => icons::Icons::load(Path::new(&dir)),
                _ => icons::Icons::empty(),
            };
            let name = args.next().unwrap_or_else(|| "launcher".into());
            let page = ui::demo_page(&name, &icons)
                .ok_or_else(|| format!("unknown page {name}: {}", ui::DEMO_PAGES.join(", ")))?;
            print!("{page}");
            Ok(())
        }
        "version" | "--version" | "-V" => {
            println!("portal {}", release_version());
            Ok(())
        }
        "help" | "--help" | "-h" => {
            print_help();
            Ok(())
        }
        _ => Err(format!("unknown command: {command}")),
    }
}

fn release_version() -> &'static str {
    option_env!("PORTAL_RELEASE_VERSION").unwrap_or(env!("CARGO_PKG_VERSION"))
}

/// One option of a command of this file. The commands under `portal inbox` have their own parser.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Opt {
    /// `--config FILE`, `--config=FILE` or `-c FILE`.
    Config,
    /// `--user LOGIN` or `--user=LOGIN`.
    User,
    /// `--all`, without a value.
    All,
    /// `--from FILE` or `--from=FILE`.
    From,
}

impl Opt {
    fn name(self) -> &'static str {
        match self {
            Opt::Config => "--config",
            Opt::User => "--user",
            Opt::All => "--all",
            Opt::From => "--from",
        }
    }
}

/// The options that a command line gives. Each option comes at most one time.
#[derive(Debug, Default, PartialEq, Eq)]
struct Options {
    config: Option<PathBuf>,
    user: Option<String>,
    from: Option<PathBuf>,
    all: bool,
}

/// Reads the options of a command, in any order. `accepts` lists the options of the command.
///
/// An option with a value accepts `--name VALUE` and `--name=VALUE`. `--config` also accepts
/// `-c VALUE`. The value is used as it is, also when it starts with `-`. An unknown option, a
/// repeated option, an option without its value and a free argument give an error. The error
/// ends with `usage`.
fn parse_options(args: &[String], accepts: &[Opt], usage: &str) -> Result<Options, String> {
    let fail = |problem: String| format!("{problem}; expected: {usage}");
    let mut options = Options::default();
    let mut seen: Vec<Opt> = Vec::new();
    let mut rest = args.iter();
    while let Some(arg) = rest.next() {
        let (name, inline) = match arg.split_once('=') {
            Some((name, value)) if name.starts_with("--") => (name, Some(value)),
            _ => (arg.as_str(), None),
        };
        let opt = match name {
            "--config" | "-c" => Opt::Config,
            "--user" => Opt::User,
            "--all" => Opt::All,
            "--from" => Opt::From,
            _ if name.starts_with('-') => return Err(fail(format!("unknown option {name}"))),
            _ => return Err(fail(format!("unexpected argument {arg}"))),
        };
        if !accepts.contains(&opt) {
            return Err(fail(format!("unknown option {name}")));
        }
        if seen.contains(&opt) {
            return Err(fail(format!(
                "the option {} is given two times",
                opt.name()
            )));
        }
        seen.push(opt);
        if opt == Opt::All {
            if inline.is_some() {
                return Err(fail("the option --all takes no value".into()));
            }
            options.all = true;
            continue;
        }
        let value = match inline {
            Some(value) => value.to_string(),
            None => rest
                .next()
                .ok_or_else(|| fail(format!("the option {} needs a value", opt.name())))?
                .clone(),
        };
        match opt {
            Opt::Config if value.is_empty() => {
                return Err(fail("the value of --config is empty".into()))
            }
            Opt::Config => options.config = Some(PathBuf::from(value)),
            Opt::User => options.user = Some(value),
            Opt::From => options.from = Some(PathBuf::from(value)),
            Opt::All => {}
        }
    }
    Ok(options)
}

/// The arguments of a command that reads only the configuration: `--config <FILE>`.
fn parse_config_arg(args: &[String]) -> Result<PathBuf, String> {
    const USAGE: &str = "--config <FILE>";
    parse_options(args, &[Opt::Config], USAGE)?
        .config
        .ok_or_else(|| format!("expected: {USAGE}"))
}

/// The arguments of `revoke-keys` and `reset-two-factor`: `--config <FILE>` and `--user <LOGIN>`
/// or `--all`. The result is the login, or `None` for all the accounts.
fn parse_revoke_args(args: &[String]) -> Result<(PathBuf, Option<String>), String> {
    const USAGE: &str = "--config <FILE> --user <LOGIN>, or --config <FILE> --all";
    let options = parse_options(args, &[Opt::Config, Opt::User, Opt::All], USAGE)?;
    match (options.config, options.user, options.all) {
        (Some(config), Some(login), false) => Ok((config, Some(login))),
        (Some(config), None, true) => Ok((config, None)),
        _ => Err(format!("expected: {USAGE}")),
    }
}

/// The arguments of the import command: `--config <FILE> --from <SQLITE FILE>`.
fn parse_import_args(args: &[String]) -> Result<(PathBuf, PathBuf), String> {
    const USAGE: &str = "--config <FILE> --from <SQLITE FILE>";
    let options = parse_options(args, &[Opt::Config, Opt::From], USAGE)?;
    match (options.config, options.from) {
        (Some(config), Some(from)) => Ok((config, from)),
        _ => Err(format!("expected: {USAGE}")),
    }
}

/// Loads the file as `run` does, and checks each secret that it names. `check-config` and
/// `print-config` use it. The result has the signer of the identity tokens, when the file has the
/// block `identity`.
fn load_and_check(path: &Path) -> Result<(Config, Option<identity::Signer>), String> {
    let (config, mut secrets) = Config::load(path)?;
    let mut signing_secret = take_session_secret(&mut secrets)?;
    let mut superadmin = load_superadmin(&secrets, &signing_secret)?;
    superadmin.password_hash.zeroize();
    signing_secret.zeroize();
    // The key must be a valid Ed25519 key. The check reads it as `run` does.
    let signer = identity_signer(&config, &mut secrets)?;
    // The secrets of the inbox must decode: the key and the tokens.
    inbox::check_secrets(&config, &mut secrets)?;
    Ok((config, signer))
}

/// Opens the database that the configuration names. With PostgreSQL, this waits up to a minute for the database to answer.
/// The command `end-sessions` works on the shared database only. With a SQLite file the sessions
/// are in the memory of the portal process, and no other process can end them.
fn require_shared_sessions(config: &Config) -> Result<(), String> {
    match config.storage.backend {
        StorageBackend::Postgres(_) => Ok(()),
        StorageBackend::Sqlite(_) => Err("end-sessions needs a configuration with a postgres block: with a SQLite file the sessions are in the memory of the portal process, so restart the portal to end them".into()),
    }
}

fn open_store(config: &Config, secrets: &Secrets) -> Result<Store, String> {
    match &config.storage.backend {
        StorageBackend::Sqlite(path) => Store::open(path),
        StorageBackend::Postgres(postgres) => {
            let password = secrets
                .postgres_password
                .as_ref()
                .ok_or("internal error: the postgres block has no password")?;
            let password = std::str::from_utf8(password.as_bytes())
                .map_err(|_| "postgres.password is not valid UTF-8".to_string())?;
            Store::open_postgres(
                postgres,
                password,
                config
                    .pdp
                    .as_ref()
                    .map_or(store::pg::DEFAULT_COUNTER_CONNECTIONS, |pdp| {
                        pdp.counter_connections
                    }),
            )
        }
    }
}

/// Creates the signer of the identity tokens from the `identity` block. The result is `None` without the block.
fn identity_signer(
    config: &Config,
    secrets: &mut Secrets,
) -> Result<Option<identity::Signer>, String> {
    match (&config.identity, secrets.identity_key.take()) {
        (Some(identity), Some(key)) => Ok(Some(identity::Signer::new(
            key.as_bytes(),
            &identity.issuer,
            identity.token_ttl,
        )?)),
        _ => Ok(None),
    }
}

/// Creates the superadmin. It has the login from the configuration, the Argon2id hash of its
/// password, and the credential generation under the session secret.
fn load_superadmin(secrets: &Secrets, signing_secret: &[u8]) -> Result<SuperAdmin, String> {
    let hashed = hash_password(secrets.superadmin_password.as_bytes())?;
    let generation = crypto::superadmin_generation(
        signing_secret,
        &secrets.superadmin_login,
        secrets.superadmin_password.as_bytes(),
    )?;
    Ok(SuperAdmin {
        login: secrets.superadmin_login.clone(),
        password_hash: hashed,
        generation,
    })
}

/// Gives the signing key of the configuration. If the mode is `ephemeral`, it gives a new random key.
fn take_session_secret(secrets: &mut Secrets) -> Result<Vec<u8>, String> {
    match secrets.session_secret.take() {
        None => generate_session_secret(),
        Some(secret) => Ok(secret.as_bytes().to_vec()),
    }
}

fn hash_password_command() -> Result<(), String> {
    let mut input = Vec::new();
    io::stdin()
        .take(16 * 1024 + 1)
        .read_to_end(&mut input)
        .map_err(|e| format!("cannot read password from stdin: {e}"))?;
    if input.len() > 16 * 1024 {
        input.zeroize();
        return Err("password input exceeds 16KiB".into());
    }
    while input.last().is_some_and(|b| matches!(b, b'\n' | b'\r')) {
        input.pop();
    }
    if input.is_empty() {
        return Err("password input is empty".into());
    }
    let result = hash_password(&input);
    input.zeroize();
    println!("{}", result?);
    Ok(())
}

fn healthcheck(path: &Path) -> Result<(), String> {
    let (config, _secrets) = Config::load(path)?;
    let listen = config.server.listen_address;
    let ip = match listen.ip() {
        IpAddr::V4(ip) if ip.is_unspecified() => IpAddr::V4(std::net::Ipv4Addr::LOCALHOST),
        IpAddr::V6(ip) if ip.is_unspecified() => IpAddr::V6(std::net::Ipv6Addr::LOCALHOST),
        ip => ip,
    };
    let addr = SocketAddr::new(ip, listen.port());
    let mut stream = TcpStream::connect_timeout(&addr, Duration::from_secs(1))
        .map_err(|e| format!("healthcheck connect failed: {e}"))?;
    stream
        .set_read_timeout(Some(Duration::from_secs(1)))
        .map_err(|e| format!("healthcheck read timeout failed: {e}"))?;
    stream
        .set_write_timeout(Some(Duration::from_secs(1)))
        .map_err(|e| format!("healthcheck write timeout failed: {e}"))?;
    use std::io::Write;
    stream
        .write_all(b"GET /healthz HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
        .map_err(|e| format!("healthcheck write failed: {e}"))?;
    let mut response = [0u8; 256];
    let n = stream
        .read(&mut response)
        .map_err(|e| format!("healthcheck read failed: {e}"))?;
    let prefix = &response[..n];
    if !(prefix.starts_with(b"HTTP/1.1 200 ") || prefix.starts_with(b"HTTP/1.0 200 ")) {
        return Err("healthcheck returned a non-200 response".into());
    }
    // With an inbox, the inbox must be ready too: `/readyz` of the same port.
    if config.inbox.is_some() {
        inbox::cli::healthcheck(listen).map_err(|e| format!("inbox healthcheck failed: {e}"))?;
    }
    Ok(())
}

fn print_help() {
    println!(
        "portal {}\n\nUSAGE:\n  portal run --config FILE\n  portal check-config --config FILE\n  portal print-config --config FILE\n  portal config-reference\n  portal revoke-keys --config FILE (--user LOGIN | --all)\n  portal reset-two-factor --config FILE (--user LOGIN | --all)\n  portal identity-public-key --config FILE\n  portal healthcheck --config FILE\n  portal import-sqlite --config FILE --from SQLITE_FILE\n  portal end-sessions --config FILE\n  portal hash-password < password.txt\n  portal inbox keygen --out FILE\n  portal inbox token --out FILE\n  portal inbox content-key --out FILE\n  portal inbox content-purge --config FILE --key ID\n  portal inbox rekey --config FILE [--from ID]\n  portal inbox import-sqlite --config FILE --from SQLITE_FILE\n  portal inbox healthcheck --config FILE\n  portal render-demo [ICONS_DIR|-] [launcher|access|access-panel-only|activity|activity-panel-only|user|new-user|services|services-panel-only|services-empty] > page.html\n  portal version\n\nOPTIONS (commands with --config, outside portal inbox):\n  The order of the options is free. Each option comes one time at most.\n  -c FILE is the short form of --config FILE.\n  An option with a value also accepts the form --NAME=VALUE, for example --config=FILE.\n\nExit status: 0 for success, 1 for any error.",
        release_version()
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(storage: &str) -> Config {
        let (config, _) = Config::parse(
            &format!(
                r#"
                server {{ listen_address = "127.0.0.1:4181" trusted_proxies = [] }}
                authentication {{
                  superadmin_login_env    = "T_LOGIN"
                  superadmin_password_env = "T_PASSWORD"
                  mfa                     = "off"
                }}
                session {{ secret_mode = "provided" secret_env = "T_SESSION" }}
                {storage}
                "#
            ),
            &|name| match name {
                "T_LOGIN" => Some("root".into()),
                "T_PASSWORD" => Some("correct-horse-battery-staple".into()),
                "T_SESSION" => Some("0123456789abcdef0123456789abcdef".into()),
                "T_DB" => Some("a-database-password".into()),
                _ => None,
            },
        )
        .unwrap();
        config
    }

    #[test]
    fn end_sessions_refuses_a_sqlite_configuration_before_it_opens_any_store() {
        let error = require_shared_sessions(&config("")).unwrap_err();
        assert!(error.contains("postgres block"), "{error}");
        assert!(error.contains("restart the portal"), "{error}");
    }

    #[test]
    fn end_sessions_accepts_a_postgres_configuration() {
        let postgres = r#"postgres {
            host = "db.example.org"
            database = "portal"
            user = "portal"
            password_env = "T_DB"
        }"#;
        assert_eq!(require_shared_sessions(&config(postgres)), Ok(()));
    }

    fn args(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_string()).collect()
    }

    #[test]
    fn cli_args_config_accepts_each_form() {
        for line in [
            &["--config", "a.hcl"][..],
            &["--config=a.hcl"][..],
            &["-c", "a.hcl"][..],
        ] {
            assert_eq!(
                parse_config_arg(&args(line)),
                Ok(PathBuf::from("a.hcl")),
                "{line:?}"
            );
        }
        // The value is used as it is: it can start with `-` or hold `=`.
        assert_eq!(
            parse_config_arg(&args(&["--config", "-x"])),
            Ok(PathBuf::from("-x"))
        );
        assert_eq!(
            parse_config_arg(&args(&["--config=a=b.hcl"])),
            Ok(PathBuf::from("a=b.hcl"))
        );
    }

    #[test]
    fn cli_args_config_refuses_a_bad_line() {
        let usage = "expected: --config <FILE>";
        assert_eq!(parse_config_arg(&args(&[])), Err(usage.to_string()));
        let cases: &[(&[&str], &str)] = &[
            (&["--config"], "the option --config needs a value"),
            (&["-c"], "the option --config needs a value"),
            (&["--config="], "the value of --config is empty"),
            (&["--config", ""], "the value of --config is empty"),
            (
                &["--config", "a", "-c", "b"],
                "the option --config is given two times",
            ),
            (&["--config", "a", "--all"], "unknown option --all"),
            (&["--config", "a", "--x=1"], "unknown option --x"),
            (&["-c=a"], "unknown option -c=a"),
            (&["a.hcl"], "unexpected argument a.hcl"),
            (&["--config", "a", "b"], "unexpected argument b"),
        ];
        for (line, problem) in cases {
            assert_eq!(
                parse_config_arg(&args(line)),
                Err(format!("{problem}; {usage}")),
                "{line:?}"
            );
        }
    }

    #[test]
    fn cli_args_revoke_takes_the_options_in_any_order() {
        let user = Ok((PathBuf::from("a.hcl"), Some("bob".to_string())));
        for line in [
            &["--config", "a.hcl", "--user", "bob"][..],
            &["--user", "bob", "--config", "a.hcl"][..],
            &["--user=bob", "-c", "a.hcl"][..],
            &["--config=a.hcl", "--user=bob"][..],
        ] {
            assert_eq!(parse_revoke_args(&args(line)), user, "{line:?}");
        }
        let all = Ok((PathBuf::from("a.hcl"), None));
        for line in [
            &["--config", "a.hcl", "--all"][..],
            &["--all", "--config", "a.hcl"][..],
            &["--all", "-c", "a.hcl"][..],
        ] {
            assert_eq!(parse_revoke_args(&args(line)), all, "{line:?}");
        }
        // As in 5.6.0, the value of --user is the next argument, also when it starts with `-`.
        assert_eq!(
            parse_revoke_args(&args(&["--config", "a.hcl", "--user", "--all"])),
            Ok((PathBuf::from("a.hcl"), Some("--all".to_string())))
        );
    }

    #[test]
    fn cli_args_revoke_refuses_a_bad_line() {
        let usage = "expected: --config <FILE> --user <LOGIN>, or --config <FILE> --all";
        for line in [
            &[][..],
            &["--config", "a.hcl"][..],
            &["--user", "bob"][..],
            &["--all"][..],
            &["--config", "a.hcl", "--user", "bob", "--all"][..],
        ] {
            assert_eq!(
                parse_revoke_args(&args(line)),
                Err(usage.to_string()),
                "{line:?}"
            );
        }
        assert_eq!(
            parse_revoke_args(&args(&["-c", "a", "--all", "--all"])),
            Err(format!("the option --all is given two times; {usage}"))
        );
        assert_eq!(
            parse_revoke_args(&args(&["-c", "a", "--all=yes"])),
            Err(format!("the option --all takes no value; {usage}"))
        );
        assert_eq!(
            parse_revoke_args(&args(&["-c", "a", "--from", "b"])),
            Err(format!("unknown option --from; {usage}"))
        );
    }

    #[test]
    fn cli_args_import_takes_the_options_in_any_order() {
        let both = Ok((PathBuf::from("a.hcl"), PathBuf::from("old.db")));
        for line in [
            &["--config", "a.hcl", "--from", "old.db"][..],
            &["--from", "old.db", "--config", "a.hcl"][..],
            &["--from=old.db", "-c", "a.hcl"][..],
        ] {
            assert_eq!(parse_import_args(&args(line)), both, "{line:?}");
        }
        let usage = "expected: --config <FILE> --from <SQLITE FILE>";
        assert_eq!(
            parse_import_args(&args(&["--config", "a.hcl"])),
            Err(usage.to_string())
        );
        assert_eq!(
            parse_import_args(&args(&["--from", "a", "--from", "b", "-c", "x"])),
            Err(format!("the option --from is given two times; {usage}"))
        );
        assert_eq!(
            parse_import_args(&args(&["-c", "a", "--user", "b"])),
            Err(format!("unknown option --user; {usage}"))
        );
    }
}
