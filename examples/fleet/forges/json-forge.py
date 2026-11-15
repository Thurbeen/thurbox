#!/usr/bin/env python3
"""A minimal forge: the forge protocol over one JSON file.

Fleet runs this program once per call, as `json-forge.py <file> <verb>`,
with one JSON request on stdin. It answers one JSON object on stdout:
{"known": ...} or {"unknown": {"why": ..., "kind": ...}}.
The protocol mirrors the runtime protocol (docs/runtime/README.md).

The file holds the forge:

    {"login": "fleet-bot",
     "changes": {"acme/widgets/7": <a change snapshot>},
     "markedReady": ["<intent key>", ...]}

A change URL is `https://<host>/<project>/pulls/<id>`, as Gitea and
Forgejo write it. Check it with: fleet forge check <host> --change <URL>
"""
import json
import os
import re
import sys

PROTOCOL = 1
VERBS = ["manifest", "parse", "whoami", "change", "markReady"]
URL = re.compile(r"^https?://([^/?#]+)/(.+)/pulls?/(\d+)/?(?:[?#].*)?$")


def answer(value):
    print(json.dumps({"known": value}))
    sys.exit(0)


def unknown(why, kind="refused"):
    print(json.dumps({"unknown": {"why": why, "kind": kind}}))
    sys.exit(0)


def load(path):
    try:
        with open(path, encoding="utf-8") as f:
            return json.load(f)
    except (OSError, ValueError) as e:
        unknown(f"cannot read {path}: {e}", "unavailable")


def save(path, forge):
    tmp = f"{path}.{os.getpid()}.tmp"
    with open(tmp, "w", encoding="utf-8") as f:
        json.dump(forge, f, indent=2)
    os.replace(tmp, path)


def lock(path):
    """Hold the file's lock until the process exits: one write at a time."""
    try:
        import fcntl
    except ImportError:
        return None
    held = open(f"{path}.lock", "w")
    fcntl.flock(held, fcntl.LOCK_EX)
    return held


def main():
    if len(sys.argv) != 3:
        unknown("usage: json-forge.py <file> <verb>", "unavailable")
    path, verb = sys.argv[1], sys.argv[2]
    try:
        request = json.load(sys.stdin)
    except ValueError as e:
        unknown(f"the request is not JSON: {e}", "unreadable")
    if verb == "manifest":
        answer({"protocol": PROTOCOL, "verbs": VERBS})
    if request.get("protocol") != PROTOCOL:
        unknown(f"this forge speaks protocol {PROTOCOL}")
    if verb == "parse":
        m = URL.match(request.get("url", ""))
        if not m or m.group(1).lower() != request.get("host"):
            unknown("the URL names no change on this forge", "unreadable")
        answer({"project": m.group(2), "id": m.group(3)})
    if verb == "whoami":
        answer(load(path).get("login", "json-forge"))
    if verb not in ("change", "markReady"):
        unknown(f"no verb {verb}")
    ref = request.get("change") or {}
    key = f"{ref.get('project')}/{ref.get('id')}"
    if verb == "change":
        # Absent from the file is proven absence: the file is the whole forge.
        answer(load(path).get("changes", {}).get(key))
    # markReady: the fresh read and the write happen under one lock, so the
    # change cannot move between them.
    held = lock(path)
    forge = load(path)
    change = forge.get("changes", {}).get(key)
    if change is None:
        answer({"failed": {"why": f"no change {key}"}})
    expect = request.get("expect") or {}
    if change["head"]["id"] != expect.get("head") or change["state"] != expect.get("state"):
        answer({"moved": {"head": change["head"]["id"], "state": change["state"]}})
    already = change["state"] != "draft"
    if not already:
        change["state"] = "open"
        forge.setdefault("markedReady", []).append(request.get("intentKey"))
        save(path, forge)
    del held
    answer({"markedReady": {"already": already}})


main()
