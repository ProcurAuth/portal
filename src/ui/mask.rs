//! The names and the structure of the pages that load only the shared stylesheet and the theme
//! script: the sign-in page, the status pages, and the launcher.
//!
//! The renderers write plain names. A final pass, [`Mask::page`], changes the page before the
//! server sends it. The pass has two parts.
//!
//! 1. The names. Each class, each identifier and each data attribute of the shared stylesheet
//!    gets a short name that looks random. The name is an HMAC of the plain name. The key comes
//!    from the session secret. Every instance that has the secret makes the same names, and a
//!    restart does not change them. The same map rewrites the stylesheet and the theme script
//!    ([`Mask::assets`]). The file names of these two files follow their new content.
//! 2. The structure. At each render, the pass inserts 0 to 3 inert elements, changes the order
//!    of the attributes, and changes the order of the elements of the head that have the same
//!    effect. The look of the page does not change: the stylesheet gives `display: contents` to
//!    the wrappers and `display: none` to the empty elements. The stylesheet of these pages has
//!    no combinator that depends on the structure (`>`, `+`, `~`, `:first-child`). A test checks it.
//!
//! This is obscurity, not security. A reader of the page can still see the form, the text and
//! the behavior. See `docs/login-hardening.md`. The settings pages keep the plain names: they
//! need a session, and they load their own files.

use super::{asset_path, assets, Assets};
use crate::crypto;

/// The data attributes of the shared stylesheet and script. The longest name is first.
const DATA_ATTRS: [&str; 5] = [
    "data-theme-switch",
    "data-theme-value",
    "data-theme-mode",
    "data-theme",
    "data-reveal",
];

/// The plain names of the classes of the inert wrappers (`display: contents`) and of the empty
/// elements (`display: none`). The pass maps them like all other names.
const WRAPPER_CLASSES: [&str; 3] = ["wrap-a", "wrap-b", "wrap-c"];
const EMPTY_CLASSES: [&str; 3] = ["void-a", "void-b", "void-c"];

/// The parents that accept a `div`: the content of the page is not a phrase.
const BLOCK_PARENTS: [&str; 9] = [
    "body", "div", "main", "section", "form", "header", "footer", "article", "li",
];
/// The parents that accept only a phrase: a `div` here would end the parent.
const PHRASE_PARENTS: [&str; 11] = [
    "p", "h1", "h2", "h3", "a", "span", "strong", "code", "label", "em", "small",
];
/// The elements that the pass can wrap. A control is not in the list, so a password manager
/// always finds a control in the same place as before.
const WRAPPABLE: [&str; 16] = [
    "div", "main", "section", "header", "form", "p", "h1", "h2", "ul", "a", "span", "strong",
    "code", "label", "svg", "footer",
];
/// The elements that a `span` cannot hold.
const BLOCK_ELEMENTS: [&str; 10] = [
    "div", "main", "section", "header", "form", "p", "h1", "h2", "ul", "footer",
];
const VOID_TAGS: [&str; 14] = [
    "area", "base", "br", "col", "embed", "hr", "img", "input", "link", "meta", "param", "source",
    "track", "wbr",
];

/// The mapped assets and the key of the names of one instance.
pub struct Mask {
    key: [u8; 32],
    assets: Assets,
}

impl Mask {
    /// Makes the key from `secret`, and the mapped stylesheet and script from the shipped ones.
    pub fn new(secret: &[u8]) -> Result<Self, String> {
        let key = crypto::ui_names_key(secret)?;
        let mut mask = Self {
            key,
            assets: Assets {
                css_path: String::new(),
                css: String::new(),
                js_path: String::new(),
                js: String::new(),
                notify_path: String::new(),
                notify: String::new(),
            },
        };
        let plain = assets();
        let mut css = walk_css(&plain.css, |selectors| {
            map_selectors(selectors, &mut |name| mask.name(name))
        });
        // The inert elements need their rules in the same file.
        let rule = |classes: &[&str], declaration: &str| {
            let list: Vec<String> = classes
                .iter()
                .map(|class| format!(".{}", mask.name(class)))
                .collect();
            format!("{}{{{declaration}}}", list.join(","))
        };
        css.push_str(&rule(&WRAPPER_CLASSES, "display:contents"));
        css.push_str(&rule(&EMPTY_CLASSES, "display:none"));
        let js = map_script(&plain.js, &mut |name| mask.name(name));
        let notify = map_script(&plain.notify, &mut |name| mask.name(name));
        mask.assets = Assets {
            css_path: asset_path(&css, "css"),
            js_path: asset_path(&js, "js"),
            notify_path: asset_path(&notify, "js"),
            css,
            js,
            notify,
        };
        Ok(mask)
    }

    /// The stylesheet and the script with the mapped names. The paths follow the content.
    pub fn assets(&self) -> &Assets {
        &self.assets
    }

    /// The mapped name of a plain class name, identifier or data attribute name. It has 6 to 10
    /// characters of `a-z0-9`, and it starts with a letter.
    pub fn name(&self, plain: &str) -> String {
        const ALPHABET: &[u8; 36] = b"abcdefghijklmnopqrstuvwxyz0123456789";
        let digest = crypto::ui_name_digest(&self.key, plain);
        let length = 6 + usize::from(digest[31]) % 5;
        let mut out = String::with_capacity(length);
        out.push(char::from(b'a' + digest[0] % 26));
        for byte in &digest[1..length] {
            out.push(char::from(ALPHABET[usize::from(*byte) % ALPHABET.len()]));
        }
        out
    }

    /// The page as the server sends it: other structure at each call, mapped names, mapped assets.
    pub fn page(&self, html: &str) -> String {
        self.page_with(html, &mut Rng::from_os())
    }

    fn page_with(&self, html: &str, rng: &mut Rng) -> String {
        let mut tokens = tokenize(html);
        if let Some(varied) = vary(&tokens, rng) {
            tokens = varied;
        }
        let plain = assets();
        for token in &mut tokens {
            if let Tok::Open { attrs, .. } = token {
                for attr in attrs.iter_mut() {
                    self.map_attribute(attr, plain);
                }
            }
        }
        serialize(&tokens)
    }

