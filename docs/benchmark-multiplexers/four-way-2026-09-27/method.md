# Four-host multiplexer benchmark method

This run compares standalone tmux, standalone Herdr, standalone RMUX, and
Thurbox using tmux. Thurbox using RMUX remains unmeasured pending validation
of that backend. The Thurbox binary is built from the pinned head of the
optional RMUX branch, `98f17e2253602e413f954921b644ae9d7b8b8719`.

The installed binaries report tmux 3.7c, Herdr 0.9.1, RMUX 0.10.0, and
Codex CLI 0.157.1. RMUX 0.10.0 was checked against its
[latest stable release](https://github.com/Helvesec/rmux/releases/tag/v0.10.0).
Each results file records the machine, binary hashes, versions, repetition
count, load, and the Thurbox commit.

## Stand-in workload

All four hosts start the same `python3 scripts/bench/agent.py <name> <state-dir>`
command. `SHELL=/bin/sh`; Herdr's pane API runs `exec` through that shell,
while the other hosts pass the command directly. Each repetition gives each
host a fresh HOME, XDG config/data/runtime tree, multiplexer socket, and server.
The host order rotates across repetitions. An attached client uses a 200 × 50
PTY. Scenarios use their existing N, output rates, client attachment, windows,
and history limits. One warm-up repetition is retained in the raw data and
excluded from summaries; five measured repetitions follow it.

CPU is percent of one logical core, with values above 100% retained. PSS is
the host's proportional memory share. The identical stand-in agent processes
are excluded from host CPU and PSS, while helper shells and attached clients
are included. `results.json` holds every sample, `results.csv` flattens list
metrics such as keystroke latency, and `summary.md` gives nearest-rank medians
and p95. Load averages are recorded before and after each sample.

From the checkout used for the run:

```sh
scripts/bench/run.sh --no-build --reps 5 --warmup 1 \
  --work "$HOME/.cache/b5/four-way" \
  --out docs/benchmark-multiplexers/four-way-2026-09-27/synthetic
```

The release binaries are built before measurement. The command checks the
pinned Herdr release hash and records the installed RMUX version. The work
path is short enough for Unix socket limits and is separate from Thurbox's
live config and sessions.

## Codex TUI with local inference fixture

This is a distinct N=1 workload, not a substitute for the stand-in figures.
Each host runs the Codex TUI with a fresh `CODEX_HOME` and a dummy key aimed
only at a loopback Responses fixture. The fixture streams `MOCK_TURN_DONE`;
no paid endpoint or operator credentials are used. The runner observes the
real `SessionStart`, `UserPromptSubmit`, and `Stop` hooks. On Thurbox it also
checks that all three signals succeed and that `session get` persists `done`.

With the client attached, five eight-byte markers are written to Codex's
prompt per repetition; each raw latency is from the write until the complete
marker appears in client output. A three-second idle window measures CPU, and
PSS is sampled afterward. These resource figures include Codex and the host,
unlike the stand-in host-only figures. A timeout is counted and remains in
the raw record. Five measured repetitions and one retained warm-up use the
same rotating host order.

```sh
python3 scripts/bench/real_tui.py --reps 5 --warmup 1 \
  --work "$HOME/.cache/b5/real-tui" \
  --out docs/benchmark-multiplexers/four-way-2026-09-27/real-tui.json \
  --thurbox-bin target/release
```

The mock server listens on `127.0.0.1` only. The runner retains raw hook
events, individual input echoes, per-process PSS, host load, and every mock
request in `real-tui.json`.
