#!/usr/bin/env python3
"""The scenarios of tools/dev/interop-dash.sh. They run in a throwaway container (python:3.13-alpine) on the network of the stack.

The portal (the PDP and the gate) and ClickHouse Dash (the PEP) are the real products. The driver plays three roles:
a superadmin, a person (alice) who owns agent keys, and an agent that calls the MCP endpoint of Dash through the edge.
Each scenario prints its checks. The driver prints a PASS/FAIL table at the end and exits with 0 only when every scenario passes.

Standard library only. Environment: X13_ONLY (for example "5,6") runs only these scenarios.
"""
import base64
import http.client
import json
import os
import re
import sys
import threading
import time
import traceback
import urllib.parse

EDGE = ("traefik", 80)
PORTAL = ("portal", 4181)
PDP = ("tap", 4182)
SERVICE = "clickhouse-dash"
UA = "x13-driver/1"
CREDS = "/run/creds"
TAP = "/tap/tap.jsonl"
DASH_LOG = "/logs/dash.log"
HOST_LOCAL = "local"


def read_cred(name):
    with open(os.path.join(CREDS, name), encoding="utf-8") as f:
        return f.read().strip()


# ---------------------------------------------------------------- HTTP
def http_call(addr, method, path, headers=None, body=None, jar=None, timeout=60):
    """One HTTP request with no redirect. `jar` is a dict of cookies that the call reads and updates."""
    conn = http.client.HTTPConnection(addr[0], addr[1], timeout=timeout)
    hdr = {"User-Agent": UA}
    hdr.update(headers or {})
    if jar:
        hdr["Cookie"] = "; ".join(f"{k}={v}" for k, v in jar.items())
    if isinstance(body, (dict, list)):
        body = json.dumps(body).encode()
        hdr.setdefault("Content-Type", "application/json")
    elif isinstance(body, str):
        body = body.encode()
    conn.request(method, path, body=body, headers=hdr)
    resp = conn.getresponse()
    data = resp.read()
    out_headers = {k.lower(): v for k, v in resp.getheaders()}
    if jar is not None:
        for k, v in resp.getheaders():
            if k.lower() == "set-cookie":
                first = v.split(";", 1)[0]
                name, _, value = first.partition("=")
                if "max-age=0" in v.lower() or "expires=thu, 01 jan 1970" in v.lower():
                    jar.pop(name, None)
                else:
                    jar[name] = value
    conn.close()
    return resp.status, out_headers, data


def as_json(data):
    try:
        return json.loads(data)
    except ValueError:
        return None


def field(page, name):
    """The value of the hidden field `name`: the attributes of a tag come in any order."""
    for tag in re.findall(r"<input[^>]*>", page):
        if re.search(r'name="%s"' % re.escape(name), tag):
            m = re.search(r'value="([^"]*)"', tag)
            return m.group(1) if m else ""
    return ""


def form_csrf(page, action):
    m = re.search(r'action="%s"[^>]*>\s*<input type="hidden" name="csrf" value="([^"]*)"' % re.escape(action), page)
    return m.group(1) if m else ""


class Person:
    """A signed-in browser session of the portal, reached through the edge."""

    def __init__(self, login, password):
        self.login = login
        self.jar = {}
        self.origin = "http://%s" % EDGE[0]
        status, _, page = self.get("/auth/login")
        page = page.decode()
        data = {k: field(page, k) for k in ("csrf", "sig", "rd", "token")}
        time.sleep(1.3)  # the form has a minimum time between its display and its submit
        data.update(login=login, password=password)
        status, h, body = self.post("/auth/login", data)
        if status != 303:
            raise RuntimeError("sign-in of %s gave %s: %s" % (login, status, body[:200]))

    def get(self, path, headers=None):
        return http_call(EDGE, "GET", path, headers, jar=self.jar)

    def post(self, path, data, headers=None):
        hdr = {"Origin": self.origin, "Content-Type": "application/x-www-form-urlencoded"}
        hdr.update(headers or {})
        pairs = data if isinstance(data, list) else list(data.items())
        return http_call(EDGE, "POST", path, hdr, urllib.parse.urlencode(pairs), jar=self.jar)

    def post_json(self, path, doc):
        return http_call(EDGE, "POST", path, {"Content-Type": "application/json"}, doc, jar=self.jar)

    def page(self, path):
        status, _, body = self.get(path)
        return status, body.decode(errors="replace")


# ---------------------------------------------------------------- the portal pages (the superadmin and the owners)
def add_user(admin, login, password):
    status, page = admin.page("/auth/settings/new-user")
    csrf = form_csrf(page, "/auth/users/add")
    s, h, b = admin.post("/auth/users/add", {"csrf": csrf, "login": login, "password": password})
    return s, h.get("location", "")


def grants_form(grants):
    """`grants` is a list of (text, limits dict, confirm). The fields follow the editor of the settings pages."""
    pairs = []
    for i, (text, limits, confirm) in enumerate(grants):
        pairs.append(("grant%d" % i, text))
        for name, value in (limits or {}).items():
            pairs.append(("limit%d.%s" % (i, name), str(value)))
        if confirm:
            pairs.append(("confirm%d" % i, "1"))
    return pairs


def set_user_grants(admin, login, grants):
    status, page = admin.page("/auth/settings/users/%s" % login)
    csrf = form_csrf(page, "/auth/users/update")
    pairs = [("csrf", csrf), ("login", login), ("grants", "1")] + grants_form(grants)
    s, h, b = admin.post("/auth/users/update", pairs)
    return s, h.get("location", ""), b.decode(errors="replace")


def create_key(owner, name, grants, kind="mcp"):
    """Makes a key and returns (status, key text or None, page)."""
    status, page = owner.page("/auth/keys/new?kind=%s" % kind)
    csrf = form_csrf(page, "/auth/keys/create")
    pairs = [("csrf", csrf), ("kind", kind), ("name", name), ("expires_at", "")] + grants_form(grants)
    s, h, b = owner.post("/auth/keys/create", pairs)
    CTX["last_location"] = h.get("location", "")
    text = b.decode(errors="replace")
    m = re.search(r'id="key-text"[^>]*value="(%s_[A-Za-z0-9]+)"' % kind, text)
    if not m:
        m = re.search(r'value="(%s_[A-Za-z0-9]{40,})"' % kind, text)
    return s, (m.group(1) if m else None), text


