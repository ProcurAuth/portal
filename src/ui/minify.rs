//! A small, deterministic minifier for the stylesheet and the script that every page loads.
//!
//! The transform removes comments and layout, and it gives short names to the local names of the
//! script. It needs no tool at build time. It is NOT a security measure. Anyone can read the
//! result and run a formatter on it. The goal is smaller files, and files that do not carry the
//! comments or the long names of the source.
//!
//! The script minifier is a tokenizer, not a parser. It accepts only the subset of JavaScript that
//! the shipped script uses: no template literal, no regular expression literal. For any other text,
//! [`script`] returns the error. The tests prove that the shipped script is inside the subset.

use std::collections::{HashMap, HashSet};

/// Removes the comments and the layout of a stylesheet. The text of a string stays as it is.
pub fn stylesheet(source: &str) -> String {
    let mut out = String::with_capacity(source.len() / 2);
    let mut chars = source.chars().peekable();
    let mut pending_space = false;
    while let Some(c) = chars.next() {
        match c {
            '/' if chars.peek() == Some(&'*') => {
                chars.next();
                let mut last = '\0';
                for next in chars.by_ref() {
                    if last == '*' && next == '/' {
                        break;
                    }
                    last = next;
                }
                // A comment separates two words, like a space.
                pending_space = true;
            }
            c if c.is_whitespace() => pending_space = true,
            '"' | '\'' => {
                flush_space(&mut out, &mut pending_space, c);
                out.push(c);
                while let Some(next) = chars.next() {
                    out.push(next);
                    if next == '\\' {
                        if let Some(escaped) = chars.next() {
                            out.push(escaped);
                        }
                    } else if next == c {
                        break;
                    }
                }
            }
            c => {
                flush_space(&mut out, &mut pending_space, c);
                out.push(c);
            }
        }
    }
    out
}

/// Writes one space before `next` when the layout needs it.
fn flush_space(out: &mut String, pending: &mut bool, next: char) {
    if *pending {
        let after_mark = matches!(out.chars().last(), None | Some('{' | '}' | ';' | ','));
        let before_mark = matches!(next, '{' | '}' | ';' | ',');
        if !after_mark && !before_mark {
            out.push(' ');
        }
    }
    *pending = false;
}

#[derive(Clone, Debug, PartialEq)]
enum Kind {
    Word,
    Number,
    Text,
    Mark,
}

#[derive(Clone, Debug)]
struct Token {
    kind: Kind,
    text: String,
    /// A line end separates this token from the one before.
    line_before: bool,
}

const MARKS: &[&str] = &[
    ">>>=", "===", "!==", "**=", "<<=", ">>=", ">>>", "...", "&&=", "||=", "??=", "=>", "==", "!=",
    "<=", ">=", "&&", "||", "??", "?.", "++", "--", "+=", "-=", "*=", "/=", "%=", "&=", "|=", "^=",
    "<<", ">>", "**",
];

const RESERVED: &[&str] = &[
    "break",
    "case",
    "catch",
    "class",
    "const",
    "continue",
    "debugger",
    "default",
    "delete",
    "do",
    "else",
    "enum",
    "export",
    "extends",
    "false",
    "finally",
    "for",
    "function",
    "if",
    "import",
    "in",
    "instanceof",
    "let",
    "new",
    "null",
    "return",
    "static",
    "super",
    "switch",
    "this",
    "throw",
    "true",
    "try",
    "typeof",
    "var",
    "void",
    "while",
    "with",
    "yield",
    "await",
    "async",
    "of",
    "get",
    "set",
    "undefined",
    "arguments",
];

