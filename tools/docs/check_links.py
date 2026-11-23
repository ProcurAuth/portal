#!/usr/bin/env python3
"""Check the links of the documents of this repository.

Usage: tools/docs/check_links.py [--online] [FILE...]

Without a file argument, the script checks README.md, the other Markdown files of the
repository root, every Markdown file under docs/, deploy/, demo/ and assets/, and every HTML
file under docs/. It checks:
  - a relative link: the file or the directory must exist, and an anchor must exist;
  - a link to https://github.com/ProcurAuth/portal/{blob,tree}/<branch>/PATH: PATH must exist here;
  - a link to https://procurauth.github.io/portal/PAGE.html#ANCHOR: the page and the anchor must exist here;
  - a link to the sibling repositories of the organisation: the file must exist in the checkout
    next to this one, when the checkout exists;
  - the forbidden forms (the old owner account and its Pages host) must not appear;
  - with --online, every other http(s) URL answers with a status below 400 (a network is needed).
The script needs only Python 3. The exit code is 1 when it finds a problem.
"""
import html.parser
import os
import re
import sys
import urllib.request

ROOT = os.path.abspath(os.path.join(os.path.dirname(__file__), "..", ".."))
ORG = "ProcurAuth"
FORBIDDEN = ["github.com/SCcagg5", "SCcagg5/", "sccagg5.github.io"]
SKIP_DIRS = {".git", "target", "node_modules", "dist", "secrets", "build", ".gradle", "__pycache__", "vendor"}
ANCHOR_CACHE = {}


def github_slug(text):
    text = re.sub(r"<[^>]*>", "", text).strip().lower()
    text = re.sub(r"[^\w\- ]", "", text, flags=re.UNICODE)
    return text.replace(" ", "-")


def md_anchors(path):
    if path in ANCHOR_CACHE:
        return ANCHOR_CACHE[path]
    seen, out = {}, set()
    in_code = False
    for line in open(path, encoding="utf-8", errors="replace"):
        if line.strip().startswith(("```", "~~~")):
            in_code = not in_code
            continue
        if in_code:
            continue
        m = re.match(r"^#{1,6}\s+(.*?)\s*#*\s*$", line)
        if m:
            text = m.group(1)
            text = re.sub(r"\[([^\]]*)\]\([^)]*\)", r"\1", text).replace("`", "").replace("**", "").replace("__", "")
            base = github_slug(text)
            n = seen.get(base, 0)
            seen[base] = n + 1
            out.add(base if n == 0 else f"{base}-{n}")
        for m in re.finditer(r'<a\s+(?:name|id)="([^"]+)"', line):
            out.add(m.group(1))
    ANCHOR_CACHE[path] = out
    return out


class IdParser(html.parser.HTMLParser):
    def __init__(self):
        super().__init__()
        self.ids = set()
        self.links = []

    def handle_starttag(self, tag, attrs):
        a = dict(attrs)
        if "id" in a:
            self.ids.add(a["id"])
        if tag == "a" and a.get("name"):
            self.ids.add(a["name"])
        for key in ("href", "src"):
            if a.get(key):
                self.links.append(a[key])


def html_info(path):
    if path in ANCHOR_CACHE:
        return ANCHOR_CACHE[path]
    parser = IdParser()
    parser.feed(open(path, encoding="utf-8", errors="replace").read())
    ANCHOR_CACHE[path] = parser.ids
    return parser.ids


def anchors_of(path):
    if path.endswith(".md"):
        return md_anchors(path)
    if path.endswith(".html"):
        return html_info(path)
    return None


def md_links(path):
    text = open(path, encoding="utf-8", errors="replace").read()
    in_code = False
    for no, line in enumerate(text.split("\n"), 1):
        if line.strip().startswith(("```", "~~~")):
            in_code = not in_code
            continue
        if in_code:
            continue
        scrub = re.sub(r"`[^`]*`", "", line)
        for m in re.finditer(r"!?\[[^\]]*\]\(([^)\s]+)(?:\s+\"[^\"]*\")?\)", scrub):
            yield no, m.group(1)
        for m in re.finditer(r"<(https?://[^>\s]+)>", scrub):
            yield no, m.group(1)
        for m in re.finditer(r'(?:href|src|srcset)="([^"]+)"', scrub):
            yield no, m.group(1)
        for m in re.finditer(r"(?<![\"(<\[])(https?://[^\s)<>\]]+[^\s)<>\].,;:])", scrub):
            yield no, m.group(1)


def html_links(path):
    parser = IdParser()
    parser.feed(open(path, encoding="utf-8", errors="replace").read())
    for link in parser.links:
        yield 0, link