# ---------------------------------------------------------------- the agent (MCP through the edge)
class Agent:
    def __init__(self, key):
        self.key = key
        self.n = 0

    def raw(self, method, path, body=None, headers=None):
        hdr = {"Authorization": "Bearer " + self.key, "Accept": "application/json"}
        hdr.update(headers or {})
        return http_call(EDGE, method, path, hdr, body)

    def rpc(self, method, params=None):
        self.n += 1
        msg = {"jsonrpc": "2.0", "id": self.n, "method": method}
        if params is not None:
            msg["params"] = params
        status, h, body = self.raw("POST", "/dash/mcp", msg)
        return status, as_json(body), body

    def call(self, tool, arguments):
        status, doc, raw = self.rpc("tools/call", {"name": tool, "arguments": arguments})
        result = (doc or {}).get("result")
        structured = (result or {}).get("structuredContent") if isinstance(result, dict) else None
        return Result(status, doc, structured, raw)

    def tools(self):
        status, doc, raw = self.rpc("tools/list")
        names = [t["name"] for t in ((doc or {}).get("result") or {}).get("tools", [])]
        return status, names, doc


class Result:
    def __init__(self, status, doc, structured, raw):
        self.status = status
        self.doc = doc
        self.structured = structured
        self.raw = raw

    @property
    def error(self):
        """The tool error code, or the JSON-RPC error code."""
        if isinstance(self.structured, dict) and isinstance(self.structured.get("error"), dict):
            return self.structured["error"].get("code")
        err = (self.doc or {}).get("error")
        if isinstance(err, dict):
            return err.get("code")
        return None

    @property
    def action(self):
        if isinstance(self.structured, dict):
            return self.structured.get("action")
        return None

    @property
    def awaiting(self):
        a = self.action
        return bool(a) and a.get("status") == "awaiting_approval" and a.get("executed") is False

    @property
    def ran(self):
        return self.status == 200 and isinstance(self.structured, dict) and "action" not in self.structured \
            and "error" not in self.structured and not (self.doc or {}).get("error") \
            and not (((self.doc or {}).get("result") or {}).get("isError"))

    def brief(self):
        text = self.raw.decode(errors="replace") if isinstance(self.raw, bytes) else str(self.raw)
        return "HTTP %s %s" % (self.status, text[:300])


# ---------------------------------------------------------------- the approval on the web page
def open_items(owner):
    """The requests that wait for the owner: [(item id, page text)] from the panel of the page Inbox."""
    status, page = owner.page("/auth/settings/inbox")
    ids = sorted(set(re.findall(r'/auth/settings/inbox/items/([0-9A-Za-z_-]+)', page)))
    return page, ids


def item_page(owner, item):
    return owner.page("/auth/settings/inbox/items/%s" % item)


def answer_item(owner, item, decision, comment=None):
    """Approve or decline on the page of the request. Returns (status, location, body)."""
    status, page = item_page(owner, item)
    if status != 200:
        return status, "", page
    # The form of the answer: the hidden fields of the form (csrf, request_id, shown_effect), then the button that the person presses.
    m = re.search(r'<form[^>]*action="/auth/settings/inbox/items/answer"[^>]*>(.*?)</form>', page, re.S)
    if not m:
        return 0, "", "no form of the answer on the page"
    pairs = []
    for tag in re.findall(r"<input[^>]*>", m.group(1)):
        name = re.search(r'name="([^"]*)"', tag)
        value = re.search(r'value="([^"]*)"', tag)
        if name and 'type="hidden"' in tag:
            pairs.append((name.group(1), value.group(1) if value else ""))
    pairs.append(("action", decision))  # "accept" or "decline"
    if comment:
        pairs.append(("text", comment))
    s, h, b = owner.post("/auth/settings/inbox/items/answer", pairs)
    return s, h.get("location", ""), b.decode(errors="replace")


# ---------------------------------------------------------------- logs
def tap_lines():
    out = []
    try:
        with open(TAP, encoding="utf-8") as f:
            for line in f:
                try:
                    out.append(json.loads(line))
                except ValueError:
                    pass
    except OSError:
        pass
    return out


def dash_log():
    try:
        with open(DASH_LOG, encoding="utf-8", errors="replace") as f:
            return f.read()
    except OSError:
        return ""


# ---------------------------------------------------------------- the report
class Scenario:
    def __init__(self, num, title):
        self.num = num
        self.title = title
        self.checks = []  # (ok, text, evidence)
        self.notes = []

    def check(self, cond, text, evidence=""):
        self.checks.append((bool(cond), text, str(evidence)[:1500] if not cond else ""))
        print("   [%s] %s%s" % ("ok" if cond else "FAIL", text, "" if cond or not evidence else "\n        evidence: %s" % str(evidence)[:600]))
        return bool(cond)

    def note(self, text):
        self.notes.append(text)
        print("   note: " + text)

    @property
    def status(self):
        if not self.checks:
            return "FAIL"
        return "PASS" if all(c[0] for c in self.checks) else "FAIL"


SCENARIOS = []


def scenario(num, title):
    def deco(fn):
        SCENARIOS.append((num, title, fn))
        return fn
    return deco


# ---------------------------------------------------------------- shared setup
S = "service:" + SERVICE
RUN = str(int(time.time()))[-6:]
CTX = {}
SQL_COUNT = "SELECT count() AS c FROM x13_data.events"
DASH_ACTIONS_PUBLISHED = {"query.run.read", "query.export", "library.read", "explorer.read", "system.read",
                          "mcp.hosts.read", "mcp.analysis.read", "mcp.session"}
DASH_ACTIONS_ABSENT = {"query.run.write", "library.write", "mcp.keys.read", "mcp.keys.write", "mcp.usage.read",
                       "observability.read"}