    fn map_attribute(&self, attr: &mut Attr, plain: &Assets) {
        let lower = attr.name.to_ascii_lowercase();
        if DATA_ATTRS.contains(&lower.as_str()) {
            attr.name = format!("data-{}", self.name(&lower));
            return;
        }
        let Some((_, value)) = attr.value.as_mut() else {
            return;
        };
        match lower.as_str() {
            "class" | "id" | "for" | "aria-labelledby" | "aria-controls" | "aria-describedby"
            | "aria-owns" => {
                let mapped: Vec<String> = value
                    .split_ascii_whitespace()
                    .map(|name| self.name(name))
                    .collect();
                *value = mapped.join(" ");
            }
            "href" | "xlink:href" => {
                if let Some(target) = value.strip_prefix('#') {
                    *value = format!("#{}", self.name(target));
                } else if *value == plain.css_path {
                    *value = self.assets.css_path.clone();
                }
            }
            "src" if *value == plain.js_path => *value = self.assets.js_path.clone(),
            "src" if *value == plain.notify_path => *value = self.assets.notify_path.clone(),
            _ => {}
        }
    }
}

// The stylesheet ---------------------------------------------------------------------------

fn is_ident_start(c: char) -> bool {
    c.is_ascii_alphabetic() || c == '_' || c == '-' || !c.is_ascii()
}

fn is_ident(c: char) -> bool {
    is_ident_start(c) || c.is_ascii_digit()
}

fn ident_end(chars: &[char], from: usize) -> usize {
    let mut end = from;
    while end < chars.len() && is_ident(chars[end]) {
        end += 1;
    }
    end
}

/// The index after the string that starts at `from` (a quote).
fn string_end(chars: &[char], from: usize) -> usize {
    let quote = chars[from];
    let mut end = from + 1;
    while end < chars.len() {
        match chars[end] {
            '\\' => end += 2,
            c if c == quote => return end + 1,
            _ => end += 1,
        }
    }
    chars.len()
}

fn text(chars: &[char], from: usize, to: usize) -> String {
    chars[from..to.min(chars.len())].iter().collect()
}

/// Gives each list of selectors of a minified stylesheet to `on_selectors`, and puts the result
/// in its place. The rest of the stylesheet stays as it is. The blocks `@media` and `@supports`
/// hold rules. The other at-rules, and the bodies of the rules, are not touched.
fn walk_css(css: &str, mut on_selectors: impl FnMut(&str) -> String) -> String {
    #[derive(PartialEq)]
    enum Block {
        Rules,
        Other,
    }
    let chars: Vec<char> = css.chars().collect();
    let mut out = String::with_capacity(css.len());
    let mut stack: Vec<Block> = Vec::new();
    let mut prelude = String::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        let in_rules = matches!(stack.last(), None | Some(Block::Rules));
        if c == '"' || c == '\'' {
            let end = string_end(&chars, i);
            let piece = text(&chars, i, end);
            if in_rules {
                prelude.push_str(&piece);
            } else {
                out.push_str(&piece);
            }
            i = end;
            continue;
        }
        i += 1;
        if !in_rules {
            out.push(c);
            match c {
                '{' => stack.push(Block::Other),
                '}' => {
                    stack.pop();
                }
                _ => {}
            }
            continue;
        }
        match c {
            '{' => {
                if let Some(at) = prelude.trim_start().strip_prefix('@') {
                    let name: String = at.chars().take_while(|c| c.is_ascii_alphabetic()).collect();
                    out.push_str(&prelude);
                    stack.push(
                        if matches!(name.as_str(), "media" | "supports" | "layer" | "container") {
                            Block::Rules
                        } else {
                            Block::Other
                        },
                    );
                } else {
                    out.push_str(&on_selectors(&prelude));
                    stack.push(Block::Other);
                }
                out.push('{');
                prelude.clear();
            }
            '}' => {
                out.push_str(&prelude);
                out.push('}');
                prelude.clear();
                stack.pop();
            }
            ';' => {
                out.push_str(&prelude);
                out.push(';');
                prelude.clear();
            }
            c => prelude.push(c),
        }
    }
    out.push_str(&prelude);
    out
}

/// Maps the classes, the identifiers and the data attributes of one list of selectors. The names
/// of the pseudo-classes, of the types and the values of the attributes stay as they are.
fn map_selectors(selectors: &str, name: &mut impl FnMut(&str) -> String) -> String {
    let chars: Vec<char> = selectors.chars().collect();
    let mut out = String::with_capacity(selectors.len());
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        match c {
            '"' | '\'' => {
                let end = string_end(&chars, i);
                out.push_str(&text(&chars, i, end));
                i = end;
            }
            '.' | '#' if chars.get(i + 1).is_some_and(|next| is_ident_start(*next)) => {
                let end = ident_end(&chars, i + 1);
                out.push(c);
                out.push_str(&name(&text(&chars, i + 1, end)));
                i = end;
            }
            ':' => {
                out.push(c);
                i += 1;
                while chars.get(i) == Some(&':') {
                    out.push(':');
                    i += 1;
                }
                let end = ident_end(&chars, i);
                out.push_str(&text(&chars, i, end));
                i = end;
            }
            '[' => {
                out.push(c);
                let end = ident_end(&chars, i + 1);
                let attr = text(&chars, i + 1, end);
                if DATA_ATTRS.contains(&attr.as_str()) {
                    out.push_str(&format!("data-{}", name(&attr)));
                } else {
                    out.push_str(&attr);
                }
                i = end;
            }
            c => {
                out.push(c);
                i += 1;
            }
        }
    }
    out
}

/// The classes and the identifiers that the stylesheet names. For the tests and the documents.
#[cfg(test)]
pub(crate) fn names_in_stylesheet(css: &str) -> std::collections::BTreeSet<String> {
    let mut found = std::collections::BTreeSet::new();
    walk_css(css, |selectors| {
        map_selectors(selectors, &mut |name| {
            found.insert(name.to_string());
            name.to_string()
        })
    });
    found
}

/// Every list of selectors of the stylesheet, as written.
#[cfg(test)]
pub(crate) fn selector_lists(css: &str) -> Vec<String> {
    let mut found = Vec::new();
    walk_css(css, |selectors| {
        found.push(selectors.to_string());
        selectors.to_string()
    });
    found
}

// The script -------------------------------------------------------------------------------

/// Maps the strings of a minified script that name the class `js` or a data attribute. The
/// script has no template literal and no regular expression: the strings are easy to find.
fn map_script(script: &str, name: &mut impl FnMut(&str) -> String) -> String {
    let chars: Vec<char> = script.chars().collect();
    let mut out = String::with_capacity(script.len());
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c != '"' && c != '\'' {
            out.push(c);
            i += 1;
            continue;
        }
        let end = string_end(&chars, i);
        let inner = text(&chars, i + 1, end.saturating_sub(1));
        out.push(c);
        if inner == "js" {
            out.push_str(&name("js"));
        } else {
            out.push_str(&map_data_attributes(&inner, name));
        }
        out.push(c);
        i = end;
    }
    out
}

