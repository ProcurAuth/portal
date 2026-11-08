//! HTML for the text of an agent: the safe subset of Markdown and the color of code.
//!
//! The phone and the server read the same tree (`inbox_core::render::markdown`) and the same tokens
//! (`inbox_core::render::highlight`). This module writes the HTML of the server. The web pages of the items call it.
//!
//! The rules of the output:
//!
//! - Every text is escaped. A character that can hide the order or the presence of text (a bidi control, a zero-width
//!   character, a control character) is not written: a visible mark `U+XXXX` takes its place.
//! - The tags are `div`, `p`, `span`, `strong`, `em`, `code`, `pre`, `ul`, `ol`, `li`, `blockquote` and `br`. The
//!   attributes are `class` and, on `ol`, `start` (a number). There is no `style`, no script, no event attribute and
//!   no `href`. The classes start with `md-` or `tok-` and come from fixed lists, never from the text.
//! - A link is not active. It shows its text, then the host in parentheses. See `inbox_core::render::markdown`.

use std::fmt::Write as _;

use inbox_core::proto::ExplanationFormat;
use inbox_core::render::highlight::{self, Class, Language};
use inbox_core::render::markdown::{self, Block, Doc, Inline, Item};

/// The text of an explanation, as HTML. `plain` keeps the line breaks and shows the characters as they are.
pub fn explanation(text: &str, format: ExplanationFormat) -> String {
    match format {
        ExplanationFormat::Plain => {
            format!(
                "<div class=\"md\"><p class=\"md-raw\">{}</p></div>",
                escape(text)
            )
        }
        ExplanationFormat::Markdown => document(&markdown::parse(text)),
    }
}

/// A parsed text, as HTML.
pub fn document(doc: &Doc) -> String {
    let mut out = String::from("<div class=\"md\">");
    blocks(&mut out, &doc.blocks);
    out.push_str("</div>");
    out
}

/// A command with the color of its language, one `md-ln` span for each line. The text is the same characters: only the
/// spans differ, and the text keeps its line breaks. A hidden character has a red mark. A letter of a script that looks
/// like ASCII (Cyrillic, Greek, full-width Latin) has an amber mark with its code.
pub fn command(text: &str, lang: Language) -> String {
    let mut lines: Vec<String> = vec![String::new()];
    for t in highlight::tokens(lang, text) {
        let piece = &text[t.start..t.end];
        for (n, part) in piece.split('\n').enumerate() {
            if n > 0 {
                lines.push(String::new());
            }
            let html = escape_marked(part, true);
            let line = lines.last_mut().expect("a line exists");
            if t.class == Class::Plain || part.is_empty() {
                line.push_str(&html);
            } else {
                let _ = write!(
                    line,
                    "<span class=\"tok-{}\">{html}</span>",
                    t.class.as_str()
                );
            }
        }
    }
    // The line break at the end of the text ends the last line: it does not start another one.
    let ends = text.ends_with('\n');
    if text.is_empty() || ends {
        lines.pop();
    }
    let mut out = format!(
        "<pre class=\"md-code md-cmd md-lang-{}\"><code>",
        lang.as_str()
    );
    for (n, line) in lines.iter().enumerate() {
        if n > 0 {
            out.push('\n');
        }
        let _ = write!(out, "<span class=\"md-ln\">{line}</span>");
    }
    if ends {
        out.push('\n');
    }
    out.push_str("</code></pre>");
    out
}

/// What the marks of a text count.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Marks {
    /// The hidden characters (bidi, zero-width, control): the code points, each one once, in order.
    pub hidden: Vec<u32>,
    /// The look-alike letters: the code points, each one once, in order.
    pub lookalike: Vec<u32>,
    /// All the characters that are not ASCII.
    pub non_ascii: usize,
    /// The characters that carry a mark, with repeats.
    pub marked: usize,
}

/// Counts the marks that the text shows: see [`escape`] and [`command`].
pub fn marks(text: &str) -> Marks {
    let mut m = Marks::default();
    for c in text.chars() {
        if !c.is_ascii() {
            m.non_ascii += 1;
        }
        let list = if is_misleading(c) {
            &mut m.hidden
        } else if is_lookalike(c) {
            &mut m.lookalike
        } else {
            continue;
        };
        m.marked += 1;
        if !list.contains(&(c as u32)) {
            list.push(c as u32);
        }
    }
    m
}

