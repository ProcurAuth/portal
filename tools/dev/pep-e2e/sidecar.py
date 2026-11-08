#!/usr/bin/env python3
"""Two small helpers of tools/dev/pep-e2e.sh. They run in a throwaway container (python:3.13-alpine).

  sidecar.py docker   Serve a fake Docker socket. The portal reads one container from it: the reference service.
  sidecar.py tap      Forward the PDP link of the service to the portal, and write each exchange to a file.

The tap sits on the link between the real PEP (the reference service) and the real PDP (the portal). It changes
nothing. It keeps what the service SENT and what the service RECEIVED, so a test can prove what the service learned.
The tap never writes the bearer token of the service.
"""
import http.client
import json
import os
import socketserver
import sys
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer


def serve_docker() -> None:
    """Answer each request with the list of containers. The portal reads the labels of the list."""
    path = os.environ["SOCKET_PATH"]
    body = json.dumps([{
        "Id": "a",
        "Names": ["/datastore"],
        "State": "running",
        "Status": "Up 1 minute",
        "Labels": {
            "portal.name": "Datastore",
            "portal.id": "datastore",
            "portal.href": "/datastore/",
            "portal.actions": os.environ["ACTIONS_URL"],
        },
    }]).encode()

    class Handler(BaseHTTPRequestHandler):
        protocol_version = "HTTP/1.0"

        def do_GET(self):  # noqa: N802 (name of the standard library)
            self.send_response(200)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)

        do_POST = do_GET  # noqa: N815

        def log_message(self, *args):
            pass

        def address_string(self):
            return "unix"

    class Server(socketserver.ThreadingMixIn, socketserver.UnixStreamServer):
        daemon_threads = True

    if os.path.exists(path):
        os.unlink(path)
    server = Server(path, Handler)
    os.chmod(path, 0o666)
    server.serve_forever()


def serve_tap() -> None:
    """Forward each request to the portal and write one JSON line for each exchange."""
    target_host, target_port = os.environ["TARGET"].rsplit(":", 1)
    log_path = os.environ["TAP_LOG"]
    lock = threading.Lock()

    def parse(raw: bytes):
        try:
            return json.loads(raw) if raw else None
        except ValueError:
            return raw.decode("utf-8", "replace")

    class Handler(BaseHTTPRequestHandler):
        protocol_version = "HTTP/1.1"

        def handle_any(self):
            length = int(self.headers.get("Content-Length") or 0)
            raw = self.rfile.read(length) if length else b""
            headers = {}
            for name in ("Authorization", "Content-Type"):
                if self.headers.get(name):
                    headers[name] = self.headers[name]
            began = time.time()
            try:
                conn = http.client.HTTPConnection(target_host, int(target_port), timeout=120)
                conn.request(self.command, self.path, body=raw or None, headers=headers)
                reply = conn.getresponse()
                answer = reply.read()
                status = reply.status
                out_headers = [(n, v) for n, v in reply.getheaders()
                               if n.lower() in ("content-type", "retry-after")]
                conn.close()
            except OSError as error:
                answer, status, out_headers = str(error).encode(), 502, []
            line = {
                "t": began,
                "method": self.command,
                "path": self.path,
                "request": parse(raw),
                "status": status,
                "response": parse(answer),
                # The headers of the answer. Never the bearer token of the request.
                "response_headers": dict(out_headers),
            }
            with lock, open(log_path, "a", encoding="utf-8") as out:
                out.write(json.dumps(line) + "\n")
            self.send_response(status)
            for name, value in out_headers:
                self.send_header(name, value)
            self.send_header("Content-Length", str(len(answer)))
            self.end_headers()
            self.wfile.write(answer)

        do_GET = do_POST = do_DELETE = do_PUT = handle_any  # noqa: N815

        def log_message(self, *args):
            pass

    server = ThreadingHTTPServer(("0.0.0.0", int(os.environ.get("LISTEN_PORT", "4182"))), Handler)
    server.daemon_threads = True
    server.serve_forever()


if __name__ == "__main__":
    mode = sys.argv[1] if len(sys.argv) > 1 else ""
    if mode == "docker":
        serve_docker()
    elif mode == "tap":
        serve_tap()
    else:
        sys.exit("usage: sidecar.py docker|tap")
