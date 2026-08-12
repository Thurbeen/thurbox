# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code)
when working with code in this repository.

## Project

Thurbox is a multi-session coding-agent TUI orchestrator built
with Rust. It runs multiple coding-agent CLI instances (Claude
Code, Codex, Antigravity, opencode, aider, … — any CLI you
define) inside persistent tmux sessions, rendered as terminal
panels via ratatui + tui-term. Sessions survive crashes/restarts
because tmux keeps the processes alive.

Each session picks **which agent** to run from a declarative
registry (`~/.config/thurbox/agents.toml`). Thurbox is
agent-neutral: it knows nothing about any agent's model,
permissions, prompts, or tools — only how to launch the CLI with
the right `command + args`. Each agent uses its own default
config (bake a model or other flags into the agent's `args` if
you want them).

## Build & Development Commands

The reproducible dev environment is a **Nix flake** (`flake.nix`, pins the Rust
toolchain + tmux/shellcheck/node/cargo-tools/just/demo stack) — enter it with
`nix develop` (or `direnv allow` once; see `.envrc`). Non-Nix fallback:
`scripts/install-dev-tools.sh`. Task entrypoint is **`just`** (`justfile`); full
guide in **`docs/DEVELOPMENT.md`**.

```bash
just build                           # cargo build --bin thurbox --bin thurbox-cli
just test                            # cargo nextest run --all
just lint                            # fmt-check + clippy + deny + rumdl + shellcheck

cargo check --all                    # Type check (bare cargo still works)
cargo build --release                # Release build (LTO, stripped)
```

To **run thurbox in an isolated sandbox** use `scripts/dev/sandbox.sh` (a.k.a.
`just sandbox*`). By default it does **thurbox-only isolation**: redirects only
thurbox's config/data into the sandbox (via the `THURBOX_CONFIG_DIR`/
`THURBOX_DATA_DIR` overrides paths.rs honors) while keeping your real `HOME` —
so your authenticated agent CLIs (claude/codex/…) work — and puts dev
`target/debug` first on PATH so an agent hook's `thurbox-cli` hits the sandbox DB.

```bash
scripts/dev/sandbox.sh               # persistent "default" profile, launch the TUI
scripts/dev/sandbox.sh --fresh       # throwaway env, wiped on exit
scripts/dev/sandbox.sh --isolate-home    # full hermetic isolation (fresh HOME; agents have no creds)
scripts/dev/sandbox.sh --shell       # shell with the sandbox env (run thurbox-cli by hand)
scripts/dev/sandbox.sh -- session list   # run a thurbox-cli command in the sandbox
scripts/dev/sandbox.sh --clean       # wipe the persistent profile
```

The isolation lives in one helper, `scripts/dev/lib/sandbox-env.sh`
(`tbx_sandbox_init` = thurbox-only, `tbx_sandbox_init_full` = full HOME/XDG),
sourced by the sandbox entrypoint plus `scripts/demo/record.sh` and
`scripts/dev/smoke/tui-smoke.sh` (which use the full flavor). Single source of
truth for the `thurbox-dev` sandbox pattern.

## Testing

```bash
cargo nextest run --all              # Run all tests (preferred runner)
cargo nextest run -E 'test(name)'    # Run a single test by name
cargo nextest run --all --profile ci # Run with CI profile
cargo test test_name                 # Run single test via cargo test
bats scripts/install.bats            # Test install script (requires bats-core)
```

### TUI acceptance (e2e) tests

The TUI has two layers of end-to-end coverage:

- **In-process driver + snapshots** (`src/app/acceptance.rs`, a `#[cfg(test)]`
  module). A `Harness` builds a real `App` on a no-op `StubBackend` +
  `Database::open_in_memory()` + a `TestPathGuard` tempdir (fully hermetic),
  feeds `AppMessage::KeyPress` events exactly as `main.rs`'s loop does, and
  renders to a headless ratatui `TestBackend`. It also drives the loop's
  **tick**: `App::tick` is split into a deterministic `tick_core` (status
  derivation, timer expiry, search debounce, automation firing, external-change
  polling — what `Harness::tick` runs, hermetic and runtime-free) and a
  spawning `tick_background` (sysinfo/git/usage shell-outs, update checks —
  `main` only). Wall-clock-gated behavior is fast-forwarded via
  `Harness::advance` (the `app::clock` test clock — a thread-local offset every
  UI-thread timer reads through), and agent output is injected per session via
  `Harness::feed_output` (same vt100 + `TermSignals` path as the PTY reader),
  so redraw detection, OSC title/bell signals, buffer-content search, and
  terminal rendering are all testable. Stable screens (welcome state,
  F1 help, theme picker) are pinned with **`insta`** snapshots
  (`src/app/snapshots/`); dynamic flows (navigation, modals, panel toggles,
  quit) assert on `App` state instead, so live metrics/clock never make them
  flaky. Runs in the normal `cargo nextest --all` — no tmux/TTY needed. Update
  snapshots with `INSTA_UPDATE=always cargo test` (or `cargo insta review`).
- **Invariant monkey test** (`monkey_random_events_uphold_invariants` in
  `src/app/acceptance.rs`). Seeded pseudo-random event streams (keys, chords,
  mouse, ticks, clock jumps, resizes, injected agent output) against the
  harness, rendering after **every** step and checking `assert_invariants`
  (selection indices in bounds, focus never on a hidden surface, panels never
  outlive their feature flag). A failure prints the seed + step for exact
  replay. When a "weird TUI behavior" reduces to a rule, add it to
  `assert_invariants` and let the monkey hunt for a violating sequence.
- **Black-box smoke test** (`scripts/dev/smoke/tui-smoke.sh`). Launches the real
  `thurbox` binary inside a throwaway tmux pane (isolated `HOME`/XDG/
  `TMUX_TMPDIR`, mirroring `scripts/demo/record.sh`), drives it with
  `tmux send-keys`, and asserts on captured frames (boot → F1 → theme → quit).
  Gated behind the `tui-smoke` CI job (needs tmux).
- **Performance counter tests** (`perf_*` in `src/app/acceptance.rs`). Assert on
  `App::perf_counters()` — wall-clock-free `u64` counters bumped at the
  render/tick hot paths (`MetricsState::perf`) — to gate the perf optimizations
  without flaky timing: e.g. idle iterations skip the paint, the session order
  is rebuilt only when its inputs change. Run with `cargo nextest run -E
  'test(perf_)'`. See `docs/PERFORMANCE.md`.

### Dev harness layout (`scripts/dev/`)

The session-backend e2e harnesses form one family under `scripts/dev/e2e/`
(`linux-container.sh` = ephemeral Podman, `windows-vm.sh` = ephemeral dockur
Windows VM, `real-host.sh` = a machine you own) sharing one sourced library,
`e2e/lib/e2e-common.sh` — colour logging, the PASS/FAIL result contract (`pass`/
`fail`, `E2E_JSON=1` for a machine-readable line), an in-shell `json_field`
extractor (**no `python3`**), the `hosts_block` `[[hosts]]` emitter, and the
`session create → get → assert` core (`e2e_create_and_get`/`e2e_assert`). The TUI
smoke test lives at `scripts/dev/smoke/tui-smoke.sh`. `scripts/dev/README.md` is
the newcomer index (which script for which job) and carries the old→new path
map (the flat `remote-ssh-test.sh`/`windows-test.sh`/`lab-test.sh`/
`tui-smoke-test.sh` names were renamed, not kept as shims).

## Performance (render loop)

