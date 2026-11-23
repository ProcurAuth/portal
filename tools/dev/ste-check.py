#!/usr/bin/env python3
"""Check Markdown files against the ASD-STE100 limits of this repository.

Usage: ste-check.py [--no-tables] FILE[:START-END] [FILE...]

A file that ends with .html is reduced to its text first (each block element is one paragraph).

The script needs only Python 3 and no other package. It reports:
  - a procedural sentence of more than 20 words;
  - a descriptive sentence of more than 25 words;
  - a paragraph of more than 6 sentences;
  - a word that ends in "ing" (an -ing form);
  - a phrasal verb from the list PHRASAL below.

A sentence is procedural when its first word is an imperative verb from
IMPERATIVES. Headings, code fences, Markdown tables and requirement id
prefixes (for example "3.7.2") are not sentences. A table cell gets the check
of the -ing forms and the phrasal verbs only. A part of a file can be checked
with FILE:START-END (line numbers). The exit code is 1 when the script finds
a problem, and 0 otherwise.
"""
import re
import sys

PROCEDURAL_LIMIT = 20
DESCRIPTIVE_LIMIT = 25
PARAGRAPH_LIMIT = 6

# Words that end in "ing" and that STE or the vocabulary of this project allows.
ING_OK = {
    "during", "string", "thing", "things", "nothing", "something", "anything", "everything",
    "morning", "ring", "bring", "spring", "ping", "king", "wing", "sing", "evening", "ceiling",
    "pending", "sibling", "bearing", "building", "warning", "binding", "bing", "sling", "swing", "sting",
    # The noun of the feature "agents": the grouping of the items by agent.
    "grouping",
    # Technical names of this project: the page names "Getting started" and "Security model and hardening", and the routing of a proxy.
    "getting", "hardening", "routing",
}

# First words of a procedural sentence (imperative mood).
IMPERATIVES = {
    "add", "apply", "ask", "back", "build", "call", "change", "check", "choose", "clear", "close",
    "compare", "configure", "connect", "copy", "create", "decide", "delete", "describe", "do",
    "enable", "enroll", "enter", "find", "follow", "generate", "get", "give", "go", "install",
    "keep", "let", "link", "list", "make", "mention", "open", "put", "read", "record", "refuse",
    "regenerate", "remove", "replace", "report", "restart", "revoke", "run", "save", "see", "select",
    "send", "set", "show", "start", "stop", "take", "tell", "test", "turn", "update", "use",
    "verify", "write",
}

