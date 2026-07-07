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
frame only when the UI is dirty (`App::needs_redraw`) or a 250 ms forced-redraw
floor (`FORCE_REDRAW_INTERVAL`) elapsed — not on every ~10 ms iteration like
before. `App::update` marks dirty on any input; `App::detect_output_redraw`
marks dirty on new agent output (lock-free, via each session's `last_output_at`
atomic); `refresh_session_statuses` marks dirty on a status change; the floor
covers time-driven UI (clock/metrics/cursor blink). Idle paints drop ~100 fps →
~4 fps with input/output latency unchanged. The session-list ordering is cached
keyed by a content signature (`App::session_order_signature`), rebuilt only when
its grouping/nesting inputs change. The per-tick session-status read is likewise
cached (`App::cached_hook_states`), reloaded only when `PRAGMA data_version`
moves — so an idle `tick` no longer rescans the `sessions` table (ADR-P6). Launch
with `THURBOX_PERF_LOG=1` to log a `startup` line (phase breakdown +
`first_frame_ms`, plus `restore_discover`/`restore_adopt`/`adopt_split`) to
`thurbox.log`. Full rationale + intentionally-skipped optimizations:
`docs/PERFORMANCE.md`.

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
archive** (`cargo nextest archive --target x86_64-pc-windows-gnu`), ships it plus
a tarball of the working tree (uncommitted changes included — needed so insta
snapshots / fixtures resolve), and runs it with `cargo-nextest.exe
--archive-file … --workspace-remap …`. CI runs the same suite natively in the
`windows` job (`.github/workflows/ci.yml`, `windows-latest` + `cargo nextest
run`); the VM is the local/offline mirror. Tests that genuinely assume Unix are
`#[cfg(unix)]`-gated; the rest are written to source the home dir from the
platform var (`USERPROFILE`/`HOME`) and use `tempfile`/`std::env::temp_dir()`
rather than hardcoded `/tmp`.

All state lives under `target/windows-test/` (gitignored): the throwaway SSH
keypair, the cached psmux + nextest zips, the generated `/oem` payload, the
cross-built test archive, and the VM disk image. Needs `/dev/kvm` +
`/dev/net/tun`. **Gotcha:** dockur forwards only `3389` to a Windows guest by
default, so the script sets `USER_PORTS=22` to push the published SSH port
through qemu's host-forward into the VM.

### Lab (real-host) test environment

`scripts/dev/e2e/real-host.sh <host> <verb>` (or `just lab <host> <verb>`) drives
the same checks against **any real machine over SSH** — a `~/.ssh/config`
alias or `user@address`. Linux/Windows is auto-detected. Because lab machines
may also run *regular* thurbox sessions, the
e2e test is fully scoped: a private `-L thurbox-lab-test` socket + session, all
remote state under one `thurbox-lab-test` directory (repo + `worktrees_dir`),
and an isolated local `THURBOX_CONFIG_DIR`/`THURBOX_DATA_DIR` — the release
socket (`thurbox`), the dev socket (`thurbox-dev`), and real config/DB are
never touched. Verbs: `check` (readiness probe), `hosts` (print the
`hosts.toml` block), `test` (headless ssh-backend e2e, mirrors
`e2e/linux-container.sh test`), `tui` (wire the host into the persistent `lab`
sandbox profile + launch for manual testing), `ssh` (interactive shell or a
one-off command), `clean`; Windows-only: `deploy`
(cross-build + install to `C:\Tools\thurbox`), `run` (the deployed TUI over
`ssh -t`), `test-suite` (nextest archive, mirrors `e2e/windows-vm.sh`),
`wsl-setup` / `wsl-check` (provision + verify a WSL distro as a thurbox
target), `native-test [agent]` (headless e2e of the **deployed** binaries
natively on the host: `thurbox-cli.exe` creates a local psmux session —
agent argv + `THURBOX_*` env asserted intact — and `thurbox.exe` boots
inside a scoped psmux pane and must show/adopt it; isolated via the
`THURBOX_SOCKET` env override, since psmux has no `TMUX_TMPDIR`-style
socket-dir isolation). Local state: `target/lab-test/` (gitignored).

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
verification, extract, post-install hints. They download from the same release:
`install.sh` pulls the `.tar.gz` for `x86_64-unknown-linux-musl` /
`aarch64-apple-darwin` (Linux x86_64 + Apple-silicon macOS — the only
platforms it installs onto; it errors cleanly on any other);
`install.ps1` pulls the
**`thurbox-<ver>-x86_64-pc-windows-msvc.zip`** (the Windows artifact built by
`cd.yml`) and extracts it with the built-in `Expand-Archive` (no tar needed).
ARM64 Windows installs the x86_64 build (runs under x64 emulation).

**`install.sh` (POSIX `sh`) specifics:**

- Colorized output (auto-disabled when stderr is not a TTY, `NO_COLOR` is set,
  or `TERM=dumb`); platforms Linux/macOS × x86_64/aarch64
- No external deps beyond standard tools (curl/wget, tar, sha256sum/shasum)
- Env vars: `VERSION=v0.1.0`, `INSTALL_DIR=/path` (default `~/.local/bin`)
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
2. **Release Decision**:
   - **If** commits include `feat`, `fix`, or `perf` → creates release
   - **If** only docs/chore/ci commits → no release (workflow exits)
3. **Automated Release** (if needed):
   - Determines semantic version (feat→minor, fix/perf→patch)
   - Creates lightweight git tag: `v{version}` (e.g., v0.1.0)
   - Pushes tag to origin
   - Builds binaries for 4 platforms (3 Unix `.tar.gz` + 1 Windows `.zip`;
     version passed via environment variable)
   - Generates changelog from commits
   - Publishes GitHub Release with binaries and release notes

### Version Management

- **Cargo.toml version**: Always `0.0.0-dev` (static development marker)
- **Real version**: Determined by release workflow (v0.1.0, v0.2.0, etc.)
- **Build-time injection**: `build.rs` uses `THURBOX_RELEASE_VERSION` environment
  variable (set by workflow) to inject version into binary
- **Development builds**: Show `0.0.0-dev` (when `THURBOX_RELEASE_VERSION` not set)
- **Release builds**: Show actual version (e.g., `0.1.0`) via env variable from workflow

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
- **winget** (`publish-winget`): bumps `PackageVersion`/`InstallerUrl`/
  `InstallerSha256`/`ReleaseNotesUrl` in the three manifests under
  `packaging/winget/manifests/` (via `packaging/winget/bump-manifests.py`,
  reading the release `checksums.txt`), then `wingetcreate submit`s the set as a
  PR to `microsoft/winget-pkgs`. Runs on `windows-latest`; needs the
  `WINGET_TOKEN` secret (a `public_repo` PAT owning a fork of
  `microsoft/winget-pkgs`). New versions go through winget-pkgs PR
  validation + review. The release zip is a `zip` installer with
  `NestedInstallerType = portable` (PATH aliases `thurbox`/`thurbox-cli`, no
  MSI). Install: `winget install Thurbeen.thurbox`. Windows x86_64 only (added
  as a moderation-independent alternative to the Chocolatey channel).

See `packaging/README.md` for the full packaging overview.

### Commit Types and Versioning

- **feat**: Minor version bump (0.x.0)
- **fix, perf**: Patch version bump (0.0.x)
- **docs, chore, ci, style, test**: No release (appear in next version)
- **BREAKING CHANGE**: Major version bump (x.0.0) - use cautiously for 0.x

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

The set of launchable coding agents is declared **as data** in
`~/.config/thurbox/agents.toml`, seeded with built-ins
(`claude`, `codex`, `antigravity`, `opencode`, `aider`, `copilot`, `vibe`) on first run.
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
Agents that omit `resume_args` simply start fresh on restart (the
live tmux process is what carries state across TUI restarts). Add
your own `[[agents]]` entry to support any CLI — no recompile.

