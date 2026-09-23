# Benchmark: raw tmux vs Herdr vs thurbox

How the three compare as the thing your coding agents live in: starting
sessions, holding them, showing them, typing into them, and surviving a crash.
Same stand-in agent everywhere, so what differs is the host.

Re-run it in one command, from a checkout of the commit you want to measure:

```sh
nix develop -c just bench-multiplexers                    # everything
nix develop -c just bench-multiplexers --quick --reps 1   # try the harness
```

Measured 2026-09-23 on a dedicated, otherwise idle 4-core machine (details
under [The machine](#the-machine)): **tmux 3.7c**, **Herdr 0.9.1** (release
binary), **thurbox** built in release from `main` at `0a7c8ece`. Every number
is a median over 5 repetitions with the worst of them (p95) in brackets, unless
it says otherwise. Raw samples: [`benchmark-multiplexers/`](benchmark-multiplexers/).

## The short version

| | tmux | Herdr | thurbox |
|---|---|---|---|
| keystroke to echo, idle | **1.0 ms** | 2.2 ms | 24 ms (p95 48) |
| create 50 sessions | **0.46 s** | 2.6 s | 4.6 s |
| host memory, 50 sessions, nothing attached | **4.2 MiB** | 41 MiB | 9.2 MiB |
| host CPU, 50 idle sessions, nothing attached | **0 %** | 22.5 % | **0 %** |
| host CPU, 50 idle sessions, client attached | **0 %** | 76 % | 10 % |
| host memory, 50 sessions, client attached | **13 MiB** | 54 MiB | 81 MiB |
| sessions running their command again after a restart | 0 of 3 | 0 of 3 (layout back) | **3 of 3** |

Where the three put the terminal, which explains most of the table:

```text
tmux       agent ─pty─ tmux server (parses, keeps the screen) ── tmux client ── your terminal
Herdr      agent ─pty─ herdr server (parses: libghostty-vt)   ── herdr client ── your terminal
thurbox    agent ─pty─ tmux server (parses, keeps the screen) ── control mode ── thurbox (parses
                                                                                  again: vt100,
                                                                                  draws: ratatui)
```

thurbox with nothing attached **is** tmux, plus an idle placeholder shell and
the automation heartbeat loop, so headless it costs what tmux costs. Attached,
it is a second terminal emulator per session and a full-screen application on
top, and that is where it pays.

**Where thurbox loses, plainly:**

- **Typing feels slower.** A keystroke comes back in 24 ms (p95 48 ms), and
  42 ms whenever another session is busy — against 1–2 ms for tmux and Herdr.
  The samples cluster rather than spread, which points at the render loop's
  pacing rather than at work — most likely the 33 ms output floor (ADR-P17):
  the echo arrives as agent output, not as input, so it does not get the
  16 ms input floor.
- **Creating sessions is the slowest of the three**: 92 ms a session against
  Herdr's ~50 and tmux's 8, so 50 sessions take 4.6 s. Every `session create`
  runs 27 processes, 20 of them separate `tmux set-option` calls re-applying
  the same server options.
- **Attached, it is the heaviest on memory** (29 MiB with one session, 81 MiB
  with 50, about 1 MiB a session) and it is never quite idle (2.8 % of a core
  with one session, 10 % with 50, where tmux is at 0).
- **Its first view of a busy session can be stale.** After a headless session
  had printed ~100 lines, the interface's first frame of it lacked the latest
  lines — every repetition, every N, and never on tmux or Herdr — until the
  agent printed again. A correctness bug, not a cost.
- **Reading history through the CLI** takes 47 ms against 6–8, most of it
  starting `thurbox-cli`.
- **Attaching** takes 170–240 ms against tmux's 11.

**Where thurbox wins:** it is the only one of the three that brings the
sessions' commands back after a shutdown (in 0.22 s); with nothing attached it
costs no CPU at all at any N, where Herdr's server burns 9 % of a core at 20
idle sessions and 22.5 % at 50; and attached with many sessions it is far
cheaper than Herdr (10 % against 76 % idle, 39 % against 90 % under output).

**Where Herdr wins:** latency (2.2 ms idle, and 0.45 ms while another session
is busy), draining a burst (127 ms against 215), and scrollback held (5 500
lines of the burst against 2 000–2 500). Its costs grow with the session count,
even at rest.

## Results by scenario

### Create N sessions

From nothing running, one after another. N=1 includes starting the host.

| N | metric | tmux | Herdr | thurbox |
|---|---|---|---|---|
| 1 | first agent running (cold start) | 38 (38) ms | 147 (148) ms | 88 (92) ms |
| 5 | all agents running | 64 (74) ms | 197 (308) ms | 457 (569) ms |
| 20 | all agents running | 196 (200) ms | 1.03 (1.15) s | 1.90 (1.99) s |
| 50 | all agents running | 460 (470) ms | 2.58 (3.09) s | 4.58 (5.20) s |
| 50 | one create command, mean | 8.6 (8.7) ms | 51 (61) ms | 92 (104) ms |
| 50 | the 50th create command | 8.0 (13) ms | 28 (117) ms | 92 (92) ms |

For a user: a script that fans out 50 agents waits 4.6 s for thurbox before
the agents even start loading. None of the three slows down much as sessions
pile up — thurbox's cost is flat per session, and flat high. Herdr's first
session includes starting its server.

### Attach, detach, reattach

| N | metric | tmux | Herdr | thurbox |
|---|---|---|---|---|
| 1 | attach | 11 (16) ms | 68 (69) ms | 168 (179) ms |
| 20 | attach | 12 (12) ms | 289 (306) ms | 185 (193) ms |
| 50 | attach | 13 (14) ms | 435 (446) ms | 242 (254) ms |
| 1 | detach | 2.7 (2.7) ms | 11 (12) ms | 12 (13) ms |
| 50 | detach | 7.0 (7.1) ms | 11 (21) ms | 24 (26) ms |
| 1 | reattach | 10 (11) ms | 182 (183) ms | 177 (187) ms |
| 50 | reattach | 9.1 (9.1) ms | 411 (430) ms | 431 (592) ms |

All sessions survived every detach on all three. For a user: tmux is instant;
thurbox and Herdr both take a noticeable fraction of a second with many
sessions, Herdr growing faster with N. thurbox's detach is its Quit (Ctrl+Q) —
the interface exits and tmux keeps the sessions.

### Memory and CPU

The host's processes only (see *Accounting*). "Output" is every session
printing 10 lines a second; attached, the client shows session 1.

| N | state | tmux | Herdr | thurbox |
|---|---|---|---|---|
| 1 | headless, idle | 3.7 MiB · 0 % | 17.7 MiB · 0.6 % | 8.3 MiB · 0 % |
| 1 | attached, idle | 6.2 MiB · 0 % | 22.6 MiB · 1.0 % | 28.6 MiB · 2.8 % |
| 1 | attached, output | 6.2 MiB · 0.3 % | 22.6 MiB · 8.7 % | 28.6 MiB · 8.1 % |
| 20 | headless, idle | 3.9 MiB · 0 % | 26.8 MiB · 9.0 % | 8.7 MiB · 0 % |
| 20 | headless, output | 4.0 MiB · 3.9 % | 27.2 MiB · 21 % | 8.7 MiB · 4.3 % |
| 20 | attached, idle | 8.7 MiB · 0 % | 34.8 MiB · 18.7 % | 49.1 MiB · 5.3 % |
| 20 | attached, output | 8.7 MiB · 4.0 % | 34.9 MiB · 33.7 % | 49.3 MiB · 19.8 % |
| 50 | headless, idle | 4.2 MiB · 0 % | 41.1 MiB · 22.5 % | 9.2 MiB · 0 % |
| 50 | headless, output | 4.6 MiB · 7.2 % | 41.9 MiB · 70 % | 9.4 MiB · 8.7 % |
| 50 | attached, idle | 12.7 MiB · 0 % | 53.8 MiB · 76 % | 80.5 MiB · 10.1 % |
| 50 | attached, output | 12.7 MiB · 6.2 % | 53.9 MiB · 90 % | 82.6 MiB · 38.7 % |

Memory is PSS; CPU is a percentage of one core over 10 s. A 65 s window at
N=20, headless and idle, to catch periodic work: tmux 0 %, Herdr 9.0 %,
thurbox 0 %. That window cannot see a process that starts and exits inside it,
so thurbox's once-a-minute `thurbox-cli automation tick` was timed on its own:
about 0.02 s of CPU a run, 0.03 % of a core.
The p95s are within 1 % of the medians except Herdr at N=50 (up to 106 %
attached under output) and thurbox's attached memory at N=50 (up to 91 MiB).

For a user: if agents sit in the background, thurbox costs what tmux costs and
Herdr costs a real slice of a core that grows with every session, even when
nothing is happening. Attached, thurbox is the heaviest on memory and sits
between the other two on CPU.

### Throughput: a 50 000-line burst

One session prints 50 000 lines of 100 bytes (5 MB) as fast as its pty takes
them.

| | metric | tmux | Herdr | thurbox |
|---|---|---|---|---|
| headless | agent's writes took | 215 (218) ms | 127 (131) ms | 215 (221) ms |
| headless | until the host went quiet | 343 (352) ms | 263 (1262) ms | 353 (355) ms |
| headless | host CPU spent | 0.21 (0.22) s | 0.14 (0.16) s | 0.21 (0.23) s |
| attached | agent's writes took | 233 (234) ms | 143 (143) ms | 246 (248) ms |
| attached | last line on the client's screen | 333 (337) ms | 158 (159) ms | 260 (263) ms |
| attached | until the host went quiet | 447 (449) ms | 790 (1036) ms | 442 (585) ms |
| attached | host CPU spent | 0.23 (0.23) s | 0.20 (0.20) s | 0.38 (0.41) s |
| attached | host memory after | 8.4 MiB | 23.2 MiB | 36.5 MiB |

Nothing was dropped: on every host and every repetition, the last lines the
host reported were the last lines written, in order. For a user: a log-dumping
agent is never slowed by any of the three; Herdr drains fastest, and thurbox
shows the end of a burst sooner than `tmux attach` does, at about 1.6 times
the CPU.

### Scrollback

After one 50 000-line burst, headless.

| metric | tmux | Herdr | thurbox |
|---|---|---|---|
| lines of it the host keeps | 2 001 | ~5 500 | 2 500 |
| lines one CLI read returns | 2 001 | 998 | 2 500 |
| host memory the history costs | 2.1 MiB | 7.0 MiB | 1.7 MiB |
| reading all of it through the CLI | 7.8 (9.2) ms | 6.3 (6.3) ms | 47 (50) ms |

Each keeps what its default allows: tmux 2 000 lines; thurbox 5 000 rows, which
is 2 500 of these lines in its 80-column headless window; Herdr 10 MB, about
5 500 rows. Herdr's `pane read` returns at most 1 000 lines however many are
asked for, so a script sees less than it holds (the ~5 500 is from its own
scroll metrics). For a user: none of them keeps a long build log by default;
thurbox's CLI is the slowest to hand it over.

### Keystroke to echo

Through the attached client, 500 keys per cell (5 repetitions of 100), spaced
20–60 ms apart at random.

| | tmux | Herdr | thurbox |
|---|---|---|---|
| idle, median (p95) | 1.04 (1.09) ms | 2.23 (2.34) ms | 24.0 (48.3) ms |
| another session busy, median (p95) | 0.77 (0.98) ms | 0.45 (0.55) ms | 42.2 (43.1) ms |

No key was lost on any host. thurbox's samples are not spread but clustered:
idle at about 4, 13, 24 and 47 ms; with another session busy at about 11 and
42 ms. For a user: 1–2 ms is imperceptible; 24–48 ms is the difference between
a local shell and a slightly laggy remote one, and it is there on every key.
Herdr getting faster while another session is busy was not investigated.

### Survival

Three sessions.

| event | tmux | Herdr | thurbox |
|---|---|---|---|
| client SIGKILLed: agents still running | 3 of 3 | 3 of 3 | 3 of 3 |
| server SIGKILLed: agents still running | 0 of 3 | 0 of 3 | 0 of 3 |
| shutdown (all SIGTERMed), host started again: sessions it lists | 0 | 3 | 3 |
| … sessions running their command again | 0 | 0 | 3, in 222 (270) ms |

For thurbox the server is its tmux server; killing the thurbox interface is
the client row, and loses nothing. After the restart,
Herdr restores its layout and would resume the agents it supports (Claude
Code, Codex and others) — the stand-in is not one, so its panes come back as
shells. thurbox re-runs the recorded command of every session it has a row
for, whatever the command is. For a user: after a reboot, thurbox puts your
sessions back; Herdr puts back the ones running an agent it knows; tmux puts
back nothing.

## What this points at in thurbox

Recorded, not fixed here — the benchmark does not tune what it measures. Each
can be re-measured with the scenario named.

1. **Echo latency** (`latency`): a keystroke's echo is agent output, so it is
   drawn on the 33 ms output floor. Output from the session that just received
   a key, arriving within a frame or two of it, is arguably input and could
   take the 16 ms floor — or no floor.
2. **Stale first view on attach** (`resources`, `first_view_stale`): write the
   end-to-end test that reproduces it (a headless session prints ~100 lines at
   80 columns; the interface attaches; its first frame lacks the last line),
   then fix it.
3. **`session create` cost** (`create`): 20 `tmux set-option` processes per
   create re-apply options the server already has. One `tmux` invocation, or
   once per server, would remove most of the 92 ms.
4. **Attached memory and idle CPU per session** (`resources`): ~1 MiB and ~0.15 %
   of a core per session with the interface up and nothing happening.
5. **`session capture` start-up** (`scrollback`): 47 ms to hand back 2 500
   lines, against 8 for `tmux capture-pane`.

## What was measured, and why these

The question is "which host costs me what, for the work every one of them
does". So each scenario is something all three do, done the way each one's own
documentation says to do it headlessly, and nothing that is one host's special
trick:

| scenario | what it does | tmux | Herdr | thurbox |
|---|---|---|---|---|
| create | N sessions from cold, one after another | `new-session` / `new-window` | `workspace create` / `tab create`, then `pane run` | `thurbox-cli session create --command` |
| attach | client on a pty; leave; come back | `tmux attach` | `herdr` | `thurbox` |
| resources | memory and CPU at rest and under output, headless and attached | | | |
| throughput | one session prints 50 000 lines as fast as it can | | | |
| scrollback | what the host keeps of that, and reading it back through its CLI | `capture-pane` | `pane read` | `session capture` |
| latency | keystroke to echo, through the attached client | | | |
| survival | client killed, server killed, a shutdown and restart | | | |

Left out, on purpose:

- **Herdr's agent detection and thurbox's plugin panes and hook-driven
  status.** Each is a feature only one of them has. Their running cost is
  inside the totals — Herdr's detection runs in its server, thurbox's panes
  are its client — because a user cannot switch them off either, but neither
  is measured as a feature against the others.
- **Remote hosts (SSH).** All three can do it, and all three differently;
  it is a benchmark of its own.
- **A real coding agent.** Claude Code or Codex would make the numbers about
  the agent: its startup, its redraw rate, its memory. The stand-in is the
  constant.

## Method

**The stand-in agent** (`scripts/bench/agent.py`) is one Python process per
session, identical on every host. It writes the monotonic time it started to
a file, then waits. Every byte typed at it is answered with a four-letter token
redrawn in place; a signal makes it print a burst of 50 000 lines of 100 bytes,
another starts or stops a trickle of 10 lines a second. Signals rather than
typed commands, so starting a burst does not go through the input path being
measured. Readiness and burst timing come from the agent's own clock, never
from polling a host.

**Hermetic.** Every repetition gets a fresh sandbox: its own `HOME`, XDG
directories, runtime dir and `TMUX_TMPDIR`, its own tmux socket (tmux with
`-f /dev/null`), its own Herdr named session and state, and thurbox relocated
with `THURBOX_CONFIG_DIR` / `THURBOX_DATA_DIR` / `THURBOX_SOCKET` exactly as
`scripts/dev/sandbox.sh` does. Every server started is killed before the next
repetition, and the sandbox deleted.

**Defaults, except where the network or a first-run question is concerned.**
Every host runs its own defaults. Changed, and why:

| host | setting | why |
|---|---|---|
| tmux | `-f /dev/null` | the machine's `~/.tmux.conf` is not tmux |
| Herdr | `onboarding = false` | what finishing the first-run welcome writes; a returning user never sees it |
| Herdr | `[update] version_check = false`, `manifest_check = false` | no network |
| Herdr | `[server] headless_cols/rows = 200x50` | the size tmux sessions are created at, so both parse the same grid |
| thurbox | `[features] version_check = false`, `auto_update = false` | no network |
| thurbox | `thurbox-cli config accept-interface` | the one-time "this is v2" question; a returning user never sees it |

thurbox's headless windows stay at its own 80x24: sizing them is not a knob it
offers, and the benchmark does not tune thurbox.

**Accounting.** "The host" is every process in the host's trees except the
agents, which report their own pids: the server, anything it keeps running
beside the sessions (thurbox's placeholder shell and automation heartbeat
loop), and an attached client with whatever it spawned (thurbox's tmux
control-mode client). Memory is PSS from `/proc/<pid>/smaps_rollup`, so a
library shared between two host processes is counted once. CPU is
`utime + stime` from `/proc/<pid>/stat` over a fixed window.

**Clients** run on a pseudo-terminal of 200x50 that answers what an
xterm-class terminal answers (cursor position, device attributes, cell size,
palette) and claims nothing newer — a client that believed the terminal spoke
the kitty keyboard protocol would read the latency scenario's keys
differently. Everything a client prints is drained continuously, so no client
is ever stalled by a terminal that stopped reading.

**Statistics.** 5 repetitions after 1 warm-up repetition that is recorded
(marked) and left out. Median and p95 are nearest-rank, so both are values that
were actually observed; with 5 repetitions the p95 is the maximum. Latency
samples (100 per repetition) are pooled across repetitions, so its p95 is a
real percentile of 500. Hosts are interleaved inside each repetition, so a
slow drift of the machine lands on all three. Timing is done in the harness
with `CLOCK_MONOTONIC`; `hyperfine` was not used, because most of these are
not "run a command N times".

**Niceness and load.** The timed runs ran at niceness 0; only the thurbox build
before them ran under `nice -n 10`. The load average is recorded next to every
sample in the raw results.

## The machine

A NixOS 26.05 machine with a 4-core Intel Core i5-6500T (2.5 GHz, one thread
per core, `powersave` governor), 15.5 GiB of RAM, Linux 6.18, dedicated to the
run and otherwise idle (load average 0.2–0.4 before each run). Python 3.13 ran
the harness and the stand-in. thurbox reports itself as `0.0.0-dev`, which is
what a build from a checkout is; the commit it was built from is recorded in
the results.

## Threats to validity

- **One machine, and an old one.** Four cores of a 2015 desktop CPU with the
  `powersave` governor. A faster machine shrinks every absolute number; it
  should not reorder them much, but ratios near 1 could flip.
- **The benchmark loads the machine itself.** Fifty Python agents starting
  push the 1-minute load average to ~2.7 on four cores during and just after
  the N=50 rounds; outside those it stayed under 1. One N=50 repetition of
  the resources scenario began at 7.4, the residue of the warm-up round before
  it (150 agents, and Herdr near a full core); its numbers sit inside the
  other repetitions' spread except Herdr's CPU under output, whose p95 is that
  sample. Hosts are interleaved inside every repetition, so all three ran
  under the same load, and no other work ran on the machine (load before each
  run: 0.2–0.4).
- **CPU windows miss short-lived processes.** A process that starts and exits
  inside a window is not counted. None of the three hosts runs one while idle
  except thurbox's once-a-minute heartbeat tick, timed separately (0.02 s of
  CPU a run).