The render loop is **demand-driven** (`run_loop` in `src/main.rs`): it paints a
frame only when the UI is dirty (`App::needs_redraw`) or the 250 ms forced-redraw
floor (`FORCE_REDRAW_INTERVAL`) elapsed, not on every ~10 ms iteration.
`App::update` marks dirty on any input; `App::detect_output_redraw` on new agent
output (lock-free, via each session's `last_output_at` atomic);
`refresh_session_statuses` on a status change; the floor covers time-driven UI
(clock/metrics/cursor blink). Idle paints drop ~100 fps → ~4 fps with
input/output latency unchanged. The session-list ordering is cached keyed by a
content signature (`App::session_order_signature`), rebuilt only when its
grouping/nesting inputs change. The per-tick session-status read is likewise
cached (`App::cached_hook_states`), reloaded only when `PRAGMA data_version`
moves — so an idle `tick` no longer rescans the `sessions` table (ADR-P6), with
the `PRAGMA` throttled to ~100 ms and the per-session OSC title/notification
re-read gated on a reader-thread generation counter (ADR-P10). Restore prefetches
all history captures in parallel (ADR-P9) and code-review diffs build off the UI
thread with a loading state (ADR-P8). The **whole new-session flow is
non-blocking** (ADR-P12): the branch `git fetch` + listing (`App::branch_list` →
`poll_branch_list`), `git worktree add` (`worktree_create`), the backend ready-up
(`ensure_backend_ready`, an ssh connect for a remote host), and `Session::spawn`
all run on workers. Because those phases can run for tens of seconds on a large
repo, progress is carried by `App::pending_spawn` (`PendingSpawn`/`SpawnPhase`)
rather than a `status_message` (which expires after 5 s): it renders a
**placeholder row** in the session list plus a **status badge**. The row sits
**inside the repo group the session will land in**
(`ui::project_list::pending_spawn_slot`, keyed on
`PendingSpawn.repo_display_names`), at that group's end — where the real row will
appear — bringing its own header when the repo has no rows yet. It lives for the
**whole wizard** — background phases *and* the modals between them
(`SpawnPhase::Configuring`, a static `◌` with no spinner/elapsed) — cleared only
when the session lands, the flow errors, or the user Escs out
(`abandon_pending_spawn`). **Observability**:
`F12` toggles a live perf HUD (counters + frame/tick percentiles + slow ops;
`[features] perf_hud`); launching with `THURBOX_PERF_LOG=1` logs a `startup`
line (phase breakdown + `first_frame_ms`, plus `restore_discover`/
`restore_adopt`/`adopt_split`/`restore_capture_prefetch`), steady-state
`perf_window` lines (~10 s), and `slow op` warnings to `thurbox.log`; while
either is active the TUI publishes a JSON snapshot read by `thurbox-cli perf`.
Full rationale + intentionally-skipped optimizations: `docs/PERFORMANCE.md`.

### Windows test environment (VM)

`scripts/dev/e2e/windows-vm.sh` provisions a throwaway **Windows VM** to exercise
thurbox's Windows support, where the session backend is
[psmux](https://github.com/psmux/psmux) (a native-Windows tmux clone — same
command language, `-L` sockets, and `-C`/`-CC` control mode that `TmuxBackend`
drives, so it installs a `tmux.exe`). Mirroring `e2e/linux-container.sh`, it runs a
real KVM-accelerated Windows VM inside a single Podman container via
[`dockur/windows`](https://github.com/dockur/windows), with an unattended
first-boot `/oem` payload that installs psmux + OpenSSH + `cargo-nextest.exe` so
the harness drives the VM **headlessly over SSH**. Default edition is **Windows
11** (`VERSION=11`); dockur has no "tiny" edition token, so override
`THURBOX_WIN_VERSION` only with values dockur recognizes (`11`, `10`, `2025`, …).

```bash
scripts/dev/e2e/windows-vm.sh up         # build /oem payload + boot the VM (first run installs Windows, ~10-20 min)
scripts/dev/e2e/windows-vm.sh wait       # block until the VM's SSH is reachable
scripts/dev/e2e/windows-vm.sh test       # headless smoke test (psmux/tmux + a -L control session round-trip)
scripts/dev/e2e/windows-vm.sh test-suite # run the FULL nextest suite inside the VM (see below)
scripts/dev/e2e/windows-vm.sh deploy     # cross-build thurbox for x86_64-pc-windows-gnu + copy the .exe in
scripts/dev/e2e/windows-vm.sh ssh        # PowerShell shell in the VM; `web`/`rdp` for eyes-on; `down`/`clean` to tear down
```

`test-suite` runs the **entire `cargo nextest` suite** inside the VM. The VM has
**no Rust toolchain**, so the host cross-builds a self-contained **nextest
archive** (`cargo nextest archive --target x86_64-pc-windows-gnu`), ships it plus a
tarball of the working tree (uncommitted changes included, so insta snapshots /
fixtures resolve), and runs `cargo-nextest.exe --archive-file … --workspace-remap
…`. CI runs the same suite natively in the `windows` job (`ci.yml`,
`windows-latest`); the VM is the local/offline mirror. Tests that genuinely assume
Unix are `#[cfg(unix)]`-gated; the rest source the home dir from the platform var
(`USERPROFILE`/`HOME`) and use `tempfile`/`std::env::temp_dir()` rather than
hardcoded `/tmp`.

All state lives under `target/windows-test/` (gitignored): the throwaway SSH
keypair, the cached psmux + nextest zips, the generated `/oem` payload, the
cross-built test archive, and the VM disk image. Needs `/dev/kvm` +
`/dev/net/tun`. **Gotcha:** dockur forwards only `3389` to a Windows guest by
default, so the script sets `USER_PORTS=22` to push the published SSH port
through qemu's host-forward into the VM.

### Lab (real-host) test environment

`scripts/dev/e2e/real-host.sh <host> <verb>` (or `just lab <host> <verb>`) drives
the same checks against **any real machine over SSH** — a `~/.ssh/config` alias or
`user@address`, Linux/Windows auto-detected. Because lab machines may also run
*regular* thurbox sessions, the e2e test is fully scoped: a private
`-L thurbox-lab-test` socket + session, all remote state under one
`thurbox-lab-test` directory (repo + `worktrees_dir`), and an isolated local
`THURBOX_CONFIG_DIR`/`THURBOX_DATA_DIR` — the release socket (`thurbox`), the dev
socket (`thurbox-dev`), and real config/DB are never touched. Verbs: `check`
(readiness probe), `hosts` (print the `hosts.toml` block), `test` (headless
ssh-backend e2e, mirrors `e2e/linux-container.sh test`), `tui` (wire the host into
the persistent `lab` sandbox profile + launch), `ssh`, `clean`; Windows-only:
`deploy` (cross-build + install to `C:\Tools\thurbox`), `run` (the deployed TUI
over `ssh -t`), `test-suite` (nextest archive, mirrors `e2e/windows-vm.sh`),
`wsl-setup`/`wsl-check` (provision + verify a WSL distro as a target),
`native-test [agent]` (headless e2e of the **deployed** binaries natively:
`thurbox-cli.exe` creates a local psmux session — agent argv + `THURBOX_*` env
asserted intact — and `thurbox.exe` boots inside a scoped psmux pane and must
show/adopt it; isolated via `THURBOX_SOCKET`, since psmux has no
`TMUX_TMPDIR`-style socket-dir isolation). Local state: `target/lab-test/`
(gitignored).

## Installation Script

**Linux / macOS** — `scripts/install.sh`:

```bash
curl -fsSL https://raw.githubusercontent.com/Thurbeen/thurbox/main/scripts/install.sh | sh
```

**Windows** — `scripts/install.ps1` (PowerShell):

```powershell
irm https://raw.githubusercontent.com/Thurbeen/thurbox/main/scripts/install.ps1 | iex
```

Both installers share the same shape: ASCII banner, platform detection, version
resolution (GitHub API → releases-page scrape fallback), SHA256 checksum
verification, extract, post-install hints. From the same release, `install.sh`
pulls the `.tar.gz` for `x86_64-unknown-linux-musl` / `aarch64-apple-darwin`
(Linux x86_64 + Apple-silicon macOS — the only platforms it installs onto; it
errors cleanly on any other), while `install.ps1` pulls
**`thurbox-<ver>-x86_64-pc-windows-msvc.zip`** (the Windows artifact built by
`cd.yml`) and extracts it with the built-in `Expand-Archive` (no tar needed).
ARM64 Windows installs the x86_64 build (runs under x64 emulation).

**`install.sh` (POSIX `sh`) specifics:**

- Colorized output (auto-disabled when stderr is not a TTY, `NO_COLOR` is set,
  or `TERM=dumb`); platforms Linux/macOS × x86_64/aarch64
- No external deps beyond standard tools (curl/wget, tar, sha256sum/shasum)
- Env vars: `VERSION=v1.0.0`, `INSTALL_DIR=/path` (default `~/.local/bin`)
- Non-interactive (safe pipe-to-shell), cleanup via `trap`
- Tested by `scripts/install.bats` (bats-core, ~28 tests; CI `install-script` job)

**`install.ps1` (PowerShell 5.1+) specifics:**

- Parameters `-Version` / `-InstallDir` / `-Repo`, or the matching
  `THURBOX_VERSION` / `THURBOX_INSTALL_DIR` / `THURBOX_REPO` env vars (env vars
  are the reliable path for the `irm | iex` form, which can't pass parameters);
  default install dir `%LOCALAPPDATA%\Programs\thurbox`
- Adds the install dir to the **user** `PATH` (`[Environment]::SetEnvironmentVariable(... 'User')`)
  when missing; reflects it into the current session
- ASCII-only source (no BOM needed; survives `irm | iex` decoding on Windows
  PowerShell 5.1); `Write-Host` for UI is intentional (`Write-Output` would leak
  into the `iex` pipeline)
- Pure helpers (`Get-Target`, `Get-ExpectedChecksum`) are guarded by
  `$env:THURBOX_PS_TEST` so the file can be dot-sourced for testing without
  running the installer
- Tested by `scripts/install.Tests.ps1` (Pester 5; CI `install-script-ps` job,
  run with `pwsh` on ubuntu since the helpers are platform-independent) —
  the PowerShell mirror of `install.bats`

## Linting & Formatting

```bash
cargo fmt --all                      # Format (rustfmt: 100 char max)
cargo clippy --all-targets --all-features -- -D warnings  # Lint
RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --all-features  # Docs
rumdl check .                        # Markdown lint (.rumdl.toml)
rumdl fmt .                          # Markdown auto-fix
```

## Comments

Comments are context for the next reader — human or LLM agent. Each one must earn
its tokens; a redundant or wrong comment makes agents *less* accurate, not more.

- **Why, not what.** Explain rationale, tradeoffs, non-obvious constraints, and
  invariants the code can't show. Never restate what the code plainly does.
- **Accuracy is non-negotiable.** A stale comment (describes a prior impl, a wrong
  signature, or behavior the code no longer has) is *worse than no comment* — it
  anchors readers on the wrong intent. When you touch code, fix or delete the
  comments around it; never leave one contradicting the code.
- **Keep** design rationale, cross-references (`see fn_x`, `mirrors Y`), and
  `ADR-*` / `schema vNN` anchors (they point at `docs/ARCHITECTURE.md` /
  `docs/PERFORMANCE.md`). **Cut** restatements, obvious trailing labels (`// list`,
  `// EOF`), and obvious test-step narration. If an LLM could infer it from the
  code, it doesn't belong.
- **Doc comments** (`///`/`//!`) document the public contract. Tighten verbose
  ones, but never delete a doc that carries intra-doc links (`` [`Item`] ``) or a
  ` ``` ` example without re-running `RUSTDOCFLAGS="-D warnings" cargo doc`
  (CI fails on a broken link/example).
- **Formatting is automatic** — `rustfmt` wraps comments at 80 cols
  (`wrap_comments`); write content, let `cargo fmt` handle width.
- This repo uses **no `TODO`/`FIXME`/`HACK` markers** and keeps **no commented-out
  code** — track work in issues, delete dead code.

## Website Linting

```bash
npm ci                               # Install deps (use lockfile)
npm run lint:website                 # Run all website linters
npm run fmt:website                  # Auto-fix formatting (Prettier)
```

## Architecture Enforcement

```bash
cargo test --test architecture_rules                      # Arch rules
cargo deny check advisories                               # Advisories
cargo deny check bans licenses sources                    # Dep policy
```

## Release Process

Releases are **fully automated** via GitHub Actions. No version commits
are created - version is determined by git tags only.

### How It Works

Every push to `main` automatically triggers the release workflow:

1. **Commit Analysis**: Analyzes all commits since last tag using cocogitto
2. **Release Decision** — a release needs **both** gates to pass:
   - **Commit type** (`check-release`'s `parse` step): commits must include
     `feat`, `fix`, or `perf`. Only docs/chore/ci commits → no release.
   - **Artifact relevance** (`check-release`'s `shipped` step): the diff since
     the last tag must touch something a user installs (`src/`, `tests/`,
     `build.rs`, `Cargo.toml`/`Cargo.lock`, `rust-toolchain.toml`, `Cross.toml`,
     `extensions/`, `packaging/`, `scripts/install.{sh,ps1}`, `cd.yml`).
     Commit type alone over-releases: Renovate labels a GitHub-Actions pin bump
     `fix(deps)` and the website is versioned `feat(ui)`/`fix(ui)`, so a
     CSS-only or lint-action-only change used to cut a real release (v1.2.13
     through v1.3.0 were four website-only releases, one a *minor*) — burning a
     4-platform build and pushing to the moderated Chocolatey/winget channels
     for a no-op binary. Such commits stay in history and ride along in the
     next real release's changelog.
   - The gate is evaluated over the **whole span since the last tag**, not the
     single push, so a website-only push landing on an unreleased `src/` commit
     still cuts the release it owes. A forced `workflow_dispatch` version
     **skips** the relevance gate — an explicit human cut is always honoured.
3. **Automated Release** (if needed):
   - Determines semantic version (feat→minor, fix/perf→patch, breaking→major)
   - Creates lightweight git tag: `v{version}` (e.g., v1.0.0)
   - Pushes tag to origin
   - Builds binaries for 4 platforms (3 Unix `.tar.gz` + 1 Windows `.zip`;
     version passed via environment variable)
   - Generates changelog from commits
   - Publishes GitHub Release with binaries and release notes

### Version Management

- **Cargo.toml version**: Always `0.0.0-dev` (static development marker)
- **Real version**: Determined by release workflow (v1.0.0, v1.1.0, etc.)
- **Build-time injection**: `build.rs` uses `THURBOX_RELEASE_VERSION` environment
  variable (set by workflow) to inject version into binary
- **Development builds**: Show `0.0.0-dev` (when `THURBOX_RELEASE_VERSION` not set)
- **Release builds**: Show actual version (e.g., `1.0.0`) via env variable from workflow
- **Explicit cuts**: `cog bump --auto` computes the next version from commits and
  works for every ordinary release (at 1.x+, a breaking change correctly bumps
  the major). To cut a *specific* version that `--auto` can't reach — the only
  way across a major boundary from a `0.x` line, or any one-off — dispatch the
  Release workflow (`cd.yml`) with the `version` input (e.g. `1.0.0`); it runs
  `cog bump --version <v>` instead of `--auto`.

### Release Artifacts

Each release includes:

- Binaries for 4 platforms:
  - `thurbox-v{ver}-x86_64-unknown-linux-gnu.tar.gz`
  - `thurbox-v{ver}-x86_64-unknown-linux-musl.tar.gz`
  - `thurbox-v{ver}-aarch64-apple-darwin.tar.gz`
  - `thurbox-v{ver}-x86_64-pc-windows-msvc.zip` (the Windows artifact
    extracted by `install.ps1` / packaged by Chocolatey + winget)
- `thurbox-v{ver}-checksums.txt` (SHA256 sums for verification)
- Changelog with categorized commits

### Distribution Packages

After the GitHub Release is published, `cd.yml` also updates the downstream
package channels (each gated on its secret, skipped on forks):

- **Homebrew** (`publish-homebrew`): bumps `version`/`sha256` in
  `packaging/homebrew/Formula/thurbox.rb` (via `packaging/homebrew/bump-formula.py`,
  reading the release `checksums.txt`) and pushes it to the
  `Thurbeen/homebrew-thurbox` tap over SSH. Needs the `HOMEBREW_TAP_DEPLOY_KEY`
  secret (a write deploy key on the tap repo; the org blocks cross-repo PATs).
  Install: `brew install thurbeen/thurbox/thurbox`. Supports macOS arm64
  (`aarch64-apple-darwin`) + Linux x86_64 (`x86_64-unknown-linux-musl`).
- **AUR** (`publish-aur`): bumps + pushes `thurbox`/`thurbox-bin` PKGBUILDs.
  Needs `AUR_SSH_PRIVATE_KEY`.
- **Chocolatey** (`publish-chocolatey`): bumps `<version>` in
  `packaging/chocolatey/thurbox.nuspec` and `$url64`/`$checksum64` in
  `tools/chocolateyinstall.ps1` (via `packaging/chocolatey/bump-nuspec.py`,
  reading the release `checksums.txt`), then `choco pack` + `choco push` to the
  community repo. Runs on `windows-latest`; needs the `CHOCOLATEY_API_KEY`
  secret. New versions go through community-repo moderation.
  Install: `choco install thurbox`. Windows x86_64 only.
  **Throttled to one push per `THROTTLE_DAYS` (30d) window** because the
  community repo moderates + rate-limits every push and can't keep up with
  thurbox's per-`feat`/`fix`/`perf` cadence (versions pile up → `choco push`
  returns **403**). The job queries the community OData feed
  (`community.chocolatey.org/api/v2/Packages()`) for the last-published
  version's age; younger than the window ⇒ **skip the push, exit green** with a
  `::warning::` (patch releases coalesce into the next monthly Chocolatey
  version — the binary still ships immediately via GitHub Releases +
  Homebrew/AUR/winget). A residual `403`/`409` at push time is likewise caught
  and exits green; only a genuine failure (bad package, auth) fails red — so a
  backed-up channel never turns the whole release red.
- **winget** (`publish-winget`): bumps `PackageVersion`/`InstallerUrl`/
  `InstallerSha256`/`ReleaseNotesUrl` in the three manifests under
  `packaging/winget/manifests/` (via `packaging/winget/bump-manifests.py`,
  reading the release `checksums.txt`), then `wingetcreate submit`s the set as a
  PR to `microsoft/winget-pkgs`. Runs on `windows-latest`; needs the
  `WINGET_TOKEN` secret (a `public_repo` PAT owning a fork of
  `microsoft/winget-pkgs`). New versions go through winget-pkgs PR
  validation + review.
  **Throttled to one submission per `THROTTLE_DAYS` (30d) window**, mirroring
  Chocolatey, because winget-pkgs is *manually moderated* — each `submit` opens a
  PR a human must review, so thurbox's per-`feat`/`fix`/`perf` cadence buried the
  maintainers (30 open PRs at once, flagged in
  [microsoft/winget-pkgs#405639](https://github.com/microsoft/winget-pkgs/pull/405639)).
  The throttle step ages our own last thurbox PR (via `gh pr list`, merged or
  open) — the winget analog of the choco feed's Published date; younger than the
  window ⇒ **skip the submission, exit green** with a `::warning::` (they
  coalesce into the next monthly PR). As second-line cleanup for a PR that still
  stacks (e.g. a manual dispatch inside the window), a follow-up `gh pr close`
  closes every older still-open `Thurbeen.thurbox` PR from the token account
  (wingetcreate's `--replace` only supersedes a *published* manifest version,
  not a pending PR; best-effort, never fails the release).
  The release zip is a `zip` installer with
  `NestedInstallerType = portable` (PATH aliases `thurbox`/`thurbox-cli`, no
  MSI). Install: `winget install Thurbeen.thurbox`. Windows x86_64 only.

See `packaging/README.md` for the full packaging overview.

### Commit Types and Versioning

Thurbox 1.0+ follows [Semantic Versioning](https://semver.org/):

- **feat**: Minor version bump (1.x.0)
- **fix, perf**: Patch version bump (1.0.x)
- **docs, chore, ci, style, test**: No release (appear in next version)
- **BREAKING CHANGE**: Major version bump (x.0.0)

A breaking change bumps the major version automatically via `cog bump --auto`
(at 1.x+; on a `0.x` line cocogitto maps breaking to a *minor* bump instead, so
the only way to cross into 1.0 was the explicit-version Release dispatch).

## Conventional Commits

All commits must follow
[Conventional Commits](https://www.conventionalcommits.org/).
Enforced by cocogitto via pre-commit hooks.

- **Types**: feat, fix, perf, refactor, docs, style, test,
  chore, ci, build, revert
- **Scopes**: api, cli, ui, git, core, docs, deps, config, mcp
- Use `cog commit feat "message"`
  or `cog commit fix "message" scope`

## Agent Definitions

> Per-agent reference + the "adding a new built-in" checklist:
> `docs/AGENTS.md` (each built-in's exact config, ID model, and status-hook
> mechanism, plus every file to update when promoting a CLI to a built-in).

The set of launchable coding agents is declared **as data** in
`~/.config/thurbox/agents.toml`, seeded with built-ins
(`claude`, `codex`, `antigravity`, `opencode`, `aider`, `copilot`, `vibe`, `pi`, `omp`) on first run.
Each `[[agents]]` entry is an `AgentDef`:

```toml
default = "claude"

[[agents]]
name = "claude"
command = "claude"
args = []                               # always passed; bake a model here if you want one
resume_args = ["--resume", "{id}"]      # emitted when resuming
fork_args = ["--resume", "{id}", "--fork-session"]
new_session_args = ["--session-id", "{id}"]  # emitted on a fresh spawn

[[agents]]
name = "codex"
command = "codex"
resume_args = ["resume", "--last"]      # id-less: resumes the last session in cwd
fork_args = ["fork", "--last"]
resume_latest = true
```

Each `*_args` group is appended only when its driving value is
present, with `{id}` substituted; `args` is always passed. No
model is ever passed — each agent uses its own default config
(put `["--model", "opus"]` in `args` if you want to pin one).
A second token, `{home}`, expands (at spawn, on the spawn worker —
`session_ops::expand_home_in_def`, called from
`spawn::adapt_def_for_launch` for a launch and `App::launch_provider_for`
for a restart) to the resolved home dir — the **remote** home for an
SSH/WSL host — so an agent that wants a session *file path* rather than a
bare id (the built-in `omp`, below) launches against a concrete,
quote-safe absolute path (a literal `~` would never expand — args are
POSIX-quoted).
Agents that omit `resume_args` simply start fresh on restart (the
live tmux process is what carries state across TUI restarts). Add
your own `[[agents]]` entry to support any CLI — no recompile.

**Session id pinning vs. `resume_latest`.** thurbox generates the
`agent_session_id` (a UUID) and `claude`/`pi` accept it at creation
(`--session-id {id}`), so only those two resume/fork by that exact id. The other
built-ins (`codex`, `opencode`, `antigravity`, `aider`, `copilot`) can't pin or
report their id, so they set `resume_latest = true` with **id-less** resume/fork
flags: the agent resolves "the last session in *this* directory" itself (`codex
resume --last`, `opencode --continue`, `agy --continue`, `aider
--restore-chat-history`, `copilot --continue`) — which works because restart
reuses the session's cwd and a single-repo fork reuses the parent's.
`resume_latest` only changes *when* the resume group fires
(`session_ops::resume_trigger_for`): for these agents restart always resumes; for
claude it defers to an on-disk transcript check. **`omp`** (Oh My Pi) is a third
kind: it generates its own internal id and won't take thurbox's, but its
`--session <path>` creates a fresh session at a missing path, so thurbox maps its
UUID to a deterministic file (`--session
{home}/.omp/agent/sessions/thurbox-{id}.jsonl` on create, `--resume` the same on
restart). Neither id-pinned nor `resume_latest`: `resume_trigger_for` resumes it
iff that JSONL exists (`session_file_template` — agent-neutral, keyed on a
`new_session_args` token that is a path *and* carries `{id}`, not on the agent
name); a remote-omp restart can't stat the host file from the UI thread, so it
starts fresh (documented fallback). Caveats: agents without `fork_args`
(`antigravity`, `aider`, `copilot`, `omp`) start fresh on `Ctrl+F`; and a
**multi-repo** fork of a cwd-scoped agent lands in a fresh symlink workspace, so
`--last`/`--continue` finds no parent session (multi-repo *restart* still resumes,
keeping the same workspace dir).

- **Data type**: `session::AgentDef` / `session::AgentRegistry`
  (`session/agent_def.rs`, pure data + substitution logic).
- **Loading**: `agent::agent_config::load_or_seed()` reads/seeds
  the TOML; `builtin_registry()` is the fallback.
- **Launching**: `agent::GenericProvider` wraps an `AgentDef` and
  implements the `AgentProvider` trait (`command()` +
  `build_args(&SessionConfig)`). `App::provider_for(&config)`
  picks the provider for the session's agent.

A session stores only its **agent name**; there are no
per-session model/permission/prompt/tool knobs.

**Custom-agent status hooks (`hook_schema`).** thurbox stays agent-neutral, so
the built-in **hooks** extension wires status hooks only for the built-ins it
knows by name — a **custom** agent (e.g. a rebranded-claude `fleet`) gets no
`--settings` patch and so never reports working/blocked/done. An optional
`AgentDef.hook_schema: Option<String>` closes this: it names the hook **family**
the CLI speaks, and `agent::extension_config::apply_agent_patches` (+ its
uninstall reverse) fans each `[[agent_patches]]` out to the built-in named
`patch.name` **and** every agent with `hook_schema == patch.name`. So
`hook_schema = "claude"` on `fleet` injects the same `--settings {home}/
claude.json` claude gets — and, because the remote rewrite
(`session_ops::spawn::adapt_agent_args_for_remote`) keys off the `--settings`
arg rather than the agent name, remote/WSL wiring follows for free. Only the
per-arg-patch families (claude, aider) need it; codex/opencode/antigravity/vibe/
copilot wire through their own config dir, so a rebrand sharing that dir already
reports. thurbox bakes in no agent knowledge — the *user* asserts the family.

### Multi-repo sessions (symlink workspace)

A session can span several repositories (the repo picker allows multiple;
headless callers pass `--add-repo`/`--add-dir`, below). Because agent CLIs differ
wildly in how — or whether — they accept extra directories, thurbox passes **no**
per-agent `--add-dir`-style flags. Instead, a session with more than one member
directory launches in a per-session **symlink workspace**:
`~/.local/share/thurbox/workspaces/<agent_session_id>/` holds one symlink per repo
(worktree checkout or plain dir) and the agent starts there (`cwd` = the
workspace), so every agent sees each repo as a subdirectory — agent-neutral, no
`agents.toml` changes.

`SessionInfo.cwd` keeps the **primary** repo (display / editor / git context); the
workspace is a spawn-time process-cwd detail, derived idempotently on every launch
from the persisted members and never stored. `workspace::ensure_workspace` /
`remove_workspace` (`src/workspace.rs`) build and tear it down; the member set is
the single `App::session_member_dirs` list that also feeds the rendered repo
names, and `App::resolve_process_cwd` picks workspace-vs-primary. Single-repo
sessions are unchanged (`cwd` = the repo directly).

**Headless multi-repo.** The same multi-repo shape is reachable without the
TUI: `SpawnRequest.extra_repos: Vec<ExtraRepo>` (`session/automation.rs`) carries
each additional repo, where `ExtraRepo { repo_path, worktree: bool, base_branch }`
either gets its **own isolated worktree** on the spawn's shared `worktree_branch`
(off its own base — the per-repo-PR model flow uses) or is attached **as-is** as
an additional dir. `session_ops::spawn::resolve_dirs` builds the worktrees +
additional dirs and `resolve_launch_cwd` mirrors the TUI's `resolve_process_cwd`
(symlink workspace when ≥2 members). The CLI exposes it on `session create` and
`task create` via repeatable `--add-repo PATH[@BASE]` (worktree) and `--add-dir
PATH` (as-is); `AutomationAction::Spawn` persists the list as JSON in the
`action_extra_repos` column (schema v33, on both `tasks` and `automations`;
`NULL`/empty = single-repo, so old rows are byte-identical). The flow extension's
`create-task.sh` forwards these flags (see `extensions/flow/FLOW.md`).

## Remote SSH & WSL Sessions

Sessions can run on an **off-local host** while the TUI runs locally: a
**remote machine over SSH**, or a **local WSL distro** (`wsl.exe`). A WSL
distro is modeled as "SSH without the ssh" — the *only* difference is the
launch prefix (`wsl.exe -d <distro>` vs `ssh <dest>`); tmux, git, the agent,
and the worktrees all run **inside the distro** at native Linux paths, so
everything downstream of the launcher (control-mode protocol, POSIX quoting,
worktree layout) is identical to the SSH path — no `wslpath` translation. Hosts
are declared as data in `~/.config/thurbox/hosts.toml` (seeded commented-out;
fresh install = zero SSH hosts, behaves as before), **plus WSL distros are
auto-discovered on Windows** (`wsl.exe -l -q`) with no config. The seeded file
documents every field inline; the schema:

```toml
# An SSH host (the default kind):
[[hosts]]
name = "devbox"               # required — backend id "ssh:devbox"; what --host expects
destination = "me@devbox"     # required for ssh — target ("user@host" or ~/.ssh/config alias)
ssh_opts = ["-o", "ControlMaster=auto", "-o", "ControlPersist=10m", "-o", "ServerAliveInterval=15"]
                              # optional (default []) — extra ssh flags; no ~ expansion, use abs paths
socket = "thurbox"            # optional (default "thurbox") — host `tmux -L` socket
session = "thurbox"           # optional (default "thurbox") — host tmux session name
worktrees_dir = "/home/me/.local/share/thurbox/worktrees"
                              # optional — abs worktrees dir on the host
multiplexer = "tmux"          # optional (default "tmux") — set "psmux" for a Windows SSH host

# A WSL distro (only needed to OVERRIDE auto-discovery, e.g. a custom worktrees_dir):
[[hosts]]
name = "ubuntu"               # → backend "wsl:ubuntu"; what --host expects
kind = "wsl"                  # required to select the WSL transport
distro = "Ubuntu-22.04"       # optional (default = name) — the wsl.exe distro name
```

Only `name` (+ `destination` for ssh, `kind` for wsl) is required; every other
field's default is in the comments above and in `docs/CONFIG.md`.

How it works: `TmuxBackend` is transport-neutral
(`agent::transport::TmuxTransport`). The local backend launches
`<mux> -L thurbox …`; an SSH backend launches `ssh <dest> <mux> -L thurbox …`;
a **WSL backend launches `wsl.exe -d <distro> tmux -L thurbox …`**
(`TmuxTransport::Wsl`). `wsl.exe` forwards whitespace-free tokens to the
in-distro shell like `ssh` does, so the same POSIX quoting
(`shell::posix_quote`) and the byte-identical control-mode protocol
(`control_mode.rs`) apply — only the one-time process launch differs. (An arg
*containing whitespace* is preserved as one word, so multi-word `sh -c` scripts
go through `wsl.exe --exec` instead — see `shell::wsl_command` /
`git::host_shell_c`.) The local `DEFAULT_MUX` is **`tmux` on
Linux/macOS and `psmux` on Windows** — psmux is a native-Windows, drop-in tmux
clone (ConPTY, no WSL) speaking the **same control-mode wire protocol** and
pane-id (`%N`) / `-L` socket model, so the whole backend is parameterized by
binary name rather than forked (a remote SSH host can also pin
`multiplexer = "psmux"`); a WSL distro runs `tmux` inside the distro. The
control-mode protocol is byte-identical over either transport/binary, with
**psmux divergences** (verified against psmux 3.3.6, each branched on
`TmuxTransport::uses_psmux()`) — psmux lacks `send-keys -H`, does not join
`new-window` trailing tokens or honour its `-e`, and implements no control-mode
paste command. So thurbox re-encodes keystrokes from the primitives psmux does
support (`send_keys_commands`), folds env + command into **one token** of
PowerShell (`psmux_window_powershell`), and routes a bracketed paste out of band
through the one-shot CLI `psmux send-paste` (`control_mode::PsmuxPaste`). Each
workaround has non-obvious quoting/tokenizing constraints — **read the psmux
divergences subsection of ADR-13 in `docs/ARCHITECTURE.md` before touching this
path**; delivery is probed by `scripts/dev/e2e/windows-vm.sh test` (probes C, D).
Each host registers a backend named
`ssh:<name>` / `wsl:<name>` (`TmuxBackend::from_host`, registered lazily in
`main.rs` from `host_config::load_all_with_warnings`: discovery/down hosts must
not block startup, so `check_available`/`ensure_ready` are deferred to first use
— `App::select_backend` only looks the backend up, and the blocking
`ensure_backend_ready` runs on a worker at spawn time, ADR-P12).

- **Data**: `session::HostDef` (with `kind: HostKind {Ssh, Wsl}`) /
  `HostRegistry` (pure data, in `session/` so both `agent` and `git` can use
  it); backend-name helpers `is_ssh_backend`/`is_wsl_backend`/
  `is_remote_backend`. **Loading**: `agent::host_config::load_all{,_with_warnings}`
  = configured hosts + `discover_wsl_hosts()` (deduped; a configured entry wins).
- **Selection**: `SessionConfig.backend` (`ssh:<host>` / `wsl:<distro>` or `None`
  = local). The TUI new-session flow shows a **host picker** first (skipped when
  none configured/discovered); the chosen host runs git worktree creation +
  branch listing on that host.
- **Worktrees**: `git::*_on(host, …)` variants run `git` via the host launcher
  (`git::host_launcher` → `ssh …` or `wsl.exe …`). Worktrees live under the
  host's `worktrees_dir` (or `$HOME/.local/share/thurbox/worktrees` resolved +
  cached per backend name — a WSL distro has no `destination`).
- **Persistence/restore**: `backend_type` round-trips in SQLite; restore
  discovers windows **per backend** so off-local sessions re-adopt against their
  own host. Remote backends are readied + discovered **in the background** (one
  thread per host, drained by `App::poll_remote_restore` each tick) so an
  unreachable or slow host never blocks the first frame — only local sessions
  restore synchronously at startup (ADR-P7, `docs/PERFORMANCE.md`).
- **Headless**: `thurbox-cli session create --host <name>` spawns on the host
  (an SSH name or an auto-discovered WSL distro name).
- **Agent config on the host**: agent args referencing thurbox-managed config
  by *local* path (the hooks extension's `--settings <config>/hooks/
  claude.json`) would kill the remote agent on launch ("Settings file not
  found"). `session_ops::spawn::adapt_def_for_launch` (shared by headless
  spawn and the TUI, run on the spawn worker — never the UI thread) rewrites
  them per host: on a POSIX remote the home-anchored path is **translated to
  the remote home**, the file copied there, and the arg substituted; on a
  psmux host (while `psmux_hook_rewrite_supported` stays off) / non-POSIX
  config root / failed copy the **flag+path pair is stripped** so the agent
  launches clean — surfaced as a `Hooks: degraded` row in the info panel
  (`SessionInfo.hook_wiring`). Literal signal commands carried directly in
  args (aider's `--notifications-command`) are rewritten too.
  The local-path env hints
  (`THURBOX_METRICS_DIR`/`THURBOX_CONFIG_DIR`/`THURBOX_DATA_DIR`) are likewise
  skipped for remote spawns (`inject_thurbox_env`); only the opaque identity
  vars travel.
- **Remote session status** (hooks-driven, like local, **all agents**):
  `thurbox-cli session signal` can't work from a host (no CLI there; it would
  write the host's own DB), so hook commands are **rewritten**
  (`builtin_hooks::rewrite_hook_signals_for_target`) to set a tmux **pane user
  option** instead — `tmux set-option -p @thurbox_state <s>` needs no socket,
  pane id, or identity inside a pane (the psmux form bakes in
  `-L <socket>`). Delivery per agent: claude's hooks file travels via its
  `--settings` arg; agents wired through their **own config dir** (codex,
  antigravity, opencode, vibe, copilot) are provisioned at spawn time by
  `session_ops::remote_hooks::provision_agent_hooks_on_host` — the rewritten
  payload shipped into the host's agent config dir with the local installer's
  safety rules (`requires_dir` probe over ssh, prune-then-merge for shared JSON,
  managed-marker guard for standalone files, compare-before-write; cached per
  `(backend, agent)`, best-effort, never fails the spawn; remote **cleanup** is a
  documented leave-behind). The local TUI's persistent control-mode connection
  subscribes once per connection (`refresh-client -B
  'thurbox-status:%*:#{@thurbox_state}'`, armed in `ControlMode::start` so
  reconnects re-arm; tmux ≥ 3.2 = the existing floor) and receives
  `%subscription-changed` pushes (≤1/s); a **remote psmux** connection instead
  runs a 1 s **poller thread** (`list-panes -F` diffed by
  `control_mode::diff_polled_hook_states`) feeding the same queue — armed only
  behind the psmux gate below (a poll is an active per-second command, unlike the
  passive subscription, and a *local* psmux session signals via `thurbox-cli`).
  Both channels drain each tick via `App::drain_remote_hook_events` into the same
  `set_hook_state` columns local signals use — so Done→seen acknowledgment, OS
  notifications, and the stuck-`working` fallback are shared. Events are matched
  by **backend name + pane id** (pane ids collide across hosts), allow-listed
  (remote-controlled text), and deduped against the cache (a reconnect re-report
  must not resurrect an acknowledged `done`). Those live channels die with the
  TUI, so the headless **`automation tick`** (the 60 s heartbeat keeper) also
  polls each host with live remote sessions in the DB
  (`session_ops::remote_hooks::poll_remote_hook_states` — one-shot `list-panes
  -F`, allow-listed, diffed against the stored `hook_state`) and writes changes
  into the same columns, so remote status keeps flowing with the TUI closed at
  tick cadence. Remaining carve-out: the **whole psmux/Windows-host path** — hook
  provisioning, rewrite shipping, and the status poller — is gated off on one
  switch (`session::psmux_hook_rewrite_supported`) until the psmux behaviors are
  proven by `scripts/dev/e2e/windows-vm.sh test`'s probes; such sessions show a
  `Hooks: degraded` hint instead of silently idling.
- **Remote teardown** (WSL inherits the SSH path): `session delete --force`
  teardown is **backend-aware** — `teardown_runtime_resources` resolves the
  session's `HostDef` from its `backend_type` and, for a remote session, kills
  the pane via `kill_pane_remote(host, backend_id)` and removes each worktree
  via `git::remove_worktree_on(Some(host), …)` (local sessions keep the
  `kill_window`/`remove_worktree` + Windows pane-reap path). Best-effort: an
  unreachable host or a missing `hosts.toml` entry is recorded in
  `ForceDeleteReport.remote_teardown_error` (surfaced in the CLI JSON) and the
  row is still soft-/force-deleted. Like local force-delete it removes the
  worktree *directory* only, leaving the branch. `wsl.exe`'s exact arg-passing
  isn't verified in CI (no WSL runner); the construction is unit-tested
  (`transport::tests::wsl_*`, `git_command_wsl_*`).
- **Local e2e**: `scripts/dev/e2e/linux-container.sh up` spins a throwaway Podman
  container (sshd + tmux + git) and `… test` asserts a session lands on the
  `ssh:podman` backend (state under `target/`, never touches your real
  `~/.ssh`/`~/.config`).

## thurbox-cli

A second binary (`thurbox-cli`) drives the same SQLite-backed,
tmux-hosted sessions headlessly (no TUI). It shares the database
with the TUI; changes appear via `PRAGMA data_version` polling.

```bash
cargo build --bin thurbox-cli
thurbox-cli session create --name demo --repo-path /path \
    --agent codex --worktree-branch feat/x
# Spawn on a remote host from hosts.toml (worktree + tmux live remotely):
thurbox-cli session create --name demo --repo-path /srv/repo \
    --host devbox --worktree-branch feat/x
# Spawn a worker under a lead session (parent must exist):
thurbox-cli session create --name worker --repo-path /path \
    --parent <lead-uuid>
# Multi-repo: each --add-repo gets its own worktree on --worktree-branch;
# --add-dir attaches a repo as-is (no branch). The agent launches in a
# symlink workspace gathering every repo. Works on `task create` too.
thurbox-cli session create --name demo --repo-path /a \
    --agent claude --worktree-branch feat/x \
    --add-repo /b@main --add-repo /c@master --add-dir /reference
thurbox-cli session list                       # human-readable table
thurbox-cli session list --json | jq           # machine output for scripts
thurbox-cli session list --parent <lead-uuid> --json | jq  # direct children only
```

Subcommands: `session` (create/list/get/delete/restore/restart/
send/capture/focus/signal), `automation` (alias `auto`:
create/list/show/edit/remove/run/runs/tick), `task` (alias `todo`:
create/list/show/edit/remove/run), `message` (alias `msg`:
send/inbox/prune — the inter-session mailbox queue; see below), `editor`
(get/set the Ctrl+O editor command; `editor mode <auto|terminal|gui>` chooses
how it launches — terminal editors get a real TTY via a tmux popup or TUI
suspend, GUI editors spawn detached; see the Editor Integration section of
`docs/FEATURES.md`), `config`
(validate/show — strict-parses every config file / prints the
effective resolved config; see `docs/CONFIG.md`), `extension`
(alias `ext`: install/uninstall/reinstall/list/available/update/activate/
deactivate/status — manage opt-in extensions; see below), `version`
(prints the running version; `--check` queries GitHub's latest release —
gated on `[features] version_check`, on by default for 1.0), `update`
(downloads, verifies, and replaces the installed binaries with the latest
release — `--force` bypasses the up-to-date/dev-build guards; gated on
`[features] auto_update`, on by default for 1.0; the TUI also runs this silently on
startup when the flag is on), `notify`
(diagnose OS desktop notifications: prints the detected delivery backend
and last error; `--test` fires a sample — see OS notifications below), `perf`
(print the perf snapshot a running TUI publishes while `THURBOX_PERF_LOG`
or its perf HUD is active — see `docs/PERFORMANCE.md`).
Output is
**human-readable by default** and switches to JSON automatically when stdout is
piped (so `… | jq` keeps working); force a format with `--json` (compact),
`--pretty` (indented JSON), or `--text` (human even when piped).

`session delete <uuid>` **soft-deletes** by default — only the DB row is marked
deleted (the TUI tears down the tmux window/worktree on its next sync), and
`session restore` revives it. `--force`
(`session_ops::delete_session_headless`) also kills the tmux window, removes
worktrees + the symlink workspace, and disables `send` automations targeting the
session — for headless cleanup with no TUI running. Teardown is best-effort
(failures land in the JSON report); the row is always soft-deleted last. A
`--force` delete stamps `sessions.force_deleted` (schema v37): the row still
appears in the restore list **tagged `force-deleted`** and is restorable
**best-effort** — force-delete removes the worktree *directory* but not the git
branch, so restore reattaches each surviving branch's committed work
(`App::recreate_worktrees`); only uncommitted/untracked changes are gone. Because
that recovery is lossy, the headless `session restore` **refuses a force-deleted
row unless `--best-effort`** (its JSON then carries `best_effort: true`).
`restore_session` clears both `deleted_at` and `force_deleted`.

The **TUI** `Ctrl+D` soft-deletes too (with a `Ctrl+Z` undo window). The
`[features] soft_delete` flag (default `true`) governs only this TUI path: set it
`false` and `Ctrl+D` becomes a hard delete — the same
`delete_session_headless(.., force=true)` teardown — since there is no `Ctrl+Z`
for it. That hard delete is **conditional**: a confirmation modal
(`Modal::ConfirmDelete`, `ui::confirm_delete_modal`) appears **only when the
session has work at risk** — uncommitted/untracked files, unmerged commits, or a
state that can't be verified (remote host / git error → confirm to be safe;
`App::assess_delete_risk` + `modals::DeleteRisk::from_stats` over
`git::worktree_stats`) — itemizing what would be lost; a known-clean session is
deleted with no prompt. Restoring a force-deleted row via `Ctrl+U` (`Enter`) first
confirms (`Modal::ConfirmRestore`, `ui::confirm_restore_modal`) since recovery is
committed-state-only, then runs the normal restore path. The flag never changes
`thurbox-cli session delete`, which stays soft unless `--force`.

### Parent sessions (lead/worker)

Sessions carry an optional **`parent_session_id`** so orchestration scripts can
model lead → worker relationships. `session create --parent <uuid>` sets it (the
parent must be an existing active session — validated before any side effects);
`session list`/`get` emit it in the JSON (`null` for top-level) and `session list
--parent <uuid>` filters to direct children. The link is **purely
informational**: deleting a parent never cascades (orphans render as top-level),
and the parent is only validated at creation. In the TUI, **`Ctrl+F` fork**
records the source session as the fork's parent; the session list nests children
under their parent **within the same repo group** (muted `└` tree prefix; a child
whose parent renders in another group keeps its own position with a `↳` mark), and
the info panel (F2) shows a `Parent:` row. The nesting lives in
`ui::project_list::compute_session_order` (`SessionOrder::depths`), so
`Ctrl+J`/`Ctrl+K` navigation follows the tree automatically. Storage: nullable
`sessions.parent_session_id` (schema v30; v29 is reserved by an in-flight branch).

### Manual session ordering

The session list is **manually orderable**: `Shift+J`/`Shift+K` (session list
focused; rebindable `SessionListMoveDown`/`SessionListMoveUp`) move the selected
session one row down/up. Manual order **wins** — status changes only recolor the
dot, never move a row. A move swaps two adjacent *blocks* (a row plus its nested
children, so a parent drags its subtree): root rows swap within their repo group,
the **whole group** swaps past a group edge, and nested children move among their
siblings only (`ui::project_list::move_in_order`, pure;
`App::move_active_session` applies it). Every move densely renumbers all sessions
`0..n` and persists, so the order survives restarts and syncs across instances via
`data_version` polling. Storage: nullable `sessions.display_order` (schema v31);
`None` = never moved, renders after ordered sessions in creation order (new
sessions append to their group). **`Shift+S`** (rebindable
`SessionListSortAlphabetically`) sorts by name **within each repo group** in one
shot, preserving group order (still by lowest `display_order`) and parent/child
nesting, reusing the same dense-renumber-and-persist path (pure helper:
`ui::project_list::sort_alphabetically_within_groups`;
`App::sort_sessions_alphabetically`).

### Inter-session messages (mailbox queue)

A general, agent-neutral **message queue** lets one session hand another a
**structured payload** without scraping its rendered terminal — the channel
extensions use for agent↔agent coordination (flow's clarify→plan→build relay is
the first consumer). A message is addressed **to** a session and carries a
free-form `kind` tag (`questions`/`plan`/`result`/… are conventions, not an enum),
a `body`, and optional provenance. Storage is the `session_messages` table (schema
**v32**, CRUD in `storage/messages.rs`); `Database::claim_messages` is a single
`UPDATE … RETURNING`, so the TUI, a cron tick, and a wake nudge can drain
concurrently without double-processing.

- **Identity (the registry key, self-knowable).** A session's `SessionId` is
  **stable for life** — `respawn_stale_session` reuses the original id on
  re-adoption (no soft-delete + new-row churn), so a cached id or queued message
  never goes stale. At spawn thurbox injects `THURBOX_SESSION` (= the `SessionId`,
  threaded via `SessionConfig.session_id` so it's known *before* launch and reused
  on respawn) and, for task-spawned sessions, `THURBOX_TASK` (= the task id) —
  both distinct from the older `THURBOX_SESSION_ID` (= `agent_session_id`, read by
  the metrics statusline). So a `thurbox-cli` call *inside* a session proves its
  own identity without scraping panes or names.
- **Consequence for the CLI surface**: an agent passes **no ids**. `message send
  --to <uuid|name>` stamps provenance from the injected identity, `message reply
  <message_id>` routes back to that message's sender (the replier never learns a
  peer's session id), and `message inbox [--claim]` defaults `--for` to the
  calling session. A send/reply with a wake also arms the automation heartbeat so
  a missed wake is still drained headless.

**Full flag list, the body/kind limits, backpressure cap, and retention/pruning
are in the Inter-Session Messages section of `docs/FEATURES.md`.**

An automation's `AutomationAction` is one of: **Send** (paste a prompt into a
running session), **Spawn** (start a fresh session and prompt it), or **Exec**
(run a shell command headlessly — `sh -c`, or `cmd /C` on Windows — with no
agent/session; its exit status + tail-truncated output land in the run history).
`Exec` is the deterministic-scheduled-job action (the task-integration sync
extensions use it). The shared runner is `session_ops::run_exec_command`, which
blocks until the child exits: the headless `automation tick` calls it directly,
while the TUI (`App::start_exec`) hands it to a **detached worker thread** so a
slow command can't park the render loop inside `waitpid` — the run is recorded
when `App::drain_exec_results` picks up the result on a later tick, and an
automation whose command is still in flight records a `skipped` run instead of
launching a second copy. The
command is stored in the `action_command` column (schema **v36**, on both
`tasks` and `automations`). Author one headlessly with `thurbox-cli automation
create --command "<shell>"` (mutually exclusive with `--session`/`--repo`), in
the TUI editor (the action selector now cycles Send → Spawn → Exec), or from an
extension manifest (`[[automations]]` with a `command` field instead of
`session_ref`/`prompt`). `Task.action` shares the enum but tasks never carry an
`Exec` (it's automation-only).

Automations fire even when the TUI is closed: a tmux heartbeat
keeper window (`automation-heartbeat`, armed on TUI startup and on
`automation create`) loops `automation tick` every 60 s and keeps
the tmux server alive. `packaging/` ships opt-in systemd/launchd
units for reboot-proof firing. Concurrent firers are de-duplicated
by `Database::claim_due_automation` (atomic CAS), so the TUI, the
keeper, and an OS timer never double-fire.

In the TUI, automations also get a dedicated **Automations pane** beneath the
session list (left column), always present (showing `none` when empty) unless
`[features] automations = false`, which hides the pane (the session list takes the
whole column, `j`/`k` wrap within it), blocks `Ctrl+P`, stops the TUI firing
schedules, and skips arming the heartbeat (the CLI stays fully functional). It is
treated as **part of the session pane**: one continuous, **circular** vertical
list with the session list — `j` past the last session drops into the pane, `k` at
the top automation hands focus back, and the ends wrap (`j` past the last
automation loops to the top of the session list, `k` above the first session to
the last automation). It is **not** a separate stop in the `Ctrl+H`/`Ctrl+L` cycle
(which treats it like the session list). Once focused, `j`/`k` select,
`Space`/`r`/`d` toggle/run/delete, `n` creates one.

The pane mirrors the session list, with the **central pane** as its
terminal-equivalent: while the pane is focused the central pane shows a **single
editor** for the selected automation (a live preview — no separate read-only info
screen). `Enter`/`Ctrl+L` (or `e`) focuses that editor, exactly as `Enter` on a
session focuses its terminal; `Ctrl+H`/`Esc` returns to the list, `Enter` saves,
`Esc` discards, `Ctrl+E` toggles enabled. The scoped automation's run history
(`db::list_automation_runs`, cached in `App::cached_automation_runs`) renders
beneath the editor and is itself focusable
([`InputFocus::AutomationRunHistory`], one more `Ctrl+L`): `j`/`k` select a run
(`App::automation_run_index`), `r` triggers a fresh run, `Enter` opens the session
that run touched (`App::open_run_related_session` parses the session id out of the
run's `detail`). `Ctrl+L`/`Ctrl+H` cycle **within the current context's ring**
(`App::focus_ring`) — the automation ring `Automations → editor → run history`
wraps back to `Automations` (never to a session; landing on the list discards
edits like `Esc`), the session ring is `SessionList → Terminal` (+ file viewer);
crossing contexts is via `j`/`k`, not the cycle. Because the in-pane
editor/history would otherwise lose chords like `Ctrl+E` to global keybindings,
`handle_key` captures input for those two focuses **before** the global lookup,
passing only the focus-cycle/quit chords. Implemented via the persistent
`App::automation_editor` state (synced by `App::sync_automation_editor`) plus
`ui::automation_editor_modal::render_automation_editor_into` +
`ui::automation_detail::render_run_history`. The `Ctrl+P` list path opens the same
editor as a centered overlay (`Modal::AutomationEditor`); both share
`AutomationEditorModal::handle_key` + `App::save_automation`.

## Tasks (todo list)

Thurbox has a **task list**: todo items (title + markdown description +
status). The whole TUI surface is gated by `[features] tasks` in
settings.toml (disabled: F5/Ctrl+W toasts, no task search results; the
CLI stays functional). A task can be **acted on by a coding agent** via a **trigger-time
picker** (`r`): you choose *Send → a running session* or *Spawn new session…*
(the normal repo→agent flow) at the moment you act — the action is **not**
authored into the task. Either way the agent is seeded with a **full context
prompt**, not the bare title: `Task::agent_prompt()` builds an `id + # title +
markdown description` block plus self-service hints (`thurbox-cli task show
<id>` to read the full record, `thurbox-cli task edit <id> --status done` to
close it out). The TUI seeds it via `App::task_agent_prompt` (bracketed-paste
safe, so the multi-line body never submits early); the headless `task run` path
builds the same string. Triggering advances the task `Todo → InProgress` (TUI:
`App::advance_task_to_in_progress`; CLI: `mark_in_progress`).
(`Task.action: Option<AutomationAction>` still exists for the CLI / external
sync, but the TUI editor never sets it.)

- **Data** (`session/task.rs`): `Task` (`id`, `title`, `description:
  Option<String>` (markdown, `None` when blank), `status: TaskStatus`
  {`Todo`/`InProgress`/`Done`}, `action: Option<AutomationAction>`, plus
  `source`/`external_id`/`external_url` for external-tracker sync — `source =
  "local"` for native todos, or a tracker tag (`github`/`gitlab`/`linear`/`jira`)
  for items imported by the task-integration extensions. `(source, external_id)`
  is the natural dedup key.
- **Storage** (`storage/tasks.rs`, schema v25): `tasks` table mirroring the
  automation action columns (`action_kind` nullable) plus a nullable
  `description` (v26 migration), soft-delete via `deleted_at`, audited under
  `EntityType::Task`. `idx_tasks_external` on `(source, external_id)` (v35) backs
  the `get_task_by_external_id` upsert lookup. CRUD: `create_task`, `get_task`,
  `get_task_by_external_id`, `list_tasks`, `update_task`, `set_task_status`,
  `soft_delete_task`.
- **UI** — tasks render in a **toggleable right-side column** between the
  terminal and the file viewer, behaving exactly like the file viewer:
  **F5**/`Ctrl+W` (`Action::FocusTasks`) shows **and** focuses it (and hides it
  again), and `Ctrl+L`/`Ctrl+H` cycle in/out of it as part of the session ring
  (`SessionList → Terminal → TaskList → FileViewer`, each a cycle stop only while
  visible). Layout: `compute_layout`'s `show_tasks_panel` flag adds a 20% column
  (`PanelAreas::tasks_panel`) at width ≥ 120. Rendered by `ui/tasks_panel.rs`
  (checkbox glyphs ☐/◐/☑) with the shared `ui::focus_block`, matching the session
  list / file viewer; `InputFocus::TaskList` is the panel focus. Rows whose task
  has an **open related session** get a trailing accent `⇄`
  (`TaskPaneEntry::linked`).
- **Full-screen preview / edit toggle** (`view::render_task_workspace`): while the
  panel is focused (`InputFocus::TaskList`) the central pane shows the selected
  task's **full-screen, scrollable** read-only **details + markdown preview**
  (`ui/task_detail`: agent linkage, **related session(s)**, status, source,
  created/updated, then the description via `ui/markdown::render_markdown`);
  `PageUp`/`PageDown` scroll it (`App::task_preview_scroll`, reset on selection
  change). Entering the central pane (`Enter`/`e` → `InputFocus::TaskEditor`)
  swaps to the **full-screen editor**
  (`ui/task_editor_modal::render_task_editor_into`); `Esc` returns to the
  preview/panel. Helpers: `sync_task_editor`, `new_task_in_pane`,
  `enter_task_editor`, `refresh_task_view`, `build_task_editor`.
- **Editor fields** — a task is just **title + description + status**
  (`TaskField`); the agent action is chosen at trigger time, not here. The
  `description` is a **multi-line** `modals::TextArea` (newline +
  vertical-cursor, distinct from single-line `TextInput`): **`Enter` inserts a
  newline** and `Up`/`Down` move within the text (field nav is `Tab`);
  **`Ctrl+S` saves from any field**.
- **Keys** (focused panel): `j`/`k` select (live-preview), `PageUp`/`PageDown`
  scroll the preview, `n` new, `e`/`Enter` open the central-pane editor,
  `Space` cycle status, `r` open the **trigger-time action picker**, `o` **open
  the task's related session** (`App::open_task_related_session` — jumps to the
  spawned `<title> · #<id>` window or a Send target, else a status hint), `d`/`Ctrl+D`
  delete, `Esc` back to the session list. In the editor: field nav +
  `Enter`/`Ctrl+S` save (→ back to panel), `Esc` discard; the editor captures
  its keys before global bindings (so `e`/`d` edit text) via
  `handle_automation_pane_capture`.
- **Trigger-time action picker** (`r`) — `Modal::TaskActionPicker`
  (`App::open_task_action_picker`, rendered by `ui/task_action_picker_modal`,
  modeled on the theme picker): one **Send → <session>** per running session
  plus **Spawn new session…**. *Send* runs immediately
  (`App::send_task_to_session`); *Spawn* stashes `App::pending_task_prompt =
  (task_id, title)` and reuses the normal `open_repo_picker` →
  `do_spawn_session` flow, whose success tail delivers the title (after
  `AGENT_BOOT_DELAY_TICKS`) and advances the task. The pending prompt is
  cleared on a manual `Ctrl+N` so a cancelled task-spawn can't leak into it.
  Both paths call `App::advance_task_to_in_progress`.
- **CLI**: `thurbox-cli task` (alias `todo`) —
  `create`/`list`/`show`/`edit`/`remove`/`run`. `create`/`edit` take an
  optional `--description` (markdown; `edit --description ""` clears it) and
  the external-sync fields `--source` / `--external-id` / `--external-url`
  (used by the task-integration extensions; an empty `--external-id`/
  `--external-url` clears it, `create` defaults `source` to `local`), and
  `task_to_json` emits a `description` field. `create` with neither
  `--session` nor `--repo` is a plain local todo; `run` triggers the
  Send/Spawn action headlessly. Tasks do **not** participate in sync
  (`SharedState`) and have no run-history table (audited via `audit_log`).

## Extensions

`extensions/` holds opt-in, **agent-agnostic** add-ons that build on
`thurbox-cli` without touching the core binary. Each ships an
`extension.toml` manifest installed via `thurbox-cli extension install
<name>` (with a thin curl-able `install.sh` shim over it).

- **`extensions/flow/`** *(experimental — new and under active testing)* — a
  focus-protecting triage agent: brain-dumps become thurbox tasks, dispatchable
  ones spawn worker sessions (on `flow/<slug>` worktree branches, agents
  `flow-worker`/`flow-worker-heavy` mapped in `agents.toml` to any CLI), a
  dedicated `flow` session monitors them, and every reply ends with the single
  next thing to focus on. Dispatch is **plan-first**: `scripts/create-task.sh`
  owns the worker prompt and injects a mandatory clarify → plan → build phase (≥3
  clarifying questions, then a written plan gated on user approval, then
  implement; seeded from `--accept`) so each worker plans before it codes. A dump
  spanning several `repos.md` repos becomes one **multi-repo** task:
  `create-task.sh` forwards `--add-repo PATH@origin/<base>` (own isolated
  worktree per repo) / `--add-dir PATH` to `task create`, and the worker opens a
  **separate PR per repo it changes** (its `result` carries `pr_urls`).
  Worker↔flow coordination is **event-driven over the
  [inter-session message queue](#inter-session-messages-mailbox-queue)**: a
  worker pushes `message send --to flow --kind questions|plan|result` (waking
  flow) with **no ids** (thurbox stamps sender + task from the injected
  `THURBOX_SESSION`/`THURBOX_TASK`); flow drains its inbox (`message inbox
  --claim`), surfaces the questions/plan under "Needs you", and relays the user's
  answer with `message reply <message_id>` — routed to that message's sender, so
  flow never maps a task to a session id (`flow-snapshot.sh` name-parsing is now
  human-board only). The worker drains its own inbox on the resulting `inbox`
  wake. Flow ships **no scheduled automation** — a **manual** `tick` is the
  janitor/safety-net (drain missed wakes, reset stale tasks, dispatch). The
  behavior spec is `FLOW.md`, surfaced to whichever CLI runs it via context-file
  symlinks (`CLAUDE.md`/`AGENTS.md`/`GEMINI.md` → `FLOW.md`). See
  `extensions/flow/README.md`.
- **`extensions/forge/`** *(experimental)* — a workflow analyst that mines
  your tasks/sessions/automations (and their run history) for **recurring
  patterns** and writes ready-to-apply `thurbox-cli automation` proposals. It
  **proposes, never imposes**: a scan (driven by a weekly `forge-scan`
  automation on the `forge` session) only reads state and writes
  `proposals.jsonl` (rendered to `proposals.md`); nothing is created until you
  `apply <slug>` — and `proposals.sh apply` refuses any command not starting
  with `thurbox-cli`. Spec: `FORGE.md`.
- **`extensions/ci-shepherd/`** *(experimental)* — watches your open change
  requests (GitHub PRs / GitLab MRs / Bitbucket PRs; repos in `repos.md`) and
  dispatches a `shepherd-worker` fixer for each one with **failing CI**, a
  **changes-requested review**, or a branch that is **behind its target**
  (needs rebase — the normalized `rebase` signal from `provider.sh`, surfaced
  as the `REBASE` action flag by `scripts/classify.sh`; `dispatch-fix.sh
  --rebase` makes the worker rebase onto the base and force-push before fixing).
  When **several PRs in one repo** are all REBASE-only, `classify.sh`
  **serializes** them — only the lowest-numbered keeps the live `REBASE` flag,
  the rest become `REBASE-QUEUED (behind #n)` — so the shepherd rebases one at a
  time (each merge advances the base for the next), clearing the stack in O(n)
  rebases instead of the O(n²) of force-pushing N mutually-invalidating branches.
  A `shepherd` session monitors via a `shepherd-tick` automation; fixers are
  thurbox **tasks** (`fix #<n>: …`) that self-report with the same `===RESULT===`
  sentinel as flow. It is **forge-agnostic**: only **git** is baked in; *how* to
  talk to a repo's host is decided by the shepherd agent each tick — built-in
  **fast paths** (github `gh`/gitlab `glab`/bitbucket REST via
  `scripts/provider.sh`) plus an **agent-driven** path for any other forge
  (`provider.sh describe` hands the agent the remote + installed clients; it
  lists the repo itself and passes `--branch`/`--checkout-cmd`/`--feedback-cmd`/
  `--comment-cmd` to `dispatch-fix.sh`). Because thurbox's `--worktree` always
  runs `git worktree add -b` (which fails on an existing branch),
  `dispatch-fix.sh` adopts the request branch itself into a shepherd-owned
  worktree. It is also **session-aware**: the snapshot joins each request's head
  branch against the live `thurbox-cli session list` (`scripts/link-sessions.sh`,
  pure + bats-tested). A request whose branch already has a **non-fixer** thurbox
  session (someone working it by hand) is **not** dispatched (two worktrees would
  force-push the same branch) but is **monitored and folded into the merge
  ordering** — that live session counts as the repo's active worker, so the other
  same-repo requests queue behind it. While such a request stays actionable the
  shepherd **nudges the live session** over the message queue (`thurbox-cli
  message send`) to do the rebase/merge — once per pending ask (guarded by
  peeking its unread inbox), not every tick — so the slot actually clears.
  Spec: `SHEPHERD.md`.
- **`extensions/renovate/`** *(experimental)* — keeps local repos on up-to-date
  dependencies. A `renovate` session sweeps a `repos.md` watch list on a weekly
  `renovate-tick` automation and dispatches a `renovate-worker` per eligible
  repo; the worker runs **Renovate's `local` platform only**
  (`scripts/renovate-run.sh` hard-codes `--platform=local` — no hosted bot, no
  token, no Renovate-opened PR), tests the result, commits to a fresh
  `renovate/updates-<ts>` branch, and opens a review PR. Updaters are thurbox
  **tasks** (`update <repo> deps …`) that self-report with the same
  `===RESULT===` sentinel as flow. Unlike ci-shepherd it starts a *new* branch,
  so `scripts/dispatch-update.sh` uses thurbox's native `--worktree` (no branch
  adoption). Version strategy is per-repo (`strategy` column: `patch`/`minor`/
  `major`/`all`, layered as a `RENOVATE_CONFIG` overlay) plus a global
  `renovate-config.json`. Spec: `RENOVATE.md`.
- **`extensions/{github-issues,gitlab-issues,linear,jira}/`** *(experimental)* —
  per-provider **task-integration** extensions that sync an external issue
  tracker **bidirectionally** with the thurbox task list. **No agent/LLM** — each
  ships a `*-tick` automation (every 15 min) that is a deterministic
  `AutomationAction::Exec` running `{home}/scripts/sync.sh`; thurbox's scheduler
  runs it (TUI or headless heartbeat) and records the output in the run history.
  `sync.sh` (identical across all four bar the `SOURCE` tag) sources
  `{home}/credentials.env` (how Linear/Jira keys reach the headless run), then
  push-then-pull: `push-status.sh` (`done` closes the issue, reopening on revert;
  only `push_back=yes` rows), then per `trackers.md` row `fetch.sh "<query>"`
  (provider API → normalized JSON) `| upsert.sh --source <tag>` (dedup by
  `(source, external_id)`; only open-vs-done is authoritative, so a local
  `in_progress` is never clobbered; `upsert.sh` is byte-identical across all
  four). Watch list is a `trackers.md` seed (`| name | query | push_back |`,
  `query` per provider: `owner/repo` flags for github, project for gitlab, team
  key for linear, JQL for jira). Backends: `gh`/`glab` CLIs, `curl` GraphQL
  (linear), `curl` REST v3 (jira). The only Rust support is the tracker-neutral
  `task --source/--external-id/--external-url` flags, `get_task_by_external_id`,
  and the `Exec` automation action (ADR-20: no provider name in the binary). See
  each extension's `README.md`.

### Extension manifests + self-heal (`thurbox-cli extension`)

Extensions stay **data, not binary** (ADR-20): core thurbox knows a declarative
**manifest format**, never a specific extension. Each extension ships an
`extension.toml` (`session::ExtensionDef`, pure data in
`session/extension_def.rs`; loaded by `agent::extension_config`) with two halves:
an **install** spec (`home`, `[[agents]]` to register in agents.toml, `[[files]]`
payload, `[[symlinks]]`, `[[external_files]]`, `[[agent_patches]]`,
`[[config_merges]]`) and a **runtime** spec (`[[sessions]]` + `[[automations]]` to
ensure/self-heal). The `{home}` token expands to the resolved home dir.

Three of those reach **outside** the extension home, all reversible:
`[[external_files]]` drops a managed file into an agent's own config dir (guarded
by `requires_dir`), `[[agent_patches]]` appends args to an existing agent in
agents.toml, and `[[config_merges]]` deep-merges shipped JSON into an agent's
*shared* config file (`agent::json_merge`; uninstall prunes by the
`thurbox-cli session signal` marker, so removal survives payload schema changes).

**Built-in `hooks` extension** (`session_ops::builtin_hooks`,
`extensions/hooks/`) — unlike user extensions it ships **embedded** in the binary
and is **auto-activated by default** (`ensure_builtin_hooks_extension` at TUI
startup + headless tick), so the default agent's status hook works with zero
setup. It materializes its embedded assets locally and installs through the
ordinary machinery above. **Which delivery mechanism each built-in gets (and the
exact states each can report) is documented per agent in `docs/AGENTS.md` →
"Status hook mechanisms"** — that is the reference to update when adding an agent.
Remote sessions are provisioned by
`session_ops::remote_hooks::provision_agent_hooks_on_host`; a psmux/Windows host
is gated off (`session::psmux_hook_rewrite_supported`) and shows `Hooks:
degraded`. Opt out with `thurbox-cli extension deactivate hooks` (records a
`builtin_hooks_optout` metadata flag so self-heal won't resurrect it);
`activate`/`install hooks` clears it.

`thurbox-cli extension` (alias `ext`) — `install <name|url|dir>` / `uninstall` /
`reinstall` / `list` / `available` (alias `search`) / `update [--all] [--force]` /
`activate` / `deactivate` / `status`. A bare name resolves to the official source
**pinned to the binary's release tag**, so a fetched extension matches the binary.

**Self-heal**: `session_ops::heal_active_extensions` re-ensures every active
extension at TUI startup (before session restore) and at the top of the headless
`automation tick`. Consequence worth knowing before debugging a "zombie" session:
while an extension is active, deleting its session/automation is a **no-op** —
they are recreated. `extension deactivate` is the real off-switch, and headless
healing needs `[features] automations = true`.

**Installer resolution order, payload flags, versioning/staleness
(`installed_with`/`is_stale`), and the full self-heal contract are in ADR-21 of
`docs/ARCHITECTURE.md`.**

## Global search (`Ctrl+/`)

A **non-modal bottom strip** (`Ctrl+/`, rebindable) searches **every scope at once**:
**sessions** (name/agent/branch + live vt100 **buffer content**), **tasks**
(title + description, with a description snippet when only the description
matched), **automations** (name), and **files** (the active session's file
tree). `Enter` jumps to the selected result and focuses its pane —
switching to a session's terminal, the tasks panel, the automations pane,
or the file viewer (revealing the path). Gated by `[features]
global_search` in settings.toml; scopes whose feature is disabled
(tasks/automations/file viewer) contribute no results.

- **Live in-place highlighting**: rather than reprinting results in the strip,
  matched characters highlight **in the panels themselves** (session list, tasks,
  automations) — accent+bold+underline on matching rows, dim on the rest — via
  the shared `src/ui/highlight.rs`. The view feeds each panel renderer the query
  through `App::global_search_query()` (`Some` only while the strip is open with a
  non-empty query). The strip shows the query, per-scope match counts, the grouped
  scrollable result list (selected row `▸`/highlighted, snippets dimmed), and key
  hints (`src/ui/global_search.rs`).
- **Live preview + cancel-restore**: moving the selection
  (`App::preview_global_search_result`, from `move_global_search_selection` and on
  query change) moves the owning panel's cursor — `active_index` /
  `task_panel_index` / `automation_panel_index` — so the previewed row is visible
  while focus stays in the strip (files are *not* previewed; they open only on
  `Enter`). `global_search_preview_kind()` tells the view which panel owns the
  preview so it force-shows that row
  (`TaskPaneState`/`AutomationsPaneState::preview_selected`). `open_global_search`
  captures a `SearchSnapshot` (focus + the three indices + `show_tasks_panel`/
  `show_file_viewer`); `Esc`/`close_global_search` restores it, `Enter`/
  `activate_global_search_result` drops it (keeps the jump).
- **State** lives in `src/app/search.rs` (`GlobalSearchState`,
  `GlobalSearchResult`, `SearchTarget`/`SearchKind`); building results +
  dispatching a selection live on `App` (`build_global_search_results`,
  `session_content_match`, `activate_global_search_result`,
  `open/close_global_search`). `InputFocus::GlobalSearch` captures all
  input before the global keybinding lookup.
- **Debounce**: cheap metadata results recompute on every keystroke; the
  expensive per-session buffer-content scan runs only after the query is
  idle for ~150 ms (`Instant`-based, driven from `tick()`), capped at
  `MAX_PER_GROUP` results and `CONTENT_LINE_CAP` lines per session.
- **Keys**: type to filter; `Up`/`Down` or `Ctrl+P`/`Ctrl+N` move the
  selection (so plain `j`/`k` still type); `Enter` activates; `Esc` closes
  and restores the prior focus.
- **Layout**: `compute_layout`'s `show_global_search` carves a full-width
  `PanelAreas::global_search` strip above the footer (shrinking the content
  area like the side panels). Rendered by `src/ui/global_search.rs`.
- **Binding**: `Action::GlobalSearch` defaults to `Ctrl+/` (the near-universal
  "search" chord), bound to all three encodings terminals deliver it as
  (`Ctrl+/` under the kitty protocol; `Ctrl+7`/`Ctrl+_` from the raw 0x1F byte
  on legacy terminals). It isn't a bare `Ctrl+<letter>`, so it never defers to
  the PTY — search opens from a focused terminal too. Fully rebindable from the
  F1 editor like any other action (there is no separate hardcoded opener).
  Global search is the **only** search: the per-pane local `/` filters
  (session list, tasks panel) were removed in favour of it. The file
  viewer's `/` (in-file text search) is unrelated and stays.

## Session status (hooks-driven)

The session list shows, at a glance, which agents are blocked, working,
or done. `SessionStatus` (`src/session/mod.rs`) has six states — five driven by
**agent hooks**, not heuristics, plus `Unreachable` for a down remote host:

| State | Colour | Glyph | Meaning |
|-------|--------|-------|---------|
| `Working` | yellow | animated braille spinner (`⠋⠙⠹…`; static `◐`) | agent is actively running |
| `Blocked` | red | `◆` | agent needs input or approval |
| `Done` | blue | `●` (filled) | a turn just finished; shown until you switch away |
| `Idle` | green | `○` (hollow) | acknowledged (you moved off a Done), never active, or at rest |
| `Error` | red | `✗` | reserved for a crashed agent — **not derived yet** (no exit-code signal; exited → `Idle`) |
| `Unreachable` | muted grey | `⊘` | remote host down/offline; a **placeholder** row (no live pane) awaiting reconnect |

**Unreachable / placeholder sessions.** A persisted **remote** session whose host
is unreachable at restore (SSH down / auth failing / offline) is inserted as a
`Session::placeholder` (`src/agent/backend.rs`) so it **always appears** in the
list instead of silently vanishing, tagged `Unreachable`. A placeholder holds no
live backend pane — its reader/writer loops are never spawned, keystrokes are
dropped with a hint, and `resize`/`kill`/`detach`/`save_state` skip it (so it
never issues a blocking ssh call on the UI thread nor clobbers the persisted
row). The remote-restore loop (`App::poll_remote_restore` /
`maybe_retry_remote_restore`) readies each remote backend off-thread, retries a
down host every `REMOTE_RETRY_INTERVAL` (20 s) — or immediately on restart
(`Ctrl+R`) — and, once the host recovers, replaces the placeholder **in place**
with the adopted session (same `SessionId`, so the order signature is unchanged).

The same treatment covers **mid-session host loss**:
`App::detect_lost_remote_sessions` (per tick) spots a *live* remote session whose
control-mode connection just died, converts it in place to an `Unreachable`
placeholder and queues a reconnect (`enqueue_remote_reconnect`). The reliable
signal is `has_exited()`: with `remain-on-exit=on` a clean agent exit keeps its
pane alive (no reader EOF), so a remote reader hitting EOF means the host/SSH
connection dropped. This composes with the fail-fast SSH hardening
(`crate::shell::SSH_HARDENING_OPTS` = `BatchMode=yes` + `ConnectTimeout` +
`ServerAlive*`), which stops a broken host from prompting for a password on the
TUI's terminal or hanging the render loop.

The live session list **animates** the `Working` spinner (`ui::SPINNER_FRAMES`,
`App::spinner_frame` advanced from `tick_count`, ~8 fps, repaints forced only
while something is working). The filled `●` (Done) vs hollow `○` (Idle) pair
reads done-vs-seen at a glance. `ui::status_glyph(status, spinner)` picks the
frame; the static `icon()` is used in non-animated contexts (info panel).

- **The callback.** Agents report transitions with
  `thurbox-cli session signal --state <working|blocked|done|idle>`
  (`cli::sessions::Action::Signal`). Identity is the injected
  `THURBOX_SESSION` (falling back to a lookup by `agent_session_id` /
  `THURBOX_SESSION_ID`), so a hook passes no id. It writes the persisted
  state and the TUI picks it up via `PRAGMA data_version` — works headless.
  (`idle` = agent at rest, e.g. a boot-time hook; `done` = a turn just finished.)
  **Remote** sessions use a different callback with the same downstream:
  the materialized hook file sets a tmux pane user option instead, delivered
  over the control-mode subscription into the same hook columns (see the
  Remote-session-status bullet in the Remote SSH & WSL section).
- **Persistence.** `sessions.hook_state` / `hook_state_at` / `seen_at`
  (schema **v34**), with targeted-UPDATE accessors `set_hook_state` /
  `mark_session_seen` / `load_hook_states` (`storage/sessions.rs`).
  `upsert_session` deliberately **never** lists the hook columns, so the
  TUI's full-row write-back can't clobber a state a headless hook set. A
  fresh spawn seeds **nothing** — a never-reported session is `Idle`, and the
  agent's hooks drive it from there (so an idle, just-booted agent doesn't look
  stuck working).
- **Derivation.** `App::refresh_session_statuses` (`src/app/mod.rs`) derives
  each session's status every tick from the hook rows — exited → `Idle`; else
  the persisted state (`working`/`blocked`; `idle`/none → `Idle`). The rows are
  **cached** (`App::cached_hook_states`) and reloaded from the DB only when
  `PRAGMA data_version` moves (an external `session signal`), not on every tick;
  same-connection writes (`seen_at`, restart's `clear_hook_state`) are applied
  write-through / via `invalidate_hook_state_cache` (see `docs/PERFORMANCE.md`
  ADR-P6). `done` shows as `Done` (blue)
  **whether focused or not** — so a turn you're watching visibly completes — and
  becomes `Idle` only when you **move focus off it** (acknowledge it): the focus
  change vs. `last_active_session_id` marks the just-left `done` session `seen`
  (persists `seen_at`, one-shot). A single focused session therefore reads
  `working ↔ done`; `idle` is the at-rest/acknowledged state.
- **Stuck-`working` fallback.** Hooks can miss the turn-end edge: Claude Code
  fires **no hook on interrupt** (Esc/Ctrl+C) nor when it returns to the idle
  prompt, so an interrupted (or crashed) turn would leave `hook_state = working`
  forever. `derive_session_status` guards with an **output-quiescence fallback**
  (`WORKING_OUTPUT_STALE_MS`, 10 s): a `working` session with no terminal output
  for that long is treated as `Idle`. TUI agents animate their progress line
  (Claude's `(Xs · esc to interrupt)` ticks every second) so a genuinely-live turn
  never trips it; only `working` is time-gated. The DB row is left untouched — the
  override is purely per-tick derivation, like exited → `Idle`.
- **Per-session only.** Status renders on the session's own row (and in the
  ` Sessions ` panel border title, one dot per session). Repo-group headers
  (`ui::project_list::group_header_line`) carry **no** status — a rolled-up group
  dot would restate what every member row shows. Status only recolors — it
  **never** reorders rows (the order cache stays status-independent).
- **Colours** are tunable theme fields: `status_working` / `status_blocked`
  / `status_done` / `status_idle` / `status_error`
  (`session::theme_config`, all 15 presets + custom-theme overrides), mapped
  by `ui::status_color`.
- **Wiring the hooks** is the job of the built-in **hooks extension**
  (auto-activated; see the Extensions section) — core thurbox only knows
  the generic `session signal` command.

## OS notifications

When a session transitions to `SessionStatus::Blocked` (the agent needs
the user, reported by a hook) thurbox fires an OS desktop notification so
the user can react without watching the TUI.
The trigger is the block edge by default; an opt-in
`also_on_waiting` extends it to the `Working → Done` (finished) edge
(the field name is historical). The transition is observed once per
tick in `App::refresh_session_statuses` (the *same* place
`SessionStatus` is computed, so the rule never drifts from the icon
in the list), dedup'd per session by `min_interval_secs`, and skipped
when the target session is the one in focus (`suppress_for_active`).

- **Delivery backend** (auto-detected). `notifications::detect_backend`
  resolves the configured `[notifications] backend` (`auto` by default)
  plus host probing (`probe_host`) into a concrete `DeliveryBackend`
  (`Dbus` / `WindowsToast` / `Macos` / `None`) via the pure, table-driven
  `resolve_backend`. `auto` picks **dbus** on a normal Linux desktop
  (a session-bus `org.freedesktop.Notifications` socket is reachable),
  the **Windows toast** path on **native Windows** (`HostProbe.is_windows`)
  and under WSL when no dbus daemon answers (`/proc/version` carries the
  Microsoft marker and `powershell.exe` is on PATH — we shell out a WinRT toast
  script, `build_powershell_toast_script`, single-quote-escaped), and the
  **macOS** native banner. Delivery errors are recorded in a process-wide
  `LAST_ERROR` slot (`notifications::last_error`) and surfaced by the diagnostic
  — under WSL the dbus path used to error on connect and only `warn!`, so the
  user saw nothing.
- **Diagnostic**: `thurbox-cli notify` (`cli/notify.rs`) prints the
  detected backend, whether it can deliver, click-to-focus support, and
  the last delivery error; `--test` fires a sample notification
  *synchronously* (`notifications::send_blocking`, since the short-lived
  CLI has no dispatcher thread) so the user can confirm end-to-end.
- **Click-to-focus** (dbus + macOS `terminal-notifier` paths). The dbus action
  callback writes a session UUID to the SQLite `metadata` row keyed by
  [`PENDING_FOCUS_SESSION_ID_KEY`](src/session/mod.rs) (the single source of
  truth shared by writer and reader). The TUI's external-state poll
  (`App::poll_external_changes` → `apply_pending_focus_request`) reads + deletes
  the row atomically (`Database::take_pending_focus_session_id`, one
  `DELETE … RETURNING`) and switches `active_index` + `InputFocus::Terminal`. On
  macOS the same row is written by `terminal-notifier`'s `-execute` flag (which
  shells back into `thurbox-cli session focus <id>`), so click-to-focus works
  wherever `terminal-notifier` is installed. The Windows-toast path and macOS's
  `osascript` fallback show the banner but ignore clicks (a Windows toast can't
  call back into WSL; the `osascript`/`UNUserNotificationCenter` callbacks need a
  signed app bundle, which thurbox is not). **Terminal window-raising is
  deliberately not implemented**: thurbox runs inside an arbitrary terminal
  emulator it doesn't own and per-emulator window control is fragile (especially
  on Wayland). The session is pre-selected; the user alt-tabs back themselves.
- **TUI-only lifecycle**. The PTY parser that observes the bell only runs while
  the TUI is alive, so notifications don't fire from headless `automation tick`.
  The dispatcher thread (`crate::notifications::start`) only starts when
  `[features] notifications = true` — zero overhead when disabled.
- **Gated by `[features] notifications`** (default on); knobs in
  `[notifications]` (also_on_waiting / suppress_for_active / sound /
  min_interval_secs / backend). `backend = "off"` is a soft delivery
  switch distinct from the `[features]` flag (the dispatcher still runs
  but drops everything). Settings live in `session::settings`
  (`NotificationBackend` enum); loader in `agent::settings_config`; full
  doc in `docs/CONFIG.md`.
- **Code shape**. `src/notifications.rs` is the leaf side-effect layer (knows only
  `session` + `paths`) — one background thread reads a per-process mpsc channel
  and dispatches over the resolved backend (`notify-rust` for dbus,
  `terminal-notifier`/`osascript` for macOS, `powershell.exe` for the WSL toast).
  The body is bounded to 200 chars (`notify_state::truncate_body`) so a huge OSC
  message can't overflow the banner. Per-session bookkeeping (prior status, dedup
  timestamps) lives in `src/app/notify_state.rs` as a pure struct owned by `App`,
  constructed only when the feature is enabled. Backend selection
  (`resolve_backend`), the WSL marker check, powershell escaping, and body
  truncation are pure functions with table-driven tests.

