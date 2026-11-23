#!/usr/bin/env python3
"""Build the static site in docs/ from the Markdown sources.

Usage:
  tools/docs/build_site.py           write the HTML pages into docs/
  tools/docs/build_site.py --check   write nothing; exit 1 when a page is out of date

The script needs only Python 3. It has no other dependency and no network access.
The Markdown files are the single source of each page. The pages in docs/*.html are
generated: do not edit them by hand. The home page comes from tools/docs/home.html.
The script also writes docs/sitemap.xml, docs/site.webmanifest and docs/robots.txt.
The exit code is 1 when a page keeps a Markdown link ("](") outside its code blocks.
Give each new page of pages.json a menu section in SECTION, or the build stops.

The Markdown reader supports the subset that the documents use: headings, paragraphs,
lists, tables, code blocks, block quotes, images, links, bold, italic and inline code.
A Mermaid block is shown on GitHub only. The site skips it, so give each diagram an
SVG picture next to the block.
"""
import html
import json
import os
import re
import sys

ROOT = os.path.abspath(os.path.join(os.path.dirname(__file__), "..", ".."))
DOCS = os.path.join(ROOT, "docs")
REPO_URL = "https://github.com/ProcurAuth/portal"
SITE_URL = "https://procurauth.github.io/portal/"
VERSION = None  # read from Cargo.toml

# (group, slug, markdown source relative to the repository root, nav label, eyebrow)
PAGES = [p for p in json.load(open(os.path.join(os.path.dirname(__file__), "pages.json"), encoding="utf-8"))
         if os.path.exists(os.path.join(ROOT, p[2])) or print("warning: missing source " + p[2], file=sys.stderr)]

MARK = ('<svg class="mark" viewBox="0 0 32 32" aria-hidden="true" focusable="false"><rect width="32" height="32" rx="8" fill="var(--accent)"/>'
        '<path d="M10.6 23.6V16.8a5.4 5.4 0 0 1 10.8 0v6.8" fill="none" stroke="var(--accent-ink)" stroke-width="4"/>'
        '<circle cx="16" cy="19.6" r="2" fill="#7dd3fc"/></svg>')
GH_ICON = ('<svg viewBox="0 0 24 24" aria-hidden="true" focusable="false"><path fill="currentColor" d="M12 .297c-6.63 0-12 5.373-12 12 0 5.303 '
           '3.438 9.8 8.205 11.385.6.113.82-.258.82-.577 0-.285-.01-1.04-.015-2.04-3.338.724-4.042-1.61-4.042-1.61C4.422 18.07 3.633 17.7 '
           '3.633 17.7c-1.087-.744.084-.729.084-.729 1.205.084 1.838 1.236 1.838 1.236 1.07 1.835 2.809 1.305 3.495.998.108-.776.417-1.305.76-1.605'
           '-2.665-.3-5.466-1.332-5.466-5.93 0-1.31.465-2.38 1.235-3.22-.135-.303-.54-1.523.105-3.176 0 0 1.005-.322 3.3 1.23.96-.267 1.98-.399 '
           '3-.405 1.02.006 2.04.138 3 .405 2.28-1.552 3.285-1.23 3.285-1.23.645 1.653.24 2.873.12 3.176.765.84 1.23 1.91 1.23 3.22 0 4.61-2.805 '
           '5.625-5.475 5.92.42.36.81 1.096.81 2.22 0 1.606-.015 2.896-.015 3.286 0 .315.21.69.825.57C20.565 22.092 24 17.592 24 12.297c0-6.627'
           '-5.373-12-12-12"/></svg>')
THEME_ICON = ('<svg viewBox="0 0 24 24" aria-hidden="true" focusable="false"><circle cx="12" cy="12" r="8.5" fill="none" stroke="currentColor" '
              'stroke-width="2"/><path d="M12 3.5a8.5 8.5 0 0 1 0 17z" fill="currentColor"/></svg>')

# The main menu. It is the same as the menu of the Stowr site.
NAV = [("index", "Overview"), ("getting-started", "Getting started"), ("configuration", "Configuration"),
       ("architecture", "Architecture"), ("security", "Security"), ("operations", "Operations"), ("reference", "Reference"),
       ("changelog", "Changelog")]