def resolve_repo_url(url):
    m = re.match(rf"^https://github\.com/{ORG}/([\w.-]+)/(?:blob|tree)/[^/]+/(.*)$", url)
    if not m:
        return None
    repo, rest = m.group(1), m.group(2)
    rest, _, frag = rest.partition("#")
    return repo, rest, frag


def check_file(path, online, problems, ext_urls):
    rel = os.path.relpath(path, ROOT)
    text = open(path, encoding="utf-8", errors="replace").read()
    for bad in FORBIDDEN:
        if bad in text:
            problems.append(f"{rel}: forbidden text {bad}")
    links = md_links(path) if path.endswith(".md") else html_links(path)
    for no, link in links:
        link = link.replace("&amp;", "&")
        if link.startswith(("mailto:", "data:", "javascript:", "tel:")):
            continue
        if link.startswith("http"):
            m = re.match(rf"^https://{ORG.lower()}\.github\.io/([\w.-]+)/?(.*)$", link)
            if m:
                repo, rest = m.group(1), m.group(2)
                rest, _, frag = rest.partition("#")
                base = ROOT if repo == "portal" else os.path.join(os.path.dirname(ROOT), repo)
                if os.path.isdir(base):
                    target = os.path.join(base, "docs", rest or "index.html")
                    if not os.path.isfile(target):
                        problems.append(f"{rel}:{no}: Pages link to a missing page: {link}")
                    elif frag and frag not in (anchors_of(target) or set()):
                        problems.append(f"{rel}:{no}: Pages link to a missing anchor: {link}")
                continue
            r = resolve_repo_url(link)
            if r:
                repo, sub, frag = r
                base = ROOT if repo == "portal" else os.path.join(os.path.dirname(ROOT), repo)
                if os.path.isdir(base):
                    target = os.path.join(base, sub.rstrip("/")) if sub else base
                    if not os.path.exists(target):
                        problems.append(f"{rel}:{no}: repository link to a missing path: {link}")
                    elif frag and os.path.isfile(target) and anchors_of(target) is not None and frag not in anchors_of(target):
                        problems.append(f"{rel}:{no}: repository link to a missing anchor: {link}")
                continue
            if re.match(rf"^https://github\.com/{ORG}/[\w.-]+/?$", link) or "/releases" in link and ORG in link or "/actions/" in link and ORG in link:
                continue
            if online:
                ext_urls.setdefault(link.split("#")[0], []).append(f"{rel}:{no}")
            continue
        if link.startswith("#"):
            target, frag = path, link[1:]
        else:
            p, _, frag = link.partition("#")
            p = p.partition("?")[0]
            target = os.path.normpath(os.path.join(os.path.dirname(path), p)) if p else path
            if not os.path.exists(target):
                problems.append(f"{rel}:{no}: missing file: {link}")
                continue
        if frag and os.path.isfile(target):
            known = anchors_of(target)
            if known is not None and frag not in known and not re.match(r"^L\d+", frag):
                problems.append(f"{rel}:{no}: missing anchor: {link}")


def default_files():
    out = []
    for name in sorted(os.listdir(ROOT)):
        if name.endswith(".md"):
            out.append(os.path.join(ROOT, name))
    for top in ("docs", ".github", "deploy", "demo", "assets", "web", "tools", "examples", "android", "crates"):
        for dirpath, dirs, files in os.walk(os.path.join(ROOT, top)):
            dirs[:] = [d for d in dirs if d not in SKIP_DIRS]
            for f in sorted(files):
                if f.endswith(".md") or (top == "docs" and f.endswith(".html")):
                    out.append(os.path.join(dirpath, f))
    return out


def main():
    args = [a for a in sys.argv[1:] if not a.startswith("--")]
    online = "--online" in sys.argv
    files = [os.path.abspath(a) for a in args] or default_files()
    problems, ext = [], {}
    for f in files:
        check_file(f, online, problems, ext)
    if online:
        for url, where in sorted(ext.items()):
            try:
                req = urllib.request.Request(url, method="HEAD", headers={"User-Agent": "link-check"})
                code = urllib.request.urlopen(req, timeout=15).status
            except Exception as exc:  # noqa: BLE001
                try:
                    req = urllib.request.Request(url, headers={"User-Agent": "link-check"})
                    code = urllib.request.urlopen(req, timeout=15).status
                except Exception as exc2:  # noqa: BLE001
                    problems.append(f"{where[0]}: URL does not answer: {url} ({exc2})")
                    continue
            if code >= 400:
                problems.append(f"{where[0]}: URL status {code}: {url}")
    for p in problems:
        print(p)
    print(f"{len(files)} files checked, {len(problems)} problems")
    return 1 if problems else 0


if __name__ == "__main__":
    sys.exit(main())
