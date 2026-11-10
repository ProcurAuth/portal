"""A service that looks like ClickHouse Dash for the pages of the portal. It publishes the action
document of ChDash (hosts and databases) and the list of the options of a host: its databases.
It has no front and it does not check the identity token: it is for the demo only."""
import json
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from urllib.parse import parse_qs, unquote, urlparse

SERVICE = "data-clickhouse-dash"
HOSTS = {
    "ch-eu1": ["default", "analytics", "logs", "events", "billing", "staging", "system"],
    "ch-eu2": ["default", "analytics", "logs", "system"],
    # A host with many databases: the form shows them page by page.
    "ch-us1": ["default", "system"] + [f"tenant_{n:03d}" for n in range(1, 121)],
}
HOST_LABELS = {"ch-eu1": "Paris, production", "ch-eu2": "Frankfurt, replica", "ch-us1": "Virginia, multi-tenant"}
ALL = ["service", "host", "database"]
ACTIONS = [
    ("query.run.read", ALL, "Run read-only queries.", ["requests", "max_rows"]),
    ("query.run.write", ALL, "Run queries that change data.", ["requests"]),
    ("query.export", ALL, "Export the result of a query.", ["max_rows"]),
    ("explorer.read", ALL, "Browse the databases, the tables and their data.", []),
    ("explorer.graph", ALL, "See the graph of the dependencies between tables.", []),
    ("system.read", ["service", "host"], "See the System page: processes, parts, disks.", []),
    ("system.documentation", ["service"], "Read the documentation of the functions.", []),
    ("logs.search", ["service", "host"], "Search the logs of the server.", ["max_rows"]),
    ("traces.read", ["service", "host"], "Read the traces of the queries.", []),
    ("metrics.read", ["service", "host"], "Read the metrics of the server.", []),
    ("mcp.keys.read", ["service"], "See the keys of the MCP endpoint.", []),
    ("mcp.keys.write", ["service"], "Make and revoke keys of the MCP endpoint.", []),
    ("mcp.usage.read", ["service"], "See how the keys are used.", []),
    ("mcp.call", ["service", "host", "database"], "Call a tool of the MCP endpoint.", ["requests"]),
]


def document():
    return {
        "server": SERVICE,
        "resource_types": [
            {"name": "host", "parent": "service", "description": "One ClickHouse server."},
            {"name": "database", "parent": "host", "description": "One database of a host."},
        ],
        "actions": [
            {
                "name": n,
                "resource_types": t,
                "description": d,
                **({"limits": l} if l else {}),
            }
            for n, t, d, l in ACTIONS
        ],
        "limits": [
            {"name": "requests", "label": "Requests", "description": "The uses of the service by one subject in a window.",
             "unit": "request", "kind": "budget"},
            {"name": "max_rows", "label": "Most rows", "description": "The most rows that one answer gives.",
             "unit": "row", "kind": "ceiling", "min": 1, "max": 1000000},
        ],
        "resources": [{"path": f"host:{h}", "label": label} for h, label in HOST_LABELS.items()],
    }


BODY = json.dumps(document()).encode()


class Handler(BaseHTTPRequestHandler):
    def do_GET(self):
        url = urlparse(self.path)
        if url.path == "/.well-known/procurauth/actions":
            return self.reply(200, BODY)
        if url.path == "/health":
            return self.reply(200, b'{"ok":true}')
        if url.path == "/.well-known/procurauth/resources":
            return self.options(parse_qs(url.query))
        if url.path in ("/chdash", "/chdash/"):
            page = b"<!doctype html><meta charset=utf-8><title>ClickHouse Dash (demo)</title><body style='font-family:sans-serif;max-width:40rem;margin:3rem auto'><h1>ClickHouse Dash</h1><p>A demo service with hosts and databases, to try the pages of the portal.</p>"
            return self.reply(200, page, "text/html; charset=utf-8")
        self.reply(404, b"{}")

    def options(self, query):
        """The children of a node (protocol 5.6): the databases of a host, page by page."""
        if "type" not in query or any(len(v) != 1 for v in query.values()):
            return self.reply(400, b'{"error":"bad parameter"}')
        parent = query.get("parent", [""])[0]
        if query["type"][0] == "database" and parent.startswith("host:"):
            host = unquote(parent[5:])
            names = HOSTS.get(host)
            if names is None:
                return self.reply(200, b'{"items":[],"next":null}')
            text = query.get("q", [""])[0].lower()
            names = [n for n in names if text in n.lower()]
            start = int(query.get("after", ["0"])[0] or 0)
            limit = max(1, min(int(query.get("limit", ["50"])[0]), 50))
            page = names[start:start + limit]
            more = start + limit < len(names)
            body = {"items": [{"id": n, "label": n, "has_children": False} for n in page],
                    "next": str(start + limit) if more else None}
            return self.reply(200, json.dumps(body).encode())
        if query["type"][0] == "host" and not parent:
            body = {"items": [{"id": h, "label": HOST_LABELS[h], "has_children": True} for h in HOSTS], "next": None}
            return self.reply(200, json.dumps(body).encode())
        self.reply(400, b'{"error":"bad parameter"}')

    def reply(self, status, body, kind="application/json"):
        self.send_response(status)
        self.send_header("Content-Type", kind)
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def log_message(self, *args):
        pass


if __name__ == "__main__":
    ThreadingHTTPServer(("0.0.0.0", 8080), Handler).serve_forever()
