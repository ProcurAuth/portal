//! The commands `portal inbox ...`. No flag has a default: a command that needs a file requires
//! its path.
//!
//! - `keygen --out FILE`: makes the key of the server. The file holds the Noise key pair only.
//! - `token --out FILE`: makes the text of an API key, for a block `api_key` of the file.
//! - `content-key --out FILE`: makes a key that seals the contents of the items, for a block
//!   `inbox.at_rest.key` of the file.
//! - `content-purge --config FILE --key ID`: erases the sealed contents of a key that the file lost, and keeps the
//!   metadata of the items.
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
  portal inbox content-key --out FILE
  portal inbox content-purge --config FILE --key ID
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
        "content-key" => {
            let f = Flags::parse(rest, &["--out"])?;
            let out = PathBuf::from(f.required("--out")?);
            let mut bytes = Zeroizing::new([0u8; super::content_box::KEY_BYTES]);
            crate::crypto::random_bytes(&mut *bytes)?;
            let text = Zeroizing::new(inbox_core::b64::encode(&*bytes));
            write_new_secret(&out, &text)?;
            eprintln!(
                "content key written to {} (keep it outside the data volume and outside the backups of the database; a lost key makes the sealed contents unreadable)",
                out.display()
            );
            Ok(())
        }
        "content-purge" => {
            let f = Flags::parse(rest, &["--config", "--key"])?;
            content_purge(
                &PathBuf::from(f.required("--config")?),
                &f.required("--key")?,
            )
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

/// Erases the sealed contents of the key `kid`. The key must not be in the file: the command is for a key that is lost.
fn content_purge(config_path: &std::path::Path, kid: &str) -> Result<(), String> {
    let (config, mut secrets) = Config::load(config_path)?;
    let at_rest = config
        .inbox
        .as_ref()
        .and_then(|i| i.at_rest.as_ref())
        .ok_or("--config must contain the block inbox.at_rest")?;
    if at_rest.keys.iter().any(|k| k == kid) {
        return Err(format!(
            "inbox.at_rest.key {kid:?} is in the file: its contents are not lost, and this command does not erase them (it erases the contents of a key that the file lost)"
        ));
    }
    let (items, turns, intents, agents) = match &config.storage.backend {
        StorageBackend::Sqlite(path) => super::items_db::purge_key_file(path, kid)?,
        StorageBackend::Postgres(pg) => {
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
                pool_size: 1,
                connect_timeout: pg.connect_timeout,
            };
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|e| format!("cannot start runtime: {e}"))?;
            runtime.block_on(async {
                let store = super::store::Store::Pg(std::sync::Arc::new(
                    PgStore::connect(&settings).await?,
                ));
                super::items_store::purge_key(&store, kid).await
            })?
        }
    };
    println!(
        "erased the sealed contents of the key {kid:?}: {items} items, {turns} turns, {intents} intents, {agents} agent hints (the metadata stays)"
    );
    Ok(())
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
        "imported {} devices, {} subscriptions, {} alerts, {} deliveries, {} connections, {} enrollment codes, {} notification settings from {}",
        imported.devices,
        imported.subscriptions,
        imported.alerts,
        imported.deliveries,
        imported.connections,
        imported.enrollment_codes,
        imported.notifications,
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

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_path(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("portal-cli-{}-{name}", std::process::id()))
    }

    #[test]
    fn content_key_command_never_overwrites() {
        let path = temp_path("content-key");
        let _ = std::fs::remove_file(&path);
        let args = [
            "content-key".to_string(),
            "--out".into(),
            path.display().to_string(),
        ];
        run(&args).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(
            text.ends_with('\n') && text.trim_end().len() == 44,
            "{text:?}"
        );
        // The key has the form that the configuration reads.
        let bytes = super::super::content_box::parse_secret(text.trim_end().as_bytes()).unwrap();
        assert_eq!(bytes.len(), 32);
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        // A second run refuses and keeps the first key.
        let error = run(&args).unwrap_err();
        assert!(
            error.contains("cannot create") && error.contains("exists"),
            "{error}"
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), text);
        // Another run in another file gives another key.
        let other = temp_path("content-key-2");
        let _ = std::fs::remove_file(&other);
        run(&[
            "content-key".into(),
            "--out".into(),
            other.display().to_string(),
        ])
        .unwrap();
        assert_ne!(std::fs::read_to_string(&other).unwrap(), text);
        // The flag is required and nothing else is accepted.
        assert!(run(&["content-key".into()])
            .unwrap_err()
            .contains("--out is required"));
        assert!(run(&["content-key".into(), "--force".into()]).is_err());
        std::fs::remove_file(path).ok();
        std::fs::remove_file(other).ok();
    }
}
