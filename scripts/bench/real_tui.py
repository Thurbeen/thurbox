#!/usr/bin/env python3
"""Run an isolated Codex TUI against a loopback Responses fixture.

This is a separate workload from run.py's Python stand-in. Resource numbers
include Codex itself; the synthetic host-only figures exclude the stand-in.
Input echo times one eight-byte marker written in a single call, then waits
for that marker in the attached client's output.
"""

import argparse
import datetime
import json
import os
import shutil
import socket
import subprocess
import sys
import threading
import time
from http.server import ThreadingHTTPServer
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
import benchlib as bl
import hosts
from mock_responses import Handler


def configure(sb, port, cli):
    home = Path(sb.root) / "codex"
    home.mkdir()
    (home / "config.toml").write_text(f"""model = "local-mock"
model_provider = "fixture"
check_for_update_on_startup = false
approval_policy = "never"
sandbox_mode = "read-only"
[model_providers.fixture]
name = "Loopback fixture"
base_url = "http://127.0.0.1:{port}/v1"
wire_api = "responses"
env_key = "MOCK_API_KEY"
""")
    commands = {"SessionStart": "idle", "UserPromptSubmit": "working", "Stop": "done"}
    hooks = {
        event: [
            {
                "hooks": [
                    {
                        "type": "command",
                        "command": f"python3 {HERE / 'real_tui_hook.py'} {state}",
                    }
                ]
            }
        ]
        for event, state in commands.items()
    }
    (home / "hooks.json").write_text(json.dumps({"hooks": hooks}))
    sb.env.update(
        CODEX_HOME=str(home),
        MOCK_API_KEY="dummy",
        BENCH_HOOK_LOG=str(Path(sb.root) / "hooks.jsonl"),
        BENCH_THURBOX_CLI=cli,
    )
    sb.env["PATH"] = str(Path(cli).parent) + os.pathsep + sb.env["PATH"]
    sb.run(["git", "init", "-q"])
    sb.agent_argv = lambda _: [
        shutil.which("codex"),
        "--dangerously-bypass-hook-trust",
        "Reply with one short word.",
    ]


def whole_stack_pids(host, client):
    """Include Codex even when the multiplexer reparents its pane process."""
    pids = set(host.host_pids(client))
    marker = f"CODEX_HOME={host.sb.env['CODEX_HOME']}".encode()
    for entry in os.scandir("/proc"):
        if not entry.name.isdigit():
            continue
        try:
            environ = (Path(entry.path) / "environ").read_bytes()
        except (OSError, ProcessLookupError):
            continue
        if marker in environ.split(b"\0"):
            pids.add(int(entry.name))
    return sorted(pids)


def run_one(name, root, port, tools):
    sb = bl.Sandbox(str(root / name))
    configure(sb, port, tools["thurbox-cli"])
    host = hosts.HOSTS[name](sb, tools)
    client = None
    try:
        host.create("codex")
        client = bl.PtyClient(host.client_argv(), sb.env, sb.work)
        if name == "thurbox":
            client.write(b"\r")
        trust_marker = b"continue" if name == "herdr" else b"Trust this folder?"
        trust = client.wait_for(trust_marker, 20)
        if trust is None:
            raise RuntimeError(
                f"{name}: Codex trust prompt never appeared; "
                f"terminal tail={bytes(client.buf[-500:])!r}"
            )
        time.sleep(0.3)
        client.write(b"\r")
        response = client.wait_for(b"MOCK_TURN_DONE", 30)
        if response is None:
            raise RuntimeError(
                f"{name}: mock-backed Codex turn never appeared; "
                f"terminal tail={bytes(client.buf[-500:])!r}"
            )
        hook_path = Path(sb.env["BENCH_HOOK_LOG"])
        seen = bl.wait_until(
            lambda: hook_path.exists() and "done" in hook_path.read_text(), 10
        )
        if not seen:
            raise RuntimeError(f"{name}: Codex Stop hook was not delivered")
        hooks_seen = [json.loads(line) for line in hook_path.read_text().splitlines()]
        expected = ["SessionStart", "UserPromptSubmit", "Stop"]
        actual = [row["event"] for row in hooks_seen]
        if actual[:3] != expected:
            raise RuntimeError(
                f"{name}: hook sequence {actual!r}, expected {expected!r}"
            )
        if name == "thurbox" and any(row["signal_exit"] != 0 for row in hooks_seen[:3]):
            raise RuntimeError("Thurbox: a Codex status hook failed to signal")
        echo_ms = []
        for i in range(5):
            marker = f"qzbench{i}".encode()
            offset = len(client.buf)
            start = bl.now_ns()
            client.write(marker)
            seen = client.wait_for(marker, 5, since=offset)
            echo_ms.append(bl.ms(seen - start) if seen is not None else None)
            client.write(b"\x15")  # clear the prompt for the next observation
            client.pump(0.3)
        if all(sample is None for sample in echo_ms):
            raise RuntimeError(f"{name}: Codex echoed none of five typed inputs")
        pids = lambda: whole_stack_pids(host, client)
        cpu = bl.cpu_over(pids, 3)
        process_pss = {}
        for pid in pids():
            comm = bl.comm(pid)
            process_pss[comm] = process_pss.get(comm, 0) + bl.memory_kib(pid)[1] / 1024
        rows = {
            "host": name,
            "underlying_multiplexer": "tmux" if name == "thurbox" else name,
            "agent": "codex",
            "provider": "local Responses fixture",
            "hooks": hooks_seen,
            "input_echo_ms": echo_ms,
            "input_echo_timeouts": echo_ms.count(None),
            "idle_cpu_pct_one_core": cpu["cpu_pct"],
            "pss_mib_including_agent": sum(process_pss.values()),
            "pss_by_process_mib": process_pss,
        }
        if name == "thurbox":
            row = json.loads(host.thurbox("session", "get", "codex", "--json").stdout)
            rows["thurbox_hook_state"] = row["hook_state"]
            if row["hook_state"] != "done":
                raise RuntimeError(
                    f"Thurbox hook state is {row['hook_state']!r}, expected done"
                )
        return rows
    finally:
        if client is not None:
            client.close()
        host.teardown()
        sb.destroy()