# The menu entry of each page that is not in the menu. The menu marks this entry as the current section.
SECTION = {
    "overview": "index", "documentation": "index",
    "authentication": "security", "grants-and-gate": "security", "keys-and-limits": "security",
    "inbox-security": "security", "design-login-hardening": "security",
    "identity-and-pdp": "architecture", "launcher": "architecture", "inbox": "architecture",
    "inbox-agents": "architecture", "inbox-android": "architecture", "design-inbox": "architecture",
    "inbox-configuration": "configuration",
    "settings-and-history": "operations", "several-instances": "operations", "traefik": "operations",
    "build-and-release": "operations", "development": "operations",
    "inbox-protocol": "reference", "inbox-requirements": "reference", "limitations": "reference", "conformance": "reference",
}

OG_ALT = "portal: the gate, the keys and the inbox for your services. One static binary."
TWITTER_ALT = "portal: the gate, the keys and the inbox for your services."
# These files of the 404 page load from the public address: the page can show at any depth.
ABSOLUTE_IN_404 = ("assets/favicon.svg", "assets/favicon-32.png", "assets/favicon.ico", "assets/apple-touch-icon.png",
                   "assets/site.css", "assets/site.js", "site.webmanifest")


def read_version():
    text = open(os.path.join(ROOT, "Cargo.toml"), encoding="utf-8").read()
    match = re.search(r'\[workspace\.package\][^\[]*?version\s*=\s*"([^"]+)"', text, re.S)
    return match.group(1) if match else "0"


# ----------------------------------------------------------------------------- Markdown

def github_slug(text):
    """The anchor that GitHub makes for a heading."""
    text = re.sub(r"<[^>]*>", "", text)
    text = text.strip().lower()
    text = re.sub(r"[^\w\- ]", "", text, flags=re.UNICODE)
    return text.replace(" ", "-")


def strip_marks(text):
    text = re.sub(r"\[([^\]]*)\]\([^)]*\)", r"\1", text)
    return text.replace("`", "").replace("**", "").replace("__", "")


class Resolver:
    """Map a link of a Markdown file to a link of the site."""

    def __init__(self, md_rel):
        self.md_dir = os.path.dirname(os.path.join(ROOT, md_rel))
        self.by_src = {os.path.normpath(os.path.join(ROOT, src)): slug for _g, slug, src, _l, _e in PAGES}

    def link(self, target):
        if re.match(r"^[a-z][a-z0-9+.-]*:", target) or target.startswith("#") or target.startswith("//"):
            return target
        path, _, frag = target.partition("#")
        full = os.path.normpath(os.path.join(self.md_dir, path))
        suffix = "#" + frag if frag else ""
        if full in self.by_src:
            return self.by_src[full] + ".html" + suffix
        if full.startswith(DOCS + os.sep) and full.endswith(".html") and os.path.exists(full):
            return os.path.relpath(full, DOCS) + suffix
        if full.startswith(DOCS + os.sep) and os.path.isfile(full) and not full.endswith(".md"):
            return os.path.relpath(full, DOCS) + suffix
        rel = os.path.relpath(full, ROOT)
        if rel.startswith(".."):
            return target
        kind = "tree" if os.path.isdir(full) else "blob"
        return f"{REPO_URL}/{kind}/main/{rel}{suffix}"

    def asset(self, target):
        if re.match(r"^[a-z][a-z0-9+.-]*:", target):
            return target
        full = os.path.normpath(os.path.join(self.md_dir, target))
        if full.startswith(DOCS + os.sep):
            return os.path.relpath(full, DOCS)
        return target


INLINE_CODE = re.compile(r"(`+)(.+?)\1")
LINK_TAIL = re.compile(r'\(([^)\s]+)(?:\s+"[^"]*")?\)')
TOKEN = re.compile("\x00(\\d+)\x00")


def closing_bracket(text, start):
    """The index of the "]" that closes the "[" at start, or -1. The search skips a code span inside the brackets."""
    depth = 0
    i = start
    while i < len(text):
        ch = text[i]
        if ch == "`":
            m = INLINE_CODE.match(text, i)
            if m:
                i = m.end()
                continue
        if ch == "[":
            depth += 1
        elif ch == "]":
            depth -= 1
            if depth == 0:
                return i
        i += 1
    return -1


