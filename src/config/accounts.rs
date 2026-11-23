//! The accounts and the keys of the file: the blocks `user` and the blocks `key` inside them
//!.
//!
//! An account of the file has grants and, optionally, the digest of a password. Its keys come only
//! from the file. The module checks each block when the program starts. It never changes a value.
//! An error names the login and, for a key, the name of the key. An error about the text of a key
//! names the attribute and the variable or the path, never the value.

use std::collections::BTreeMap;

use serde_json::{Map, Value as Json};

use super::hcl::Lit;
use super::resolve::{GrantItem, LabeledValues, Resolved};
use super::{source_of, FileAccount, FileKey};
use crate::grants::{Grant, Mode, ACCESS};
use crate::limits::Limits;
use crate::portal_actions;

/// What the blocks of the file need from the rest of the configuration.
pub struct Rules {
    pub mode: Mode,
    pub superadmin_login: String,
    /// The block `keys` is present.
    pub keys_enabled: bool,
    /// `users.max_grants`.
    pub max_grants: usize,
    /// `keys.max_per_user`. Without the block `keys`, the default of the table.
    pub max_keys_per_user: usize,
}

/// A key holds at most 512 grants, at any value of `users.max_grants`.
pub const MAX_KEY_GRANTS: usize = 512;

/// The limit values of the object `limits` of a grant. The form is the form of the database: a
/// whole number for a ceiling, and a list of objects with `amount` and `window` for a budget.
fn limits_of(members: &[(String, Lit)]) -> Result<Limits, String> {
    let mut object = Map::new();
    for (name, value) in members {
        let json = match value {
            Lit::Int(text) => Json::Number(
                text.parse::<u64>()
                    .map_err(|_| format!("the limit {name:?} is not a whole number: {text}"))?
                    .into(),
            ),
            Lit::Items(items) => Json::Array(
                items
                    .iter()
                    .map(|item| budget_item(name, item))
                    .collect::<Result<_, _>>()?,
            ),
            Lit::List(items) if items.is_empty() => Json::Array(Vec::new()),
            other => {
                return Err(format!(
                    "the limit {name:?} is a whole number or a list of objects with amount and window, got {}",
                    other.shown()
                ))
            }
        };
        object.insert(name.clone(), json);
    }
    Limits::from_json(&Json::Object(object).to_string())
}

fn budget_item(name: &str, item: &Lit) -> Result<Json, String> {
    let Lit::Object(members) = item else {
        return Err(format!(
            "each item of the budget {name:?} is an object with amount and window, got {}",
            item.shown()
        ));
    };
    let mut out = Map::new();
    for (member, value) in members {
        let json = match (member.as_str(), value) {
            ("amount", Lit::Int(text)) => Json::Number(
                text.parse::<u64>()
                    .map_err(|_| format!("the amount of the budget {name:?} is not a whole number: {text}"))?
                    .into(),
            ),
            ("window", Lit::Str(text)) => Json::String(text.clone()),
            (other, _) => {
                return Err(format!(
                    "the member {other} of an item of the budget {name:?} is not known or has a bad form: an item has amount (a number) and window (a quoted text)"
                ))
            }
        };
        out.insert(member.clone(), json);
    }
    Ok(Json::Object(out))
}

/// The grants of a block, with their limit values. `what` names the block in the errors.
fn grants_of(items: Vec<GrantItem>, what: &str) -> Result<Vec<Grant>, String> {
    let mut grants = Vec::with_capacity(items.len());
    for item in items {
        let grant = Grant::parse(&item.text)
            .map_err(|e| format!("invalid {what}.grants: item {:?}: {e}", item.text))?;
        let limits = limits_of(&item.limits)
            .map_err(|e| format!("invalid {what}.grants: item {:?}: {e}", item.text))?;
        // A budget value is only on the grant `access` on a service.
        if limits
            .iter()
            .any(|(_, value)| matches!(value, crate::limits::Value::Budget(_)))
            && grant.action != ACCESS
        {
            return Err(format!(
                "invalid {what}.grants: item {:?}: a budget value is only on the grant {ACCESS} on a service",
                item.text
            ));
        }
        let grant = grant.with_limits(limits).with_confirm(item.confirm);
        if item.confirm {
            if let Some(reason) = crate::grants::confirm_refusal(&grant) {
                return Err(format!(
                    "invalid {what}.grants: item {:?}: confirm = true: {reason}",
                    item.text
                ));
            }
        }
        grants.push(grant);
    }
    grants.sort();
    Ok(grants)
}