**Session id pinning vs. `resume_latest`.** thurbox generates the
`agent_session_id` (a UUID) and only `claude` accepts it at creation
(`--session-id {id}`), so only claude can resume/fork by that exact id.
The other built-ins (`codex`, `opencode`, `antigravity`, `aider`, `copilot`)
can't pin or report their id, so they set `resume_latest = true` with
**id-less** resume/fork flags (no `{id}` token): the agent resolves "the last
session in *this* directory" itself (`codex resume --last`, `opencode
--continue`, `agy --continue`, `aider --restore-chat-history`, `copilot
--continue`).
This works because restart reuses the session's cwd and a single-repo
fork reuses the parent's cwd. `resume_latest` only changes *when* the
resume group fires (see `session_ops::resume_trigger_for`): for these
agents restart always triggers resume; for claude it still defers to an
on-disk transcript check. Caveats: agents without `fork_args`
(`antigravity`, `aider`, `copilot` — none of these CLIs fork) start fresh on
`Ctrl+F`; and a
**multi-repo** fork of a cwd-scoped agent lands in a fresh symlink
workspace, so `--last`/`--continue` finds no parent session (multi-repo
*restart* still resumes, since it keeps the same workspace dir).

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

### Multi-repo sessions (symlink workspace)

A session can span several repositories (the repo picker allows
multiple; headless callers pass `--add-repo`/`--add-dir`, below). Because agent
CLIs differ wildly in how — or whether —
they accept extra directories, thurbox does **not** pass per-agent
`--add-dir`-style flags. Instead, when a session has more than one
member directory it is launched in a per-session **symlink
workspace**: `~/.local/share/thurbox/workspaces/<agent_session_id>/`
holds one symlink per repo (worktree checkout or plain dir), and the
agent process is started there (`cwd` = the workspace). Every agent
then sees each repo as a subdirectory — fully agent-neutral, no
`agents.toml` changes.

`SessionInfo.cwd` keeps the **primary** repo (for display / editor /
git context); the workspace is a spawn-time process-cwd detail,
derived idempotently on every launch from the persisted members and
never stored. `workspace::ensure_workspace` / `remove_workspace`
(`src/workspace.rs`) build and tear it down; the member set is the
single `App::session_member_dirs` list that also feeds the rendered
repo names, and `App::resolve_process_cwd` picks workspace-vs-primary.
Single-repo sessions are unchanged (`cwd` = the repo directly).

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

| Field | Req | Default | Purpose |
|-------|-----|---------|---------|
| `name` | yes | — | unique id; registers backend `ssh:<name>` / `wsl:<name>` |
| `kind` | no | `ssh` | transport: `ssh` (remote machine) or `wsl` (local distro) |
| `destination` | ssh | — | ssh target, resolved via `~/.ssh/config` |
| `distro` | no | `name` | WSL distro name (`kind = "wsl"` only) |
| `ssh_opts` | no | `[]` | extra `ssh` flags (one token per element; no `~` expansion) |
| `socket` | no | `thurbox` | host `tmux -L` socket name |
| `session` | no | `thurbox` | host tmux session name |
| `worktrees_dir` | no | `$HOME/.local/share/thurbox/worktrees` | abs worktrees dir on the host/distro |
| `multiplexer` | no | `tmux` | host multiplexer binary; `psmux` for a Windows SSH host |

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
**psmux divergences** (all verified against psmux 3.3.6, each branched on
`TmuxTransport::uses_psmux()`):

- **`send-keys -H`** is not implemented (given `-H 62` it injects the literal
  text "62", so typing `b`, Enter, Backspace, … were all broken on Windows).
  `send_keys_commands` gives psmux an equivalent encoding built from the
  primitives it *does* support (`send-keys -l` literal runs +
  `Enter`/`Tab`/`Escape`/`BSpace`/`C-<letter>` key-names), reconstructing the
  same byte stream the pane's PTY receives; tmux (incl. a WSL distro's tmux)
  keeps the byte-exact `-H` hex path. Literal runs go out as `-l -N 1 "…"`
  (double-quoted, `\"`/`\\` escaped): psmux's send-coalescing pass re-quotes
  literals with a POSIX `'\''` escape its own parser can't read back, so any
  `'` typed arrived in the pane as `\` (`it's` → `it\s`) — the `-N` flag makes
  psmux's coalescing decoder bail, and its direct handler reads the
  double-quote framing correctly (`flush_psmux_literal` / `psmux_quote`).
- **`new-window` trailing tokens are not joined** — tmux joins them into one
  shell command; psmux keeps only the *first* token and silently drops the
  rest, so the agent launched with **no args**. And **`new-window -e` is
  ignored** (on the argv path too) — env vars never reached the window's
  process (no `THURBOX_SESSION` identity). `TmuxBackend::psmux_window_powershell`
  therefore folds env + command into **one token** of PowerShell
  (`Set-Item Env:K 'v'; & 'claude' '--session-id' …` — psmux runs the token
  via `powershell -NoLogo -Command`, whose Win32 command line strips unescaped
  double quotes, hence PowerShell single-quoting throughout; backslash is
  literal everywhere in psmux's parser, so `C:\` paths survive). Control-mode
  spawns (`psmux_window_command`) frame it in double quotes — psmux's
  tokenizer concatenates adjacent `'…'` segments but passes `'` through `"…"`
  tokens — and the headless local `spawn_window` passes it as a single argv
  arg. The local socket honors the `THURBOX_SOCKET` env override
  (`local_socket()`) so test/sandbox tooling can scope an instance on Windows,
  where every `-L <name>` resolves machine-wide (no `TMUX_TMPDIR`).

Each host registers a backend named
`ssh:<name>` / `wsl:<name>` (`TmuxBackend::from_host`, registered lazily in
`main.rs` from `host_config::load_all_with_warnings`: discovery/down hosts must
not block startup, so `check_available`/`ensure_ready` are deferred to first use
via `App::backend_for`).

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
  found"). `session_ops::spawn::adapt_agent_args_for_remote` (shared by
  headless spawn and the TUI) rewrites them per host: on a POSIX remote the
  home-anchored path is **translated to the remote home**, the file copied
  there, and the arg substituted; on a psmux host / non-POSIX config root /
  failed copy the **flag+path pair is stripped** so the agent launches clean.
  The local-path env hints
  (`THURBOX_METRICS_DIR`/`THURBOX_CONFIG_DIR`/`THURBOX_DATA_DIR`) are likewise
  skipped for remote spawns (`inject_thurbox_env`); only the opaque identity
  vars travel.
- **Remote session status** (hooks-driven, like local): `thurbox-cli session
  signal` can't work from a host (no CLI there; it would write the host's own
  DB), so the materialized hook file's commands are **rewritten**
  (`builtin_hooks::rewrite_hook_signals_for_remote`) to set a tmux **pane user
  option** instead — `tmux set-option -p @thurbox_state <s>` needs no socket,
  pane id, or identity inside a pane. The local TUI's persistent control-mode
  connection subscribes once per connection (`refresh-client -B
  'thurbox-status:%*:#{@thurbox_state}'`, armed in `ControlMode::start` so
  reconnects re-arm; tmux ≥ 3.2 = the existing floor) and receives
  `%subscription-changed` pushes (≤1/s), queued on the connection and drained
  each tick by `App::drain_remote_hook_events` into the same `set_hook_state`
  columns local signals use — so Done→seen acknowledgment, OS notifications,
  rollups, and the stuck-`working` fallback are shared. Events are matched by
  **backend name + pane id** (pane ids collide across hosts), allow-listed
  (remote-controlled text), and deduped against the cache (a reconnect
  re-report must not resurrect an acknowledged `done`). Carve-outs: psmux
  remotes (no subscriptions; hooks stripped) and non-claude agents (their hook
  configs aren't materialized remotely) stay Idle-only.
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
send/inbox/prune — the inter-session mailbox queue; see below), `editor`, `config`
(validate/show — strict-parses every config file / prints the
effective resolved config; see `docs/CONFIG.md`), `extension`
(alias `ext`: install/uninstall/reinstall/list/available/update/activate/
deactivate/status — manage opt-in extensions; see below), `version`
(prints the running version; `--check` queries GitHub's latest release —
gated on `[features] version_check`, off by default), `update`
(downloads, verifies, and replaces the installed binaries with the latest
release — `--force` bypasses the up-to-date/dev-build guards; gated on
`[features] auto_update`, off by default; the TUI also runs this silently on
startup when the flag is on), `notify`
(diagnose OS desktop notifications: prints the detected delivery backend
and last error; `--test` fires a sample — see OS notifications below), `perf`
(print the perf snapshot a running TUI publishes while `THURBOX_PERF_LOG`
or its perf HUD is active — see `docs/PERFORMANCE.md`).
Output is
**human-readable by default** and switches to JSON automatically when stdout is
piped (so `… | jq` keeps working); force a format with `--json` (compact),
`--pretty` (indented JSON), or `--text` (human even when piped).

`session delete <uuid>` **soft-deletes** by default — only the DB
row is marked deleted (the TUI tears down the tmux window/worktree
on its next sync), and `session restore` revives it. Pass `--force`
(`session_ops::delete_session_headless`) to also kill the tmux
window, remove worktrees + the symlink workspace, and disable
`send` automations targeting the session — for headless cleanup
when no TUI is running. Teardown is best-effort (failures land in
the JSON report); the row is always soft-deleted last. A `--force`
delete also stamps the `sessions.force_deleted` column (schema v37):
the row still appears in the restore list **tagged `force-deleted`** and
is restorable **best-effort** — force-delete removes the worktree
*directory* but not the git branch, so restore reattaches each surviving
branch's committed work (`App::recreate_worktrees`); only uncommitted/
untracked changes are gone. Because that recovery is lossy, the headless
`session restore` **refuses a force-deleted row unless `--best-effort`**
is passed (its JSON then carries `best_effort: true`); a plain soft delete
restores with no flag. `restore_session` clears both `deleted_at` and
`force_deleted`.

The **TUI** `Ctrl+D` soft-deletes by default too (with a `Ctrl+Z` undo
window). The `[features] soft_delete` flag (settings.toml, default
`true`) governs only this TUI path: set it `false` and `Ctrl+D` becomes
a hard delete — the same `delete_session_headless(.., force=true)`
teardown — since there is no `Ctrl+Z` for it. That hard delete is
**conditional**: a confirmation modal (`Modal::ConfirmDelete`, rendered
by `ui::confirm_delete_modal`) appears **only when the session has work
at risk** — uncommitted/untracked files or unmerged commits, or a state
that can't be verified (remote host / git error → confirm to be safe;
`App::assess_delete_risk` + `modals::DeleteRisk::from_stats` over
`git::worktree_stats`). The modal itemizes what would be lost; a
known-clean session is deleted with no prompt. A force-deleted session
is then tagged in the restore list (above); restoring one via `Ctrl+U`
(`Enter`) first asks for confirmation (`Modal::ConfirmRestore`, rendered
by `ui::confirm_restore_modal`) since the recovery is best-effort
(committed branch state only), then runs the normal restore path. The
flag never changes `thurbox-cli session delete`, which stays soft unless
`--force`.

