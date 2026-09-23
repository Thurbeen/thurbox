"""The three hosts, behind one interface.

Each adapter creates sessions the way that host's own documentation says to do
it headlessly, and nothing cleverer: ``tmux new-window``, Herdr's ``tab create``
plus ``pane run``, ``thurbox-cli session create --command``. The scenarios only
ever talk to this interface, so a number in the report is never one host's
special path against another's general one.

What a host "is", for accounting, is ``host_pids``: every process in the
host's trees except the stand-in agents, which report their own pids. So a
shell or helper a host keeps beside its sessions is the host's cost, and the
agents — identical everywhere — are nobody's.
"""

import json
import os
import shlex
import signal
import subprocess

import benchlib as bl

SESSION = "bench"


class Host:
    name = "?"
    #: What a user presses to leave the client with the sessions still running.
    detach_keys = b""

    def __init__(self, sandbox, tools):
        self.sb = sandbox
        self.tools = tools
        self.names = []

    # -- lifecycle ------------------------------------------------------------

    def create(self, name):
        """Create one session running the stand-in agent. Returns when the
        host's own command returns; the agent may not have started yet."""
        raise NotImplementedError

    def teardown(self):
        raise NotImplementedError

    def kill_server(self):
        """SIGKILL the host's long-lived process(es), as a crash would."""
        for pid in self.server_pids():
            try:
                os.kill(pid, signal.SIGKILL)
            except ProcessLookupError:
                pass

    def recover(self):
        """Do what a user does after the host died: start it again the normal
        way. Returns the client started for it, if recovery needs one."""
        raise NotImplementedError

    # -- clients --------------------------------------------------------------

    def client_argv(self):
        raise NotImplementedError

    def client_ready_marker(self):
        """Bytes that mean the client has drawn the first session's pane."""
        return f"AGENT-{self.names[0]}-READY".encode()

    def prepare_client(self, client):
        """Whatever keystrokes put input focus on the first session's pane."""

    # -- observation ----------------------------------------------------------

    def server_pids(self):
        raise NotImplementedError

    def agent_pids(self):
        """The stand-in agents, by the pid each one wrote when it started."""
        out = []
        for name in self.names:
            ready = self.sb.ready(name)
            if ready and os.path.exists(f"/proc/{ready[1]}"):
                out.append(ready[1])
        return out

    def host_pids(self, client=None):
        """Every process that exists because of the host rather than because
        of an agent: the server(s), whatever they keep running beside the
        sessions, and an attached client with anything it spawned."""
        kids = bl.children_map()
        roots = list(self.server_pids())
        if client is not None and client.alive():
            roots.append(client.pid)
        tree = []
        for pid in roots:
            tree += [pid] + bl.descendants(pid, kids)
        agents = set(self.agent_pids())
        return sorted({p for p in tree if p not in agents})

    def wait_ready(self, names, timeout=60):
        """{name: (ns, pid)} once every named agent has started, else None."""

        def all_ready():
            got = {n: self.sb.ready(n) for n in names}
            return got if all(got.values()) else None

        return bl.wait_until(all_ready, timeout)

    def capture(self, name, lines):
        """The last ``lines`` lines of the session's history, as text."""
        raise NotImplementedError

    def versions(self):
        raise NotImplementedError

    def layout_after_restart(self):
        """{"listed": sessions the host lists after ``recover``}."""
        raise NotImplementedError

    def held_rows(self, name):
        """Rows of history the host says it holds, when it can say so but not
        hand them all back (Herdr); None when a full read is the answer."""
        return

    def _tmux_server_pid(self, base):
        """The pid of the tmux server on ``base``'s socket, asked once and then
        remembered while it lives: asking spawns a tmux client, which would
        otherwise land inside every measurement that looks the server up."""
        cached = getattr(self, "_server_pid", None)
        if cached and os.path.exists(f"/proc/{cached}"):
            return [cached]
        out = self.sb.run(base + ["display-message", "-p", "#{pid}"], check=False)
        if out.returncode != 0 or not out.stdout.strip():
            return []
        self._server_pid = int(out.stdout)
        return [self._server_pid]


# --- raw tmux ------------------------------------------------------------------