/// Replaces each data attribute name of `DATA_ATTRS` in `text`.
fn map_data_attributes(text: &str, name: &mut impl FnMut(&str) -> String) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    'scan: while let Some(c) = rest.chars().next() {
        for attr in DATA_ATTRS {
            let long_enough = rest
                .strip_prefix(attr)
                .is_some_and(|after| !after.starts_with(is_ident));
            if long_enough {
                out.push_str(&format!("data-{}", name(attr)));
                rest = &rest[attr.len()..];
                continue 'scan;
            }
        }
        out.push(c);
        rest = &rest[c.len_utf8()..];
    }
    out
}

// The page ---------------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq)]
struct Attr {
    name: String,
    /// The quote and the raw text of the value. `None`: a boolean attribute.
    value: Option<(char, String)>,
}

#[derive(Clone, Debug, PartialEq)]
enum Tok {
    Text(String),
    Open {
        name: String,
        attrs: Vec<Attr>,
        self_closing: bool,
    },
    Close(String),
}

impl Tok {
    fn open(name: &str, attrs: Vec<Attr>) -> Tok {
        Tok::Open {
            name: name.to_string(),
            attrs,
            self_closing: false,
        }
    }

    fn is_void(&self) -> bool {
        matches!(self, Tok::Open { name, self_closing, .. }
            if *self_closing || VOID_TAGS.contains(&name.as_str()))
    }
}

fn attribute(name: &str, value: &str) -> Attr {
    Attr {
        name: name.to_string(),
        value: Some(('"', value.to_string())),
    }
}

/// Reads the HTML that the renderers write. It never fails: a `<` that does not start a tag is
/// text. The renderers escape all the text of a user.
fn tokenize(html: &str) -> Vec<Tok> {
    let chars: Vec<char> = html.chars().collect();
    let mut out: Vec<Tok> = Vec::new();
    let mut plain = String::new();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] != '<' {
            plain.push(chars[i]);
            i += 1;
            continue;
        }
        match read_tag(&chars, i) {
            Some((tok, end)) => {
                if !plain.is_empty() {
                    out.push(Tok::Text(std::mem::take(&mut plain)));
                }
                i = end;
                let raw_text = match &tok {
                    Tok::Open {
                        name,
                        self_closing: false,
                        ..
                    } if name == "script" || name == "style" => Some(name.clone()),
                    _ => None,
                };
                out.push(tok);
                if let Some(name) = raw_text {
                    // The content of a script or of a style is text up to its end tag.
                    let close = format!("</{name}");
                    let mut end = i;
                    while end < chars.len()
                        && !text(&chars, end, end + close.len()).eq_ignore_ascii_case(&close)
                    {
                        end += 1;
                    }
                    if end > i {
                        out.push(Tok::Text(text(&chars, i, end)));
                    }
                    i = end;
                }
            }
            None => {
                plain.push('<');
                i += 1;
            }
        }
    }
    if !plain.is_empty() {
        out.push(Tok::Text(plain));
    }
    out
}

/// Reads the tag that starts at `from`. Gives the token and the index after the tag. A doctype
/// and a comment are text.
fn read_tag(chars: &[char], from: usize) -> Option<(Tok, usize)> {
    let next = *chars.get(from + 1)?;
    if next == '!' {
        let end = chars[from..].iter().position(|c| *c == '>')? + from + 1;
        return Some((Tok::Text(text(chars, from, end)), end));
    }
    if next == '/' {
        let start = from + 2;
        if !chars.get(start).is_some_and(|c| c.is_ascii_alphabetic()) {
            return None;
        }
        let end = chars[start..].iter().position(|c| *c == '>')? + start;
        let name = text(chars, start, end).trim().to_ascii_lowercase();
        return Some((Tok::Close(name), end + 1));
    }
    if !next.is_ascii_alphabetic() {
        return None;
    }
    let mut i = from + 1;
    let start = i;
    while i < chars.len() && !chars[i].is_whitespace() && chars[i] != '/' && chars[i] != '>' {
        i += 1;
    }
    let name = text(chars, start, i).to_ascii_lowercase();
    let mut attrs = Vec::new();
    loop {
        while i < chars.len() && chars[i].is_whitespace() {
            i += 1;
        }
        match chars.get(i)? {
            '>' => {
                return Some((
                    Tok::Open {
                        name,
                        attrs,
                        self_closing: false,
                    },
                    i + 1,
                ))
            }
            '/' => {
                if chars.get(i + 1) == Some(&'>') {
                    return Some((
                        Tok::Open {
                            name,
                            attrs,
                            self_closing: true,
                        },
                        i + 2,
                    ));
                }
                i += 1;
            }
            _ => {
                let start = i;
                while i < chars.len()
                    && !chars[i].is_whitespace()
                    && !matches!(chars[i], '=' | '>' | '/')
                {
                    i += 1;
                }
                let attr_name = text(chars, start, i);
                while i < chars.len() && chars[i].is_whitespace() {
                    i += 1;
                }
                if chars.get(i) != Some(&'=') {
                    attrs.push(Attr {
                        name: attr_name,
                        value: None,
                    });
                    continue;
                }
                i += 1;
                while i < chars.len() && chars[i].is_whitespace() {
                    i += 1;
                }
                let quote = *chars.get(i)?;
                if quote == '"' || quote == '\'' {
                    let end = chars[i + 1..].iter().position(|c| *c == quote)? + i + 1;
                    attrs.push(Attr {
                        name: attr_name,
                        value: Some((quote, text(chars, i + 1, end))),
                    });
                    i = end + 1;
                } else {
                    let start = i;
                    while i < chars.len() && !chars[i].is_whitespace() && chars[i] != '>' {
                        i += 1;
                    }
                    attrs.push(Attr {
                        name: attr_name,
                        value: Some(('"', text(chars, start, i))),
                    });
                }
            }
        }
    }
}

fn serialize(tokens: &[Tok]) -> String {
    let mut out = String::new();
    for token in tokens {
        match token {
            Tok::Text(text) => out.push_str(text),
            Tok::Close(name) => {
                out.push_str("</");
                out.push_str(name);
                out.push('>');
            }
            Tok::Open {
                name,
                attrs,
                self_closing,
            } => {
                out.push('<');
                out.push_str(name);
                for attr in attrs {
                    out.push(' ');
                    out.push_str(&attr.name);
                    if let Some((quote, value)) = &attr.value {
                        out.push('=');
                        out.push(*quote);
                        out.push_str(value);
                        out.push(*quote);
                    }
                }
                out.push_str(if *self_closing { "/>" } else { ">" });
            }
        }
    }
    out
}