/// A letter from a script that a reader takes for ASCII. A French or a German letter is not here.
fn is_lookalike(c: char) -> bool {
    matches!(c,
        '\u{0370}'..='\u{03FF}' | '\u{0400}'..='\u{052F}' | '\u{0530}'..='\u{058F}'
        | '\u{FF01}'..='\u{FF5E}' | '\u{1D400}'..='\u{1D7FF}' | '\u{2100}'..='\u{214F}')
}

/// A text as HTML: escaped, with a visible mark for each character that can mislead.
pub fn escape(text: &str) -> String {
    escape_marked(text, false)
}

/// The same. With `lookalike`, a letter of a script that looks like ASCII has a mark with its code.
fn escape_marked(text: &str, lookalike: bool) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            c if is_misleading(c) => {
                let _ = write!(out, "<span class=\"md-ctl\">U+{:04X}</span>", c as u32);
            }
            c if lookalike && is_lookalike(c) => {
                let _ = write!(
                    out,
                    "<span class=\"md-ctl md-na\">{c} U+{:04X}</span>",
                    c as u32
                );
            }
            c => out.push(c),
        }
    }
    out
}

/// A character that a reader cannot see or that changes the order of the text. A line break, a tab and a carriage
/// return are text.
fn is_misleading(c: char) -> bool {
    if matches!(c, '\n' | '\t' | '\r') {
        return false;
    }
    c.is_control()
        || matches!(
            c,
            '\u{202A}'..='\u{202E}'
                | '\u{2066}'..='\u{2069}'
                | '\u{200B}'..='\u{200F}'
                | '\u{2028}' | '\u{2029}'
                | '\u{2060}'
                | '\u{061C}'
                | '\u{00AD}'
                | '\u{FEFF}'
        )
}

fn blocks(out: &mut String, list: &[Block]) {
    for b in list {
        block(out, b);
    }
}

fn block(out: &mut String, b: &Block) {
    match b {
        Block::Paragraph { c } => {
            out.push_str("<p class=\"md-p\">");
            inlines(out, c);
            out.push_str("</p>");
        }
        Block::Heading { level, c } => {
            // A heading is a bold line. The level only sets the size, from a fixed list.
            let level = (*level).clamp(1, 3);
            let _ = write!(out, "<p class=\"md-h md-h{level}\"><strong>");
            inlines(out, c);
            out.push_str("</strong></p>");
        }
        Block::Code { lang, text } => code_block(out, *lang, text),
        Block::Quote { c } => {
            out.push_str("<blockquote class=\"md-quote\">");
            blocks(out, c);
            out.push_str("</blockquote>");
        }
        Block::List {
            ordered,
            start,
            items,
        } => {
            if *ordered {
                if *start == 1 {
                    out.push_str("<ol class=\"md-list\">");
                } else {
                    let _ = write!(out, "<ol class=\"md-list\" start=\"{start}\">");
                }
            } else {
                out.push_str("<ul class=\"md-list\">");
            }
            for item in items {
                list_item(out, item);
            }
            out.push_str(if *ordered { "</ol>" } else { "</ul>" });
        }
        Block::Text { s } => {
            out.push_str("<p class=\"md-raw\">");
            out.push_str(&escape(s));
            out.push_str("</p>");
        }
    }
}

fn list_item(out: &mut String, item: &Item) {
    match item.checked {
        None => out.push_str("<li class=\"md-item\">"),
        Some(done) => {
            out.push_str("<li class=\"md-item md-task\">");
            // A read-only mark: the text carries the state, so it works without style and for a screen reader.
            out.push_str(if done {
                "<span class=\"md-check md-check-on\">[x] </span>"
            } else {
                "<span class=\"md-check\">[ ] </span>"
            });
        }
    }
    blocks(out, &item.c);
    out.push_str("</li>");
}

