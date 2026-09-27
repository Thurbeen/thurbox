#!/usr/bin/env python3
"""Record Codex hook delivery and forward it to an isolated Thurbox row."""

import json
import os
import subprocess
import sys
from pathlib import Path


def main():
    state = sys.argv[1]
    event = json.load(sys.stdin)
    signal = None
    if os.environ.get("THURBOX_SESSION"):
        signal = subprocess.run(
            [os.environ["BENCH_THURBOX_CLI"], "session", "signal", "--state", state],
            capture_output=True,
            text=True,
            check=False,
            timeout=10,
        )
    row = {
        "event": event.get("hook_event_name"),
        "state": state,
        "signal_exit": signal.returncode if signal else None,
        "has_thurbox_identity": bool(os.environ.get("THURBOX_SESSION")),
    }
    with Path(os.environ["BENCH_HOOK_LOG"]).open("a") as out:
        out.write(json.dumps(row) + "\n")
    if state == "done":
        print("{}")


if __name__ == "__main__":
    main()
