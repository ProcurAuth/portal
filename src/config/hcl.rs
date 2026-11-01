//! The small HCL subset of the configuration file: blocks of `key = value` lines.
//!
//! A block has a name (`server { ... }`), or a name and a label (`user "alice" { ... }`). A value
//! is a scalar or a list of quoted strings (`["a", "b"]`).
//!
//! The parser keeps the written form of each scalar (quoted text, bare number or bare word), so
//! the typed layer can refuse a number in quotes and a word without quotes.

use std::collections::{HashMap, HashSet};

/// A scalar as the file writes it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Lit {
    /// A quoted string: `"text"`.
    Str(String),
    /// A bare token that starts with a digit: `4181`.
    Int(String),
    /// A bare word: `true`.
    Word(String),
    /// A list of quoted strings: `["a", "b"]`.
    List(Vec<String>),
    /// A list that holds another kind of item: `["a", { grant = "b" }]`.
    Items(Vec<Lit>),
    /// An object: `{ grant = "a", limits = { max_rows = 10 } }`. The members keep the order of the
    /// file. A name occurs one time.
    Object(Vec<(String, Lit)>),
}

impl Lit {
    /// The text as the file wrote it, for an error message.
    pub fn shown(&self) -> String {
        match self {
            Lit::Str(text) => format!("{text:?}"),
            Lit::Int(text) | Lit::Word(text) => text.clone(),
            Lit::List(items) => format!(
                "[{}]",
                items
                    .iter()
                    .map(|item| format!("{item:?}"))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            Lit::Items(items) => format!(
                "[{}]",
                items.iter().map(Lit::shown).collect::<Vec<_>>().join(", ")
            ),
            Lit::Object(members) => format!(
                "{{ {} }}",
                members
                    .iter()
                    .map(|(name, value)| format!("{name} = {}", value.shown()))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        }
    }
}

/// A block with a label, for example `user "alice" { ... }`.
#[derive(Debug, Default, Clone)]
pub struct Labeled {
    /// The name of the block: `user`.
    pub kind: String,
    /// The label: `alice`.
    pub label: String,
    pub values: HashMap<String, Lit>,
    /// The blocks with a label inside this block, for example `key "nightly report" { ... }` in a
    /// `user` block. They are in the order of the file.
    pub children: Vec<Labeled>,
}

/// What the file says: the blocks that are present, and the key of each block.
#[derive(Debug, Default)]
pub struct Document {
    pub blocks: HashSet<String>,
    pub values: HashMap<(String, String), Lit>,
    /// The blocks with a label, in the order of the file.
    pub labeled: Vec<Labeled>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Token {
    Ident(String),
    String(String),
    Bare(String),
    LBrace,
    RBrace,
    LBracket,
    RBracket,
    Comma,
    Equal,
}

/// The deepest nesting of arrays and objects in a value.
const MAX_VALUE_DEPTH: usize = 6;

/// Parse the file. `allowed_blocks` is the closed list of block names without a label.
/// `labeled_blocks` is the closed list of block names that take a label. `nested_blocks` lists
/// the pairs `(parent, child)`: a block `parent` can hold blocks `child` with a label.
/// `nested_plain` lists the pairs `(parent, child)`: a block `parent` can hold a block `child`
/// without a label. The keys of such a block are in the block `parent.child` of the document.
pub fn parse(
    raw: &str,
    allowed_blocks: &[&str],
    labeled_blocks: &[&str],
    nested_blocks: &[(&str, &str)],
    nested_plain: &[(&str, &str)],
) -> Result<Document, String> {
    let tokens = tokenize(raw)?;
    let mut document = Document::default();
    let mut i = 0usize;
    while i < tokens.len() {
        let block = match tokens.get(i) {
            Some(Token::Ident(s)) => s.clone(),
            other => return Err(format!("expected a block name, found {other:?}")),
        };
        i += 1;
        let label = match tokens.get(i) {
            Some(Token::String(label)) => {
                i += 1;
                Some(label.clone())
            }
            _ => None,
        };
        match &label {
            None if labeled_blocks.contains(&block.as_str()) => {
                return Err(format!(
                    "the block {block} needs a label: write {block} \"<name>\" {{ ... }}"
                ));
            }
            None if !allowed_blocks.contains(&block.as_str()) => {
                return Err(format!("unknown configuration block: {block}"));
            }
            None => {
                if !document.blocks.insert(block.clone()) {
                    return Err(format!("duplicate configuration block: {block}"));
                }
            }
            Some(label) if labeled_blocks.contains(&block.as_str()) => {
                if document
                    .labeled
                    .iter()
                    .any(|other| other.kind == block && other.label == *label)
                {
                    return Err(format!("duplicate configuration block: {block} {label:?}"));
                }
            }
            Some(label) if allowed_blocks.contains(&block.as_str()) => {
                return Err(format!(
                    "the block {block} takes no label, found {block} {label:?}"
                ));
            }
            Some(_) => return Err(format!("unknown configuration block: {block}")),
        }
        let path = match &label {
            Some(label) => format!("{block}.{label}"),
            None => block.clone(),
        };
        match tokens.get(i) {
            Some(Token::LBrace) => i += 1,
            other => return Err(format!("expected '{{' after {path}, found {other:?}")),
        }
        let nested: Vec<&str> = nested_blocks
            .iter()
            .filter(|(parent, _)| *parent == block)
            .map(|(_, child)| *child)
            .collect();
        let plain: Vec<&str> = nested_plain
            .iter()
            .filter(|(parent, _)| *parent == block)
            .map(|(_, child)| *child)
            .collect();
        let Body {
            values,
            children,
            plain: plain_children,
        } = body(&tokens, &mut i, &path, &nested, &plain)?;
        match label {
            Some(label) => document.labeled.push(Labeled {
                kind: block,
                label,
                values,
                children,
            }),
            None => {
                for (key, value) in values {
                    document.values.insert((block.clone(), key), value);
                }
                for (child, child_values) in plain_children {
                    let name = format!("{block}.{child}");
                    document.blocks.insert(name.clone());
                    for (key, value) in child_values {
                        document.values.insert((name.clone(), key), value);
                    }
                }
                // A block with a label inside a block without a label stands at the top of the
                // list, with its own kind.
                document.labeled.extend(children);
            }
        }
    }
    Ok(document)
}

/// What a block holds.
struct Body {
    values: HashMap<String, Lit>,
    /// The blocks with a label, in the order of the file.
    children: Vec<Labeled>,
    /// The blocks without a label, with their keys, in the order of the file.
    plain: Vec<(String, HashMap<String, Lit>)>,
}

/// The body of a block, after its `{` and up to its `}`: the `key = value` lines, the blocks
/// with a label that the block can hold (`nested`) and the blocks without a label that it can
/// hold (`nested_plain`).
fn body(
    tokens: &[Token],
    i: &mut usize,
    path: &str,
    nested: &[&str],
    nested_plain: &[&str],
) -> Result<Body, String> {
    let mut values: HashMap<String, Lit> = HashMap::new();
    let mut children: Vec<Labeled> = Vec::new();
    let mut plain: Vec<(String, HashMap<String, Lit>)> = Vec::new();
    loop {
        match tokens.get(*i) {
            Some(Token::RBrace) => {
                *i += 1;
                return Ok(Body {
                    values,
                    children,
                    plain,
                });
            }
            Some(Token::Ident(key)) => {
                let key = key.clone();
                *i += 1;
                if matches!(tokens.get(*i), Some(Token::LBrace))
                    && nested_plain.contains(&key.as_str())
                {
                    // A block without a label inside the block.
                    if plain.iter().any(|(name, _)| *name == key) {
                        return Err(format!("duplicate configuration block: {path}.{key}"));
                    }
                    *i += 1;
                    let child_path = format!("{path}.{key}");
                    let inner = body(tokens, i, &child_path, &[], &[])?;
                    plain.push((key, inner.values));
                    continue;
                }
                if let Some(Token::String(label)) = tokens.get(*i) {
                    // A block with a label inside the block.
                    if !nested.contains(&key.as_str()) {
                        return Err(format!("nested HCL blocks are not supported: {path}.{key}"));
                    }
                    let label = label.clone();
                    *i += 1;
                    if children.iter().any(|c| c.kind == key && c.label == label) {
                        return Err(format!(
                            "duplicate configuration block: {path}.{key} {label:?}"
                        ));
                    }
                    let child_path = format!("{path}.{key}.{label}");
                    match tokens.get(*i) {
                        Some(Token::LBrace) => *i += 1,
                        other => {
                            return Err(format!(
                                "expected '{{' after {child_path}, found {other:?}"
                            ))
                        }
                    }
                    let inner = body(tokens, i, &child_path, &[], &[])?;
                    debug_assert!(inner.children.is_empty() && inner.plain.is_empty());
                    children.push(Labeled {
                        kind: key,
                        label,
                        values: inner.values,
                        children: Vec::new(),
                    });
                    continue;
                }
                match tokens.get(*i) {
                    Some(Token::Equal) => *i += 1,
                    other => {
                        return Err(format!("expected '=' after {path}.{key}, found {other:?}"))
                    }
                }
                let value = value(tokens, i, path, &key, 0)?;
                if values.insert(key.clone(), value).is_some() {
                    return Err(format!("duplicate configuration key: {path}.{key}"));
                }
            }
            Some(Token::LBrace) => return Err("nested HCL blocks are not supported".into()),
            None => return Err(format!("unterminated block: {path}")),
            other => return Err(format!("unexpected token in block {path}: {other:?}")),
        }
    }
}

/// One value at `tokens[*i]`: a scalar, an array or an object. The index goes after the value.
fn value(
    tokens: &[Token],
    i: &mut usize,
    path: &str,
    key: &str,
    depth: usize,
) -> Result<Lit, String> {
    if depth > MAX_VALUE_DEPTH {
        return Err(format!("{path}.{key}: the value is nested too deep"));
    }
    let lit = match tokens.get(*i) {
        Some(Token::String(v)) => {
            *i += 1;
            Lit::Str(v.clone())
        }
        Some(Token::Bare(v)) => {
            *i += 1;
            Lit::Int(v.clone())
        }
        Some(Token::Ident(v)) => {
            *i += 1;
            Lit::Word(v.clone())
        }
        Some(Token::LBracket) => {
            *i += 1;
            array(tokens, i, path, key, depth)?
        }
        Some(Token::LBrace) => {
            *i += 1;
            object(tokens, i, path, key, depth)?
        }
        other => {
            return Err(format!(
                "expected a value for {path}.{key}, found {other:?}"
            ))
        }
    };
    Ok(lit)
}

/// The items of an array after `[`. The items are separated by commas. A comma after the last
/// item is permitted. An array of quoted strings is a `List`, any other array is `Items`.
fn array(
    tokens: &[Token],
    i: &mut usize,
    path: &str,
    key: &str,
    depth: usize,
) -> Result<Lit, String> {
    let mut items: Vec<Lit> = Vec::new();
    loop {
        match tokens.get(*i) {
            Some(Token::RBracket) => {
                *i += 1;
                break;
            }
            None => return Err(format!("unterminated list: {path}.{key}")),
            _ => {
                items.push(value(tokens, i, path, key, depth + 1)?);
                match tokens.get(*i) {
                    Some(Token::Comma) => *i += 1,
                    Some(Token::RBracket) => {}
                    other => {
                        return Err(format!(
                            "expected ',' or ']' in the list {path}.{key}, found {other:?}"
                        ))
                    }
                }
            }
        }
    }
    if items.iter().all(|item| matches!(item, Lit::Str(_))) {
        Ok(Lit::List(
            items
                .into_iter()
                .filter_map(|item| match item {
                    Lit::Str(text) => Some(text),
                    _ => None,
                })
                .collect(),
        ))
    } else {
        Ok(Lit::Items(items))
    }
}

/// The members of an object after `{`: `name = value`, with an optional comma after each.
fn object(
    tokens: &[Token],
    i: &mut usize,
    path: &str,
    key: &str,
    depth: usize,
) -> Result<Lit, String> {
    let mut members: Vec<(String, Lit)> = Vec::new();
    loop {
        match tokens.get(*i) {
            Some(Token::RBrace) => {
                *i += 1;
                return Ok(Lit::Object(members));
            }
            Some(Token::Ident(name)) => {
                let name = name.clone();
                *i += 1;
                match tokens.get(*i) {
                    Some(Token::Equal) => *i += 1,
                    other => {
                        return Err(format!(
                            "expected '=' after {name} in the object {path}.{key}, found {other:?}"
                        ))
                    }
                }
                let member = value(tokens, i, path, &format!("{key}.{name}"), depth + 1)?;
                if members.iter().any(|(other, _)| *other == name) {
                    return Err(format!(
                        "duplicate member {name} in the object {path}.{key}"
                    ));
                }
                members.push((name, member));
                if let Some(Token::Comma) = tokens.get(*i) {
                    *i += 1;
                }
            }
            None => return Err(format!("unterminated object: {path}.{key}")),
            other => {
                return Err(format!(
                    "expected a member name in the object {path}.{key}, found {other:?}"
                ))
            }
        }
    }
}

fn tokenize(raw: &str) -> Result<Vec<Token>, String> {
    let chars: Vec<char> = raw.chars().collect();
    let mut out = Vec::new();
    let mut i = 0usize;
    while i < chars.len() {
        let c = chars[i];
        if c.is_whitespace() {
            i += 1;
            continue;
        }
        if c == '#' {
            while i < chars.len() && chars[i] != '\n' {
                i += 1;
            }
            continue;
        }
        if c == '/' && i + 1 < chars.len() && chars[i + 1] == '/' {
            i += 2;
            while i < chars.len() && chars[i] != '\n' {
                i += 1;
            }
            continue;
        }
        match c {
            '{' => {
                out.push(Token::LBrace);
                i += 1;
            }
            '}' => {
                out.push(Token::RBrace);
                i += 1;
            }
            '=' => {
                out.push(Token::Equal);
                i += 1;
            }
            '[' => {
                out.push(Token::LBracket);
                i += 1;
            }
            ']' => {
                out.push(Token::RBracket);
                i += 1;
            }
            ',' => {
                out.push(Token::Comma);
                i += 1;
            }
            '"' => {
                i += 1;
                let mut value = String::new();
                let mut closed = false;
                while i < chars.len() {
                    match chars[i] {
                        '"' => {
                            i += 1;
                            closed = true;
                            break;
                        }
                        '\\' => {
                            i += 1;
                            if i >= chars.len() {
                                return Err("unterminated escape in string".into());
                            }
                            match chars[i] {
                                '"' => value.push('"'),
                                '\\' => value.push('\\'),
                                'n' => value.push('\n'),
                                'r' => value.push('\r'),
                                't' => value.push('\t'),
                                other => return Err(format!("unsupported escape: \\{other}")),
                            }
                            i += 1;
                        }
                        ch => {
                            if ch == '\0' {
                                return Err("NUL is forbidden in configuration strings".into());
                            }
                            value.push(ch);
                            i += 1;
                        }
                    }
                }
                if !closed {
                    return Err("unterminated quoted string".into());
                }
                out.push(Token::String(value));
            }
            _ if is_ident_start(c) => {
                let start = i;
                i += 1;
                while i < chars.len() && is_ident_continue(chars[i]) {
                    i += 1;
                }
                out.push(Token::Ident(chars[start..i].iter().collect()));
            }
            _ if c.is_ascii_digit() => {
                let start = i;
                i += 1;
                while i < chars.len()
                    && !chars[i].is_whitespace()
                    && !matches!(chars[i], '{' | '}' | '=' | '[' | ']' | ',')
                {
                    i += 1;
                }
                out.push(Token::Bare(chars[start..i].iter().collect()));
            }
            other => return Err(format!("unexpected character in HCL: {other:?}")),
        }
    }
    Ok(out)
}

fn is_ident_start(c: char) -> bool {
    c.is_ascii_alphabetic() || c == '_'
}

fn is_ident_continue(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '_' | '-')
}

#[cfg(test)]
mod tests {
    use super::*;

    const BLOCKS: &[&str] = &["server", "metrics"];
    const LABELED: &[&str] = &["user"];

    fn parse(raw: &str, blocks: &[&str]) -> Result<Document, String> {
        super::parse(raw, blocks, LABELED, NESTED, &[])
    }

    const NESTED: &[(&str, &str)] = &[("user", "key")];

    #[test]
    fn keeps_the_written_form_of_each_scalar() {
        let doc = parse(r#"server { a = "4" b = 4 c = true }"#, BLOCKS).unwrap();
        let get = |k: &str| doc.values[&("server".to_string(), k.to_string())].clone();
        assert_eq!(get("a"), Lit::Str("4".into()));
        assert_eq!(get("b"), Lit::Int("4".into()));
        assert_eq!(get("c"), Lit::Word("true".into()));
    }

    #[test]
    fn an_empty_block_is_present() {
        let doc = parse("metrics {}", BLOCKS).unwrap();
        assert!(doc.blocks.contains("metrics"));
        assert!(doc.values.is_empty());
    }

    #[test]
    fn rejects_duplicates_unknown_blocks_and_nesting() {
        assert!(parse("server { workers = 1 workers = 2 }", BLOCKS).is_err());
        assert!(parse("unknown {}", BLOCKS).is_err());
        assert!(parse("server {} server {}", BLOCKS).is_err());
        assert!(parse("server { a { } }", BLOCKS).is_err());
        assert!(parse("server { a = \"x }", BLOCKS).is_err());
    }

    #[test]
    fn a_labeled_block_keeps_its_label_and_its_list() {
        let doc = parse(
            r#"user "alice" { grants = ["access on service:a", "x on service:b",] }
               user "bob" { grants = [] }"#,
            BLOCKS,
        )
        .unwrap();
        assert_eq!(doc.labeled.len(), 2);
        assert_eq!(doc.labeled[0].label, "alice");
        assert_eq!(
            doc.labeled[0].values["grants"],
            Lit::List(vec!["access on service:a".into(), "x on service:b".into()])
        );
        assert_eq!(doc.labeled[1].values["grants"], Lit::List(vec![]));
        assert!(doc.values.is_empty() && doc.blocks.is_empty());
    }

    #[test]
    fn labels_are_closed_and_unique() {
        assert!(
            parse(r#"user "a" {} user "a" {}"#, BLOCKS).is_err(),
            "duplicate label"
        );
        assert!(parse(r#"user "a" {} user "b" {}"#, BLOCKS).is_ok());
        assert!(parse("user {}", BLOCKS).is_err(), "a label is mandatory");
        assert!(
            parse(r#"server "x" {}"#, BLOCKS).is_err(),
            "server takes no label"
        );
        assert!(parse(r#"other "x" {}"#, BLOCKS).is_err());
        assert!(
            parse(r#"user "a" { g = ["x" "y"] }"#, BLOCKS).is_err(),
            "a comma is mandatory"
        );
        assert!(
            parse(r#"user "a" { g = [1 2] }"#, BLOCKS).is_err(),
            "a comma is mandatory"
        );
        assert!(parse(r#"user "a" { g = ["x" }"#, BLOCKS).is_err());
        assert!(parse(r#"user "a" { g = [] g = [] }"#, BLOCKS).is_err());
    }

    #[test]
    fn an_array_can_hold_strings_and_objects_and_an_object_keeps_the_order_of_its_members() {
        let doc = parse(
            r#"user "a" {
                 grants = [
                   "access on service:x",
                   { grant = "data.read on service:y", limits = { max_rows = 10, requests = [ { amount = 300, window = "minute" } ] } },
                 ]
               }"#,
            BLOCKS,
        )
        .unwrap();
        let Lit::Items(items) = &doc.labeled[0].values["grants"] else {
            panic!("an array with an object is `Items`");
        };
        assert_eq!(items[0], Lit::Str("access on service:x".into()));
        let Lit::Object(members) = &items[1] else {
            panic!("the second item is an object");
        };
        assert_eq!(members[0].0, "grant");
        assert_eq!(members[1].0, "limits");
        let Lit::Object(limits) = &members[1].1 else {
            panic!("the limits are an object");
        };
        assert_eq!(limits[0], ("max_rows".to_string(), Lit::Int("10".into())));
        assert!(matches!(&limits[1].1, Lit::Items(rows) if rows.len() == 1));
        for bad in [
            r#"user "a" { g = { a = 1, a = 2 } }"#,
            r#"user "a" { g = { a 1 } }"#,
            r#"user "a" { g = { a = 1 "#,
            r#"user "a" { g = [ { a = 1 ] }"#,
            r#"user "a" { g = [[[[[[[[1]]]]]]]] }"#,
        ] {
            assert!(parse(bad, BLOCKS).is_err(), "{bad}");
        }
        assert!(parse(r#"user "a" { g = [] }"#, BLOCKS).is_ok());
    }

    #[test]
    fn a_user_block_holds_blocks_key_with_a_label_and_nothing_else() {
        let doc = parse(
            r#"user "carol" {
                 grants = ["access on service:x"]
                 key "nightly report" { key_env = "CAROL_KEY" grants = [] }
                 key "backup check" { key_sha256_file = "/run/secrets/sum" }
               }"#,
            BLOCKS,
        )
        .unwrap();
        let carol = &doc.labeled[0];
        assert_eq!(carol.children.len(), 2);
        assert_eq!(carol.children[0].kind, "key");
        assert_eq!(carol.children[0].label, "nightly report");
        assert_eq!(
            carol.children[0].values["key_env"],
            Lit::Str("CAROL_KEY".into())
        );
        assert_eq!(carol.children[1].label, "backup check");
        for bad in [
            r#"user "a" { key "x" { } key "x" { } }"#,
            r#"user "a" { other "x" { } }"#,
            r#"user "a" { key "x" { key "y" { } } }"#,
            r#"user "a" { key "x" }"#,
            r#"user "a" { key "x" { a = 1 "#,
            r#"server { key "x" { } }"#,
        ] {
            assert!(parse(bad, BLOCKS).is_err(), "{bad}");
        }
        // The same name in two users is permitted (portal 3.4c).
        assert!(parse(
            r#"user "a" { key "x" { } } user "b" { key "x" { } }"#,
            BLOCKS
        )
        .is_ok());
    }
}
