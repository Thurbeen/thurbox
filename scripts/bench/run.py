#!/usr/bin/env python3
"""Run the multiplexer benchmark: raw tmux vs Herdr vs thurbox.

    scripts/bench/run.py                         # every scenario, every host
    scripts/bench/run.py --scenarios latency --hosts tmux,thurbox --reps 3

Writes ``results.json`` (every sample, with the load average it ran under),
``results.csv`` (one row per sample) and ``summary.md`` (median and p95 per
scenario) into ``--out``. ``scripts/bench/run.sh`` fetches the pinned Herdr and
builds thurbox first; this file assumes both are there.

Each repetition gets a fresh sandbox (own HOME, XDG dirs, tmux socket, Herdr
state), and every server it started is killed before the next one. Hosts are
interleaved within a repetition so a slow drift in the machine lands on all
three rather than on whichever ran last. The first ``--warmup`` repetitions are
recorded but marked, and left out of the summary.
"""

import argparse
import csv
import datetime
import importlib
import json
import os
import platform
import shutil
import signal
import subprocess
import sys
import time

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
sys.path.insert(0, os.path.join(HERE, "scenarios"))

import benchlib as bl
import hosts

SCENARIOS = ["create", "attach", "resources", "throughput", "scrollback", "latency", "survival"]
CACHE = os.path.expanduser(os.environ.get("BENCH_CACHE", "~/.cache/thurbox-bench"))


class Ctx:
    def __init__(self, args, tools):
        self.hosts = args.hosts
        self.reps = args.reps
        self.warmup = args.warmup
        self.quick = args.quick
        self.tools = tools
        # Short on purpose: see Sandbox.SOCKET_SUFFIX.
        self.root = os.path.join(args.work, "sb")
        self.records = []

    def repetitions(self):
        """(index, is_warmup) for every repetition, warm-up first."""
        for i in range(self.warmup + self.reps):
            yield i, i < self.warmup

    def fresh(self, name):
        """A new host in a new sandbox. Scenarios must ``close`` it."""
        sandbox = bl.Sandbox(os.path.join(self.root, name))
        return hosts.HOSTS[name](sandbox, self.tools)

    def close(self, host, *clients):
        for client in clients:
            if client is not None:
                client.close()
        host.teardown()
        for pid in host.agent_pids():
            bl.kill_tree(pid)
        host.sb.destroy()

    def record(self, scenario, host, variant, rep, warmup, load, **metrics):
        """``load`` is the load average when the repetition began; the one when
        this sample ended is taken here, since a repetition can hold several
        samples and minutes of measuring."""
        row = {
            "scenario": scenario,
            "host": host,
            "variant": variant,
            "rep": rep,
            "warmup": warmup,
            "load1": load[0],
            "load5": load[1],
            "load1_end": bl.load()[0],
            "metrics": metrics,
        }
        self.records.append(row)
        shown = ", ".join(
            f"{k}={v:.1f}" if isinstance(v, float) else f"{k}={v}" for k, v in metrics.items()
        )
        tag = " (warm-up)" if warmup else ""
        print(f"  {scenario:<10} {host:<8} {variant:<18} rep {rep}{tag}: {shown}", flush=True)


def resolve_tools(args):
    """The binaries the selected hosts need, and only those: a run of tmux and
    thurbox must not fail for want of a Herdr it will never start."""
    tools = {}
    if {"tmux", "thurbox"} & set(args.hosts):
        tools["tmux"] = shutil.which("tmux")
    if "herdr" in args.hosts:
        tools["herdr"] = args.herdr or os.path.join(
            CACHE, f"herdr-v0.9.1-{platform.machine()}", "herdr"
        )
    if "thurbox" in args.hosts:
        bindir = args.thurbox_bin or os.path.join(
            os.path.dirname(os.path.dirname(HERE)), "target", "release"
        )
        tools["thurbox"] = os.path.join(bindir, "thurbox")
        tools["thurbox-cli"] = os.path.join(bindir, "thurbox-cli")
    for name, path in tools.items():
        if not path or not os.access(path, os.X_OK):
            sys.exit(
                f"missing {name}: {path!r} (run scripts/bench/run.sh, which fetches and builds it)"
            )
    return tools


def versions(ctx):
    out = {}
    for name in ctx.hosts:
        host = ctx.fresh(name)
        try:
            out.update(host.versions())
        finally:
            ctx.close(host)
    out["python"] = sys.version.split()[0]
    return out


def thurbox_commit(tools):
    """The commit the thurbox binaries were built from, when they sit in a git
    checkout's target/ (which is how run.sh builds them)."""
    if "thurbox" not in tools:
        return None
    repo = os.path.dirname(os.path.dirname(os.path.dirname(tools["thurbox"])))
    try:
        return subprocess.run(
            ["git", "-C", repo, "rev-parse", "HEAD"], capture_output=True, text=True, check=True
        ).stdout.strip()
    except (OSError, subprocess.CalledProcessError):
        return None


CSV_FIELDS = ["scenario", "host", "variant", "rep", "warmup", "load1", "load1_end"]


def flatten(records):
    """One row per value: a metric holding a list of samples (latency) becomes
    one row per sample, numbered, so the CSV carries every observation."""
    for r in records:
        base = {k: r.get(k) for k in CSV_FIELDS}
        for metric, value in r["metrics"].items():
            if isinstance(value, dict):
                continue
            values = value if isinstance(value, list) else [value]
            for i, v in enumerate(values):
                sample = i if isinstance(value, list) else None
                yield base | {"metric": metric, "sample": sample, "value": v}