fn inlines(out: &mut String, list: &[Inline]) {
    for n in list {
        match n {
            Inline::Text { s } => out.push_str(&escape(s)),
            Inline::Code { s } => {
                out.push_str("<code class=\"md-icode\">");
                out.push_str(&escape(s));
                out.push_str("</code>");
            }
            Inline::Em { c } => {
                out.push_str("<em>");
                inlines(out, c);
                out.push_str("</em>");
            }
            Inline::Strong { c } => {
                out.push_str("<strong>");
                inlines(out, c);
                out.push_str("</strong>");
            }
            Inline::Link { c, host } => {
                out.push_str("<span class=\"md-link\">");
                inlines(out, c);
                out.push_str("</span> <span class=\"md-host\">(");
                out.push_str(&escape(host));
                out.push_str(")</span>");
            }
            Inline::Br => out.push_str("<br>"),
            Inline::Soft => out.push(' '),
        }
    }
}

fn code_block(out: &mut String, lang: Language, text: &str) {
    let _ = write!(
        out,
        "<pre class=\"md-code md-lang-{}\"><code>",
        lang.as_str()
    );
    for t in highlight::tokens(lang, text) {
        let piece = &text[t.start..t.end];
        if t.class == Class::Plain {
            out.push_str(&escape(piece));
        } else {
            let _ = write!(out, "<span class=\"tok-{}\">", t.class.as_str());
            out.push_str(&escape(piece));
            out.push_str("</span>");
        }
    }
    out.push_str("</code></pre>");
}

#[cfg(test)]
mod tests {
    use super::*;

    const TAGS: &[&str] = &[
        "div",
        "p",
        "span",
        "strong",
        "em",
        "code",
        "pre",
        "ul",
        "ol",
        "li",
        "blockquote",
        "br",
    ];

    /// A small HTML reader for the output of this module. It fails on anything that is not the closed subset.
    fn check_html(html: &str) {
        let b = html.as_bytes();
        let mut i = 0;
        let mut stack: Vec<String> = Vec::new();
        while i < b.len() {
            match b[i] {
                b'<' => {
                    let end = html[i..]
                        .find('>')
                        .unwrap_or_else(|| panic!("an open tag in {html}"))
                        + i;
                    let inner = &html[i + 1..end];
                    assert!(!inner.is_empty(), "{html}");
                    if let Some(name) = inner.strip_prefix('/') {
                        assert!(TAGS.contains(&name), "closing tag {name:?}");
                        assert_eq!(stack.pop().as_deref(), Some(name), "unbalanced in {html}");
                    } else {
                        let mut parts = inner.splitn(2, ' ');
                        let name = parts.next().unwrap();
                        assert!(TAGS.contains(&name), "tag {name:?} in {html}");
                        if let Some(attrs) = parts.next() {
                            check_attributes(name, attrs);
                        }
                        if name != "br" {
                            stack.push(name.to_string());
                        }
                    }
                    i = end + 1;
                }
                b'>' => panic!("a raw > in the text of {html}"),
                _ => i += 1,
            }
        }
        assert!(stack.is_empty(), "open tags {stack:?} in {html}");
        // No character that hides or reorders text reaches the output.
        assert!(
            !html.chars().any(is_misleading),
            "a misleading character in the output"
        );
    }

    fn check_attributes(tag: &str, attrs: &str) {
        // `name="value"` pairs, one space between them. A value never holds a quote: the writer escapes it.
        let mut rest = attrs;
        while !rest.is_empty() {
            let (name, after) = rest
                .split_once("=\"")
                .unwrap_or_else(|| panic!("attribute in {attrs}"));
            let (value, after) = after
                .split_once('"')
                .unwrap_or_else(|| panic!("value in {attrs}"));
            match name {
                "class" => {
                    for class in value.split(' ') {
                        assert!(
                            (class == "md"
                                || class.starts_with("md-")
                                || class.starts_with("tok-"))
                                && class.bytes().all(|c| c.is_ascii_lowercase()
                                    || c.is_ascii_digit()
                                    || c == b'-'),
                            "class {class:?}"
                        );
                    }
                }
                "start" => {
                    assert_eq!(tag, "ol");
                    assert!(value.bytes().all(|c| c.is_ascii_digit()) && !value.is_empty());
                }
                other => panic!("attribute {other:?} on {tag}"),
            }
            rest = after.strip_prefix(' ').unwrap_or(after);
        }
    }

    /// The text that a reader sees: tags removed, entities decoded.
    fn visible(html: &str) -> String {
        let mut out = String::new();
        let mut in_tag = false;
        for c in html.chars() {
            match c {
                '<' => in_tag = true,
                '>' => in_tag = false,
                c if !in_tag => out.push(c),
                _ => {}
            }
        }
        out.replace("&lt;", "<")
            .replace("&gt;", ">")
            .replace("&quot;", "\"")
            .replace("&#39;", "'")
            .replace("&amp;", "&")
    }

