//! Turn the parsed file into typed values, one for each key of the table.
//!
//! Every value is checked once here. The error of a bad value names the key, what the key
//! accepts and the value that the file gave. A secret never appears in an error or in the
//! effective configuration.

use std::collections::{BTreeSet, HashMap};
use std::fs::File;
use std::io::Read;
use std::path::Path;
use std::time::Duration;

use zeroize::Zeroize;

use super::hcl::{Document, Labeled, Lit};
use super::keys::{self, Key, Kind, Presence, When, KEYS};

/// The text that stands for a secret in the effective configuration.
pub const HIDDEN: &str = "<hidden>";

/// A secret. The bytes are erased from memory when the value is dropped.
pub struct Secret(Vec<u8>);

impl Secret {
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }
}

impl Drop for Secret {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}

impl std::fmt::Debug for Secret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Secret(<hidden>)")
    }
}

/// A typed value.
#[derive(Debug, Clone, PartialEq)]
pub enum Val {
    Text(String),
    Int(u64),
    Bool(bool),
    Duration(Duration),
    Size(usize),
    List(Vec<String>),
}

/// One line of the effective configuration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Effective {
    /// `block.key`.
    pub key: String,
    /// The value as the file writes it, or `<hidden>` for a secret.
    pub value: String,
    /// `default`, `file`, `env:VAR` or `file:/path`.
    pub source: String,
}

/// The values of one block with a label.
pub struct LabeledValues {
    pub kind: String,
    pub label: String,
    values: HashMap<&'static str, Val>,
    secrets: HashMap<&'static str, Secret>,
}

impl LabeledValues {
    pub fn list(&self, name: &str) -> Result<Vec<String>, String> {
        match self.values.get(name) {
            Some(Val::List(items)) => Ok(items.clone()),
            _ => Err(format!(
                "internal error: {}.{}.{name} is not a list",
                self.kind, self.label
            )),
        }
    }

    /// Take a secret out of the block. The caller owns it from here.
    pub fn take_secret(&mut self, name: &str) -> Option<Secret> {
        self.secrets.remove(name)
    }
}

/// The values of all keys that apply.
#[derive(Default)]
pub struct Resolved {
    values: HashMap<(&'static str, &'static str), Val>,
    secrets: HashMap<(&'static str, &'static str), Secret>,
    pub effective: Vec<Effective>,
    /// The blocks that the file has.
    blocks: BTreeSet<String>,
    /// The blocks with a label, in the order of the file.
    pub labeled: Vec<LabeledValues>,
}

impl Resolved {
    /// The value of a key. A key that does not apply gives its default, so the typed structure
    /// always holds a value. The effective configuration lists only the keys that apply.
    fn find(&self, block: &str, name: &str) -> Result<Val, String> {
        let key = keys::find(block, name)
            .ok_or_else(|| format!("internal error: {block}.{name} is not in the key table"))?;
        if let Some(value) = self.values.get(&(key.block, key.name)) {
            return Ok(value.clone());
        }
        match key.presence {
            Presence::Default(text) => parse_scalar(key, &default_literal(key, text)),
            _ => Err(format!("internal error: {block}.{name} has no value")),
        }
    }

    pub fn has_block(&self, block: &str) -> bool {
        self.blocks.contains(block)
    }

    /// True when the key applies and the file or its default gave a value.
    pub fn has(&self, block: &str, name: &str) -> bool {
        keys::find(block, name).is_some_and(|key| self.values.contains_key(&(key.block, key.name)))
    }

    pub fn text(&self, block: &str, name: &str) -> Result<String, String> {
        match self.find(block, name)? {
            Val::Text(v) => Ok(v),
            _ => Err(format!("internal error: {block}.{name} is not a text")),
        }
    }

    pub fn int(&self, block: &str, name: &str) -> Result<u64, String> {
        match self.find(block, name)? {
            Val::Int(v) => Ok(v),
            _ => Err(format!("internal error: {block}.{name} is not a number")),
        }
    }

    pub fn count(&self, block: &str, name: &str) -> Result<usize, String> {
        usize::try_from(self.int(block, name)?)
            .map_err(|_| format!("internal error: {block}.{name} does not fit in usize"))
    }