## Code review (native, tuicr-like)

Thurbox has a **built-in, natively-rendered** code-review view (no external
binary, no nested TUI) — a tuicr-like GitHub-style continuous diff with
classified comments and a review summary — targeting the active session's
worktree (`<base>..HEAD`), which maps cleanly onto thurbox's model (every session
is a worktree on a branch forked from a base). Toggle with `Ctrl+X` (`F7`
alternate; rebindable `Action::ToggleReview`): `Ctrl+X` is in
`terminal_passthrough` (the emacs prefix key), so in a focused terminal it
reaches the agent and `F7` opens the review. Gated by `[features] code_review`.

Shape, in one pass: it owns the central pane with its own
`InputFocus::CodeReview` (it *captures* keys, unlike the shell pane's
`TerminalView`) plus a focusable changed-files list in the file-viewer column
(`InputFocus::ReviewFiles`); **unified or true paired side-by-side** layout
(`v`), horizontal scroll or a **wrap toggle** (`w`) for long lines, find-in-diff
(`/`), retargetable to Working / Branch / a single commit (`t`), multi-repo aware
(repo-qualified paths), syntax-highlighted, mouse-first, and persisted per
session. Export is agent-native: `y` copies markdown, `e` pastes the review into
the session's agent to address it.

Where the code lives: `ui::code_review` (render) + `ui::syntax`,
`app::code_review::CodeReviewState` (state), `session::review` (pure diff types +
`parse_unified_diff`, so `ui` renders without importing `git`),
`storage::review` (`review_comments` + `review_marks`, schema v38, keyed on the
write-once `sessions.base_branch`), `git::diff_against{,_on}` (local or SSH). The
diff pipeline runs on a worker with a loading state (ADR-P8).

