"""Keystroke to echo, through an attached client, as a user types.

A byte goes into the client's terminal; the time is until the agent's echo of
that byte comes back out of it — the whole round trip a user feels
(terminal -> client -> server -> agent -> server -> client -> terminal).
Keys are spaced 20-60 ms apart at random, so the samples do not lock onto a
host's frame clock. ``idle``: nothing else happening. ``other-busy``: session
2, not on screen, is emitting back-to-back bursts the whole time.

Samples are pooled across repetitions; ``timeouts`` counts keys whose echo
never arrived within 2 s (left out of the percentiles).
"""

import os
import random
import threading
import time

from common import attach, create_ready, focus, usr1

import benchlib as bl


def burster(host, pid, stop):
    path = os.path.join(host.sb.agents, "s2.flood")
    while not stop.is_set():
        try:
            os.remove(path)
        except FileNotFoundError:
            pass
        try:
            usr1(pid)
        except ProcessLookupError:
            return
        while not stop.is_set() and not os.path.exists(path):
            time.sleep(0.005)


def run(ctx):
    samples = 30 if ctx.quick else 100
    rng = random.Random(1)
    for variant in ("idle", "other-busy"):
        for rep, warm in ctx.repetitions():
            for name in ctx.hosts:
                load = bl.load()
                host = ctx.fresh(name)
                client = None
                stop = threading.Event()
                thread = None
                try:
                    create_ready(host, 2)
                    ready = host.wait_ready(["s2"])
                    client, _ = attach(host)
                    focus(host, client)
                    if variant == "other-busy":
                        thread = threading.Thread(target=burster, args=(host, ready["s2"][1], stop))
                        thread.start()
                        client.pump(1.0)
                    values, timeouts = [], 0
                    byte = ord("a")
                    for i in range(samples + 10):
                        byte = ord("a") + (byte - ord("a") + 1) % 26
                        off = len(client.buf)
                        start = bl.now_ns()
                        client.write(bytes([byte]))
                        seen = client.wait_for(bl.token(byte), 2.0, since=off)
                        if i >= 10:  # the first ten warm the path up
                            if seen is None:
                                timeouts += 1
                            else:
                                values.append(bl.ms(seen - start))
                        gap = time.monotonic() + rng.uniform(0.02, 0.06)
                        while time.monotonic() < gap:
                            client.pump(max(0.0, gap - time.monotonic()))
                    ctx.record(
                        "latency",
                        name,
                        variant,
                        rep,
                        warm,
                        load,
                        echo_ms=values,
                        timeouts=timeouts,
                    )
                finally:
                    stop.set()
                    if thread:
                        thread.join()
                    ctx.close(host, client)


if __name__ == "__main__":
    import sys

    import run as entry

    entry.main(["--scenarios", "latency"] + sys.argv[1:])