def base_grants():
    """What a key needs to reach Dash: the gate and the MCP session. The other grants come from the scenario."""
    return [("access on " + S, {}, False), ("mcp.session on " + S, {}, False)]


def owner_grants(extra=()):
    """The grants of an account that owns keys: every action that Dash publishes, and the rights on MCP keys."""
    g = [("access on " + S, {}, False)]
    g += [("%s on %s" % (a, S), {}, False) for a in sorted(DASH_ACTIONS_PUBLISHED)]
    g += [("keys.own.read on service:portal", {}, False),
          ("keys.own.create on service:portal/key_kind:mcp", {}, False),
          ("keys.own.use on service:portal/key_kind:mcp", {}, False)]
    return g + list(extra)


def editor_model(admin, login):
    """The data of the grant editor on the page of an account, as the portal builds it from the documents of the services."""
    st, page = admin.page("/auth/settings/users/%s" % login)
    m = re.search(r'<script[^>]*class="grantEditor__data"[^>]*>(.*?)</script>', page, re.S)
    return json.loads(__import__("html").unescape(m.group(1))) if m else {}


def wait_for_document(admin, login, seconds=90):
    """The portal reads the document of Dash after the discovery and after access.actions_refresh (10 s in this stack)."""
    began = time.time()
    while time.time() - began < seconds:
        svc = next((x for x in editor_model(admin, login).get("services", []) if x.get("id") == SERVICE), {})
        if svc.get("source", {}).get("state") == "fresh":
            return time.time() - began
        time.sleep(2)
    return None


def setup():
    admin = Person("root", read_cred("superadmin_password"))
    CTX["admin"] = admin
    for login in ("alice", "dave", "erin", "carol"):
        # The passwords are fixed: the data of the stack is throwaway, and a second run can reuse a kept stack.
        pw = "interop-%s-password-x13" % login
        CTX[login + "_pw"] = pw
        s, loc = add_user(admin, login, pw)
        if s != 303 or not ("user-added" in loc or "exists" in loc):
            raise RuntimeError("cannot add the account %s: %s %s" % (login, s, loc))
    waited = wait_for_document(admin, "alice")
    print("   the portal read the document of Dash (waited %s s)" % (None if waited is None else round(waited)))
    # carol has no grant on purpose.
    for login in ("alice", "dave", "erin"):
        s, loc, _ = set_user_grants(admin, login, owner_grants())
        if "user-updated" not in loc and "unchanged" not in loc:
            raise RuntimeError("cannot give the grants of %s: %s %s" % (login, s, loc))
    CTX["alice"] = Person("alice", CTX["alice_pw"])


def new_key(owner, name, grants):
    s, key, page = create_key(owner, "%s-%s" % (name, RUN), grants)
    if not key:
        text = re.sub(r"\s+", " ", re.sub(r"<[^>]+>", " ", re.sub(r"<script.*?</script>", "", page, flags=re.S)))
        raise RuntimeError("no key made (%s): %s" % (s, text[:300]))
    return Agent(key)


def tap_since(t0, path="/access/v1/evaluation"):
    return [l for l in tap_lines() if l["t"] >= t0 and l["path"] == path]


def question(line):
    """(action, context, decision, response context) of one single question in the tap."""
    r = line["request"] or {}
    resp = line["response"] or {}
    return (r.get("action", {}).get("name"), r.get("context") or {}, resp.get("decision"), resp.get("context") or {})


def fmt(r):
    return r.brief()


# ---------------------------------------------------------------- the scenarios
@scenario(1, "The actions document of Dash in the portal")
def s1(sc):
    st, h, b = http_call(("dash", 8080), "GET", "/.well-known/procurauth/actions")
    doc = as_json(b) or {}
    sc.check(st == 200 and doc.get("server") == SERVICE, "Dash publishes its document with server=%s" % SERVICE, b[:300])
    published = {a["name"] for a in doc.get("actions", [])}
    sc.check(published == DASH_ACTIONS_PUBLISHED, "the document lists the 8 actions of this configuration", sorted(published ^ DASH_ACTIONS_PUBLISHED))
    sc.check(not (published & DASH_ACTIONS_ABSENT), "the document lists no query.run.write, library.write, mcp.keys.*, mcp.usage.read or observability.read", sorted(published & DASH_ACTIONS_ABSENT))
    st, page = CTX["admin"].page("/auth/settings/users/alice")
    m = re.search(r'<script[^>]*class="grantEditor__data"[^>]*>(.*?)</script>', page, re.S)
    model = json.loads(__import__("html").unescape(m.group(1))) if m else {}
    svc = next((x for x in model.get("services", []) if x.get("id") == SERVICE), {})
    sc.check(bool(svc), "the grant editor of the portal knows the service %s" % SERVICE, [x.get("id") for x in model.get("services", [])])
    names = {a["name"] for a in svc.get("actions", [])} - {"access"}
    sc.check(names == DASH_ACTIONS_PUBLISHED, "the editor shows exactly the published actions (no read error, no absent action)", sorted(names ^ DASH_ACTIONS_PUBLISHED))
    ceilings_doc = {a["name"]: sorted(l for l in a.get("limits", []) if l not in ("queries", "exports", "requests")) for a in doc.get("actions", [])}
    ceilings_ed = {a["name"]: sorted(l["name"] for l in a.get("limits", []) if not l.get("budget")) for a in svc.get("actions", []) if a["name"] != "access"}
    sc.check(ceilings_doc == ceilings_ed, "the editor shows the ceilings that each action declares", (ceilings_doc, ceilings_ed))
    mem = next((l for a in svc.get("actions", []) if a["name"] == "query.run.read" for l in a.get("limits", []) if l["name"] == "max_memory"), {})
    memdoc = next((l for l in doc.get("limits", []) if l["name"] == "max_memory"), {})
    sc.check(mem.get("range") == [memdoc.get("min"), memdoc.get("max")] and memdoc.get("max", 0) > 10**9, "the range of max_memory in the editor is the range of the document (%s)" % mem.get("range"), (mem, memdoc))
    budgets_doc = sorted(l["name"] for l in doc.get("limits", []) if l.get("kind") == "budget")
    budgets_ed = sorted(l["name"] for a in svc.get("actions", []) if a["name"] == "access" for l in a.get("limits", []) if l.get("budget"))
    sc.check(budgets_doc == budgets_ed == ["exports", "queries", "requests"], "the editor offers the three budgets of Dash (queries, exports, requests)", (budgets_doc, budgets_ed))
    sc.check(svc.get("source", {}).get("state") == "fresh", "the document is fresh in the portal (no read error)", svc.get("source"))
    sc.check([t["name"] for t in svc.get("types", [])] == ["host", "database"], "the resource types host and database come through", svc.get("types"))
    hosts = [n for n in svc.get("nodes", []) if n.get("kind") == "host"]
    sc.check(len(hosts) == len(doc.get("resources", [])) == 32, "the editor lists the %d hosts of the document" % len(doc.get("resources", [])), (len(hosts), len(doc.get("resources", []))))
    st, page = CTX["admin"].page("/auth/settings/services")
    text = re.sub(r"\s+", " ", re.sub(r"<[^>]+>", " ", page))
    sc.check(st == 200 and "Last read" in text, "the page Services shows the last read of Dash", text[:500])
    st, h, b = http_call(PORTAL, "GET", "/metrics")
    m = re.search(r"portal_actions_age_seconds ([0-9.]+)", b.decode())
    sc.check(m and float(m.group(1)) < 60, "portal_actions_age_seconds is small", m.group(0) if m else b[:200])
    CTX["model"] = model


