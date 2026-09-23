"""Memory and CPU of the host with N sessions: at rest and under output,
headless and with a client attached.

Memory is the PSS of every host process (the agents excluded — see
``hosts.py``), so a shared library is counted once. CPU is the host's CPU time
over a fixed window as a percentage of one core. "Output" is every session
printing ``BENCH_TRICKLE_HZ`` (default 10) lines a second — a room of busy
agents, not a flood. With a client attached it shows session 1.

``first_view_stale`` (attached idle) records whether the client, attaching
after that output, first showed session 1 without its latest line until the
agent printed again — a correctness finding, not a cost.

``idle-long`` is one 65 s window at N=20: periodic work (a 60 s heartbeat, a
detection poll) can fall between the short windows.
"""

import os
import signal
import time

from common import attach_after_output, create_ready, focus, mib, signal_agents

import benchlib as bl


def measure(host, client, window):
    pids = lambda: host.host_pids(client)
    mem = bl.memory_of(pids())
    cpu = bl.cpu_over(pids, window) if client is None else pumped_cpu(pids, client, window)
    return {
        "pss_mib": mib(mem["pss_kib"]),
        "rss_mib": mib(mem["rss_kib"]),
        "cpu_pct": cpu["cpu_pct"],
    }


def pumped_cpu(pids_fn, client, window):
    """cpu_over, but draining the client's terminal meanwhile — a client whose
    output nobody reads stalls, which would flatter it."""
    before = {pid: bl.cpu_ticks(pid) for pid in pids_fn()}
    start = time.monotonic()
    while time.monotonic() - start < window:
        client.pump(0.05)
    elapsed = time.monotonic() - start
    ticks = sum(bl.cpu_ticks(pid) - before.get(pid, 0) for pid in pids_fn())
    cpu_s = ticks / bl.CLK_TCK
    return {"cpu_s": cpu_s, "cpu_pct": 100.0 * cpu_s / elapsed}


def run(ctx):
    counts = (1, 5) if ctx.quick else (1, 20, 50)
    window = 3.0 if ctx.quick else 10.0
    for n in counts:
        for rep, warm in ctx.repetitions():
            for name in ctx.hosts:
                load = bl.load()
                host = ctx.fresh(name)
                client = None
                try:
                    create_ready(host, n)
                    agents = host.agent_pids()
                    time.sleep(2.0)
                    ctx.record(
                        "resources",
                        name,
                        f"N={n} headless idle",
                        rep,
                        warm,
                        load,
                        **measure(host, None, window),
                    )
                    signal_agents(agents, signal.SIGUSR2)
                    time.sleep(1.0)
                    ctx.record(
                        "resources",
                        name,
                        f"N={n} headless output",
                        rep,
                        warm,
                        load,
                        **measure(host, None, window),
                    )
                    signal_agents(agents, signal.SIGUSR2)
                    client, stale = attach_after_output(host)
                    focus(host, client)
                    pump = time.monotonic()
                    while time.monotonic() - pump < 3.0:
                        client.pump(0.05)
                    ctx.record(
                        "resources",
                        name,
                        f"N={n} attached idle",
                        rep,
                        warm,
                        load,
                        first_view_stale=stale,
                        **measure(host, client, window),
                    )
                    signal_agents(agents, signal.SIGUSR2)
                    client.pump(1.0)
                    ctx.record(
                        "resources",
                        name,
                        f"N={n} attached output",
                        rep,
                        warm,
                        load,
                        **measure(host, client, window),
                    )
                    signal_agents(agents, signal.SIGUSR2)
                finally:
                    ctx.close(host, client)
    if ctx.quick or os.environ.get("BENCH_SKIP_LONG_IDLE"):
        return
    for rep, warm in ctx.repetitions():
        for name in ctx.hosts:
            load = bl.load()
            host = ctx.fresh(name)
            try:
                create_ready(host, 20)
                time.sleep(2.0)
                ctx.record(
                    "resources",
                    name,
                    "N=20 headless idle-long",
                    rep,
                    warm,
                    load,
                    **measure(host, None, 65.0),
                )
            finally:
                ctx.close(host)


if __name__ == "__main__":
    import sys

    import run as entry

    entry.main(["--scenarios", "resources"] + sys.argv[1:])
