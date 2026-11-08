//! The effect hash of an operation and the view that shows it (ProcurAuth protocol 17.3).
//!
//! The effect binds a service, an action, a resource and an operation (`op`). The server, the
//! phone (JNI) and the tests all compute it with this module, so the rule exists once.
//!
//! The hash input is the JSON object `{"v":1,"service":..,"action":..,"resource":..,"op":..}`.
//! It is written with JCS (RFC 8785), encoded in UTF-8 and hashed with SHA-256.
//! `op` has only objects, arrays, strings, booleans, `null` and integers of at most 2^53-1 in
//! absolute value. Any other number, a duplicate member name or an `op` that is not an object is
//! invalid. A `view` is a template over `op`: each value that it shows is an RFC 6901 pointer.

use std::fmt;

use serde::de::{self, DeserializeSeed, MapAccess, SeqAccess, Visitor};
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};

/// Largest absolute value of an integer in `op`: 2^53 - 1.
pub const MAX_SAFE_INTEGER: i64 = 9_007_199_254_740_991;

/// Version of the hash input. It is the member `v`.
pub const HASH_INPUT_VERSION: u8 = 1;

/// Deepest nesting of `op` that this module accepts. The parser of serde_json stops at 128.
pub const MAX_DEPTH: usize = 64;

/// A `view` has at most this number of fields.
pub const MAX_VIEW_FIELDS: usize = 12;
/// Characters of a title of a view, of a label of a field and of the label of the body.
pub const MAX_VIEW_TITLE_CHARS: usize = 128;
pub const MAX_VIEW_LABEL_CHARS: usize = 48;
/// Bytes of one JSON Pointer of a view.
pub const MAX_POINTER_BYTES: usize = 256;

/// The reason why an effect or a view is refused. The codes are the codes of the protocol.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EffectError {
    /// `op` breaks the rules of 17.3: the API code is `invalid_op`.
    InvalidOp(&'static str),
    /// The view breaks a rule, or a pointer names no value of `op`: the API code is `invalid_view`.
    InvalidView(&'static str),
}

impl EffectError {
    /// The code of the error, as the API and the JNI bridge give it.
    pub fn code(&self) -> &'static str {
        match self {
            EffectError::InvalidOp(_) => "invalid_op",
            EffectError::InvalidView(_) => "invalid_view",
        }
    }

    /// The text that tells which rule failed.
    pub fn reason(&self) -> &'static str {
        match self {
            EffectError::InvalidOp(r) | EffectError::InvalidView(r) => r,
        }
    }
}

impl fmt::Display for EffectError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.code(), self.reason())
    }
}

impl std::error::Error for EffectError {}

// ---------------------------------------------------------------- strict `op`

/// The value of `op`. Its parser applies every rule of 17.3 while it reads the JSON text:
/// no float, no integer above 2^53-1, no duplicate member name, only the allowed kinds.
///
/// A parser of serde_json into `Value` drops a duplicate member name without a sign.
/// This type does not. Use it for each `op` that comes from outside.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct Op(Value);

impl Op {
    /// Reads an `op` from JSON text. The value must be an object.
    pub fn parse(text: &str) -> Result<Op, EffectError> {
        let mut de = serde_json::Deserializer::from_str(text);
        let value =
            Strict { depth: 0 }.deserialize(&mut de).map_err(|e| classify(&e.to_string()))?;
        de.end().map_err(|_| EffectError::InvalidOp("not one JSON value"))?;
        Op::from_value(value)
    }

    /// Takes a value that was built in memory. The checks are the same except the duplicates,
    /// which a `Value` cannot hold.
    pub fn from_value(value: Value) -> Result<Op, EffectError> {
        if !value.is_object() {
            return Err(EffectError::InvalidOp("op is not an object"));
        }
        check_value(&value, 0)?;
        Ok(Op(value))
    }

    pub fn as_value(&self) -> &Value {
        &self.0
    }

    pub fn into_value(self) -> Value {
        self.0
    }
}

impl<'de> Deserialize<'de> for Op {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Op, D::Error> {
        let value = Strict { depth: 0 }.deserialize(d)?;
        if !value.is_object() {
            return Err(de::Error::custom("op is not an object"));
        }
        Ok(Op(value))
    }
}