**Full detail — every key, layout invariant, helper name, and the named v1
follow-ups — is in the Code Review section of `docs/FEATURES.md`.** Two rules to
keep in mind when touching it: **1 logical diff row = 1 selectable unit**
(wrapping expands only *visual* rows; selection, comment anchoring, and hitboxes
stay logical), and the diff types stay in `session` (architecture rule).

## Demo Video

The demo media is **generated**, not hand-recorded. A single
script drives the *real* TUI via
[VHS](https://github.com/charmbracelet/vhs) (needs `vhs` +
`ffmpeg` + `ttyd` + `tmux`) and writes GIF **and** MP4 straight
into `docs/media/`:

```bash
scripts/demo/record.sh                 # regenerate ALL demo videos
scripts/demo/record.sh theme automations   # re-record a subset
```

`record.sh` records every video pair in one pass, one VHS tape each
(`scripts/demo/<feature>.tape`): the hero demo (`thurbox-demo.*` via
`agents.tape`), one clip per feature
(`thurbox-{file-manager,info-panel,theme,session-creation,fork}.*`), and the
`automations`/`tasks`/`search` demos (`<stem>-demo.*`). No args = all; pass tape
stems for a subset (`agents` is the hero, `automations`/`tasks`/`search` map to
`<stem>-demo.*`, every other stem to `thurbox-<stem>.*`).