/// The shape of the elements: for each token, the index of its end tag, its parent, and whether
/// it is inside an `svg`.
struct Tree {
    end: Vec<usize>,
    parent: Vec<Option<usize>>,
    in_svg: Vec<bool>,
}

/// `None` when the end tags do not match the start tags. The pass then keeps the order.
fn tree(tokens: &[Tok]) -> Option<Tree> {
    let mut tree = Tree {
        end: (0..tokens.len()).collect(),
        parent: vec![None; tokens.len()],
        in_svg: vec![false; tokens.len()],
    };
    let mut stack: Vec<usize> = Vec::new();
    for (index, token) in tokens.iter().enumerate() {
        tree.parent[index] = stack.last().copied();
        tree.in_svg[index] = stack
            .iter()
            .any(|open| matches!(&tokens[*open], Tok::Open { name, .. } if name == "svg"));
        match token {
            Tok::Open { .. } if !token.is_void() => stack.push(index),
            Tok::Close(name) => {
                let open = stack.pop()?;
                match &tokens[open] {
                    Tok::Open { name: opened, .. } if opened == name => tree.end[open] = index,
                    _ => return None,
                }
            }
            _ => {}
        }
    }
    stack.is_empty().then_some(tree)
}

fn open_name(token: &Tok) -> &str {
    match token {
        Tok::Open { name, .. } => name,
        _ => "",
    }
}

/// A small generator. The variation of a page does not need a secret generator, only a value
/// that an observer cannot guess.
struct Rng(u64);

impl Rng {
    fn from_os() -> Self {
        let mut bytes = [0u8; 8];
        if crypto::random_bytes(&mut bytes).is_err() {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_nanos());
            bytes = (now as u64).to_le_bytes();
        }
        Rng(u64::from_le_bytes(bytes))
    }

    #[cfg(test)]
    fn seeded(seed: u64) -> Self {
        Rng(seed)
    }

    /// SplitMix64.
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }

    fn shuffle<T>(&mut self, items: &mut [T]) {
        for i in (1..items.len()).rev() {
            items.swap(i, self.below(i + 1));
        }
    }
}

/// Changes what has no meaning: the order of the attributes, the order of the elements of the
/// head that have the same effect, and the inert elements. `None` when the page is not well formed.
fn vary(tokens: &[Tok], rng: &mut Rng) -> Option<Vec<Tok>> {
    let mut tokens = tokens.to_vec();
    tree(&tokens)?;
    for token in &mut tokens {
        if let Tok::Open { attrs, .. } = token {
            rng.shuffle(attrs);
        }
    }
    let tokens = shuffle_head(tokens, rng)?;
    insert_inert(tokens, rng)
}

fn is_charset_meta(unit: &[Tok]) -> bool {
    matches!(unit.first(), Some(Tok::Open { name, attrs, .. })
        if name == "meta" && attrs.iter().any(|a| a.name == "charset"))
}

/// The elements of the head, except the charset, in any order. The script and the stylesheet are
/// both loaded before the first paint, so their order has no effect.
fn shuffle_head(tokens: Vec<Tok>, rng: &mut Rng) -> Option<Vec<Tok>> {
    let tree = tree(&tokens)?;
    let Some(head) = tokens
        .iter()
        .position(|t| matches!(t, Tok::Open { name, .. } if name == "head"))
    else {
        return Some(tokens);
    };
    let close = tree.end[head];
    let mut units: Vec<Vec<Tok>> = Vec::new();
    let mut index = head + 1;
    while index < close {
        let end = tree.end[index];
        units.push(tokens[index..=end].to_vec());
        index = end + 1;
    }
    let fixed = usize::from(units.first().is_some_and(|unit| is_charset_meta(unit)));
    rng.shuffle(&mut units[fixed..]);
    let mut out = tokens[..=head].to_vec();
    out.extend(units.into_iter().flatten());
    out.extend_from_slice(&tokens[close..]);
    Some(out)
}

/// Inserts 0 to 3 elements with no content of their own: wrappers around an element, and empty
/// elements between the children of a block.
fn insert_inert(tokens: Vec<Tok>, rng: &mut Rng) -> Option<Vec<Tok>> {
    let tree = tree(&tokens)?;
    let body = tokens
        .iter()
        .position(|t| matches!(t, Tok::Open { name, .. } if name == "body"))?;
    let body_end = tree.end[body];
    let inside = |index: usize| index > body && index < body_end && !tree.in_svg[index];
    // The elements that a wrapper can enclose, with the tag of the wrapper.
    let mut wrappable: Vec<(usize, &'static [&'static str])> = Vec::new();
    // The elements that can hold an empty element.
    let mut holders: Vec<usize> = vec![body];
    for (index, token) in tokens.iter().enumerate() {
        let Tok::Open { name, .. } = token else {
            continue;
        };
        if !inside(index) {
            continue;
        }
        let parent = open_name(&tokens[tree.parent[index]?]);
        // A label that holds a field (the box "Remember me", the trap field) stays as it is.
        let holds_field = name == "label"
            && tokens[index..=tree.end[index]]
                .iter()
                .any(|t| matches!(t, Tok::Open { name, .. } if name == "input"));
        if WRAPPABLE.contains(&name.as_str()) && !holds_field {
            let block = BLOCK_ELEMENTS.contains(&name.as_str());
            if BLOCK_PARENTS.contains(&parent) {
                // A `span` can hold a phrase only.
                wrappable.push((index, if block { &["div"] } else { &["div", "span"] }));
            } else if PHRASE_PARENTS.contains(&parent) && !block {
                wrappable.push((index, &["span"]));
            }
        }
        if BLOCK_PARENTS.contains(&name.as_str()) && !token.is_void() {
            holders.push(index);
        }
    }

    // (first token, last token, tag, class) of the wrappers; (token that follows, class) of the empty elements.
    let mut wraps: Vec<(usize, usize, &str, &str)> = Vec::new();
    let mut empties: Vec<(usize, &str)> = Vec::new();
    for _ in 0..rng.below(4) {
        if rng.below(2) == 0 && !wrappable.is_empty() {
            let (index, tags) = wrappable[rng.below(wrappable.len())];
            let tag = tags[rng.below(tags.len())];
            let class = WRAPPER_CLASSES[rng.below(WRAPPER_CLASSES.len())];
            wraps.push((index, tree.end[index], tag, class));
        } else {
            let holder = holders[rng.below(holders.len())];
            // The places between the children of the holder, the end of the holder included.
            let mut places = Vec::new();
            let mut index = holder + 1;
            while index < tree.end[holder] {
                places.push(index);
                index = tree.end[index] + 1;
            }
            places.push(tree.end[holder]);
            let class = EMPTY_CLASSES[rng.below(EMPTY_CLASSES.len())];
            empties.push((places[rng.below(places.len())], class));
        }
    }
    // An outer wrapper opens first and closes last.
    // Two wrappers of one element: the `div` is the outer one, because a `span` cannot hold it.
    wraps.sort_by_key(|(first, last, tag, _)| (*first, std::cmp::Reverse(*last), *tag != "div"));

    let mut out = Vec::with_capacity(tokens.len() + 12);
    for (index, token) in tokens.into_iter().enumerate() {
        for (_, class) in empties.iter().filter(|(place, _)| *place == index) {
            out.push(Tok::open(
                "div",
                vec![attribute("class", class), attribute("aria-hidden", "true")],
            ));
            out.push(Tok::Close("div".to_string()));
        }
        for (_, _, tag, class) in wraps.iter().filter(|(first, ..)| *first == index) {
            out.push(Tok::open(tag, vec![attribute("class", class)]));
        }
        out.push(token);
        for (_, _, tag, _) in wraps.iter().rev().filter(|(_, last, ..)| *last == index) {
            out.push(Tok::Close((*tag).to_string()));
        }
    }
    Some(out)
}