# Phrasal verbs: (list of verb forms, list of particles). A pronoun may sit between them.
PHRASAL = [
    (["set", "sets", "setting"], ["up", "out", "off", "aside"]),
    (["log", "logs", "logged", "logging"], ["in", "out", "on", "off"]),
    (["sign", "signs", "signed", "signing"], ["in", "out", "up", "off"]),
    (["turn", "turns", "turned", "turning"], ["on", "off", "up", "down", "out", "back"]),
    (["switch", "switches", "switched"], ["on", "off", "over", "back"]),
    (["carry", "carries", "carried"], ["out", "on", "over"]),
    (["point", "points", "pointed"], ["out"]),
    (["back", "backs", "backed"], ["up", "off", "out"]),
    (["check", "checks", "checked"], ["out", "in", "up"]),
    (["fill", "fills", "filled"], ["in", "out", "up"]),
    (["find", "finds", "found"], ["out"]),
    (["give", "gives", "gave", "given"], ["up", "out", "away", "back"]),
    (["go", "goes", "went", "gone"], ["on", "off", "up", "down", "out", "back", "over"]),
    (["look", "looks", "looked"], ["up", "out", "over"]),
    (["make", "makes", "made"], ["up", "out", "over"]),
    (["pick", "picks", "picked"], ["up", "out"]),
    (["put", "puts"], ["in", "on", "off", "out", "up", "down", "away", "back"]),
    (["run", "runs", "ran"], ["out", "up", "down", "over", "into"]),
    (["send", "sends", "sent"], ["back", "out", "off"]),
    (["show", "shows", "showed", "shown"], ["up", "off"]),
    (["shut", "shuts"], ["down", "off", "up"]),
    (["start", "starts", "started"], ["up", "over", "off"]),
    (["take", "takes", "took", "taken"], ["off", "out", "up", "over", "down", "back", "away"]),
    (["try", "tries", "tried"], ["out"]),
    (["use", "uses", "used"], ["up"]),
    (["come", "comes", "came"], ["back", "in", "out", "up", "down", "off"]),
    (["hand", "hands", "handed"], ["over", "out", "in"]),
    (["hold", "holds", "held"], ["on", "off", "up", "back", "out"]),
    (["keep", "keeps", "kept"], ["on", "up", "out", "off", "away"]),
    (["pass", "passes", "passed"], ["on", "over", "out"]),
    (["read", "reads"], ["out", "back"]),
    (["rule", "rules", "ruled"], ["out"]),
    (["sort", "sorts", "sorted"], ["out"]),
    (["throw", "throws", "threw", "thrown"], ["away", "out", "up"]),
    (["write", "writes", "wrote", "written"], ["down", "out", "up", "off"]),
    (["clean", "cleans", "cleaned"], ["up", "out"]),
    (["break", "breaks", "broke", "broken"], ["down", "up", "in", "out"]),
    (["bring", "brings", "brought"], ["up", "back", "down", "in", "out"]),
    (["build", "builds", "built"], ["up"]),
    (["call", "calls", "called"], ["off", "back", "up", "out"]),
    (["cut", "cuts"], ["off", "out", "down", "back"]),
    (["drop", "drops", "dropped"], ["off", "out", "in"]),
    (["end", "ends", "ended"], ["up"]),
    (["get", "gets", "got", "gotten"], ["back", "out", "up", "off", "over", "rid"]),
    (["move", "moves", "moved"], ["on", "out", "over", "away"]),
    (["pay", "pays", "paid"], ["back", "off"]),
    (["play", "plays", "played"], ["back", "out"]),
    (["roll", "rolls", "rolled"], ["back", "out", "over", "up"]),
    (["step", "steps", "stepped"], ["in", "out", "up", "down"]),
    (["wind", "winds", "wound"], ["up", "down"]),
    (["work", "works", "worked"], ["out", "around"]),
    (["zoom", "zooms", "zoomed"], ["in", "out"]),
    (["wrap", "wraps", "wrapped"], ["up"]),
    (["wipe", "wipes", "wiped"], ["out"]),
    (["tear", "tears", "tore"], ["down"]),
    (["spin", "spins", "spun"], ["up", "down"]),
    (["scale", "scales", "scaled"], ["up", "down", "out"]),
    (["opt", "opts", "opted"], ["in", "out"]),
    ]
PRONOUNS = {"it", "them", "this", "that", "him", "her", "us", "me", "you"}

PHRASAL_INDEX = {}
for forms, particles in PHRASAL:
    for form in forms:
        PHRASAL_INDEX.setdefault(form, set()).update(particles)

ID_PREFIX = re.compile(r"^(?:\d+(?:\.\d+)*[a-z]?\s+)")
LIST_PREFIX = re.compile(r"^(?:[-*+]|\d+[.)])\s+")
WORD = re.compile(r"[A-Za-z][A-Za-z0-9_'-]*|\d+")
SPLIT = re.compile(r"(?<=[.!?:;])\s+(?=[A-Z0-9(])")
TABLE_ROW = re.compile(r"^\s*\|")
LINK_DEFINITION = re.compile(r"^\[[^\]]+\]:\s+\S+$")
TABLE_RULE = re.compile(r"^\s*\|[\s:|-]+\|\s*$")


def clean(text):
    """Remove Markdown marks and replace inline code by one neutral word."""
    text = re.sub(r"`[^`]*`", "CODE", text)
    text = re.sub(r"\[([^\]]*)\]\([^)]*\)", r"\1", text)
    text = re.sub(r"https?://\S+", "URL", text)
    text = re.sub(r"<[^>]*>", "TAG", text)
    text = text.replace("**", "").replace("__", "")
    return text.strip()


def words_of(sentence):
    return WORD.findall(sentence)


def is_procedural(sentence):
    words = words_of(sentence)
    return bool(words) and words[0].lower() in IMPERATIVES and words[0][0].isupper()


def split_sentences(text):
    return [s for s in SPLIT.split(text) if words_of(s)]


