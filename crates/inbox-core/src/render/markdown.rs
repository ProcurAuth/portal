//! A safe subset of Markdown, as a closed tree. The server (HTML) and the phone (spans) share this parser.
//!
//! The text is the explanation of an agent: it is not trusted. The parser keeps only what it can show with no risk,
//! and every other construct stays plain text, as the author wrote it. It never fails: over a limit, the whole text
//! becomes one plain block.
//!
//! Kept: paragraphs, headings of level 1 to 3, emphasis, strong text, inline code, fenced and indented code blocks,
//! block quotes, ordered and unordered lists, task items, line breaks and links. A link is never active: it shows its
//! text and the host in parentheses, and only for `http` and `https`. A link with another scheme shows its text only.
//!
//! Plain text: raw HTML, images, autolinks, tables, footnotes, thematic breaks and headings of level 4 to 6.
//!
//! The parser comes from the crate `pulldown-cmark` with no feature. This module reads its events and builds the
//! tree, so the tree holds nothing that the crate adds later.

use pulldown_cmark::{CodeBlockKind, Event, LinkType, Options, Parser, Tag, TagEnd};
use serde::Serialize;

use super::highlight::Language;

/// Maximum length of a Markdown text, in characters. It is the limit of an explanation.
pub const MAX_INPUT_CHARS: usize = 8000;
/// Maximum number of nodes (blocks, items and inline nodes). Over it, the text is plain.
pub const MAX_NODES: usize = 2000;
/// Maximum depth of quotes, list items, emphasis, strong text and links. Over it, the text is plain.
pub const MAX_DEPTH: usize = 4;
/// Maximum depth of lists. Over it, the text is plain.
pub const MAX_LIST_DEPTH: usize = 3;

/// A parsed text.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Doc {
    /// True when a limit made the whole text plain. The only block is then a `text` block.
    #[serde(skip_serializing_if = "is_false")]
    pub fallback: bool,
    pub blocks: Vec<Block>,
}

fn is_false(v: &bool) -> bool {
    !*v
}

/// A block. `text` is a block of plain text: the characters of the author, with their line breaks.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum Block {
    Paragraph {
        c: Vec<Inline>,
    },
    /// A heading of level 1 to 3. A screen shows it as a bold line.
    Heading {
        level: u8,
        c: Vec<Inline>,
    },
    /// A block of code. `lang` is the language of the info string, or `plain`. It only colors.
    Code {
        lang: Language,
        text: String,
    },
    Quote {
        c: Vec<Block>,
    },
    List {
        ordered: bool,
        /// The number of the first item of an ordered list. 1 for an unordered list.
        start: u64,
        items: Vec<Item>,
    },
    Text {
        s: String,
    },
}

/// An item of a list. `checked` is set for a task item: a read-only box, never a control.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Item {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub checked: Option<bool>,
    pub c: Vec<Block>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum Inline {
    Text {
        s: String,
    },
    /// Inline code.
    Code {
        s: String,
    },
    Em {
        c: Vec<Inline>,
    },
    Strong {
        c: Vec<Inline>,
    },
    /// A link to an `http` or `https` address. It is not active: show `c`, then the host in parentheses.
    Link {
        c: Vec<Inline>,
        host: String,
    },
    /// A hard line break.
    Br,
    /// A soft line break. A screen shows a space.
    Soft,
}

/// The tree of a text. Never fails.
pub fn parse(text: &str) -> Doc {
    if text.chars().count() > MAX_INPUT_CHARS {
        return fallback(text);
    }
    match build(text) {
        Some(blocks) => Doc { fallback: false, blocks },
        None => fallback(text),
    }
}

fn fallback(text: &str) -> Doc {
    let blocks =
        if text.is_empty() { Vec::new() } else { vec![Block::Text { s: text.to_string() }] };
    Doc { fallback: true, blocks }
}