@scenario(2, "Metadata of the PDP and a large batch of questions")
def s2(sc):
    st, h, b = http_call(PDP, "GET", "/.well-known/authzen-configuration")
    meta = as_json(b) or {}
    expected = {"procurauth_idle_max_seconds": 10, "procurauth_evaluations_max_bytes": 65536, "procurauth_evaluations_max_items": 128,
                "procurauth_usage_max_seconds": 5}
    for k, v in expected.items():
        sc.check(meta.get(k) == v, "the portal publishes %s = %s" % (k, v), meta.get(k))
    approval = sorted(k for k in meta if k.startswith("procurauth_approval_"))
    sc.check(len(approval) == 12, "the portal publishes the 12 members procurauth_approval_*", approval)
    log = dash_log()
    line = next((l for l in log.splitlines() if '"access.pdp_metadata"' in l), "")
    sc.check(bool(line), "Dash logs access.pdp_metadata", log[:300])
    if line:
        d = as_json(line.encode()) or {}
        sc.check((d.get("evaluations_max_items"), d.get("evaluations_max_bytes"), d.get("usage_max_seconds"), d.get("idle_max_seconds")) == (128, 65536, 5, 10),
                 "the line of Dash has the four values of the portal", line[:300])
    sc.check('"access.pdp_metadata_failed"' not in log, "Dash logs no access.pdp_metadata_failed", [l for l in log.splitlines() if "metadata_failed" in l][:2])
    # A page that needs many questions: the navigation batch of 32 hosts.
    alice = Person("alice", CTX["alice_pw"])
    t0 = time.time()
    st1, p1 = alice.page("/dash/api/version")
    st2, p2 = alice.page("/dash/api/hosts")
    st3, p3 = alice.page("/dash/")
    sc.check(st1 == 200 and st2 == 200 and st3 == 200, "the pages /api/version, /api/hosts and / answer 200 for a person", (st1, st2, st3, p1[:200], p2[:200]))
    batches = tap_since(t0, "/access/v1/evaluations")
    sizes = [len((l["request"] or {}).get("evaluations", [])) for l in batches]
    sc.check(sizes and max(sizes) > 64, "a batch of more than 64 questions reached the PDP (largest: %s)" % (max(sizes) if sizes else 0), sizes)
    bad = [(l["status"], len((l["request"] or {}).get("evaluations", []))) for l in batches if l["status"] != 200]
    sc.check(not bad, "no batch got a 400 or a 503 (%d batches, sizes %s)" % (len(batches), sorted(set(sizes), reverse=True)[:4]), bad)
    hosts = (as_json(p2.encode()) or {})
    count = len(hosts.get("hosts", hosts if isinstance(hosts, list) else []))
    sc.check(count >= 32, "alice sees all the hosts in /api/hosts (%d)" % count, p2[:300])
    log = dash_log()
    sc.check('"access.page_unavailable"' not in log and '"event":"access.pdp_unavailable"' not in log, "Dash logs no page_unavailable", [l for l in log.splitlines() if "unavailable" in l][:2])


@scenario(3, "A person signs in, sees the tile and opens a page of Dash through the gate")
def s3(sc):
    st, h, b = http_call(EDGE, "GET", "/dash/api/version")
    sc.check(st in (301, 302, 303, 401, 403) and st != 200, "without a session the gate refuses the page (HTTP %s)" % st, (st, h.get("location"), b[:100]))
    alice = Person("alice", CTX["alice_pw"])
    st, home = alice.page("/")
    sc.check(st == 200 and "ClickHouse Dash" in home and 'href="/dash/"' in home, "alice sees the tile ClickHouse Dash on the launcher", home[:300])
    st, page = alice.page("/dash/")
    sc.check(st == 200 and "<html" in page.lower(), "alice opens /dash/ through the gate (HTTP %s)" % st, page[:300])
    st, h, b = alice.get("/dash/api/version")
    doc = as_json(b) or {}
    sc.check(st == 200 and bool(doc), "Dash accepts the identity token on /api/version (HTTP %s)" % st, b[:300])
    log = dash_log()
    sc.check('"subject":"alice"' in log, "the access log of Dash names the subject alice (from the identity token)", log[-400:])
    carol = Person("carol", CTX["carol_pw"])
    st, h, b = carol.get("/dash/api/version")
    sc.check(st != 200, "carol (no grant) does not pass the gate (HTTP %s)" % st, (st, h.get("location"), b[:100]))
    st, home = carol.page("/")
    sc.check(st == 200 and "ClickHouse Dash" not in home, "carol sees no tile", home[:200])