def word_problems(sentence):
    """Yield (kind, text) for -ing forms and phrasal verbs of one sentence."""
    words = words_of(sentence)
    for word in words:
        low = word.lower()
        if low.endswith("ing") and len(low) > 4 and low not in ING_OK and word != "CODE":
            yield "-ing form", word
    lows = [w.lower() for w in words]
    for i, low in enumerate(lows):
        particles = PHRASAL_INDEX.get(low)
        if not particles:
            continue
        nxt = lows[i + 1] if i + 1 < len(lows) else ""
        if nxt in PRONOUNS and i + 2 < len(lows):
            nxt = lows[i + 2]
        if nxt in particles:
            yield "phrasal verb", f"{words[i]} {words[i + 1]}" if nxt == lows[i + 1] else \
                f"{words[i]} {words[i + 1]} {words[i + 2]}"


def html_to_text(raw):
    """Reduce an HTML page to lines of text: one paragraph for each block element."""
    import html as htmllib
    raw = re.sub(r"(?is)<(script|style|svg|pre|head)\b.*?</\1>", "", raw)
    raw = re.sub(r"(?i)</(p|li|h[1-6]|div|section|figcaption|td|th|summary|header|footer|nav|ul|ol|table|tr|a)>", "\n\n", raw)
    raw = re.sub(r"(?i)<br\s*/?>", "\n\n", raw)
    raw = re.sub(r"<[^>]*>", "", raw)
    return htmllib.unescape(raw)


def check_file(spec, check_tables):
    path, _, rng = spec.partition(":")
    first, last = 1, 10**9
    if rng:
        a, _, b = rng.partition("-")
        first, last = int(a), int(b)
    with open(path, encoding="utf-8") as handle:
        raw = handle.read()
    if path.endswith(".html"):
        raw = html_to_text(raw)
    lines = raw.split("\n")
    problems = []
    in_code = False
    paragraph = []  # list of (line number, text)

    def flush():
        if not paragraph:
            return
        sentences = []
        for no, text in paragraph:
            for sent in split_sentences(text):
                sentences.append((no, sent))
        for no, sent in sentences:
            limit = PROCEDURAL_LIMIT if is_procedural(sent) else DESCRIPTIVE_LIMIT
            count = len(words_of(sent))
            if count > limit:
                kind = "procedural" if limit == PROCEDURAL_LIMIT else "descriptive"
                problems.append(f"{path}:{no}: {kind} sentence of {count} words (max {limit}): {sent[:90]}")
            for kind, word in word_problems(sent):
                problems.append(f"{path}:{no}: {kind}: {word}")
        if len(sentences) > PARAGRAPH_LIMIT:
            problems.append(
                f"{path}:{paragraph[0][0]}: paragraph of {len(sentences)} sentences (max {PARAGRAPH_LIMIT})")
        paragraph.clear()

    for no, line in enumerate(lines, 1):
        if no < first or no > last:
            continue
        if line.lstrip().startswith("```"):
            flush()
            in_code = not in_code
            continue
        if in_code:
            continue
        stripped = line.strip()
        if not stripped:
            flush()
            continue
        if stripped.startswith("#"):
            flush()
            continue
        if TABLE_ROW.match(line):
            flush()
            if check_tables and not TABLE_RULE.match(line):
                for cell in line.strip().strip("|").split("|"):
                    for sent in split_sentences(clean(cell)):
                        for kind, word in word_problems(sent):
                            problems.append(f"{path}:{no}: {kind}: {word}")
            continue
        if stripped.startswith(("<!--", "-->")) or stripped == "---" or LINK_DEFINITION.match(stripped):
            flush()
            continue
        if LIST_PREFIX.match(stripped):
            flush()
            stripped = LIST_PREFIX.sub("", stripped, count=1)
        body = clean(stripped)
        # A requirement id prefix, with an optional mark such as FUTURE or [5.1.0], is not a sentence.
        body = ID_PREFIX.sub("", body, count=1)
        body = re.sub(r"^(?:FUTURE\.|\[[0-9.]+\])\s*", "", body)
        if body:
            paragraph.append((no, body))
    flush()
    return problems


def main(argv):
    check_tables = True
    args = []
    for arg in argv:
        if arg == "--no-tables":
            check_tables = False
        elif arg in ("-h", "--help"):
            print(__doc__)
            return 0
        else:
            args.append(arg)
    if not args:
        print(__doc__, file=sys.stderr)
        return 2
    total = 0
    for spec in args:
        problems = check_file(spec, check_tables)
        for line in problems:
            print(line)
        total += len(problems)
        print(f"== {spec}: {len(problems)} problems")
    print(f"TOTAL {total} problems")
    return 1 if total else 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
