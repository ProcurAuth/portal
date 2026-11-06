"""Demo service "notes" for the portal. It is a policy enforcement point (PEP) of ProcurAuth.

The service trusts only the identity token of the portal (header X-Identity-Token). For each
operation it asks the PDP of the portal. It enforces the ceiling max_results, and it counts one use
of the budget requests. This file is for a local demo only.
"""

import base64
import hashlib
import json
import os
import re
import sys
import time
import urllib.error
import urllib.request
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from urllib.parse import unquote, urlparse

from cryptography.hazmat.primitives.serialization import Encoding, PublicFormat, load_pem_public_key

SERVICE = "notes"
PREFIX = "/notes"
PDP_URL = os.environ.get("PDP_URL", "http://portal:4182")
ISSUER = os.environ["IDENTITY_ISSUER"]
HEADER = "X-Identity-Token"
# The maximum age of a token and the clock skew (protocol 9.4). The portal gives tokens with a life
# of identity.token_ttl (5m in portal.hcl). Do not set MAX_AGE below that life.
MAX_AGE = 300
SKEW = 30
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
# The ceilings that each action lists. The service can apply only these (protocol 7.5).
CEILING_NAMES = {limit["name"] for limit in DOCUMENT["limits"] if limit["kind"] == "ceiling"}
CEILINGS = {action["name"]: CEILING_NAMES.intersection(action.get("limits", []))
            for action in DOCUMENT["actions"]}


def b64url(text):
    return base64.urlsafe_b64decode(text + "=" * (-len(text) % 4))


def thumbprint(key):
    """The RFC 7638 thumbprint of an Ed25519 public key."""
    x = base64.urlsafe_b64encode(key.public_bytes(Encoding.Raw, PublicFormat.Raw)).rstrip(b"=")
    digest = hashlib.sha256(b'{"crv":"Ed25519","kty":"OKP","x":"%s"}' % x).digest()
    return base64.urlsafe_b64encode(digest).rstrip(b"=").decode()


# The kid of the key of the edge. The portal writes this kid in the header of each token.
KID = thumbprint(PUBLIC_KEY)


def is_number(value):
    return isinstance(value, (int, float)) and not isinstance(value, bool)


def is_whole(value):
    return isinstance(value, int) and not isinstance(value, bool)


def verify_token(token):
    """Gives the claims of a good identity token (protocol 9.4), or raises ValueError."""
    try:
        head, body, signature = token.split(".")
        header = json.loads(b64url(head))
        if not isinstance(header, dict) or header.get("alg") != "Ed25519":
            raise ValueError("bad alg")
        if header.get("typ") != "JWT":
            raise ValueError("bad typ")
        if "kid" in header and header["kid"] != KID:
            raise ValueError("unknown kid")
        PUBLIC_KEY.verify(b64url(signature), f"{head}.{body}".encode())
        claims = json.loads(b64url(body))
    except Exception as error:  # any failure means a refusal
        raise ValueError(f"bad token: {error}") from error
    if not isinstance(claims, dict):
        raise ValueError("bad claims")
    audience = claims.get("aud")
    audiences = audience if isinstance(audience, list) else [audience]
    if claims.get("iss") != ISSUER:
        raise ValueError("bad issuer")
    if SERVICE not in audiences:
        raise ValueError("bad audience")
    if not isinstance(claims.get("sub"), str) or not claims["sub"]:
        raise ValueError("no sub")
    if not is_number(claims.get("iat")) or not is_number(claims.get("exp")):
        raise ValueError("no iat or no exp")
    now = time.time()
    if claims["exp"] - claims["iat"] > MAX_AGE:
        raise ValueError("the life of the token is too long")
    if claims["iat"] > now + SKEW:
        raise ValueError("iat is in the future")
    if now > claims["exp"] + SKEW:
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
    """Calls the PDP one time, with no retry (protocol 10.6). Gives (status, json). The status is None
    when no answer came. The json is None when the body is not JSON."""
    request = urllib.request.Request(
        f"{PDP_URL}/access/v1/{endpoint}",
        data=json.dumps(body).encode(),
        headers={"Authorization": f"Bearer {PDP_TOKEN}", "Content-Type": "application/json"},
        method="POST",
    )
    try:
        with urllib.request.urlopen(request, timeout=3) as reply:
            status, raw = reply.status, reply.read()
    except urllib.error.HTTPError as error:
        try:
            status, raw = error.code, error.read()
        except OSError:
            status, raw = error.code, b""
    except OSError as error:
        return None, {"error": f"no answer: {error}"}
    try:
        return status, json.loads(raw)
    except ValueError:
        return status, None