Every clip uses **real agent CLIs**: one session per installed CLI (`claude`,
`opencode`, `codex`, `antigravity`), launched with no prompt. `HOME` is
overridden so agents boot with fresh history/config (keyring-authenticated CLIs
stay logged in but show no account email). The tapes exercise the session list,
info panel (`Ctrl+B`), file viewer (`Ctrl+E`), native code review (`Ctrl+X`;
`F7` alternate), theme picker, session-creation flow, and the Automations pane;
the hero `agents` demo also opens the review, seeding the same
worktree-with-a-committed-diff session the dedicated `code-review` clip uses.

**The demo scenario is a realistic one, not a UI tour.** The repo is a
**vendored snapshot of thurbox's own tree** (a fixed file list copied into the
throwaway `HOME` and `git init`ed there — MIT, already on the recording machine,
so recordings stay hermetic and offline). Sessions are named after the **work**,
not the agent running it (`fix-osc52-tmux`, `add-wsl-host-tests`,
`perf-session-order-cache`, `docs-remote-hooks` — see `demo_session_name`), so
the list reads as one backlog with four branches in flight; the agent stays
visible per session in the info panel and tab title. The code-review clip's diff
is a real follow-up fix (`posix_quote` rejecting newlines, plus its test) applied
to the vendored copy only — never your working tree. The seeded tasks/automation
and the queries in `search.tape`/`tasks.tape` are keyed to that same narrative,
so **editing one means editing the others** (the tapes type literal queries —
`host` must match both a session and a task — and literal names).
The built-in **hooks extension is deactivated** for recordings: it is
auto-activated by default and makes claude open a "Hooks need review" modal on
first launch in a fresh `HOME`, hiding the agent UI and swallowing keystrokes
(no tape asserts on the status dots it wires).