class Tmux(Host):
    name = "tmux"
    detach_keys = b"\x02d"  # prefix, d

    def __init__(self, sandbox, tools):
        super().__init__(sandbox, tools)
        # `-f /dev/null`: tmux's defaults, not whatever ~/.tmux.conf a machine has.
        self.base = [tools["tmux"], "-L", "bench-raw", "-f", "/dev/null"]

    def tmux(self, *args, check=True):
        return self.sb.run(self.base + list(args), check=check)

    def create(self, name):
        agent = self.sb.agent_argv(name)
        if not self.names:
            self.tmux(
                "new-session",
                "-d",
                "-s",
                SESSION,
                "-n",
                name,
                "-x",
                str(bl.COLS),
                "-y",
                str(bl.ROWS),
                *agent,
            )
        else:
            self.tmux("new-window", "-d", "-t", SESSION, "-n", name, *agent)
        self.names.append(name)

    def server_pids(self):
        return self._tmux_server_pid(self.base)

    def client_argv(self):
        return self.base + ["attach-session", "-t", f"{SESSION}:{self.names[0]}"]

    def capture(self, name, lines):
        start = "-" if lines is None else f"-{lines}"
        return self.tmux("capture-pane", "-p", "-J", "-t", f"{SESSION}:{name}", "-S", start).stdout

    def teardown(self):
        pids = self.server_pids()
        self.tmux("kill-server", check=False)
        for pid in pids:
            bl.kill_tree(pid)

    def recover(self):
        # tmux has no memory of its sessions once its server is gone.
        return None

    def layout_after_restart(self):
        out = self.tmux("list-windows", "-a", check=False)
        return {"listed": len(out.stdout.splitlines()) if out.returncode == 0 else 0}

    def versions(self):
        return {"tmux": self.sb.run([self.tools["tmux"], "-V"]).stdout.strip()}


# --- Herdr ---------------------------------------------------------------------


HERDR_CONFIG = f"""\
# Hermetic benchmark config. `onboarding = false` is what finishing the
# first-run welcome writes, answered here for the same reason thurbox's v2
# question is. The headless size matches the size tmux sessions are created at.
# No network. Everything else default.
onboarding = false

[server]
headless_cols = {bl.COLS}
headless_rows = {bl.ROWS}

[update]
version_check = false
manifest_check = false
"""


class Herdr(Host):
    name = "herdr"
    detach_keys = b"\x02q"  # ctrl+b q, per Herdr's README

    def __init__(self, sandbox, tools):
        super().__init__(sandbox, tools)
        self.bin = tools["herdr"]
        env = sandbox.env
        env["HERDR_SESSION"] = SESSION
        env["HERDR_DISABLE_SOUND"] = "1"
        conf = os.path.join(env["XDG_CONFIG_HOME"], "herdr")
        os.makedirs(conf, exist_ok=True)
        with open(os.path.join(conf, "config.toml"), "w") as f:
            f.write(HERDR_CONFIG)
        self.server = None
        self.panes = {}
        self.workspace = None

    def herdr(self, *args, check=True):
        return self.sb.run([self.bin] + list(args), check=check)

    def api_ready(self):
        return self.herdr("workspace", "list", check=False).returncode == 0

    def start_server(self):
        self.server = subprocess.Popen(
            [self.bin, "--session", SESSION, "server"],
            env=self.sb.env,
            cwd=self.sb.work,
            stdin=subprocess.DEVNULL,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
            start_new_session=True,
        )
        if not bl.wait_until(self.api_ready, 30, interval=0.005):
            raise RuntimeError("herdr server API never came up")

    def create(self, name):
        if self.server is None or self.server.poll() is not None:
            self.start_server()
        if self.workspace is None:
            out = self.herdr(
                "workspace", "create", "--cwd", self.sb.work, "--label", SESSION, "--no-focus"
            )
            result = json.loads(out.stdout)["result"]
            self.workspace = result["workspace"]["workspace_id"]
        else:
            out = self.herdr(
                "tab",
                "create",
                "--workspace",
                self.workspace,
                "--cwd",
                self.sb.work,
                "--label",
                name,
                "--no-focus",
            )
            result = json.loads(out.stdout)["result"]
        pane = result["root_pane"]["pane_id"]
        # `exec` so the pane's shell becomes the agent, leaving the same one
        # process per session that the other two hosts run.
        self.herdr("pane", "run", pane, "exec " + shlex.join(self.sb.agent_argv(name)))
        self.panes[name] = pane
        self.names.append(name)

    def server_pids(self):
        if self.server is not None and self.server.poll() is None:
            return [self.server.pid]
        return []

    def client_argv(self):
        return [self.bin, "--session", SESSION]

    def held_rows(self, name):
        pane = json.loads(self.herdr("pane", "get", self.panes[name]).stdout)["result"]["pane"]
        return pane["scroll"]["max_offset_from_bottom"] + pane["scroll"]["viewport_rows"]

    def capture(self, name, lines):
        # A read returns at most 1000 lines however many are asked for (measured
        # on 0.9.1); `held_rows` is how the scrollback scenario sees the rest.
        args = ["pane", "read", self.panes[name], "--source", "recent-unwrapped"]
        args += ["--lines", str(lines if lines is not None else 10_000_000)]
        return self.herdr(*args).stdout

    def kill_server(self):
        super().kill_server()
        if self.server is not None:
            self.server.wait()

    def teardown(self):
        if self.server is not None:
            self.herdr("server", "stop", check=False)
            bl.kill_tree(self.server.pid)
            try:
                self.server.wait(timeout=5)
            except subprocess.TimeoutExpired:
                pass

    def recover(self):
        self.start_server()

    def layout_after_restart(self):
        out = self.herdr("pane", "list", check=False)
        if out.returncode != 0:
            return {"listed": 0}
        return {"listed": len(json.loads(out.stdout)["result"]["panes"])}

    def versions(self):
        return {"herdr": self.herdr("--version").stdout.strip()}