def judge(action, status, answer, usage):
    """Reads the answer of the PDP to one question (protocol 6.12, 7.5, 10.3 and 11.1). Gives
    ("allow", limits), ("deny", None), ("budget", retry_after) or ("error", None)."""
    if status != 200 or not isinstance(answer, dict) or not isinstance(answer.get("decision"), bool):
        return "error", None
    context = answer.get("context", {})
    if not isinstance(context, dict):
        return "error", None
    if not answer["decision"]:
        if context.get("reason") != "budget_exceeded":
            return "deny", None
        wait = context.get("retry_after")
        if not is_whole(wait) or wait < 1:
            return "error", None
        return "budget", wait
    if usage:
        counted = context.get("usage")
        units = counted.get("units") if isinstance(counted, dict) else None
        if not is_whole(units) or units not in (0, 1):
            return "deny", None
    limits = context.get("limits", {})
    if not isinstance(limits, dict):
        return "deny", None
    for name, value in limits.items():
        if name not in CEILINGS.get(action, ()) or not is_whole(value) or value < 0:
            return "deny", None
    return "allow", limits


def decide(subject, action, path, usage=False, match="node"):
    """One question to the PDP. Gives (verdict, value, exchange): judge() gives the verdict and the value."""
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
    verdict, value = judge(action, status, answer, usage)
    return verdict, value, {"request": body, "status": status, "response": answer}


def client_view(exchange):
    """The exchange with the PDP as the client sees it: with no context.reason (protocol 10.3)."""
    shown = json.loads(json.dumps(exchange))
    response = shown.get("response")
    answers = [response]
    if isinstance(response, dict) and isinstance(response.get("evaluations"), list):
        answers += response["evaluations"]
    for answer in answers:
        if isinstance(answer, dict) and isinstance(answer.get("context"), dict):
            answer["context"].pop("reason", None)
    return shown


def decode_once(text):
    """Decodes each %XX of a query value one time. A "+" stays a "+" (protocol 5.7)."""
    if re.search(r"%(?![0-9A-Fa-f]{2})", text):
        raise ValueError("bad percent-encoding")
    return unquote(text, errors="strict")


def parse_query(raw):
    """The query parameters of protocol 5.7. A parameter that occurs two times raises ValueError."""
    out = {}
    for part in filter(None, raw.split("&")):
        name, _, value = part.partition("=")
        name = decode_once(name)
        if name in out:
            raise ValueError(f"the parameter {name} occurs more than one time")
        out[name] = decode_once(value)
    return out