def main():
    p = argparse.ArgumentParser()
    p.add_argument("--hosts", default="tmux,herdr,rmux,thurbox")
    p.add_argument("--work", required=True)
    p.add_argument("--out", required=True)
    p.add_argument("--thurbox-bin", required=True)
    p.add_argument("--reps", type=int, default=3)
    p.add_argument("--warmup", type=int, default=1)
    args = p.parse_args()
    names = args.hosts.split(",")
    if not names or any(
        name not in ("tmux", "herdr", "rmux", "thurbox") for name in names
    ):
        p.error("--hosts must contain only tmux, herdr, rmux, thurbox")
    bindir = Path(args.thurbox_bin).resolve()
    tools = {
        "tmux": shutil.which("tmux"),
        "herdr": shutil.which("herdr"),
        "rmux": shutil.which("rmux"),
        "thurbox": str(bindir / "thurbox"),
        "thurbox-cli": str(bindir / "thurbox-cli"),
    }
    root = Path(args.work)
    root.mkdir(parents=True, exist_ok=True)
    output_path = Path(args.out)
    output_path.parent.mkdir(parents=True, exist_ok=True)
    partial_path = output_path.with_suffix(output_path.suffix + ".partial")
    partial_path.unlink(missing_ok=True)
    (root / "mock-requests.jsonl").unlink(missing_ok=True)
    started = datetime.datetime.now(datetime.timezone.utc).isoformat()
    with socket.socket() as sock:
        sock.bind(("127.0.0.1", 0))
        port = sock.getsockname()[1]
    server = ThreadingHTTPServer(("127.0.0.1", port), Handler)
    server.log = root / "mock-requests.jsonl"
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    results = []
    try:
        for rep in range(args.warmup + args.reps):
            order = names[rep % len(names) :] + names[: rep % len(names)]
            for name in order:
                load = bl.load()
                row = run_one(name, root / f"rep{rep}", port, tools)
                row.update(
                    rep=rep,
                    warmup=rep < args.warmup,
                    load1=load[0],
                    load1_end=bl.load()[0],
                )
                results.append(row)
                partial_path.write_text(
                    json.dumps({"incomplete": True, "records": results}, indent=2)
                    + "\n"
                )
                print(f"real TUI {name} rep {rep}: complete", flush=True)
    finally:
        server.shutdown()
        thread.join()
        server.server_close()
    codex_version = subprocess.run(
        ["codex", "--version"], capture_output=True, text=True, check=True
    ).stdout.strip()
    versions = {
        name: subprocess.run(
            [tools[name], "-V" if name in ("tmux", "rmux") else "--version"],
            capture_output=True,
            text=True,
            check=True,
        ).stdout.strip()
        for name in names
        if name != "thurbox"
    }
    versions["codex"] = codex_version
    versions["thurbox"] = subprocess.run(
        [tools["thurbox-cli"], "--version"], capture_output=True, text=True, check=True
    ).stdout.strip()
    hashes = {name: bl.binary_hash(path) for name, path in tools.items()}
    hashes["codex"] = bl.binary_hash(shutil.which("codex"))
    summary = {}
    for name in names:
        kept = [row for row in results if row["host"] == name and not row["warmup"]]
        summary[name] = {
            "input_echo_ms": bl.summarize(
                [sample for row in kept for sample in row["input_echo_ms"]]
            ),
            "input_echo_timeouts": sum(row["input_echo_timeouts"] for row in kept),
            "idle_cpu_pct_one_core": bl.summarize(
                [row["idle_cpu_pct_one_core"] for row in kept]
            ),
            "pss_mib_including_agent": bl.summarize(
                [row["pss_mib_including_agent"] for row in kept]
            ),
        }
    output = {
        "kind": "real Codex TUI, mocked inference",
        "started": started,
        "machine": bl.machine(),
        "versions": versions,
        "binary_sha256": hashes,
        "thurbox_commit": subprocess.run(
            ["git", "rev-parse", "HEAD"], capture_output=True, text=True, check=True
        ).stdout.strip(),
        "hosts": names,
        "reps": args.reps,
        "warmup": args.warmup,
        "session_command": [
            "codex",
            "--dangerously-bypass-hook-trust",
            "Reply with one short word.",
        ],
        "shell": "/bin/sh",
        "input_echo_definition": "8-byte marker sent in one write; time until seen in client output",
        "records": results,
        "summary": summary,
        "mock_requests": [
            json.loads(line) for line in server.log.read_text().splitlines()
        ],
    }
    output_path.write_text(json.dumps(output, indent=2) + "\n")
    partial_path.unlink(missing_ok=True)
    print(json.dumps(output, indent=2))


if __name__ == "__main__":
    main()