It runs fully isolated from your real environment — a dev build
(`0.0.0-dev` → `dev_build` cfg) uses the `thurbox-dev` socket and XDG subdirs,
and the script points `TMUX_TMPDIR` + `XDG_{DATA,CONFIG,STATE,CACHE}_HOME` at a
throwaway temp dir. **`TMUX_TMPDIR` is essential**: the `thurbox-dev` socket
*name* is shared by every dev build, so without a private socket directory the
cleanup `kill-server` would tear down dev sessions you have running.

`.github/workflows/pages.yml` copies the mp4s into `website/assets/` at deploy
time and `README.md` embeds the gifs, so regenerating them propagates everywhere.

### The `iddqd` easter egg (website)

Typing **`iddqd`** — Doom's god-mode cheat — anywhere on the website
opens a modal playing `doom-easter-egg.mp4`: Doom running **inside a
thurbox pane**, in a session on the **pi** agent with the
[pi-doom](https://github.com/badlogic/pi-doom) extension. It fits the
landing page's existing Doom-arcade art direction (`--doom-red`, the HUD
bar, the `Doom` theme preset). Two clues, neither spelling the code: the
footer's `.footer-secret` line (also the accessible one — it names the
game, the year and the mode) and the HUD bar's `God Mode / 5 keys`
segment (that bar is `aria-hidden` decoration).

