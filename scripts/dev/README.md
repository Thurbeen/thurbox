# `scripts/dev/` — developer test & run harnesses

Which script for which job. Everything here is isolated from your real
environment (its own tmux socket, scoped `THURBOX_*_DIR` / temp `HOME`, private
`-L` sockets) and writes only under `target/` (gitignored).

## e2e — session-backend end-to-end family (`e2e/`)

The three harnesses share one concept — **provision (or point at) a host, create
a session on it over the session backend, assert it landed there** — and differ
only in *where the host comes from*. They share `e2e/lib/e2e-common.sh` (colour
logging, the PASS/FAIL contract, an in-shell JSON extractor, the `[[hosts]]`
emitter, and the create → get → assert core).

| Script | Target host | Ephemeral? | Deps | In CI? |
|---|---|---|---|---|
| `e2e/linux-container.sh` | throwaway Podman/Linux SSH container | yes | podman, ssh, cargo | no (manual) |
| `e2e/windows-vm.sh` | throwaway dockur/Windows psmux VM | yes (KVM) | podman+kvm, ssh, curl | no (manual; the native `windows` CI job mirrors it) |
| `e2e/real-host.sh` | a real Linux/Windows/WSL machine you own | no | ssh, cargo | no (manual) |

Shared verbs across the trio: `up` (provision) · `test` (headless e2e) · `ssh`
(shell) · `hosts` (print the `hosts.toml` block) · `clean` / `down` (teardown).
`e2e/windows-vm.sh` and `e2e/real-host.sh` add host-specific extras (`deploy`,
`test-suite`, `wait`, `wsl-setup`, …) — run a script with no args for its full
usage.

```bash
scripts/dev/e2e/linux-container.sh up      # then: … test
scripts/dev/e2e/windows-vm.sh up           # first run installs Windows (~10-20 min)
scripts/dev/e2e/real-host.sh devbox check  # readiness probe on a real host
```

## The TUI smoke test moved

The black-box TUI test is `tests/tui_e2e.rs`: the real `thurbox` binary on a
real pty, asserting on the reconstructed frames *and* on the byte stream (the
alternate screen, mouse modes, a reflow without a screen clear). It runs in the
ordinary `cargo nextest run --all` — `just smoke` runs only it — and replaced
`smoke/tui-smoke.sh`, which drove a tmux pane and could see neither the bytes
nor a session's own terminal.

## Performance

`perf-run.sh` runs the **real binary under a reproducible load** — real tmux
panes, a real vt100 grid per session, the real render loop — in the same
isolated sandbox as everything else, with `sh` printing on a timer as the agent
so the measurement is of thurbox rather than of whichever coding CLI is
installed. It reports CPU from `/proc` plus the loop's own `perf_window` line,
and keeps the full log at `target/perf-run.log`.

```bash
scripts/dev/perf-run.sh                        # 8 sessions, 1 printing, 30s
scripts/dev/perf-run.sh -n 19 -p 3 -s 255x62   # a working machine's shape
scripts/dev/perf-run.sh --idle                 # the settled floor
scripts/dev/perf-run.sh -u 0                   # no URLs in the output (ADR-P20)
scripts/dev/perf-run.sh -w 4                   # 4 sessions `working`: the spinner clock runs
scripts/dev/perf-run.sh -w 4                   # 4 sessions `working`, so the spinner clock runs
scripts/dev/perf-run.sh --no-perf-log --json   # CPU alone, machine-readable
```

A reading is only comparable with another at the same size and session count,
so pass `-s` and `-n` explicitly when recording one. Its companion is
`cargo bench --bench frame_cost`, which measures the *pieces* of a frame rather
than the whole binary; both are explained in `docs/PERFORMANCE.md`.

## Dev utilities (not tests)

| Script | What it does |
|---|---|
| `sandbox.sh` | run the dev TUI/CLI against an isolated sandbox (`just sandbox*`); launched from the sandbox root so the interface directory is isolated too, not just the database |
| `render-og-image.sh` | rasterize the website OpenGraph card |
| `lib/sandbox-env.sh` | shared isolation helper sourced by `sandbox.sh` and `scripts/demo/record.sh` |

## Result contract

Every e2e harness ends on a single line: `PASS <message>` (green, exit 0) or
`FAIL <message>` (red, exit 1), from `e2e-common.sh`'s `pass`/`fail`. Set
`E2E_JSON=1` to also emit a `{"result":…}` line for CI/automation.

## Moved paths

These entrypoints were renamed (the old flat paths no longer exist). Update any
references:

| Old | New |
|---|---|
| `scripts/dev/remote-ssh-test.sh` | `scripts/dev/e2e/linux-container.sh` |
| `scripts/dev/windows-test.sh` | `scripts/dev/e2e/windows-vm.sh` |
| `scripts/dev/lab-test.sh` | `scripts/dev/e2e/real-host.sh` |
| `scripts/dev/tui-smoke-test.sh` | `tests/tui_e2e.rs` (a Rust integration test, not a script) |