def inline(text, res, autolink=True):
    """Convert the inline Markdown of one text to HTML.

    The code spans, the links and the images become tokens first, so a link text can hold a
    code span ([`name`](target)). The other marks apply to the text around the tokens.
    """
    tokens = []
    plain = []

    def keep(markup):
        tokens.append(markup)
        return f"\x00{len(tokens) - 1}\x00"

    i = 0
    while i < len(text):
        ch = text[i]
        if ch == "`":
            m = INLINE_CODE.match(text, i)
            if m:
                code = m.group(2).strip() or m.group(2)
                plain.append(keep("<code>" + html.escape(code, quote=False) + "</code>"))
                i = m.end()
                continue
        image = ch == "!" and text.startswith("[", i + 1)
        if ch == "[" or image:
            start = i + 1 if image else i
            end = closing_bracket(text, start)
            tail = LINK_TAIL.match(text, end + 1) if end > start + (0 if image else 1) else None
            if tail:
                label, target = text[start + 1:end], tail.group(1)
                if image:
                    plain.append(keep(f'<img src="{html.escape(res.asset(target), quote=True)}" alt="{html.escape(label, quote=True)}">'))
                else:
                    plain.append(keep(f'<a href="{html.escape(res.link(target), quote=True)}">{inline(label, res, autolink=False)}</a>'))
                i = tail.end()
                continue
        plain.append(ch)
        i += 1
    chunk = html.escape("".join(plain), quote=False)
    chunk = re.sub(r"&lt;br\s*/?&gt;", "<br>", chunk)
    chunk = re.sub(r"\*\*(.+?)\*\*", r"<strong>\1</strong>", chunk)
    chunk = re.sub(r"(?<![\w*])\*(?!\s)(.+?)(?<!\s)\*(?![\w*])", r"<em>\1</em>", chunk)
    chunk = re.sub(r"(?<![\w])_(?!\s)(.+?)(?<!\s)_(?![\w])", r"<em>\1</em>", chunk)
    if autolink:
        chunk = re.sub(r"(?<![\"'>=(])(https?://[^\s<)\x00]+[^\s<).,;:\x00])", r'<a href="\1">\1</a>', chunk)
    return TOKEN.sub(lambda m: tokens[int(m.group(1))], chunk)


def leftover_links(page):
    """True when the text of a page (outside the code blocks) keeps a Markdown link."""
    text = re.sub(r"<pre\b.*?</pre>", "", page, flags=re.S)
    text = re.sub(r"<code>.*?</code>", "<code></code>", text, flags=re.S)
    return "](" in text


def split_row(line):
    line = line.strip()
    if line.startswith("|"):
        line = line[1:]
    if line.endswith("|") and not line.endswith("\\|"):
        line = line[:-1]
    cells, cur, in_code = [], "", False
    i = 0
    while i < len(line):
        ch = line[i]
        if ch == "\\" and i + 1 < len(line) and line[i + 1] == "|":
            cur += "|"
            i += 2
            continue
        if ch == "`":
            in_code = not in_code
        if ch == "|" and not in_code:
            cells.append(cur.strip())
            cur = ""
        else:
            cur += ch
        i += 1
    cells.append(cur.strip())
    return cells