/// The Argon2id digest of a password in the PHC string form.
fn check_password_hash(text: &str) -> Result<(), String> {
    use argon2::password_hash::PasswordHash;
    let parsed = PasswordHash::new(text).map_err(|_| "not a PHC string".to_string())?;
    if parsed.algorithm.as_str() != "argon2id" {
        return Err("the algorithm is not argon2id".into());
    }
    Ok(())
}

/// The accounts of the file, by login.
pub fn build(r: &Resolved, rules: &Rules) -> Result<BTreeMap<String, FileAccount>, String> {
    let mut accounts = BTreeMap::new();
    let mut key_ids: Vec<(String, String, String)> = Vec::new();
    let mut digests: Vec<([u8; 32], String, String)> = Vec::new();
    for block in r.labeled.iter().filter(|b| b.kind == "user") {
        let login = &block.label;
        if rules.mode == Mode::Dynamic {
            return Err(format!(
                "user {login:?}: a user block is not permitted when access.grants_mode is \"dynamic\"; use \"static\" or \"hybrid\", or give the grants in the settings pages"
            ));
        }
        if crate::users::normalize_login(login).as_deref() != Ok(login.as_str()) {
            return Err(format!(
                "user {login:?}: expected a login of 1 to 32 characters of a-z, 0-9, '.', '-' and '_'"
            ));
        }
        if *login == rules.superadmin_login {
            return Err(format!(
                "user {login:?}: this is the login of the superadmin, who has all grants: remove the block"
            ));
        }
        let what = format!("user.{login}");
        let grants = grants_of(block.grants("grants")?, &what)?;
        if grants.len() > rules.max_grants {
            return Err(format!(
                "user {login:?}: the block has {} grants, users.max_grants is {}",
                grants.len(),
                rules.max_grants
            ));
        }
        // A key of an account of the file comes only from the file.
        if grants.iter().any(|g| {
            g.service == portal_actions::SERVICE && g.action == portal_actions::KEYS_OWN_CREATE
        }) {
            return Err(format!(
                "user {login:?}: the block gives {}: a key of an account of the file comes only from the file",
                portal_actions::KEYS_OWN_CREATE
            ));
        }
        // The grants on the service portal follow the document of the features that the file
        // turns on. The caller checks them when the whole configuration exists.
        let password_hash = match block.secret_bytes("password_hash") {
            None => None,
            Some(bytes) => {
                let text = std::str::from_utf8(bytes).map_err(|_| {
                    format!(
                        "user.{login}.password_hash: the value is not valid UTF-8 ({}; the value is hidden)",
                        source_of(r, &format!("{what}.password_hash"))
                    )
                })?;
                check_password_hash(text).map_err(|reason| {
                    format!(
                        "user.{login}.password_hash: expected an Argon2id digest in the PHC string form ({reason}; {}; the value is hidden)",
                        source_of(r, &format!("{what}.password_hash"))
                    )
                })?;
                Some(text.to_string())
            }
        };
        if !block.children.is_empty() && !rules.keys_enabled {
            return Err(format!(
                "user {login:?}: a block key holds an MCP key and needs the block keys with keys.mcp = true; without it, no MCP key is accepted"
            ));
        }
        if block.children.len() > rules.max_keys_per_user {
            return Err(format!(
                "user {login:?}: the block has {} blocks key, keys.max_per_user is {}",
                block.children.len(),
                rules.max_keys_per_user
            ));
        }
        let mut keys = Vec::new();
        for child in &block.children {
            keys.push(file_key(r, login, child, &mut key_ids, &mut digests)?);
        }
        accounts.insert(
            login.clone(),
            FileAccount {
                grants,
                password_hash,
                keys,
            },
        );
    }
    Ok(accounts)
}

