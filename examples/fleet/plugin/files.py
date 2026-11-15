#!/usr/bin/env python3
"""A minimal Fleet plugin: the `File` kind keeps one file's content.

Fleet runs this program once per call, with the call as the last argument
and one JSON request on stdin. It answers one JSON object on stdout.
`docs/fleet/plugins.md` walks through it.

- manifest:  the kinds this plugin serves, with their JSON Schemas.
- probes:    what the controller needs to know (pure).
- observe:   read the file (I/O, bounded by Fleet).
- reconcile: compare spec, status and observations; return a plan (pure).
- perform:   write the file (I/O, journaled by Fleet).

Files live in the plugin's working directory, `cwd` in fleet.toml.
"""
import json
import sys

MANIFEST = {
    "protocol": 1,
    "name": "files",
    "kinds": [
        {
            "kind": "File",
            "pollMs": 60000,
            "spec": {
                "type": "object",
                "additionalProperties": False,
                "required": ["name", "content"],
                "properties": {
                    "name": {"type": "string", "pattern": "^[A-Za-z0-9][A-Za-z0-9._-]*$"},
                    "content": {"type": "string"},
                },
            },
            "status": {
                "type": "object",
                "properties": {
                    "phase": {"enum": ["Writing", "Ready", "Unknown"]},
                    "observedGeneration": {"type": "integer"},
                },
            },
        }
    ],
}


def read(name):
    try:
        with open(name, encoding="utf-8") as f:
            return {"known": {"exists": True, "content": f.read()}}
    except FileNotFoundError:
        return {"known": {"exists": False}}
    except OSError as e:
        return {"unknown": {"why": str(e)}}


def probes(req):
    if "file" in req["observations"]:
        return {"probes": []}
    name = req["object"]["spec"]["name"]
    return {"probes": [{"id": "file", "type": "plugin", "request": {"name": name}}]}


def reconcile(req):
    obj = req["object"]
    spec = obj["spec"]
    generation = obj["metadata"]["generation"]
    seen = req["observations"]["file"]
    if "unknown" in seen:
        # Unknown never means absent: keep the status, look again later.
        return {"plan": {"status": {**own(obj["status"]), "phase": "Unknown"}}}
    if seen["known"].get("content") == spec["content"]:
        return {"plan": {"status": {"phase": "Ready", "observedGeneration": generation}}}
    write = {"key": f"write/{generation}", "request": {"name": spec["name"], "content": spec["content"]}}
    # Look again in a second, to confirm the write, instead of after pollMs.
    return {
        "plan": {
            "status": {"phase": "Writing", "observedGeneration": generation},
            "effects": [write],
            "notBefore": req["now"] + 1000,
        }
    }


def own(status):
    """The plugin's own status fields, without the ones Fleet reserves."""
    return {k: v for k, v in (status or {}).items() if k not in ("notBefore", "effects", "host")}


def perform(req):
    request = req["effect"]["request"]
    try:
        with open(request["name"], "w", encoding="utf-8") as f:
            f.write(request["content"])
    except OSError as e:
        return {"outcome": {"failed": {"why": str(e)}}}
    return {"outcome": {"done": {"bytes": len(request["content"].encode())}}}


def main():
    call = sys.argv[-1]
    req = json.loads(sys.stdin.readline())
    if call == "manifest":
        answer = {"manifest": MANIFEST}
    elif call == "probes":
        answer = probes(req)
    elif call == "observe":
        answer = read(req["probe"]["request"]["name"])
    elif call == "reconcile":
        answer = reconcile(req)
    elif call == "perform":
        answer = perform(req)
    else:
        answer = {"ok": False, "why": f"unknown call {call}"}
    answer.setdefault("ok", True)
    answer["id"] = req["id"]
    print(json.dumps(answer))


if __name__ == "__main__":
    main()