Implementation is `website/js/main.js` (sequence detection + the modal, built on
first trigger so the several-MB clip costs nothing until someone knows the code)
plus `.doom-overlay*` in `website/css/components.css`. The clip URL is resolved
from `body[data-assets]` (set by `base.njk`), because one shared `main.js` is
served to pages at different depths.

**Running Doom in thurbox** needs no thurbox change: `pi` is already a built-in
agent, so with the CLI and extension installed
(`npm i -g --ignore-scripts @earendil-works/pi-coding-agent`, then
`pi install git:github.com/badlogic/pi-doom`; pi needs **node ≥ 22.19**) typing
`/doom` in a pi session plays it in the pane — truecolour and half-block glyphs
round-trip through `vt100` + `tui-term` intact. One limit: thurbox forwards key
**presses** but not **releases** (`main.rs` requests only
`DISAMBIGUATE_ESCAPE_CODES`, `run_loop` matches `KeyEventKind::Press`) while
pi-doom opts into key-release events for held movement — so tap input (menus,
cheats) works and a held key latches. Lifting that means `REPORT_EVENT_TYPES` +
encoding kitty press/release to the pty, changing input for *every* agent;
deliberately not done.

The media is **not** a VHS tape (VHS drives a TUI through ttyd + a headless
browser; this is a *nested* TUI — thurbox rendering pi rendering Doom).
`scripts/demo/record-doom.sh` records the real binary with **asciinema** (fully
isolated `THURBOX_CONFIG_DIR`/`THURBOX_DATA_DIR`/`TMUX_TMPDIR`, so it never
touches your sessions), trims the cast with `scripts/demo/trim-cast.mjs`, and
rasterises with **agg** — no browser. agg ships **no font**, so point `FONT_DIR`
at a monospace TTF with box-drawing, block *and* braille coverage (a Nerd Font
works; braille is thurbox's spinner). The clip is Doom's self-playing **attract
demo**, sidestepping the key-release limit. agg is slow (~8 min for 20 s).

## Architecture (TEA Pattern)

The app follows **The Elm Architecture**:
`Event → Message → update(model, msg) → view(model) → Frame`

### Module Dependency Rules (enforced by tests/architecture_rules.rs)

```text
session  ← pure data types, no crate-internal references
agent    ← session (+ paths/shell utils; NEVER ui, git, app)
ui       ← session + app model/view state (+ fuzzy/paths;
           NEVER agent or git)
app      ← coordinator, imports all modules
```

Enforcement is an **allowlist**: every module under `src/` must
have a `ModuleRules` entry in `tests/architecture_rules.rs`
naming the crate modules it may reference — in *any* form (`use`,
`pub use`, brace groups, and fully-qualified `crate::…` paths) —
and a new module fails the test until its place in the
architecture is declared. `ui → app` is the TEA `view(model)`
coupling: ui renders state types owned by `app` (modal structs,
status messages) but never triggers side effects. `session_ops`
and `cli` may reach `crate::agent::…` (the narrow tmux helpers)
via fully-qualified paths only — never `use` — so the headless →
backend dependency stays visible at each call site.

### Module Responsibilities

- **`app/`** — Model (`App` struct) + Update
  (`AppMessage` enum + `handle_key/resize`) + View.
  Owns all state, coordinates side effects.
- **`agent/`** — Side-effect layer. `AgentProvider` trait
  abstracts CLI command + arg construction; `GenericProvider`
  implements it from a declarative `AgentDef` (loaded via
  `agent_config`). `Session` wraps a `SessionBackend`
  trait. `BackendRegistry` holds the backends, keyed by name.
  `TmuxBackend` runs tmux over a `TmuxTransport` (`transport.rs`):
  `Local` (`tmux -L thurbox`) for the default `local-tmux`
  backend, or `Ssh` (`ssh <dest> tmux …`) for each remote host
  in `hosts.toml` (registered as `ssh:<host>`, loaded via
  `host_config`). The control-mode protocol (`control_mode.rs`)
  is identical over either transport. Reads output into
  `Arc<Mutex<vt100::Parser>>`, writes input via mpsc channel.
  `input.rs` translates crossterm `KeyCode` → xterm ANSI bytes.
- **`session/`** — Plain data: `SessionId`, `SessionStatus`,
  `SessionInfo` (with `agent` name), `SessionConfig` (agent
  name, backend name, ids, cwd, env), `AgentDef`/`AgentRegistry`,
  `HostDef`/`HostRegistry` (remote SSH hosts).
  Mostly Display/Default impls plus the agent-arg
  substitution logic.
- **`ui/`** — Pure rendering functions. `layout.rs` computes
  panel areas (responsive: <80 = terminal only, >=80 = 2-panel,
  >=120 = optional 3-panel). Widgets: `project_list` (session
  list with repo/branch display; `compute_session_order` is the
  single comparator that orders sessions by manual order
  (`display_order`, never by status) and groups them by repo
  under headers — shared with `App`'s `Ctrl+J/K` navigation so
  the two never drift; `move_in_order` is the pure reorder step
  behind `Shift+J`/`Shift+K`),
  `terminal_view`, `info_panel`,
  `status_bar`, `repo_picker_modal` (repo selection with
  worktree toggle). `selection.rs` handles mouse-drag text
  selection, `links.rs` detects plain-text URLs in the rendered rows
  for Ctrl+Click — while a **rich-text link** (OSC 8) prints only its
  label, so its target is captured from the escape at parse time
  (`agent::osc8` → `session::hyperlink`) and wins over a plain-text
  match at the same cell (`App::url_at_click`). Because ratatui knows
  nothing about hyperlinks, each painted frame is followed by
  `App::paint_terminal_hyperlinks`, which re-prints the visible runs
  wrapped in OSC 8 (same glyphs, same styles, read back out of the drawn
  frame) so the **outer** terminal can open the user's own browser — the
  only route to one when thurbox runs on a remote host. The click always toasts
  its outcome, and on a host with no browser (headless / SSH — no
  `DISPLAY`, no `BROWSER`) it **copies** the URL instead of spawning an
  opener that goes nowhere, riding the same OSC 52 leg as `Ctrl+C` so
  the URL reaches the user's own clipboard. See the Clickable URLs
  section of `docs/FEATURES.md`.
  Mouse clicks, buttons, and both on-border affordances all route through one
  per-frame **click-target registry** (`App::click_targets`, mirroring
  `scrollbar_hits`): renderers return `ui::RowHitbox`/`ui::ButtonHit`es,
  `App::view` records them as `ClickAction`s, and `handle_mouse_click` hit-tests
  them — rows select/confirm, panes focus, footer pills replay their `Action`,
  modal buttons/fields replay the matching key so a click always matches the
  keyboard path. That registry also carries the **session-list collapse chevron**
  (`◀`/`▶` + F9 at the central pane's top-left border) and the **central-pane tab
  strip** (`Agent · Review · F7 · Shell · F8`, packed to its right), both recorded
  *before* the pane's whole-rect focus fallback so an on-border click wins. Tabs
  *select* a view (`App::select_central_tab`), distinct from the keyboard
  `Ctrl+T`/`Ctrl+X` *toggles*, and prefer the **F-key** hint since a focused
  terminal passes `Ctrl+<letter>` through to the agent. All gated by `[features]
  mouse` — disabled, mouse capture is never enabled and the terminal keeps native
  mouse behavior. **Every hitbox kind, the pill/hover styling, the feature-gated
  footer packing, and the tab/chevron rendering helpers are in the Mouse
  Navigation section of `docs/FEATURES.md`.**
  `agent_picker_modal` drives the new-session flow.
- **`cli/`** — `thurbox-cli` subcommand dispatch (headless
  session ops + scheduling + editor command).

### Event Loop (main.rs)

```text
tokio::main → load AgentRegistry (agents.toml)
  → init BackendRegistry (local-tmux)
  → open SQLite DB → init terminal → spawn/restore sessions → loop {
    draw frame → poll crossterm events (10ms)
    → convert to AppMessage → app.update() → app.tick()
} → app.shutdown() (detach sessions) → restore terminal
```

- Logging goes to `~/.local/share/thurbox/thurbox.log`
  (file-based, since stdout is owned by the TUI)
- Panic hook restores terminal before printing

## Pre-commit Hooks

17 hooks run automatically via `prek` (Rust-based pre-commit
framework). Install with `prek install`. Stages:

- **commit-msg**: conventional commit validation (`cog verify`)
- **pre-commit**: fmt, clippy, check, nextest, architecture,
  deny, doc, bats, shellcheck, rumdl, prettier, htmlhint,
  stylelint, eslint
- **pre-push**: commit history check (`cog check`)

Shell scripts are linted with **shellcheck** (config in
`.shellcheckrc`); install it from your package manager (it is not a
cargo crate — `scripts/install-dev-tools.sh` prints a reminder).

## Key Technical Details

- MSRV: 1.75, Edition 2021
- Async runtime: tokio (multi-threaded)
- Session backend: `TmuxBackend` over a `TmuxTransport`
  (local `tmux -L thurbox`, or `ssh <dest> tmux …` for
  `ssh:<host>` backends from `hosts.toml`)
- Output reader runs in `tokio::task::spawn_blocking`
  (blocking I/O), writer in `tokio::spawn` (async)
- Terminal state parsed by `vt100::Parser`,
  rendered by `tui_term::PseudoTerminal`
- Sessions persist across restarts (tmux keeps them alive)
- Session state in SQLite:
  `~/.local/share/thurbox/thurbox.db` (XDG_DATA_HOME respected);
  agent definitions in `~/.config/thurbox/agents.toml`;
  remote SSH hosts in `~/.config/thurbox/hosts.toml`
- Requires tmux >= 3.2

## Keybindings (Vim-Inspired)

Global keys use `Ctrl` + semantic Vim conventions:

| Key | Action | Mnemonic |
|-----|--------|----------|
| `Ctrl+Q` | Quit (detach sessions) | **Q**uit |
| `Ctrl+N` | New session (opens repo picker) | **N**ew |
| `Ctrl+C` | Copy selection, else status message; SIGINT in a focused terminal | **C**opy |
| `Ctrl+V` | Paste from clipboard | Paste |
| `Ctrl+P` | Automations (list/new/edit/toggle/run/delete) | **P**rogram |
| `Ctrl+W` / `F5` | Toggle tasks panel (todo list) | Work items |
| `Ctrl+/` | Global search (sessions/tasks/automations/files) | **/** = search |
| `Ctrl+T` / `F8` | Toggle shell pane | **T**erminal |
| `Ctrl+X` / `F7` | Toggle native code-review view | Review |
| `Ctrl+H` | Focus previous pane (cycle backward) | Vim: **h** = left |
| `Ctrl+J` | Select next session | Vim: **j** = down |
| `Ctrl+K` | Select previous session | Vim: **k** = up |
| `Ctrl+L` | Focus next pane (cycle forward) | Vim: **l** = right |
| `Ctrl+D` | Delete session | Vim: **d** = delete |
| `Ctrl+O` | Open active session's working dirs in editor | **O**pen |
| `Ctrl+R` | Restart active session | **R**estart |
| `Ctrl+F` | Fork active session | **F**ork |
| `Ctrl+S` | Sync worktrees with their base branch | **S**ync |
| `Ctrl+Z` | Undo session delete | **Z** = undo |
| `Ctrl+U` | Restore deleted sessions | **U**ndelete |
| `Ctrl+Y` / `F4` | Pick TUI theme | Color **Y**oke |
| `Ctrl+,` / `F6` | Settings panel (edit settings.toml) | **,** = preferences |
| `Ctrl+B` / `F2` | Toggle info panel (visible at width >= 120) | Info **b**ox |
| `Ctrl+E` / `F3` | Toggle file viewer | **E**xplore files |
| `F9` | Toggle session-list pane (hide for full-width terminal) | Sessions list |
| `F12` | Toggle perf HUD (live counters + frame/tick timing) | Diagnostics |
| `F1` / `Ctrl+G` | Keybindings help + interactive editor | Universal |

List contexts use plain `j`/`k`/`Enter` for navigation.
In the focused session list, `Shift+J`/`Shift+K` move the selected
session down/up (manual reordering; whole groups move past a group
edge). Terminal forwards all non-Ctrl keys to the PTY.
`Shift+arrows/PageUp/PageDown` for scrollback; `Alt+PageUp/PageDown`
also page (fallback for terminals that claim `Shift+Page` for their
own scrollback, e.g. Terminal.app/iTerm2).

These defaults can be overridden two ways, both writing the same
`~/.config/thurbox/keybindings.json` (an `Action` name → one or more
chord strings, e.g. `{ "QuitApp": ["ctrl+a"] }`):

- **Interactively** from the F1 panel, a live editor rather than a read-only
  overlay: `j`/`k` select an action, `Enter`/`r` starts capture (the **next
  physical keypress** — including chords like `ctrl+q` — becomes that action's sole
  binding), `d` resets the selected action to its default, `Shift+D` resets **all**
  (via `App::reset_all_keybindings`, which deletes the override file so defaults
  stay authoritative). A captured chord already bound elsewhere is reassigned
  (stolen) and a toast reports the move. Each change persists immediately via
  `KeyBindings::{rebind,reset}` + `storage::keybindings::save_keybindings_json` and
  takes effect on the next keystroke — no restart. The editor lives in
  `Modal::Help(HelpModal { selected, capturing })`; capture input routes through
  `App::handle_help_key` inside `handle_priority_key` (**before** the global
  `keybindings.lookup`, so capturing `ctrl+q` rebinds instead of quitting).
  Selection indices match `Action::rebindable_in_order()` — the flattened
  `keybindings::help_sections()`, shared with `render_help_overlay`.
- **By hand-editing** the JSON file (e.g. via `$EDITOR`); reloaded live
  (mtime poll — see `docs/CONFIG.md`).

**Context-scoped bindings.** Each `Action` has a `KeyContext` (`Global`,
`SessionList`, `Automations`, `Tasks`, `FileViewer`, `Terminal`). Global
actions are active everywhere; scoped actions fire only while their pane is
focused, so a single-letter key like `j` can drive the file viewer, session
list, automations pane, and tasks pane independently (and the terminal still
forwards it to the PTY). `handle_key` resolves
keys via `KeyBindings::lookup_in(App::focus_key_context(), …)`, dispatched
through `dispatch_action`. Conflict detection (`KeyBindings::rebind`) only
steals a chord between actions whose scopes overlap (`contexts_overlap`) —
global-vs-anything, or same scope. Capital/shift-letter chords are
canonicalized via `KeyChord::normalized` (e.g. `Shift+N` → `{shift, n}`) so
capture, lookup, and the JSON round-trip agree regardless of how the
terminal encodes them. **Copy/Paste** are global rebindable actions handled
early in `handle_priority_key` (so Paste reaches modal text inputs).

**Terminal PTY passthrough.** thurbox's global chords share the
`Ctrl+<letter>` namespace with readline / shell line editing (`Ctrl+A` =
start-of-line, `Ctrl+E` = end-of-line, `Ctrl+W` = delete-word, `Ctrl+U` =
kill-line, `Ctrl+R` = reverse-search, `Ctrl+D` = EOF, …). So when a session
**terminal is focused**, the actions flagged by `Action::terminal_passthrough`
(`ToggleInfoPanel`/`DeleteSession`/`ToggleFileViewer`/
`ForkSession`/`OpenInEditor`/`OpenAutomations`/`RestartSession`/`StartSync`/
`OpenRestoreSessions`/`FocusTasks`/`ToggleReview`) **defer to the agent CLI**
instead of running the thurbox command — `handle_key` skips `dispatch_action`
and falls through to `handle_terminal_key`, which forwards the bytes to the PTY
(so e.g. `Ctrl+X` reaches emacs's prefix key in a focused terminal). The
thurbox command stays reachable from the **session list** (and via its `F`-key
alternate where one exists — `F2`/`F3`/`F5`/`F7`). The deferral is gated on the
bound chord still being a bare `Ctrl+<letter>` (`is_ctrl_letter_chord`), so
rebinding a passthrough action to a non-conflicting key keeps it working in the
terminal. Navigation / app-control chords (`Ctrl+H/J/K/L` focus + session nav,
`Ctrl+Q` quit, `Ctrl+N` new, …) are **not** deferred — they are the keyboard
escape route out of the terminal, so they must keep working there even though a
few collide with readline.

**Readline editing in modal text fields.** thurbox's own text inputs (session /
branch name, repo-picker path & search, automation editor, task title /
description) accept the standard emacs/readline chords: `Ctrl+A`/`Ctrl+E` line
start/end, `Ctrl+B`/`Ctrl+F` by char, `Ctrl+H`/`Ctrl+D` delete before/under the
cursor, `Ctrl+W` delete word, `Ctrl+U`/`Ctrl+K` kill to line start/end. Dispatch
lives in one place — `modals::apply_ctrl_line_edit` over the `LineEdit` trait
(implemented by both `TextInput` and `TextArea`) — and **every** `Ctrl`+letter is
consumed (mapped or swallowed) so a bare control letter never leaks into the
field. A `Ctrl` chord with a non-letter key (arrows, Home/End) falls through to
normal cursor handling.

A few stateful keys stay literal (the F1 panel lists them under
**Fixed (not rebindable)**): modal selectors (j/k/Enter/Esc), the
automation **run-history** sub-mode, the file-viewer **search sub-mode**, and
the terminal's catch-all PTY forwarding. The automations and tasks panes
themselves are **rebindable** scoped contexts (`KeyContext::Automations` /
`KeyContext::Tasks`), mirroring the session list.

### macOS

Ctrl chords work unchanged in macOS terminals (raw mode bypasses
flow control; `Ctrl+Y`'s DSUSP quirk is why the `F4` alternate
exists). On top of that:

- **Cmd chords.** `main.rs` enables the kitty keyboard protocol
  (`PushKeyboardEnhancementFlags(DISAMBIGUATE_ESCAPE_CODES)`, gated
  on `supports_keyboard_enhancement()`, popped on shutdown and in
  the panic hook) so the Command key can be bound like any modifier:
  `cmd+j` in `keybindings.json` (`super`/`command`/`win` are parse
  aliases; `cmd` is the canonical display form) or captured live in
  the F1 editor. Delivered by iTerm2 3.5+, kitty, WezTerm, Ghostty;
  **not** Terminal.app (no kitty protocol — everything else still
  works there). The emulator's own Cmd shortcuts (`Cmd+Q/W/N/T/C/V`,
  `Cmd+K` clears, `Cmd+H` hides, `Cmd+digits` switch tabs) are
  consumed at the GUI level and can never reach the TUI — only bind
  what the terminal leaves free.
- **macOS-only default alternates** — appended after the Ctrl
  primaries via `Action::default_chords_for(macos)` (the
  `cfg!(target_os = "macos")` decision lives in `default_chords()`;
  Linux defaults are byte-identical): `Cmd+J`/`Cmd+Shift+J` =
  next/previous session, `Cmd+L`/`Cmd+Shift+L` = focus
  next/previous pane.
- **Unbound Cmd chords are swallowed**, never forwarded to the PTY
  (`agent::input::key_to_bytes` returns `None` for SUPER).
- **F-keys** (`F1`–`F5` alternates) need `Fn` on Mac laptops unless
  "Use F1, F2, etc. keys as standard function keys" is enabled;
  `Cmd+V` pastes via the terminal's native paste (bracketed paste),
  no binding needed.

## Themes

The TUI ships with **thirty-six palettes** — twenty-eight dark (**Default**,
**Catppuccin Mocha**, **Tokyo Night**, **Gruvbox Dark**, **Doom**, **Nord**, …)
and eight light (**Catppuccin Latte**, **Tokyo Night Day**, **Solarized Light**,
…); the full enumeration lives in `docs/FEATURES.md` and
`session::theme_config`. Users can add **custom themes** in
`~/.config/thurbox/themes.toml` (a built-in `base` plus per-colour
overrides — see `docs/CONFIG.md`); they appear in the picker after the built-ins
and persist by name exactly like a preset
(`session::theme_config::CustomThemeDef` → `ThemeEntry`, loaded by
`agent::themes_config::load_or_seed_with_warnings`, published via
`ui::theme::set_custom_themes`). Pick one with `Ctrl+Y` (or `F4`, which avoids
terminals that intercept Ctrl+Y as DSUSP); the choice persists in SQLite under
`metadata.active_theme`, and other thurbox processes pick up theme changes within
one tick via `PRAGMA data_version` polling.

Because the list is long, the picker (`ui::theme_picker_modal`) can **filter**,
but behind `/` (mirroring the file viewer's and code review's find) so its keys
stay consistent with the other selectors: `j`/`k` (+ `↑`/`↓`, `PageUp`/`PageDown`
by the rendered list height via `App::theme_picker_page`, `g`/`G`, `Home`/`End`,
`Ctrl+N`/`Ctrl+P`) navigate, and only after `/` do letters append to a query —
matched against each theme's display name *and* stable id, with a live
`matched/total` count. `ThemePickerModal::filter` is `Option<TextInput>` (`None` =
navigation mode); `Esc` closes the filter first (full list restored, cursor kept)
then the picker. Entries group under `Dark`/`Light` headers drawn *inside* their
entry's row, so selection, hitboxes, and the scrollbar stay in entry space (a
header is never selectable) and a header disappears with its filtered-out
section. `ThemePickerModal::index` indexes the **match** list, not the entry list,
and every consumer resolves it through `matches` — refining a query keeps the
cursor on the same theme when it survives, so narrowing can't apply a palette
other than the previewed one. See `docs/FEATURES.md`.

## Settings panel

`Ctrl+,` (rebindable `Action::OpenSettings`; `F6` alternate) opens a
centered **Settings modal** (`Modal::Settings(SettingsModal)`) that views
and edits **all of settings.toml** — the `[features]` toggles, 4
`[notifications]` knobs, and 4 scalars — without hand-editing the file.
Persistence stays in `settings.toml`: `agent::settings_config::save_settings`
writes it back through a `toml_edit::DocumentMut` so the seed's
documentation comments survive (the first save adds real uncommented keys
below the commented examples). The modal edits a working-copy `draft` and
applies **only on `Ctrl+S`** (`Esc` discards — there is no live preview).

Feature flags that gate UI panels (`tasks`, `file_viewer`, `info_panel`,
`global_search`, `shell_pane`, `code_review`, `soft_delete`) are read from
`App.features` every frame, so `submit_settings_panel` copies the draft's flags
into `self.features` (via `App::apply_live_settings`) and they take effect
immediately. `apply_live_settings` also runs `enforce_feature_visibility`, which
tears down any surface a now-disabled live flag left open (the `show_*` panel
toggles, an open shell view or code review) and moves focus off it — otherwise the
panel would keep rendering with its tab/footer affordance gone; each branch only
forces the *hidden* state, so re-enabling never re-opens anything. Everything else
is read once at startup from the write-once `settings::global()` `OnceLock`: those
rows are marked `⟳`, and a save that changes one toasts "some changes apply after
restart".

`settings.toml` is **live-reloaded** like `agents.toml`/`keybindings.json`:
`App::poll_config_reload` watches its mtime and, on any external change (a
hand-edit, the panel in another instance), re-applies the live feature flags
via the same `apply_live_settings` and toasts (noting a restart when
`Settings::restart_only_differs` vs the global). The panel's own write calls
`mark_settings_saved` so the poll doesn't re-toast it.

`SettingsField` (in `app/modals.rs`) owns the field order, labels, short
scannable keywords and descriptions (both avoid naming key chords, since those
are rebindable; each row renders the bold `keyword` then the dimmed
`description`, via the single `meta()` table so the parallel lookups never
drift), scalar-vs-bool/step logic (`adjust` with per-field clamping), and the
per-row live/restart marker (`restart_required`); the canonical
live/restart comparison is `Settings::restart_only_differs`
(`session/settings.rs`), reused by both the panel toast and the reload path.
The renderer is `ui::settings_modal::render_settings_modal` (modeled on
`automation_editor_modal`, with blank separators between the Features /
Notifications / Scalars sections, an aligned value column, and
scroll-windowing for short terminals).

## OpenSpec (spec-driven changes)

Non-trivial changes can be planned through
[OpenSpec](https://github.com/Fission-AI/OpenSpec) before any code is written.
It is **tooling, not a gate** — small fixes still go straight to a commit.

Six skills drive the loop: **propose** (draft `proposal.md` → `specs/` deltas →
`design.md` → `tasks.md`) → **apply** (implement the tasks) → **archive** (fold
the deltas into `openspec/specs/`, move the change to
`openspec/changes/archive/<date>-<name>/`). `explore` (think first), `update`
(revise artifacts), and `sync` (merge deltas without archiving) fill the gaps.

They are installed for the three agents this repo already configures, each in
that agent's own layout (the skills are identical; only the command alias and
its spelling differ):

| Agent | Skills | Commands | Invoked as |
|-------|--------|----------|------------|
| claude | `.claude/skills/openspec-*/` | `.claude/commands/opsx/` | `/opsx:propose` |
| pi | `.pi/skills/openspec-*/` | `.pi/prompts/opsx-*.md` | `/opsx-propose` |
| opencode | `.opencode/skills/openspec-*/` | `.opencode/commands/opsx-*.md` | `/opsx-propose` |

- **Layout**: `openspec/config.yaml` (schema `spec-driven`, plus optional
  project `context` and per-artifact `rules`), `openspec/specs/` (current
  requirements), `openspec/changes/<name>/` (in-flight work). One shared
  `openspec/` tree serves all three agents.
- **CLI**: `npm install -g @fission-ai/openspec`. The skills are restricted to
  `Bash(openspec:*)`, so the binary must be on PATH — an `npx` fallback will
  not work. `openspec update` refreshes the skills after a CLI upgrade;
  regenerate them with `openspec init --tools claude,pi,opencode` (re-running
  `init` is additive — it leaves already-configured agents intact).
- Skill and command files are generated — edit `openspec/config.yaml` to shape
  the workflow rather than hand-patching them, since `update` overwrites them.
- `.opencode` joins `.claude`/`.pi`/`openspec` in `rumdl`'s exclude list
  (`.rumdl.toml`). The generated skills and change artifacts would otherwise
  block commits on prose we don't author; excluding the whole agent dir (rather
  than the generated files inside it) is how `.claude`/`.pi` were already
  treated.

## Design Documentation

For rationale behind decisions, see `docs/`:

- `docs/CONSTITUTION.md` — Core principles and non-negotiable rules
- `docs/ARCHITECTURE.md` — Architectural decisions with rationale
- `docs/FEATURES.md` — Feature-level design choices
- `docs/CONFIG.md` — Every config file/env var/DB setting in one place
- `docs/AGENTS.md` — Each built-in agent's exact config + behavior, and
  the checklist for adding a new built-in
- `docs/PERFORMANCE.md` — Render/tick performance: demand-driven redraw,
  perf counters, the session-order cache, and how to measure

**Rule**: If a code change invalidates or extends a documented
decision, update the relevant doc in the same PR.