enum Frame {
    Root {
        blocks: Vec<Block>,
        pending: Vec<Inline>,
    },
    Quote {
        blocks: Vec<Block>,
        pending: Vec<Inline>,
    },
    Item {
        blocks: Vec<Block>,
        pending: Vec<Inline>,
        checked: Option<bool>,
    },
    List {
        ordered: bool,
        start: u64,
        items: Vec<Item>,
    },
    Paragraph {
        inl: Vec<Inline>,
    },
    Heading {
        level: u8,
        inl: Vec<Inline>,
    },
    Em {
        inl: Vec<Inline>,
    },
    Strong {
        inl: Vec<Inline>,
    },
    Link {
        inl: Vec<Inline>,
        host: Option<String>,
    },
    Code {
        lang: Language,
        text: String,
    },
    /// A construct that stays plain text: its events are skipped, its source is the text.
    Raw {
        depth: usize,
        range: std::ops::Range<usize>,
        inline: bool,
    },
}

impl Frame {
    fn counts_as_depth(&self) -> bool {
        matches!(
            self,
            Frame::Quote { .. }
                | Frame::Item { .. }
                | Frame::Em { .. }
                | Frame::Strong { .. }
                | Frame::Link { .. }
        )
    }
}

struct Builder<'a> {
    src: &'a str,
    stack: Vec<Frame>,
    nodes: usize,
}

