"""Demo service "notes" for the portal. It is a policy enforcement point (PEP) of ProcurAuth.

The service trusts only the identity token of the portal (header X-Identity-Token). For each
operation it asks the PDP of the portal. It enforces the ceiling max_results, and it counts one use
of the budget requests. This file is for a local demo only.
"""

import base64
import json
import os
import sys
import time
import urllib.error
import urllib.request
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from urllib.parse import parse_qs, unquote, urlparse

from cryptography.hazmat.primitives.serialization import load_pem_public_key

SERVICE = "notes"
PREFIX = "/notes"
PDP_URL = os.environ.get("PDP_URL", "http://portal:4182")
ISSUER = os.environ["IDENTITY_ISSUER"]
HEADER = "X-Identity-Token"
PDP_TOKEN = open(os.environ["PDP_TOKEN_FILE"], encoding="utf-8").read().strip()
PUBLIC_KEY = load_pem_public_key(open(os.environ["IDENTITY_PUBLIC_KEY_FILE"], "rb").read())
INDEX = open(os.path.join(os.path.dirname(__file__), "index.html"), "rb").read()

DOCUMENT = {
    "server": SERVICE,
    "resource_types": [
        {"name": "notebook", "parent": "service", "description": "A notebook of notes."},
        {"name": "note", "parent": "notebook", "description": "One note of a notebook."},
    ],
    "actions": [
        {
            "name": "notes.read",
            "resource_types": ["service", "notebook", "note"],
            "description": "Read the notes.",
            "limits": ["requests", "max_results"],
        },
        {
            "name": "notes.write",
            "resource_types": ["service", "notebook"],
            "description": "Add a note.",
            "limits": ["requests"],
        },
        {
            "name": "notes.delete",
            "resource_types": ["service", "notebook", "note"],
            "description": "Delete a note.",
            "limits": ["requests"],
        },
    ],
    "limits": [
        {
            "name": "requests",
            "label": "Requests",
            "description": "The uses of the service by one subject in a window.",
            "unit": "request",
            "kind": "budget",
        },
        {
            "name": "max_results",
            "label": "Most notes in one read",
            "description": "The most notes that one read gives.",
            "unit": "note",
            "kind": "ceiling",
            "min": 1,
            "max": 100,
        },
    ],
    "resources": [
        {"path": "notebook:work", "label": "Work"},
        {"path": "notebook:public", "label": "Public"},
        {"path": "notebook:private", "label": "Private"},
    ],
}

NOTEBOOKS = {
    "work": [{"id": 1, "text": "Ship the portal demo"}, {"id": 2, "text": "Review the limits"},
             {"id": 3, "text": "Write the report"}],
    "public": [{"id": 1, "text": "Welcome to the demo"}, {"id": 2, "text": "Everyone can read this"}],
    "private": [{"id": 1, "text": "A secret note"}],
}
NEXT_ID = {name: len(notes) + 1 for name, notes in NOTEBOOKS.items()}


def b64url(text):
    return base64.urlsafe_b64decode(text + "=" * (-len(text) % 4))


def verify_token(token):
    """Gives the claims of a good identity token, or raises ValueError."""
    try:
        head, body, signature = token.split(".")
        header = json.loads(b64url(head))
        if header.get("alg") != "Ed25519":
            raise ValueError("bad alg")
        PUBLIC_KEY.verify(b64url(signature), f"{head}.{body}".encode())
        claims = json.loads(b64url(body))
    except Exception as error:  # any failure means a refusal
        raise ValueError(f"bad token: {error}") from error
    now = time.time()
    audience = claims.get("aud")
    audiences = audience if isinstance(audience, list) else [audience]
    if claims.get("iss") != ISSUER:
        raise ValueError("bad issuer")
    if SERVICE not in audiences:
        raise ValueError("bad audience")
    if not isinstance(claims.get("exp"), (int, float)) or claims["exp"] < now:
        raise ValueError("token expired")
    return claims


def resource_of(path):
    """A resource from a list of (type, id) pairs below the service."""
    if not path:
        return {"type": "service", "id": SERVICE}
    ancestors = [{"type": "service", "id": SERVICE}]
    ancestors += [{"type": kind, "id": ident} for kind, ident in path[:-1]]
    kind, ident = path[-1]
    return {"type": kind, "id": ident, "properties": {"ancestors": ancestors}}


def pdp_call(endpoint, body):
    """Calls the PDP. Gives (status, json)."""
    request = urllib.request.Request(
        f"{PDP_URL}/access/v1/{endpoint}",
        data=json.dumps(body).encode(),
        headers={"Authorization": f"Bearer {PDP_TOKEN}", "Content-Type": "application/json"},
        method="POST",
    )
    try:
        with urllib.request.urlopen(request, timeout=3) as reply:
            return reply.status, json.loads(reply.read() or b"{}")
    except urllib.error.HTTPError as error:
        try:
            return error.code, json.loads(error.read() or b"{}")
        except ValueError:
            return error.code, {}
    except OSError as error:
        return 503, {"error": str(error)}


