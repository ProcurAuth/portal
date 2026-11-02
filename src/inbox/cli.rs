//! The commands `portal inbox ...`. No flag has a default: a command that needs a file requires
//! its path.
//!
//! - `keygen --out FILE`: makes the key of the server. The file holds the Noise key pair only.
//! - `token --out FILE`: makes the text of an API key, for a block `api_key` of the file.
//! - `import-sqlite --config FILE --from FILE`: copies the data of a SQLite file of the inbox into
//!   the empty PostgreSQL database of the portal.
//! - `healthcheck --config FILE`: asks `/readyz` of the listener of the portal.

use std::io::{BufRead, Write};
use std::net::{SocketAddr, TcpStream};
use std::path::PathBuf;
use std::time::Duration;

use inbox_core::server_key::ServerKey;
use zeroize::Zeroizing;

use super::pg::{PgSettings, PgStore};
use crate::config::{Config, StorageBackend};

/// Reads the flags of a command. Each flag has a value.
struct Flags {
    values: Vec<(String, String)>,
}

impl Flags {
    fn parse(args: &[String], with_value: &[&str]) -> Result<Flags, String> {
        let mut f = Flags { values: Vec::new() };
        let mut it = args.iter();
        while let Some(a) = it.next() {
            if with_value.contains(&a.as_str()) {
                let v = it.next().ok_or_else(|| format!("{a} requires a value"))?;
                if f.values.iter().any(|(k, _)| k == a) {
                    return Err(format!("{a} given twice"));
                }
                f.values.push((a.clone(), v.clone()));
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
}

/// The text of the usage of the commands.
pub const USAGE: &str = "\
  portal inbox keygen --out FILE
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
            let f = Flags::parse(rest, &["--out"])?;
            let out = PathBuf::from(f.required("--out")?);
            let key = ServerKey::generate().map_err(|e| e.to_string())?;
            write_new_secret(&out, &key.encode())?;
            eprintln!(
                "server key written to {} (back it up: a new key makes each enrolled phone invalid)",
                out.display()
            );
            Ok(())
        }
        "token" => {
            let f = Flags::parse(rest, &["--out"])?;
            let out = PathBuf::from(f.required("--out")?);
            let token = crate::keys::generate(crate::keys::KeyKind::Api)?;
            write_new_secret(&out, &token)?;
            eprintln!("API key written to {}", out.display());
            Ok(())
        }
        "import-sqlite" => {
            let f = Flags::parse(rest, &["--config", "--from"])?;
            let config_path = PathBuf::from(f.required("--config")?);
            let from = PathBuf::from(f.required("--from")?);
            import_sqlite(&config_path, &from)
        }
        "healthcheck" => {
            let f = Flags::parse(rest, &["--config"])?;
            let (config, _secrets) = Config::load(&PathBuf::from(f.required("--config")?))?;
            if config.inbox.is_none() {
                return Err("the configuration has no inbox block".into());
            }
            healthcheck(config.server.listen_address)
        }
        other => Err(format!("unknown inbox command {other:?}\n{USAGE}")),
    }
}

fn import_sqlite(config_path: &std::path::Path, from: &std::path::Path) -> Result<(), String> {
    let (config, mut secrets) = Config::load(config_path)?;
    let inbox = config
        .inbox
        .as_ref()
        .ok_or("--config must contain an inbox block")?;
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
        "imported {} devices, {} subscriptions, {} alerts, {} deliveries, {} connections, {} enrollment codes from {}",
        imported.devices,
        imported.subscriptions,
        imported.alerts,
        imported.deliveries,
        imported.connections,
        imported.enrollment_codes,
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

/// `GET /readyz` on the listener of the portal, for the health checks of a container (the image
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
