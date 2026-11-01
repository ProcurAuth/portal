//! The commands `portal inbox ...`. No flag has a default: a command that needs a file requires
//! its path.
//!
//! - `keygen --out FILE`: makes the key of the server.
//! - `credential --server-key FILE --out FILE [--password-stdin]`: makes the record of the shared
//!   password of the phones.
//! - `token --out FILE`: makes the token of a producer.
//! - `import-sqlite --config FILE --from FILE`: copies the data of a SQLite file of the inbox into
//!   the empty PostgreSQL database of the portal.
//! - `healthcheck --config FILE`: asks `/readyz` of the listener of the phones.

use std::io::{BufRead, IsTerminal, Write};
use std::net::{SocketAddr, TcpStream};
use std::path::PathBuf;
use std::time::Duration;

use inbox_core::rng::Rng;
use inbox_core::server_key::ServerKey;
use inbox_core::suite::Role;
use zeroize::Zeroizing;

use super::pg::{PgSettings, PgStore};
use crate::config::{Config, StorageBackend};

/// The shortest shared password of the phones.
const MIN_PASSWORD_LEN: usize = 8;

/// Reads the flags of a command. A flag with a value, and a flag without a value.
struct Flags {
    values: Vec<(String, String)>,
    switches: Vec<String>,
}

impl Flags {
    fn parse(args: &[String], with_value: &[&str], switches: &[&str]) -> Result<Flags, String> {
        let mut f = Flags {
            values: Vec::new(),
            switches: Vec::new(),
        };
        let mut it = args.iter();
        while let Some(a) = it.next() {
            if with_value.contains(&a.as_str()) {
                let v = it.next().ok_or_else(|| format!("{a} requires a value"))?;
                if f.values.iter().any(|(k, _)| k == a) {
                    return Err(format!("{a} given twice"));
                }
                f.values.push((a.clone(), v.clone()));
            } else if switches.contains(&a.as_str()) {
                f.switches.push(a.clone());
            } else {
                return Err(format!("unknown argument {a:?}"));
            }
        }
        Ok(f)
    }

    fn required(&self, name: &str) -> Result<String, String> {
        self.values
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.clone())
            .ok_or_else(|| format!("{name} is required"))
    }

    fn has(&self, name: &str) -> bool {
        self.switches.iter().any(|s| s == name)
    }
}

/// The text of the usage of the commands.
pub const USAGE: &str = "\
  portal inbox keygen --out FILE
  portal inbox credential --server-key FILE --out FILE [--password-stdin]
  portal inbox token --out FILE
  portal inbox import-sqlite --config FILE --from SQLITE_FILE
  portal inbox healthcheck --config FILE";

/// Runs `portal inbox <command> ...`. `args` start after the word `inbox`.
pub fn run(args: &[String]) -> Result<(), String> {
    let Some((cmd, rest)) = args.split_first() else {
        return Err(format!("expected a command:\n{USAGE}"));
    };
    match cmd.as_str() {
        "keygen" => {
            let f = Flags::parse(rest, &["--out"], &[])?;
            let out = PathBuf::from(f.required("--out")?);
            let key = ServerKey::generate(&mut Rng::os()).map_err(|e| e.to_string())?;
            write_new_secret(&out, &key.encode())?;
            eprintln!(
                "server key written to {} (back it up: a new key makes each enrolled phone invalid)",
                out.display()
            );
            Ok(())
        }
        "credential" => {
            let f = Flags::parse(rest, &["--server-key", "--out"], &["--password-stdin"])?;
            let key_path = PathBuf::from(f.required("--server-key")?);
            let out = PathBuf::from(f.required("--out")?);
            let key_text = std::fs::read_to_string(&key_path)
                .map(Zeroizing::new)
                .map_err(|e| format!("--server-key: cannot read {}: {e}", key_path.display()))?;
            let key =
                ServerKey::decode(key_text.trim()).map_err(|e| format!("--server-key: {e}"))?;
            let password = read_password(f.has("--password-stdin"))?;
            let record = inbox_core::login::register(
                &key.opaque,
                Role::Device,
                password.as_bytes(),
                &mut Rng::os(),
            )
            .map_err(|e| e.to_string())?;
            write_new_secret(&out, &inbox_core::b64::encode(&record))?;
            eprintln!("device credential written to {}", out.display());
            Ok(())
        }
        "token" => {
            let f = Flags::parse(rest, &["--out"], &[])?;
            let out = PathBuf::from(f.required("--out")?);
            let token = Zeroizing::new(super::util::hex(&Rng::os().bytes::<32>()));
            write_new_secret(&out, &token)?;
            eprintln!("producer token written to {}", out.display());
            Ok(())
        }
        "import-sqlite" => {
            let f = Flags::parse(rest, &["--config", "--from"], &[])?;
            let config_path = PathBuf::from(f.required("--config")?);
            let from = PathBuf::from(f.required("--from")?);
            import_sqlite(&config_path, &from)
        }
        "healthcheck" => {
            let f = Flags::parse(rest, &["--config"], &[])?;
            let (config, _secrets) = Config::load(&PathBuf::from(f.required("--config")?))?;
            let inbox = config
                .inbox
                .as_ref()
                .ok_or("the configuration has no inbox block")?;
            healthcheck(inbox.devices.listen)
        }
        other => Err(format!("unknown inbox command {other:?}\n{USAGE}")),
    }
}