impl Builder<'_> {
    fn count(&mut self) -> Option<()> {
        self.nodes += 1;
        (self.nodes <= MAX_NODES).then_some(())
    }

    fn push(&mut self, frame: Frame) -> Option<()> {
        if frame.counts_as_depth()
            && self.stack.iter().filter(|f| f.counts_as_depth()).count() >= MAX_DEPTH
        {
            return None;
        }
        if matches!(frame, Frame::List { .. })
            && self.stack.iter().filter(|f| matches!(f, Frame::List { .. })).count()
                >= MAX_LIST_DEPTH
        {
            return None;
        }
        self.stack.push(frame);
        Some(())
    }

    /// The place where an inline node goes: the inline frame at the top, or the pending text of a container.
    fn inline_sink(&mut self) -> &mut Vec<Inline> {
        match self.stack.last_mut().expect("the root is never popped") {
            Frame::Paragraph { inl }
            | Frame::Heading { inl, .. }
            | Frame::Em { inl }
            | Frame::Strong { inl }
            | Frame::Link { inl, .. } => inl,
            Frame::Root { pending, .. }
            | Frame::Quote { pending, .. }
            | Frame::Item { pending, .. } => pending,
            // These frames hold no inline node: the events of a raw construct are skipped before this call, and a
            // list or a code block has its own events.
            Frame::List { .. } | Frame::Code { .. } | Frame::Raw { .. } => {
                unreachable!("an inline node has no place in this frame")
            }
        }
    }

    fn add_inline(&mut self, node: Inline) -> Option<()> {
        // A list holds items only. Text outside an item cannot happen in a parsed document, but the tree must
        // stay valid if it does: the text goes to the level above.
        if matches!(self.stack.last(), Some(Frame::List { .. } | Frame::Code { .. })) {
            return Some(());
        }
        if let Inline::Text { s } = &node
            && let Some(Inline::Text { s: last }) = self.inline_sink().last_mut()
        {
            last.push_str(s);
            return Some(());
        }
        self.count()?;
        self.inline_sink().push(node);
        Some(())
    }

    /// Turns the pending inline nodes of the container at the top into a paragraph (the text of a tight list item).
    fn flush(&mut self) -> Option<()> {
        let pending = match self.stack.last_mut()? {
            Frame::Root { pending, .. }
            | Frame::Quote { pending, .. }
            | Frame::Item { pending, .. } => std::mem::take(pending),
            _ => return Some(()),
        };
        if !pending.is_empty() {
            self.count()?;
            self.add_block(Block::Paragraph { c: pending });
        }
        Some(())
    }

    /// Closes the pending inline nodes of a container that ends: they become its last paragraph.
    fn close_pending(&mut self, blocks: &mut Vec<Block>, pending: Vec<Inline>) -> Option<()> {
        if !pending.is_empty() {
            self.count()?;
            blocks.push(Block::Paragraph { c: pending });
        }
        Some(())
    }

    fn add_block(&mut self, block: Block) {
        match self.stack.last_mut().expect("the root is never popped") {
            Frame::Root { blocks, .. }
            | Frame::Quote { blocks, .. }
            | Frame::Item { blocks, .. } => blocks.push(block),
            // A block inside an inline frame cannot happen in a parsed document.
            _ => {}
        }
    }

    fn raw_text(&self, range: &std::ops::Range<usize>) -> String {
        self.src.get(range.clone()).unwrap_or("").trim_end_matches(['\n', '\r']).to_string()
    }

    fn start(&mut self, tag: Tag<'_>, range: std::ops::Range<usize>) -> Option<()> {
        // Inside a raw construct nothing starts.
        if let Some(Frame::Raw { depth, .. }) = self.stack.last_mut() {
            *depth += 1;
            return Some(());
        }
        match tag {
            Tag::Paragraph => {
                self.flush()?;
                self.push(Frame::Paragraph { inl: Vec::new() })
            }
            Tag::Heading { level, .. } if (level as usize) <= 3 => {
                self.flush()?;
                self.push(Frame::Heading { level: level as u8, inl: Vec::new() })
            }
            Tag::BlockQuote(_) => {
                self.flush()?;
                self.push(Frame::Quote { blocks: Vec::new(), pending: Vec::new() })
            }
            Tag::CodeBlock(kind) => {
                self.flush()?;
                let lang = match kind {
                    CodeBlockKind::Indented => Language::Plain,
                    CodeBlockKind::Fenced(info) => Language::from_info(&info),
                };
                self.push(Frame::Code { lang, text: String::new() })
            }
            Tag::List(start) => {
                self.flush()?;
                self.push(Frame::List {
                    ordered: start.is_some(),
                    start: start.unwrap_or(1),
                    items: Vec::new(),
                })
            }
            Tag::Item => {
                self.push(Frame::Item { blocks: Vec::new(), pending: Vec::new(), checked: None })
            }
            Tag::Emphasis => self.push(Frame::Em { inl: Vec::new() }),
            Tag::Strong => self.push(Frame::Strong { inl: Vec::new() }),
            Tag::Link { link_type, dest_url, .. } => {
                let host = match link_type {
                    LinkType::Inline
                    | LinkType::Reference
                    | LinkType::Collapsed
                    | LinkType::Shortcut => link_host(&dest_url),
                    // An autolink, an address of e-mail or a link that nothing defines is plain text.
                    _ => None,
                };
                self.push(Frame::Link { inl: Vec::new(), host })
            }
            // An image, a table, a footnote, a heading of level 4 to 6, raw HTML and every tag that the parser could
            // add later: the source stays as text.
            other => {
                let inline = matches!(other, Tag::Image { .. });
                if !inline {
                    self.flush()?;
                }
                self.stack.push(Frame::Raw { depth: 0, range, inline });
                Some(())
            }
        }
    }

    fn end(&mut self, end: TagEnd) -> Option<()> {
        if let Some(Frame::Raw { depth, .. }) = self.stack.last_mut() {
            if *depth > 0 {
                *depth -= 1;
                return Some(());
            }
            let Some(Frame::Raw { range, inline, .. }) = self.stack.pop() else { unreachable!() };
            let text = self.raw_text(&range);
            if inline {
                return self.add_inline(Inline::Text { s: text });
            }
            if !text.is_empty() {
                self.count()?;
                self.add_block(Block::Text { s: text });
            }
            return Some(());
        }
        let _ = end;
        match self.stack.pop()? {
            Frame::Paragraph { inl } => {
                self.count()?;
                self.add_block(Block::Paragraph { c: inl });
            }
            Frame::Heading { level, inl } => {
                self.count()?;
                self.add_block(Block::Heading { level, c: inl });
            }
            Frame::Quote { mut blocks, pending } => {
                self.close_pending(&mut blocks, pending)?;
                self.count()?;
                self.add_block(Block::Quote { c: blocks });
            }
            Frame::Item { mut blocks, pending, checked } => {
                self.close_pending(&mut blocks, pending)?;
                self.count()?;
                if let Some(Frame::List { items, .. }) = self.stack.last_mut() {
                    items.push(Item { checked, c: blocks });
                }
            }
            Frame::List { ordered, start, items } => {
                self.count()?;
                self.add_block(Block::List { ordered, start, items });
            }
            Frame::Em { inl } => self.add_inline(Inline::Em { c: inl })?,
            Frame::Strong { inl } => self.add_inline(Inline::Strong { c: inl })?,
            Frame::Link { inl, host } => match host {
                Some(host) => self.add_inline(Inline::Link { c: inl, host })?,
                // Plain: the text of the link goes to the level above, with no link.
                None => {
                    for node in inl {
                        self.add_inline(node)?;
                    }
                }
            },
            Frame::Code { lang, mut text } => {
                if text.ends_with('\n') {
                    text.pop();
                }
                self.count()?;
                self.add_block(Block::Code { lang, text });
            }
            Frame::Root { .. } | Frame::Raw { .. } => return None,
        }
        Some(())
    }

    fn event(&mut self, event: Event<'_>, range: std::ops::Range<usize>) -> Option<()> {
        if let Some(Frame::Raw { .. }) = self.stack.last() {
            return match event {
                Event::Start(t) => self.start(t, range),
                Event::End(e) => self.end(e),
                _ => Some(()),
            };
        }
        match event {
            Event::Start(tag) => self.start(tag, range),
            Event::End(end) => self.end(end),
            Event::Text(s) => {
                if let Some(Frame::Code { text, .. }) = self.stack.last_mut() {
                    text.push_str(&s);
                    Some(())
                } else {
                    self.add_inline(Inline::Text { s: s.into_string() })
                }
            }
            Event::Code(s) => self.add_inline(Inline::Code { s: s.into_string() }),
            Event::Html(s) | Event::InlineHtml(s) => {
                if let Some(Frame::Code { text, .. }) = self.stack.last_mut() {
                    text.push_str(&s);
                    Some(())
                } else {
                    self.add_inline(Inline::Text { s: s.into_string() })
                }
            }
            Event::InlineMath(s) | Event::DisplayMath(s) => {
                self.add_inline(Inline::Text { s: s.into_string() })
            }
            Event::FootnoteReference(s) => self.add_inline(Inline::Text { s: format!("[^{s}]") }),
            Event::SoftBreak => self.add_inline(Inline::Soft),
            Event::HardBreak => self.add_inline(Inline::Br),
            Event::Rule => {
                self.flush()?;
                let text = self.raw_text(&range);
                if !text.is_empty() {
                    self.count()?;
                    self.add_block(Block::Text { s: text });
                }
                Some(())
            }
            Event::TaskListMarker(done) => {
                if let Some(Frame::Item { checked, .. }) = self.stack.last_mut() {
                    *checked = Some(done);
                }
                Some(())
            }
        }
    }
}