/// Maps the text of a serde error back to a fixed reason.
fn classify(text: &str) -> EffectError {
    for (needle, reason) in [
        ("float", "a number with a fraction or an exponent"),
        ("range", "an integer above 2^53-1 in absolute value"),
        ("duplicate", "duplicate member name"),
        ("depth", "op is nested too deep"),
    ] {
        if text.contains(needle) {
            return EffectError::InvalidOp(reason);
        }
    }
    EffectError::InvalidOp("not valid JSON")
}

struct Strict {
    depth: usize,
}

impl<'de> DeserializeSeed<'de> for Strict {
    type Value = Value;
    fn deserialize<D: Deserializer<'de>>(self, d: D) -> Result<Value, D::Error> {
        if self.depth > MAX_DEPTH {
            return Err(de::Error::custom("depth"));
        }
        d.deserialize_any(StrictVisitor { depth: self.depth })
    }
}

struct StrictVisitor {
    depth: usize,
}

impl<'de> Visitor<'de> for StrictVisitor {
    type Value = Value;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("an object, an array, a string, a boolean, null or an integer")
    }

    fn visit_bool<E: de::Error>(self, v: bool) -> Result<Value, E> {
        Ok(Value::Bool(v))
    }

    fn visit_i64<E: de::Error>(self, v: i64) -> Result<Value, E> {
        if !(-MAX_SAFE_INTEGER..=MAX_SAFE_INTEGER).contains(&v) {
            return Err(E::custom("range"));
        }
        Ok(Value::from(v))
    }

    fn visit_u64<E: de::Error>(self, v: u64) -> Result<Value, E> {
        if v > MAX_SAFE_INTEGER as u64 {
            return Err(E::custom("range"));
        }
        Ok(Value::from(v))
    }

    fn visit_f64<E: de::Error>(self, _v: f64) -> Result<Value, E> {
        // serde_json reads `1.5`, `1.0`, `1e2`, `-0` and an integer of more than 64 bits as f64.
        Err(E::custom("float"))
    }

    fn visit_str<E: de::Error>(self, v: &str) -> Result<Value, E> {
        Ok(Value::String(v.to_owned()))
    }

    fn visit_string<E: de::Error>(self, v: String) -> Result<Value, E> {
        Ok(Value::String(v))
    }

    fn visit_unit<E: de::Error>(self) -> Result<Value, E> {
        Ok(Value::Null)
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Value, A::Error> {
        let mut items = Vec::new();
        while let Some(item) = seq.next_element_seed(Strict { depth: self.depth + 1 })? {
            items.push(item);
        }
        Ok(Value::Array(items))
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Value, A::Error> {
        let mut out = Map::new();
        while let Some(key) = map.next_key::<String>()? {
            let value = map.next_value_seed(Strict { depth: self.depth + 1 })?;
            if out.insert(key, value).is_some() {
                return Err(de::Error::custom("duplicate"));
            }
        }
        Ok(Value::Object(out))
    }
}

/// Checks a value that is already in memory against the rules of 17.3.
fn check_value(v: &Value, depth: usize) -> Result<(), EffectError> {
    if depth > MAX_DEPTH {
        return Err(EffectError::InvalidOp("op is nested too deep"));
    }
    match v {
        Value::Null | Value::Bool(_) | Value::String(_) => Ok(()),
        Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                if (-MAX_SAFE_INTEGER..=MAX_SAFE_INTEGER).contains(&i) {
                    return Ok(());
                }
                return Err(EffectError::InvalidOp("an integer above 2^53-1 in absolute value"));
            }
            if let Some(u) = n.as_u64() {
                if u <= MAX_SAFE_INTEGER as u64 {
                    return Ok(());
                }
                return Err(EffectError::InvalidOp("an integer above 2^53-1 in absolute value"));
            }
            Err(EffectError::InvalidOp("a number with a fraction or an exponent"))
        }
        Value::Array(items) => items.iter().try_for_each(|i| check_value(i, depth + 1)),
        Value::Object(map) => map.values().try_for_each(|i| check_value(i, depth + 1)),
    }
}

// ---------------------------------------------------------------- JCS