fn tokenize(source: &str) -> Result<Vec<Token>, String> {
    let chars: Vec<char> = source.chars().collect();
    let mut tokens: Vec<Token> = Vec::new();
    let mut i = 0;
    let mut line_before = false;
    while i < chars.len() {
        let c = chars[i];
        if c == '\n' || c == '\r' {
            line_before = true;
            i += 1;
        } else if c.is_whitespace() {
            i += 1;
        } else if c == '/' && chars.get(i + 1) == Some(&'/') {
            while i < chars.len() && chars[i] != '\n' {
                i += 1;
            }
        } else if c == '/' && chars.get(i + 1) == Some(&'*') {
            i += 2;
            while i < chars.len() && !(chars[i] == '*' && chars.get(i + 1) == Some(&'/')) {
                if chars[i] == '\n' {
                    line_before = true;
                }
                i += 1;
            }
            i += 2;
        } else if c == '"' || c == '\'' {
            let start = i;
            i += 1;
            while i < chars.len() && chars[i] != c {
                if chars[i] == '\n' {
                    return Err("a string with a line end".into());
                }
                i += if chars[i] == '\\' { 2 } else { 1 };
            }
            i += 1;
            if i > chars.len() {
                return Err("a string that does not end".into());
            }
            tokens.push(Token {
                kind: Kind::Text,
                text: chars[start..i].iter().collect(),
                line_before: std::mem::take(&mut line_before),
            });
        } else if c == '`' {
            return Err("a template literal".into());
        } else if c.is_ascii_digit() {
            let start = i;
            while i < chars.len() && (chars[i].is_ascii_alphanumeric() || chars[i] == '.') {
                i += 1;
            }
            tokens.push(Token {
                kind: Kind::Number,
                text: chars[start..i].iter().collect(),
                line_before: std::mem::take(&mut line_before),
            });
        } else if c.is_ascii_alphabetic() || c == '_' || c == '$' {
            let start = i;
            while i < chars.len()
                && (chars[i].is_ascii_alphanumeric() || chars[i] == '_' || chars[i] == '$')
            {
                i += 1;
            }
            tokens.push(Token {
                kind: Kind::Word,
                text: chars[start..i].iter().collect(),
                line_before: std::mem::take(&mut line_before),
            });
        } else if !c.is_ascii() {
            return Err("a non-ASCII character outside a string".into());
        } else {
            if c == '/' {
                let division = tokens.last().is_some_and(|t| match t.kind {
                    Kind::Word => !matches!(t.text.as_str(), "return" | "typeof" | "case"),
                    Kind::Number | Kind::Text => true,
                    Kind::Mark => matches!(t.text.as_str(), ")" | "]"),
                });
                if !division {
                    return Err("a regular expression literal".into());
                }
            }
            let rest: String = chars[i..chars.len().min(i + 4)].iter().collect();
            let text = MARKS
                .iter()
                .find(|mark| rest.starts_with(**mark))
                .map_or_else(|| c.to_string(), |mark| (*mark).to_string());
            i += text.chars().count();
            tokens.push(Token {
                kind: Kind::Mark,
                text,
                line_before: std::mem::take(&mut line_before),
            });
        }
    }
    Ok(tokens)
}

/// Removes the comments and the layout of a script, and gives short names to its local names.
/// The result has the same behavior as the source, for the subset in the header of this module.
pub fn script(source: &str) -> Result<String, String> {
    let mut tokens = tokenize(source)?;
    rename_locals(&mut tokens);
    Ok(join(&tokens))
}