class Doc:
    def __init__(self, md_rel):
        self.md_rel = md_rel
        self.res = Resolver(md_rel)
        self.headings = []  # (level, id, text)
        self.title = None
        self.lede = None
        self.slugs = {}

    def unique_slug(self, text):
        base = github_slug(text)
        n = self.slugs.get(base, 0)
        self.slugs[base] = n + 1
        return base if n == 0 else f"{base}-{n}"

    def render(self, lines):
        out = []
        i = 0
        n = len(lines)
        para = []

        def flush_para():
            if para:
                text = " ".join(s.strip() for s in para)
                para.clear()
                img = re.fullmatch(r"!\[([^\]]*)\]\(([^)\s]+)\)", text)
                if img:
                    alt = html.escape(img.group(1))
                    src, _, frag = img.group(2).partition("#")
                    cls = " only-dark" if frag == "gh-dark-mode-only" else " only-light" if frag == "gh-light-mode-only" else ""
                    out.append(f'<figure class="panel{cls}"><img src="{self.res.asset(src)}" alt="{alt}" loading="lazy">'
                               f'<figcaption>{alt}</figcaption></figure>')
                    return
                body = inline(text, self.res)
                out.append(f"<p>{body}</p>")
                if self.lede is None and self.title is not None:
                    self.lede = strip_marks(text)

        while i < n:
            line = lines[i]
            stripped = line.strip()
            # HTML comment (one line or several)
            if stripped.startswith("<!--"):
                flush_para()
                while i < n and "-->" not in lines[i]:
                    i += 1
                i += 1
                continue
            m = re.match(r"^(`{3,}|~{3,})\s*([\w+-]*)\s*(.*)$", stripped)
            if m and not para or (m and stripped.startswith(("```", "~~~"))):
                flush_para()
                fence, lang = m.group(1), m.group(2)
                i += 1
                body = []
                while i < n and not lines[i].strip().startswith(fence):
                    body.append(lines[i].rstrip("\n"))
                    i += 1
                i += 1
                if lang == "mermaid":
                    continue
                out.append(self.code_block(body, lang))
                continue
            if not stripped:
                flush_para()
                i += 1
                continue
            m = re.match(r"^(#{1,6})\s+(.*?)\s*#*\s*$", stripped)
            if m:
                flush_para()
                level, text = len(m.group(1)), m.group(2)
                hid = self.unique_slug(strip_marks(text))
                if level == 1 and self.title is None:
                    self.title = strip_marks(text)
                    i += 1
                    continue
                self.headings.append((level, hid, strip_marks(text)))
                out.append(f'<h{level} id="{hid}"><a class="self" href="#{hid}">{inline(text, self.res)}</a></h{level}>')
                i += 1
                continue
            if re.match(r"^(-{3,}|\*{3,}|_{3,})$", stripped):
                flush_para()
                out.append("<hr>")
                i += 1
                continue
            if stripped.startswith(">"):
                flush_para()
                block = []
                while i < n and lines[i].strip().startswith(">"):
                    block.append(re.sub(r"^\s*>\s?", "", lines[i]))
                    i += 1
                sub = Doc.__new__(Doc)
                sub.__dict__.update(self.__dict__)
                sub.title = "x"
                sub.lede = "x"
                out.append('<div class="callout">' + sub.render(block) + "</div>")
                continue
            if stripped.startswith("|") and i + 1 < n and re.match(r"^\s*\|?[\s:|-]+\|[\s:|-]*$", lines[i + 1]) and "-" in lines[i + 1]:
                flush_para()
                head = split_row(lines[i])
                i += 2
                rows = []
                while i < n and lines[i].strip().startswith("|"):
                    rows.append(split_row(lines[i]))
                    i += 1
                out.append(self.table(head, rows))
                continue
            if re.match(r"^(\s*)([-*+]|\d+[.)])\s+", line):
                flush_para()
                block = []
                while i < n:
                    cur = lines[i]
                    if re.match(r"^(\s*)([-*+]|\d+[.)])\s+", cur) or (cur.startswith(("  ", "\t")) and cur.strip()) \
                            or (not cur.strip() and i + 1 < n and (re.match(r"^(\s*)([-*+]|\d+[.)])\s+", lines[i + 1]) or lines[i + 1].startswith("  ")) and lines[i + 1].strip()):
                        block.append(cur)
                        i += 1
                    else:
                        break
                out.append(self.list_block(block))
                continue
            para.append(line)
            i += 1
        flush_para()
        return "\n".join(out)

    def code_block(self, body, lang):
        label = {"hcl": "HCL", "sh": "Shell", "bash": "Shell", "console": "Shell", "json": "JSON", "yaml": "YAML", "text": "", "toml": "TOML",
                 "sql": "SQL", "rust": "Rust", "python": "Python", "http": "HTTP", "kotlin": "Kotlin", "xml": "XML", "dockerfile": "Dockerfile",
                 "ini": "INI", "js": "JavaScript", "javascript": "JavaScript", "html": "HTML", "diagram": "", "": ""}.get(lang, lang)
        lns = []
        for raw in body:
            expanded = raw.expandtabs(2)
            indent = len(expanded) - len(expanded.lstrip(" "))
            lns.append(f'<span class="ln" style="--i:{indent}">{html.escape(expanded.strip(chr(10))) if expanded.strip() else ""}</span>')
        tag = f'<span class="label">{html.escape(label)}</span>' if label else ""
        # A block of plain text is a drawing in characters: the site keeps its lines and does not wrap them.
        cls = ' class="diagram"' if lang in ("text", "diagram") else ""
        return (f'<div class="code-wrap">{tag}<pre{cls} data-copy tabindex="0"><code>{"".join(lns)}</code></pre></div>')

    def table(self, head, rows):
        labels = [strip_marks(h) for h in head]
        o = ['<div class="tablewrap"><table><thead><tr>']
        for h in head:
            o.append(f'<th scope="col">{inline(h, self.res)}</th>')
        o.append("</tr></thead><tbody>")
        for row in rows:
            row = (row + [""] * len(head))[: len(head)]
            o.append("<tr>")
            for k, cell in enumerate(row):
                if k == 0:
                    o.append(f'<th scope="row">{inline(cell, self.res)}</th>')
                else:
                    o.append(f'<td data-label="{html.escape(labels[k])}">{inline(cell, self.res)}</td>')
            o.append("</tr>")
        o.append("</tbody></table></div>")
        return "".join(o)

    def list_block(self, block):
        items = []  # (indent, ordered, text lines)
        for raw in block:
            m = re.match(r"^(\s*)([-*+]|\d+[.)])\s+(.*)$", raw)
            if m:
                indent = len(m.group(1).expandtabs(2))
                items.append([indent, m.group(2)[0].isdigit(), [m.group(3)]])
            elif raw.strip() and items:
                items[-1][2].append(raw.strip())
            elif not raw.strip() and items:
                items[-1][2].append("")
        return self.build_list(items, 0)[0]

    def build_list(self, items, pos):
        base = items[pos][0]
        ordered = items[pos][1]
        tag = "ol" if ordered else "ul"
        o = [f"<{tag}>"]
        while pos < len(items) and items[pos][0] >= base:
            indent, _ordered, lines = items[pos]
            if indent > base:
                # deeper item without parent: treat as nested in the previous li (handled below)
                break
            # separate the text of the item from a fenced code that sits inside it
            text_lines = []
            code_html = []
            k = 0
            while k < len(lines):
                ln = lines[k]
                fm = re.match(r"^(`{3,}|~{3,})\s*([\w+-]*)", ln)
                if fm:
                    fence = fm.group(1)
                    body = []
                    k += 1
                    while k < len(lines) and not lines[k].startswith(fence):
                        body.append(lines[k])
                        k += 1
                    k += 1
                    if fm.group(2) != "mermaid":
                        code_html.append(self.code_block(body, fm.group(2)))
                    continue
                text_lines.append(ln)
                k += 1
            body = inline(" ".join(t for t in text_lines if t != ""), self.res)
            pos += 1
            nested = ""
            if pos < len(items) and items[pos][0] > base:
                nested, pos = self.build_list(items, pos)
            o.append(f"<li>{body}{''.join(code_html)}{nested}</li>")
        o.append(f"</{tag}>")
        return "".join(o), pos


