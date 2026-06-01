# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code)
when working with code in this repository.

## Project

Thurbox is a multi-session coding-agent TUI orchestrator built
with Rust. It runs multiple coding-agent CLI instances (Claude
Code, Codex, Gemini CLI, opencode, aider, … — any CLI you
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
- 25 tests covering platform detection, checksum verification, binary extraction, and error handling
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
   - Builds binaries for 3 platforms (version passed via environment variable)
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

- Binaries for 3 platforms:
  - `thurbox-v{ver}-x86_64-unknown-linux-gnu.tar.gz`
  - `thurbox-v{ver}-x86_64-unknown-linux-musl.tar.gz`
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
- **Scopes**: cli, ui, git, core, docs, deps, config, agent
- Use `cog commit feat "message"`
  or `cog commit fix "message" scope`

## Agent Definitions

The set of launchable coding agents is declared **as data** in
`~/.config/thurbox/agents.toml`, seeded with built-ins
(`claude`, `codex`, `gemini`, `opencode`, `aider`, `vibe`) on first run.
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
```

Each `*_args` group is appended only when its driving value is
present, with `{id}` substituted; `args` is always passed. No
model is ever passed — each agent uses its own default config
(put `["--model", "opus"]` in `args` if you want to pin one).
Agents that omit `resume_args` simply start fresh on restart (the
live tmux process is what carries state across TUI restarts). Add
your own `[[agents]]` entry to support any CLI — no recompile.

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

## thurbox-cli

A second binary (`thurbox-cli`) drives the same SQLite-backed,
tmux-hosted sessions headlessly (no TUI). It shares the database
with the TUI; changes appear via `PRAGMA data_version` polling.

```bash
cargo build --bin thurbox-cli
thurbox-cli session create --name demo --repo-path /path \
    --agent codex --worktree-branch feat/x
thurbox-cli session list | jq
```

Subcommands: `session` (create/list/get/delete/restore/restart/
send/capture), `automation` (alias `auto`:
create/list/show/edit/remove/run/runs/tick), `editor`. Pass
`--pretty` for indented JSON.

Automations fire even when the TUI is closed: a tmux heartbeat
keeper window (`automation-heartbeat`, armed on TUI startup and on
`automation create`) loops `automation tick` every 60 s and keeps
the tmux server alive. `packaging/` ships opt-in systemd/launchd
units for reboot-proof firing. Concurrent firers are de-duplicated
by `Database::claim_due_automation` (atomic CAS), so the TUI, the
keeper, and an OS timer never double-fire.

In the TUI, automations also get a dedicated **Automations pane**
beneath the session list (left column). It is always present
(showing `none` when empty) and is treated as **part of the session
pane**: it forms one continuous vertical list with the session list,
so pressing `j` past the last session drops focus into the pane and
`k` at the top automation hands focus back to the last session. It
is **not** a separate stop in the `Ctrl+H`/`Ctrl+L` cycle (which
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
feature (`thurbox-{file-manager,info-panel,theme,session-creation}.*`),
and the automations demo (`automations-demo.*` via
`automations.tape`) — one VHS tape each
(`scripts/demo/<feature>.tape`). With no args it records all of
them; pass tape stems to re-record a subset (the `agents` stem is
the hero, `automations` is the automations clip, every other stem
maps to `thurbox-<stem>.*`).

Every clip uses **real agent CLIs**: the script seeds one session
per installed CLI (`claude`, `opencode`, `codex`, `gemini`) in a
throwaway sample repo and launches them with no prompt. It
overrides `HOME`, so agents boot with fresh history/config (no
past conversations leak); CLIs that authenticate via the system
keyring stay logged in but show no account email on screen. The
tapes exercise the session list, info panel (`Ctrl+B`), file
viewer (`Ctrl+E`), theme picker, session-creation flow, and the
Automations pane over the seeded sessions and sample tree.

It runs fully isolated from your real environment — a dev build
(`0.0.0-dev` → `dev_build` cfg) uses the `thurbox-dev` socket and
XDG subdirs, and the script points `TMUX_TMPDIR` and
`XDG_{DATA,CONFIG,STATE,CACHE}_HOME` at a throwaway temp dir.
**`TMUX_TMPDIR` is essential**: the `thurbox-dev` socket *name* is
shared by every dev build, so without a private socket directory
the cleanup `kill-server` would tear down dev sessions you already
have running.

The hidden `__demo-agent <scenario>` subcommand (`src/main.rs` →
`agent::demo::run_demo_agent()` in `src/agent/demo.rs`, streaming
canned `src/agent/demo_scenarios/*.txt`) still exists in the
binary but is **no longer used by the recording pipeline** — the
deterministic recording path was retired in favor of the single
real-agents script.

`.github/workflows/pages.yml` copies the mp4s into
`website/assets/` at deploy time and `README.md` embeds the gifs,
so regenerating these files propagates everywhere.

## Architecture (TEA Pattern)

The app follows **The Elm Architecture**:
`Event → Message → update(model, msg) → view(model) → Frame`

### Module Dependency Rules (enforced by tests/architecture_rules.rs)

```text
session  ← pure data types, no local imports
agent    ← imports session only (NEVER ui or git)
ui       ← imports session only (NEVER agent or git)
app      ← coordinator, imports all modules
```

### Module Responsibilities

- **`app/`** — Model (`App` struct) + Update
  (`AppMessage` enum + `handle_key/resize`) + View.
  Owns all state, coordinates side effects.
- **`agent/`** — Side-effect layer. `AgentProvider` trait
  abstracts CLI command + arg construction; `GenericProvider`
  implements it from a declarative `AgentDef` (loaded via
  `agent_config`). `Session` wraps a `SessionBackend`
  trait. `BackendRegistry` holds the backends; the only
  backend is `LocalTmuxBackend` (using `tmux -L thurbox`).
  Reads output into `Arc<Mutex<vt100::Parser>>`, writes input
  via mpsc channel. `input.rs` translates crossterm `KeyCode`
  → xterm ANSI bytes.
- **`session/`** — Plain data: `SessionId`, `SessionStatus`,
  `SessionInfo` (with `agent` name), `SessionConfig` (agent
  name, ids, cwd, env), `AgentDef`/`AgentRegistry`.
  Mostly Display/Default impls plus the agent-arg
  substitution logic.
- **`ui/`** — Pure rendering functions. `layout.rs` computes
  panel areas (responsive: <80 = terminal only, >=80 = 2-panel,
  >=120 = optional 3-panel). Widgets: `project_list` (session
  list with repo/branch display), `terminal_view`, `info_panel`,
  `status_bar`, `repo_picker_modal` (repo selection with
  worktree toggle). `selection.rs` handles mouse-drag text
  selection, `links.rs` detects clickable URLs for Ctrl+Click.
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

16 hooks run automatically via `prek` (Rust-based pre-commit
framework). Install with `prek install`. Stages:

- **commit-msg**: conventional commit validation (`cog verify`)
- **pre-commit**: fmt, clippy, check, nextest, architecture,
  deny, doc, bats, rumdl, prettier, htmlhint, stylelint,
  eslint
- **pre-push**: commit history check (`cog check`)

## Key Technical Details

- MSRV: 1.75, Edition 2021
- Async runtime: tokio (multi-threaded)
- Session backend: `LocalTmuxBackend` (`tmux -L thurbox`)
- Output reader runs in `tokio::task::spawn_blocking`
  (blocking I/O), writer in `tokio::spawn` (async)
- Terminal state parsed by `vt100::Parser`,
  rendered by `tui_term::PseudoTerminal`
- Sessions persist across restarts (tmux keeps them alive)
- Session state in SQLite:
  `~/.local/share/thurbox/thurbox.db` (XDG_DATA_HOME respected);
  agent definitions in `~/.config/thurbox/agents.toml`
- Requires tmux >= 3.2

## Keybindings (Vim-Inspired)

Global keys use `Ctrl` + semantic Vim conventions:

| Key | Action | Mnemonic |
|-----|--------|----------|
| `Ctrl+Q` | Quit (detach sessions) | **Q**uit |
| `Ctrl+N` | New session (opens repo picker) | **N**ew |
| `Ctrl+C` | Copy selection / SIGINT (terminal) | **C**opy |
| `Ctrl+V` | Paste from clipboard | Paste |
| `Ctrl+P` | Automations (list/new/edit/toggle/run/delete) | **P**rogram |
| `Ctrl+T` | Toggle shell pane | **T**erminal |
| `Ctrl+H` | Focus previous pane (cycle backward) | Vim: **h** = left |
| `Ctrl+J` | Select next session | Vim: **j** = down |
| `Ctrl+K` | Select previous session | Vim: **k** = up |
| `Ctrl+L` | Focus next pane (cycle forward) | Vim: **l** = right |
| `Ctrl+D` | Delete session | Vim: **d** = delete |
| `Ctrl+O` | Open active session's worktree in editor | **O**pen |
| `Ctrl+R` | Restart active session | **R**estart |
| `Ctrl+F` | Fork active session | **F**ork |
| `Ctrl+S` | Sync worktrees with origin/main | **S**ync |
| `Ctrl+Z` | Undo session delete | **Z** = undo |
| `Ctrl+U` | Restore deleted sessions | **U**ndelete |
| `Ctrl+Y` / `F4` | Pick TUI theme | Color **Y**oke |
| `F1` | Toggle keybindings help | Universal |
| `F2` | Toggle info panel (visible at width >= 120) | Next to F1 |
| `F3` | Toggle file viewer | Next to F2 |

List contexts use plain `j`/`k`/`Enter` for navigation.
Terminal forwards all non-Ctrl keys to the PTY.
`Shift+arrows/PageUp/PageDown` for scrollback.

These defaults can be overridden by writing
`~/.config/thurbox/keybindings.json`. The file maps an `Action`
name to one or more chord strings, e.g. `{ "QuitApp": ["ctrl+x"] }`.
Modal-internal keys (j/k/Enter/Esc inside selectors) are not
customizable.

## Themes

The TUI ships with eight palettes — four dark (**Default**, **Catppuccin
Mocha**, **Tokyo Night**, **Gruvbox Dark**) and four light (**Catppuccin
Latte**, **Tokyo Night Day**, **Gruvbox Light**, **Solarized Light**).
Pick one with `Ctrl+Y` (or `F4`,
which avoids terminals that intercept Ctrl+Y as DSUSP); the choice
is persisted in SQLite under `metadata.active_theme` and survives
restarts. Other thurbox processes pick up theme changes within one
tick via `PRAGMA data_version` polling.

## Design Documentation

For rationale behind decisions, see `docs/`:

- `docs/CONSTITUTION.md` — Core principles and non-negotiable rules
- `docs/ARCHITECTURE.md` — Architectural decisions with rationale
- `docs/FEATURES.md` — Feature-level design choices

**Rule**: If a code change invalidates or extends a documented
decision, update the relevant doc in the same PR.
