"""Attach a client with N sessions running, detach, attach again.

``attach_ms``: from starting the client until it has drawn session 1's agent.
``detach_ms``: from the host's own leave key until the client process is gone
(thurbox's is Quit — the interface exits, tmux keeps the sessions).
``reattach_ms``: a second client, 3 s after the first left, with everything in
the page cache. (The gap matters for Herdr: one attaching within about a
second of a detach took ~280 ms on 0.9.1 instead of ~50.)
``survivors``: agents still running after the detach, out of N.
"""

import time

from common import alive, attach, create_ready, pump_for

import benchlib as bl


def run(ctx):
    counts = (1, 5) if ctx.quick else (1, 20, 50)
    for n in counts:
        for rep, warm in ctx.repetitions():
            for name in ctx.hosts:
                load = bl.load()
                host = ctx.fresh(name)
                client = None
                try:
                    create_ready(host, n)
                    agents = host.agent_pids()
                    time.sleep(1.0)
                    client, attach_ns = attach(host)
                    pump_for(client, 1.0)
                    start = bl.now_ns()
                    client.write(host.detach_keys)
                    # Pumped while waiting, so a client flushing its last frame
                    # to the terminal is never the thing holding it up.
                    deadline = time.monotonic() + 10
                    while client.alive() and time.monotonic() < deadline:
                        client.pump(0.001)
                    detach_ns = None if client.alive() else bl.now_ns() - start
                    client.close()
                    time.sleep(3.0)
                    survivors = alive(agents)
                    client, reattach_ns = attach(host)
                    ctx.record(
                        "attach",
                        name,
                        f"N={n}",
                        rep,
                        warm,
                        load,
                        attach_ms=bl.ms(attach_ns),
                        detach_ms=bl.ms(detach_ns) if detach_ns else None,
                        reattach_ms=bl.ms(reattach_ns),
                        survivors=survivors,
                    )
                finally:
                    ctx.close(host, client)


if __name__ == "__main__":
    import sys

    import run as entry

    entry.main(["--scenarios", "attach"] + sys.argv[1:])
