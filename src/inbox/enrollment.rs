//! The enrollment codes and the devices of an account.
//!
//! An account of the portal makes a code. The code holds the address of the listener of the
//! phones, the public key of the server and a secret of 128 bits. The inbox keeps only the SHA-256
//! digest of the secret. The phone uses the code one time on `/v1/enroll` (see `device.rs`).

use hyper::StatusCode;
use inbox_core::enroll::EnrollCode;
use inbox_core::proto::close;
use inbox_core::rng::Rng;
use serde::Serialize;
use sha2::{Digest, Sha256};
use zeroize::Zeroize;

use super::api::ApiError;
use super::db::CodeInsert;
use super::log;
use super::state::App;

/// A code that an account just made.
#[derive(Debug, Serialize)]
pub struct NewCode {
    /// The text of the code: `portal-inbox://enroll?s=...&k=...&c=...`. It is a secret until its first use.
    pub code: String,
    /// The end of the life of the code, in Unix milliseconds.
    pub expires_at_ms: i64,
    /// The first 8 hexadecimal characters of the SHA-256 digest of the public key of the server.
    pub fingerprint: String,
}

/// Makes a code for `login`. The account must exist: the caller checks it. `stored` tells that the
/// account is an account of the database: the store then refuses the code when the row of the
/// account is gone, in the transaction of the code. An account that a delete ends at the same time
/// therefore keeps no code. The inbox refuses the code when the account has
/// `inbox.enrollment.max_active_per_account` codes that are not used and not expired. With a
/// replica, the call returns after the copy holds the digest of the code.
pub async fn create_code(app: &App, login: &str, stored: bool) -> Result<NewCode, ApiError> {
    let mut secret: [u8; 16] = Rng::os().bytes();
    let digest: [u8; 32] = Sha256::digest(secret).into();
    let settings = &app.config.enrollment;
    let outcome = app
        .db
        .create_code(
            digest,
            login.to_string(),
            settings.code_ttl.as_millis() as i64,
            settings.max_active_per_account,
            stored,
        )
        .await;
    let expires_at_ms = match outcome {
        Ok(CodeInsert::Created { expires_at_ms }) => expires_at_ms,
        Ok(CodeInsert::NoAccount) => {
            secret.zeroize();
            return Err(ApiError::new(
                StatusCode::NOT_FOUND,
                "unknown_account",
                "the account does not exist",
            ));
        }
        Ok(CodeInsert::TooMany) => {
            secret.zeroize();
            return Err(ApiError::new(
                StatusCode::TOO_MANY_REQUESTS,
                "too_many_codes",
                format!(
                    "the account has {} enrollment codes that are not used and not expired: use one or wait for it to expire",
                    settings.max_active_per_account
                ),
            ));
        }
        Err(e) => {
            secret.zeroize();
            return Err(ApiError::storage(e));
        }
    };
    if let Err(e) = app.replica.sync().await {
        app.metrics.replica_sync_failure();
        log::warn!(event = "replica_sync_failed", error = %e);
        if app.replica.strict_alerts() {
            secret.zeroize();
            // The code is not given to anyone: it can go.
            if let Err(e) = app.db.delete_code(digest).await {
                log::warn!(event = "enroll_code_cleanup_failed", error = %e);
            }
            return Err(ApiError::new(
                StatusCode::SERVICE_UNAVAILABLE,
                "replication_unavailable",
                "the code could not be replicated, retry",
            ));
        }
    }
    let server_public_key: [u8; 32] = app
        .secrets
        .server_key
        .noise_public
        .as_slice()
        .try_into()
        .map_err(|_| {
            ApiError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal",
                "the key of the server has a bad length",
            )
        })?;
    let code = EnrollCode {
        address: app.config.devices.public_address.clone(),
        server_public_key,
        secret,
    };
    secret.zeroize();
    log::info!(event = "enroll_code_created", user.name = %login, expires_at_ms);
    Ok(NewCode {
        fingerprint: code.fingerprint(),
        code: code.encode(),
        expires_at_ms,
    })
}

/// Ends the inbox state of the account `login`. This is the one operation for every end of an
/// account: the delete of an account of the database, an account of the file that left the file,
/// and an old login of the superadmin. It deletes the unused codes, revokes the devices and
/// detaches the rows that name the login (see `db::end_account`). The sessions of the devices end
/// with the close code 4403. A new account with the same login starts with nothing. The result is
/// the number of devices that the call revoked.
pub async fn end_account(app: &App, login: &str) -> Result<usize, String> {
    let ids = app
        .db
        .end_account(login)
        .await
        .map_err(|e| format!("inbox storage: {e}"))?;
    for id in &ids {
        app.hub.kick(id, close::REVOKED);
    }
    // The open items of a deleted owner are withdrawn.
    if let Some(items) = app.items() {
        match items.cancel_where(None, Some(login), "owner_deleted").await {
            Ok(n) if n > 0 => log::info!(event = "items_cancelled", user.name = %login, count = n),
            Ok(_) => {}
            Err(e) => log::warn!(event = "items_cancel_failed", user.name = %login, error = %e),
        }
    }
    if let Err(e) = app.replica.sync().await {
        app.metrics.replica_sync_failure();
        log::warn!(event = "replica_sync_failed", error = %e);
    }
    log::info!(event = "account_inbox_ended", user.name = %login, devices = ids.len());
    Ok(ids.len())
}