/// One block `key` of the user `login`.
fn file_key(
    r: &Resolved,
    login: &str,
    block: &LabeledValues,
    ids: &mut Vec<(String, String, String)>,
    digests: &mut Vec<([u8; 32], String, String)>,
) -> Result<FileKey, String> {
    let name = &block.label;
    let path = format!("user.{login}.key.{name}");
    let here = |what: &str| format!("user {login:?}, key {name:?}: {what}");
    crate::keys::check_name(name).map_err(|e| here(&e))?;
    let grants = grants_of(block.grants("grants")?, &path)?;
    if grants.len() > MAX_KEY_GRANTS {
        return Err(here(&format!(
            "the block has {} grants, a key holds at most {MAX_KEY_GRANTS}",
            grants.len()
        )));
    }
    portal_actions::check_key_grants(crate::keys::KeyKind::Mcp, &grants).map_err(|e| here(&e))?;
    let digest = match (block.has_secret("key"), block.has_secret("key_sha256")) {
        (true, true) => {
            return Err(here(
                "give exactly one of key_env, key_file, key_sha256_env and key_sha256_file, not two of them",
            ))
        }
        (false, false) => {
            return Err(here(
                "give exactly one of key_env, key_file, key_sha256_env and key_sha256_file",
            ))
        }
        (true, false) => {
            let source = source_of(r, &format!("{path}.key"));
            let bytes = block.secret_bytes("key").unwrap_or_default();
            let text = std::str::from_utf8(bytes)
                .map_err(|_| here(&format!("{path}.key: the value is not valid UTF-8 ({source}; the value is hidden)")))?;
            let kind = crate::keys::check_format(text).map_err(|reason| {
                here(&format!(
                    "{path}.key: the text of a key is mcp_, 43 characters and 6 characters of checksum, 53 characters in all ({reason:?}; {source}; the value is hidden)"
                ))
            })?;
            if kind != crate::keys::KeyKind::Mcp {
                return Err(here(&format!(
                    "{path}.key: a block key holds an MCP key, with the prefix mcp_: the key of a service is a block api_key in the block inbox ({source}; the value is hidden)"
                )));
            }
            crate::keys::digest(text)
        }
        (false, true) => {
            let source = source_of(r, &format!("{path}.key_sha256"));
            let bytes = block.secret_bytes("key_sha256").unwrap_or_default();
            let text = std::str::from_utf8(bytes).map_err(|_| {
                here(&format!(
                    "{path}.key_sha256: the value is not valid UTF-8 ({source}; the value is hidden)"
                ))
            })?;
            crate::keys::parse_digest_hex(text).map_err(|_| {
                here(&format!(
                    "{path}.key_sha256: expected 64 hexadecimal characters, the value has {} bytes ({source}; the value is hidden)",
                    bytes.len()
                ))
            })?
        }
    };
    let id = crate::keys::file_id(login, name);
    if let Some((_, other_login, other_name)) = ids.iter().find(|(other, _, _)| *other == id) {
        return Err(here(&format!(
            "the subject id key:{id} is also the id of the key {other_name:?} of the user {other_login:?}"
        )));
    }
    if let Some((_, other_login, other_name)) =
        digests.iter().find(|(other, _, _)| *other == digest)
    {
        return Err(here(&format!(
            "the digest of the key is also the digest of the key {other_name:?} of the user {other_login:?}"
        )));
    }
    ids.push((id.clone(), login.to_string(), name.clone()));
    digests.push((digest, login.to_string(), name.clone()));
    Ok(FileKey {
        name: name.clone(),
        id,
        digest,
        interaction: crate::keys::Interaction::from_name(&block.text("interaction")?)
            .ok_or_else(|| here("internal error: the value of interaction is not known"))?,
        grants,
    })
}