    fn md(text: &str) -> String {
        let html = explanation(text, ExplanationFormat::Markdown);
        check_html(&html);
        html
    }

    #[test]
    fn the_subset_renders_with_fixed_classes() {
        let html = md("# T\n\nHello *a* **b** `c`\n\n- [x] done\n- [ ] todo\n\n3. n\n\n> q\n\n```rust\nfn x() {}\n```\n\n[l](https://h.example/p)");
        for piece in [
            "<p class=\"md-h md-h1\"><strong>T</strong></p>",
            "<em>a</em>",
            "<strong>b</strong>",
            "<code class=\"md-icode\">c</code>",
            "<li class=\"md-item md-task\"><span class=\"md-check md-check-on\">[x] </span>",
            "<span class=\"md-check\">[ ] </span>",
            "<ol class=\"md-list\" start=\"3\">",
            "<blockquote class=\"md-quote\">",
            "<pre class=\"md-code md-lang-rust\"><code><span class=\"tok-keyword\">fn</span> ",
            "<span class=\"tok-function\">x</span>",
            "<span class=\"md-link\">l</span> <span class=\"md-host\">(h.example)</span>",
        ] {
            assert!(html.contains(piece), "{piece} not in {html}");
        }
        assert!(!html.contains("href"));
    }

    #[test]
    fn the_xss_corpus_gives_only_allowed_tags_and_shows_the_source_as_text() {
        let corpus: &[&str] = &[
            "<script>alert(1)</script>",
            "text <script>alert(1)</script> text",
            "<img src=x onerror=alert(1)>",
            "<img src=\"x\" onerror=\"alert(1)\">",
            "![alt](https://evil.example/x.png)",
            "![alt](javascript:alert(1))",
            "[x](javascript:alert(1))",
            "[x](JaVaScRiPt:alert(1))",
            "[x](  javascript:alert(1))",
            "[x](&#106;avascript:alert(1))",
            "[x](java\tscript:alert(1))",
            "[x](data:text/html;base64,PHNjcmlwdD5hbGVydCgxKTwvc2NyaXB0Pg==)",
            "[x](vbscript:msgbox(1))",
            "[x](file:///etc/passwd)",
            "[x](https://good.example@evil.example/)",
            "[x](https://exämple.com/)",
            "[x](https://x.example \"t\\\" onclick=alert(1) x=\\\"\")",
            "[a][b]\n\n[b]: javascript:alert(1)",
            "[<script>alert(1)</script>](https://x.example)",
            "[\"><svg onload=alert(1)>](https://x.example)",
            "<a href=\"javascript:alert(1)\">x</a>",
            "<iframe src=\"https://evil.example\"></iframe>",
            "<svg onload=alert(1)>",
            "<svg><script>alert(1)</script></svg>",
            "<style>body{background:url(javascript:alert(1))}</style>",
            "<!-- comment --><script>alert(1)</script>",
            "<![CDATA[x]]>",
            "<?php echo 1; ?>",
            "<details open ontoggle=alert(1)>x</details>",
            "<math><mi xlink:href=\"data:x\">x</mi></math>",
            "<form action=\"https://evil.example\"><input name=p></form>",
            "<https://evil.example/x>",
            "<a@b.c>",
            "&lt;script&gt;alert(1)&lt;/script&gt;",
            "&#x3C;script&#x3E;alert(1)&#x3C;/script&#x3E;",
            "&amp;lt;script&amp;gt;",
            "\\<script\\>alert(1)\\</script\\>",
            "`<script>alert(1)</script>`",
            "```\"><script>alert(1)</script>\n<script>alert(2)</script>\n```",
            "```html onclick=alert(1)\n<b>x</b>\n```",
            "```javascript\"><script>alert(1)</script>\nx\n```",
            "- <script>alert(1)</script>\n- [x](javascript:alert(1))",
            "> <img src=x onerror=alert(1)>",
            "| <script> |\n|---|\n| <img src=x onerror=alert(1)> |",
            "# <script>alert(1)</script>",
            "Title <b onmouseover=alert(1)>\n=====",
            "*<script>*alert(1)*</script>*",
            "**<img src=x onerror=alert(1)>**",
            "[^1]: <script>alert(1)</script>\n\ntext[^1]",
            "---\n<script>alert(1)</script>",
            "<div style=\"position:fixed;top:0;left:0;width:100%;height:100%\">cover</div>",
            "a\u{0}b<script>\u{7}alert(1)</script>",
        ];
        for text in corpus {
            let html = md(text);
            let seen = visible(&html);
            // The dangerous markup is shown as text, or it is not in the output at all.
            assert!(!html.contains("<script"), "{text:?} gave {html}");
            assert!(!html.contains("<img"), "{text:?} gave {html}");
            assert!(!html.contains("<a "), "{text:?} gave {html}");
            assert!(
                !html.contains("<iframe") && !html.contains("<svg") && !html.contains("<style"),
                "{text:?}"
            );
            assert!(
                !html.to_ascii_lowercase().contains("javascript:")
                    || seen.to_ascii_lowercase().contains("javascript:"),
                "{text:?}"
            );
        }
        // Raw HTML is read as text, character for character.
        let seen = visible(&md("<script>alert(1)</script>"));
        assert!(seen.contains("<script>alert(1)</script>"), "{seen}");
        let seen = visible(&md("a <b onclick=\"x\">y</b>"));
        assert!(seen.contains("<b onclick=\"x\">"), "{seen}");
        // An image is its source, a link with another scheme is its text.
        assert!(visible(&md("![alt](https://evil.example/x.png)"))
            .contains("![alt](https://evil.example/x.png)"));
        let html = md("[click](javascript:alert(1))");
        assert_eq!(visible(&html), "click");
        // The host after the user information is the host that the reader sees.
        let html = md("[x](https://good.example@evil.example/)");
        assert!(
            html.contains("(evil.example)") && !html.contains("good.example"),
            "{html}"
        );
        // An autolink is text with no host.
        let html = md("<https://evil.example/x>");
        assert_eq!(visible(&html), "https://evil.example/x");
        assert!(!html.contains("md-host"));
        // An entity is decoded once, then escaped.
        let html = md("&lt;script&gt;");
        assert!(
            html.contains("&lt;script&gt;") && !html.contains("<script"),
            "{html}"
        );
        let html = md("&amp;lt;");
        assert!(html.contains("&amp;lt;"), "{html}");
    }