/// Writes a value as JCS text (RFC 8785) on the subset of 17.3.
///
/// A float, an integer above 2^53-1 in absolute value and a nesting above [`MAX_DEPTH`] are
/// refused with `InvalidOp`. The members of an object sort by UTF-16 code unit.
pub fn jcs(v: &Value) -> Result<String, EffectError> {
    let mut out = String::new();
    write_jcs(v, &mut out, 0)?;
    Ok(out)
}

fn write_jcs(v: &Value, out: &mut String, depth: usize) -> Result<(), EffectError> {
    check_depth(depth)?;
    match v {
        Value::Null => out.push_str("null"),
        Value::Bool(true) => out.push_str("true"),
        Value::Bool(false) => out.push_str("false"),
        Value::Number(_) => {
            check_value(v, depth)?;
            // After the check the number is an integer of the safe range.
            out.push_str(&v.to_string());
        }
        Value::String(s) => write_string(s, out),
        Value::Array(items) => {
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_jcs(item, out, depth + 1)?;
            }
            out.push(']');
        }
        Value::Object(map) => {
            let mut members: Vec<(&String, &Value)> = map.iter().collect();
            members.sort_by(|a, b| a.0.encode_utf16().cmp(b.0.encode_utf16()));
            out.push('{');
            for (i, (k, item)) in members.into_iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_string(k, out);
                out.push(':');
                write_jcs(item, out, depth + 1)?;
            }
            out.push('}');
        }
    }
    Ok(())
}

fn check_depth(depth: usize) -> Result<(), EffectError> {
    if depth > MAX_DEPTH { Err(EffectError::InvalidOp("op is nested too deep")) } else { Ok(()) }
}

fn write_string(s: &str, out: &mut String) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\u{8}' => out.push_str("\\b"),
            '\t' => out.push_str("\\t"),
            '\n' => out.push_str("\\n"),
            '\u{c}' => out.push_str("\\f"),
            '\r' => out.push_str("\\r"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
}

// ---------------------------------------------------------------- the effect

/// The text that the hash covers: the JCS form of the hash input.
///
/// `op` must be an object. The strings `service`, `action` and `resource` are free text here;
/// the caller checks their form.
pub fn canonical_input(
    service: &str,
    action: &str,
    resource: &str,
    op: &Value,
) -> Result<String, EffectError> {
    if !op.is_object() {
        return Err(EffectError::InvalidOp("op is not an object"));
    }
    let mut out = String::from("{\"action\":");
    write_string(action, &mut out);
    out.push_str(",\"op\":");
    write_jcs(op, &mut out, 0)?;
    out.push_str(",\"resource\":");
    write_string(resource, &mut out);
    out.push_str(",\"service\":");
    write_string(service, &mut out);
    out.push_str(&format!(",\"v\":{HASH_INPUT_VERSION}}}"));
    Ok(out)
}

/// The effect: `sha256:` and 64 lower-case hexadecimal digits.
pub fn effect_hash(
    service: &str,
    action: &str,
    resource: &str,
    op: &Value,
) -> Result<String, EffectError> {
    let text = canonical_input(service, action, resource, op)?;
    let digest = Sha256::digest(text.as_bytes());
    let mut out = String::with_capacity(7 + 64);
    out.push_str("sha256:");
    for b in digest {
        out.push_str(&format!("{b:02x}"));
    }
    Ok(out)
}

/// The effect of an `op` that comes as JSON text. The text is read with the strict rules.
pub fn effect_hash_text(
    service: &str,
    action: &str,
    resource: &str,
    op_json: &str,
) -> Result<String, EffectError> {
    let op = Op::parse(op_json)?;
    effect_hash(service, action, resource, op.as_value())
}

/// True when the text has the form of an effect: `sha256:` and 64 lower-case hexadecimal digits.
pub fn is_effect(text: &str) -> bool {
    text.strip_prefix("sha256:").is_some_and(|h| {
        h.len() == 64 && h.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    })
}

/// The short form of an effect for the screen: the first 8 hexadecimal digits, no prefix.
/// The screen shows `sha256:` before it. `None` when the text is not an effect.
pub fn short(effect: &str) -> Option<&str> {
    if is_effect(effect) { Some(&effect[7..15]) } else { None }
}

// ---------------------------------------------------------------- the view