    pub fn bool(&self, block: &str, name: &str) -> Result<bool, String> {
        match self.find(block, name)? {
            Val::Bool(v) => Ok(v),
            _ => Err(format!("internal error: {block}.{name} is not a boolean")),
        }
    }

    pub fn duration(&self, block: &str, name: &str) -> Result<Duration, String> {
        match self.find(block, name)? {
            Val::Duration(v) => Ok(v),
            _ => Err(format!("internal error: {block}.{name} is not a duration")),
        }
    }

    pub fn size(&self, block: &str, name: &str) -> Result<usize, String> {
        match self.find(block, name)? {
            Val::Size(v) => Ok(v),
            _ => Err(format!("internal error: {block}.{name} is not a size")),
        }
    }

    /// The text of a key that may be absent.
    pub fn optional_text(&self, block: &str, name: &str) -> Result<Option<String>, String> {
        if self.has(block, name) {
            self.text(block, name).map(Some)
        } else {
            Ok(None)
        }
    }

    /// Take a secret out of the set. The caller owns it from here.
    pub fn take_secret(&mut self, block: &str, name: &str) -> Option<Secret> {
        let key = keys::find(block, name)?;
        self.secrets.remove(&(key.block, key.name))
    }

    fn applies(&self, when: When) -> bool {
        let flag = |name: &str| self.bool("security", name).unwrap_or(false);
        match when {
            When::Always => true,
            When::PostgresBlock => self.has_block("postgres"),
            When::NoPostgres => !self.has_block("postgres"),
            When::KubernetesOn => self.bool("discovery", "kubernetes").unwrap_or(false),
            When::LoginLimiterOn => flag("login_rate_limit_enabled"),
            When::GlobalLimiterOn => flag("global_login_rate_limit_enabled"),
            When::AnyLimiterOn => {
                flag("login_rate_limit_enabled") || flag("global_login_rate_limit_enabled")
            }
            When::MetricsOn => self.bool("metrics", "enabled").unwrap_or(false),
            When::SecretProvided => {
                self.text("session", "secret_mode").is_ok_and(|mode| mode == "provided")
            }
            When::IdentityBlock => self.has_block("identity"),
            When::PdpBlock => self.has_block("pdp"),
            When::LabeledBlock => true,
        }
    }
}

/// How the program reads an environment variable. The program reads only the variables that
/// the file names.
pub type Env<'a> = &'a dyn Fn(&str) -> Option<String>;

/// The value of the key `name` of the table, as the default writes it.
fn default_literal(key: &Key, text: &str) -> Lit {
    match key.kind {
        Kind::Int { .. } => Lit::Int(text.to_string()),
        Kind::Bool => Lit::Word(text.to_string()),
        _ => Lit::Str(text.to_string()),
    }
}

/// Read, check and type every key.
pub fn resolve(document: &Document, env: Env<'_>) -> Result<Resolved, String> {
    check_names(document)?;
    let mut resolved = Resolved {
        blocks: document.blocks.iter().cloned().collect(),
        ..Resolved::default()
    };
    for key in KEYS {
        if keys::is_optional_block(key.block) && !resolved.has_block(key.block) {
            continue;
        }
        let applies = resolved.applies(key.when);
        if key.is_secret() {
            resolve_secret(key, applies, document, env, &mut resolved)?;
            continue;
        }
        let written = document
            .values
            .get(&(key.block.to_string(), key.name.to_string()));
        if !applies {
            if written.is_some() {
                return Err(format!(
                    "{} must not be set when {}",
                    key.path(),
                    key.when.refusal()
                ));
            }
            continue;
        }
        let (literal, source) = match (written, &key.presence) {
            (Some(lit), _) => (lit.clone(), "file"),
            (None, Presence::Required) => {
                return Err(format!(
                    "missing required configuration key: {} (expected {})",
                    key.path(),
                    key.expected()
                ));
            }
            (None, Presence::Default(text)) => (default_literal(key, text), "default"),
            (None, Presence::Optional) => continue,
        };
        let value = parse_scalar(key, &literal)?;
        resolved.effective.push(Effective {
            key: key.path(),
            value: shown_value(key, &literal),
            source: source.to_string(),
        });
        resolved.values.insert((key.block, key.name), value);
    }
    for block in &document.labeled {
        let values = resolve_labeled(block, env, &mut resolved.effective)?;
        resolved.labeled.push(values);
    }
    Ok(resolved)
}