#[cfg(test)]
impl Mask {
    /// Whether the page has an element `tag` with all these attributes. `None` as value: the
    /// attribute is a boolean attribute. The values are the values in the page, so mapped.
    pub(crate) fn has_element(html: &str, tag: &str, wanted: &[(&str, Option<&str>)]) -> bool {
        tokenize(html).iter().any(|token| {
            let Tok::Open { name, attrs, .. } = token else {
                return false;
            };
            name == tag
                && wanted.iter().all(|(wanted_name, wanted_value)| {
                    attrs.iter().any(|attr| {
                        attr.name == *wanted_name
                            && attr.value.as_ref().map(|(_, v)| v.as_str()) == *wanted_value
                    })
                })
        })
    }

    /// The names in the attributes `class` (`ids` false) or `id` (`ids` true) of a page.
    pub(crate) fn names_in_page(html: &str, ids: bool) -> Vec<String> {
        let wanted = if ids { "id" } else { "class" };
        let mut out = Vec::new();
        for token in tokenize(html) {
            if let Tok::Open { attrs, .. } = token {
                for attr in attrs {
                    if let (true, Some((_, value))) = (attr.name == wanted, attr.value) {
                        out.extend(value.split_whitespace().map(str::to_string));
                    }
                }
            }
        }
        out
    }