def decide(subject, action, path, usage=False, match="node"):
    """One decision. Gives (allowed, context, exchange)."""
    context = {"match": match}
    if usage:
        context["usage"] = {"units": 1}
    body = {
        "subject": {"type": "user", "id": subject},
        "action": {"name": action},
        "resource": resource_of(path),
        "context": context,
    }
    status, answer = pdp_call("evaluation", body)
    exchange = {"request": body, "status": status, "response": answer}
    allowed = status == 200 and answer.get("decision") is True
    return allowed, answer.get("context", {}), exchange


def parse_path(text):
    """The text form of a path: type:id/type:id."""
    out = []
    for part in filter(None, text.split("/")):
        kind, _, ident = part.partition(":")
        out.append((kind, ident))
    return out


class Handler(BaseHTTPRequestHandler):
    server_version = "notes-demo"
    protocol_version = "HTTP/1.1"

    def log_message(self, fmt, *args):
        sys.stderr.write("%s %s\n" % (self.command, fmt % args))

    def send_json(self, status, body, headers=None):
        data = json.dumps(body).encode()
        self.send_response(status)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(data)))
        self.send_header("Cache-Control", "no-store")
        for name, value in (headers or {}).items():
            self.send_header(name, value)
        self.end_headers()
        self.wfile.write(data)

    def read_body(self):
        length = int(self.headers.get("Content-Length") or 0)
        if length > 8192:
            raise ValueError("body too large")
        raw = self.rfile.read(length) if length else b"{}"
        return json.loads(raw or b"{}")

    def identity(self):
        token = self.headers.get(HEADER)
        if not token:
            self.send_json(401, {"error": f"no {HEADER} header: the request did not pass the portal"})
            return None
        try:
            return verify_token(token)
        except ValueError as error:
            self.send_json(401, {"error": str(error)})
            return None

    def refuse(self, exchange, context):
        """The answer for a decision that is false."""
        reason = context.get("reason")
        status = exchange["status"]
        if status != 200:
            self.send_json(503 if status >= 500 else 502, {"error": "the PDP did not decide", "pdp": exchange})
        elif reason == "budget_exceeded":
            wait = int(context.get("retry_after", 1))
            self.send_json(429, {"error": "budget exceeded", "retry_after": wait, "pdp": exchange},
                           {"Retry-After": str(wait)})
        else:
            self.send_json(403, {"error": "the PDP refused", "reason": reason, "pdp": exchange})

    def do_GET(self):
        path = urlparse(self.path).path
        if path == "/.well-known/procurauth/actions":
            return self.send_json(200, DOCUMENT)
        if path == "/health":
            return self.send_json(200, {"ok": True})
        if path == "/.well-known/procurauth/resources":
            return self.list_options()
        if path in (PREFIX, PREFIX + "/"):
            self.send_response(200)
            self.send_header("Content-Type", "text/html; charset=utf-8")
            self.send_header("Content-Length", str(len(INDEX)))
            self.send_header("Cache-Control", "no-store")
            self.end_headers()
            self.wfile.write(INDEX)
            return
        claims = self.identity()
        if claims is None:
            return
        subject = claims.get("sub", "")
        if path == PREFIX + "/api/whoami":
            return self.send_json(200, {"claims": claims})
        if path == PREFIX + "/api/notebooks":
            return self.list_notebooks(subject)
        parts = path[len(PREFIX):].strip("/").split("/")
        if len(parts) == 4 and parts[:2] == ["api", "notebooks"] and parts[3] == "notes":
            return self.read_notes(subject, parts[2])
        self.send_json(404, {"error": "no such route"})

    def list_options(self):
        """The children of a node, for the forms of grants (protocol 5.6 to 5.8). It lists only the
        children that the subject of the token can read: the PDP decides for each one."""
        claims = self.identity()
        if claims is None:
            return
        query = parse_qs(urlparse(self.path).query, keep_blank_values=True)
        if any(len(values) != 1 for values in query.values()) or "type" not in query:
            return self.send_json(400, {"error": "bad parameter"})
        kind = query["type"][0]
        parent = [(k, unquote(i)) for k, i in parse_path(query.get("parent", [""])[0])]
        text = query.get("q", [""])[0].lower()
        try:
            start = int(query.get("after", ["0"])[0] or 0)
            limit = max(1, min(int(query.get("limit", ["50"])[0]), 50))
        except ValueError:
            return self.send_json(400, {"error": "bad parameter"})
        subject = claims.get("sub", "")
        if kind == "notebook" and not parent:
            children = [(name, name.capitalize()) for name in NOTEBOOKS]
        elif kind == "note" and len(parent) == 1 and parent[0][0] == "notebook" and parent[0][1] in NOTEBOOKS:
            children = [(str(n["id"]), n["text"][:60]) for n in NOTEBOOKS[parent[0][1]]]
        else:
            return self.send_json(400, {"error": "bad parameter"})
        items = []
        position = start
        while position < len(children) and len(items) < limit:
            ident, label = children[position]
            position += 1
            if text and text not in ident.lower() and text not in label.lower():
                continue
            allowed, _, _ = decide(subject, "notes.read", parent + [(kind, ident)], match="subtree")
            if allowed:
                items.append({"id": ident, "label": label, "has_children": kind == "notebook"})
        more = position < len(children)
        self.send_json(200, {"items": items, "next": str(position) if more else None})

    def list_notebooks(self, subject):
        names = list(NOTEBOOKS)
        evaluations = [
            {"action": {"name": action}, "resource": resource_of([("notebook", name)])}
            for name in names
            for action in ("notes.read", "notes.write", "notes.delete")
        ]
        body = {"subject": {"type": "user", "id": subject}, "context": {"match": "node"},
                "evaluations": evaluations}
        status, answer = pdp_call("evaluations", body)
        if status != 200:
            return self.send_json(503, {"error": "the PDP did not decide", "pdp": answer})
        results = answer.get("evaluations", [])
        out = []
        for index, name in enumerate(names):
            flags = [bool(results[index * 3 + n].get("decision")) for n in range(3)] \
                if len(results) == len(evaluations) else [False] * 3
            out.append({"id": name, "count": len(NOTEBOOKS[name]),
                        "read": flags[0], "write": flags[1], "delete": flags[2]})
        self.send_json(200, {"notebooks": out, "pdp": {"request": body, "status": status, "response": answer}})

    def read_notes(self, subject, notebook):
        if notebook not in NOTEBOOKS:
            return self.send_json(404, {"error": "no such notebook"})
        allowed, context, exchange = decide(subject, "notes.read", [("notebook", notebook)], usage=True)
        if not allowed:
            return self.refuse(exchange, context)
        notes = NOTEBOOKS[notebook]
        limits = context.get("limits", {})
        cut = limits.get("max_results")
        shown = notes[:cut] if isinstance(cut, int) else notes
        self.send_json(200, {"notes": shown, "total": len(notes), "limits": limits,
                             "usage": context.get("usage"), "pdp": exchange})

    def do_POST(self):
        path = urlparse(self.path).path
        claims = self.identity()
        if claims is None:
            return
        subject = claims.get("sub", "")
        try:
            body = self.read_body()
        except ValueError as error:
            return self.send_json(400, {"error": str(error)})
        if path == PREFIX + "/api/pdp":
            return self.playground(subject, body)
        parts = path[len(PREFIX):].strip("/").split("/")
        if len(parts) == 4 and parts[:2] == ["api", "notebooks"] and parts[3] == "notes":
            notebook = parts[2]
            if notebook not in NOTEBOOKS:
                return self.send_json(404, {"error": "no such notebook"})
            text = str(body.get("text", "")).strip()[:200]
            if not text:
                return self.send_json(400, {"error": "the text is empty"})
            allowed, context, exchange = decide(subject, "notes.write", [("notebook", notebook)], usage=True)
            if not allowed:
                return self.refuse(exchange, context)
            note = {"id": NEXT_ID[notebook], "text": text}
            NEXT_ID[notebook] += 1
            NOTEBOOKS[notebook].append(note)
            return self.send_json(201, {"note": note, "pdp": exchange})
        self.send_json(404, {"error": "no such route"})

    def do_DELETE(self):
        path = urlparse(self.path).path
        claims = self.identity()
        if claims is None:
            return
        subject = claims.get("sub", "")
        parts = path[len(PREFIX):].strip("/").split("/")
        if len(parts) == 5 and parts[:2] == ["api", "notebooks"] and parts[3] == "notes":
            notebook, note_id = parts[2], parts[4]
            if notebook not in NOTEBOOKS or not note_id.isdigit():
                return self.send_json(404, {"error": "no such note"})
            allowed, context, exchange = decide(
                subject, "notes.delete", [("notebook", notebook), ("note", note_id)], usage=True)
            if not allowed:
                return self.refuse(exchange, context)
            before = len(NOTEBOOKS[notebook])
            NOTEBOOKS[notebook][:] = [n for n in NOTEBOOKS[notebook] if n["id"] != int(note_id)]
            status = 200 if len(NOTEBOOKS[notebook]) < before else 404
            return self.send_json(status, {"deleted": status == 200, "pdp": exchange})
        self.send_json(404, {"error": "no such route"})

    def playground(self, subject, body):
        """Sends any question to the PDP for the subject of the identity token."""
        action = str(body.get("action", "access"))
        match = body.get("match", "node")
        try:
            path = parse_path(str(body.get("path", "")))
        except ValueError:
            return self.send_json(400, {"error": "bad path"})
        _, _, exchange = decide(subject, action, path, usage=bool(body.get("usage")), match=match)
        self.send_json(200, {"pdp": exchange})


if __name__ == "__main__":
    ThreadingHTTPServer(("0.0.0.0", 8080), Handler).serve_forever()
