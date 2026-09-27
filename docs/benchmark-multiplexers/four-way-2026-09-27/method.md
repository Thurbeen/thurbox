# Four-configuration multiplexer benchmark method

Each machine compares standalone tmux, standalone Herdr, standalone RMUX, and
Thurbox using tmux. Thurbox using RMUX remains unmeasured pending validation
of that backend. The benchmark source and Thurbox binary are pinned to
`1f9bd654db4209b1b5832248c5c444c627acd178`, based on the optional RMUX
branch head `98f17e2253602e413f954921b644ae9d7b8b8719`. The archive
transferred to the remote machines has SHA-256
`f7702d23be6b606ed01923e9a659364d640d4aea9bd9853c020d10b924de4bfa`.
Results from different machines are stored and interpreted separately.

Herdr 0.9.1, RMUX 0.10.0, and Codex CLI 0.157.1 are pinned across machines;
the installed tmux version is recorded for each machine. Herdr 0.9.1 was
checked against its
[latest stable release](https://github.com/herdrdev/herdr/releases/tag/v0.9.1).
RMUX 0.10.0 was checked against its
[latest stable release](https://github.com/Helvesec/rmux/releases/tag/v0.10.0).
The RMUX release archive was checked against the release's
[SHA256SUMS](https://github.com/Helvesec/rmux/releases/download/v0.10.0/SHA256SUMS)
(`1bec11eff08c3313c3a400196e7a93d00b8ad4a24f81ef13debb03355c2696c5`),
and the Herdr release binary against its published digest
(`2a02fed16beb651ef006e1d43f048f652ca4dc58ad053cd2d44450563d5c54b7`).
The Codex binary SHA-256 is
`3e2584f3f3829a43a0495011a1cecb2facbe64a2403e2b682351fd9c2983f970`.
Each results file records machine specifications, binary hashes, versions,
repetition count, load, and the source commit.

Both remote machines used the same source archive, extracted at `$BASE/src`.
The release binaries were built with `cargo build --release --bin thurbox --bin
thurbox-cli` on Debian and `nix develop . --command cargo build --release
--bin thurbox --bin thurbox-cli` on NixOS. The latter also supplied a Python
3.14.7 interpreter from the pinned flake. The RMUX Linux x86-64 release archive
was extracted under `$BASE/rmux-0.10.0-linux-x86_64` with its complete `bin/`
and `libexec/` layout intact. `run.sh` checked or fetched the pinned Herdr
binary. The same Codex 0.157.1 Linux musl executable was placed in `$BASE/bin`
on both machines and checked against the digest above. The
[Codex release instructions](https://github.com/openai/codex/blob/main/README.md#installing-and-running-codex)
describe the upstream Linux binary layout; no local credentials were copied.

The first end-to-end probe was run before standalone RMUX support existed:
`python3 -m unittest scripts/bench/test_rmux_create.py` failed with
`unknown host 'rmux'; known: tmux, herdr, thurbox`. It passed after the
standalone RMUX adapter was added. No Thurbox/RMUX adapter was used.

## Stand-in workload

All four configurations start the same `python3 scripts/bench/agent.py <name> <state-dir>`
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
CPU accounting samples the host process set at the window boundaries; a helper
that starts and exits entirely inside a window can escape it. In particular,
an idle Thurbox automation tick is a separate transient process, so a displayed
0% idle sample is not a claim that all periodic work consumed zero CPU.

From the pinned source checkout or archive, run on each machine with its own
results directory:

```sh
BASE="$HOME/.cache/thurbox-bench-fourway"
RESULTS="$BASE/results"
export PATH="$BASE/bin:$BASE/rmux-0.10.0-linux-x86_64/bin:$PATH"
export BENCH_CACHE="$BASE"
export BENCH_SOURCE_COMMIT=1f9bd654db4209b1b5832248c5c444c627acd178
scripts/bench/run.sh --no-build --reps 5 --warmup 1 \
  --work "$HOME/.cache/b5/synth-run" \
  --out "$RESULTS/synthetic"
```

The release binaries are built before measurement. The command checks the
pinned Herdr release hash and records the installed RMUX version. For an
archive without `.git`, `BENCH_SOURCE_COMMIT` is set to the pinned commit above.
The work path is short enough for Unix socket limits and is separate from
Thurbox's live config and sessions.

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
but exclude the shared mock server, unlike the stand-in host-only figures.
A timeout is counted and remains in
the raw record. Five measured repetitions and one retained warm-up use the
same rotating host order.

```sh
BASE="$HOME/.cache/thurbox-bench-fourway"
RESULTS="$BASE/results"
export PATH="$BASE/bin:$BASE/rmux-0.10.0-linux-x86_64/bin:$PATH"
export BENCH_SOURCE_COMMIT=1f9bd654db4209b1b5832248c5c444c627acd178
python3 scripts/bench/real_tui.py --reps 5 --warmup 1 \
  --work "$HOME/.cache/b5/real-run" \
  --out "$RESULTS/real-tui.json" \
  --thurbox-bin target/release
```

The mock server listens on `127.0.0.1` only. The runner retains raw hook
events and Thurbox's persisted status after each hook, individual input
echoes, per-process PSS, host load, and every mock request in `real-tui.json`.

## Machine eligibility

The published measurements come from one four-core NixOS machine with 15.5 GiB
of RAM and an Intel Core i5-6500T. Its CPU pressure was low during the clean
run. A prior run on that machine overlapped with a benchmark-owned heartbeat
server accidentally started during a version probe; it was discarded and the
server was removed before the published repetitions began.

The same pinned tools passed the quick stand-in scenarios and a one-repetition
Codex TUI/hook probe on a four-core Debian machine. Fair full-run timing was
unavailable there in this run: the user cgroup's `cpu.max` was `max 100000`,
with no throttling and four CPUs in its cpuset, but CPU PSI `some avg10` stayed
around 15–21% after benchmark-owned processes were removed. Separate
two-second `/proc/stat` probes showed 472, 543, and 586 process starts.
Short-lived installed Thurbox CLI restart and version commands, plus shells,
appeared under the benchmark account outside the isolated benchmark tree.
Their source was not isolated, so no Debian timing is published. Load
averages alone were not used to judge contention because they can include work
outside the benchmark's four-CPU allowance.
After the NixOS run, PSI eased to 3.64%, but the process counter still rose 183
in two seconds; the churn source was still unidentified.

The local machine also had concurrent Rust builds and active interface work.
After the NixOS run, CPU PSI `some avg10` was still about 15% with the interface
active. Its timing remains pending; no local timings are combined with the
NixOS run.