    /// The page without what the variation inserted and with the attributes and the head in a
    /// fixed order. Two renders of the same page have the same canonical form.
    pub(crate) fn canonical(&self, html: &str) -> String {
        let tokens = tokenize(html);
        let inert: std::collections::BTreeSet<String> = WRAPPER_CLASSES
            .iter()
            .chain(EMPTY_CLASSES.iter())
            .map(|class| self.name(class))
            .collect();
        let shape = tree(&tokens).expect("a well formed page");
        let is_inert = |token: &Tok| match token {
            Tok::Open { attrs, .. } => attrs.iter().any(|a| {
                a.name == "class" && a.value.as_ref().is_some_and(|(_, v)| inert.contains(v))
            }),
            _ => false,
        };
        let mut drop = vec![false; tokens.len()];
        for (index, token) in tokens.iter().enumerate() {
            if is_inert(token) {
                drop[index] = true;
                drop[shape.end[index]] = true;
            }
        }
        let mut kept: Vec<Tok> = Vec::new();
        for (index, token) in tokens.iter().enumerate() {
            if drop[index] {
                continue;
            }
            let mut token = token.clone();
            if let Tok::Open { attrs, .. } = &mut token {
                attrs.sort_by(|a, b| a.name.cmp(&b.name));
            }
            kept.push(token);
        }
        // The elements of the head in the order of their text.
        let shape = tree(&kept).expect("a well formed page");
        let head = kept
            .iter()
            .position(|t| matches!(t, Tok::Open { name, .. } if name == "head"))
            .expect("a head");
        let mut units: Vec<String> = Vec::new();
        let mut index = head + 1;
        while index < shape.end[head] {
            let end = shape.end[index];
            units.push(serialize(&kept[index..=end]));
            index = end + 1;
        }
        units.sort();
        format!(
            "{}{}{}",
            serialize(&kept[..=head]),
            units.concat(),
            serialize(&kept[shape.end[head]..])
        )
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;
    use crate::icons::Icons;
    use crate::ui::{
        demo_catalog, denied_page, home_page, message_page, HomeContext, CSS, THEME_JS,
    };

    const SECRET: &[u8] = b"a secret of the tests, long enough for the key";

    fn mask() -> Mask {
        Mask::new(SECRET).unwrap()
    }

    /// The sign-in page, as the server writes it. `rd` is the return target: `/` has no band.
    fn login_page_for(rd: &str) -> String {
        crate::ui::signin::login_page(&crate::ui::signin::Login {
            csrf: "c",
            rd,
            sig: "s",
            form_token: "t",
            host: Some("app.example.com"),
            error: Some(crate::ui::signin::INVALID_CREDENTIALS),
            max_password_bytes: 1024,
            keep_for: Some("30 days"),
            names: crate::ui::signin::LoginNames {
                form_token: "token",
                trap: "website",
                keep: "keep",
                keep_value: "1",
            },
        })
    }

    /// The sign-in page with a band.
    fn login_page_html() -> String {
        login_page_for("/x")
    }

    /// The card of the second step, with a band.
    fn code_page_html() -> String {
        crate::ui::signin::code_page(&crate::ui::signin::Code {
            hidden: r#"<input type="hidden" name="csrf" value="c"><input type="hidden" name="pending" value="p">"#,
            rd: "/x",
            host: Some("app.example.com"),
            error: Some("Invalid code"),
            invalid: "Invalid code",
            action: "/auth/login/code",
        })
    }

    /// The launcher of an account that receives notifications: the inbox icon, its popover with
    /// two rows, the toasts and the script.
    fn launcher() -> String {
        use crate::inbox::notify::{Importance, Level};
        let rows = [
            crate::ui::notify::Row {
                id: "aa".into(),
                title: "Disk full".into(),
                group: "Infra".into(),
                importance: Importance::Critical,
                created_at_ms: 1_000,
                href: Some("/auth/settings/inbox?open=aa#a-aa".into()),
                unread: true,
            },
            crate::ui::notify::Row {
                id: "bb".into(),
                title: "Hello".into(),
                group: "Direct".into(),
                importance: Importance::Ring,
                created_at_ms: 500,
                href: None,
                unread: false,
            },
        ];
        let bell = crate::ui::notify::Bell {
            header: crate::ui::notify::Header {
                level: Level::All,
                unread: 1,
                cursor_ms: 2_000,
            },
            rows: &rows,
            now_ms: 2_000,
            csrf: "tok",
            forms: true,
        };
        home_page(
            &demo_catalog(),
            &Icons::empty(),
            &HomeContext {
                viewer: "alice",
                impersonating: true,
                logout_token: "t",
                users_token: "u",
                hidden_services: 0,
                settings: Some("/auth/settings/access"),
                bell: Some(&bell),
            },
        )
    }

    /// Every page that the mask changes.
    fn pages() -> Vec<(&'static str, String)> {
        vec![
            ("login", login_page_html()),
            ("login without band", login_page_for("/")),
            ("code", code_page_html()),
            ("denied", denied_page("No.", "alice", "t")),
            ("message", message_page("Page not found", "x", "Back")),
            ("launcher", launcher()),
        ]
    }

    /// The classes and the identifiers of a page, in the attributes that name them.
    fn names_of(html: &str) -> (BTreeSet<String>, BTreeSet<String>) {
        let mut classes = BTreeSet::new();
        let mut ids = BTreeSet::new();
        for token in tokenize(html) {
            let Tok::Open { attrs, .. } = token else {
                continue;
            };
            for attr in attrs {
                let Some((_, value)) = attr.value else {
                    continue;
                };
                match attr.name.as_str() {
                    "class" => classes.extend(value.split_whitespace().map(str::to_string)),
                    "id" => {
                        ids.insert(value);
                    }
                    _ => {}
                }
            }
        }
        (classes, ids)
    }

    #[test]
    fn two_instances_with_the_same_secret_make_the_same_names_and_the_same_files() {
        let (a, b) = (mask(), Mask::new(SECRET).unwrap());
        for plain in [
            "card",
            "hp",
            "button--primary",
            "login",
            "data-theme",
            "i-sun",
        ] {
            assert_eq!(a.name(plain), b.name(plain), "{plain}");
        }
        assert_eq!(a.assets().css, b.assets().css);
        assert_eq!(a.assets().js, b.assets().js);
        assert_eq!(a.assets().css_path, b.assets().css_path);
        assert_eq!(a.assets().js_path, b.assets().js_path);
    }

    #[test]
    fn two_secrets_make_other_names_and_other_files() {
        let (a, b) = (
            mask(),
            Mask::new(b"another secret of the tests, long enough").unwrap(),
        );
        let same = ["card", "hp", "button", "tiles", "tile__name", "authShell"]
            .iter()
            .filter(|plain| a.name(plain) == b.name(plain))
            .count();
        assert_eq!(same, 0);
        assert_ne!(a.assets().css, b.assets().css);
        assert_ne!(a.assets().js, b.assets().js);
        assert_ne!(a.assets().css_path, b.assets().css_path);
        assert_ne!(a.assets().js_path, b.assets().js_path);
    }

    #[test]
    fn a_name_looks_random_and_two_names_differ() {
        let mask = mask();
        let mut plain: BTreeSet<String> = names_in_stylesheet(&assets().css);
        plain.extend(WRAPPER_CLASSES.iter().map(|s| s.to_string()));
        plain.extend(EMPTY_CLASSES.iter().map(|s| s.to_string()));
        plain.extend(DATA_ATTRS.iter().map(|s| s.to_string()));
        plain.insert("js".to_string());
        for (_, html) in pages() {
            let (classes, ids) = names_of(&html);
            plain.extend(classes);
            plain.extend(ids);
        }
        assert!(plain.len() > 60, "{}", plain.len());
        let mut seen = BTreeSet::new();
        for name in &plain {
            let mapped = mask.name(name);
            assert!((6..=10).contains(&mapped.len()), "{mapped}");
            assert!(mapped.as_bytes()[0].is_ascii_lowercase(), "{mapped}");
            assert!(
                mapped
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit()),
                "{mapped}"
            );
            assert!(seen.insert(mapped.clone()), "{name} and {mapped} collide");
        }
    }

    #[test]
    fn the_stylesheet_of_these_pages_has_no_rule_that_the_structure_can_break() {
        for list in selector_lists(&assets().css) {
            for part in [
                "+",
                "~",
                "first-child",
                "last-child",
                "nth-",
                "of-type",
                "only-",
            ] {
                assert!(!list.contains(part), "{part} in {list}");
            }
            // A child combinator is a `>` outside the parentheses of a function.
            let mut depth = 0;
            for c in list.chars() {
                match c {
                    '(' | '[' => depth += 1,
                    ')' | ']' => depth -= 1,
                    '>' if depth == 0 => panic!("a child combinator in {list}"),
                    _ => {}
                }
            }
        }
    }

