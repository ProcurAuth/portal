//! Shared application state, built once at startup.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::AtomicU64;

use inbox_core::opaque_ke::ServerRegistration;
use inbox_core::proto::GroupInfo;
use inbox_core::server_key::ServerKey;
use inbox_core::suite::Suite;
use sha2::{Digest, Sha256};
use zeroize::Zeroizing;

use crate::config::Config;
use crate::db::Db;
use crate::hub::Hub;
use crate::ratelimit::AuthLimiter;
use crate::secrets::read_secret;
use crate::shutdown::Shutdown;
use crate::telemetry::{Gauges, Metrics};

pub struct Producer {
    pub name: String,
    pub token_hash: [u8; 32],
    pub groups: Vec<String>,
    pub webhook_send_resolved: bool,
}

/// Secret material loaded from the files named in the configuration.
pub struct Secrets {
    pub server_key: ServerKey,
    pub device_record: ServerRegistration<Suite>,
    pub producers: Vec<Producer>,
}

pub const MIN_TOKEN_LEN: usize = 32;

impl Secrets {
    /// Reads and validates every secret file. Used by `serve` and `check-config`.
    pub fn load(config: &Config) -> Result<Secrets, String> {
        let key_text = read_secret(&config.server.server_key_file, "server.server_key_file")?;
        let server_key = ServerKey::decode(&key_text)
            .map_err(|e| format!("server.server_key_file: {e} (generate with `inbox keygen`)"))?;
        let device_record =
            load_record(&config.devices.credential_file, "devices.credential_file")?;
        let mut producers = Vec::new();
        let mut hashes = std::collections::HashSet::new();
        for (name, p) in &config.producer {
            let label = format!("producer.{name}.token_file");
            let token = read_secret(&p.token_file, &label)?;
            if token.len() < MIN_TOKEN_LEN || !token.bytes().all(|b| b.is_ascii_graphic()) {
                return Err(format!(
                    "{label}: token must be >= {MIN_TOKEN_LEN} printable characters (use `inbox token`)"
                ));
            }
            let token_hash: [u8; 32] = Sha256::digest(token.as_bytes()).into();
            if !hashes.insert(token_hash) {
                return Err(format!("{label}: token reused by another producer"));
            }
            producers.push(Producer {
                name: name.clone(),
                token_hash,
                groups: p.groups.clone(),
                webhook_send_resolved: p.webhook_send_resolved,
            });
        }
        Ok(Secrets { server_key, device_record, producers })
    }
}

fn load_record(path: &std::path::Path, label: &str) -> Result<ServerRegistration<Suite>, String> {
    let text = read_secret(path, label)?;
    let bytes = Zeroizing::new(
        inbox_core::b64::decode(&text).ok_or_else(|| format!("{label}: not base64"))?,
    );
    inbox_core::login::parse_record(&bytes)
        .map_err(|_| format!("{label}: not a credential record (use `inbox credential`)"))
}

pub struct AppState {
    pub config: Config,
    pub secrets: Secrets,
    pub db: Db,
    pub hub: Hub,
    pub limiter: AuthLimiter,
    pub metrics: Metrics,
    pub gauges: Arc<Gauges>,
    pub shutdown: Shutdown,
    pub groups: Vec<GroupInfo>,
    pub group_names: HashMap<String, String>,
    pub next_session_id: AtomicU64,
}

impl AppState {
    /// Alerts created before this Unix time (ms) have expired for delivery.
    pub fn delivery_cutoff(&self) -> i64 {
        crate::util::now_ms() - self.config.storage.delivery_ttl.as_millis() as i64
    }

    pub fn group_name(&self, id: &str) -> String {
        self.group_names.get(id).cloned().unwrap_or_else(|| id.to_string())
    }

    /// Finds the producer owning `token`, comparing hashes in constant time.
    pub fn producer_for_token(&self, token: &str) -> Option<&Producer> {
        let presented: [u8; 32] = Sha256::digest(token.as_bytes()).into();
        let mut found = None;
        for p in &self.secrets.producers {
            let mut diff = 0u8;
            for (a, b) in p.token_hash.iter().zip(presented.iter()) {
                diff |= a ^ b;
            }
            if diff == 0 {
                found = Some(p);
            }
        }
        found
    }
}
