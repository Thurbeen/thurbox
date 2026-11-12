# `scripts/dev/` — developer test & run harnesses

Which script for which job. Everything here is isolated from your real
environment (its own tmux socket, scoped `THURBOX_*_DIR` / temp `HOME`, private
`-L` sockets). Build output stays under `target/` (gitignored); host probes use
disposable profiles on the host.

## e2e — remote and Windows hosts (`e2e/`)

The three harnesses provision a host, or point at one, for testing thurbox and
its runtimes on it. They differ only in *where the host comes from*. They share
`e2e/lib/e2e-common.sh` (colour logging, the PASS/FAIL contract and the
`[[hosts]]` emitter).

| Script | Target host | Ephemeral? | Deps | In CI? |
|---|---|---|---|---|
| `e2e/linux-container.sh` | throwaway Podman/Linux SSH container | yes | podman, ssh | no (manual) |
| `e2e/windows-vm.sh` | throwaway dockur/Windows psmux VM | yes (KVM) | podman+kvm, ssh, curl | the VM run is manual (the native `windows` CI job mirrors it); its gate probes are unit-tested by `e2e/windows-vm.bats` |
| `e2e/real-host.sh` | a real Linux/Windows/WSL machine you own | no | ssh, cargo | no (manual) |

None of them creates a session: sessions are Fleet Tasks, and Fleet's own tests
and `fleet runtime check` cover the runtime on a host.

`e2e/windows-vm.sh test` checks psmux itself on the VM, and decides whether
runtime-mux's psmux hook-status gate (`HOOK_STATUS` in
`crates/runtime-mux/src/psmux.rs`) still matches what psmux does. It **fails**
when the two disagree either way round.
`e2e/windows-vm.bats` drives those helpers with no VM, so the failing branch is
exercised in CI (`just test-scripts`).

Shared verbs across the trio: `ssh` (shell) · `hosts` (print the `hosts.toml`
block) · `clean` (teardown). `e2e/linux-container.sh` and `e2e/windows-vm.sh`
add `up` and `down`; `e2e/windows-vm.sh` and `e2e/real-host.sh` add
host-specific extras (`deploy`, `test-suite`, `wait`, `wsl-setup`, …) — run a
script with no args for its full usage.

```bash
scripts/dev/e2e/linux-container.sh up      # then: … hosts
scripts/dev/e2e/windows-vm.sh up           # first run installs Windows (~10-20 min)
scripts/dev/e2e/real-host.sh devbox check  # readiness probe on a real host
```

## The TUI smoke test

The black-box TUI test is `tests/tui_e2e.rs`: the real `thurbox` binary on a
real pty, asserting on the reconstructed frames *and* on the byte stream (the
alternate screen, mouse modes, a reflow without a screen clear). It runs in the
ordinary `cargo nextest run --all`; `just smoke` runs only it.

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
scripts/dev/perf-run.sh -w 4                   # 4 sessions `working`, so the spinner clock runs
scripts/dev/perf-run.sh --no-perf-log --json   # CPU alone, machine-readable
```

A reading is only comparable with another at the same size and session count,
so pass `-s` and `-n` explicitly when recording one. It **refuses to run inside
a validation step** (`THURBOX_GATE`, the sentinel to export around one): it
looks enough like a test to be picked up as one, and a step that waits through
a release build and timed runs fails on the agent timeout — for a number a
loaded machine cannot produce honestly anyway. Override with
`THURBOX_PERF_ALLOW_IN_GATE=1` if you mean it. Its companion is
`cargo bench --bench frame_cost`, which measures the *pieces* of a frame rather
than the whole binary; both are explained in `docs/thurbox/performance.md`.

## Dev utilities (not tests)

| Script | What it does |
|---|---|
| `sandbox.sh` | run the dev TUI/CLI against an isolated sandbox (`just sandbox*`); launched from the sandbox root |
| `demo-repo.sh` | seed a demo repository + sessions into the sandbox (`sandbox.sh --demo` / `--demo-big`, `just sandbox-demo*`) |
| `reap-tmux-servers.sh` | reap orphaned *test* tmux servers a signal left behind (`just reap-tmux`, Linux; `--dry-run` lists) |
| `render-og-image.sh` | rasterize the website OpenGraph card |
| `lib/sandbox-env.sh` | shared isolation helper sourced by `sandbox.sh` and the demo recorders |

## Result contract

Every e2e harness ends on a single line: `PASS <message>` (green, exit 0) or
`FAIL <message>` (red, exit 1), from `e2e-common.sh`'s `pass`/`fail`. Set
`E2E_JSON=1` to also emit a `{"result":…}` line for CI/automation.