/// The names that a script declares: `var`, `let` and `const` names, the names and the parameters
/// of functions, and the name of a `catch`. The transform does not rename a name that is the name of
/// a property (after a dot) or the key of an object literal. If the script uses a form that the
/// transform does not understand, it renames nothing.
fn rename_locals(tokens: &mut [Token]) {
    let words: HashSet<&str> = tokens
        .iter()
        .filter(|t| t.kind == Kind::Word)
        .map(|t| t.text.as_str())
        .collect();
    if [
        "class", "with", "eval", "import", "export", "async", "await",
    ]
    .iter()
    .any(|word| words.contains(word))
        || tokens.iter().any(|t| t.text == "=>")
    {
        return;
    }
    let mut declared: Vec<String> = Vec::new();
    fn add(name: &str, declared: &mut Vec<String>) {
        if !RESERVED.contains(&name) && !declared.iter().any(|d| d == name) {
            declared.push(name.to_string());
        }
    }
    let mut i = 0;
    while i < tokens.len() {
        let text = tokens[i].text.as_str();
        let is_word = tokens[i].kind == Kind::Word;
        if is_word && matches!(text, "var" | "let" | "const") {
            match tokens.get(i + 1) {
                Some(next) if next.kind == Kind::Word => {
                    let name = next.text.clone();
                    add(&name, &mut declared);
                    // A declaration with a comma declares more names. The transform does not
                    // follow it: it gives up.
                    let mut depth = 0i32;
                    let mut j = i + 2;
                    while j < tokens.len() && !(depth == 0 && tokens[j].text == ";") {
                        match tokens[j].text.as_str() {
                            "(" | "[" | "{" => depth += 1,
                            ")" | "]" | "}" => depth -= 1,
                            "," if depth == 0 => return,
                            _ => {}
                        }
                        if depth < 0 {
                            break;
                        }
                        j += 1;
                    }
                }
                _ => return,
            }
        } else if is_word && text == "function" {
            let mut j = i + 1;
            if tokens.get(j).is_some_and(|t| t.kind == Kind::Word) {
                let name = tokens[j].text.clone();
                add(&name, &mut declared);
                j += 1;
            }
            if tokens.get(j).map(|t| t.text.as_str()) != Some("(") {
                return;
            }
            j += 1;
            while j < tokens.len() && tokens[j].text != ")" {
                if tokens[j].kind == Kind::Word {
                    let name = tokens[j].text.clone();
                    add(&name, &mut declared);
                } else if tokens[j].text != "," {
                    return;
                }
                j += 1;
            }
        } else if is_word && text == "catch" {
            if tokens.get(i + 1).map(|t| t.text.as_str()) == Some("(")
                && tokens.get(i + 2).is_some_and(|t| t.kind == Kind::Word)
            {
                let name = tokens[i + 2].text.clone();
                add(&name, &mut declared);
            } else {
                return;
            }
        }
        i += 1;
    }
    if declared.is_empty() {
        return;
    }
    // The new names avoid every word of the source. A word that is not declared is a global or a
    // property name, and it must keep its meaning.
    let mut used: HashSet<String> = tokens
        .iter()
        .filter(|t| t.kind == Kind::Word)
        .map(|t| t.text.clone())
        .collect();
    let mut counts: HashMap<&str, usize> = HashMap::new();
    for token in tokens.iter().filter(|t| t.kind == Kind::Word) {
        *counts.entry(token.text.as_str()).or_insert(0) += 1;
    }
    declared.sort_by(|a, b| counts[b.as_str()].cmp(&counts[a.as_str()]).then(a.cmp(b)));
    let mut pool = short_names();
    let mut map: HashMap<String, String> = HashMap::new();
    for name in &declared {
        let new = loop {
            let candidate = pool.next().expect("the pool of names is endless");
            if !used.contains(&candidate) && !RESERVED.contains(&candidate.as_str()) {
                break candidate;
            }
        };
        used.insert(new.clone());
        map.insert(name.clone(), new);
    }
    for index in 0..tokens.len() {
        if tokens[index].kind != Kind::Word {
            continue;
        }
        let Some(new) = map.get(&tokens[index].text) else {
            continue;
        };
        let after_dot = index > 0 && matches!(tokens[index - 1].text.as_str(), "." | "?.");
        let before = index.checked_sub(1).map(|p| tokens[p].text.as_str());
        let object_key = matches!(before, Some("{" | ","))
            && tokens.get(index + 1).map(|t| t.text.as_str()) == Some(":");
        if !after_dot && !object_key {
            tokens[index].text = new.clone();
        }
    }
}

/// `a`, `b`, ... `z`, `A`, ... `Z`, `aa`, `ab`, ...
fn short_names() -> impl Iterator<Item = String> {
    const LETTERS: &[u8] = b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ";
    (1usize..).flat_map(|length| {
        (0..LETTERS.len().pow(length as u32)).map(move |n| {
            let mut n = n;
            let mut name = vec![0u8; length];
            for slot in name.iter_mut().rev() {
                *slot = LETTERS[n % LETTERS.len()];
                n /= LETTERS.len();
            }
            String::from_utf8(name).expect("ASCII letters")
        })
    })
}

fn join(tokens: &[Token]) -> String {
    let mut out = String::new();
    for (index, token) in tokens.iter().enumerate() {
        if let Some(previous) = index.checked_sub(1).map(|p| &tokens[p]) {
            if let Some(separator) = separator(previous, token) {
                out.push_str(separator);
            }
        }
        out.push_str(&token.text);
    }
    out
}

