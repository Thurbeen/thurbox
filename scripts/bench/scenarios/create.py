"""Create N sessions from cold, one after another, as a script would.

From nothing running: the host's first command also starts its server, so the
N=1 row is the cold start to a first usable session. ``total_ms`` runs until
the last agent has started; ``last_create_ms`` is how long the host's own
command took for the Nth session, which is where a per-session cost that grows
with N shows.
"""

from common import create_ready

import benchlib as bl


def run(ctx):
    counts = (1, 5) if ctx.quick else (1, 5, 20, 50)
    for n in counts:
        for rep, warm in ctx.repetitions():
            for name in ctx.hosts:
                load = bl.load()
                host = ctx.fresh(name)
                try:
                    t0, durations, ready = create_ready(host, n)
                    last = max(r[0] for r in ready.values())
                    ctx.record(
                        "create",
                        name,
                        f"N={n}",
                        rep,
                        warm,
                        load,
                        total_ms=bl.ms(last - t0),
                        first_ready_ms=bl.ms(ready["s1"][0] - t0),
                        last_create_ms=bl.ms(durations[-1]),
                        mean_create_ms=bl.ms(sum(durations) / n),
                    )
                finally:
                    ctx.close(host)


if __name__ == "__main__":
    import sys

    import run as entry

    entry.main(["--scenarios", "create"] + sys.argv[1:])
