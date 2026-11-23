"""A service that publishes a large action document, to try the pages of the portal with
hundreds of actions and resources. It serves only the document and a health answer."""
import json
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

SERVICE = "bigdb"
LEVELS = ["service", "cluster", "host", "database", "table"]
GROUPS = [
    ("service", ["audit", "config", "metrics", "settings"]),
    ("cluster", ["cluster", "replica", "shard", "keeper"]),
    ("host", ["host", "process", "disk", "backup"]),
    ("database", ["database", "user", "role", "query", "dictionary", "view"]),
    ("table", ["table", "partition", "index", "mutation", "projection", "column"]),
]
VERBS = ["read", "list", "create", "update", "delete", "drop", "alter", "grant", "revoke", "export",
         "import", "optimize", "truncate", "attach", "detach", "rename", "kill", "show", "inspect",
         "restore"]


def build():
    actions = []
    count = 0
    for depth, (level, names) in enumerate(GROUPS):
        # An action of a type applies to that type and to the types below it.
        types = LEVELS[max(depth, 1):] if depth else ["service"] + LEVELS[1:]
        for name in names:
            for verb in VERBS[: 6 if depth == 0 else 20]:
                limits = ["requests", "max_rows"] if count % 5 == 0 else (["requests"] if count % 3 == 0 else [])
                actions.append({
                    "name": f"{name}.{verb}",
                    "resource_types": types,
                    "description": f"{verb.capitalize()} the {name} objects of a {level} and of what it holds.",
                    **({"limits": limits} if limits else {}),
                })
                count += 1
    resources = []
    for c in range(1, 5):
        cpath = f"cluster:eu{c}"
        resources.append({"path": cpath, "label": f"eu{c}"})
        for h in range(1, 7):
            hpath = f"{cpath}/host:ch-{c}-{h}"
            resources.append({"path": hpath, "label": f"ch-{c}-{h}"})
            for d in range(1, 4):
                dpath = f"{hpath}/database:db{d}"
                resources.append({"path": dpath, "label": f"db{d}"})
                for t in range(1, 4 if d == 1 else 2):
                    resources.append({"path": f"{dpath}/table:t{t}", "label": f"t{t}"})
    return {
        "server": SERVICE,
        "resource_types": [
            {"name": "cluster", "parent": "service", "description": "A cluster."},
            {"name": "host", "parent": "cluster", "description": "A host of a cluster."},
            {"name": "database", "parent": "host", "description": "A database of a host."},
            {"name": "table", "parent": "database", "description": "A table of a database."},
        ],
        "actions": actions,
        "limits": [
            {"name": "requests", "label": "Requests", "description": "The uses of the service by one subject in a window.",
             "unit": "request", "kind": "budget"},
            {"name": "max_rows", "label": "Most rows", "description": "The most rows that one read gives.",
             "unit": "rows", "kind": "ceiling", "min": 1, "max": 1000000},
        ],
        "resources": resources,
    }


DOCUMENT = build()
BODY = json.dumps(DOCUMENT).encode()
PAGE = (
    "<!doctype html><meta charset=utf-8><title>Big database</title>"
    "<body style='font-family:sans-serif;max-width:40rem;margin:3rem auto'>"
    "<h1>Big database</h1><p>This service has no front. It publishes "
    f"{len(DOCUMENT['actions'])} actions and {len(DOCUMENT['resources'])} resources, "
    "to try the pages of the portal with a large action document.</p>"
    "<p><a href='/bigdb/.well-known/procurauth/actions'>The action document</a></p>"
).encode()


class Handler(BaseHTTPRequestHandler):
    def do_GET(self):
        if self.path == "/.well-known/procurauth/actions":
            self.reply(BODY)
        elif self.path == "/health":
            self.reply(b'{"ok":true}')
        elif self.path in ("/bigdb", "/bigdb/"):
            self.reply(PAGE, "text/html; charset=utf-8")
        elif self.path == "/bigdb/.well-known/procurauth/actions":
            self.reply(BODY)
        else:
            self.send_response(404)
            self.end_headers()

    def reply(self, body, kind="application/json"):
        self.send_response(200)
        self.send_header("Content-Type", kind)
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def log_message(self, *args):
        pass


if __name__ == "__main__":
    ThreadingHTTPServer(("0.0.0.0", 8080), Handler).serve_forever()
