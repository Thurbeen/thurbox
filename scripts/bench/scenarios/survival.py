"""What is left when something dies. Three sessions each time.

``client-killed``: the attached client is SIGKILLed; agents still running, of 3.
``server-killed``: the host's server is SIGKILLed (for thurbox that is its tmux
server — the TUI is only a client); agents still running, of 3.
``restart``: server and agents are all SIGTERMed, as a machine shutdown does,
then the host is started again the way a user would (tmux: nothing to start;
Herdr: its server; thurbox: its interface, which respawns every session it has
a record of). ``commands_back``: sessions running their command again within
30 s, of 3; ``restore_ms``: until the last of them started; ``listed``: sessions
the host itself lists after the restart, whatever runs in them.
"""

import signal
import time

from common import alive, attach, create_ready, focus, signal_agents

import benchlib as bl

SESSIONS = 3


def run(ctx):
    for rep, warm in ctx.repetitions():
        for name in ctx.hosts:
            load = bl.load()
            host = ctx.fresh(name)
            client = None
            try:
                create_ready(host, SESSIONS)
                agents = host.agent_pids()
                client, _ = attach(host)
                focus(host, client)
                client.proc.kill()
                client.proc.wait()
                time.sleep(1.0)
                after_client = alive(agents)
                host.kill_server()
                time.sleep(1.0)
                after_server = alive(agents)
                ctx.record(
                    "survival",
                    name,
                    "crash",
                    rep,
                    warm,
                    load,
                    alive_after_client_kill=after_client,
                    alive_after_server_kill=after_server,
                )
            finally:
                ctx.close(host, client)
            client = None
            load = bl.load()
            host = ctx.fresh(name)
            try:
                create_ready(host, SESSIONS)
                agents = host.agent_pids()
                time.sleep(2.0)
                signal_agents(host.server_pids() + agents, signal.SIGTERM)
                bl.wait_until(lambda h=host, a=agents: alive(a + h.server_pids()) == 0, 10)
                for n in host.names:
                    host.sb.forget(n)
                start = bl.now_ns()
                if host.recover() == "client":
                    client = bl.PtyClient(host.client_argv(), host.sb.env, host.sb.work)
                deadline = time.monotonic() + 30
                back = {}
                while time.monotonic() < deadline and len(back) < SESSIONS:
                    for n in host.names:
                        if n not in back and host.sb.ready(n):
                            back[n] = host.sb.ready(n)[0]
                    if client is not None:
                        client.pump(0.02)
                    else:
                        time.sleep(0.02)
                ctx.record(
                    "survival",
                    name,
                    "restart",
                    rep,
                    warm,
                    load,
                    commands_back=len(back),
                    restore_ms=bl.ms(max(back.values()) - start) if len(back) == SESSIONS else None,
                    **host.layout_after_restart(),
                )
            finally:
                ctx.close(host, client)


if __name__ == "__main__":
    import sys

    import run as entry

    entry.main(["--scenarios", "survival"] + sys.argv[1:])