fn build(text: &str) -> Option<Vec<Block>> {
    let mut b = Builder {
        src: text,
        stack: vec![Frame::Root { blocks: Vec::new(), pending: Vec::new() }],
        nodes: 0,
    };
    for (event, range) in Parser::new_ext(text, Options::ENABLE_TASKLISTS).into_offset_iter() {
        b.event(event, range)?;
    }
    b.flush()?;
    if b.stack.len() != 1 {
        return None;
    }
    match b.stack.pop() {
        Some(Frame::Root { blocks, .. }) => Some(blocks),
        _ => None,
    }
}

/// The host of an address that a user can read, or `None` for a link that must not show an address.
///
/// Only `http` and `https` count, in any case. The host is the part after the user information (`a@host`), so that
/// `https://good.example@bad.example/` shows `bad.example`. A host with a character other than a letter, a digit, `.`,
/// `-` or a port gives `None`: an internationalized name is not shown, as the reader cannot tell it from a copy.
pub fn link_host(url: &str) -> Option<String> {
    let url = url.trim();
    let lower = url.get(..8).unwrap_or(url).to_ascii_lowercase();
    let rest = if lower.starts_with("https://") {
        &url[8..]
    } else if lower.starts_with("http://") {
        &url[7..]
    } else {
        return None;
    };
    let authority = rest.split(['/', '?', '#', '\\']).next().unwrap_or("");
    let host = authority.rsplit('@').next().unwrap_or("");
    let (name, port) = match host.rsplit_once(':') {
        Some((n, p)) if !host.starts_with('[') => (n, Some(p)),
        _ => (host, None),
    };
    if let Some(p) = port
        && (p.is_empty() || p.len() > 5 || !p.bytes().all(|b| b.is_ascii_digit()))
    {
        return None;
    }
    let ok = !name.is_empty()
        && name.len() <= 253
        && !name.starts_with(['.', '-'])
        && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'-');
    if !ok {
        return None;
    }
    let mut out = name.to_ascii_lowercase();
    if let Some(p) = port {
        out.push(':');
        out.push_str(p);
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(text: &str) -> Vec<Block> {
        let d = parse(text);
        assert!(!d.fallback, "{text:?}");
        d.blocks
    }

    fn plain_inline(s: &str) -> Inline {
        Inline::Text { s: s.into() }
    }

    #[test]
    fn paragraphs_emphasis_and_code() {
        let b = p("Hello *world*, **bold** and `code`.\n\nSecond.");
        assert_eq!(b.len(), 2);
        assert_eq!(
            b[0],
            Block::Paragraph {
                c: vec![
                    plain_inline("Hello "),
                    Inline::Em { c: vec![plain_inline("world")] },
                    plain_inline(", "),
                    Inline::Strong { c: vec![plain_inline("bold")] },
                    plain_inline(" and "),
                    Inline::Code { s: "code".into() },
                    plain_inline("."),
                ]
            }
        );
    }

    #[test]
    fn headings_one_to_three_and_the_rest_as_text() {
        let b = p("# One\n## Two\n### Three\n#### Four\n");
        assert!(matches!(&b[0], Block::Heading { level: 1, .. }));
        assert!(matches!(&b[1], Block::Heading { level: 2, .. }));
        assert!(matches!(&b[2], Block::Heading { level: 3, .. }));
        assert_eq!(b[3], Block::Text { s: "#### Four".into() });
        let b = p("Title\n=====\n");
        assert!(matches!(&b[0], Block::Heading { level: 1, .. }));
    }

    #[test]
    fn fenced_and_indented_code() {
        let b =
            p("```rust\nfn main() {}\n```\n\n    indented\n\n```\nno lang\n```\n```cobol\nx\n```");
        assert_eq!(b[0], Block::Code { lang: Language::Rust, text: "fn main() {}".into() });
        assert_eq!(b[1], Block::Code { lang: Language::Plain, text: "indented".into() });
        assert_eq!(b[2], Block::Code { lang: Language::Plain, text: "no lang".into() });
        assert_eq!(b[3], Block::Code { lang: Language::Plain, text: "x".into() });
        let b = p("```SQL\nselect 1\n```");
        assert!(matches!(&b[0], Block::Code { lang: Language::Sql, .. }));
    }

    #[test]
    fn lists_quotes_and_tasks() {
        let b = p("- a\n- b\n  - c\n\n3. x\n4. y\n\n> quoted\n> text\n\n- [ ] todo\n- [x] done\n");
        let Block::List { ordered: false, items, .. } = &b[0] else { panic!("{b:?}") };
        assert_eq!(items.len(), 2);
        assert!(matches!(&items[1].c[1], Block::List { ordered: false, .. }));
        let Block::List { ordered: true, start: 3, items, .. } = &b[1] else { panic!("{b:?}") };
        assert_eq!(items.len(), 2);
        assert!(matches!(&b[2], Block::Quote { .. }));
        let Block::List { items, .. } = &b[3] else { panic!("{b:?}") };
        assert_eq!(items[0].checked, Some(false));
        assert_eq!(items[1].checked, Some(true));
    }

    #[test]
    fn breaks() {
        let b = p("one  \ntwo\nthree");
        let Block::Paragraph { c } = &b[0] else { panic!() };
        assert!(c.contains(&Inline::Br));
        assert!(c.contains(&Inline::Soft));
    }

    #[test]
    fn links_show_the_host_and_never_act() {
        let b = p("[docs](https://Example.com/a?b=1#c)");
        let Block::Paragraph { c } = &b[0] else { panic!() };
        assert_eq!(
            c[0],
            Inline::Link { c: vec![plain_inline("docs")], host: "example.com".into() }
        );
        // The user information is not the host.
        assert_eq!(
            link_host("https://good.example@evil.example/x").as_deref(),
            Some("evil.example")
        );
        assert_eq!(link_host("HTTP://host:8080/").as_deref(), Some("host:8080"));
        assert_eq!(link_host("https://a.b.c").as_deref(), Some("a.b.c"));
        // Everything else has no host.
        for url in [
            "javascript:alert(1)",
            "data:text/html;base64,AAAA",
            "ftp://x.example/",
            "mailto:a@b.c",
            "//x.example/",
            "/relative",
            "https://",
            "https://exämple.com/",
            "https://a b.com/",
            "https://x.example:abc/",
            "https://x.example:/",
            "https://-bad.example/",
            "https://[::1]/",
            "https:///path",
        ] {
            assert_eq!(link_host(url), None, "{url}");
        }
        let b = p("[click](javascript:alert(1)) and [x](data:text/html,1)");
        assert_eq!(
            b[0],
            Block::Paragraph {
                c: vec![plain_inline("click"), plain_inline(" and "), plain_inline("x")]
            }
            .normalize()
        );
    }

    #[test]
    fn autolinks_html_and_images_are_text() {
        let b = p("<https://evil.example/x> and <a@b.c>");
        let Block::Paragraph { c } = &b[0] else { panic!() };
        assert!(c.iter().all(|n| matches!(n, Inline::Text { .. })), "{c:?}");
        let b = p("<script>alert(1)</script>\n\nx");
        assert_eq!(b[0], Block::Text { s: "<script>alert(1)</script>".into() });
        let b = p("a <b onclick=\"x\">y</b> ![alt](https://evil.example/i.png)");
        let Block::Paragraph { c } = &b[0] else { panic!() };
        assert!(c.iter().all(|n| matches!(n, Inline::Text { .. })), "{c:?}");
        let Inline::Text { s } = &c[0] else { panic!() };
        assert!(s.contains("<b onclick=\"x\">"), "{s}");
        assert!(s.contains("![alt](https://evil.example/i.png)"), "{s}");
    }

    #[test]
    fn tables_footnotes_and_rules_are_text() {
        let b = p("| a | b |\n|---|---|\n| 1 | 2 |\n\nText[^1]\n\n[^1]: note\n\n---\n");
        let all = format!("{b:?}");
        assert!(all.contains("| a | b |"), "{all}");
        assert!(all.contains("---"), "{all}");
        assert!(!all.contains("Table"), "{all}");
    }

    #[test]
    fn entities_are_text() {
        let b = p("&lt;script&gt; &amp; &copy; &#35;");
        let Block::Paragraph { c } = &b[0] else { panic!() };
        // The parser reads an entity as the character. A renderer escapes the character.
        assert_eq!(c, &vec![plain_inline("<script> & \u{a9} #")]);
    }

    #[test]
    fn limits_make_the_text_plain() {
        // Too long.
        let long = "x".repeat(MAX_INPUT_CHARS + 1);
        let d = parse(&long);
        assert!(d.fallback);
        assert_eq!(d.blocks, vec![Block::Text { s: long }]);
        // Exactly the limit is parsed.
        assert!(!parse(&"x".repeat(MAX_INPUT_CHARS)).fallback);
        // Too deep: quotes.
        let deep = format!("{}x", "> ".repeat(MAX_DEPTH + 1));
        let d = parse(&deep);
        assert!(d.fallback, "{d:?}");
        assert!(!parse(&format!("{}x", "> ".repeat(MAX_DEPTH))).fallback);
        // Too deep: lists.
        let lists = "- a\n  - b\n    - c\n      - d\n";
        assert!(parse(lists).fallback);
        assert!(!parse("- a\n  - b\n    - c\n").fallback);
        // Too many nodes.
        let many = "*a* ".repeat(MAX_NODES);
        assert!(parse(&many).fallback);
        assert!(parse(&"- a\n".repeat(MAX_NODES)).fallback);
        // A bomb of emphasis marks stays inside the limits and does not panic.
        let bomb = "*".repeat(7000) + &"_".repeat(900);
        let _ = parse(&bomb);
        let bomb = "[".repeat(4000) + "x" + &"]".repeat(3000);
        let _ = parse(&bomb);
        let bomb = "<".repeat(7999);
        let _ = parse(&bomb);
    }

    #[test]
    fn empty_and_blank() {
        assert!(parse("").blocks.is_empty());
        assert!(parse("\n\n  \n").blocks.is_empty());
    }

    #[test]
    fn bidi_and_zero_width_characters_stay() {
        let b = p("a\u{202e}b \u{200b}c `x\u{2066}y`");
        let Block::Paragraph { c } = &b[0] else { panic!() };
        let joined = format!("{c:?}");
        assert!(joined.contains("202e") || joined.contains('\u{202e}'), "{joined}");
        let text: String = c
            .iter()
            .map(|n| match n {
                Inline::Text { s } | Inline::Code { s } => s.as_str(),
                _ => "",
            })
            .collect();
        assert!(
            text.contains('\u{202e}') && text.contains('\u{200b}') && text.contains('\u{2066}')
        );
    }

    #[test]
    fn json_shape_is_stable() {
        let d = parse("# T\n\n- [x] a\n\n```sql\nselect 1\n```\n[l](https://h.example/)");
        let json = serde_json::to_string(&d).unwrap();
        assert_eq!(
            json,
            r#"{"blocks":[{"t":"heading","level":1,"c":[{"t":"text","s":"T"}]},{"t":"list","ordered":false,"start":1,"items":[{"checked":true,"c":[{"t":"paragraph","c":[{"t":"text","s":"a"}]}]}]},{"t":"code","lang":"sql","text":"select 1"},{"t":"paragraph","c":[{"t":"link","c":[{"t":"text","s":"l"}],"host":"h.example"}]}]}"#
        );
        let f = serde_json::to_string(&parse(&"x".repeat(9000))).unwrap();
        assert!(f.starts_with(r#"{"fallback":true,"blocks":[{"t":"text""#), "{}", &f[..60]);
    }

    impl Block {
        /// Joins the text nodes that follow each other, as the parser does.
        fn normalize(self) -> Block {
            match self {
                Block::Paragraph { c } => {
                    let mut out: Vec<Inline> = Vec::new();
                    for n in c {
                        match (out.last_mut(), n) {
                            (Some(Inline::Text { s: a }), Inline::Text { s: b }) => a.push_str(&b),
                            (_, n) => out.push(n),
                        }
                    }
                    Block::Paragraph { c: out }
                }
                other => other,
            }
        }
    }
}