def summarize(records):
    groups = {}
    for row in flatten(r for r in records if not r["warmup"]):
        key = (row["scenario"], row["variant"], row["metric"], row["host"])
        if isinstance(row["value"], bool):
            groups.setdefault(key, []).append(1.0 if row["value"] else 0.0)
        elif isinstance(row["value"], (int, float)):
            groups.setdefault(key, []).append(float(row["value"]))
    return {k: bl.summarize(v) for k, v in groups.items()}


def fmt(v):
    if v is None:
        return "—"
    if abs(v) >= 100:
        return f"{v:.0f}"
    if abs(v) >= 10:
        return f"{v:.1f}"
    return f"{v:.2f}"


def summary_markdown(summary, meta):
    lines = ["# Multiplexer benchmark — summary", ""]
    lines.append(
        f"Run {meta['started']} · reps {meta['reps']} (+{meta['warmup']} warm-up discarded)"
    )
    lines.append("")
    lines.append("```json")
    lines.append(json.dumps({"machine": meta["machine"], "versions": meta["versions"]}, indent=2))
    lines.append("```")
    host_order = meta["hosts"]
    by_scenario = {}
    for (scenario, variant, metric, host), stats in summary.items():
        by_scenario.setdefault(scenario, {}).setdefault((variant, metric), {})[host] = stats
    for scenario in SCENARIOS:
        if scenario not in by_scenario:
            continue
        lines += ["", f"## {scenario}", ""]
        head = (
            "| variant | metric | " + " | ".join(f"{h} median | {h} p95" for h in host_order) + " |"
        )
        lines.append(head)
        lines.append("|" + "---|" * (2 + 2 * len(host_order)))
        for (variant, metric), per_host in sorted(by_scenario[scenario].items()):
            cells = []
            for h in host_order:
                s = per_host.get(h, {})
                cells += [fmt(s.get("median")), fmt(s.get("p95"))]
            lines.append(f"| {variant} | {metric} | " + " | ".join(cells) + " |")
    return "\n".join(lines) + "\n"


def main(argv=None):
    p = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter
    )
    p.add_argument("--hosts", default="tmux,herdr,thurbox")
    p.add_argument("--scenarios", default=",".join(SCENARIOS))
    p.add_argument("--reps", type=int, default=5)
    p.add_argument("--warmup", type=int, default=1)
    p.add_argument(
        "--quick", action="store_true", help="smaller N and shorter windows, for trying the harness"
    )
    p.add_argument("--work", default=os.path.join(CACHE, "work"), help="sandboxes live here")
    p.add_argument(
        "--out", default=None, help="results directory (default: <work>/results-<timestamp>)"
    )
    p.add_argument("--herdr", default=None, help="path to the herdr binary")
    p.add_argument("--thurbox-bin", default=None, help="directory holding thurbox and thurbox-cli")
    args = p.parse_args(argv)
    args.hosts = [h for h in args.hosts.split(",") if h]
    scenarios = [s for s in args.scenarios.split(",") if s]
    for s in scenarios:
        if s not in SCENARIOS:
            sys.exit(f"unknown scenario {s!r}; known: {', '.join(SCENARIOS)}")
    for h in args.hosts:
        if h not in hosts.HOSTS:
            sys.exit(f"unknown host {h!r}; known: {', '.join(hosts.HOSTS)}")

    # Background jobs start with SIGINT ignored, and SIGTERM would end the run
    # without a single `finally`; both are turned into an exit that unwinds
    # through every scenario's teardown, so no server outlives the run.
    signal.signal(signal.SIGINT, signal.default_int_handler)
    signal.signal(signal.SIGTERM, lambda *_: sys.exit(143))

    # The same refusal as run.sh's, for a scenario script run on its own.
    if os.environ.get("THURBOX_GATE") and not os.environ.get("THURBOX_PERF_ALLOW_IN_GATE"):
        sys.exit("run.py: refusing to run inside a validation step (THURBOX_GATE is set)")

    tools = resolve_tools(args)
    ctx = Ctx(args, tools)
    stamp = datetime.datetime.now(datetime.timezone.utc).strftime("%Y%m%dT%H%M%SZ")
    out = args.out or os.path.join(args.work, f"results-{stamp}")
    os.makedirs(out, exist_ok=True)

    meta = {
        "started": stamp,
        "machine": bl.machine(),
        "load_at_start": bl.load(),
        "niceness": os.nice(0),
        "hosts": args.hosts,
        "scenarios": scenarios,
        "reps": args.reps,
        "warmup": args.warmup,
        "quick": args.quick,
        "versions": versions(ctx),
        "thurbox_commit": thurbox_commit(tools),
        "timing": "in-harness, CLOCK_MONOTONIC (hyperfine not used)",
    }
    print(json.dumps(meta, indent=2), flush=True)

    for name in scenarios:
        print(f"== {name} (load {bl.load()})", flush=True)
        started = time.monotonic()
        importlib.import_module(name).run(ctx)
        meta.setdefault("scenario_seconds", {})[name] = round(time.monotonic() - started, 1)
        # Written after every scenario so an interrupted run keeps what it has.
        bl.dump_json(os.path.join(out, "results.json"), {"meta": meta, "records": ctx.records})

    meta["load_at_end"] = bl.load()
    bl.dump_json(os.path.join(out, "results.json"), {"meta": meta, "records": ctx.records})
    with open(os.path.join(out, "results.csv"), "w", newline="") as f:
        w = csv.DictWriter(f, fieldnames=CSV_FIELDS + ["metric", "sample", "value"])
        w.writeheader()
        w.writerows(flatten(ctx.records))
    summary = summarize(ctx.records)
    with open(os.path.join(out, "summary.md"), "w") as f:
        f.write(summary_markdown(summary, meta))
    shutil.rmtree(ctx.root, ignore_errors=True)
    print(f"results in {out}")


if __name__ == "__main__":
    main()