- **A real agent may cost Herdr more or less.** Herdr classifies what runs in
  each pane (working, blocked, idle); the stand-in is not an agent it knows,
  and how much of Herdr's idle CPU is that classification was not isolated.
- **The stand-in is Python.** Its ~35 ms start is inside every "until the
  agent started" number, equally for all three. A real agent starts in
  seconds, which would swamp most create differences.
- **Herdr goes through a shell.** Its documented way to run a command in a new
  pane is `tab create` then `pane run`, which types the command into a shell.
  That is two CLI calls and a shell start per session where tmux and thurbox
  exec the command directly. It is what a Herdr user scripting sessions does,
  but it is not Herdr's floor.
- **Different defaults, measured as they come.** Scrollback (tmux 2 000 lines,
  thurbox's tmux 5 000 rows, Herdr 10 MB), headless window size (thurbox's
  80x24 against 200x50), and what each client draws around the pane. The
  scrollback table reports retention next to cost for that reason.
- **The terminal is a harness.** Clients draw into a pty the harness reads,
  not a terminal emulator, so what a real terminal spends parsing each
  client's output is not counted, and neither are the bytes each client
  writes to it.
- **Herdr's attach is bimodal** — about 50–70 ms or about 170–300 ms, with no
  cause found; answering its terminal queries did not change it, and
  attaching again within a second of a detach made the slow mode reliable. The
  attach scenario waits 3 s before reattaching; the medians above include
  whichever mode came up.
- **p95 over 5 repetitions is the maximum.** Treat the p95 columns as "worst
  seen" everywhere except latency, where it is over 500 pooled samples.
- **Two invocations.** `create` and `attach` come from one run and the other
  five scenarios from a second, the same day on the same machine and build
  (87 minutes of machine time between them). The first run stopped on a
  harness bug — a banner scrolled off screen before a client attached — fixed
  before the second.

## Re-running it

```sh
nix develop -c just bench-multiplexers                       # all of it
nix develop -c just bench-multiplexers --scenarios latency,attach --reps 10
nix develop -c just bench-multiplexers --hosts tmux,thurbox --no-build
```

`scripts/bench/run.sh` fetches the pinned Herdr release (checked against its
SHA-256) into `~/.cache/thurbox-bench/`, builds thurbox in release from the
checkout it is in, and runs `scripts/bench/run.py`, which writes
`results.json`, `results.csv` and `summary.md` under
`~/.cache/thurbox-bench/work/results-<timestamp>/`. Each scenario is also a
script of its own (`python3 scripts/bench/scenarios/latency.py --reps 3`).
Run it on a machine with nothing else busy, and look at `load1` in the results
before believing a number.