/// What goes between two tokens: nothing, a space, or a line end. A line end stays only where the
/// source can depend on the automatic semicolon.
fn separator(previous: &Token, next: &Token) -> Option<&'static str> {
    let ends_expression = match previous.kind {
        Kind::Word | Kind::Number | Kind::Text => true,
        Kind::Mark => matches!(previous.text.as_str(), ")" | "]" | "}" | "++" | "--"),
    };
    let restricted = previous.kind == Kind::Word
        && matches!(
            previous.text.as_str(),
            "return" | "break" | "continue" | "throw"
        );
    let starts_statement = matches!(next.kind, Kind::Word | Kind::Number | Kind::Text)
        || matches!(next.text.as_str(), "{" | "++" | "--");
    if next.line_before && ((ends_expression && starts_statement) || restricted) {
        return Some("\n");
    }
    let word_like = |t: &Token| matches!(t.kind, Kind::Word | Kind::Number);
    if word_like(previous) && word_like(next) {
        return Some(" ");
    }
    if previous.kind == Kind::Number && next.text.starts_with('.') {
        return Some(" ");
    }
    if previous.kind == Kind::Mark && next.kind == Kind::Mark {
        let last = previous.text.chars().last();
        let first = next.text.chars().next();
        if last == first && matches!(last, Some('+' | '-' | '/')) {
            return Some(" ");
        }
        if previous.text == "/" && next.text.starts_with('*') {
            return Some(" ");
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_stylesheet_loses_its_comments_and_its_layout_and_keeps_its_strings() {
        let source = "/* note */\n.a  >  .b {\n  color : red ; /* x */\n  content: \"a  /* b */ c\";\n  width: calc(100% - 4px);\n}\n";
        assert_eq!(
            stylesheet(source),
            ".a > .b{color : red;content: \"a  /* b */ c\";width: calc(100% - 4px);}"
        );
    }

    #[test]
    fn a_comment_between_two_words_leaves_one_space() {
        assert_eq!(stylesheet("a/* x */b"), "a b");
    }

    #[test]
    fn a_script_loses_its_comments_and_renames_its_local_names() {
        let source = "// head\n(function () {\n  \"use strict\";\n  var counter = 0;\n  /* x */\n  function bump(step) { counter += step; return counter; }\n  window.counter = bump(2);\n})();\n";
        let out = script(source).unwrap();
        assert!(!out.contains("head") && !out.contains("/*"), "{out}");
        assert!(!out.contains("bump") && !out.contains("step"), "{out}");
        // The property of `window` keeps its name. The local name changes.
        assert!(out.contains("window.counter="), "{out}");
        assert!(out.len() < source.len() * 2 / 3, "{out}");
    }

    #[test]
    fn the_line_end_stays_where_the_automatic_semicolon_needs_it() {
        let out = script("var a = 1\nvar b = 2\nfunction f() { return\n a }").unwrap();
        assert!(out.contains('\n'), "{out}");
        assert!(out.contains("return\n"), "{out}");
    }

    #[test]
    fn two_signs_and_a_number_with_a_dot_do_not_merge() {
        assert_eq!(
            script("x = a + +b; y = a - -b; z = 1 .toString()").unwrap(),
            "x=a+ +b;y=a- -b;z=1 .toString()"
        );
    }

    #[test]
    fn the_names_of_properties_and_of_keys_do_not_change() {
        let out = script("var value = {value: 1}; foo.value = value;").unwrap();
        assert!(
            out.contains("{value:1}") && out.contains("foo.value="),
            "{out}"
        );
    }

    #[test]
    fn a_form_that_the_transform_does_not_understand_is_not_renamed_or_it_is_refused() {
        assert!(script("var a = `x`;").is_err());
        assert!(script("var r = x / 2; var q = /a/;").is_err());
        let out = script("var a = 1, b = 2;\nwindow.x = a + b;").unwrap();
        assert!(out.contains("var a=1,b=2;"), "{out}");
        let out = script("var f = (v) => v;").unwrap();
        assert!(out.contains("(v)=>v"), "{out}");
    }

    #[test]
    fn the_short_names_are_endless_and_unique() {
        let names: Vec<String> = short_names().take(60).collect();
        assert_eq!(names[0], "a");
        assert_eq!(names[52], "aa");
        let set: HashSet<&String> = names.iter().collect();
        assert_eq!(set.len(), names.len());
    }
}
