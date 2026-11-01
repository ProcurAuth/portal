//! `inbox`: the Inbox server binary.

mod admin;
mod api;
mod cli;
mod config;
mod db;
mod device;
mod hub;
mod net;
mod peer;
mod pg;
mod ratelimit;
mod replica;
mod secrets;
mod server;
mod shutdown;
mod spool;
mod state;
mod store;
mod telemetry;
mod util;

use std::process::ExitCode;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");
pub const GIT_SHA: &str = env!("INBOX_GIT_SHA");

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match cli::run(&args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(cli::Failure::Usage(msg)) => {
            eprintln!("error: {msg}\n\n{}", cli::USAGE);
            ExitCode::from(2)
        }
        Err(cli::Failure::Fatal(msg)) => {
            eprintln!("error: {msg}");
            ExitCode::from(1)
        }
    }
}
