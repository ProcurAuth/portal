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

/// Parse the file. `allowed_blocks` is the closed list of block names without a label.
/// `labeled_blocks` is the closed list of block names that take a label.
pub fn parse(
    raw: &str,
    allowed_blocks: &[&str],
    labeled_blocks: &[&str],
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
        let mut values: HashMap<String, Lit> = HashMap::new();
        loop {
            match tokens.get(i) {
                Some(Token::RBrace) => {
                    i += 1;
                    break;
                }
                Some(Token::Ident(key)) => {
                    let key = key.clone();
                    i += 1;
                    match tokens.get(i) {
                        Some(Token::Equal) => i += 1,
                        other => {
                            return Err(format!("expected '=' after {path}.{key}, found {other:?}"))
                        }
                    }
                    let value = match tokens.get(i) {
                        Some(Token::String(v)) => Lit::Str(v.clone()),
                        Some(Token::Bare(v)) => Lit::Int(v.clone()),
                        Some(Token::Ident(v)) => Lit::Word(v.clone()),
                        Some(Token::LBracket) => {
                            let (items, next) = list(&tokens, i + 1, &path, &key)?;
                            i = next - 1;
                            Lit::List(items)
                        }
                        other => {
                            return Err(format!(
                                "expected a value for {path}.{key}, found {other:?}"
                            ))
                        }
                    };
                    i += 1;
                    if values.insert(key.clone(), value).is_some() {
                        return Err(format!("duplicate configuration key: {path}.{key}"));
                    }
                }
                Some(Token::LBrace) => return Err("nested HCL blocks are not supported".into()),
                None => return Err(format!("unterminated block: {path}")),
                other => return Err(format!("unexpected token in block {path}: {other:?}")),
            }
        }
        match label {
            Some(label) => document.labeled.push(Labeled {
                kind: block,
                label,
                values,
            }),
            None => {
                for (key, value) in values {
                    document.values.insert((block.clone(), key), value);
                }
            }
        }
    }
    Ok(document)
}

/// The items of a list that starts after `[` at `start`. The items are quoted strings, separated
/// by commas. A comma after the last item is permitted. Returns the items and the index after `]`.
fn list(
    tokens: &[Token],
    start: usize,
    path: &str,
    key: &str,
) -> Result<(Vec<String>, usize), String> {
    let mut items = Vec::new();
    let mut i = start;
    loop {
        match tokens.get(i) {
            Some(Token::RBracket) => return Ok((items, i + 1)),
            Some(Token::String(item)) => {
                items.push(item.clone());
                i += 1;
                match tokens.get(i) {
                    Some(Token::Comma) => i += 1,
                    Some(Token::RBracket) => {}
                    other => {
                        return Err(format!(
                            "expected ',' or ']' in the list {path}.{key}, found {other:?}"
                        ))
                    }
                }
            }
            None => return Err(format!("unterminated list: {path}.{key}")),
            other => {
                return Err(format!(
                    "expected a quoted string in the list {path}.{key}, found {other:?}"
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
        super::parse(raw, blocks, LABELED)
    }

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
        assert!(parse(r#"user "a" {} user "a" {}"#, BLOCKS).is_err(), "duplicate label");
        assert!(parse(r#"user "a" {} user "b" {}"#, BLOCKS).is_ok());
        assert!(parse("user {}", BLOCKS).is_err(), "a label is mandatory");
        assert!(parse(r#"server "x" {}"#, BLOCKS).is_err(), "server takes no label");
        assert!(parse(r#"other "x" {}"#, BLOCKS).is_err());
        assert!(parse(r#"user "a" { g = ["x" "y"] }"#, BLOCKS).is_err(), "a comma is mandatory");
        assert!(parse(r#"user "a" { g = [1] }"#, BLOCKS).is_err(), "items are quoted");
        assert!(parse(r#"user "a" { g = ["x" }"#, BLOCKS).is_err());
        assert!(parse(r#"user "a" { g = [] g = [] }"#, BLOCKS).is_err());
    }
}