    #[test]
    fn bombs_and_long_texts_stay_inside_the_subset() {
        let bombs = [
            "> ".repeat(3900) + "x",
            "- ".repeat(3900) + "x",
            "*".repeat(8000),
            "_".repeat(8000),
            "[".repeat(8000),
            "(".repeat(8000),
            "<".repeat(8000),
            "`".repeat(8000),
            "![".repeat(4000),
            "[a](".repeat(1600),
            "*a ".repeat(2600),
            "# a\n".repeat(2000),
            "x".repeat(8000),
            "x".repeat(8001),
            "\u{202e}".repeat(8000),
            "a  \n".repeat(2000),
            "1. a\n".repeat(1600),
            "```\n".repeat(2000),
            "    a\n".repeat(1100),
            "<a ".repeat(2600),
            "&".repeat(8000),
            "&#".repeat(4000),
        ];
        for text in &bombs {
            let start = std::time::Instant::now();
            let html = md(text);
            assert!(
                start.elapsed() < std::time::Duration::from_secs(5),
                "{}",
                &text[..text.len().min(20)]
            );
            assert!(
                html.len() < 2_000_000,
                "the output grows with the input only"
            );
        }
    }

    #[test]
    fn a_limit_gives_the_plain_text_of_the_author() {
        let html = md(&("> ".repeat(5) + "x"));
        assert!(
            html.contains("md-raw") && html.contains("&gt; &gt; &gt; &gt; &gt; x"),
            "{html}"
        );
    }