# ----------------------------------------------------------------------------- Pages

def head_html(title, description, slug):
    url = SITE_URL if slug == "index" else f"{SITE_URL}{slug}.html"
    d = html.escape(description, quote=True)
    t = html.escape(title, quote=True)
    # The 404 page is not a page to index, and it has no address of its own.
    robots = '<meta name="robots" content="noindex">\n' if slug == "404" else ""
    canonical = "" if slug == "404" else f'<link rel="canonical" href="{url}">\n'
    og_url = "" if slug == "404" else f'<meta property="og:url" content="{url}">\n'
    return f"""<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
{robots}<meta name="color-scheme" content="light dark">
<meta name="description" content="{d}">
<meta property="og:type" content="website">
<meta property="og:site_name" content="portal">
<meta property="og:title" content="{t}">
<meta property="og:description" content="{d}">
{og_url}<meta property="og:image" content="{SITE_URL}assets/og.png">
<meta property="og:image:width" content="1200">
<meta property="og:image:height" content="630">
<meta property="og:image:alt" content="{html.escape(OG_ALT, quote=True)}">
<meta name="twitter:card" content="summary_large_image">
<meta name="twitter:title" content="{t}">
<meta name="twitter:description" content="{d}">
<meta name="twitter:image" content="{SITE_URL}assets/og.png">
<meta name="twitter:image:alt" content="{html.escape(TWITTER_ALT, quote=True)}">
{canonical}<title>{t}</title>
<link rel="icon" href="assets/favicon.svg" type="image/svg+xml">
<link rel="icon" href="assets/favicon-32.png" sizes="32x32" type="image/png">
<link rel="icon" href="assets/favicon.ico" sizes="16x16 32x32 48x48">
<link rel="apple-touch-icon" href="assets/apple-touch-icon.png">
<link rel="manifest" href="site.webmanifest">
<meta name="theme-color" content="#fafafd" media="(prefers-color-scheme: light)">
<meta name="theme-color" content="#0c0e15" media="(prefers-color-scheme: dark)">
<link rel="stylesheet" href="assets/site.css">
<script src="assets/site.js"></script>
</head>
<body>
<a class="skip" href="#main">Skip to the content</a>
"""


def header_html(slug):
    section = SECTION.get(slug)
    items = []
    for s, label in NAV:
        # "page" marks the page itself. "true" marks the menu entry of the section that holds the page.
        cur = ' aria-current="page"' if s == slug else ' aria-current="true"' if s == section else ""
        items.append(f'<li><a href="{s}.html"{cur}>{label}</a></li>')
    items.append(f'<li class="nav-gh"><a href="{REPO_URL}">GitHub</a></li>')
    return f"""<header class="site-header">
  <div class="wrap bar">
    <a class="brand" href="index.html" aria-label="portal, home page">{MARK}<span>port<b>al</b></span></a>
    <button class="menu-btn" id="menu-btn" type="button" aria-expanded="false" aria-controls="site-nav">Menu</button>
    <nav id="site-nav" aria-label="Main"><ul>{''.join(items)}</ul></nav>
    <a class="gh-link" href="{REPO_URL}" aria-label="portal on GitHub" title="portal on GitHub">{GH_ICON}<span>GitHub</span></a>
    <button class="icon-btn" id="theme-btn" type="button" aria-label="Switch colour theme" title="Switch colour theme">{THEME_ICON}</button>
  </div>
</header>
<main id="main">
"""


