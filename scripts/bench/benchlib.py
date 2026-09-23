"""Shared machinery for the multiplexer benchmark: sandboxes, process
accounting, a pty-hosted client, and the statistics every scenario reports.

Nothing here knows about a particular host; ``hosts.py`` does.
"""

import fcntl
import json
import os
import platform
import re
import select
import shutil
import signal
import struct
import subprocess
import termios
import time

HERE = os.path.dirname(os.path.abspath(__file__))
AGENT = os.path.join(HERE, "agent.py")
CLK_TCK = os.sysconf("SC_CLK_TCK")

# The size every client is attached at, and every headless session is created
# at where the host lets us say. One size for all three, so no host draws more
# cells than another.
COLS, ROWS = 200, 50


def now_ns():
    return time.monotonic_ns()


def ms(ns):
    return ns / 1e6


def wait_until(pred, timeout, interval=0.002):
    """Poll ``pred`` until it returns a truthy value; return it, or None."""
    deadline = time.monotonic() + timeout
    while True:
        value = pred()
        if value:
            return value
        if time.monotonic() > deadline:
            return None
        time.sleep(interval)


def token(byte):
    """The echo the stand-in agent draws for one input byte (mirrors
    ``agent.token``)."""
    return "".join(chr(97 + (byte + 7 * k) % 26) for k in range(4)).encode()


# --- statistics ----------------------------------------------------------------


