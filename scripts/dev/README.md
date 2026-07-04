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

## smoke — TUI smoke test (`smoke/`)

`smoke/tui-smoke.sh` launches the real `thurbox` binary in a throwaway tmux
pane, drives it with `send-keys`, and asserts on captured frames (boot → F1 →
theme → quit). Runs in CI as the `tui-smoke` job; also `just smoke`.

## Dev utilities (not tests)

| Script | What it does |
|---|---|
| `sandbox.sh` | run the dev TUI/CLI against an isolated sandbox (`just sandbox*`) |
| `render-og-image.sh` | rasterize the website OpenGraph card |
| `lib/sandbox-env.sh` | shared isolation helper sourced by `sandbox.sh`, `smoke/tui-smoke.sh`, and `scripts/demo/record.sh` |

## Result contract

Every e2e harness ends on a single line: `PASS <message>` (green, exit 0) or
`FAIL <message>` (red, exit 1), from `e2e-common.sh`'s `pass`/`fail`. Set
`E2E_JSON=1` to also emit a `{"result":…}` line for CI/automation.

## Moved paths

These entrypoints were renamed; thin shims at the old paths still forward (and
print a note). Update references to the new paths:

| Old | New |
|---|---|
| `scripts/dev/remote-ssh-test.sh` | `scripts/dev/e2e/linux-container.sh` |
| `scripts/dev/windows-test.sh` | `scripts/dev/e2e/windows-vm.sh` |
| `scripts/dev/lab-test.sh` | `scripts/dev/e2e/real-host.sh` |
| `scripts/dev/tui-smoke-test.sh` | `scripts/dev/smoke/tui-smoke.sh` |
