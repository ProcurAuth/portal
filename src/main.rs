#![forbid(unsafe_code)]

mod access;
mod catalog;
mod config;
mod crypto;
mod discovery;
mod http;
mod icons;
mod metrics;
mod rate_limit;
mod session;
mod store;
mod tracking;
mod transport;
mod ui;
mod users;
mod util;

use std::env;
use std::fs::File;
use std::io::{self, Read};
use std::net::{IpAddr, SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use config::{Config, SessionSecretConfig};
use crypto::{generate_session_secret, hash_password};
use http::SuperAdmin;
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
            let config_path = parse_config_arg(args.collect())?;
            let mut config = Config::load(&config_path)?;
            let superadmin = load_superadmin(&config)?;
            let signing_secret = load_session_secret(&mut config)?;
            let store = Arc::new(Store::open(&config.storage.database)?);
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
            )?);
            http::serve(state)
        }
        "check-config" => {
            let config_path = parse_config_arg(args.collect())?;
            let mut config = Config::load(&config_path)?;
            let mut superadmin = load_superadmin(&config)?;
            superadmin.password_hash.zeroize();
            let mut signing_secret = load_session_secret(&mut config)?;
            signing_secret.zeroize();
            println!("configuration ok");
            Ok(())
        }
        "healthcheck" => {
            let config_path = parse_config_arg(args.collect())?;
            healthcheck(&config_path)
        }
        "hash-password" => hash_password_command(),
        // Synthetic launcher page on stdout: to look at the design without a runtime or a login.
        "render-demo" => {
            let icons = match args.next() {
                Some(dir) => icons::Icons::load(Path::new(&dir)),
                None => icons::Icons::empty(),
            };
            let catalog = ui::demo_catalog();
            let registry = ui::demo_registry();
            let admin = ui::demo_admin(&catalog, &registry);
            print!(
                "{}",
                ui::home_page(
                    &catalog,
                    &icons,
                    &ui::HomeContext {
                        viewer: "root",
                        impersonating: false,
                        logout_token: "demo",
                        users_token: "demo",
                        hidden_services: 0,
                        admin: Some(&admin),
                    },
                )
            );
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

fn parse_config_arg(args: Vec<String>) -> Result<PathBuf, String> {
    if args.len() != 2 || args[0] != "--config" {
        return Err("expected: --config <FILE>".into());
    }
    Ok(PathBuf::from(&args[1]))
}

fn read_secret_file(path: &Path, max_bytes: usize, label: &str) -> Result<Vec<u8>, String> {
    let file = File::open(path)
        .map_err(|e| format!("cannot open {label} {}: {e}", path.display()))?;
    let mut value = Vec::new();
    file.take(max_bytes.saturating_add(1) as u64)
        .read_to_end(&mut value)
        .map_err(|e| format!("cannot read {label} {}: {e}", path.display()))?;
    if value.len() > max_bytes {
        value.zeroize();
        return Err(format!("{label} exceeds {max_bytes} bytes: {}", path.display()));
    }
    Ok(value)
}

/// The superadmin: its login (trimmed, lower case) and the Argon2id hash of its password. Both
/// come from files the environment fills in.
fn load_superadmin(config: &Config) -> Result<SuperAdmin, String> {
    let login_path = &config.authentication.superadmin_login_file;
    let raw = read_secret_file(login_path, 256, "superadmin login")?;
    let login = String::from_utf8(raw)
        .map_err(|_| format!("superadmin login is not UTF-8: {}", login_path.display()))?
        .trim()
        .to_ascii_lowercase();
    if login.is_empty() || login.len() > 64 || login.chars().any(char::is_control) {
        return Err(format!(
            "superadmin login must be 1 to 64 printable characters: {}",
            login_path.display()
        ));
    }

    let path = &config.authentication.superadmin_password_file;
    let mut password = read_secret_file(path, config.security.max_password_bytes, "superadmin password")?;
    if password.is_empty() {
        password.zeroize();
        return Err(format!("superadmin password is empty: {}", path.display()));
    }
    let hashed = hash_password(&password);
    password.zeroize();
    Ok(SuperAdmin {
        login,
        password_hash: hashed?,
    })
}

fn load_session_secret(config: &mut Config) -> Result<Vec<u8>, String> {
    match std::mem::replace(&mut config.session.secret, SessionSecretConfig::Ephemeral) {
        SessionSecretConfig::Ephemeral => generate_session_secret(),
        SessionSecretConfig::File(path) => {
            let mut secret = read_secret_file(&path, 4096, "session signing secret")?;
            while secret.last().is_some_and(|b| matches!(b, b'\n' | b'\r')) {
                secret.pop();
            }
            if secret.len() < 32 {
                let len = secret.len();
                secret.zeroize();
                return Err(format!(
                    "session signing secret {} is too short: {len} bytes, minimum is 32",
                    path.display()
                ));
            }
            Ok(secret)
        }
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
    let config = Config::load(path)?;
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
    if prefix.starts_with(b"HTTP/1.1 200 ") || prefix.starts_with(b"HTTP/1.0 200 ") {
        Ok(())
    } else {
        Err("healthcheck returned a non-200 response".into())
    }
}

fn print_help() {
    println!(
        "portal {}\n\nUSAGE:\n  portal run --config FILE\n  portal check-config --config FILE\n  portal healthcheck --config FILE\n  portal hash-password < password.txt\n  portal render-demo [ICONS_DIR] > page.html\n  portal version",
        release_version()
    );
}