def percentile(values, p):
    """Nearest-rank percentile: always one of the observed values."""
    if not values:
        return None
    ordered = sorted(values)
    rank = max(1, -(-len(ordered) * p // 100))
    return ordered[int(rank) - 1]


def summarize(values):
    values = [v for v in values if v is not None]
    if not values:
        return {"n": 0}
    return {
        "n": len(values),
        "median": percentile(values, 50),
        "p95": percentile(values, 95),
        "min": min(values),
        "max": max(values),
    }


# --- the machine ---------------------------------------------------------------


def machine():
    """What the numbers were measured on — generic, never identifying."""
    info = {"cpus": os.cpu_count(), "kernel": platform.release()}
    try:
        with open("/proc/meminfo") as f:
            total_kb = int(f.readline().split()[1])
        info["ram_gib"] = round(total_kb / 1024 / 1024, 1)
    except OSError:
        pass
    try:
        with open("/etc/os-release") as f:
            fields = dict(line.rstrip("\n").split("=", 1) for line in f if "=" in line)
        info["os"] = fields.get("PRETTY_NAME", "").strip('"')
    except OSError:
        pass
    try:
        with open("/proc/cpuinfo") as f:
            for line in f:
                if line.startswith("model name"):
                    info["cpu_model"] = line.split(":", 1)[1].strip()
                    break
    except OSError:
        pass
    try:
        path = "/sys/devices/system/cpu/cpu0/cpufreq/scaling_governor"
        with open(path) as f:
            info["governor"] = f.read().strip()
    except OSError:
        pass
    return info


def load():
    return [round(x, 2) for x in os.getloadavg()]


# --- process accounting --------------------------------------------------------


def _read(path):
    try:
        with open(path) as f:
            return f.read()
    except OSError:
        return None


def children_map():
    """ppid -> [pid] for every live process."""
    kids = {}
    for entry in os.listdir("/proc"):
        if not entry.isdigit():
            continue
        stat = _read(f"/proc/{entry}/stat")
        if not stat:
            continue
        ppid = int(stat.rsplit(")", 1)[1].split()[1])
        kids.setdefault(ppid, []).append(int(entry))
    return kids


def descendants(pid, kids=None):
    kids = kids if kids is not None else children_map()
    out, stack = [], [pid]
    while stack:
        for child in kids.get(stack.pop(), []):
            out.append(child)
            stack.append(child)
    return out


def comm(pid):
    return (_read(f"/proc/{pid}/comm") or "").strip()


def cmdline(pid):
    raw = _read(f"/proc/{pid}/cmdline") or ""
    return raw.replace("\0", " ").strip()


def cpu_ticks(pid):
    stat = _read(f"/proc/{pid}/stat")
    if not stat:
        return 0
    fields = stat.rsplit(")", 1)[1].split()
    return int(fields[11]) + int(fields[12])


def memory_kib(pid):
    """(rss, pss) in KiB. PSS splits shared pages between their users, so a sum
    of PSS over a process set is that set's real share of memory."""
    rss = pss = 0
    rollup = _read(f"/proc/{pid}/smaps_rollup")
    if rollup:
        for line in rollup.splitlines():
            if line.startswith("Rss:"):
                rss = int(line.split()[1])
            elif line.startswith("Pss:"):
                pss = int(line.split()[1])
    return rss, pss


def memory_of(pids):
    rss = pss = 0
    for pid in pids:
        r, p = memory_kib(pid)
        rss += r
        pss += p
    return {"rss_kib": rss, "pss_kib": pss}


def cpu_over(pids_fn, seconds):
    """CPU seconds a (re-evaluated) set of processes burns over a window, and
    that as a percentage of one core."""
    before = {pid: cpu_ticks(pid) for pid in pids_fn()}
    start = time.monotonic()
    time.sleep(seconds)
    elapsed = time.monotonic() - start
    ticks = 0
    for pid in pids_fn():
        ticks += cpu_ticks(pid) - before.get(pid, 0)
    cpu_s = ticks / CLK_TCK
    return {"cpu_s": cpu_s, "cpu_pct": 100.0 * cpu_s / elapsed}


def kill_tree(pid, sig=signal.SIGKILL):
    for p in [pid] + descendants(pid):
        try:
            os.kill(p, sig)
        except ProcessLookupError:
            pass


# --- the sandbox ---------------------------------------------------------------


class Sandbox:
    """One hermetic world: its own HOME, XDG dirs and runtime dir, so a host
    finds none of the operator's config and leaves nothing outside ``root``."""

    def __init__(self, root, extra_path=()):
        self.root = root
        shutil.rmtree(root, ignore_errors=True)
        dirs = ["home", "config", "data", "state", "cache", "run", "tmux", "work", "agents"]
        for d in dirs:
            os.makedirs(os.path.join(root, d), exist_ok=True)
        os.chmod(os.path.join(root, "run"), 0o700)
        self.work = os.path.join(root, "work")
        self.agents = os.path.join(root, "agents")
        path = os.pathsep.join(list(extra_path) + [os.environ.get("PATH", "")])
        self.env = {
            "PATH": path,
            "HOME": os.path.join(root, "home"),
            "XDG_CONFIG_HOME": os.path.join(root, "config"),
            "XDG_DATA_HOME": os.path.join(root, "data"),
            "XDG_STATE_HOME": os.path.join(root, "state"),
            "XDG_CACHE_HOME": os.path.join(root, "cache"),
            "XDG_RUNTIME_DIR": os.path.join(root, "run"),
            "TMUX_TMPDIR": os.path.join(root, "tmux"),
            "TERM": "xterm-256color",
            "LANG": "C.UTF-8",
            "SHELL": "/bin/sh",
        }
        for key in ("BENCH_FLOOD_LINES", "BENCH_TRICKLE_HZ"):
            if key in os.environ:
                self.env[key] = os.environ[key]

    def run(self, argv, check=True, timeout=60, **kw):
        return subprocess.run(
            argv,
            env=self.env,
            cwd=self.work,
            capture_output=True,
            text=True,
            check=check,
            timeout=timeout,
            **kw,
        )

    def agent_argv(self, name):
        return ["python3", AGENT, name, self.agents]

    def ready(self, name):
        """(monotonic ns, pid) the agent wrote when it started, or None."""
        text = _read(os.path.join(self.agents, f"{name}.ready"))
        if not text:
            return None
        ns, pid = text.split()
        return int(ns), int(pid)

    def flood_times(self, name):
        text = _read(os.path.join(self.agents, f"{name}.flood"))
        if not text:
            return None
        start, end = text.split()
        return int(start), int(end)

    def forget(self, name):
        for suffix in ("ready", "flood"):
            try:
                os.remove(os.path.join(self.agents, f"{name}.{suffix}"))
            except FileNotFoundError:
                pass

    def destroy(self):
        shutil.rmtree(self.root, ignore_errors=True)


# --- a client on a pseudo-terminal ---------------------------------------------

ANSI = re.compile(rb"\x1b\[[0-?]*[ -/]*[@-~]|\x1b\][^\x07\x1b]*(\x07|\x1b\\)|\x1b[@-Z\\-_]")


class PtyClient:
    """An interactive client (``tmux attach``, ``herdr``, ``thurbox``) running
    on a pty of ``COLS``x``ROWS``, the way a user's terminal would host it.

    Everything it prints is kept, so a scenario can ask "has X appeared since
    offset N" without racing the reader.
    """

    def __init__(self, argv, env, cwd, cols=COLS, rows=ROWS):
        self.master, slave = os.openpty()
        fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", rows, cols, 0, 0))
        self.proc = subprocess.Popen(
            argv,
            env=env,
            cwd=cwd,
            stdin=slave,
            stdout=slave,
            stderr=slave,
            start_new_session=True,
            # The pty becomes the controlling terminal, as a login shell's is.
            # Safe here: every client is started before any thread exists.
            preexec_fn=lambda: fcntl.ioctl(0, termios.TIOCSCTTY, 0),  # noqa: PLW1509
        )
        os.close(slave)
        self.buf = bytearray()
        self.stripped = bytearray()
        self._answer_queries = True

    @property
    def pid(self):
        return self.proc.pid

    def pump(self, timeout=0.0):
        """Read whatever is waiting; returns bytes read."""
        got = 0
        while True:
            r, _, _ = select.select([self.master], [], [], timeout)
            if not r:
                return got
            try:
                data = os.read(self.master, 65536)
            except OSError:
                return got
            if not data:
                return got
            got += len(data)
            self.buf += data
            self._answer(data)
            timeout = 0.0

    def _answer(self, data):
        # A TUI asks its terminal questions at startup and some wait for the
        # answers. This one answers what an xterm-class terminal answers, so no
        # client sits out a timeout a user's terminal would never cause. It
        # deliberately claims nothing newer (no kitty keyboard protocol, no
        # colour-scheme reports): a client that believed it would change how it
        # reads the keys the latency scenario types.
        replies = []
        if b"\x1b[6n" in data:  # cursor position
            replies.append(b"\x1b[1;1R")
        if b"\x1b[c" in data or b"\x1b[0c" in data:  # primary device attributes
            replies.append(b"\x1b[?62;22c")
        if b"\x1b[16t" in data:  # cell size in pixels
            replies.append(b"\x1b[6;20;10t")
        for index in re.findall(rb"\x1b\]4;(\d+);\?", data):  # palette entries
            replies.append(b"\x1b]4;" + index + b";rgb:8080/8080/8080\x1b\\")
        for code in re.findall(rb"\x1b\](1[01]);\?", data):  # foreground / background
            colour = b"ffff/ffff/ffff" if code == b"10" else b"0000/0000/0000"
            replies.append(b"\x1b]" + code + b";rgb:" + colour + b"\x1b\\")
        if replies:
            os.write(self.master, b"".join(replies))

    def wait_for(self, pattern, timeout, since=0):
        """Monotonic ns at which ``pattern`` (bytes) first appeared in the output
        after offset ``since`` (escape sequences ignored), or None."""
        deadline = time.monotonic() + timeout
        while True:
            if pattern in ANSI.sub(b"", bytes(self.buf[since:])):
                return now_ns()
            left = deadline - time.monotonic()
            if left <= 0:
                return None
            self.pump(min(left, 0.05))

    def write(self, data):
        os.write(self.master, data)

    def alive(self):
        return self.proc.poll() is None

    def close(self):
        if self.alive():
            kill_tree(self.proc.pid, signal.SIGKILL)
        try:
            self.proc.wait(timeout=5)
        except subprocess.TimeoutExpired:
            pass
        try:
            os.close(self.master)
        except OSError:
            pass


def dump_json(path, obj):
    with open(path + ".tmp", "w") as f:
        json.dump(obj, f, indent=2, sort_keys=True)
        f.write("\n")
    os.replace(path + ".tmp", path)