    #[test]
    fn the_mapped_stylesheet_has_none_of_the_plain_names() {
        let mask = mask();
        let plain = names_in_stylesheet(&assets().css);
        assert!(plain.contains("card") && plain.contains("hp") && plain.contains("js"));
        let mapped = names_in_stylesheet(&mask.assets().css);
        for name in plain
            .iter()
            .filter(|name| !DATA_ATTRS.contains(&name.as_str()))
        {
            assert!(!mapped.contains(name), "{name} is still in the stylesheet");
            assert!(mapped.contains(&mask.name(name)), "{name} has no rule");
        }
        assert!(!mask.assets().css.contains("data-theme"));
        // The files keep their meaning: the values, the colors and the media queries stay.
        assert!(mask
            .assets()
            .css
            .contains("@media (prefers-color-scheme: light)"));
        assert!(mask.assets().css.contains("--bg: #0d0f12"));
        assert!(!mask.assets().css.contains(r#"[data-theme="dark"]"#));
        let theme = format!(r#"[data-{}="dark"]"#, mask.name("data-theme"));
        assert!(mask.assets().css.contains(&theme), "{theme}");
        assert!(mask.assets().css.contains(r#"[aria-pressed="true"]"#));
    }

    #[test]
    fn the_stylesheet_walker_changes_the_selectors_and_nothing_else() {
        let css = "@media (max-width:640px){.a .b,#c>d{margin:0 .5em;content:\".x\"}}\
                   a:not(.k)[data-theme=\"d.e\"]::before{color:red}@keyframes pulse{0%{opacity:1}}.m:hover{top:0}";
        let upper = |name: &str| name.to_uppercase();
        let out = walk_css(css, |selectors| map_selectors(selectors, &mut |n| upper(n)));
        assert_eq!(
            out,
            "@media (max-width:640px){.A .B,#C>d{margin:0 .5em;content:\".x\"}}\
             a:not(.K)[data-DATA-THEME=\"d.e\"]::before{color:red}@keyframes pulse{0%{opacity:1}}.M:hover{top:0}"
        );
    }

    #[test]
    fn the_mapped_script_has_none_of_the_plain_names_and_still_parses() {
        let mask = mask();
        let js = &mask.assets().js;
        assert!(THEME_JS.contains("data-theme-switch"));
        assert!(!js.contains("data-theme"), "{js}");
        assert!(!js.contains(r#""js""#), "{js}");
        for attr in DATA_ATTRS {
            assert!(js.contains(&format!("data-{}", mask.name(attr))), "{attr}");
        }
        assert!(js.contains(&format!(r#""{}""#, mask.name("js"))));
        // The key of the storage and the values of the theme do not change.
        assert!(js.contains("ui-color-mode") && js.contains(r#""dark""#));
        let dir = std::env::temp_dir().join(format!("mask-script-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("script.js");
        std::fs::write(&file, js).unwrap();
        match std::process::Command::new("node")
            .arg("--check")
            .arg(&file)
            .output()
        {
            Ok(output) => assert!(
                output.status.success(),
                "node --check: {}",
                String::from_utf8_lossy(&output.stderr)
            ),
            Err(_) => eprintln!("node is not in this image: the syntax check is skipped"),
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_script_maps_a_data_attribute_only_as_a_whole_name() {
        let out = map_data_attributes("[data-theme-switch] data-themes data-theme", &mut |n| {
            n.to_uppercase()
        });
        assert_eq!(out, "[data-DATA-THEME-SWITCH] data-themes data-DATA-THEME");
    }

    #[test]
    fn a_page_that_is_read_and_written_again_does_not_change() {
        for (name, html) in pages() {
            assert_eq!(serialize(&tokenize(&html)), html, "{name}");
        }
    }

    #[test]
    fn the_pages_have_none_of_the_plain_names_and_every_class_has_a_rule() {
        let mask = mask();
        let mapped_css = names_in_stylesheet(&mask.assets().css);
        for (name, html) in pages() {
            let (plain_classes, plain_ids) = names_of(&html);
            assert!(
                plain_classes.contains("card")
                    || plain_classes.contains("signin")
                    || name == "launcher"
            );
            let page = mask.page(&html);
            let (classes, ids) = names_of(&page);
            for plain in plain_classes.iter().chain(plain_ids.iter()) {
                assert!(
                    !classes.contains(plain) && !ids.contains(plain),
                    "{name}: {plain}"
                );
                // The word can stay in the text of the page, but not as a class or an identifier.
                assert!(
                    !page.contains(&format!(r#"class="{plain}""#)),
                    "{name}: {plain}"
                );
            }
            // Each class of the page has a rule in the mapped stylesheet.
            for class in &classes {
                assert!(
                    mapped_css.contains(class),
                    "{name}: {class} has no rule in the mapped stylesheet"
                );
            }
            // The page loads the mapped files, never the plain ones.
            assert!(page.contains(&mask.assets().css_path), "{name}");
            assert!(page.contains(&mask.assets().js_path), "{name}");
            assert!(!page.contains(&assets().css_path), "{name}");
            assert!(!page.contains(&assets().js_path), "{name}");
            for word in [
                "authShell",
                "themeSwitch",
                "button--primary",
                "appHeader",
                "tile__",
                "signin",
                "data-reveal",
                "visually-hidden",
                "reveal",
            ] {
                assert!(!page.contains(word), "{name}: {word}");
            }
        }
    }

    #[test]
    fn the_identifiers_and_their_references_agree() {
        let mask = mask();
        for (name, html) in pages() {
            let page = mask.page(&html);
            let (_, ids) = names_of(&page);
            for token in tokenize(&page) {
                let Tok::Open { attrs, .. } = token else {
                    continue;
                };
                for attr in attrs {
                    let Some((_, value)) = attr.value else {
                        continue;
                    };
                    let target = match attr.name.as_str() {
                        "for" | "aria-labelledby" | "aria-controls" | "aria-describedby" => {
                            Some(value.as_str())
                        }
                        "href" => value.strip_prefix('#'),
                        _ => None,
                    };
                    if let Some(target) = target {
                        assert!(ids.contains(target), "{name}: {} -> {target}", attr.name);
                    }
                }
            }
        }
        // The field names and the attributes of the password managers do not change.
        let page = mask.page(&login_page_html());
        for keep in [
            r#"name="login""#,
            r#"name="password""#,
            r#"name="website""#,
            r#"name="keep""#,
            r#"data-lpignore="true""#,
            r#"data-1p-ignore="true""#,
            r#"data-bwignore="true""#,
            r#"autocomplete="current-password""#,
            r#"autocomplete="username""#,
        ] {
            assert!(
                tokenize(&page)
                    .iter()
                    .any(|t| matches!(t, Tok::Open { attrs, .. }
                    if attrs.iter().any(|a| format!("{}=\"{}\"", a.name,
                        a.value.as_ref().map_or("", |(_, v)| v.as_str())) == keep))),
                "{keep}"
            );
        }
    }

    #[test]
    fn two_renders_of_a_page_differ_but_have_the_same_canonical_form() {
        let mask = mask();
        for (name, html) in pages() {
            let mut distinct = BTreeSet::new();
            let mut canonical = BTreeSet::new();
            for seed in 0..200 {
                let page = mask.page_with(&html, &mut Rng::seeded(seed));
                assert!(tree(&tokenize(&page)).is_some(), "{name} {seed}: {page}");
                canonical.insert(mask.canonical(&page));
                distinct.insert(page);
            }
            assert_eq!(canonical.len(), 1, "{name}");
            assert!(distinct.len() > 20, "{name}: {}", distinct.len());
        }
    }

    #[test]
    fn the_variation_inserts_at_most_three_elements_with_no_content() {
        let mask = mask();
        let inert: BTreeSet<String> = WRAPPER_CLASSES
            .iter()
            .chain(EMPTY_CLASSES.iter())
            .map(|class| mask.name(class))
            .collect();
        let mut counts = BTreeSet::new();
        let (mut wrappers, mut empties) = (0, 0);
        for (name, html) in pages() {
            for seed in 0..300 {
                let page = mask.page_with(&html, &mut Rng::seeded(seed));
                let tokens = tokenize(&page);
                let count = tokens
                    .iter()
                    .enumerate()
                    .filter(|(index, token)| {
                        let Tok::Open { attrs, .. } = token else {
                            return false;
                        };
                        let is_inert = attrs.iter().any(|a| {
                            a.name == "class"
                                && a.value.as_ref().is_some_and(|(_, v)| inert.contains(v))
                        });
                        if is_inert && tokens[index + 1] == Tok::Close("div".to_string())
                            && matches!(token, Tok::Open { attrs, .. } if attrs.iter().any(|a| a.name == "aria-hidden"))
                        {
                            empties += 1;
                        } else if is_inert {
                            wrappers += 1;
                        }
                        is_inert
                    })
                    .count();
                assert!(count <= 3, "{name} {seed}: {count}");
                counts.insert(count);
            }
        }
        assert_eq!(counts, BTreeSet::from([0, 1, 2, 3]));
        assert!(wrappers > 100 && empties > 100, "{wrappers} {empties}");
    }

    #[test]
    fn the_variation_keeps_what_a_browser_and_a_password_manager_read() {
        let mask = mask();
        for seed in 0..300 {
            let page = mask.page_with(&login_page_html(), &mut Rng::seeded(seed));
            let tokens = tokenize(&page);
            let find = |wanted: &str| {
                tokens.iter().position(|t| {
                    matches!(t, Tok::Open { name, attrs, .. }
                    if name == "input" && attrs.iter().any(|a| a.name == "name"
                        && a.value.as_ref().is_some_and(|(_, v)| v == wanted)))
                })
            };
            // The login comes before the password, and both are in the form, before the trap.
            let (login, password, trap) = (
                find("login").unwrap(),
                find("password").unwrap(),
                find("website").unwrap(),
            );
            assert!(login < password && password < trap, "{seed}");
            let form = tokens
                .iter()
                .position(|t| matches!(t, Tok::Open { name, .. } if name == "form"))
                .unwrap();
            let end = tree(&tokens).unwrap().end[form];
            assert!(form < login && trap < end, "{seed}");
            // The wrappers have no content of their own and no tab stop.
            for (index, token) in tokens.iter().enumerate() {
                if let Tok::Open { name, attrs, .. } = token {
                    let inert = attrs.iter().any(|a| {
                        a.name == "class"
                            && a.value.as_ref().is_some_and(|(_, v)| {
                                WRAPPER_CLASSES
                                    .iter()
                                    .chain(EMPTY_CLASSES.iter())
                                    .any(|c| mask.name(c) == *v)
                            })
                    });
                    if inert {
                        assert!(name == "div" || name == "span", "{name}");
                        // A `span` never holds a block, and a `div` is never in a phrase.
                        let next = open_name(&tokens[index + 1]);
                        assert!(name == "div" || !BLOCK_ELEMENTS.contains(&next), "{next}");
                        assert!(!attrs
                            .iter()
                            .any(|a| a.name == "tabindex" || a.name == "href"));
                    }
                }
            }
            // The head keeps the charset first, and the trap keeps its attributes.
            let head = tokens
                .iter()
                .position(|t| matches!(t, Tok::Open { name, .. } if name == "head"))
                .unwrap();
            assert!(is_charset_meta(&tokens[head + 1..]), "{seed}");
            for keep in ["aria-hidden", "inert"] {
                let hp = tokens.iter().find(|t| {
                    matches!(t, Tok::Open { attrs, .. }
                    if attrs.iter().any(|a| a.name == "class"
                        && a.value.as_ref().is_some_and(|(_, v)| *v == mask.name("hp"))))
                });
                let Some(Tok::Open { attrs, .. }) = hp else {
                    panic!("no trap")
                };
                assert!(attrs.iter().any(|a| a.name == keep), "{keep}");
            }
        }
    }

    #[test]
    fn the_masked_sign_in_page_keeps_its_hidden_button_its_heading_and_its_script_hook() {
        let mask = mask();
        for seed in 0..50 {
            let page = mask.page_with(&login_page_html(), &mut Rng::seeded(seed));
            let tokens = tokenize(&page);
            // The button that shows the password is hidden in the page. Its mapped data attribute is in the script.
            let hook = format!("data-{}", mask.name("data-reveal"));
            let button = tokens.iter().find(|t| {
                matches!(t, Tok::Open { name, attrs, .. }
                    if name == "button" && attrs.iter().any(|a| a.name == hook))
            });
            let Some(Tok::Open { attrs, .. }) = button else {
                panic!("no button with {hook}: {page}");
            };
            assert!(attrs
                .iter()
                .any(|a| a.name == "hidden" && a.value.is_none()));
            assert!(attrs.iter().any(|a| a.name == "aria-pressed"));
            assert!(mask.assets().js.contains(&format!("[{hook}]")));
            // One heading, with a mapped class and an identifier that the form names.
            assert_eq!(page.matches("<h1").count(), 1, "{page}");
            let heading = tokens
                .iter()
                .find_map(|t| match t {
                    Tok::Open { name, attrs, .. } if name == "h1" => Some(attrs.clone()),
                    _ => None,
                })
                .unwrap();
            let id = heading
                .iter()
                .find(|a| a.name == "id")
                .and_then(|a| a.value.as_ref().map(|(_, v)| v.clone()))
                .unwrap();
            assert_eq!(id, mask.name("signin-title"));
            assert!(tokens
                .iter()
                .any(|t| matches!(t, Tok::Open { name, attrs, .. }
                if name == "form" && attrs.iter().any(|a| a.name == "aria-labelledby"
                    && a.value.as_ref().is_some_and(|(_, v)| *v == id)))));
        }
    }

    #[test]
    fn a_page_that_is_not_well_formed_still_gets_the_names() {
        let mask = mask();
        let page = mask.page(r#"<div class="card"><p class="hint">x</div></p>"#);
        assert!(!page.contains(r#"class="card""#) && !page.contains(r#"class="hint""#));
        assert!(mask.page("<p>1 < 2</p>").contains("1 < 2"));
    }

    #[test]
    fn the_shipped_stylesheet_and_script_are_what_the_mask_reads() {
        // The mask maps the minified files. If the minifier failed, the mask would map the source.
        assert!(CSS.contains("/*") && !assets().css.contains("/*"));
        assert!(!assets().js.contains("//"));
    }
}
