#!/usr/bin/env python3
import json
import sys

CONFIRM_WRITE_AFTER_MS = 1000
FLEET_RESERVED_STATUS_FIELDS = ("notBefore", "effects", "host")

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
        return {"plan": {"status": keep_status_as_unknown(obj["status"])}}
    if seen["known"].get("content") == spec["content"]:
        return {"plan": {"status": {"phase": "Ready", "observedGeneration": generation}}}
    key = write_key(obj["status"], generation)
    write = {"key": key, "request": {"name": spec["name"], "content": spec["content"]}}
    plan = {"status": {"phase": "Writing", "observedGeneration": generation}, "effects": [write]}
    if not last_failed(obj["status"]):
        plan["notBefore"] = req["now"] + CONFIRM_WRITE_AFTER_MS
    return {"plan": plan}


def keep_status_as_unknown(status):
    return {**own(status), "phase": "Unknown"}


def write_key(status, generation):
    prefix = f"write/{generation}/"
    effects = (status or {}).get("effects", {})
    attempts = {int(k[len(prefix):]): e for k, e in effects.items() if k.startswith(prefix)}
    if not attempts:
        return f"{prefix}1"
    last = max(attempts)
    attempt_in_flight = "receipt" not in attempts[last]
    if attempt_in_flight:
        return f"{prefix}{last}"
    return f"{prefix}{last + 1}"


def last_failed(status):
    done = [e for k, e in (status or {}).get("effects", {}).items() if k.startswith("write/") and "receipt" in e]
    if not done:
        return False
    newest = max(done, key=lambda e: e["receipt"]["at"])
    return "failed" in newest["receipt"]["outcome"]


def own(status):
    return {k: v for k, v in (status or {}).items() if k not in FLEET_RESERVED_STATUS_FIELDS}


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
