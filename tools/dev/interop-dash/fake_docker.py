#!/usr/bin/env python3
"""A fake Docker socket for tools/dev/interop-dash.sh. It runs in a throwaway container (python:3.13-alpine).

The portal reads the container labels from the list that this socket gives. The list has one container: ClickHouse Dash.
The portal so learns the id, the path of the pages and the address of the document of the actions. It sees no other container.
"""
import json
import os
import socketserver
from http.server import BaseHTTPRequestHandler


def main() -> None:
    path = os.environ["SOCKET_PATH"]
    service_id = os.environ["SERVICE_ID"]
    body = json.dumps([{
        "Id": "a",
        "Names": ["/" + service_id],
        "State": "running",
        "Status": "Up 1 minute",
        "Labels": {
            "portal.name": "ClickHouse Dash",
            "portal.id": service_id,
            "portal.href": "/dash/",
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


if __name__ == "__main__":
    main()