fn import_sqlite(config_path: &std::path::Path, from: &std::path::Path) -> Result<(), String> {
    let (config, mut secrets) = Config::load(config_path)?;
    let inbox = config
        .inbox
        .as_ref()
        .ok_or("--config must contain a inbox block")?;
    let StorageBackend::Postgres(pg) = &config.storage.backend else {
        return Err("--config must contain a postgres block".into());
    };
    let password = secrets
        .postgres_password
        .take()
        .ok_or("internal error: the postgres block has no password")?;
    let password = std::str::from_utf8(password.as_bytes())
        .map(|p| Zeroizing::new(p.to_string()))
        .map_err(|_| "postgres.password is not valid UTF-8".to_string())?;
    let settings = PgSettings {
        host: pg.host.clone(),
        port: pg.port,
        database: pg.database.clone(),
        user: pg.user.clone(),
        password,
        pool_size: inbox.storage.pool_size,
        connect_timeout: pg.connect_timeout,
    };
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| format!("cannot start runtime: {e}"))?;
    let imported = runtime.block_on(async {
        let store = PgStore::connect(&settings).await?;
        let result = store.import_sqlite(from).await;
        let _ = store.shutdown().await;
        result
    })?;
    println!(
        "imported {} devices, {} subscriptions, {} alerts, {} deliveries, {} connections from {}",
        imported.devices,
        imported.subscriptions,
        imported.alerts,
        imported.deliveries,
        imported.connections,
        from.display()
    );
    Ok(())
}

/// Writes a new secret file with the mode 0600. It never overwrites a file.
fn write_new_secret(path: &std::path::Path, contents: &str) -> Result<(), String> {
    use std::os::unix::fs::OpenOptionsExt;
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .map_err(|e| format!("cannot create {}: {e}", path.display()))?;
    f.write_all(contents.as_bytes())
        .and_then(|_| f.write_all(b"\n"))
        .and_then(|_| f.sync_all())
        .map_err(|e| format!("cannot write {}: {e}", path.display()))
}

/// Reads the shared password of the phones. It comes from `--password-stdin` or from a standard
/// input that is not a terminal. The command does not ask for the password at a terminal.
fn read_password(from_stdin: bool) -> Result<Zeroizing<String>, String> {
    if !from_stdin && std::io::stdin().is_terminal() {
        return Err(
            "the standard input is a terminal: give the password with --password-stdin, for example `printf '%s\\n' \"$PASSWORD\" | portal inbox credential --password-stdin ...`"
                .into(),
        );
    }
    let mut line = Zeroizing::new(String::new());
    std::io::stdin()
        .lock()
        .read_line(&mut line)
        .map_err(|e| format!("cannot read password: {e}"))?;
    let password = Zeroizing::new(line.trim_end_matches(['\r', '\n']).to_string());
    if password.chars().count() < MIN_PASSWORD_LEN {
        return Err(format!(
            "device password must be at least {MIN_PASSWORD_LEN} characters"
        ));
    }
    Ok(password)
}

/// `GET /readyz` on the listener of the phones, for the health checks of a container (the image
/// has no shell and no `curl`).
pub fn healthcheck(listen: SocketAddr) -> Result<(), String> {
    let target = if listen.ip().is_unspecified() {
        let loopback = if listen.is_ipv4() { "127.0.0.1" } else { "::1" };
        SocketAddr::new(loopback.parse().expect("loopback"), listen.port())
    } else {
        listen
    };
    let mut stream = TcpStream::connect_timeout(&target, Duration::from_secs(3))
        .map_err(|e| format!("connect {target}: {e}"))?;
    stream.set_read_timeout(Some(Duration::from_secs(3))).ok();
    stream
        .write_all(
            format!("GET /readyz HTTP/1.1\r\nHost: {target}\r\nConnection: close\r\n\r\n")
                .as_bytes(),
        )
        .map_err(|e| format!("write: {e}"))?;
    let mut status = String::new();
    std::io::BufReader::new(stream)
        .read_line(&mut status)
        .map_err(|e| format!("read: {e}"))?;
    if status.split_whitespace().nth(1) == Some("200") {
        Ok(())
    } else {
        Err(format!("not ready: {}", status.trim()))
    }
}