@scenario(4, "An agent key calls tools/list and run_query")
def s4(sc):
    agent = new_key(CTX["alice"], "plain", base_grants() + [("mcp.hosts.read on " + S, {}, False), ("query.run.read on " + S, {}, False), ("query.export on " + S, {}, False)])
    st, doc, raw = agent.rpc("initialize", {"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "x13", "version": "1"}})
    sc.check(st == 200 and "result" in (doc or {}), "initialize works through the key route (mcp.session)", raw[:200])
    st, names, doc = agent.tools()
    sc.check(st == 200 and {"list_hosts", "run_query", "export_query"} <= set(names), "tools/list has list_hosts, run_query and export_query", names)
    sc.check("run_write_query" not in names and "library_write" not in names, "tools/list has no write tool (no mcp_write_uri, read-only library)", names)
    r = agent.call("run_query", {"host": HOST_LOCAL, "sql": SQL_COUNT})
    text = json.dumps(r.structured)
    sc.check(r.ran and '"1000"' in text, "run_query with no confirm runs and gives 1000 rows counted", fmt(r))
    sc.check(agent.raw("POST", "/dash/mcp", {"jsonrpc": "2.0", "id": 9, "method": "tools/list"}, {"Authorization": "Bearer mcp_" + "A" * 49})[0] == 401,
             "a bad key gets 401 at the edge")


def awaiting_call(agent, args, tool="run_query", lease=30):
    a = dict(args)
    a["lease_s"] = lease
    return agent.call(tool, a)


@scenario(5, "Approval of a call: approve once, deny, lapse")
def s5(sc):
    alice = CTX["alice"]
    agent = new_key(alice, "confirm", base_grants() + [("query.run.read on " + S, {}, True)])
    args = {"host": HOST_LOCAL, "sql": SQL_COUNT}
    r = agent.call("run_query", args)
    sc.check(r.error == "invalid_argument", "without a control argument the call gets invalid_argument (the PDP asked for an approval)", fmt(r))
    r = awaiting_call(agent, args, lease=30)
    sc.check(r.awaiting and r.action.get("id") and r.action.get("code"), "with lease_s the call is NOT executed: awaiting_approval with an id and a code", fmt(r))
    rid = (r.action or {}).get("id")
    _, ids = open_items(alice)
    sc.check(rid in ids, "the request is on the page Inbox of the owner", (rid, ids))
    st, page = item_page(alice, rid)
    sc.check(st == 200 and "x13_data.events" in page and "query.run.read" in page, "the page of the request shows the SQL and the action", page[:200])
    st = agent.call("get_action_status", {"request_id": rid})
    sc.check((st.structured or {}).get("status") == "pending", "get_action_status says pending", fmt(st))
    s_, loc, body = answer_item(alice, rid, "accept")
    sc.check(s_ == 303 and "approved" in loc, "the owner approves on the web page", (s_, loc, body[:200]))
    st = agent.call("get_action_status", {"request_id": rid})
    sc.check((st.structured or {}).get("status") == "approved", "get_action_status says approved", fmt(st))
    r1 = awaiting_call(agent, args)
    sc.check(r1.ran and '"1000"' in json.dumps(r1.structured), "the same call runs once", fmt(r1))
    r2 = awaiting_call(agent, args)
    sc.check(r2.awaiting and (r2.action or {}).get("id") != rid, "a second retry asks again (the approval is consumed)", fmt(r2))
    sc.note("The retry must repeat a control argument (lease_s): without it Dash answers invalid_argument before the approval is looked at.")
    # Deny path.
    args2 = {"host": HOST_LOCAL, "sql": "SELECT 2 AS deny_case"}
    r = awaiting_call(agent, args2)
    rid2 = (r.action or {}).get("id")
    sc.check(r.awaiting, "a new call asks for an approval", fmt(r))
    s_, loc, body = answer_item(alice, rid2, "decline")
    sc.check(s_ == 303 and "declined" in loc, "the owner declines", (s_, loc, body[:200]))
    st = agent.call("get_action_status", {"request_id": rid2})
    sc.check((st.structured or {}).get("status") == "denied", "get_action_status says denied", fmt(st))
    r = awaiting_call(agent, args2)
    sc.check(r.error == "approval_denied", "the same call gets the tool error approval_denied (cooldown)", fmt(r))
    # One call that waits: wait_s makes Dash hold the call, the owner approves, the call runs in the same request.
    args5 = {"host": HOST_LOCAL, "sql": "SELECT 5 AS wait_case"}
    known = set(open_items(alice)[1])
    box = {}

    def approve_later():
        for _ in range(40):
            time.sleep(0.25)
            new = [i for i in open_items(alice)[1] if i not in known]
            if new:
                time.sleep(1.0)
                box["answer"] = answer_item(alice, new[0], "accept")
                return

    th = threading.Thread(target=approve_later)
    th.start()
    began = time.time()
    r = agent.call("run_query", dict(args5, lease_s=30, wait_s=15))
    took = time.time() - began
    th.join()
    sc.check(box.get("answer", (0, ""))[0] == 303, "the owner approves while the call waits", box.get("answer"))
    sc.check(r.ran, "with wait_s the call runs in the same request after the approval (%.1f s)" % took, fmt(r))
    # Void path: approved, then not used before consume_before (8 s).
    args3 = {"host": HOST_LOCAL, "sql": "SELECT 3 AS lapse_case"}
    r = awaiting_call(agent, args3, lease=5)
    rid3 = (r.action or {}).get("id")
    s_, loc, body = answer_item(alice, rid3, "accept")
    sc.check(s_ == 303 and "approved" in loc, "the owner approves a third call", (s_, loc))
    time.sleep(10)
    st = agent.call("get_action_status", {"request_id": rid3})
    sc.check((st.structured or {}).get("status") == "void", "an approval that nobody used before consume_before is void", fmt(st))
    r = awaiting_call(agent, args3, lease=5)
    sc.check(r.awaiting and (r.action or {}).get("id") != rid3, "the lapsed approval does not run the call: it asks again", fmt(r))
    # Abandoned: the lease ends and nobody renews it.
    rid4 = (r.action or {}).get("id")
    time.sleep(12)
    r = awaiting_call(agent, args3, lease=5)
    sc.check(r.awaiting and not r.ran, "after the lease the call is still not executed", fmt(r))
    st = agent.call("get_action_status", {"request_id": rid4})
    sc.check((st.structured or {}).get("status") in ("void", "pending"), "the old request is void (or revived by the read: %s)" % (st.structured,), fmt(st))


@scenario(6, "Several permissions: export_query = query.export + query.run.read (Q53)")
def s6(sc):
    alice = CTX["alice"]
    # a) confirm on query.run.read only.
    a = new_key(alice, "q53a", base_grants() + [("query.run.read on " + S, {}, True), ("query.export on " + S, {}, False)])
    args = {"host": HOST_LOCAL, "sql": "SELECT number FROM system.numbers LIMIT 5", "format": "csv"}
    t0 = time.time()
    before = open_items(alice)[1]
    r = a.call("export_query", args)
    sc.check(r.ran, "confirm only on query.run.read: export_query runs and asks NO approval", fmt(r))
    after = open_items(alice)[1]
    sc.check(after == before, "no request was opened for the export", (before, after))
    qs = [question(l) for l in tap_since(t0)]
    po = [q for q in qs if q[0] == "query.run.read" and q[1].get("permission_only") is True]
    sc.check(po and all("usage" not in q[1] and "approval" not in q[1] for q in po) and all(q[2] is True and "reason" not in q[3] for q in po),
             "the PDP got the question of query.run.read with context.permission_only=true, no usage, no effect, and answered true with no reason", qs)
    sc.check(any(q[0] == "query.export" and "usage" in q[1] and q[2] is True for q in qs), "the use of query.export carried usage and got true", qs)
    subject = ((tap_since(t0) or [{}])[0].get("request") or {}).get("subject", {}).get("id", "")
    dlog = [as_json(x.encode()) or {} for x in dash_log().splitlines() if '"access.decision"' in x and subject and subject in x]
    plain = [d for d in dlog if d.get("action") == "query.run.read" and not d.get("usage") and not d.get("approval") and d.get("decision") is True and not d.get("reason")]
    sc.check(bool(plain), "the Dash log has the access.decision of query.run.read with no usage, no effect, true (the log has no field permission_only: the tap shows it)", dlog[:3])
    # b) confirm on query.export.
    b = new_key(alice, "q53b", base_grants() + [("query.run.read on " + S, {}, False), ("query.export on " + S, {}, True)])
    t0 = time.time()
    r = awaiting_call(b, args, tool="export_query")
    rid = (r.action or {}).get("id")
    sc.check(r.awaiting, "confirm on query.export: export_query is NOT executed, awaiting_approval", fmt(r))
    qs = [question(l) for l in tap_since(t0)]
    sc.check(any(q[0] == "query.export" and q[3].get("reason") == "approval_required" for q in qs), "the PDP answered approval_required for query.export", qs)
    sc.check(any(q[0] == "query.run.read" and q[1].get("permission_only") is True and q[2] is True and "reason" not in q[3] for q in qs),
             "the question of query.run.read has permission_only and no reason", qs)
    s_, loc, body = answer_item(alice, rid, "accept")
    sc.check(s_ == 303 and "approved" in loc, "the owner approves the export", (s_, loc))
    r1 = awaiting_call(b, args, tool="export_query")
    sc.check(r1.ran, "the same export runs after the approval", fmt(r1))
    r2 = awaiting_call(b, args, tool="export_query")
    sc.check(r2.awaiting, "the next export asks again (the approval is consumed)", fmt(r2))
    # c) confirm on both: the approval is for query.export only.
    c = new_key(alice, "q53c", base_grants() + [("query.run.read on " + S, {}, True), ("query.export on " + S, {}, True)])
    r = awaiting_call(c, args, tool="export_query")
    sc.check(r.awaiting, "confirm on both: export_query asks (for query.export)", fmt(r))
    r = awaiting_call(c, {"host": HOST_LOCAL, "sql": "SELECT 1"}, tool="run_query")
    sc.check(r.awaiting, "confirm on query.run.read still asks for run_query (the tool that uses it)", fmt(r))


@scenario(7, "The own confirm of a person (Q51)")
def s7(sc):
    admin = CTX["admin"]
    grants = [(t, l, (c or (t.startswith("query.run.read on")))) for (t, l, c) in owner_grants()]
    s_, loc, body = set_user_grants(admin, "dave", grants)
    sc.check("user-updated" in loc, "the portal accepts confirm on query.run.read in the grants of a person", (s_, loc, body[:200]))
    dave = Person("dave", CTX["dave_pw"])
    t0 = time.time()
    st, h, b = dave.get("/dash/api/meta?host_id=%s" % HOST_LOCAL)
    sc.check(st == 200, "dave uses a page that needs query.run.read: HTTP 200, not blocked (his own confirm is ignored)", (st, b[:300]))
    qs = [question(l) for l in tap_since(t0)]
    sc.check(any(q[0] == "query.run.read" and q[2] is True and "reason" not in q[3] for q in qs), "the PDP answered true with no reason for his own question", qs)
    st, h, b = dave.post_json("/dash/api/query/run", {"host_id": HOST_LOCAL, "sql": "SELECT 1"})
    sc.check(st in (200, 201, 202), "dave runs a statement on the Query page (HTTP %s)" % st, b[:300])
    # The same grant on his key asks.
    agent = new_key(dave, "own", base_grants() + [("query.run.read on " + S, {}, False)])
    r = awaiting_call(agent, {"host": HOST_LOCAL, "sql": SQL_COUNT})
    sc.check(r.awaiting, "the key of dave asks for an approval, because the grant of its owner has confirm", fmt(r))
    s_, loc, body = answer_item(dave, (r.action or {}).get("id", ""), "accept")
    sc.check(s_ == 303 and "approved" in loc, "dave approves the request of his own key", (s_, loc))
    r = awaiting_call(agent, {"host": HOST_LOCAL, "sql": SQL_COUNT})
    sc.check(r.ran, "the key runs after the approval", fmt(r))


@scenario(8, "confirm on mcp.session is refused, and no tool uses mcp.session")
def s8(sc):
    admin, alice = CTX["admin"], CTX["alice"]
    # Control: the same grants of an account with no confirm are accepted.
    s_, loc, body = set_user_grants(admin, "erin", owner_grants())
    sc.check("user-updated" in loc or "unchanged" in loc, "control: the grants of erin with no confirm are accepted (%s)" % loc, (s_, loc))
    grants = [(t, l, c or t.startswith("mcp.session on")) for (t, l, c) in owner_grants()]
    s_, loc, body = set_user_grants(admin, "erin", grants)
    sc.check("user-updated" not in loc and "unchanged" not in loc, "the form of an account refuses confirm on mcp.session (%s)" % (loc or s_), (s_, loc, body[:200]))
    s_, key, page = create_key(alice, "bad-" + RUN, [("access on " + S, {}, False), ("mcp.session on " + S, {}, True)])
    loc = CTX.get("last_location", "")
    text = re.sub(r"\s+", " ", re.sub(r"<[^>]+>", " ", re.sub(r"<script.*?</script>", "", page, flags=re.S)))
    sc.check(key is None, "the form of a key refuses confirm on mcp.session: no key is made (HTTP %s %s)" % (s_, loc), text[:300])
    sc.check("bad_confirm" in loc or "bad-confirm" in loc or "bad_confirm" in text or "confirm" in (loc + text).lower(), "the refusal names the lock confirm", (loc, text[:300]))
    model = editor_model(admin, "alice")
    svc = next((x for x in model.get("services", []) if x.get("id") == SERVICE), {})
    locks = {a["name"]: a.get("approval") for a in svc.get("actions", [])}
    sc.check(locks.get("mcp.session") is False and locks.get("access") is False, "the editor offers no lock on access and mcp.session", locks)
    sc.check(all(locks.get(n) for n in ("mcp.hosts.read", "mcp.analysis.read", "query.run.read", "query.export")), "the editor offers a lock on the other actions", locks)
    hcl = {}
    for name in ("hcl-confirm-mcp-session", "hcl-confirm-ok"):
        try:
            with open("/out/%s.txt" % name, encoding="utf-8") as f:
                hcl[name] = f.read()
        except OSError:
            hcl[name] = ""
    first = hcl["hcl-confirm-mcp-session"].splitlines()[:1]
    sc.check(first == ["exit=1"] and "mcp.session" in hcl["hcl-confirm-mcp-session"] and "confirm" in hcl["hcl-confirm-mcp-session"],
             "a file with confirm on mcp.session stops portal check-config", hcl["hcl-confirm-mcp-session"][:400])
    sc.check(hcl["hcl-confirm-ok"].splitlines()[:1] == ["exit=0"], "the same file with confirm on query.run.read is valid (control)", hcl["hcl-confirm-ok"][:400])
    # The tools use their own actions.
    agent = new_key(alice, "own-actions", base_grants() + [("mcp.hosts.read on " + S, {}, True), ("mcp.analysis.read on " + S, {}, True), ("query.run.read on " + S, {}, False)])
    for tool, action in (("list_hosts", "mcp.hosts.read"), ("mcp_analysis_my_limits", "mcp.analysis.read"), ("mcp_analysis_my_usage", "mcp.analysis.read")):
        t0 = time.time()
        r = awaiting_call(agent, {}, tool=tool)
        qs = [question(l) for l in tap_since(t0)]
        sc.check(r.awaiting, "%s asks for an approval through %s" % (tool, action), fmt(r))
        sc.check(qs and not any(q[0] == "mcp.session" for q in qs) and any(q[0] == action for q in qs), "the PDP got a question for %s and none for mcp.session" % action, [q[0] for q in qs])
    # confirm only on query.run.read: list_hosts runs freely, and initialize works.
    free = new_key(alice, "free-tools", base_grants() + [("mcp.hosts.read on " + S, {}, False), ("mcp.analysis.read on " + S, {}, False), ("query.run.read on " + S, {}, True)])
    r = free.call("list_hosts", {})
    sc.check(r.ran, "with confirm only on query.run.read, list_hosts runs with no approval", fmt(r))
    st, doc, raw = free.rpc("initialize", {"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "x13", "version": "1"}})
    sc.check(st == 200 and "result" in (doc or {}), "initialize (mcp.session) is never blocked by confirm on a tool", raw[:200])


@scenario(9, "A ceiling above 1e9")
def s9(sc):
    admin, alice = CTX["admin"], CTX["alice"]
    big = 2147483648
    grants = [(t, ({"max_memory": big} if t.startswith("query.run.read on") else l), c) for (t, l, c) in owner_grants()]
    s_, loc, body = set_user_grants(admin, "erin", grants)
    sc.check("user-updated" in loc, "the portal accepts max_memory = %d in the grants of an account" % big, (s_, loc, body[:300]))
    agent = new_key(alice, "ceiling", base_grants() + [("query.run.read on " + S, {"max_memory": big}, False)])
    marker = "x13_ceiling_" + RUN
    t0 = time.time()
    r = agent.call("run_query", {"host": HOST_LOCAL, "sql": "SELECT count() AS %s FROM x13_data.events" % marker})
    sc.check(r.ran, "the key with max_memory = %d runs run_query on host local" % big, fmt(r))
    qs = [(q, l) for l in tap_since(t0) for q in [question(l)]]
    got = [q[3].get("limits") for q, l in qs if q[0] == "query.run.read" and "usage" in q[1]]
    sc.check(got and got[0] and got[0].get("max_memory") == big, "the PDP answer carries limits.max_memory = %d" % big, got)
    pw = os.environ.get("X13_CH_ADMIN_PASSWORD", "")
    if pw:
        def ch(sql):
            st, h, b = http_call(("clickhouse", 8123), "POST", "/?user=x13admin&password=%s" % urllib.parse.quote(pw), body=sql)
            return st, b.decode().strip()
        ch("SYSTEM FLUSH LOGS")
        st, out = ch("SELECT Settings['max_memory_usage'] FROM system.query_log WHERE type = 'QueryFinish' AND query LIKE '%%%s%%' AND query NOT LIKE '%%system.query_log%%' ORDER BY event_time DESC LIMIT 1" % marker)
        sc.check(st == 200 and out == str(big), "ClickHouse ran the statement with max_memory_usage = %d (query_log)" % big, (st, out))
    else:
        sc.check(False, "the password of ClickHouse is missing: cannot read the query_log")
    # The host ro1 has readonly = 1: ClickHouse takes no setting, so Dash cannot enforce max_memory.
    t0 = time.time()
    r = agent.call("run_query", {"host": "ro1", "sql": SQL_COUNT})
    sc.note("host ro1 (readonly = 1) with max_memory: %s" % fmt(r)[:200])
    log = dash_log()
    sc.check(r.error == "ceiling_unenforceable" and not r.ran, "on the readonly = 1 host, Dash refuses the call with the tool error ceiling_unenforceable: it does not run", fmt(r))
    sc.check("access.ceiling_unenforceable" in log or "ceiling_unenforceable" in log, "the log of Dash names ceiling_unenforceable", [l[:200] for l in log.splitlines() if "ceiling" in l][:3])


@scenario(10, "A page route that needs an approval answers approval_cannot_wait")
def s10(sc):
    alice = CTX["alice"]
    agent = new_key(alice, "page", base_grants() + [("query.run.read on " + S, {}, True)])
    before = open_items(alice)[1]
    for path, method, body in (("/dash/api/meta?host_id=%s" % HOST_LOCAL, "GET", None),
                               ("/dash/api/query/run", "POST", {"host_id": HOST_LOCAL, "sql": "SELECT 1"})):
        st, h, b = agent.raw(method, path, body)
        doc = as_json(b)
        sc.check(st == 403 and doc == {"error": "approval_cannot_wait"}, "%s %s answers 403 {\"error\":\"approval_cannot_wait\"}" % (method, path.split("?")[0]), (st, b[:200]))
    after = open_items(alice)[1]
    sc.check(after == before, "no request is submitted for a page", (before, after))


@scenario(11, "Extra: the budget queries of Dash, counted by the portal")
def s11(sc):
    alice = CTX["alice"]
    agent = new_key(alice, "budget", [("access on " + S, {"queries.minute": 2}, False), ("mcp.session on " + S, {}, False), ("query.run.read on " + S, {}, False)])
    results = [agent.call("run_query", {"host": HOST_LOCAL, "sql": "SELECT %d AS budget_case" % i}) for i in range(3)]
    sc.check(results[0].ran and results[1].ran, "the first two calls run (budget of 2 each minute)", [fmt(r) for r in results[:2]])
    sc.check(results[2].error == "budget_exceeded" and not results[2].ran, "the third call gets the tool error budget_exceeded", fmt(results[2]))
    err = (results[2].structured or {}).get("error", {})
    sc.check(isinstance(err.get("retry_after"), int) and err.get("retry_after") >= 1 and err.get("retryable") is True, "the error has retry_after and retryable", err)


def main():
    only = {x for x in os.environ.get("X13_ONLY", "").split(",") if x.strip()}
    print("== setup")
    setup()
    done = []
    for num, title, fn in SCENARIOS:
        if only and str(num) not in only:
            continue
        sc = Scenario(num, title)
        print("== scenario %d: %s" % (num, title))
        try:
            fn(sc)
        except Exception:
            sc.check(False, "the scenario stopped with an exception", traceback.format_exc()[-900:])
        done.append(sc)
    if not only:
        sc = Scenario(12, "Wire summary of the whole run (the tap between Dash and the PDP)")
        lines = tap_lines()
        by = {}
        for l in lines:
            route = "/approval/v1/requests/{id}" if l["path"].startswith("/approval/v1/requests/") else l["path"].split("?")[0]
            by[(route, l["status"])] = by.get((route, l["status"]), 0) + 1
        sc.note("exchanges: " + ", ".join("%s %s x%d" % (k[0], k[1], v) for k, v in sorted(by.items())))
        sc.check(lines and not [l for l in lines if l["status"] >= 500], "no exchange between Dash and the PDP got a 5xx (%d exchanges)" % len(lines), [(l["path"], l["status"]) for l in lines if l["status"] >= 500][:5])
        sc.check(not [l for l in lines if l["status"] == 400], "no exchange got a 400", [(l["path"], l["status"], l["response"]) for l in lines if l["status"] == 400][:5])
        other = [(l["path"], l["status"], l["response"]) for l in lines if l["status"] >= 400]
        sc.note("4xx answers: %s" % other[:6])
        sc.check(not [l for l in lines if l["path"].startswith("/approval/") and l["status"] >= 400 and l["status"] not in (404, 429)], "the approval API gave no error other than the cases of the table", other[:6])
        done.append(sc)
    print()
    print("%-3s %-6s %-6s %s" % ("#", "Result", "Checks", "Scenario"))
    for sc in done:
        bad = sum(1 for c in sc.checks if not c[0])
        print("%-3s %-6s %-6s %s" % (sc.num, sc.status, "%d/%d" % (len(sc.checks) - bad, len(sc.checks)), sc.title))
    fails = [(sc, c) for sc in done for c in sc.checks if not c[0]]
    if fails:
        print()
        print("Failed checks:")
        for sc, c in fails:
            print("  scenario %d: %s" % (sc.num, c[1]))
            if c[2]:
                print("      evidence: %s" % c[2][:500])
    try:
        with open("/out/report.json", "w", encoding="utf-8") as f:
            json.dump([{"num": sc.num, "title": sc.title, "status": sc.status, "checks": sc.checks, "notes": sc.notes} for sc in done], f, indent=1)
    except OSError:
        pass
    sys.exit(0 if not fails else 1)


if __name__ == "__main__":
    main()
