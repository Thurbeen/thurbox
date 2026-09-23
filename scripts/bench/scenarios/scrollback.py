"""What holding and reading back a session's history costs, headless.

After one burst of ``BENCH_FLOOD_LINES`` lines: ``retained_lines`` is how many
of them the host still has (each host's default history limit decides it),
``readable_lines`` how many one read through the host's CLI hands back — the
way a script or an agent searches it — and ``read_all_ms`` the median of five
such reads. ``held_mib`` is the host's PSS growth over its size before the
burst. Retention differs by default, so read the time and memory next to the
lines they bought.

Herdr's CLI returns at most 1000 lines a read, so its ``retained_lines`` comes
from the scroll metrics it reports instead (one 110-byte line is one row at
the 200-column headless size, so rows are lines there).
"""

import time

from common import create_ready, flood_numbers, mib, usr1, wait_quiet

import benchlib as bl


def run(ctx):
    for rep, warm in ctx.repetitions():
        for name in ctx.hosts:
            load = bl.load()
            host = ctx.fresh(name)
            try:
                create_ready(host, 1)
                agent = host.agent_pids()[0]
                time.sleep(1.0)
                before = bl.memory_of(host.host_pids())["pss_kib"]
                usr1(agent)
                if bl.wait_until(lambda h=host: h.sb.flood_times("s1"), 180, interval=0.01) is None:
                    raise RuntimeError(f"{name}: the flood never finished")
                wait_quiet(host)
                after = bl.memory_of(host.host_pids())["pss_kib"]
                reads, text = [], ""
                for _ in range(5):
                    start = bl.now_ns()
                    text = host.capture("s1", None)
                    reads.append(bl.ms(bl.now_ns() - start))
                readable = len(set(flood_numbers(text, "s1")))
                held = host.held_rows("s1")
                ctx.record(
                    "scrollback",
                    name,
                    "after 1 burst",
                    rep,
                    warm,
                    load,
                    retained_lines=readable if held is None else held,
                    readable_lines=readable,
                    held_mib=mib(after - before),
                    read_all_ms=bl.percentile(reads, 50),
                )
            finally:
                ctx.close(host)


if __name__ == "__main__":
    import sys

    import run as entry

    entry.main(["--scenarios", "scrollback"] + sys.argv[1:])
