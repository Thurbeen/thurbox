# Benchmark: raw tmux vs Herdr vs thurbox

How the three compare as the thing your coding agents live in: starting
sessions, holding them, showing them, typing into them, and surviving a crash.
Same stand-in agent everywhere, so what differs is the host.

This page keeps the harness and its method. It keeps no results: the earlier
numbers measured thurbox's own session engine, which thurbox no longer has
(ADR-41). Run the harness to measure the current build.

```sh
nix develop -c just bench-multiplexers                    # everything
nix develop -c just bench-multiplexers --quick --reps 1   # try the harness
```

The [source-pinned five-way website comparison](../../website/docs/performance.html)
adds RMUX, stage-1 deltas, repeated-run noise, received wakeups and remaining gaps.

## What was measured, and why these

The question is "which host costs me what, for the work every one of them
does". So each scenario is something all three do, done the way each one's own
documentation says to do it headlessly, and nothing that is one host's special
trick:

| scenario | what it does | tmux | Herdr | thurbox |
|---|---|---|---|---|
| create | N sessions from cold, one after another | `new-session` / `new-window` | `workspace create` / `tab create`, then `pane run` | a Fleet Task (`fleet apply`), started by `fleet up` |
| attach | client on a pty; leave; come back | `tmux attach` | `herdr` | `thurbox` |
| resources | memory and CPU at rest and under output, headless and attached | | | |
| throughput | one session prints 50 000 lines as fast as it can | | | |
| scrollback | what the host keeps of that, and reading it back through its CLI | `capture-pane` | `pane read` | the runtime protocol's `capture` |
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
redrawn in place; a signal makes it print a burst of 50 000 lines of ~107 bytes,
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

thurbox's headless windows stay at its own 80x24: sizing them is not a knob it
offers, and the benchmark does not tune thurbox.

**Accounting.** "The host" is every process in the host's trees except the
agents, which report their own pids: the server, anything it keeps running
beside the sessions (thurbox's `fleet up` and its runtime-mux tmux
server), and an attached client with whatever it spawned (thurbox's tmux
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
before them ran under `nice -n 10`. Every sample in the raw results carries the
1-minute load average at the start of its repetition (`load1`); the harness now
also records it when each sample ends (`load1_end`), which the latency re-run
below has and the other scenarios' committed data predates.

## Threats to validity

- **The benchmark loads the machine itself.** Fifty Python agents starting
  push the load average up during the N=50 rounds. Hosts are interleaved
  inside every repetition, so all three run under the same load. Look at
  `load1` in the results before believing a number.
- **CPU windows miss short-lived processes.** A process that starts and exits
  inside a window is not counted.
- **A real agent may cost Herdr more or less.** Herdr classifies what runs in
  each pane (working, blocked, idle); the stand-in is not an agent it knows.
- **The stand-in is Python.** Its start is inside every "until the agent
  started" number, equally for all three. A real agent starts in seconds,
  which would swamp most create differences.
- **Herdr goes through a shell.** Its documented way to run a command in a new
  pane is `tab create` then `pane run`, which types the command into a shell.
  That is two CLI calls and a shell start per session where tmux and thurbox
  exec the command directly. It is what a Herdr user scripting sessions does,
  but it is not Herdr's floor.
- **Different defaults, measured as they come.** Scrollback, headless window
  size and what each client draws around the pane differ. The scrollback
  scenario reports retention next to cost for that reason.
- **The terminal is a harness.** Clients draw into a pty the harness reads,
  not a terminal emulator, so what a real terminal spends parsing each
  client's output is not counted.
- **p95 over 5 repetitions is the maximum.** Treat a p95 as "worst seen"
  everywhere except latency, where it is over the pooled samples.

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
before believing a number. Like `scripts/dev/perf-run.sh`, it refuses to run
where `THURBOX_GATE` is exported: it is a benchmark, not a test, and a
validation step is the opposite of a quiet machine.
