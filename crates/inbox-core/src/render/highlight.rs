//! Syntax color of code. The server (HTML) and the phone (spans) share this one implementation.
//!
//! The tokenizers are written by hand. They have no dependency, they run in linear time, and they never panic. They
//! read the text as bytes and stop only on ASCII bytes, so every offset of a token is on a character boundary. A
//! string or a comment that is not closed runs to the end of its line, or to the end of the text where the language
//! lets it span lines.
//!
//! The result is a list of tokens that covers the whole text, in order, with no gap. A run of text that has no
//! color has the class `plain`. The color never changes the text: a renderer shows the same characters.

use serde::{Deserialize, Serialize};

/// The closed list of the languages of the color. A language that is not in the list has no color.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Language {
    #[default]
    Plain,
    Sql,
    Json,
    Yaml,
    Toml,
    Bash,
    Python,
    Rust,
    Javascript,
    Typescript,
    Diff,
}

impl Language {
    /// Every language, in the order of the documentation.
    pub const ALL: [Language; 11] = [
        Language::Plain,
        Language::Sql,
        Language::Json,
        Language::Yaml,
        Language::Toml,
        Language::Bash,
        Language::Python,
        Language::Rust,
        Language::Javascript,
        Language::Typescript,
        Language::Diff,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Language::Plain => "plain",
            Language::Sql => "sql",
            Language::Json => "json",
            Language::Yaml => "yaml",
            Language::Toml => "toml",
            Language::Bash => "bash",
            Language::Python => "python",
            Language::Rust => "rust",
            Language::Javascript => "javascript",
            Language::Typescript => "typescript",
            Language::Diff => "diff",
        }
    }

    /// The language that a name of the closed list gives. No alias, no other case.
    pub fn parse(name: &str) -> Option<Language> {
        Language::ALL.into_iter().find(|l| l.as_str() == name)
    }

    /// The language that the info string of a fenced code block names. It is lenient: the case does not count, a
    /// common alias works, and a name that is not known gives `plain`.
    pub fn from_info(info: &str) -> Language {
        let word = info
            .trim_start_matches(|c: char| c.is_whitespace() || c == '{' || c == '.')
            .split(|c: char| c.is_whitespace() || c == '}' || c == ',')
            .next()
            .unwrap_or("");
        let lower = word.to_ascii_lowercase();
        match lower.as_str() {
            "sh" | "shell" | "zsh" | "console" | "shellscript" => Language::Bash,
            "js" | "mjs" | "cjs" | "jsx" | "node" => Language::Javascript,
            "ts" | "tsx" => Language::Typescript,
            "py" | "python3" => Language::Python,
            "rs" => Language::Rust,
            "yml" => Language::Yaml,
            "jsonc" | "json5" => Language::Json,
            "patch" | "udiff" => Language::Diff,
            "postgresql" | "postgres" | "mysql" | "clickhouse" | "psql" => Language::Sql,
            other => Language::parse(other).unwrap_or(Language::Plain),
        }
    }
}

/// The class of a token. The classes `add`, `del`, `hunk` and `meta` belong to `diff`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Class {
    Keyword,
    String,
    Number,
    Comment,
    Function,
    Type,
    Operator,
    Punctuation,
    Plain,
    Add,
    Del,
    Hunk,
    Meta,
}

impl Class {
    pub fn as_str(self) -> &'static str {
        match self {
            Class::Keyword => "keyword",
            Class::String => "string",
            Class::Number => "number",
            Class::Comment => "comment",
            Class::Function => "function",
            Class::Type => "type",
            Class::Operator => "operator",
            Class::Punctuation => "punctuation",
            Class::Plain => "plain",
            Class::Add => "add",
            Class::Del => "del",
            Class::Hunk => "hunk",
            Class::Meta => "meta",
        }
    }
}

/// A run of text: bytes `start..end` of the text, on character boundaries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Token {
    pub start: usize,
    pub end: usize,
    pub class: Class,
}

/// A text longer than this has no color: one `plain` token. The limits of the protocol are lower.
pub const MAX_HIGHLIGHT_BYTES: usize = 64 * 1024;

/// The tokens of a text. They cover the text from the first byte to the last, in order, with no gap and no overlap.
/// An empty text gives no token.
pub fn tokens(lang: Language, text: &str) -> Vec<Token> {
    if text.is_empty() {
        return Vec::new();
    }
    if lang == Language::Plain || text.len() > MAX_HIGHLIGHT_BYTES {
        return vec![Token { start: 0, end: text.len(), class: Class::Plain }];
    }
    let mut lx = Lexer { s: text.as_bytes(), out: Vec::new() };
    match lang {
        Language::Diff => lx.diff(),
        Language::Plain => unreachable!("handled above"),
        other => lx.code(&spec(other), other),
    }
    lx.out
}

/// The tokens with their offsets in UTF-16 code units, which is how Java and Kotlin index a string.
/// The JNI bridge sends these.
pub fn tokens_utf16(lang: Language, text: &str) -> Vec<(usize, usize, Class)> {
    let mut at = 0usize;
    tokens(lang, text)
        .into_iter()
        .map(|t| {
            let n: usize = text[t.start..t.end].chars().map(char::len_utf16).sum();
            let out = (at, at + n, t.class);
            at += n;
            out
        })
        .collect()
}

// ---------------------------------------------------------------- the description of a language

struct Spec {
    line_comments: &'static [&'static str],
    /// A line comment of this kind starts only at the start of a word (the `#` of a shell or of YAML).
    comment_after_space: bool,
    block_comment: bool,
    nested_blocks: bool,
    /// The bytes that open a string.
    quotes: &'static [u8],
    triple_quotes: bool,
    /// A string of `'` can span lines, and a doubled quote is a quote (SQL).
    sql_strings: bool,
    /// A string of a backtick can span lines.
    multiline_backtick: bool,
    keywords: &'static str,
    types: &'static str,
    ignore_case: bool,
    capitalized_types: bool,
    /// The word after one of these keywords is a function or a type.
    def_function: &'static str,
    def_type: &'static str,
    /// `name(` is a call. For a shell only `name()` (a definition) is.
    calls: bool,
}