/// A template over `op`. It is not part of the effect.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct View {
    /// One line, 1 to 128 characters.
    pub title: String,
    /// `[label, pointer]` pairs, 0 to 12. Each pointer names a string, an integer, a boolean or `null`.
    pub fields: Vec<(String, String)>,
    /// Pointer to the command text. It names a string of `op`.
    pub body: String,
    /// Label of the command, 1 to 48 characters. Absent: the phone shows a fixed label.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body_label: Option<String>,
}

/// One field of a resolved view.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ResolvedField {
    pub label: String,
    pub value: String,
}

/// The view with the values of `op`. The text is what the screen shows.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ResolvedView {
    pub title: String,
    pub fields: Vec<ResolvedField>,
    pub body: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub body_label: Option<String>,
}

fn is_line(text: &str, max_chars: usize) -> bool {
    let n = text.chars().count();
    n >= 1 && n <= max_chars && !text.chars().any(char::is_control)
}

/// Reads a view from JSON text. Structure and sizes only: use [`check_view`] for the pointers.
pub fn parse_view(text: &str) -> Result<View, EffectError> {
    serde_json::from_str::<View>(text).map_err(|_| EffectError::InvalidView("not a valid view"))
}

/// Looks up an RFC 6901 pointer. A pointer that is not valid or that names nothing gives `None`.
pub fn pointer<'a>(op: &'a Value, ptr: &str) -> Option<&'a Value> {
    if ptr.is_empty() {
        return Some(op);
    }
    let rest = ptr.strip_prefix('/')?;
    let mut cur = op;
    for raw in rest.split('/') {
        let token = unescape(raw)?;
        cur = match cur {
            Value::Object(map) => map.get(&token)?,
            Value::Array(items) => {
                // An index is `0` or digits with no zero at the start. `-` names no value.
                let ok = !token.is_empty()
                    && token.bytes().all(|b| b.is_ascii_digit())
                    && (token == "0" || !token.starts_with('0'));
                if !ok {
                    return None;
                }
                items.get(token.parse::<usize>().ok()?)?
            }
            _ => return None,
        };
    }
    Some(cur)
}

fn unescape(raw: &str) -> Option<String> {
    let mut out = String::with_capacity(raw.len());
    let mut chars = raw.chars();
    while let Some(c) = chars.next() {
        if c == '~' {
            match chars.next()? {
                '0' => out.push('~'),
                '1' => out.push('/'),
                _ => return None,
            }
        } else {
            out.push(c);
        }
    }
    Some(out)
}

fn field_text(v: &Value) -> Option<String> {
    match v {
        Value::String(s) => Some(s.clone()),
        Value::Number(_) | Value::Bool(_) | Value::Null => Some(v.to_string()),
        _ => None,
    }
}

/// Checks a view against `op`: the sizes, the text rules, and every pointer.
/// A pointer must exist. The body must name a string. A field must name a scalar.
pub fn check_view(op: &Value, view: &View) -> Result<(), EffectError> {
    resolve_view(op, view).map(|_| ())
}

impl View {
    /// Checks the sizes and the text rules of the view. It reads no pointer.
    pub fn check_shape(&self) -> Result<(), EffectError> {
        if !is_line(&self.title, MAX_VIEW_TITLE_CHARS) {
            return Err(EffectError::InvalidView("title is not one line of 1 to 128 characters"));
        }
        if self.fields.len() > MAX_VIEW_FIELDS {
            return Err(EffectError::InvalidView("more than 12 fields"));
        }
        if let Some(l) = &self.body_label
            && !is_line(l, MAX_VIEW_LABEL_CHARS)
        {
            return Err(EffectError::InvalidView(
                "body_label is not one line of 1 to 48 characters",
            ));
        }
        for (label, ptr) in &self.fields {
            if !is_line(label, MAX_VIEW_LABEL_CHARS) {
                return Err(EffectError::InvalidView(
                    "a label is not one line of 1 to 48 characters",
                ));
            }
            if ptr.len() > MAX_POINTER_BYTES {
                return Err(EffectError::InvalidView("a pointer is longer than 256 bytes"));
            }
        }
        if self.body.len() > MAX_POINTER_BYTES {
            return Err(EffectError::InvalidView("a pointer is longer than 256 bytes"));
        }
        Ok(())
    }
}