def footer_html():
    groups = {}
    for g, slug, _src, label, _e in PAGES:
        groups.setdefault(g, []).append((slug, label))
    cols = []
    for g in ("Start", "Use", "Configure and operate", "Reference"):
        links = "".join(f'<li><a href="{s}.html">{html.escape(l)}</a></li>' for s, l in groups.get(g, [])[:8])
        if g == "Start":
            links = '<li><a href="documentation.html">All the documentation</a></li>' + links
        cols.append(f"<div><h2>{g}</h2><ul>{links}</ul></div>")
    cols.append(f'<div><h2>Project</h2><ul><li><a href="{REPO_URL}">Repository</a></li>'
                f'<li><a href="{REPO_URL}/releases/latest">Downloads</a></li>'
                f'<li><a href="{REPO_URL}/blob/main/SECURITY.md">Security policy</a></li>'
                f'<li><a href="{REPO_URL}/blob/main/CONTRIBUTING.md">Contribute</a></li>'
                '<li><a href="https://procurauth.github.io/procurauth/">ProcurAuth specification</a></li></ul></div>')
    return f"""</main>
<footer class="site-footer">
  <div class="wrap">
    <div class="foot-grid">{''.join(cols)}</div>
    <div class="foot-legal">
      <p>portal, version {VERSION}. Part of <a href="https://procurauth.github.io/procurauth/">ProcurAuth</a>.</p>
      <p>Licence: <a href="{REPO_URL}/blob/main/LICENSE-MIT">MIT</a> or <a href="{REPO_URL}/blob/main/LICENSE-APACHE">Apache-2.0</a>.</p>
    </div>
  </div>
</footer>
</body>
</html>
"""


def toc_html(headings, depth):
    items = [(lvl, hid, text) for lvl, hid, text in headings if 2 <= lvl <= depth]
    if len(items) < 2:
        return ""
    lis = []
    for lvl, hid, text in items:
        pad = f' style="padding-left:{(lvl - 2) * 14 + 10}px"' if lvl > 2 else ""
        lis.append(f'<li><a href="#{hid}"{pad}>{html.escape(text)}</a></li>')
    return (f'<nav class="toc" aria-label="On this page"><details class="toc-details" open><summary>On this page</summary>'
            f'<ol>{"".join(lis)}</ol></details></nav>')


def page_neighbours(slug):
    order = [p[1] for p in PAGES]
    if slug not in order:
        return ""
    k = order.index(slug)
    label = {p[1]: p[3] for p in PAGES}
    parts = []
    if k > 0:
        parts.append(f'<a class="btn small" href="{order[k-1]}.html">&larr; {html.escape(label[order[k-1]])}</a>')
    if k + 1 < len(order):
        parts.append(f'<a class="btn small" href="{order[k+1]}.html">{html.escape(label[order[k+1]])} &rarr;</a>')
    return f'<div class="btn-row" style="margin-top:40px">{"".join(parts)}</div>'


def build_page(group, slug, src, label, eyebrow, depth):
    doc = Doc(src)
    lines = open(os.path.join(ROOT, src), encoding="utf-8").read().split("\n")
    body = doc.render(lines)
    title = doc.title or label
    lede = doc.lede or ""
    edit = f"{REPO_URL}/blob/main/{src}"
    page = head_html(f"{title} | portal", lede or title, slug)
    page += header_html(slug)
    page += (f'<div class="page-head"><div class="wrap"><p class="eyebrow"><span class="badge">Version {VERSION}</span> {html.escape(eyebrow)}</p>'
             f'<h1>{html.escape(title)}</h1>' + (f'<p class="lede">{html.escape(lede)}</p>' if lede else "") + "</div></div>")
    page += f'<div class="wrap doc-layout">{toc_html(doc.headings, depth)}<article class="doc">{drop_first_para(body)}'
    page += f'<p class="muted" style="margin-top:40px">Source of this page: <a href="{edit}"><code>{html.escape(src)}</code></a>.</p>'
    page += f"{page_neighbours(slug)}</article></div>"
    page += footer_html()
    return page