### Parent sessions (lead/worker)

Sessions carry an optional **`parent_session_id`** so orchestration
scripts can model lead → worker relationships. `session create
--parent <uuid>` sets it (the parent must be an existing active
session — validated before any side effects); `session list`/`get`
emit it in the JSON (`null` for top-level sessions) and `session
list --parent <uuid>` filters to direct children. The link is
**purely informational**: deleting a parent never cascades to
children (orphans simply render as top-level), and the parent is
only validated at creation. In the TUI, **`Ctrl+F` fork** records
the source session as the fork's parent; the session list nests
children under their parent **within the same repo group** (muted
`└` tree prefix; a child whose parent renders in another group
keeps its own position with a `↳` mark instead), and the info panel
(F2) shows a `Parent:` row. The nesting lives in
`ui::project_list::compute_session_order` (`SessionOrder::depths`),
so `Ctrl+J`/`Ctrl+K` navigation follows the tree automatically.
Storage: nullable `sessions.parent_session_id` column (schema v30;
v29 is reserved by an in-flight branch).

### Manual session ordering

The session list is **manually orderable**: `Shift+J`/`Shift+K`
(while the session list is focused; rebindable
`SessionListMoveDown`/`SessionListMoveUp` actions) move the selected
session one row down/up. Manual order **wins** — status changes only
recolor the dot, never move a row. A move swaps two adjacent
*blocks* (a row plus its nested children, so a parent drags its
subtree): root rows swap within their repo group, the **whole
group** swaps past a group edge, and nested children move among
their siblings only (`ui::project_list::move_in_order`, pure;
`App::move_active_session` applies it). On every move all sessions
are densely renumbered `0..n` and persisted, so the order survives
restarts and syncs across instances via the existing
`data_version` polling. Storage: nullable `sessions.display_order`
column (schema v31); `None` = never moved, renders after ordered
sessions in creation order (new sessions append to their group).
**`Shift+S`** (rebindable `SessionListSortAlphabetically`) sorts
sessions alphabetically by name **within each repo group** in one
shot: group order is preserved (still by lowest `display_order`),
and parent/child nesting is preserved (children sort among their
siblings). It reuses the same dense-renumber-and-persist path, so
the alphabetised order survives restarts just like a manual move
(pure helper: `ui::project_list::sort_alphabetically_within_groups`;
`App::sort_sessions_alphabetically` applies it).

### Inter-session messages (mailbox queue)

A general, agent-neutral **message queue** lets one session hand another a
**structured payload** without scraping its rendered terminal — the channel
extensions use for agent↔agent coordination (flow's clarify→plan→build relay is
the first consumer). A message is addressed **to** a session and carries a
free-form `kind` tag (any short string — `questions`/`plan`/`result`/… are
conventions, not an enum), a `body`, and optional provenance (`from_session_id`,
`from_task_id`).

- **Data**: `session::SessionMessage` (pure data, `session/message.rs`;
  `validate_kind_body` bounds `kind`≤32 B / `body`≤64 KiB). **Storage**:
  `session_messages` table (schema **v32**, plain-TEXT uuids, no FK — mirrors
  `tasks.target_session`), with a partial unread index + a `created_at` index.
  CRUD in `storage/messages.rs`.
- **Exactly-once delivery**: `Database::claim_messages` is a single
  `UPDATE … WHERE read_at IS NULL … RETURNING` — SQLite serializes writers, so
  the TUI, a cron tick, and a worker's wake nudge can drain concurrently without
  double-processing or dropping a message. `list_messages` peeks without
  consuming.
- **Bounded growth**: `enqueue_message` enforces a per-recipient unread cap
  (`MAX_UNREAD_PER_RECIPIENT`, backpressure not silent loss) + the body/kind
  limits; `prune_messages` / `prune_old_messages` (read messages older than
  `DEFAULT_RETENTION_DAYS`) run at DB open and on every `automation tick`,
  mirroring audit-log pruning. The mailbox is **not** audited (high-churn).
- **Identity (the registry key, self-knowable).** A session's thurbox
  `SessionId` is **stable for life** — `respawn_stale_session` reuses the
  original id on re-adoption (no soft-delete + new-row churn), so a cached id or
  queued message addressed to a session never goes stale. At spawn thurbox
  injects `THURBOX_SESSION` (= the `SessionId`, threaded via
  `SessionConfig.session_id` so it's known *before* launch and reused on
  respawn) and, for task-spawned sessions, `THURBOX_TASK` (= the task id). These
  are distinct from the pre-existing `THURBOX_SESSION_ID` (= `agent_session_id`,
  read by the metrics statusline). A `thurbox-cli` call running *inside* a
  session thus proves its own identity without scraping panes or names.
- **CLI** (`thurbox-cli message`, alias `msg`) — identity-aware:
  - `send --to <uuid|name> --kind <k> [--task <id>] [--from <uuid|name>] --body
    <text> [--no-wake]` enqueues and, unless `--no-wake`, types a short `inbox`
    token into the recipient's pane (`agent::tmux::send_prompt_now`) to nudge a
    drain. **Provenance + task tag default to the caller's injected identity**
    (`THURBOX_SESSION`/`THURBOX_TASK`) so an agent passes **no ids**; `--from`/
    `--task` override.
  - `reply <message_id> --body <text> [--kind k] [--from …] [--no-wake]` —
    enqueues back to the *original message's sender* (looked up via
    `get_message`) and wakes them, carrying the original `from_task_id`. The
    replier handles only the opaque message id — never a peer's session id. This
    is how flow relays the user's answer without name-scraping.
  - `inbox [--for <uuid|name>] [--claim] [--all] [--limit N]` reads it (`--claim`
    = atomic drain); **`--for` defaults to the calling session** so an agent
    reads its own mail with no id.
  - `prune [--older-than-days N] [--read-only]`.
  - `cli::messages` resolves a session by UUID **or** name (`resolve_uuid_or_name`
    → `Database::get_session_by_name`); a `send`/`reply` with a wake also arms the
    automation heartbeat (`cli::automations::arm_heartbeat`) so a missed wake is
    still drained headless. `PRAGMA data_version` already surfaces writes to the
    TUI — no sync/`SharedState` change.

An automation's `AutomationAction` is one of: **Send** (paste a prompt into a
running session), **Spawn** (start a fresh session and prompt it), or **Exec**
(run a shell command headlessly — `sh -c`, or `cmd /C` on Windows — with no
agent/session; its exit status + tail-truncated output land in the run history).
`Exec` is the deterministic-scheduled-job action (the task-integration sync
extensions use it). The shared runner is `session_ops::run_exec_command` (called
by both the headless `automation tick` and the TUI `App::fire_automation`); the
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

In the TUI, automations also get a dedicated **Automations pane**
beneath the session list (left column). It is always present
(showing `none` when empty) — unless disabled via `[features]
automations = false` in settings.toml, which hides the pane (the
session list takes the whole column and `j`/`k` wrap within it),
blocks `Ctrl+P`, stops the TUI firing schedules, and skips arming the
heartbeat (the CLI surface stays fully functional) — and is treated
as **part of the session pane**: it forms one continuous, **circular** vertical list with the
session list. `j` past the last session drops focus into the pane and
`k` at the top automation hands focus back to the last session; the
ends wrap too — `j` past the last automation loops to the **top** of
the session list, and `k` above the first session loops to the
**last** automation. It is **not** a separate stop in the
`Ctrl+H`/`Ctrl+L` cycle (which
treats it like the session list). Once focused, `j`/`k` select,
`Space`/`r`/`d` toggle/run/delete the selected automation, and `n`
creates one.