/// The view with the values of `op`. It runs the same checks as [`check_view`].
pub fn resolve_view(op: &Value, view: &View) -> Result<ResolvedView, EffectError> {
    view.check_shape()?;
    let mut fields = Vec::with_capacity(view.fields.len());
    for (label, ptr) in &view.fields {
        let value =
            pointer(op, ptr).ok_or(EffectError::InvalidView("a pointer names no value of op"))?;
        let value = field_text(value)
            .ok_or(EffectError::InvalidView("a field names an object or an array"))?;
        fields.push(ResolvedField { label: label.clone(), value });
    }
    let body = match pointer(op, &view.body) {
        Some(Value::String(s)) => s.clone(),
        Some(_) => return Err(EffectError::InvalidView("the body does not name a string")),
        None => return Err(EffectError::InvalidView("a pointer names no value of op")),
    };
    Ok(ResolvedView {
        title: view.title.clone(),
        fields,
        body,
        body_label: view.body_label.clone(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn op() -> Value {
        json!({"tool":"run_write_query","host":"prod","n":3,"ok":true,"sql":"INSERT 1","a/b":{"~":[10,20]}})
    }

    fn view(fields: &[(&str, &str)], body: &str) -> View {
        View {
            title: "Write".into(),
            fields: fields.iter().map(|(a, b)| (a.to_string(), b.to_string())).collect(),
            body: body.into(),
            body_label: Some("SQL".into()),
        }
    }

    /// The vectors of ProcurAuth 17.3. `$PROCURAUTH_CONFORMANCE/vectors/effect.json` is read when
    /// the variable is set. Otherwise the embedded copy is used.
    #[test]
    fn effect_vectors_match() {
        let embedded = include_str!("../tests/fixtures/effect-vectors.json");
        let external = std::env::var("PROCURAUTH_CONFORMANCE")
            .ok()
            .and_then(|d| std::fs::read_to_string(format!("{d}/vectors/effect.json")).ok());
        let from_env = external.is_some();
        let text = external.as_deref().unwrap_or(embedded);
        let file: Value = serde_json::from_str(text).unwrap();
        assert_eq!(file["v"], 1);
        let (mut v01, mut v02) = (0, 0);
        for vector in file["vectors"].as_array().unwrap() {
            let id = vector["id"].as_str().unwrap();
            let input = &vector["input"];
            let s = |k: &str| input[k].as_str().unwrap().to_string();
            // A float of the file stays a float in `Value`, so the V02 inputs are refused.
            let op = Op::from_value(input["op"].clone());
            match vector["group"].as_str().unwrap() {
                "V01" => {
                    v01 += 1;
                    let op = op.unwrap_or_else(|e| panic!("{id}: {e}"));
                    let canonical =
                        canonical_input(&s("service"), &s("action"), &s("resource"), op.as_value())
                            .unwrap();
                    assert_eq!(canonical, vector["canonical"].as_str().unwrap(), "{id}");
                    let effect =
                        effect_hash(&s("service"), &s("action"), &s("resource"), op.as_value());
                    assert_eq!(effect.unwrap(), vector["effect"].as_str().unwrap(), "{id}");
                }
                "V02" => {
                    v02 += 1;
                    assert_eq!(vector["error"], "invalid_op", "{id}");
                    let err = match op {
                        Err(e) => e,
                        Ok(op) => {
                            effect_hash(&s("service"), &s("action"), &s("resource"), op.as_value())
                                .unwrap_err()
                        }
                    };
                    assert_eq!(err.code(), "invalid_op", "{id}");
                }
                g => panic!("{id}: unknown group {g}"),
            }
        }
        assert!(v01 >= 15 && v02 >= 9, "{v01} {v02} (external copy: {from_env})");
    }

    #[test]
    fn jcs_rejects_float_and_integer_above_2_53() {
        for bad in [
            "1.5",
            "1.0",
            "1e2",
            "-0",
            "9007199254740992",
            "-9007199254740992",
            "18446744073709551616",
        ] {
            let v: Value = serde_json::from_str(&format!(r#"{{"x":{bad}}}"#)).unwrap();
            assert_eq!(jcs(&v).unwrap_err().code(), "invalid_op", "{bad}");
            assert!(Op::parse(&format!(r#"{{"x":{bad}}}"#)).is_err(), "{bad}");
        }
        for good in ["0", "-1", "9007199254740991", "-9007199254740991"] {
            let v: Value = serde_json::from_str(&format!(r#"{{"x":{good}}}"#)).unwrap();
            assert!(jcs(&v).is_ok(), "{good}");
        }
        // Nested floats are found too.
        assert!(Op::parse(r#"{"a":[{"b":0.5}]}"#).is_err());
        // The strict reader refuses a duplicate member name and an op that is not an object.
        assert_eq!(
            Op::parse(r#"{"a":1,"a":2}"#).unwrap_err(),
            EffectError::InvalidOp("duplicate member name")
        );
        assert!(Op::parse("[1,2]").is_err());
        assert!(Op::parse(r#"{"a":1} {"b":2}"#).is_err());
        let deep = format!("{}{}", "[".repeat(100), "]".repeat(100));
        assert!(Op::parse(&format!(r#"{{"a":{deep}}}"#)).is_err());
    }

    #[test]
    fn view_pointer_must_exist() {
        let op = op();
        // Existing pointers: a member, an escaped name (`~0`, `~1`), an array index.
        let ok =
            view(&[("Host", "/host"), ("N", "/n"), ("Ok", "/ok"), ("Item", "/a~1b/~0/1")], "/sql");
        let r = resolve_view(&op, &ok).unwrap();
        assert_eq!(r.fields[1].value, "3");
        assert_eq!(r.fields[2].value, "true");
        assert_eq!(r.fields[3].value, "20");
        assert_eq!(r.body, "INSERT 1");
        check_view(&op, &ok).unwrap();
        // A pointer that names nothing, in a field or in the body.
        for bad in
            ["/missing", "/a~1b/~0/2", "/a~1b/~0/01", "/a~1b/~0/-", "host", "/host/x", "/a~2"]
        {
            assert_eq!(
                check_view(&op, &view(&[("X", bad)], "/sql")).unwrap_err().code(),
                "invalid_view",
                "{bad}"
            );
        }
        assert!(check_view(&op, &view(&[], "/missing")).is_err());
        // The body names a string. A field names a scalar.
        assert!(check_view(&op, &view(&[], "/n")).is_err());
        assert!(check_view(&op, &view(&[("X", "/a~1b")], "/sql")).is_err());
        assert!(check_view(&op, &view(&[], "")).is_err());
        // Sizes and text rules.
        let mut many = view(&[], "/sql");
        many.fields = (0..13).map(|i| (format!("L{i}"), "/host".to_string())).collect();
        assert!(check_view(&op, &many).is_err());
        let mut bad_title = view(&[], "/sql");
        bad_title.title = "two\nlines".into();
        assert!(check_view(&op, &bad_title).is_err());
        let mut long = view(&[], "/sql");
        long.title = "x".repeat(129);
        assert!(check_view(&op, &long).is_err());
        assert!(parse_view(r#"{"title":"t","fields":[],"body":"/sql","x":1}"#).is_err());
        assert!(parse_view(r#"{"title":"t","fields":[["a","/host"]],"body":"/sql"}"#).is_ok());
    }

    #[test]
    fn effect_changes_with_service_action_resource_op_each() {
        let op = json!({"sql":"DELETE 1"});
        let base = effect_hash("svc", "data.write", "host:prod", &op).unwrap();
        assert!(is_effect(&base));
        assert_eq!(effect_hash("svc", "data.write", "host:prod", &op).unwrap(), base);
        for other in [
            effect_hash("svc2", "data.write", "host:prod", &op).unwrap(),
            effect_hash("svc", "data.read", "host:prod", &op).unwrap(),
            effect_hash("svc", "data.write", "host:stage", &op).unwrap(),
            effect_hash("svc", "data.write", "host:prod", &json!({"sql":"DELETE 2"})).unwrap(),
        ] {
            assert_ne!(other, base);
        }
        // The text of the field is not interchangeable between members.
        assert_ne!(
            effect_hash("a", "b", "c", &op).unwrap(),
            effect_hash("b", "a", "c", &op).unwrap()
        );
        assert_eq!(short(&base), Some(&base[7..15]));
        assert_eq!(short("sha256:xyz"), None);
        assert_eq!(effect_hash("s", "a", "r", &json!([1])).unwrap_err().code(), "invalid_op");
    }
}