    #[test]
    fn bidi_and_zero_width_characters_are_visible_marks() {
        let html = md("a\u{202e}b \u{200b}c `x\u{2066}y`\n\n```\nz\u{202a}z\n```");
        assert!(
            html.contains("<span class=\"md-ctl\">U+202E</span>"),
            "{html}"
        );
        assert!(
            html.contains("<span class=\"md-ctl\">U+200B</span>"),
            "{html}"
        );
        assert!(
            html.contains("<span class=\"md-ctl\">U+2066</span>"),
            "{html}"
        );
        assert!(
            html.contains("<span class=\"md-ctl\">U+202A</span>"),
            "{html}"
        );
        // A bidi override in a link text cannot reorder the host: the host is a separate span.
        let html = md("[a\u{202e}b](https://h.example/)");
        assert!(
            html.contains("<span class=\"md-host\">(h.example)</span>"),
            "{html}"
        );
        // A plain explanation shows them too.
        let html = explanation("x\u{202e}y\u{0}z", ExplanationFormat::Plain);
        check_html(&html);
        assert!(html.contains("U+202E") && html.contains("U+0000"), "{html}");
        // A line break and a tab are text.
        assert!(explanation("a\n\tb", ExplanationFormat::Plain).contains("a\n\tb"));
    }

    #[test]
    fn plain_format_escapes_everything() {
        let html = explanation(
            "<b>x</b> & \"q\" 'r' [l](https://h.example/)",
            ExplanationFormat::Plain,
        );
        check_html(&html);
        assert_eq!(
            html,
            "<div class=\"md\"><p class=\"md-raw\">&lt;b&gt;x&lt;/b&gt; &amp; &quot;q&quot; &#39;r&#39; [l](https://h.example/)</p></div>"
        );
    }

    #[test]
    fn a_command_keeps_its_characters_and_gets_spans() {
        let sql = "SELECT 1 -- <script>\nFROM \"t\" WHERE x = 'a<b'";
        let html = command(sql, Language::Sql);
        check_html(&html);
        assert!(html
            .starts_with("<pre class=\"md-code md-cmd md-lang-sql\"><code><span class=\"md-ln\">"));
        assert_eq!(html.matches("md-ln").count(), 2);
        // The spans change nothing in the text.
        assert_eq!(visible(&html), sql);
        assert!(html.contains("<span class=\"tok-keyword\">SELECT</span>"));
        assert!(html.contains("<span class=\"tok-comment\">-- &lt;script&gt;</span>"));
        assert!(html.contains("<span class=\"tok-string\">&#39;a&lt;b&#39;</span>"));
        // Every language of the list gives the same text back.
        let text = "a 'b\" /* c\n\t\u{e9}\u{1f600} -- d\n@@ e\n+f\n-g";
        for lang in Language::ALL {
            let html = command(text, lang);
            check_html(&html);
            assert_eq!(visible(&html), text, "{lang:?}");
        }
        let html = command("+a\n-b\n@@ c", Language::Diff);
        assert!(html.contains("tok-add") && html.contains("tok-del") && html.contains("tok-hunk"));
    }

    #[test]
    fn a_command_has_a_span_for_each_line_and_marks_the_hidden_and_look_alike_characters() {
        // The line break at the end does not make a line. An empty line in the middle is a line.
        let html = command("a\n\nb\n", Language::Plain);
        assert_eq!(html.matches("<span class=\"md-ln\">").count(), 3, "{html}");
        assert_eq!(visible(&html), "a\n\nb\n");
        assert_eq!(command("", Language::Sql).matches("md-ln").count(), 0);
        let text = "DELETE FROM p\u{430}y\u{200B}ments -- \u{202E}x \u{e9}";
        let html = command(text, Language::Sql);
        check_html(&html);
        assert!(html.contains("<span class=\"md-ctl md-na\">\u{430} U+0430</span>"));
        assert!(html.contains("<span class=\"md-ctl\">U+200B</span>"));
        assert!(html.contains("<span class=\"md-ctl\">U+202E</span>"));
        // A French letter is not a look-alike: it has no mark, but it counts as non-ASCII.
        assert_eq!(html.matches("md-ctl").count(), 3, "{html}");
        let m = marks(text);
        assert_eq!(
            (m.hidden, m.lookalike, m.non_ascii, m.marked),
            (vec![0x200B, 0x202E], vec![0x430], 4, 3)
        );
    }

    #[test]
    fn no_class_comes_from_the_text() {
        // The language of a code block comes from a closed list: an info string that is not in it gives `plain`.
        let html = md("```\"><b class=x>\ncode\n```");
        assert!(html.contains("md-lang-plain"), "{html}");
        let html = md("```SQL extra words\nselect 1\n```");
        assert!(
            html.contains("md-lang-sql") && !html.contains("extra"),
            "{html}"
        );
    }
}