# --- thurbox -------------------------------------------------------------------


THURBOX_SETTINGS = """\
# Hermetic benchmark config: the two switches that reach the network are off.
# Everything else is thurbox's default.
[features]
version_check = false
auto_update = false
"""


class Thurbox(Host):
    name = "thurbox"
    # Ctrl+Q, the kernel's reserved quit: the interface exits and tmux keeps
    # the sessions.
    detach_keys = b"\x11"

    def __init__(self, sandbox, tools):
        super().__init__(sandbox, tools)
        env = sandbox.env
        env["THURBOX_CONFIG_DIR"] = os.path.join(env["XDG_CONFIG_HOME"], "thurbox")
        env["THURBOX_DATA_DIR"] = os.path.join(env["XDG_DATA_HOME"], "thurbox")
        # Named outright, like scripts/dev/lib/sandbox-env.sh does, so teardown
        # can find it by name.
        env["THURBOX_SOCKET"] = "bench-thurbox"
        os.makedirs(env["THURBOX_CONFIG_DIR"], exist_ok=True)
        os.makedirs(env["THURBOX_DATA_DIR"], exist_ok=True)
        with open(os.path.join(env["THURBOX_CONFIG_DIR"], "settings.toml"), "w") as f:
            f.write(THURBOX_SETTINGS)
        self.cli = tools["thurbox-cli"]
        self.tui = tools["thurbox"]
        self.tmux_base = [tools["tmux"], "-L", "bench-thurbox"]
        # The one-time "this is the v2 interface" question a profile is asked on
        # its first launch. A returning user never sees it, so it is answered
        # here rather than timed.
        self.thurbox("config", "accept-interface")

    def thurbox(self, *args, check=True):
        return self.sb.run([self.cli] + list(args), check=check)

    def create(self, name):
        args = ["session", "create", "--name", name, "--repo-path", self.sb.work, "--json"]
        agent = self.sb.agent_argv(name)
        args += ["--command", agent[0]]
        for a in agent[1:]:
            args += ["--arg", a]
        self.thurbox(*args)
        self.names.append(name)

    def server_pids(self):
        return self._tmux_server_pid(self.tmux_base)

    def client_argv(self):
        return [self.tui]

    def prepare_client(self, client):
        # The interface opens with the session list focused; Enter hands the
        # keyboard to the selected session's terminal.
        client.write(b"\r")

    def capture(self, name, lines):
        n = str(lines if lines is not None else 10_000_000)
        return self.thurbox("session", "capture", name, "--lines", n, "--text").stdout

    def teardown(self):
        pids = self.server_pids()
        self.sb.run(self.tmux_base + ["kill-server"], check=False)
        for pid in pids:
            bl.kill_tree(pid)

    def recover(self):
        # What brings a thurbox session back after its tmux server died is the
        # interface, which respawns every row it surveys without a pane.
        return "client"

    def layout_after_restart(self):
        out = self.thurbox("session", "list", "--json", check=False)
        if out.returncode != 0:
            return {"listed": 0}
        rows = json.loads(out.stdout)
        rows = rows.get("sessions", rows) if isinstance(rows, dict) else rows
        return {"listed": len(rows)}

    def versions(self):
        # Only the version and schema: the rest of the answer is paths.
        out = json.loads(self.thurbox("version", "--json").stdout)
        return {"thurbox": f"{out['version']} (schema v{out['schema_version']})"}


HOSTS = {"tmux": Tmux, "herdr": Herdr, "thurbox": Thurbox}
