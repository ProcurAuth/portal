#!/usr/bin/env python3
"""Write the plan of the screenshots (a JSON file for capture.js).

Usage:
  make_plan.py items  DEMO_JSON OUT_FILE   the test server of items_demo_for_the_eyes (the Inbox and the approval)
  make_plan.py agents DEMO_JSON OUT_FILE   the test server of agents_demo_for_the_eyes (the agents)
  make_plan.py demo   BASE_URL OUT_FILE    the Docker demo (demo/up.sh): reads demo/.secrets/credentials.txt

DEMO_JSON is the file demo.json that the test server writes. The script needs only Python 3.
"""
import json
import os
import re
import sys

ROOT = os.path.abspath(os.path.join(os.path.dirname(__file__), "..", "..", ".."))


def write(plan, out):
    with open(out, "w", encoding="utf-8") as handle:
        json.dump(plan, handle, indent=1)


def test_server(kind, demo_json, out):
    demo = json.load(open(demo_json, encoding="utf-8"))
    ids = demo["ids"]
    base = f"http://127.0.0.1:{demo['port']}"
    owner = demo["owner"]
    inbox = "/auth/settings/inbox"
    if kind == "items":
        shots = [
            {"name": "inbox", "user": owner, "path": inbox, "height": 960},
            {"name": "approval", "user": owner, "path": f"{inbox}/items/{ids['markdown']}", "height": 1480},
        ]
    else:
        shots = [
            {"name": "agents", "user": owner, "path": f"{inbox}/agents", "height": 560},
            {"name": "agent", "user": owner, "path": f"{inbox}/agents/{ids['agent_billing']}", "height": 700},
        ]
    write({"base": base, "users": {owner: demo["password"]}, "shots": shots}, out)


def docker_demo(base, out):
    creds = open(os.path.join(ROOT, "demo", ".secrets", "credentials.txt"), encoding="utf-8").read()
    users = {}
    for line in creds.splitlines():
        m = re.match(r"^(\w+):\s+(\w+) / (\S+)$", line.strip())
        if m:
            users[m.group(2)] = m.group(3)
    key_id = open(os.path.join(ROOT, "demo", ".secrets", "shots_key_id"), encoding="utf-8").read().strip()
    shots = [
        {"name": "signin", "user": None, "path": "/auth/login", "height": 440},
        {"name": "launcher", "user": "admin", "path": "/", "height": 300},
        {"name": "settings-users", "user": "admin", "path": "/auth/settings", "width": 1440, "height": 350},
        {"name": "settings-grants", "user": "admin", "path": "/auth/settings/users/alice", "height": 760, "click": "text=Notes"},
        {"name": "settings-keys", "user": "admin", "path": f"/auth/keys/edit?id={key_id}", "height": 760, "click": "text=Notes"},
        {"name": "settings-services", "user": "admin", "path": "/auth/settings/services", "height": 330},
        {"name": "history", "user": "admin", "path": "/auth/settings/activity", "height": 660},
    ]
    write({"base": base, "users": users, "shots": shots}, out)


if __name__ == "__main__":
    kind, arg, out = sys.argv[1:4]
    if kind in ("items", "agents"):
        test_server(kind, arg, out)
    elif kind == "demo":
        docker_demo(arg, out)
    else:
        sys.exit(__doc__)
