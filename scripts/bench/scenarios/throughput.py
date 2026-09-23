"""One session emits a burst of ``BENCH_FLOOD_LINES`` (default 50 000) lines of
about 107 bytes (5.4 MB) as fast as its pty takes them.

``producer_ms``: the agent's own clock from first write to last write
returning — how fast the host drains the pty (a slow reader back-pressures
the writer). ``settle_ms``: from the first write until the host's processes
stop using CPU. ``visible_ms`` (attached only): until the end marker reaches
the client's terminal. ``host_cpu_s``: CPU the host spent on the burst.
``intact``: the last lines the host reports (up to 120 rows) are the last
lines written, in order — nothing dropped at the tail. Earlier lines are not
checked here: each host's history limit has already let most of them go. ``pss_after_mib``: host memory after.
"""

import time

from common import (
    FLOOD_LINES,
    attach,
    create_ready,
    flood_numbers,
    focus,
    host_ticks,
    mib,
    usr1,
    wait_quiet,
)

import benchlib as bl


def run(ctx):
    for variant in ("headless", "attached"):
        for rep, warm in ctx.repetitions():
            for name in ctx.hosts:
                load = bl.load()
                host = ctx.fresh(name)
                client = None
                try:
                    create_ready(host, 1)
                    agent = host.agent_pids()[0]
                    if variant == "attached":
                        client, _ = attach(host)
                        focus(host, client)
                        client.pump(1.0)
                    time.sleep(1.0)
                    ticks0 = host_ticks(host, client)
                    off = len(client.buf) if client else 0
                    usr1(agent)
                    visible = None
                    marker = f"FLOOD-END-s1-{FLOOD_LINES}".encode()
                    if client is not None:
                        visible = client.wait_for(marker, 180, since=off)
                    flood = bl.wait_until(lambda h=host: h.sb.flood_times("s1"), 180, interval=0.01)
                    if flood is None:
                        raise RuntimeError(f"{name}: the flood never finished")
                    quiet = wait_quiet(host, client)
                    cpu_s = (host_ticks(host, client) - ticks0) / bl.CLK_TCK
                    tail = host.capture("s1", 120)
                    numbers = flood_numbers(tail, "s1")
                    intact = (
                        marker.decode() in tail.replace("\n", "")
                        and len(numbers) >= 20
                        and numbers[-1] == FLOOD_LINES - 1
                        and numbers == list(range(numbers[0], numbers[0] + len(numbers)))
                    )
                    mem = bl.memory_of(host.host_pids(client))
                    ctx.record(
                        "throughput",
                        name,
                        variant,
                        rep,
                        warm,
                        load,
                        producer_ms=bl.ms(flood[1] - flood[0]),
                        settle_ms=bl.ms(quiet - flood[0]) if quiet else None,
                        visible_ms=bl.ms(visible - flood[0]) if visible else None,
                        host_cpu_s=cpu_s,
                        intact=intact,
                        pss_after_mib=mib(mem["pss_kib"]),
                    )
                finally:
                    ctx.close(host, client)


if __name__ == "__main__":
    import sys

    import run as entry

    entry.main(["--scenarios", "throughput"] + sys.argv[1:])