/// A list of words: separated by spaces.
fn words(list: &'static str) -> impl Iterator<Item = &'static str> {
    list.split_ascii_whitespace()
}

const NO_WORDS: &str = "";

fn spec(lang: Language) -> Spec {
    let base = Spec {
        line_comments: &[],
        comment_after_space: false,
        block_comment: false,
        nested_blocks: false,
        quotes: b"\"'",
        triple_quotes: false,
        sql_strings: false,
        multiline_backtick: false,
        keywords: NO_WORDS,
        types: NO_WORDS,
        ignore_case: false,
        capitalized_types: false,
        def_function: NO_WORDS,
        def_type: NO_WORDS,
        calls: true,
    };
    match lang {
        Language::Sql => Spec {
            line_comments: &["--"],
            block_comment: true,
            quotes: b"'\"`",
            sql_strings: true,
            keywords: SQL_KEYWORDS,
            types: SQL_TYPES,
            ignore_case: true,
            ..base
        },
        Language::Json => Spec {
            line_comments: &["//"],
            block_comment: true,
            quotes: b"\"",
            keywords: "true false null",
            calls: false,
            ..base
        },
        Language::Yaml => Spec {
            line_comments: &["#"],
            comment_after_space: true,
            keywords: "true false null yes no on off",
            calls: false,
            ..base
        },
        Language::Toml => Spec {
            line_comments: &["#"],
            triple_quotes: true,
            keywords: "true false inf nan",
            calls: false,
            ..base
        },
        Language::Bash => Spec {
            line_comments: &["#"],
            comment_after_space: true,
            keywords: BASH_KEYWORDS,
            calls: false,
            ..base
        },
        Language::Python => Spec {
            line_comments: &["#"],
            triple_quotes: true,
            keywords: PYTHON_KEYWORDS,
            types: PYTHON_TYPES,
            capitalized_types: true,
            def_function: "def",
            def_type: "class",
            ..base
        },
        Language::Rust => Spec {
            line_comments: &["//"],
            block_comment: true,
            nested_blocks: true,
            quotes: b"\"",
            keywords: RUST_KEYWORDS,
            types: RUST_TYPES,
            capitalized_types: true,
            def_function: "fn",
            def_type: "struct enum trait type union mod",
            ..base
        },
        Language::Javascript => Spec {
            line_comments: &["//"],
            block_comment: true,
            quotes: b"\"'`",
            multiline_backtick: true,
            keywords: JS_KEYWORDS,
            types: NO_WORDS,
            capitalized_types: true,
            def_function: "function",
            def_type: "class",
            ..base
        },
        Language::Typescript => Spec {
            line_comments: &["//"],
            block_comment: true,
            quotes: b"\"'`",
            multiline_backtick: true,
            keywords: TS_KEYWORDS,
            types: TS_TYPES,
            capitalized_types: true,
            def_function: "function",
            def_type: "class interface type enum namespace",
            ..base
        },
        Language::Plain | Language::Diff => base,
    }
}

const SQL_KEYWORDS: &str = "\
    add all alter and any as asc begin between by case check column commit constraint create \
    cross database default delete desc distinct drop else end engine exists false from full \
    grant group having if ilike in index inner insert into is join key left like limit \
    materialized not null offset on or order outer over partition primary references replace \
    returning revoke right rollback schema select set table then true truncate union unique \
    update using values view when where window with foreign recursive final prewhere format \
    settings optimize attach detach show describe explain use";

const SQL_TYPES: &str = "\
    int integer bigint smallint tinyint serial bigserial varchar char text boolean bool date \
    time timestamp timestamptz datetime decimal numeric float double real json jsonb uuid bytea \
    blob array string uint8 uint16 uint32 uint64 uint128 uint256 int8 int16 int32 int64 int128 \
    int256 float32 float64 datetime64 nullable lowcardinality fixedstring map tuple enum ipv4 \
    ipv6";

const BASH_KEYWORDS: &str = "\
    if then else elif fi for while until do done case esac in function select time return exit \
    break continue local export readonly declare unset set shift source alias eval exec trap \
    true false";

const PYTHON_KEYWORDS: &str = "\
    and as assert async await break class continue def del elif else except finally for from \
    global if import in is lambda nonlocal not or pass raise return try while with yield match \
    case True False None";

const PYTHON_TYPES: &str = "int str float bool list dict set tuple bytes object frozenset complex";

const RUST_KEYWORDS: &str = "\
    as async await break const continue crate dyn else enum extern false fn for if impl in let \
    loop match mod move mut pub ref return self Self static struct super trait true type unsafe \
    use where while union";

const RUST_TYPES: &str =
    "bool char str u8 u16 u32 u64 u128 usize i8 i16 i32 i64 i128 isize f32 f64";

const JS_KEYWORDS: &str = "\
    async await break case catch class const continue debugger default delete do else export \
    extends false finally for function if import in instanceof let new null of return static \
    super switch this throw true try typeof undefined var void while with yield";

const TS_KEYWORDS: &str = "\
    abstract as async await break case catch class const continue declare default delete do else \
    enum export extends false finally for function if implements import in infer instanceof \
    interface is keyof let namespace new null of private protected public readonly return \
    satisfies static super switch this throw true try type typeof undefined var void while with \
    yield";

const TS_TYPES: &str = "string number boolean object symbol bigint unknown never any";

// ---------------------------------------------------------------- the scanner

struct Lexer<'a> {
    s: &'a [u8],
    out: Vec<Token>,
}

fn is_ident_start(b: u8) -> bool {
    b.is_ascii_alphabetic() || b == b'_' || b >= 0x80
}