def whole(text, lowest):
    """A whole number in decimal digits, not below lowest, or ValueError."""
    if not re.fullmatch(r"[0-9]+", text) or int(text) < lowest:
        raise ValueError("bad number")
    return int(text)


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

    def refuse(self, verdict, value, exchange):
        """The answer to a use that the PDP did not allow (protocol 10.3). The body never shows
        context.reason."""
        shown = client_view(exchange)
        if verdict == "budget":
            self.send_json(429, {"error": "budget exceeded", "retry_after": value, "pdp": shown},
                           {"Retry-After": str(value)})
        elif verdict == "deny":
            self.send_json(403, {"error": "the PDP refused", "pdp": shown})
        else:
            self.send_json(503, {"error": "the PDP did not decide", "pdp": shown})

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
        try:
            query = parse_query(urlparse(self.path).query)
            kind = query["type"]
            parent = [(k, decode_once(i)) for k, i in parse_path(query.get("parent", ""))]
            start = whole(query.get("after", "0"), 0)
            # The service MAY give fewer items than the limit: it gives 50 at most.
            limit = min(whole(query.get("limit", "50"), 1), 50)
        except (KeyError, ValueError):
            return self.send_json(400, {"error": "bad parameter"})
        text = query.get("q", "").lower()
        subject = claims["sub"]
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
            verdict, _, _ = decide(subject, "notes.read", parent + [(kind, ident)], match="subtree")
            if verdict == "error":
                return self.send_json(503, {"error": "the PDP did not decide"})
            if verdict == "allow":
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
        exchange = client_view({"request": body, "status": status, "response": answer})
        results = answer.get("evaluations") if status == 200 and isinstance(answer, dict) else None
        if not isinstance(results, list):
            return self.send_json(503, {"error": "the PDP did not decide", "pdp": exchange})
        out = []
        for index, name in enumerate(names):
            items = results[index * 3:index * 3 + 3] if len(results) == len(evaluations) else [{}] * 3
            flags = [isinstance(item, dict) and item.get("decision") is True for item in items]
            out.append({"id": name, "count": len(NOTEBOOKS[name]),
                        "read": flags[0], "write": flags[1], "delete": flags[2]})
        self.send_json(200, {"notebooks": out, "pdp": exchange})

    def read_notes(self, subject, notebook):
        if notebook not in NOTEBOOKS:
            return self.send_json(404, {"error": "no such notebook"})
        verdict, limits, exchange = decide(subject, "notes.read", [("notebook", notebook)], usage=True)
        if verdict != "allow":
            return self.refuse(verdict, limits, exchange)
        notes = NOTEBOOKS[notebook]
        cut = limits.get("max_results")
        shown = notes if cut is None else notes[:cut]
        self.send_json(200, {"notes": shown, "total": len(notes), "limits": limits,
                             "usage": exchange["response"]["context"]["usage"], "pdp": client_view(exchange)})

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
            verdict, value, exchange = decide(subject, "notes.write", [("notebook", notebook)], usage=True)
            if verdict != "allow":
                return self.refuse(verdict, value, exchange)
            note = {"id": NEXT_ID[notebook], "text": text}
            NEXT_ID[notebook] += 1
            NOTEBOOKS[notebook].append(note)
            return self.send_json(201, {"note": note, "pdp": client_view(exchange)})
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
            verdict, value, exchange = decide(
                subject, "notes.delete", [("notebook", notebook), ("note", note_id)], usage=True)
            if verdict != "allow":
                return self.refuse(verdict, value, exchange)
            before = len(NOTEBOOKS[notebook])
            NOTEBOOKS[notebook][:] = [n for n in NOTEBOOKS[notebook] if n["id"] != int(note_id)]
            status = 200 if len(NOTEBOOKS[notebook]) < before else 404
            return self.send_json(status, {"deleted": status == 200, "pdp": client_view(exchange)})
        self.send_json(404, {"error": "no such route"})

    def playground(self, subject, body):
        """Sends any question to the PDP for the subject of the identity token. It shows the answer
        without context.reason (protocol 10.3)."""
        action = str(body.get("action", "access"))
        match = body.get("match", "node")
        try:
            path = parse_path(str(body.get("path", "")))
        except ValueError:
            return self.send_json(400, {"error": "bad path"})
        _, _, exchange = decide(subject, action, path, usage=bool(body.get("usage")), match=match)
        self.send_json(200, {"pdp": client_view(exchange)})


if __name__ == "__main__":
    ThreadingHTTPServer(("0.0.0.0", 8080), Handler).serve_forever()
