#!/usr/bin/env python3
"""The stand-in agent every host runs, so the benchmark measures the host.

One process per session, identical on every host. It never decides anything; it
only does what the harness asks, and records *when* it did it on the system-wide
monotonic clock, which the harness reads too.

    agent.py <name> <state-dir>

- On start it writes ``<state-dir>/<name>.ready`` (the monotonic ns it started)
  and prints a banner.
- Every byte on stdin is answered with a token redrawn in place (``\\r`` + four
  letters) that is a function of the byte alone. The harness types bytes whose
  consecutive tokens differ in every letter, so a renderer that only repaints
  changed cells still has to emit all four, contiguously — which is what lets
  the latency scenario find the echo in a client's output stream.
- ``SIGUSR1`` writes ``BENCH_FLOOD_LINES`` lines as fast as the pty takes them,
  then a ``FLOOD-END`` marker; ``<name>.flood`` records the start and end.
- ``SIGUSR2`` toggles a steady trickle of ``BENCH_TRICKLE_HZ`` lines a second.
- ``SIGRTMIN`` prints the banner again, so a client attaching after output
  has scrolled it away still has it to draw.

Signals rather than typed commands, so starting a burst does not itself go
through the input path of the host being measured.
"""

import os
import select
import signal
import sys
import termios
import time
import tty

NAME, STATE = sys.argv[1], sys.argv[2]
FLOOD_LINES = int(os.environ.get("BENCH_FLOOD_LINES", "50000"))
TRICKLE_HZ = float(os.environ.get("BENCH_TRICKLE_HZ", "10"))
# 100 bytes a line, the width of a typical log line; no escape sequences, so a
# host's parser does the same work whatever it is.
FILLER = (
    "lorem ipsum dolor sit amet consectetur adipiscing elit sed do eiusmod tempor incididunt ut lab"
)

trickle_on = False
trickle_count = 0


def write(text):
    data = text.encode()
    while data:
        n = os.write(1, data)
        data = data[n:]


def mark(suffix, text):
    path = os.path.join(STATE, f"{NAME}.{suffix}")
    with open(path + ".tmp", "w") as f:
        f.write(text)
    os.replace(path + ".tmp", path)


def on_usr1(_sig, _frame):
    # Python runs a handler between bytecodes of the main thread, not in the
    # async-signal context, so doing the work here is safe — and immediate,
    # where a flag would wait out the select() that PEP 475 restarts.
    flood()


def on_usr2(_sig, _frame):
    global trickle_on
    trickle_on = not trickle_on


def on_rtmin(_sig, _frame):
    write(f"\r\n{banner()}\r\n")


def banner():
    return f"AGENT-{NAME}-READY pid={os.getpid()}"


def flood():
    start = time.monotonic_ns()
    chunk = []
    for i in range(FLOOD_LINES):
        chunk.append(f"{NAME} {i:07d} {FILLER}\r\n")
        if len(chunk) == 64:
            write("".join(chunk))
            chunk.clear()
    write("".join(chunk))
    write(f"FLOOD-END-{NAME}-{FLOOD_LINES}\r\n")
    mark("flood", f"{start} {time.monotonic_ns()}\n")


def token(n):
    return "".join(chr(97 + (n + 7 * k) % 26) for k in range(4))


def main():
    global trickle_count
    signal.signal(signal.SIGUSR1, on_usr1)
    signal.signal(signal.SIGUSR2, on_usr2)
    signal.signal(signal.SIGRTMIN, on_rtmin)
    tty_in = os.isatty(0)
    saved = termios.tcgetattr(0) if tty_in else None
    if tty_in:
        tty.setraw(0)
    mark("ready", f"{time.monotonic_ns()} {os.getpid()}\n")
    write(f"{banner()}\r\n")
    try:
        while True:
            timeout = 1.0 / TRICKLE_HZ if trickle_on else 0.5
            try:
                readable, _, _ = select.select([0], [], [], timeout)
            except InterruptedError:
                continue
            if readable:
                data = os.read(0, 64)
                if not data:
                    return
                for byte in data:
                    write("\r" + token(byte))
            elif trickle_on:
                trickle_count += 1
                write(f"\r\n{NAME} trickle {trickle_count:07d} {FILLER}")
    finally:
        if saved is not None:
            termios.tcsetattr(0, termios.TCSADRAIN, saved)


if __name__ == "__main__":
    main()