def drop_first_para(body):
    """The first paragraph is the lede in the page head: do not repeat it."""
    return re.sub(r"^<p>.*?</p>\n?", "", body, count=1, flags=re.S) if body.startswith("<p>") else body


def short(text, limit=200):
    """The first sentences of a lede that fit in the limit."""
    parts = re.split(r"(?<=[.!?])\s+", text.strip())
    out = parts[0]
    for part in parts[1:]:
        if len(out) + 1 + len(part) > limit:
            break
        out += " " + part
    if len(out) > limit:
        out = out[:limit].rsplit(" ", 1)[0] + "..."
    return out


def build_documentation_index():
    groups = {}
    for g, slug, src, label, _e in PAGES:
        doc = Doc(src)
        doc.render(open(os.path.join(ROOT, src), encoding="utf-8").read().split("\n"))
        groups.setdefault(g, []).append((slug, label, doc.lede or ""))
    parts = []
    for g in ("Start", "Use", "Configure and operate", "Reference"):
        cards = "".join(
            f'<a class="card" href="{s}.html"><h3>{html.escape(l)}</h3><p>{html.escape(short(d))}</p></a>'
            for s, l, d in groups.get(g, []))
        parts.append(f'<section class="section" style="padding:24px 0"><div class="section-head"><h2>{g}</h2></div><div class="grid c3">{cards}</div></section>')
    page = head_html("Documentation | portal", "The documentation of portal: guides, configuration, security, operations and reference.", "documentation")
    page += header_html("documentation")
    page += ('<div class="page-head"><div class="wrap"><p class="eyebrow"><span class="badge">Version ' + VERSION + '</span> Documentation</p>'
             '<h1>Documentation</h1><p class="lede">Each page has one purpose. Start with the overview, then follow the guides in the order that you need them.</p></div></div>'
             '<div class="wrap" style="padding-bottom:64px">' + "".join(parts) + "</div>")
    page += footer_html()
    return page


def build_home():
    template = open(os.path.join(os.path.dirname(__file__), "home.html"), encoding="utf-8").read()
    page = head_html("portal: the gate, the keys and the inbox for your services", "portal is one program that signs people in, protects services, "
                     "gives keys to agents, answers AuthZEN questions and sends alerts to phones.", "index")
    page += header_html("index")
    page += template.replace("{{VERSION}}", VERSION)
    page += footer_html()
    return page


def not_found():
    """The 404 page. GitHub Pages shows it at any depth, so its links use the public address."""
    page = (head_html("Page not found | portal", "This page does not exist. Go to the home page or to the documentation of portal.", "404")
            + header_html("404")
            + '<div class="page-head"><div class="wrap"><p class="eyebrow"><span class="badge">Error 404</span> Page not found</p>'
            '<h1>This page does not exist</h1><p class="lede">The address is wrong, or the page moved. '
            'Go to the home page or to the documentation.</p></div></div>'
            '<div class="page-body"><div class="wrap"><div class="btn-row"><a class="btn primary" href="index.html">Home page</a>'
            '<a class="btn" href="documentation.html">Documentation</a></div></div></div>\n'
            + footer_html())
    for rel in ABSOLUTE_IN_404:
        page = page.replace(f'"{rel}"', f'"{SITE_URL}{rel}"')
    return re.sub(r'href="([a-z0-9-]+\.html)"', lambda m: f'href="{SITE_URL}{m.group(1)}"', page)


def image_size(path):
    """The intrinsic (width, height) of a PNG, WebP or SVG file, or None."""
    try:
        with open(path, "rb") as handle:
            data = handle.read(4096)
    except OSError:
        return None
    if data[:8] == b"\x89PNG\r\n\x1a\n" and data[12:16] == b"IHDR":
        return int.from_bytes(data[16:20], "big"), int.from_bytes(data[20:24], "big")
    if data[:4] == b"RIFF" and data[8:12] == b"WEBP":
        kind = data[12:16]
        if kind == b"VP8X":
            return 1 + int.from_bytes(data[24:27], "little"), 1 + int.from_bytes(data[27:30], "little")
        if kind == b"VP8 ":
            return int.from_bytes(data[26:28], "little") & 0x3FFF, int.from_bytes(data[28:30], "little") & 0x3FFF
        if kind == b"VP8L":
            bits = int.from_bytes(data[21:25], "little")
            return (bits & 0x3FFF) + 1, ((bits >> 14) & 0x3FFF) + 1
        return None
    if path.endswith(".svg"):
        root = re.search(r"<svg\b[^>]*>", data.decode("utf-8", "replace"), re.S)
        if not root:
            return None
        tag = root.group(0)
        box = re.search(r'viewBox="\s*[-\d.]+[\s,]+[-\d.]+[\s,]+([\d.]+)[\s,]+([\d.]+)\s*"', tag)
        width = re.search(r'\swidth="([\d.]+)"', tag)
        height = re.search(r'\sheight="([\d.]+)"', tag)
        if width and height:
            return round(float(width.group(1))), round(float(height.group(1)))
        if box:
            return round(float(box.group(1))), round(float(box.group(2)))
    return None