/// The value of a key as the effective configuration shows it.
fn shown_value(key: &Key, literal: &Lit) -> String {
    match (&key.kind, literal) {
        (_, Lit::List(_)) => literal.shown(),
        (
            Kind::Text { .. } | Kind::Duration { .. } | Kind::Size { .. } | Kind::Enum(_),
            Lit::Str(text) | Lit::Int(text) | Lit::Word(text),
        ) => format!("{text:?}"),
        (_, Lit::Str(text) | Lit::Int(text) | Lit::Word(text)) => text.clone(),
    }
}

/// Read, check and type the keys of one block with a label. The table of labeled keys is closed.
fn resolve_labeled(
    block: &Labeled,
    env: Env<'_>,
    effective: &mut Vec<Effective>,
) -> Result<LabeledValues, String> {
    let path = format!("{}.{}", block.kind, block.label);
    let mut written: Vec<&String> = block.values.keys().collect();
    written.sort();
    for name in written {
        if keys::find_labeled(&block.kind, name).is_some_and(|key| !key.is_secret()) {
            continue;
        }
        let secret_form = ["_env", "_file"].iter().find_map(|suffix| {
            let base = name.strip_suffix(suffix)?;
            keys::find_labeled(&block.kind, base).map(|key| (key, *suffix))
        });
        match secret_form {
            Some((key, _)) if key.is_secret() => continue,
            Some((_, suffix)) => {
                return Err(format!(
                    "{path}.{name}: the key does not accept the form {suffix}; write the value itself"
                ));
            }
            None => {}
        }
        if keys::find_labeled(&block.kind, name).is_some() {
            return Err(format!(
                "{path}.{name} must not be written in the file: use {path}.{name}_env or {path}.{name}_file"
            ));
        }
        return Err(format!("unknown configuration key: {path}.{name}"));
    }
    let mut out = LabeledValues {
        kind: block.kind.clone(),
        label: block.label.clone(),
        values: HashMap::new(),
        secrets: HashMap::new(),
    };
    for key in keys::LABELED_KEYS.iter().filter(|k| k.block == block.kind) {
        let key_path = format!("{path}.{}", key.name);
        if key.is_secret() {
            let get = |suffix: &str| block.values.get(&format!("{}{suffix}", key.name));
            let (secret, source) =
                read_secret(key, &key_path, get(""), get("_env"), get("_file"), env)?;
            effective.push(Effective {
                key: key_path,
                value: HIDDEN.to_string(),
                source,
            });
            out.secrets.insert(key.name, secret);
            continue;
        }
        let literal = match (block.values.get(key.name), &key.presence) {
            (Some(lit), _) => lit.clone(),
            (None, Presence::Default(text)) => default_literal(key, text),
            (None, _) => {
                return Err(format!(
                    "missing required configuration key: {key_path} (expected {})",
                    key.expected()
                ))
            }
        };
        let value = parse_scalar(key, &literal).map_err(|e| {
            e.replacen(&key.path(), &key_path, 1)
        })?;
        effective.push(Effective {
            key: key_path,
            value: shown_value(key, &literal),
            source: if block.values.contains_key(key.name) {
                "file".into()
            } else {
                "default".into()
            },
        });
        out.values.insert(key.name, value);
    }
    Ok(out)
}

/// Refuse a name that is not in the table. A key and the forms of a secret are the only names.
fn check_names(document: &Document) -> Result<(), String> {
    let mut written: Vec<&(String, String)> = document.values.keys().collect();
    written.sort();
    for (block, name) in written {
        if keys::find(block, name).is_some_and(|key| !key.is_secret()) {
            continue;
        }
        let secret_form = ["_env", "_file"].iter().find_map(|suffix| {
            let base = name.strip_suffix(suffix)?;
            keys::find(block, base).map(|key| (key, *suffix))
        });
        match secret_form {
            Some((key, _)) if key.is_secret() => continue,
            Some((key, suffix)) => {
                return Err(format!(
                    "{block}.{name}: {} does not accept the form {suffix}; write the value itself",
                    key.path()
                ));
            }
            None => {}
        }
        if keys::find(block, name).is_some() {
            return Err(format!(
                "{block}.{name} must not be written in the file: use {block}.{name}_env or {block}.{name}_file"
            ));
        }
        return Err(format!("unknown configuration key: {block}.{name}"));
    }
    Ok(())
}

