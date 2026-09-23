"""Steps more than one scenario takes."""

import os
import signal
import sys
import time

# Each scenario also runs as a script of its own, from this directory, and the
# shared machinery lives one level up.
sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))

import benchlib as bl

FLOOD_LINES = int(os.environ.get("BENCH_FLOOD_LINES", "50000"))


def names(n):
    return [f"s{i + 1}" for i in range(n)]


def create_ready(host, n, timeout=180):
    """Create ``n`` sessions one after another, as a script would, and wait for
    every agent. Returns (t0, per-command durations in ns, ready map)."""
    t0 = bl.now_ns()
    durations = []
    for name in names(n):
        start = bl.now_ns()
        host.create(name)
        durations.append(bl.now_ns() - start)
    ready = host.wait_ready(host.names, timeout)
    if ready is None:
        raise RuntimeError(f"{host.name}: not every agent started within {timeout}s")
    return t0, durations, ready


def attach(host, timeout=30):
    """Start a client on a pty; return (client, ns until it drew session 1)."""
    announce(host)
    t0 = bl.now_ns()
    client = bl.PtyClient(host.client_argv(), host.sb.env, host.sb.work)
    seen = client.wait_for(host.client_ready_marker(), timeout)
    if seen is None:
        client.close()
        raise RuntimeError(f"{host.name}: client never drew the first session")
    return client, seen - t0


def attach_after_output(host, grace=5):
    """Attach to sessions that have already printed a lot. Returns (client,
    stale): ``stale`` is whether the client's first view of session 1 lacked
    its latest line for ``grace`` seconds, so that the agent had to print
    again before the client showed it."""
    announce(host)
    client = bl.PtyClient(host.client_argv(), host.sb.env, host.sb.work)
    if client.wait_for(host.client_ready_marker(), grace):
        return client, False
    off = len(client.buf)
    announce(host)
    if client.wait_for(host.client_ready_marker(), 30, since=off):
        return client, True
    client.close()
    raise RuntimeError(f"{host.name}: client never drew the first session")


def announce(host):
    """Have session 1 print its banner again — output since it started may
    have scrolled it away, and the banner is how a client is seen to draw it.
    Done outside any timed window."""
    ready = host.sb.ready(host.names[0])
    if ready and ready[1] in host.agent_pids():
        signal_agents([ready[1]], signal.SIGRTMIN)
        time.sleep(0.3)


def focus(host, client, tries=10):
    """Put the keyboard on session 1's pane and prove it with one echo."""
    host.prepare_client(client)
    client.pump(0.3)
    for _ in range(tries):
        off = len(client.buf)
        client.write(b"a")
        if client.wait_for(bl.token(ord("a")), 1.0, since=off):
            return True
    raise RuntimeError(f"{host.name}: keystrokes never reached session 1")


def signal_agents(pids, sig):
    for pid in pids:
        try:
            os.kill(pid, sig)
        except ProcessLookupError:
            pass


def alive(pids):
    return sum(1 for pid in pids if os.path.exists(f"/proc/{pid}") and bl.comm(pid) != "")


def host_ticks(host, client=None):
    return sum(bl.cpu_ticks(p) for p in host.host_pids(client))


def wait_quiet(host, client=None, timeout=60, step=0.1, quiet_steps=3):
    """Monotonic ns at which the host stopped burning CPU: the start of the
    first run of ``quiet_steps`` samples ``step`` apart with no new ticks.
    A client, when there is one, is pumped meanwhile so it never blocks."""
    deadline = time.monotonic() + timeout
    last = host_ticks(host, client)
    quiet_since, streak = None, 0
    while time.monotonic() < deadline:
        if client is not None:
            client.pump(step)
        else:
            time.sleep(step)
        now = host_ticks(host, client)
        if now == last:
            streak += 1
            quiet_since = quiet_since or bl.now_ns()
            if streak >= quiet_steps:
                return quiet_since
        else:
            streak, quiet_since = 0, None
        last = now
    return None


def pump_for(client, seconds):
    end = time.monotonic() + seconds
    while time.monotonic() < end:
        client.pump(min(0.05, max(0.0, end - time.monotonic())))


def flood_numbers(text, name):
    """The line numbers of a flood found in captured text. Soft wraps are
    removed first, so a host that captures an 80-column window reads the same
    as one that joins wrapped lines."""
    import re

    flat = text.replace("\r", "").replace("\n", "")
    return [int(m) for m in re.findall(rf"{name} (\d{{7}}) lorem", flat)]


def mib(kib):
    return kib / 1024.0


def usr1(pid):
    os.kill(pid, signal.SIGUSR1)