The pane mirrors the session list, with the **central pane** as its
terminal-equivalent: while the pane is focused the central pane
shows a **single editor** for the selected automation (a live
preview — there is no separate read-only "info" screen). Pressing
`Enter`/`Ctrl+L` (or `e`) focuses that editor to change fields,
exactly as `Enter`/`Ctrl+L` on a session focuses its terminal;
`Ctrl+H`/`Esc` returns to the list, `Enter` saves, `Esc` discards,
`Ctrl+E` toggles enabled. The scoped automation's run history
(`db::list_automation_runs`, cached in `App::cached_automation_runs`)
renders beneath the editor and is itself focusable
([`InputFocus::AutomationRunHistory`], one more `Ctrl+L` past the
editor): `j`/`k` select a run (`App::automation_run_index`), `r`
triggers a fresh run, and `Enter` opens the session that run touched
(`App::open_run_related_session` parses the session id out of the
run's `detail` and switches to its terminal when still open).
`Ctrl+L`/`Ctrl+H` cycle **within the current
context's ring** (`App::focus_ring`) — the automation ring
`Automations → editor → run history` wraps back to `Automations`
(never to a session; landing on the list discards edits like `Esc`),
the session ring is `SessionList → Terminal` (+ file viewer). Crossing
contexts is via `j`/`k`, not the cycle. Because the in-pane
editor/history would otherwise lose chords like `Ctrl+E` to global
keybindings, `handle_key` captures input for those two focuses
**before** the global lookup, letting only the focus-cycle/quit chords
pass through. Implemented via
the persistent `App::automation_editor` state (kept in sync by
`App::sync_automation_editor`) and
`ui::automation_editor_modal::render_automation_editor_into` +
`ui::automation_detail::render_run_history`. The
`Ctrl+P` list path opens the same editor as a centered overlay
(`Modal::AutomationEditor`); both share
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

- **Data** (`session/task.rs`): `Task` (`id`, `title`,
  `description: Option<String>` (free-form markdown notes, `None` when blank),
  `status: TaskStatus` {`Todo`/`InProgress`/`Done`},
  `action: Option<AutomationAction>`, plus `source`/`external_id`/
  `external_url` for external-tracker sync — `source = "local"` for native
  todos, or a tracker tag (`github`/`gitlab`/`linear`/`jira`) for items
  imported by the per-provider task-integration extensions (below). The
  `(source, external_id)` pair is the natural dedup key.
- **Storage** (`storage/tasks.rs`, schema v25): `tasks` table mirroring
  the automation action columns (`action_kind` nullable) plus a nullable
  `description` column (added in the v26 migration), soft-delete via
  `deleted_at`, audited under `EntityType::Task`. The
  `idx_tasks_external` index on `(source, external_id)` (v35) backs the
  `get_task_by_external_id` upsert lookup. CRUD: `create_task`,
  `get_task`, `get_task_by_external_id`, `list_tasks`, `update_task`,
  `set_task_status`,
  `soft_delete_task`.
- **UI** — tasks render in a **toggleable right-side column** that sits
  between the terminal and the file viewer, behaving exactly like the file
  viewer: **F5**/`Ctrl+W` (`Action::FocusTasks`) shows **and** focuses it
  (and hides it again), and `Ctrl+L`/`Ctrl+H` cycle in/out of it as part of
  the session ring (`SessionList → Terminal → TaskList → FileViewer`, each a
  cycle stop only while visible). Layout: `compute_layout`'s
  `show_tasks_panel` flag adds a 20% column (`PanelAreas::tasks_panel`)
  between `terminal` and `file_viewer` at width ≥ 120. Rendered by
  `ui/tasks_panel.rs` (checkbox glyphs ☐/◐/☑) with the shared
  `ui::focus_block` for the highlighted title + accent border, matching the
  session list / file viewer. `InputFocus::TaskList` is the panel focus. Rows
  whose task has an **open related session** get a trailing accent `⇄` marker
  (`TaskPaneEntry::linked`).
- **Full-screen preview / edit toggle** — the central pane is a clean toggle
  (`view::render_task_workspace`): while the tasks panel is focused
  (`InputFocus::TaskList`) it shows the selected task's **full-screen,
  scrollable** read-only **details + markdown preview** (`ui/task_detail`:
  agent linkage, **related session(s)**, status, source, created/updated, then
  the markdown-rendered description via `ui/markdown::render_markdown`);
  `PageUp`/`PageDown` scroll it
  (`App::task_preview_scroll`, reset on selection change). Entering the central
  pane (`Enter`/`e` → `InputFocus::TaskEditor`) swaps to the **full-screen
  editor** (`ui/task_editor_modal::render_task_editor_into`); `Esc` returns to
  the preview/panel. Helpers: `sync_task_editor`, `new_task_in_pane`,
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

- **`extensions/flow/`** *(experimental — new and under active
  testing)* — a focus-protecting triage agent: brain-dumps
  become thurbox tasks, dispatchable ones spawn worker sessions (on
  `flow/<slug>` worktree branches, agents `flow-worker` /
  `flow-worker-heavy` mapped in `agents.toml` to any CLI), a dedicated
  `flow` session monitors them, and every reply ends with the single next
  thing to focus on. Dispatch is **plan-first**: `scripts/create-task.sh`
  owns the worker prompt and injects a mandatory clarify → plan → build
  phase (≥3 clarifying questions, then a written plan gated on user
  approval, then implement; seeded from `--accept`) so each worker plans
  before it codes and stays in scope. A dump spanning several `repos.md`
  repos becomes one **multi-repo** task: `create-task.sh` forwards
  `--add-repo PATH@origin/<base>` (own isolated `flow/<slug>` worktree per
  repo) / `--add-dir PATH` (attached as-is) to `task create`, and the worker
  opens a **separate PR per repo it changes** (its `result` carries
  `pr_urls`). Worker↔flow coordination is
  **event-driven over the [inter-session message queue](#inter-session-messages-mailbox-queue)**:
  a worker pushes `message send --to flow --kind questions|plan|result`
  (which wakes flow) — passing **no ids** (thurbox auto-stamps the sender + task
  from the injected `THURBOX_SESSION`/`THURBOX_TASK`); flow drains its inbox
  (`message inbox --claim`, `--for` defaults to itself), surfaces the
  questions/plan under "Needs you", and relays the user's answer/approval back
  with `message reply <message_id>` — thurbox routes it to that message's sender,
  so flow never maps a task to a session id (the old `flow-snapshot.sh`
  name-parsing is now human-board only). The worker drains its own inbox on the
  resulting `inbox` wake. Flow ships **no scheduled automation** — it is purely
  event-driven; a **manual** `tick` is the janitor/safety-net (drain missed
  wakes, reset stale tasks, dispatch) you type at the flow session. The
  behavior spec
  is `FLOW.md`, surfaced to whichever CLI runs it via context-file
  symlinks (`CLAUDE.md`/`AGENTS.md`/`GEMINI.md` → `FLOW.md`). Install with
  `thurbox-cli extension install flow` (its `install.sh` is a thin shim
  over that). See `extensions/flow/README.md`.
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
  --rebase` makes the worker rebase onto the base and force-push before
  fixing). When **several PRs in one repo** are all REBASE-only,
  `classify.sh` **serializes** them — only the lowest-numbered keeps the live
  `REBASE` flag, the rest become `REBASE-QUEUED (behind #n)` and are held — so
  the shepherd rebases one at a time (each merge advances the base for the next)
  instead of force-pushing N branches that immediately re-invalidate each other,
  clearing the stack in O(n) rebases instead of O(n²). A `shepherd` session
  monitors via a
  `shepherd-tick` automation; fixers are thurbox **tasks** (`fix #<n>: …`) that
  self-report with the same `===RESULT===` sentinel as flow. It is
  **forge-agnostic**: the only thing baked in is **git**; *how* to talk to a
  repo's host is decided by the shepherd agent each tick — built-in **fast
  paths** (github `gh`/gitlab `glab`/bitbucket REST via `scripts/provider.sh`)
  plus an **agent-driven** path for any other git forge (`provider.sh describe`
  hands the agent the remote + installed clients; it lists the repo itself and
  passes `--branch`/`--checkout-cmd`/`--feedback-cmd`/`--comment-cmd` to
  `dispatch-fix.sh`). Because thurbox's `--worktree` always runs `git worktree
  add -b` (which fails on an existing branch), `dispatch-fix.sh` adopts the
  request branch itself (git-universal) into a shepherd-owned worktree. It is
  also **session-aware**: the snapshot joins each request's head branch against
  the live `thurbox-cli session list` (`scripts/link-sessions.sh`, pure +
  bats-tested). A request whose branch already has a **non-fixer** thurbox
  session (the user/another agent working it by hand) is **not** dispatched (two
  worktrees would force-push the same branch) but is **monitored and folded into
  the merge ordering** — the live session counts as that repo's active worker,
  so the other same-repo requests queue behind it rather than the shepherd
  standing the request down. When that request is still actionable the shepherd
  **proactively nudges the live session** over the inter-session message queue
  (`thurbox-cli message send`) to do the required rebase/merge — once per pending
  ask (guarded by peeking the session's unread inbox), not every tick — so the
  slot the others wait behind actually clears. Spec: `SHEPHERD.md`.
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
  push-then-pull: `push-status.sh` (push thurbox status back — `done` closes the
  issue, reopening on revert; only `push_back=yes` rows), then per `trackers.md`
  row `fetch.sh "<query>"` (provider API → normalized JSON) `| upsert.sh --source
  <tag>` (dedup by `(source, external_id)`; status rule treats only open-vs-done
  as authoritative so a local `in_progress` is never clobbered; `upsert.sh` is
  byte-identical across all four). Watch list is a `trackers.md` seed
  (`| name | query | push_back |`, `query` interpreted per provider:
  `owner/repo` flags for github, project for gitlab, team key for linear, JQL for
  jira). Backends: `gh`/`glab` CLIs (github/gitlab), `curl` GraphQL (linear),
  `curl` REST v3 (jira). The only Rust support is the generic, tracker-neutral
  `task --source/--external-id/--external-url` flags, `get_task_by_external_id`,
  and the `Exec` automation action (ADR-20: no provider name in the binary). See
  each extension's `README.md`.

### Extension manifests + self-heal (`thurbox-cli extension`)

Extensions stay **data, not binary** (ADR-20): core thurbox knows a
declarative **manifest format**, never a specific extension. Each
extension ships an `extension.toml` (`session::ExtensionDef`, pure data in
`session/extension_def.rs`; loaded by `agent::extension_config`). It has
two halves: an **install** spec (`home`, `[[agents]]` to register in
agents.toml, `[[files]]` payload, `[[symlinks]]`, `[[external_files]]`,
`[[agent_patches]]`, `[[config_merges]]`) and a **runtime** spec
(`[[sessions]]` + `[[automations]]` to ensure/self-heal). The `{home}`
token is substituted with the resolved home dir.

Three install-spec capabilities exist for reaching **outside** the extension
home (added for the built-in hooks extension): `[[external_files]]` places
a file into an agent's own config dir (absolute / `~` / `{home}` path,
guarded by `requires_dir` so it's skipped when that agent isn't installed);
`[[agent_patches]]` appends args to an **existing** agent in
agents.toml (`apply_agent_patches` via `toml_edit`, reversible — uninstall
removes exactly the injected subsequence); and `[[config_merges]]`
**reversibly deep-merges** shipped JSON into an agent's own *shared* config
file (`{path, source, requires_dir}`) — for agents whose hooks live in a
file that would be clobbered by `[[external_files]]` (antigravity's
`settings.json`). The merge (`agent::json_merge`) recurses objects, unions
arrays by deep-equality, and leaves a user's conflicting value untouched;
uninstall **prunes by marker** (every shipped hook command contains
`thurbox-cli session signal`), so removal stays correct even after the
payload's schema changes across an update — no orphans. Writes are skipped
when unchanged (it re-runs every startup + heartbeat tick). All three are
honoured by `install_extension`/`uninstall_extension`.

**Built-in `hooks` extension** (`session_ops::builtin_hooks`,
`extensions/hooks/`). Unlike user extensions it ships **embedded** in the
binary and is **auto-activated by default** (`ensure_builtin_hooks_extension`
at TUI startup + headless tick) so the default agent's status hook is
pre-configured with zero setup. It materializes its embedded assets to a
local dir and installs through the ordinary machinery: an `[[agent_patches]]`
adds `--settings {home}/claude.json` to `claude` (claude merges it, never
clobbering user settings), aider gets `--notifications-command` (blocked-only),
a `[[config_merges]]` deep-merges `codex`'s claude-shaped hooks into
`~/.codex/hooks.json` (idle/working/done, no blocked; **experimental**, replaced
the old `-c notify=…` done-only override — a reversible write into `hooks.json`,
never `config.toml`), an `[[external_files]]` drops an opencode plugin into
`~/.config/opencode/plugin/` (idle/working/blocked/done) and a managed
`~/.vibe/hooks.toml` for Mistral `vibe` (working/blocked/done; **experimental**,
refused if a user file already exists), an `[[external_files]]` drops a managed
`~/.copilot/hooks/thurbox-status.json` for GitHub Copilot CLI (`copilot`)
(idle/working/blocked/done; **experimental**, copilot's own hook schema —
`notification` matched to `permission_prompt` drives blocked; ships both
`bash`+`powershell` commands), and a `[[config_merges]]` deep-merges
hook entries into `antigravity`'s shared `~/.gemini/settings.json`
(idle/working/blocked/done; `agy` adopted claude's hook schema, verified
against agy 1.0.9 — `PreToolUse` drives working, `Notification` blocked, see
`extensions/hooks/README.md`).
Opt out with `thurbox-cli extension deactivate hooks` (records a
`builtin_hooks_optout` metadata flag so self-heal won't resurrect it);
`activate`/`install hooks` clears it. See the Session-status section.

`thurbox-cli extension install <name|url|dir> [--home <dir>] [--force]`
(`session_ops::install_extension`) is the one-command installer: it
resolves the source (`agent::extension_config::resolve_source` — a bare
name → the official source `official_base()/<name>` over curl/wget,
**pinned to the binary's release tag** (`main` for dev builds) so a
fetched extension matches the binary; a path → a local dir), fetches + lays
down the payload files (with `executable` / `if_absent` / `substitute`
flags; paths are validated against traversal — no absolute/`..`), creates
the symlinks, registers the agents (`ensure_agents_registered` appends to
agents.toml, preserving existing entries), writes the home-resolved
manifest to the discovery dir, and activates. A `substitute` file the user
edited (its managed marker removed) is not clobbered on reinstall unless
`--force`. A **bare-name** install that can't fetch its manifest (a typo or an
unknown extension) is turned into a discovery error
(`agent::extension_config::unknown_extension_help`): it names the known official
extensions (`OFFICIAL_EXTENSIONS`), offers a Levenshtein "did you mean?"
suggestion, and points at `extension available`. `uninstall <name> [--purge]`
(`session_ops::uninstall_extension`) reverses install: tear down session +
automation, remove the extension's agents (`remove_agents_from_toml`,
text-edit to preserve comments), delete the manifest, and with `--purge`
delete the home dir. `reinstall <name> [--purge]`
(`session_ops::reinstall_extension`) is the clean-slate hammer — a full
uninstall followed by a fresh `install --force` from the recorded source
(rewriting even user-edited seed/`substitute` files; `--purge` also resets the
home dir), heavier than `update --force` which only refreshes payload files in
place. Flow's `install.sh` is a thin shim over `install`.

`thurbox-cli extension` (alias `ext`) — `install` / `uninstall <name>
[--purge]` / `reinstall <name> [--purge]` / `list` / `available [<query>]`
(alias `search`) / `update [<name>] [--all] [--force]` (no name ⇒ all) /
`activate <name>` / `deactivate <name> [--force] [--purge]` / `status [<name>]`
— wraps `session_ops::extensions`: `ensure_extension` idempotently (re)creates
any missing declared resource (reusing `spawn_session_headless` +
`db.create_automation`, matching by name so existing ones are reused);
`activate_extension` also records the name in the SQLite `metadata`
`active_extensions` JSON set; `deactivate_extension` tears the resources
down and clears the set. The CLI layer arms the tmux automation heartbeat
on activate so a `Send` automation actually fires headlessly. `available`
lists the official extensions (`OFFICIAL_EXTENSIONS`) for discovery — offline,
with an `installed` flag and ready-to-run `install_command` per entry. Every
mutating subcommand's JSON carries a human-readable `summary` line (and
`list`/`status` surface each extension's `description`).

**Versioning + update.** A manifest declares its own `version` and a
`min_thurbox_version` (soft compat gate — install/activate/heal *warn*,
never block, if the binary is older). The installer stamps two provenance
fields into the discovery-dir copy: `installed_with` (the thurbox version
that installed it) and `source` (the resolved install target). After a
thurbox upgrade the on-disk copy is older than the binary, so
`ExtensionDef::is_stale` flags it (`extension list`/`status`, and a
self-heal nudge). With `[features] auto_update` on (the same opt-in flag that
self-updates the binary), the self-heal pass — `heal_one_extension`, run on TUI
startup **and** the headless `automation tick` — goes a step past the nudge and
**refreshes the stale extension in place** (calls `update_extension`); the
`is_stale` gate is local/network-free, so a refresh fetches at most once per
extension per binary version. `update_extension` re-runs `install_extension` from
the recorded `source` — a bare name re-resolves against the *new* binary's
release tag, so the matching extension version is pulled — preserving
user-edited files unless `--force`; `update_all_extensions` does every
installed one. Version helpers (`compare_versions`, `is_dev_version`,
`is_stale`, `compat_warning`) are pure functions in
`session::extension_def`; dev builds (`0.0.0-dev`) skip staleness/compat
since their version doesn't order against tags. No version-snapshot store:
rollback = pin a tagged install URL or downgrade the binary + `update`.

**Self-heal**: `session_ops::heal_active_extensions` re-ensures every
active extension and is called at **TUI startup** (`main.rs`, before
session restore so healed sessions are adopted normally) and at the top of
the headless **`automation tick`** (`cli/automations.rs`, so healing works
with the TUI closed via the heartbeat keeper). Consequence: while an
extension is active, deleting its session/automation is a no-op — they're
recreated (a startup status toast says so). `extension deactivate` is the
real off-switch. Headless healing requires `[features] automations = true`
(the heartbeat); with it off, healing happens only at TUI startup. The
flow installer now delegates its bootstrap to `extension activate flow`
(with an inline fallback for older thurbox).

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

- **Live in-place highlighting**: instead of reprinting results in the
  strip, matched characters highlight **in the panels themselves** (session
  list, tasks, automations) — accent+bold+underline on matching rows, dim
  on the rest — via the shared `src/ui/highlight.rs` helper. The view feeds
  each panel renderer the global query through `App::global_search_query()`
  (`Some` only while the strip is open with a non-empty query). The strip
  shows a query line, per-scope match counts, the grouped scrollable result
  list (selected row marked `▸`/highlighted, content snippets dimmed), and
  key hints (rendered by `src/ui/global_search.rs`).
- **Live preview + cancel-restore**: moving the selection
  (`App::preview_global_search_result`, called from `move_global_search_selection`
  and on query change) moves the owning panel's cursor — `active_index` /
  `task_panel_index` / `automation_panel_index` — so the previewed row is
  visible while focus stays in the strip (files are *not* previewed; they
  open only on `Enter`). `global_search_preview_kind()` tells the view which
  panel owns the preview so it force-shows that row's selection
  (`TaskPaneState`/`AutomationsPaneState::preview_selected`). `open_global_search`
  captures a `SearchSnapshot` (focus + the three indices + `show_tasks_panel`/
  `show_file_viewer`); `Esc`/`close_global_search` restores it, while `Enter`/
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

The same treatment covers **mid-session host loss**: `App::detect_lost_remote_sessions`
(per tick) spots a *live* remote session whose control-mode connection just died
and converts it in place to an `Unreachable` placeholder + queues it for
reconnect (`enqueue_remote_reconnect`). The reliable signal is `has_exited()`:
because tmux runs with `remain-on-exit=on`, a clean agent exit keeps its pane
alive (no reader EOF), so a remote session's reader hitting EOF means the host/SSH
connection dropped, not a normal exit. So a running session whose host dies flips
to `Unreachable` and auto-reconnects instead of silently going `Idle`.
This composes with the fail-fast SSH hardening (`crate::shell::SSH_HARDENING_OPTS`
= `BatchMode=yes` + `ConnectTimeout` + `ServerAlive*`), which stops a broken host
from prompting for a password on the TUI's terminal or hanging the render loop.

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
- **Stuck-`working` fallback.** Hooks are the *primary* signal, but they can
  miss the turn-end edge: Claude Code fires **no hook on interrupt** (Esc/Ctrl+C)
  and none when it returns to the idle prompt, so an interrupted (or crashed)
  turn would leave `hook_state = working` and spin forever. `derive_session_status`
  guards against this with an **output-quiescence fallback** (`WORKING_OUTPUT_STALE_MS`,
  10 s): a `working` session that has produced no terminal output for that long is
  treated as `Idle`. TUI agents animate their progress line (Claude's
  `(Xs · esc to interrupt)` ticks every second) so a genuinely-live turn never
  trips it; only `working` is time-gated (`blocked`/`done` are not). The DB row is
  left untouched — the override is purely in the per-tick derivation, like
  exited → `Idle`.
- **Rollup.** Repo groups roll up to their most-urgent member
  (`Blocked > Error > Working > Done > Unreachable > Idle`), rendered as a colored dot on
  the group header (`ui::project_list::group_status` +
  `group_header_line`). Status only recolors — it **never** reorders rows
  (the order cache stays status-independent).
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
  **macOS** native banner. This fixed a
  silent-failure bug: under WSL the dbus path errored on connect but only
  logged a `warn!`, so the user saw nothing. Delivery errors are now
  recorded in a process-wide `LAST_ERROR` slot (`notifications::last_error`)
  and surfaced by the diagnostic.
- **Diagnostic**: `thurbox-cli notify` (`cli/notify.rs`) prints the
  detected backend, whether it can deliver, click-to-focus support, and
  the last delivery error; `--test` fires a sample notification
  *synchronously* (`notifications::send_blocking`, since the short-lived
  CLI has no dispatcher thread) so the user can confirm end-to-end.
- **Click-to-focus** (dbus + macOS `terminal-notifier` paths). The dbus action callback writes a
  session UUID to the SQLite `metadata` row keyed by
  [`PENDING_FOCUS_SESSION_ID_KEY`](src/session/mod.rs) (= the single
  source of truth shared by writer and reader). The TUI's
  external-state poll (`App::poll_external_changes` →
  `apply_pending_focus_request`) reads + deletes the row atomically
  (`Database::take_pending_focus_session_id`, a single
  `DELETE … RETURNING` statement) and switches `active_index` +
  `InputFocus::Terminal`. On macOS the same row is written by
  `terminal-notifier`'s `-execute` flag (which shells back into
  `thurbox-cli session focus <id>`), so click-to-focus works whenever
  `terminal-notifier` is installed. The Windows-toast path and macOS's
  `osascript` fallback show the banner but ignore clicks (a Windows toast
  can't call back into WSL; the `osascript`/`UNUserNotificationCenter`
  action callbacks need a signed app bundle, which thurbox is not).
  **Terminal window-raising is
  deliberately not implemented**: thurbox runs inside an arbitrary
  terminal emulator it doesn't own, and per-emulator window control is
  fragile (especially on Wayland). The session is pre-selected; the
  user alt-tabs back themselves.
- **TUI-only lifecycle**. The PTY parser that observes the bell only
  runs while the TUI is alive, so notifications don't fire from
  headless `automation tick`. The dispatcher thread itself
  (`crate::notifications::start`) only starts when `[features]
  notifications = true` — zero overhead when disabled.
- **Gated by `[features] notifications`** (default on); knobs in
  `[notifications]` (also_on_waiting / suppress_for_active / sound /
  min_interval_secs / backend). `backend = "off"` is a soft delivery
  switch distinct from the `[features]` flag (the dispatcher still runs
  but drops everything). Settings live in `session::settings`
  (`NotificationBackend` enum); loader in `agent::settings_config`; full
  doc in `docs/CONFIG.md`.
- **Code shape**. `src/notifications.rs` is the leaf side-effect layer
  (only knows `session` + `paths`) — a single background thread reads a
  per-process mpsc channel and dispatches over the resolved backend
  (`notify-rust` for dbus, `terminal-notifier`/`osascript` for macOS,
  `powershell.exe` for the WSL toast). The
  notification body is bounded to 200 chars (`notify_state::truncate_body`)
  so a huge OSC message can't overflow the banner. The per-session
  bookkeeping (prior status, dedup timestamps) lives in
  `src/app/notify_state.rs` as a pure unit-testable struct, owned by
  `App` and constructed only when the feature is enabled. Backend
  selection (`resolve_backend`), the WSL marker check, powershell
  escaping, and body truncation are all pure functions with table-driven
  tests.

## Code review (native, tuicr-like)

Thurbox has a **built-in, natively-rendered** code-review view (no external
binary, no nested TUI) — a tuicr-like GitHub-style continuous diff with
classified comments and a review summary. It targets the active session's
worktree (`<base>..HEAD`), which maps cleanly onto thurbox's model (every
session is a worktree on a branch forked from a base). Toggle with `Ctrl+X`
(`F7` alternate; rebindable `Action::ToggleReview`), like the shell pane —
`Ctrl+X` is in `terminal_passthrough` (the emacs prefix key), so in a focused
terminal it reaches the agent and `F7` opens the review. Gated by `[features]
code_review`.

- **Surface (tuicr-like).** A continuous diff stream in the central pane (its own
  `InputFocus::CodeReview` — unlike the shell pane, a `TerminalView` that forwards
  to a PTY, it *captures* keys), plus a **changed-files list in the file-viewer
  column** (forced visible while a review is open via `layout_for`): it tracks the
  current file and clicking a row jumps the diff to that file
  (`ui::code_review::render_files_list` → `ClickAction::ReviewFile` →
  `cr_jump_to_file`). That changed-files list is itself a **focusable pane**
  (`InputFocus::ReviewFiles`, its ring stop while a review owns the column,
  replacing the plain `FileViewer`): focus it via `Ctrl+L`/`Ctrl+H` or a click,
  then `j`/`k` (+ arrows) walk file→file with the diff following, `g`/`G` jump to
  the first/last *file*, `Ctrl+D`/`U` + PageUp/Down half-page, `Enter`/`l` drop
  into the diff at the selected file (the file viewer's "open"), `r`/`R` toggle
  the file/hunk reviewed mark, and `Esc` closes the review
  (`App::handle_review_files_key`, captured before the global lookup like the diff
  pane). While open it owns the central pane; `Esc`/`Ctrl+X` (or `F7`) close it.
  Rendered by `ui::code_review`, reusing `scrollbar`/`focus_block`/
  `render_button_bar`/theme. **Unified or true paired side-by-side** diff layout,
  toggled with `v` / the footer button (`side_by_side`). Side-by-side is
  GitHub-style paired: a deletion (left) and its aligned addition (right) share
  **one** screen row (positional `del[k] ↔ add[k]` alignment via the pure
  `session::review::pair_hunk`; unpaired remainders get a blank half-cell). The
  pairing is a rendering concern only — `ReviewRow::Line` stays row-granular, so
  a paired row is still one selectable unit and every `match` on it is unchanged;
  the row build (`push_file_rows`) just emits one row per pair (the addition
  folds into its deletion's row) when `side_by_side`. Which side a comment
  attaches to is resolved at compose time (`CodeReviewState::selected_anchor`):
  keyboard defaults to New (the addition), a mouse click uses the column it hit
  (`App::cr_click_row` → `click_side`; left = Old, right = New). **Mouse-first**
  (no vim modal): click a diff line to select/comment, click footer buttons, drag
  the scrollbar, wheel-scroll. **tuicr nav keys**: `j`/`k` + arrows, PageUp/Down + `Ctrl+D`/`U`,
  `g`/`G`, `{`/`}` (or Tab) next/prev file, `[`/`]` next/prev hunk. Every footer
  button is labelled with its key (`Comment·c`, `Send→Agent·e`, `Find·/`, …) so
  the shortcuts are discoverable; the changed-files column shows a nav-key legend.
- **Long lines: horizontal scroll + wrap toggle.** A diff line wider than the
  pane doesn't get lost. By default the body scrolls horizontally with
  `Left`/`Right` (or `h`/`l`) while the line-number gutter stays pinned
  (`CodeReviewState::h_scroll`, stepped by `App::cr_scroll_h`, clamped to the
  longest line). A **wrap toggle** (`w` / the `Wrap`/`NoWrap` footer pill,
  `CodeReviewState::wrap`, `App::cr_toggle_wrap`) soft-wraps long lines onto
  extra screen rows instead. **Wrap works in both layouts** — a unified line
  wraps its body; a paired side-by-side row wraps each half independently and the
  taller half drives the visual-row count (the shorter half pads blank past its
  last chunk). Horizontal scroll stays unified-only (side-by-side always pins
  `h_scroll = 0`). The core invariant — **1 logical diff
  row = 1 selectable unit** — is preserved: selection, comment anchoring, click
  hitboxes, and the `selected`-primary scrollbar stay logical; wrapping only
  expands the *visual* rows in `render_rows`, and every visual sub-row carries
  its parent's logical index (so a click on a wrapped continuation selects the
  whole line, and compose anchors to the line's first visual row). Rendering:
  `unified_diff_line` (h-scroll) / `unified_diff_line_wrapped` (wrap) /
  `paired_diff_line` (side-by-side, wrap-aware) — the wrapped-row counts are
  mirrored by `visual_line_count` / `paired_visual_count` for the scroll walk.
- **Find in diff (`/`).** A `/`-triggered find sub-mode (also the `Find·/` footer
  button, and `/` from the changed-files pane) searches every visible row's text
  — file paths, hunk headings, diff line bodies, comment bodies (case-insensitive
  literal substring) — via the pure `CodeReviewState::{row_text,search_matches}`.
  It **mirrors the file viewer's find**: a bar at the top of the diff shows the
  `/`-prefixed query, the match position / count, and key hints; typing is
  incremental (the selection jumps to the first match live); while typing,
  `Enter`/`↓`/`Ctrl+N` step to the next match and `↑`/`Ctrl+P` the previous (all
  staying in the input), `Tab` commits (the bar stays for highlighting), and after
  committing `n`/`N` step matches relative to the cursor (`cr_search_step` scans
  from the selection + wraps, like the file viewer's `next_match`). `Esc` clears
  the search (a second `Esc` closes the review). Matched runs are highlighted in
  place with the shared `ui::highlight` accent+underline emphasis (on a matched
  diff line the literal hit replaces syntax colour for that line). State is
  `CodeReviewState::search: Option<ReviewSearch>` (the displayed position is
  derived from the selection, not stored), captured before the global keybinding
  lookup like compose / the target picker. Side-by-side diff rows navigate but
  aren't substring-highlighted (a v1 follow-up); folded (reviewed) files
  contribute only their header to the search until expanded.
- **Colours.** Dedicated theme keys `diff_added`/`diff_removed` (line fg) and
  `diff_added_bg`/`diff_removed_bg` (a subtle full-row tint) — added to
  `ThemePalette` (all 15 presets derive them; bg blended toward `app_bg` via
  `blend_rgb`) and overridable per custom theme. Classification badges reuse the
  status/accent/danger palette colours, so the whole view is theme-aware.
- **Review targets** (`t` / the Target footer button). The diff can show the
  whole branch (`<base>..HEAD`, the default), the **uncommitted working changes**
  (`git diff HEAD`), or a **single commit** (`git show`) — mirroring tuicr's
  `-r`/`-w`/commit targets. An in-view picker lists Working, Branch, and each
  commit in `<base>..HEAD` (`git log`); selecting one (keyboard ↑/↓/Enter **or a
  mouse click** on the entry — `render_target_picker` returns a `RowHitbox` per
  entry, recorded as `ClickAction::ReviewTarget(i)` → `App::cr_select_target`)
  recomputes the diff (`ReviewTarget`, `build_target_diff`,
  `git::{diff_working_on,show_commit_on, list_commits_on}`). A session with no
  resolvable base defaults to the working-changes target.
- **Multi-repo sessions.** A multi-repo session reviews **all** its worktrees at
  once: the diff is built per repo and concatenated, with each file path
  namespaced `"<repo>/<path>"` so files, comments, and "reviewed" marks stay
  unambiguous; the changed-files column shows the repo-qualified paths. Each repo
  resolves its own base (the session base if that branch exists there, else that
  repo's default branch); the commit picker lists commits across every repo,
  repo-tagged. A commit target scopes to its one repo. State is
  `Vec<ReviewRepo>` on `CodeReviewState`; the diff is assembled by `build_files`.
- **Diff model + parser.** Pure data in `session::review` (`DiffFile`/`DiffHunk`/
  `DiffLine`, `Classification`, `CommentAnchor`, `ReviewComment`) with a
  unit-tested `parse_unified_diff`. `git::diff_against{,_on}` runs `git diff`
  (local or over SSH). The diff types live in `session` so `ui` can render them
  without importing `git` (architecture rule).
- **Syntax highlighting.** The unified diff body is syntax-highlighted by a
  small dependency-free lexer (`ui::syntax`: comments / strings / numbers /
  keywords / capitalised types), themed from the palette. Add/remove stays on the
  gutter `+`/`-` sign + the row tint, so the code text itself carries the syntax
  colours (GitHub-style). Side-by-side keeps plain add/remove colouring.
- **Comments.** Line / file / review-summary level, each with a classification
  (issue / suggestion / note / praise, colored badges). Composed in an **in-view
  box that floats inline at the selected line** (`render_compose_inline` anchors
  it to the line's screen row, falling back above/below as room allows) — a
  `ComposeState` sub-mode, not a separate modal. State lives in
  `app::code_review::CodeReviewState`.
- **Reviewed marks.** `r` / `R` toggle a file / hunk as reviewed (`✓`); `r`
  resolves the file from **any** row inside it (line, hunk, header, or a comment),
  not just the file header.
- **Persistence.** `review_comments` + `review_marks` tables (schema v38) keyed by
  session id (`storage::review`); the worktree's fork point is the write-once
  `sessions.base_branch` column (targeted accessors, like `hook_state`), set at
  spawn. Reviews are kept open per session across switches (like the shell view).
  Legacy/NULL base falls back to the repo's default branch.
- **Export.** No GitHub/GitLab submit (intentionally out of scope). Instead:
  `y` copies the review as markdown to the clipboard, and `e` (Send→Agent) pastes
  the compiled review into the session's agent as a prompt to address it — the
  review → agent → re-review loop, the orchestrator-native equivalent of submit.
- **v1 follow-ups** (named, not silently dropped): range/multi-line comments,
  token-level intra-line word diffs on a paired row (v1 aligns whole lines
  positionally, not sub-line), async diff build for
  huge repos (v1 builds synchronously on toggle), grammar-aware syntax
  highlighting (v1's lexer is heuristic + language-agnostic), horizontal
  scroll in the **side-by-side** layout (wrap now works there; paired rows still
  pin `h_scroll = 0`), per-side search-match highlighting in
  side-by-side (v1 navigates but doesn't substring-highlight paired rows),
  auto-revealing a horizontally-scrolled-off search match, and
  search-match highlight across a wrap-boundary seam.

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

`record.sh` records every video pair in one pass: the combined
hero demo (`thurbox-demo.*` via `agents.tape`), one clip per
feature
(`thurbox-{file-manager,info-panel,theme,session-creation,fork}.*`),
and the automations/tasks/search demos (`automations-demo.*`,
`tasks-demo.*`, `search-demo.*`) — one VHS tape each
(`scripts/demo/<feature>.tape`). With no args it records all of
them; pass tape stems to re-record a subset (the `agents` stem is
the hero, `automations`/`tasks`/`search` map to `<stem>-demo.*`,
every other stem maps to `thurbox-<stem>.*`).

Every clip uses **real agent CLIs**: the script seeds one session
per installed CLI (`claude`, `opencode`, `codex`, `antigravity`) in a
throwaway sample repo and launches them with no prompt. It
overrides `HOME`, so agents boot with fresh history/config (no
past conversations leak); CLIs that authenticate via the system
keyring stay logged in but show no account email on screen. The
tapes exercise the session list, info panel (`Ctrl+B`), file
viewer (`Ctrl+E`), native code review (`Ctrl+X`, the default
`ToggleReview` chord; `F7` alternate), theme picker, session-creation flow, and the
Automations pane over the seeded sessions and sample tree. The hero
`agents` demo also opens the code-review view, so it seeds the same
worktree-with-a-committed-diff session the dedicated `code-review`
clip uses.

It runs fully isolated from your real environment — a dev build
(`0.0.0-dev` → `dev_build` cfg) uses the `thurbox-dev` socket and
XDG subdirs, and the script points `TMUX_TMPDIR` and
`XDG_{DATA,CONFIG,STATE,CACHE}_HOME` at a throwaway temp dir.
**`TMUX_TMPDIR` is essential**: the `thurbox-dev` socket *name* is
shared by every dev build, so without a private socket directory
the cleanup `kill-server` would tear down dev sessions you already
have running.

The deterministic recording path (a hidden `__demo-agent`
subcommand streaming canned scenarios) was retired in favor of the
single real-agents script and has been removed from the binary.

`.github/workflows/pages.yml` copies the mp4s into
`website/assets/` at deploy time and `README.md` embeds the gifs,
so regenerating these files propagates everywhere.

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
  selection, `links.rs` detects clickable URLs for Ctrl+Click.
  Mouse clicks are routed through a per-frame registry
  (`App::click_targets`, mirroring `scrollbar_hits`): list/modal
  renderers return `ui::RowHitbox`es, `App::view` records them as
  `ClickAction`s, and `handle_mouse_click` hit-tests them (rows
  select/confirm, panes focus, modals swallow everything else; the
  hovered row is underlined via mouse-move events).
  **Clickable buttons** reuse the same registry: `ui::render_button_bar`
  draws filled "pill" buttons (` Label ` on a solid accent/gray fill, no
  brackets) and returns `ui::ButtonHit`es. The bottom status-bar footer
  renders Help/Info/Files/Theme/Tasks/Settings/Quit pills, ordered by F-key
  (`Help · F1` … `Settings · F6`) with `Quit` last, each suffixed with its
  live (rebindable) shortcut (an F-key alternate where one exists, else the
  caret-ctrl chord `Quit · ^Q`). The panel toggles are feature-gated
  (Info/Files/Tasks dropped when their panel feature is off; the same pills
  are also dropped *together* when the footer is too narrow to fit the full
  set (`pill_block_width` vs the footer width) — so the essential
  Help/Theme/Settings/Quit pills never fall off). When the file viewer is open
  its hints fill the space to their left.
  Pills are recorded as `ClickAction::Global(Action)` (a click runs
  `dispatch_action`, ignored while a modal is open). Every modal footer renders action buttons
  (Save/Cancel/Select/…) returned as `ui::ModalButtons` (each `ButtonHit`
  paired with the key it replays) and recorded as
  `ClickAction::ModalButton { code, mods }`; `handle_modal_click` replays
  that key through the modal's own handler so a click matches the keyboard
  path. **Clicking a field** selects it: editor modals (Settings / Automation)
  ship per-field hitboxes recorded as `ClickAction::ModalField(i)` (→
  `select_modal_field`, sets the active field like Tab/↑↓) — and in **Settings**
  a click on a boolean row also **toggles** it (its whole point is the on/off
  switch; scalar rows only select, so a stray click never changes a number); the in-pane
  automation/task editors record `ClickAction::PaneField { focus, index }` (→
  focus the editor + `select_pane_field`); the repo picker records
  `ClickAction::RepoFocus(..)` for its path-input / search sub-fields. Hovering
  a button reverses its fill (`Modifier::REVERSED`), distinct from the row
  underline. With a modal
  open, the wheel steps its selection and overflowing picker lists
  render a draggable scrollbar (`ScrollTarget::Modal`, drag replayed
  as Up/Down through the modal's key handler). All of it is gated by
  `[features] mouse` in settings.toml — disabled, mouse capture is
  never enabled and the terminal keeps native mouse behavior.
  `agent_picker_modal` drives the new-session flow.
- **Central-pane tab strip.** The agent terminal, the per-session shell, and the
  code-review view share the central pane, surfaced as a clickable tab strip
  (`Agent · Review · F7 · Shell · F8`) painted on the pane's **top border** by
  `App::draw_central_tabs`, which renders each tab as a filled **pill button**
  (`ui::render_pill`, the standalone form of the footer's `render_button_bar`
  chips) so it reads as clickable exactly like the Help/Tasks/… footer pills —
  the active view is the accent-filled "primary" pill, the rest neutral
  "secondary" pills (hover reverses the fill via the shared `is_button` path).
  Each tab carries its toggle's live shortcut hint, preferring the **F-key**
  alternate
  (`tab_shortcut`) — a focused agent terminal passes `Ctrl+<letter>` chords
  through to the CLI (`Ctrl+X` is emacs's prefix key, so it never reaches
  `ToggleReview`), whereas the F-key dispatches in every pane. Agent has no
  dedicated key (the Shell toggle returns to it), so it shows no hint.
  Shell/Review tabs are gated by their feature
  flags. `central_tab_cells` lays out the on-border hitboxes (recorded as
  `ClickAction::CentralTab(CentralTab::{Agent,Shell,Review})` **before** the
  pane's whole-rect focus fallback so a tab click wins); a click runs
  `App::select_central_tab`, which *selects* the view (closing any open review
  when switching to Agent/Shell, opening it for Review) — distinct from the
  keyboard `Ctrl+T`/`Ctrl+X` *toggles*. So the central pane's session-info title
  (`terminal_view`/`code_review`) is **right-aligned** (via `title_top` +
  `ui::title_style`) to leave the border's left free for the tabs. The F-keys
  switch views from **any** view: `ToggleShell` is a `review_escape_chord` (so an
  open review lets F8 fall through to the global binding instead of swallowing
  it), and `toggle_shell_view` is review-aware — with a review open it closes it
  and lands on the shell, mirroring the Shell tab.
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

- **Interactively** from the F1 panel, which is a live editor rather
  than a read-only overlay. `j`/`k` select an action, `Enter`/`r`
  starts capture (the **next physical keypress** — including chords
  like `ctrl+q` — becomes that action's sole binding), `d` resets the
  selected action to its built-in default, and `Shift+D` resets **all**
  actions (via `App::reset_all_keybindings`, which deletes the override
  file so defaults stay authoritative). If the captured chord was already
  bound elsewhere it is reassigned (stolen from the other action) and a
  status toast reports the move. Each change is persisted immediately
  via `KeyBindings::{rebind,reset}` + `storage::keybindings::save_keybindings_json`
  and takes effect on the next keystroke — no restart. The editor lives
  in `Modal::Help(HelpModal { selected, capturing })`; capture input is
  routed through `App::handle_help_key` inside `handle_priority_key`
  (**before** the global `keybindings.lookup`, so capturing `ctrl+q`
  rebinds instead of quitting). Selection indices match
  `Action::rebindable_in_order()` — the flattened
  `keybindings::help_sections()`, the shared order used by
  `render_help_overlay`.
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
description) accept the standard emacs/readline line-editing chords, so the same
muscle memory works there as in a terminal: `Ctrl+A`/`Ctrl+E` line start/end,
`Ctrl+B`/`Ctrl+F` move by char, `Ctrl+H`/`Ctrl+D` delete the char before/under
the cursor, `Ctrl+W` delete word, `Ctrl+U`/`Ctrl+K` kill to line start/end. The
dispatch lives in one place — `modals::apply_ctrl_line_edit` over the `LineEdit`
trait (implemented by both `TextInput` and `TextArea`) — and **every**
`Ctrl`+letter is consumed (mapped or swallowed) so a bare control letter never
leaks into the field. A `Ctrl` chord with a non-letter key (arrows, Home/End)
falls through to normal cursor handling.

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

The TUI ships with fifteen palettes — eleven dark (**Default**, **Catppuccin
Mocha**, **Tokyo Night**, **Gruvbox Dark**, **Doom**, **Nord**, **Dracula**,
**One Dark**, **Rosé Pine Moon**, **Everforest**, **Kanagawa**) and four light
(**Catppuccin Latte**, **Tokyo Night Day**, **Gruvbox Light**,
**Solarized Light**). Users can add **custom themes** in
`~/.config/thurbox/themes.toml` (a built-in `base` plus per-colour
overrides — see `docs/CONFIG.md`); they appear in the picker after the
built-ins and persist by name exactly like a preset
(`session::theme_config::CustomThemeDef` → `ThemeEntry`, loaded by
`agent::themes_config::load_or_seed_with_warnings`, published via
`ui::theme::set_custom_themes`).
Pick one with `Ctrl+Y` (or `F4`,
which avoids terminals that intercept Ctrl+Y as DSUSP); the choice
is persisted in SQLite under `metadata.active_theme` and survives
restarts. Other thurbox processes pick up theme changes within one
tick via `PRAGMA data_version` polling.

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
`global_search`, `shell_pane`, `code_review`, `soft_delete`) are read from `App.features`
every frame, so `submit_settings_panel` copies the draft's flags into
`self.features` (via `App::apply_live_settings`) and they take effect
immediately. `apply_live_settings` also runs `enforce_feature_visibility`,
which tears down any surface a now-disabled live flag left open (the `show_*`
panel toggles, a session's open shell view, an open code review) and moves
focus off it — otherwise the panel would keep rendering with its tab/footer
affordance gone. Each branch only forces the *hidden* state, so re-enabling a
flag never re-opens anything. Everything else is read once at startup from the write-once
`settings::global()` `OnceLock` (which can't be re-applied in-process):
those rows are marked `⟳`, and a save that changes one toasts "some changes
apply after restart".

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

## Design Documentation

For rationale behind decisions, see `docs/`:

- `docs/CONSTITUTION.md` — Core principles and non-negotiable rules
- `docs/ARCHITECTURE.md` — Architectural decisions with rationale
- `docs/FEATURES.md` — Feature-level design choices
- `docs/CONFIG.md` — Every config file/env var/DB setting in one place
- `docs/PERFORMANCE.md` — Render/tick performance: demand-driven redraw,
  perf counters, the session-order cache, and how to measure

**Rule**: If a code change invalidates or extends a documented
decision, update the relevant doc in the same PR.