def size_images(page):
    """Write the intrinsic width and height on each local image, so the layout does not move while it loads.

    The image in the hero of a page loads first. The other images keep their lazy load.
    """
    hero = re.search(r'<section class="hero".*?</section>', page, re.S)
    hero_span = hero.span() if hero else (0, 0)

    def fix(m):
        tag = m.group(0)
        src = re.search(r'\ssrc="([^"]+)"', tag)
        if src and " width=" not in tag and not re.match(r"^[a-z][a-z0-9+.-]*:", src.group(1)):
            size = image_size(os.path.join(DOCS, html.unescape(src.group(1)).partition("#")[0]))
            if size:
                tag = tag.replace(src.group(0), f'{src.group(0)} width="{size[0]}" height="{size[1]}"', 1)
        if hero_span[0] <= m.start() < hero_span[1]:
            tag = re.sub(r'\sloading="[^"]*"', "", tag)
            tag = tag[:-1] + ' loading="eager" fetchpriority="high">'
        return tag

    return re.sub(r"<img\b[^>]*>", fix, page)


def small_files():
    """The sitemap, the web manifest and the robots file."""
    urls = [SITE_URL] + [f"{SITE_URL}{p[1]}.html" for p in PAGES] + [f"{SITE_URL}documentation.html"]
    sitemap = '<?xml version="1.0" encoding="UTF-8"?>\n<urlset xmlns="http://www.sitemaps.org/schemas/sitemap/0.9">\n'
    sitemap += "".join(f"  <url><loc>{u}</loc></url>\n" for u in urls) + "</urlset>\n"
    manifest = """{
  "name": "portal",
  "short_name": "portal",
  "description": "The gate, the keys and the inbox of ProcurAuth: sign-in, access control, keys for agents and alerts to phones.",
  "start_url": "index.html",
  "scope": "./",
  "display": "browser",
  "background_color": "#fafafd",
  "theme_color": "#4338ca",
  "icons": [
    { "src": "assets/icon-192.png", "sizes": "192x192", "type": "image/png" },
    { "src": "assets/icon-512.png", "sizes": "512x512", "type": "image/png" }
  ]
}
"""
    robots = f"User-agent: *\nAllow: /\n\nSitemap: {SITE_URL}sitemap.xml\n"
    return {"sitemap.xml": sitemap, "site.webmanifest": manifest, "robots.txt": robots}


def build():
    """Every generated file of docs/, by name."""
    missing = [p[1] for p in PAGES if p[1] not in SECTION and p[1] not in dict(NAV)]
    if missing:
        raise SystemExit("error: give a menu section to these pages in SECTION: " + ", ".join(missing))
    outputs = {}
    depth_of = {"configuration": 4}
    for group, slug, src, label, eyebrow in PAGES:
        outputs[slug + ".html"] = build_page(group, slug, src, label, eyebrow, depth_of.get(slug, 3))
    outputs["documentation.html"] = build_documentation_index()
    outputs["index.html"] = build_home()
    outputs["404.html"] = not_found()
    outputs = {name: size_images(text) for name, text in outputs.items()}
    outputs.update(small_files())
    return outputs


def main():
    global VERSION
    VERSION = read_version()
    check = "--check" in sys.argv
    outputs = build()
    bad = 0
    for name, text in outputs.items():
        if name.endswith(".html") and leftover_links(text):
            print(f"Markdown link left in the text: docs/{name}")
            bad += 1
    for name, text in outputs.items():
        path = os.path.join(DOCS, name)
        old = open(path, encoding="utf-8").read() if os.path.exists(path) else None
        if old != text:
            if check:
                print(f"out of date: docs/{name}")
                bad += 1
            else:
                with open(path, "w", encoding="utf-8") as handle:
                    handle.write(text)
                print(f"wrote docs/{name}")
    for name in sorted(os.listdir(DOCS)):
        if name.endswith(".html") and name not in outputs:
            if check:
                print(f"stale page: docs/{name}")
                bad += 1
            else:
                os.remove(os.path.join(DOCS, name))
                print(f"removed docs/{name}")
    if bad:
        print("run tools/docs/build_site.py" if check else "fix the pages above")
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