fn is_ident_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b >= 0x80
}

fn is_space(b: u8) -> bool {
    matches!(b, b' ' | b'\t' | b'\r' | b'\n' | 0x0b | 0x0c)
}

impl Lexer<'_> {
    fn push(&mut self, start: usize, end: usize, class: Class) {
        if end <= start {
            return;
        }
        if let Some(last) = self.out.last_mut()
            && last.class == class
            && last.end == start
        {
            last.end = end;
            return;
        }
        self.out.push(Token { start, end, class });
    }

    fn at(&self, i: usize) -> u8 {
        self.s.get(i).copied().unwrap_or(0)
    }

    fn starts_with(&self, i: usize, pat: &str) -> bool {
        self.s.get(i..).is_some_and(|rest| rest.starts_with(pat.as_bytes()))
    }

    /// The length in bytes of the character at `i`. `i` is on a character boundary.
    fn char_len(&self, i: usize) -> usize {
        let mut j = i + 1;
        if self.at(i) >= 0xC0 {
            while j < self.s.len() && self.s[j] & 0xC0 == 0x80 {
                j += 1;
            }
        }
        j - i
    }

    fn line_end(&self, mut i: usize) -> usize {
        while i < self.s.len() && self.s[i] != b'\n' {
            i += 1;
        }
        i
    }

    // ------------------------------------------------------------ diff

    fn diff(&mut self) {
        let n = self.s.len();
        let mut pos = 0;
        let mut prev_old_header = false;
        while pos < n {
            let end = self.line_end(pos);
            let line = &self.s[pos..end];
            let next_is_new = {
                let nl = (end + 1).min(n);
                self.s[nl..].starts_with(b"+++ ")
            };
            let class = if (line.starts_with(b"+++ ") && prev_old_header)
                || (line.starts_with(b"--- ") && next_is_new)
            {
                Class::Meta
            } else if line.starts_with(b"@@") {
                Class::Hunk
            } else if line.starts_with(b"+") {
                Class::Add
            } else if line.starts_with(b"-") {
                Class::Del
            } else if [
                &b"diff "[..],
                b"index ",
                b"new file",
                b"deleted file",
                b"old mode",
                b"new mode",
                b"similarity ",
                b"rename ",
                b"copy ",
                b"Binary files",
                b"\\ ",
            ]
            .iter()
            .any(|p| line.starts_with(p))
            {
                Class::Meta
            } else {
                Class::Plain
            };
            prev_old_header = line.starts_with(b"--- ") && class == Class::Meta;
            self.push(pos, end, class);
            if end < n {
                self.push(end, end + 1, Class::Plain);
            }
            pos = end + 1;
        }
    }

    // ------------------------------------------------------------ the other languages

    fn code(&mut self, sp: &Spec, lang: Language) {
        let n = self.s.len();
        let mut i = 0;
        // True until the first token of a line that is not a space or a list dash.
        let mut fresh = true;
        // The keyword that came last, for `fn name` and `class Name`.
        let mut last_kw: Option<&'static str> = None;
        while i < n {
            let b = self.s[i];
            if b == b'\n' {
                self.push(i, i + 1, Class::Plain);
                i += 1;
                fresh = true;
                continue;
            }
            if is_space(b) {
                let start = i;
                while i < n && is_space(self.s[i]) && self.s[i] != b'\n' {
                    i += 1;
                }
                self.push(start, i, Class::Plain);
                continue;
            }
            // Structured keys.
            if fresh {
                if lang == Language::Yaml
                    && b != b'#'
                    && !(b == b'-' && is_space(self.at(i + 1)))
                    && let Some(end) = self.yaml_key(i)
                {
                    self.push(i, end, Class::Type);
                    i = end;
                    fresh = false;
                    continue;
                }
                if lang == Language::Toml {
                    if b == b'[' {
                        let end = self.toml_header(i);
                        self.push(i, end, Class::Type);
                        i = end;
                        fresh = false;
                        continue;
                    }
                    if let Some(end) = self.toml_key(i) {
                        self.push(i, end, Class::Type);
                        i = end;
                        fresh = false;
                        continue;
                    }
                }
            }
            if !(lang == Language::Yaml && b == b'-' && is_space(self.at(i + 1))) {
                fresh = false;
            }
            // Comments.
            let after_space = i == 0
                || is_space(self.s[i - 1])
                || matches!(self.s[i - 1], b';' | b'|' | b'&' | b'(');
            if sp
                .line_comments
                .iter()
                .any(|c| self.starts_with(i, c) && (!sp.comment_after_space || after_space))
            {
                let end = self.line_end(i);
                self.push(i, end, Class::Comment);
                i = end;
                continue;
            }
            if sp.block_comment && self.starts_with(i, "/*") {
                let end = self.block_comment_end(i, sp.nested_blocks);
                self.push(i, end, Class::Comment);
                i = end;
                continue;
            }
            // Rust: raw strings, lifetimes and characters.
            if lang == Language::Rust {
                if let Some(end) = self.rust_raw_string(i) {
                    self.push(i, end, Class::String);
                    i = end;
                    last_kw = None;
                    continue;
                }
                if b == b'\'' {
                    let (end, class) = self.rust_quote(i);
                    self.push(i, end, class);
                    i = end;
                    last_kw = None;
                    continue;
                }
            }
            // Strings.
            if sp.quotes.contains(&b) {
                let end = self.string_end(i, sp);
                let mut class = Class::String;
                if lang == Language::Sql && b != b'\'' {
                    // A quoted name in SQL is an identifier, not a text.
                    class = Class::Plain;
                }
                if lang == Language::Json && self.followed_by_colon(end) {
                    class = Class::Type;
                }
                self.push(i, end, class);
                i = end;
                last_kw = None;
                continue;
            }
            // Numbers.
            if b.is_ascii_digit() {
                let end = self.number_end(i);
                self.push(i, end, Class::Number);
                i = end;
                last_kw = None;
                continue;
            }
            // Words.
            if is_ident_start(b) {
                let start = i;
                i += 1;
                while i < n && is_ident_byte(self.s[i]) {
                    i += 1;
                }
                // A prefixed string of Python: r"..", b'..', f"..".
                if lang == Language::Python
                    && i - start <= 2
                    && matches!(self.at(i), b'"' | b'\'')
                    && self.s[start..i]
                        .iter()
                        .all(|c| matches!(c.to_ascii_lowercase(), b'r' | b'b' | b'f' | b'u'))
                {
                    let end = self.string_end(i, sp);
                    self.push(start, end, Class::String);
                    i = end;
                    last_kw = None;
                    continue;
                }
                // Keys of a TOML or YAML value are plain words with a dash: `foo-bar`.
                if matches!(lang, Language::Yaml | Language::Toml | Language::Bash) {
                    while i < n && (is_ident_byte(self.s[i]) || self.s[i] == b'-') {
                        i += 1;
                    }
                }
                let word = &self.s[start..i];
                let (class, kw) = self.classify_word(word, i, sp, lang, last_kw);
                self.push(start, i, class);
                last_kw = kw;
                continue;
            }
            // Variables and options of a shell.
            if lang == Language::Bash {
                if b == b'$' {
                    let end = self.shell_variable(i);
                    self.push(i, end, Class::Type);
                    i = end;
                    continue;
                }
                if b == b'-'
                    && (is_ident_start(self.at(i + 1)) || self.at(i + 1) == b'-')
                    && after_space
                {
                    let mut end = i + 1;
                    while end < n && (is_ident_byte(self.s[end]) || self.s[end] == b'-') {
                        end += 1;
                    }
                    self.push(i, end, Class::Operator);
                    i = end;
                    continue;
                }
            }
            // Decorators.
            if b == b'@'
                && matches!(lang, Language::Python | Language::Typescript)
                && is_ident_start(self.at(i + 1))
            {
                let mut end = i + 1;
                while end < n && (is_ident_byte(self.s[end]) || self.s[end] == b'.') {
                    end += 1;
                }
                self.push(i, end, Class::Function);
                i = end;
                continue;
            }
            // YAML anchors, aliases and tags.
            if lang == Language::Yaml
                && matches!(b, b'&' | b'*' | b'!')
                && !is_space(self.at(i + 1))
            {
                let mut end = i + 1;
                while end < n
                    && !is_space(self.s[end])
                    && !matches!(self.s[end], b',' | b']' | b'}')
                {
                    end += 1;
                }
                self.push(i, end, if b == b'!' { Class::Type } else { Class::Function });
                i = end;
                continue;
            }
            let len = self.char_len(i);
            let class = match b {
                b'+' | b'-' | b'*' | b'/' | b'%' | b'=' | b'<' | b'>' | b'!' | b'&' | b'|'
                | b'^' | b'~' | b'?' => Class::Operator,
                b'(' | b')' | b'{' | b'}' | b'[' | b']' | b',' | b';' | b':' | b'.' | b'@'
                | b'#' | b'\\' => Class::Punctuation,
                _ => Class::Plain,
            };
            self.push(i, i + len, class);
            i += len;
            last_kw = None;
        }
    }

    fn classify_word(
        &self,
        word: &[u8],
        end: usize,
        sp: &Spec,
        lang: Language,
        last_kw: Option<&'static str>,
    ) -> (Class, Option<&'static str>) {
        let eq = |w: &str| {
            if sp.ignore_case {
                w.as_bytes().eq_ignore_ascii_case(word)
            } else {
                w.as_bytes() == word
            }
        };
        if let Some(kw) = words(sp.keywords).find(|w| eq(w)) {
            return (Class::Keyword, Some(kw));
        }
        if let Some(prev) = last_kw {
            if words(sp.def_function).any(|w| w == prev) {
                return (Class::Function, None);
            }
            if words(sp.def_type).any(|w| w == prev) {
                return (Class::Type, None);
            }
        }
        if words(sp.types).any(eq) {
            return (Class::Type, None);
        }
        if sp.capitalized_types
            && word[0].is_ascii_uppercase()
            && word.iter().any(u8::is_ascii_lowercase)
        {
            return (Class::Type, None);
        }
        let next = self.at(end);
        let call = match lang {
            Language::Bash => next == b'(' && self.at(end + 1) == b')',
            Language::Rust => {
                next == b'(' || (next == b'!' && matches!(self.at(end + 1), b'(' | b'[' | b'{'))
            }
            _ => sp.calls && next == b'(',
        };
        if call {
            return (Class::Function, None);
        }
        (Class::Plain, None)
    }

    fn block_comment_end(&self, start: usize, nested: bool) -> usize {
        let n = self.s.len();
        let mut depth = 1usize;
        let mut i = start + 2;
        while i < n {
            if self.starts_with(i, "*/") {
                depth -= 1;
                i += 2;
                if depth == 0 {
                    return i;
                }
            } else if nested && self.starts_with(i, "/*") {
                depth += 1;
                i += 2;
            } else {
                i += 1;
            }
        }
        n
    }

    /// The end of the string that opens at `start`. A string that is not closed ends at the end of the line, or at
    /// the end of the text where the language lets it span lines.
    fn string_end(&self, start: usize, sp: &Spec) -> usize {
        let n = self.s.len();
        let q = self.s[start];
        if sp.triple_quotes && self.at(start + 1) == q && self.at(start + 2) == q {
            let mut i = start + 3;
            while i < n {
                if self.s[i] == b'\\' && q == b'"' {
                    i += 2;
                } else if self.s[i] == q && self.at(i + 1) == q && self.at(i + 2) == q {
                    return i + 3;
                } else {
                    i += 1;
                }
            }
            return n;
        }
        let multi = (q == b'\'' && sp.sql_strings) || (q == b'`' && sp.multiline_backtick);
        let mut i = start + 1;
        while i < n {
            let b = self.s[i];
            if b == b'\\' && !sp.sql_strings {
                i += 2;
                continue;
            }
            if b == q {
                if sp.sql_strings && self.at(i + 1) == q {
                    i += 2;
                    continue;
                }
                return i + 1;
            }
            if b == b'\n' && !multi {
                return i;
            }
            i += 1;
        }
        n
    }

    fn number_end(&self, start: usize) -> usize {
        let n = self.s.len();
        let mut i = start + 1;
        while i < n {
            let b = self.s[i];
            if b.is_ascii_alphanumeric() || b == b'_' {
                // The sign of an exponent: `1e-5`.
                if matches!(b, b'e' | b'E')
                    && matches!(self.at(i + 1), b'+' | b'-')
                    && self.at(i + 2).is_ascii_digit()
                    && !self.s[start..i].starts_with(b"0x")
                {
                    i += 2;
                }
                i += 1;
            } else if b == b'.' && self.at(i + 1).is_ascii_digit() {
                i += 1;
            } else {
                break;
            }
        }
        i
    }

    fn shell_variable(&self, start: usize) -> usize {
        let n = self.s.len();
        let mut i = start + 1;
        match self.at(i) {
            b'{' => {
                while i < n && self.s[i] != b'}' && self.s[i] != b'\n' {
                    i += 1;
                }
                if self.at(i) == b'}' {
                    i += 1;
                }
            }
            b'(' => return start + 1,
            b'@' | b'*' | b'#' | b'?' | b'$' | b'!' | b'-' => i += 1,
            _ => {
                while i < n && (self.s[i].is_ascii_alphanumeric() || self.s[i] == b'_') {
                    i += 1;
                }
            }
        }
        i
    }

    fn rust_raw_string(&self, start: usize) -> Option<usize> {
        let n = self.s.len();
        let mut i = start;
        if self.at(i) == b'b' {
            i += 1;
        }
        if self.at(i) != b'r' {
            return None;
        }
        // The `r` must start a word.
        if start > 0 && is_ident_byte(self.s[start - 1]) {
            return None;
        }
        i += 1;
        let mut hashes = 0;
        while self.at(i) == b'#' {
            hashes += 1;
            i += 1;
        }
        if self.at(i) != b'"' {
            return None;
        }
        i += 1;
        while i < n {
            if self.s[i] == b'"' && (0..hashes).all(|k| self.at(i + 1 + k) == b'#') {
                return Some(i + 1 + hashes);
            }
            i += 1;
        }
        Some(n)
    }

    /// A `'` in Rust opens a character or a lifetime.
    fn rust_quote(&self, start: usize) -> (usize, Class) {
        let n = self.s.len();
        let next = self.at(start + 1);
        if next == b'\\' {
            let mut i = start + 2;
            while i < n && i < start + 12 && self.s[i] != b'\'' && self.s[i] != b'\n' {
                i += 1;
            }
            if self.at(i) == b'\'' {
                return (i + 1, Class::String);
            }
            return (start + 1, Class::Punctuation);
        }
        if next == b'\n' || next == 0 {
            return (start + 1, Class::Punctuation);
        }
        let after = start + 1 + self.char_len(start + 1);
        if self.at(after) == b'\'' {
            return (after + 1, Class::String);
        }
        if is_ident_start(next) {
            let mut i = start + 1;
            while i < n && is_ident_byte(self.s[i]) {
                i += 1;
            }
            return (i, Class::Type);
        }
        (start + 1, Class::Punctuation)
    }

    fn followed_by_colon(&self, mut i: usize) -> bool {
        while i < self.s.len() && is_space(self.s[i]) {
            i += 1;
        }
        self.at(i) == b':'
    }

    /// The end of a key of YAML that starts at `start`, or `None`. A key ends with `:` and a space or the end of the line.
    fn yaml_key(&self, start: usize) -> Option<usize> {
        let n = self.s.len();
        let mut i = start;
        let first = self.s[i];
        if matches!(first, b'"' | b'\'') {
            i += 1;
            while i < n && self.s[i] != first && self.s[i] != b'\n' {
                if self.s[i] == b'\\' && first == b'"' {
                    i += 1;
                }
                i += 1;
            }
            if self.at(i) != first {
                return None;
            }
            i += 1;
            let key_end = i;
            while matches!(self.at(i), b' ' | b'\t') {
                i += 1;
            }
            return (self.at(i) == b':' && (i + 1 >= n || is_space(self.at(i + 1))))
                .then_some(key_end);
        }
        if matches!(first, b'{' | b'[' | b'|' | b'>' | b'%' | b'@' | b'`' | b',' | b']' | b'}') {
            return None;
        }
        while i < n && self.s[i] != b'\n' {
            let b = self.s[i];
            if b == b'#' && i > start && is_space(self.s[i - 1]) {
                return None;
            }
            if b == b':' && (i + 1 >= n || is_space(self.s[i + 1])) {
                let mut end = i;
                while end > start && matches!(self.s[end - 1], b' ' | b'\t') {
                    end -= 1;
                }
                return (end > start).then_some(end);
            }
            i += 1;
        }
        None
    }

    fn toml_header(&self, start: usize) -> usize {
        let n = self.s.len();
        let mut i = start;
        while i < n && self.s[i] == b'[' {
            i += 1;
        }
        while i < n && self.s[i] != b']' && self.s[i] != b'\n' {
            i += 1;
        }
        while i < n && self.s[i] == b']' {
            i += 1;
        }
        i
    }

    /// The end of a key of TOML that starts at `start`: bare or quoted names with dots, then `=`.
    fn toml_key(&self, start: usize) -> Option<usize> {
        let n = self.s.len();
        let mut i = start;
        while i < n {
            let b = self.s[i];
            if b == b'"' || b == b'\'' {
                i += 1;
                while i < n && self.s[i] != b && self.s[i] != b'\n' {
                    i += 1;
                }
                if self.at(i) != b {
                    return None;
                }
                i += 1;
            } else if is_ident_byte(b) || b == b'-' || b == b'.' {
                i += 1;
            } else {
                break;
            }
        }
        if i == start {
            return None;
        }
        let key_end = i;
        while matches!(self.at(i), b' ' | b'\t') {
            i += 1;
        }
        (self.at(i) == b'=').then_some(key_end)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(lang: Language, text: &str) -> Vec<(String, Class)> {
        tokens(lang, text)
            .into_iter()
            .map(|t| (text[t.start..t.end].to_string(), t.class))
            .collect()
    }

    fn has(lang: Language, text: &str, piece: &str, class: Class) -> bool {
        kinds(lang, text).iter().any(|(t, c)| t == piece && *c == class)
    }

    fn check_cover(lang: Language, text: &str) {
        let toks = tokens(lang, text);
        let mut at = 0;
        for t in &toks {
            assert_eq!(t.start, at, "{lang:?} gap in {text:?}");
            assert!(t.end > t.start);
            assert!(
                text.is_char_boundary(t.start) && text.is_char_boundary(t.end),
                "{lang:?} {text:?}"
            );
            at = t.end;
        }
        assert_eq!(at, text.len(), "{lang:?} {text:?}");
    }

    #[test]
    fn the_language_list_is_closed() {
        assert_eq!(Language::ALL.len(), 11);
        for l in Language::ALL {
            assert_eq!(Language::parse(l.as_str()), Some(l));
        }
        for bad in ["", "SQL", "Rust", "c", "html", "shell", "sh", "py", " sql"] {
            assert_eq!(Language::parse(bad), None, "{bad:?}");
        }
        assert_eq!(serde_json::to_string(&Language::Typescript).unwrap(), "\"typescript\"");
        assert!(serde_json::from_str::<Language>("\"cobol\"").is_err());
    }

    #[test]
    fn an_info_string_is_lenient() {
        assert_eq!(Language::from_info("rust"), Language::Rust);
        assert_eq!(Language::from_info("RS"), Language::Rust);
        assert_eq!(Language::from_info("sh -x"), Language::Bash);
        assert_eq!(Language::from_info("ts"), Language::Typescript);
        assert_eq!(Language::from_info("yml"), Language::Yaml);
        assert_eq!(Language::from_info("{.python}"), Language::Python);
        assert_eq!(Language::from_info(""), Language::Plain);
        assert_eq!(Language::from_info("cobol"), Language::Plain);
    }

    #[test]
    fn sql_snippet() {
        let sql = "-- purge\nSELECT count(*), name FROM staging.events WHERE ts < '2026-11-01' AND n >= 10 /* c */;";
        assert!(has(Language::Sql, sql, "-- purge", Class::Comment));
        assert!(has(Language::Sql, sql, "SELECT", Class::Keyword));
        assert!(has(Language::Sql, sql, "count", Class::Function));
        assert!(has(Language::Sql, sql, "'2026-11-01'", Class::String));
        assert!(has(Language::Sql, sql, "10", Class::Number));
        assert!(has(Language::Sql, sql, "/* c */", Class::Comment));
        assert!(has(Language::Sql, "CREATE TABLE t (id UInt64, s String)", "UInt64", Class::Type));
        // A doubled quote stays inside the string, and a string can span lines.
        assert!(has(Language::Sql, "SELECT 'it''s\nok'", "'it''s\nok'", Class::String));
        check_cover(Language::Sql, sql);
    }

    #[test]
    fn json_snippet() {
        let json = "{\n  \"name\": \"portal\",\n  \"n\": -12.5e+3,\n  \"ok\": true,\n  \"x\": null, \"a\": [1, 2]\n}";
        assert!(has(Language::Json, json, "\"name\"", Class::Type));
        assert!(has(Language::Json, json, "\"portal\"", Class::String));
        assert!(has(Language::Json, json, "12.5e+3", Class::Number));
        assert!(has(Language::Json, json, "true", Class::Keyword));
        assert!(has(Language::Json, json, "null", Class::Keyword));
        assert!(has(Language::Json, json, "{", Class::Punctuation));
        check_cover(Language::Json, json);
    }

    #[test]
    fn yaml_snippet() {
        let yaml = "# config\nname: portal\nitems:\n  - id: 1\n    ok: true\n  - \"q\": 'x'  # tail\nurl: http://a:80/b\n";
        assert!(has(Language::Yaml, yaml, "# config", Class::Comment));
        assert!(has(Language::Yaml, yaml, "name", Class::Type));
        assert!(has(Language::Yaml, yaml, "items", Class::Type));
        assert!(has(Language::Yaml, yaml, "id", Class::Type));
        assert!(has(Language::Yaml, yaml, "1", Class::Number));
        assert!(has(Language::Yaml, yaml, "true", Class::Keyword));
        assert!(has(Language::Yaml, yaml, "\"q\"", Class::Type));
        assert!(has(Language::Yaml, yaml, "'x'", Class::String));
        assert!(has(Language::Yaml, yaml, "# tail", Class::Comment));
        // The `:` in a URL does not make a key.
        assert!(has(Language::Yaml, yaml, "url", Class::Type));
        assert!(!has(Language::Yaml, yaml, "http", Class::Type));
        check_cover(Language::Yaml, yaml);
    }

    #[test]
    fn toml_snippet() {
        let toml = "[inbox]\nenabled = true\nname = \"x\" # c\nlist = [1, 2.5]\n[[a.b]]\nk.j = '''raw\nmore'''\n";
        assert!(has(Language::Toml, toml, "[inbox]", Class::Type));
        assert!(has(Language::Toml, toml, "enabled", Class::Type));
        assert!(has(Language::Toml, toml, "true", Class::Keyword));
        assert!(has(Language::Toml, toml, "\"x\"", Class::String));
        assert!(has(Language::Toml, toml, "# c", Class::Comment));
        assert!(has(Language::Toml, toml, "2.5", Class::Number));
        assert!(has(Language::Toml, toml, "[[a.b]]", Class::Type));
        assert!(has(Language::Toml, toml, "k.j", Class::Type));
        assert!(has(Language::Toml, toml, "'''raw\nmore'''", Class::String));
        check_cover(Language::Toml, toml);
    }

    #[test]
    fn bash_snippet() {
        let sh = "#!/bin/sh\nset -eu\nif [ -f \"$HOME/x\" ]; then echo 'a b' --flag ${VAR:-d} # done\nfi\nf() { :; }";
        assert!(has(Language::Bash, sh, "#!/bin/sh", Class::Comment));
        assert!(has(Language::Bash, sh, "set", Class::Keyword));
        assert!(has(Language::Bash, sh, "-eu", Class::Operator));
        assert!(has(Language::Bash, sh, "if", Class::Keyword));
        assert!(has(Language::Bash, sh, "\"$HOME/x\"", Class::String));
        assert!(has(Language::Bash, sh, "'a b'", Class::String));
        assert!(has(Language::Bash, sh, "--flag", Class::Operator));
        assert!(has(Language::Bash, sh, "${VAR:-d}", Class::Type));
        assert!(has(Language::Bash, sh, "# done", Class::Comment));
        assert!(has(Language::Bash, sh, "f", Class::Function));
        // A `#` inside a word is not a comment.
        assert!(!has(Language::Bash, "echo a#b", "#b", Class::Comment));
        check_cover(Language::Bash, sh);
    }

    #[test]
    fn python_snippet() {
        let py = "@app.route('/')\ndef handler(req: int) -> Response:\n    \"\"\"doc\n    more\"\"\"\n    x = f\"a{req}\" + 0x1F  # c\n    return None\n";
        assert!(has(Language::Python, py, "@app.route", Class::Function));
        assert!(has(Language::Python, py, "def", Class::Keyword));
        assert!(has(Language::Python, py, "handler", Class::Function));
        assert!(has(Language::Python, py, "int", Class::Type));
        assert!(has(Language::Python, py, "Response", Class::Type));
        assert!(has(Language::Python, py, "\"\"\"doc\n    more\"\"\"", Class::String));
        assert!(has(Language::Python, py, "f\"a{req}\"", Class::String));
        assert!(has(Language::Python, py, "0x1F", Class::Number));
        assert!(has(Language::Python, py, "# c", Class::Comment));
        assert!(has(Language::Python, py, "None", Class::Keyword));
        check_cover(Language::Python, py);
    }

    #[test]
    fn rust_snippet() {
        let rs = "/// doc\n#[derive(Debug)]\npub struct Item<'a> { name: &'a str }\nfn main() { let x: u32 = 1_000; println!(\"hi {x}\"); let c = 'z'; let r = r#\"a\"b\"#; /* a /* b */ c */ }";
        assert!(has(Language::Rust, rs, "/// doc", Class::Comment));
        assert!(has(Language::Rust, rs, "pub", Class::Keyword));
        assert!(has(Language::Rust, rs, "Item", Class::Type));
        assert!(has(Language::Rust, rs, "'a", Class::Type));
        assert!(has(Language::Rust, rs, "main", Class::Function));
        assert!(has(Language::Rust, rs, "u32", Class::Type));
        assert!(has(Language::Rust, rs, "1_000", Class::Number));
        assert!(has(Language::Rust, rs, "println", Class::Function));
        assert!(has(Language::Rust, rs, "\"hi {x}\"", Class::String));
        assert!(has(Language::Rust, rs, "'z'", Class::String));
        assert!(has(Language::Rust, rs, "r#\"a\"b\"#", Class::String));
        assert!(has(Language::Rust, rs, "/* a /* b */ c */", Class::Comment));
        check_cover(Language::Rust, rs);
    }

    #[test]
    fn javascript_and_typescript_snippets() {
        let js = "// c\nasync function load(url) { const r = await fetch(`${url}/a`); return r.ok ? 1.5 : null; }\nclass Box extends Base {}";
        assert!(has(Language::Javascript, js, "// c", Class::Comment));
        assert!(has(Language::Javascript, js, "async", Class::Keyword));
        assert!(has(Language::Javascript, js, "load", Class::Function));
        assert!(has(Language::Javascript, js, "fetch", Class::Function));
        assert!(has(Language::Javascript, js, "`${url}/a`", Class::String));
        assert!(has(Language::Javascript, js, "1.5", Class::Number));
        assert!(has(Language::Javascript, js, "Box", Class::Type));
        let ts = "interface Item { id: string; n?: number }\nexport type Id = Item['id'];\n@Component({})\nclass A implements B {}";
        assert!(has(Language::Typescript, ts, "interface", Class::Keyword));
        assert!(has(Language::Typescript, ts, "Item", Class::Type));
        assert!(has(Language::Typescript, ts, "string", Class::Type));
        assert!(has(Language::Typescript, ts, "number", Class::Type));
        assert!(has(Language::Typescript, ts, "'id'", Class::String));
        assert!(has(Language::Typescript, ts, "@Component", Class::Function));
        check_cover(Language::Javascript, js);
        check_cover(Language::Typescript, ts);
    }

    #[test]
    fn diff_snippet() {
        let d = "diff --git a/x b/x\nindex 1..2 100644\n--- a/x\n+++ b/x\n@@ -1,2 +1,2 @@ fn f()\n context\n-old\n+new\n--- not a header\n\\ No newline at end of file\n";
        let k = kinds(Language::Diff, d);
        let class_of = |piece: &str| k.iter().find(|(t, _)| t == piece).map(|x| x.1);
        assert_eq!(class_of("diff --git a/x b/x"), Some(Class::Meta));
        assert_eq!(class_of("index 1..2 100644"), Some(Class::Meta));
        assert_eq!(class_of("--- a/x"), Some(Class::Meta));
        assert_eq!(class_of("+++ b/x"), Some(Class::Meta));
        assert_eq!(class_of("@@ -1,2 +1,2 @@ fn f()"), Some(Class::Hunk));
        assert_eq!(class_of("-old"), Some(Class::Del));
        assert_eq!(class_of("+new"), Some(Class::Add));
        // A removed line that starts with two dashes is a removed line.
        assert_eq!(class_of("--- not a header"), Some(Class::Del));
        assert_eq!(class_of("\\ No newline at end of file"), Some(Class::Meta));
        check_cover(Language::Diff, d);
    }

    #[test]
    fn unterminated_constructs_run_to_the_end() {
        for (lang, text, class) in [
            (Language::Sql, "select 'abc", Class::String),
            (Language::Sql, "select /* open", Class::Comment),
            (Language::Json, "{\"a\": \"open", Class::String),
            (Language::Python, "x = \"\"\"never closed\nmore", Class::String),
            (Language::Rust, "let s = \"open\\", Class::String),
            (Language::Rust, "/* /* nested open */", Class::Comment),
            (Language::Rust, "r##\"raw open", Class::String),
            (Language::Javascript, "`template open\nstill", Class::String),
            (Language::Toml, "k = '''open", Class::String),
            (Language::Bash, "echo \"open", Class::String),
            (Language::Yaml, "k: \"open", Class::String),
        ] {
            let toks = tokens(lang, text);
            assert!(
                toks.iter().any(|t| t.class == class && t.end == text.len()),
                "{lang:?} {text:?}: {toks:?}"
            );
            check_cover(lang, text);
        }
        // A string of one line ends at the end of its line.
        assert!(has(Language::Python, "x = 'one line\ny = 1", "'one line", Class::String));
        assert!(has(Language::Python, "x = 'one line\ny = 1", "1", Class::Number));
    }

    #[test]
    fn plain_and_empty_and_large_texts() {
        assert!(tokens(Language::Rust, "").is_empty());
        let t = tokens(Language::Plain, "SELECT 1");
        assert_eq!(t, vec![Token { start: 0, end: 8, class: Class::Plain }]);
        let big = "a".repeat(MAX_HIGHLIGHT_BYTES + 1);
        assert_eq!(tokens(Language::Python, &big).len(), 1);
    }

    #[test]
    fn utf8_text_has_boundaries_and_utf16_offsets() {
        let text = "x = \"héllo 😀\"  # é😀\nlet é = 1";
        for lang in Language::ALL {
            check_cover(lang, text);
        }
        let t = tokens_utf16(Language::Python, text);
        let total: usize = text.chars().map(char::len_utf16).sum();
        assert_eq!(t.first().map(|x| x.0), Some(0));
        assert_eq!(t.last().map(|x| x.1), Some(total));
        // The emoji counts for two units.
        let s = t.iter().find(|x| x.2 == Class::String).unwrap();
        assert_eq!(s.1 - s.0, "\"héllo 😀\"".chars().map(char::len_utf16).sum::<usize>());
    }

    /// A small generator without a dependency.
    struct Soup(u64);
    impl Soup {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            self.0
        }
    }

    #[test]
    fn random_text_never_panics_and_spans_are_in_bounds() {
        const PIECES: &[&str] = &[
            "'", "\"", "`", "/*", "*/", "//", "#", "--", "\n", " ", "\\", "r#\"", "\"\"\"", "'''",
            "${", "[", "]", "{", "}", ":", "=", "é", "😀", "\u{202e}", "\u{0}", "0x", "1e", "1e+",
            ".", "@@", "+++ ", "--- ", "diff ", "- ", "&", "*", "!", "<", ">", "(", ")", "name",
            "select", "fn", "def",
        ];
        let mut rng = Soup(0x9E3779B97F4A7C15);
        for round in 0..400 {
            let mut text = String::new();
            let pieces = 1 + (rng.next() % 200) as usize;
            for _ in 0..pieces {
                if rng.next().is_multiple_of(7) {
                    // A random byte soup, made valid by a lossy conversion.
                    let bytes: Vec<u8> = (0..(rng.next() % 12)).map(|_| rng.next() as u8).collect();
                    text.push_str(&String::from_utf8_lossy(&bytes));
                } else {
                    text.push_str(PIECES[(rng.next() as usize) % PIECES.len()]);
                }
            }
            for lang in Language::ALL {
                check_cover(lang, &text);
                let _ = tokens_utf16(lang, &text);
            }
            let _ = round;
        }
    }

    #[test]
    fn work_is_linear_on_hostile_input() {
        // Each text repeats a construct that would be quadratic with a lookahead that does not stop.
        let cases = [
            "'".repeat(60_000),
            "\"".repeat(60_000),
            "/*".repeat(30_000),
            "r#\"".repeat(20_000),
            "[".repeat(60_000),
            "a: ".repeat(20_000),
            "k = ".repeat(15_000),
            "\\\n".repeat(30_000),
            "@@ ".repeat(20_000),
            "\"a\" \n \n \n".repeat(6_000),
            "x".repeat(60_000),
        ];
        let start = std::time::Instant::now();
        for text in &cases {
            for lang in Language::ALL {
                let toks = tokens(lang, text);
                assert!(toks.iter().map(|t| t.end - t.start).sum::<usize>() == text.len());
            }
        }
        // Eleven texts of 60 KB, eleven languages: far below a second in release and below a few seconds in debug.
        assert!(start.elapsed() < std::time::Duration::from_secs(20), "{:?}", start.elapsed());
    }
}
