//! Command line. No flag has a default: commands that need a file require its path.

use std::io::{BufRead, IsTerminal, Write};
use std::net::{SocketAddr, TcpStream};
use std::path::PathBuf;
use std::time::Duration;

use inbox_core::rng::Rng;
use inbox_core::server_key::ServerKey;
use inbox_core::suite::Role;
use zeroize::Zeroizing;

use crate::config::Config;
use crate::secrets::{read_secret, write_new_secret};
use crate::state::Secrets;
use crate::{GIT_SHA, VERSION};

pub const USAGE: &str = "\
usage:
  inbox serve        --config <file>
  inbox check-config --config <file>
  inbox healthcheck  --config <file>
  inbox keygen       --out <file>
  inbox credential   --server-key <file> --out <file> [--password-stdin]
  inbox token        --out <file>
  inbox version";

pub enum Failure {
    Usage(String),
    Fatal(String),
}

impl From<String> for Failure {
    fn from(s: String) -> Self {
        Failure::Fatal(s)
    }
}

struct Flags {
    values: Vec<(String, String)>,
    switches: Vec<String>,
}

impl Flags {
    fn parse(args: &[String], with_value: &[&str], switches: &[&str]) -> Result<Flags, Failure> {
        let mut f = Flags { values: Vec::new(), switches: Vec::new() };
        let mut it = args.iter();
        while let Some(a) = it.next() {
            if with_value.contains(&a.as_str()) {
                let v = it.next().ok_or_else(|| Failure::Usage(format!("{a} requires a value")))?;
                if f.values.iter().any(|(k, _)| k == a) {
                    return Err(Failure::Usage(format!("{a} given twice")));
                }
                f.values.push((a.clone(), v.clone()));
            } else if switches.contains(&a.as_str()) {
                f.switches.push(a.clone());
            } else {
                return Err(Failure::Usage(format!("unknown argument {a:?}")));
            }
        }
        Ok(f)
    }

    fn required(&self, name: &str) -> Result<String, Failure> {
        self.values
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.clone())
            .ok_or_else(|| Failure::Usage(format!("{name} is required")))
    }

    fn has(&self, name: &str) -> bool {
        self.switches.iter().any(|s| s == name)
    }
}

pub fn run(args: &[String]) -> Result<(), Failure> {
    let Some((cmd, rest)) = args.split_first() else {
        return Err(Failure::Usage("missing command".into()));
    };
    match cmd.as_str() {
        "serve" => {
            let f = Flags::parse(rest, &["--config"], &[])?;
            let config = Config::load(&PathBuf::from(f.required("--config")?))?;
            crate::server::serve(config).map_err(Failure::Fatal)
        }
        "check-config" => {
            let f = Flags::parse(rest, &["--config"], &[])?;
            let config = Config::load(&PathBuf::from(f.required("--config")?))?;
            Secrets::load(&config)?;
            println!(
                "config ok: {} groups, {} producers, devices on {}, admin on {}",
                config.group.len(),
                config.producer.len(),
                config.server.listen,
                config.server.admin_listen
            );
            Ok(())
        }
        "healthcheck" => {
            let f = Flags::parse(rest, &["--config"], &[])?;
            let config = Config::load(&PathBuf::from(f.required("--config")?))?;
            healthcheck(config.server.listen).map_err(Failure::Fatal)
        }
        "keygen" => {
            let f = Flags::parse(rest, &["--out"], &[])?;
            let out = PathBuf::from(f.required("--out")?);
            let key = ServerKey::generate(&mut Rng::os()).map_err(|e| e.to_string())?;
            write_new_secret(&out, &key.encode())?;
            eprintln!(
                "server key written to {} (back it up: replacing it invalidates every device)",
                out.display()
            );
            Ok(())
        }
        "credential" => {
            let f = Flags::parse(rest, &["--server-key", "--out"], &["--password-stdin"])?;
            let key_path = PathBuf::from(f.required("--server-key")?);
            let out = PathBuf::from(f.required("--out")?);
            let key = ServerKey::decode(&read_secret(&key_path, "--server-key")?)
                .map_err(|e| format!("--server-key: {e}"))?;
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
            let token = Zeroizing::new(crate::util::hex(&Rng::os().bytes::<32>()));
            write_new_secret(&out, &token)?;
            eprintln!("producer token written to {}", out.display());
            Ok(())
        }
        "version" | "--version" | "-V" => {
            println!("inbox {VERSION} ({GIT_SHA})");
            Ok(())
        }
        "help" | "--help" | "-h" => {
            println!("{USAGE}");
            Ok(())
        }
        other => Err(Failure::Usage(format!("unknown command {other:?}"))),
    }
}

/// Minimum length of the shared device password.
const MIN_PASSWORD_LEN: usize = 8;

fn read_password(from_stdin: bool) -> Result<Zeroizing<String>, Failure> {
    let min = MIN_PASSWORD_LEN;
    let password = if from_stdin || !std::io::stdin().is_terminal() {
        let mut line = Zeroizing::new(String::new());
        std::io::stdin()
            .lock()
            .read_line(&mut line)
            .map_err(|e| format!("cannot read password: {e}"))?;
        Zeroizing::new(line.trim_end_matches(['\r', '\n']).to_string())
    } else {
        let first = prompt_hidden("device password: ")?;
        let second = prompt_hidden("again: ")?;
        if *first != *second {
            return Err(Failure::Fatal("passwords differ".into()));
        }
        first
    };
    if password.chars().count() < min {
        return Err(Failure::Fatal(format!("device password must be at least {min} characters")));
    }
    Ok(password)
}

/// Reads a line from the terminal with echo disabled.
fn prompt_hidden(prompt: &str) -> Result<Zeroizing<String>, Failure> {
    eprint!("{prompt}");
    let _ = std::io::stderr().flush();
    let fd = libc::STDIN_FILENO;
    // SAFETY: termios is plain data; tcgetattr/tcsetattr only read/write it for fd 0.
    let mut term: libc::termios = unsafe { std::mem::zeroed() };
    let have_term = unsafe { libc::tcgetattr(fd, &mut term) } == 0;
    let original = term;
    if have_term {
        term.c_lflag &= !libc::ECHO;
        unsafe { libc::tcsetattr(fd, libc::TCSANOW, &term) };
    }
    let mut line = Zeroizing::new(String::new());
    let read = std::io::stdin().lock().read_line(&mut line);
    if have_term {
        unsafe { libc::tcsetattr(fd, libc::TCSANOW, &original) };
    }
    eprintln!();
    read.map_err(|e| format!("cannot read password: {e}"))?;
    Ok(Zeroizing::new(line.trim_end_matches(['\r', '\n']).to_string()))
}

/// `GET /readyz` on the device listener, for container health checks (the image
/// has no shell nor curl).
fn healthcheck(listen: SocketAddr) -> Result<(), String> {
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
    std::io::BufReader::new(stream).read_line(&mut status).map_err(|e| format!("read: {e}"))?;
    if status.split_whitespace().nth(1) == Some("200") {
        Ok(())
    } else {
        Err(format!("not ready: {}", status.trim()))
    }
}
