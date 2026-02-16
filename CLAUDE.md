# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code)
when working with code in this repository.

## Project

Thurbox is a multi-session Claude Code TUI orchestrator built
with Rust. It runs multiple `claude` CLI instances inside
persistent tmux sessions, rendered as terminal panels via
ratatui + tui-term. Sessions survive crashes/restarts.

## Build & Development Commands

```bash
cargo check --all                    # Type check
cargo build                          # Debug build
cargo build --release                # Release build (LTO, stripped)
cargo run                            # Run in dev mode
```

## Testing

```bash
cargo nextest run --all              # Run all tests (preferred runner)
cargo nextest run -E 'test(name)'    # Run a single test by name
cargo nextest run --all --profile ci # Run with CI profile
cargo test test_name                 # Run single test via cargo test
bats scripts/install.bats            # Test install script (requires bats-core)
```

## Installation Script

**Location:** `scripts/install.sh`

One-liner installation for end users:

```bash
curl -fsSL https://raw.githubusercontent.com/Thurbeen/thurbox/main/scripts/install.sh | sh
```

**Features:**

- Platform detection (Linux/macOS, x86_64/aarch64)
- Automatic version fetching with API rate limit fallback (scrapes releases page)
- SHA256 checksum verification
- Creates `~/.local/bin` if needed
- Post-install instructions
- Graceful error handling with helpful messages

**Environment variables:**

- `VERSION=v0.1.0` - Install specific version (default: latest from GitHub API)
- `INSTALL_DIR=/path` - Custom install directory (default: `~/.local/bin`)

**Testing:**

- Comprehensive test suite in `scripts/install.bats` using bats-core framework
- 36 tests covering platform detection, checksum verification, binary extraction, and error handling
- Run tests locally: `bats scripts/install.bats`
- CI runs tests automatically on every commit

**Implementation notes:**

- POSIX shell (`#!/usr/bin/env sh`) for maximum compatibility
- No external dependencies beyond standard tools (curl/wget, tar, sha256sum/shasum)
- Non-interactive for safe pipe-to-shell execution
- Proper error handling and cleanup via trap

## Linting & Formatting

```bash
cargo fmt --all                      # Format (rustfmt: 100 char max)
cargo clippy --all-targets --all-features -- -D warnings  # Lint
RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --all-features  # Docs
rumdl check .                        # Markdown lint (.rumdl.toml)
rumdl fmt .                          # Markdown auto-fix
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
   - Builds binaries for 4 platforms (version passed via environment variable)
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
  - `thurbox-v{ver}-x86_64-apple-darwin.tar.gz`
  - `thurbox-v{ver}-aarch64-apple-darwin.tar.gz`
- `thurbox-v{ver}-checksums.txt` (SHA256 sums for verification)
- Changelog with categorized commits

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
- **Scopes**: api, cli, ui, git, core, docs, deps, config
- Use `cog commit feat "message"`
  or `cog commit fix "message" scope`

## Architecture (TEA Pattern)

The app follows **The Elm Architecture**:
`Event → Message → update(model, msg) → view(model) → Frame`

### Module Dependency Rules (enforced by tests/architecture_rules.rs)

```text
session  ← pure data types, no project-local imports
project  ← pure data types + config loading, imports session only
claude   ← imports session only (NEVER ui, git, or project)
ui       ← imports session and project only (NEVER claude or git)
app      ← coordinator, imports all modules
```

### Module Responsibilities

- **`app/`** — Model (`App` struct) + Update
  (`AppMessage` enum + `handle_key/resize`) + View.
  Owns all state, coordinates side effects.
- **`claude/`** — Side-effect layer. `Session` wraps a
  `SessionBackend` trait (default: `LocalTmuxBackend` using
  `tmux -L thurbox`). Reads output into
  `Arc<Mutex<vt100::Parser>>`, writes input via mpsc channel.
  `input.rs` translates crossterm `KeyCode` → xterm ANSI bytes.
- **`session/`** — Plain data: `SessionId`, `SessionStatus`,
  `SessionInfo`, `SessionConfig` (with optional `cwd`).
  No logic beyond Display/Default impls.
- **`project/`** — Plain data + config loading: `ProjectId`,
  `ProjectConfig`, `ProjectInfo`. Loads project list from
  `~/.config/thurbox/config.toml`. Imports `session` only.
- **`ui/`** — Pure rendering functions. `layout.rs` computes
  panel areas (responsive: <80 = terminal only, >=80 = 2-panel,
  >=120 = optional 3-panel). Widgets: `project_list` (two-section
  left panel), `terminal_view`, `info_panel`, `status_bar`.

### Event Loop (main.rs)

```text
tokio::main → init backend (tmux) → load project config
→ init terminal → spawn/restore sessions → loop {
    draw frame → poll crossterm events (10ms)
    → convert to AppMessage → app.update() → app.tick()
} → app.shutdown() (detach sessions) → restore terminal
```

- Logging goes to `~/.local/share/thurbox/thurbox.log`
  (file-based, since stdout is owned by the TUI)
- Panic hook restores terminal before printing

## Pre-commit Hooks

11 hooks run automatically via `prek` (Rust-based pre-commit
framework). Install with `prek install`. Stages:

- **commit-msg**: conventional commit validation (`cog verify`)
- **pre-commit**: fmt, clippy, check, nextest, architecture,
  deny, audit, doc, rumdl
- **pre-push**: commit history check (`cog check`)

## Key Technical Details

- MSRV: 1.75, Edition 2021
- Async runtime: tokio (multi-threaded)
- Session backend: `tmux -L thurbox` (dedicated server)
- Output reader runs in `tokio::task::spawn_blocking`
  (blocking I/O), writer in `tokio::spawn` (async)
- Terminal state parsed by `vt100::Parser`,
  rendered by `tui_term::PseudoTerminal`
- Sessions persist across restarts (tmux keeps them alive)
- `Ctrl+Q` is the quit key (detaches, does not kill sessions)
- Config file: `~/.config/thurbox/config.toml`
  (XDG_CONFIG_HOME respected)
- Requires tmux >= 3.2

## Design Documentation

For rationale behind decisions, see `docs/`:

- `docs/CONSTITUTION.md` — Core principles and non-negotiable rules
- `docs/ARCHITECTURE.md` — Architectural decisions with rationale
- `docs/FEATURES.md` — Feature-level design choices

**Rule**: If a code change invalidates or extends a documented
decision, update the relevant doc in the same PR.