fn type_error(key: &Key, literal: &Lit, detail: Option<&str>) -> String {
    let detail = detail.map(|d| format!(" ({d})")).unwrap_or_default();
    format!(
        "invalid {}: expected {}{detail}, got {}",
        key.path(),
        key.expected(),
        literal.shown()
    )
}

/// Check a written literal against the type of the key.
fn parse_scalar(key: &Key, literal: &Lit) -> Result<Val, String> {
    let bad = |detail: Option<&str>| Err(type_error(key, literal, detail));
    match (&key.kind, literal) {
        (Kind::Text { check, .. }, Lit::Str(text)) => match check(text) {
            Ok(()) => Ok(Val::Text(text.clone())),
            Err(reason) => bad(Some(&reason)),
        },
        (Kind::Int { min, max }, Lit::Int(text)) => {
            if text.len() > 1 && text.starts_with('0') {
                return bad(Some("a leading zero is not allowed"));
            }
            match text.parse::<u64>() {
                Ok(n) if (*min..=*max).contains(&n) => Ok(Val::Int(n)),
                Ok(_) => bad(Some("out of range")),
                Err(_) => bad(Some("not a decimal number")),
            }
        }
        (Kind::Bool, Lit::Word(text)) => match text.as_str() {
            "true" => Ok(Val::Bool(true)),
            "false" => Ok(Val::Bool(false)),
            _ => bad(None),
        },
        (Kind::Duration { min, max }, Lit::Str(text)) => match keys::parse_duration(text) {
            Ok(d) if (*min..=*max).contains(&d) => Ok(Val::Duration(d)),
            Ok(_) => bad(Some("out of range")),
            Err(reason) => bad(Some(&reason)),
        },
        (Kind::Size { min, max }, Lit::Str(text)) => match keys::parse_size(text) {
            Ok(n) if (*min..=*max).contains(&n) => Ok(Val::Size(n)),
            Ok(_) => bad(Some("out of range")),
            Err(reason) => bad(Some(&reason)),
        },
        (Kind::Enum(values), Lit::Str(text)) if values.contains(&text.as_str()) => {
            Ok(Val::Text(text.clone()))
        }
        (
            Kind::List {
                check, max_items, ..
            },
            Lit::List(items),
        ) => {
            if items.len() > *max_items {
                return bad(Some("too many items"));
            }
            for (index, item) in items.iter().enumerate() {
                if let Err(reason) = check(item) {
                    return bad(Some(&format!("item {item:?}: {reason}")));
                }
                if items[..index].contains(item) {
                    return bad(Some(&format!("item {item:?} is written two times")));
                }
            }
            Ok(Val::List(items.clone()))
        }
        _ => bad(None),
    }
}

/// Resolve a secret key: the file gives `<name>_env` or `<name>_file`, never the value.
fn resolve_secret(
    key: &Key,
    applies: bool,
    document: &Document,
    env: Env<'_>,
    resolved: &mut Resolved,
) -> Result<(), String> {
    let get = |suffix: &str| {
        document
            .values
            .get(&(key.block.to_string(), format!("{}{suffix}", key.name)))
    };
    let (plain, from_env, from_file) = (get(""), get("_env"), get("_file"));
    let path = key.path();
    if !applies && plain.is_none() {
        if from_env.is_some() || from_file.is_some() {
            return Err(format!("{path}_env and {path}_file must not be set when {}", key.when.refusal()));
        }
        return Ok(());
    }
    let (secret, source) = read_secret(key, &path, plain, from_env, from_file, env)?;
    resolved.effective.push(Effective {
        key: path,
        value: HIDDEN.to_string(),
        source,
    });
    resolved.secrets.insert((key.block, key.name), secret);
    Ok(())
}

/// Read one secret from the variable or the file that the configuration names. `path` is the
/// name of the key in the messages.
fn read_secret(
    key: &Key,
    path: &str,
    plain: Option<&Lit>,
    from_env: Option<&Lit>,
    from_file: Option<&Lit>,
    env: Env<'_>,
) -> Result<(Secret, String), String> {
    let Kind::Secret { max_bytes } = key.kind else {
        return Err(format!("internal error: {path} is not a secret"));
    };
    if plain.is_some() {
        return Err(format!(
            "{path} must not be written in the file: use {path}_env or {path}_file"
        ));
    }
    let (bytes, source) = match (from_env, from_file) {
        (None, None) => {
            return Err(format!(
                "missing required configuration key: give exactly one of {path}_env (the name of an environment variable) or {path}_file (an absolute path)"
            ));
        }
        (Some(_), Some(_)) => {
            return Err(format!("give exactly one of {path}_env and {path}_file, not both"));
        }
        (Some(lit), None) => {
            let attribute = format!("{path}_env");
            let Lit::Str(name) = lit else {
                return Err(format!(
                    "invalid {attribute}: expected the name of an environment variable in a quoted string, got {}",
                    lit.shown()
                ));
            };
            keys::check_env_name(name).map_err(|reason| {
                format!(
                    "invalid {attribute}: expected the name of an environment variable ({reason}), got {name:?}"
                )
            })?;
            let value = env(name).ok_or_else(|| {
                format!("{attribute}: the environment variable {name} is not set, or its value is not valid UTF-8")
            })?;
            if value.is_empty() {
                return Err(format!("{attribute}: the environment variable {name} is empty"));
            }
            (value.into_bytes(), format!("env:{name}"))
        }
        (None, Some(lit)) => {
            let attribute = format!("{path}_file");
            let Lit::Str(file) = lit else {
                return Err(format!(
                    "invalid {attribute}: expected an absolute path in a quoted string, got {}",
                    lit.shown()
                ));
            };
            let file_check = {
                let p = Path::new(file);
                if file.is_empty() || !p.is_absolute() {
                    Err("the path is not absolute")
                } else if p
                    .components()
                    .any(|c| matches!(c, std::path::Component::ParentDir))
                {
                    Err("the path contains '..'")
                } else {
                    Ok(())
                }
            };
            file_check.map_err(|reason| {
                format!("invalid {attribute}: expected an absolute path without '..' ({reason}), got {file:?}")
            })?;
            (read_secret_file(&attribute, Path::new(file), max_bytes)?, format!("file:{file}"))
        }
    };
    let mut bytes = bytes;
    if bytes.len() > max_bytes {
        bytes.zeroize();
        return Err(format!("{path}: the value is larger than {max_bytes} bytes ({source})"));
    }
    if bytes.is_empty() {
        return Err(format!("{path}: the value is empty ({source})"));
    }
    Ok((Secret(bytes), source))
}

/// The content of the file without its last line end.
fn read_secret_file(attribute: &str, path: &Path, max_bytes: usize) -> Result<Vec<u8>, String> {
    let file = File::open(path)
        .map_err(|e| format!("{attribute}: cannot open {}: {e}", path.display()))?;
    let mut value = Vec::new();
    file.take(max_bytes.saturating_add(3) as u64)
        .read_to_end(&mut value)
        .map_err(|e| format!("{attribute}: cannot read {}: {e}", path.display()))?;
    if value.last() == Some(&b'\n') {
        value.pop();
        if value.last() == Some(&b'\r') {
            value.pop();
        }
    }
    if value.len() > max_bytes {
        value.zeroize();
        return Err(format!(
            "{attribute}: {} is larger than {max_bytes} bytes",
            path.display()
        ));
    }
    if value.is_empty() {
        return Err(format!("{attribute}: {} is empty", path.display()));
    }
    Ok(value)
}

#[cfg(test)]
/// Check that a default of the table is a valid value of its key.
pub(super) fn check_default(key: &Key, text: &str) -> Result<(), String> {
    parse_scalar(key, &default_literal(key, text)).map(|_| ())
}
