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
- **Scopes**: api, cli, ui, git, core, docs, deps, config, mcp
- Use `cog commit feat "message"`
  or `cog commit fix "message" scope`

## MCP Server

A separate binary (`thurbox-mcp`) exposes Thurbox configuration
over the Model Context Protocol. It supports stdio (default) and
Streamable HTTP (`--transport streamable-http`) transports, and
shares the same SQLite database as the TUI — changes appear
automatically via `PRAGMA data_version` polling.

```bash
cargo build --bin thurbox-mcp       # Build MCP server
cargo run --bin thurbox-mcp         # Run stdio (default)
thurbox-mcp --transport streamable-http        # HTTP on 127.0.0.1:8080
thurbox-mcp --transport streamable-http --port 9090  # Custom port
```

### Available Tools

| Tool | Description |
|------|-------------|
| `list_roles` | List all global roles |
| `set_roles` | Atomically replace all global roles |
| `list_mcp_servers` | List all global MCP servers |
| `set_mcp_servers` | Set global MCP servers |
| `list_sessions` | List all active sessions |
| `get_session` | Get a session by UUID |
| `delete_session` | Soft-delete a session (TUI cleans up tmux/worktree) |
| `restart_session` | Restart a session synchronously (kills the tmux window and re-spawns claude with --resume) |
| `restore_session` | Restore a soft-deleted session |
| `list_skills` | List all effective skills (disk-source + registered) with source tag |
| `set_skills` | Atomically replace all skill registry entries |
| `register_skill` | Register/update a single skill reference (path must contain SKILL.md) |
| `unregister_skill` | Remove a skill from the registry (never touches disk) |
| `list_profiles` | List all effective profiles (bundled role/MCP/skill presets) |
| `get_profile` | Get a single profile by name |
| `set_profiles` | Atomically replace all global profiles |
| `register_profile` | Register/update a single profile — referenced roles/MCP/skills must exist |
| `unregister_profile` | Remove a profile from the registry |
| `create_session` | Spawn a new local-tmux session (optionally on a fresh worktree) |
| `send_prompt` | Send text to a session's terminal immediately (orchestrator mode) |
| `capture_session_output` | Read rendered pane contents from a session |
| `schedule_command` | Schedule text to be sent to a session at a future time |
| `list_scheduled_commands` | List pending scheduled commands, optionally by session |
| `get_scheduled_command` | Get a scheduled command by ID |
| `cancel_scheduled_command` | Cancel a pending scheduled command |
| `get_editor_command` | Get the editor command used by Ctrl+O |
| `set_editor_command` | Set the editor command used by Ctrl+O |
| `list_themes` | List available built-in TUI theme presets |
| `get_theme` | Get the active TUI theme preset id |
| `set_theme` | Set the active TUI theme (live-applied via data-version polling) |
| `get_keybindings` | Get the user keybindings JSON document (or built-in defaults) |
| `set_keybindings` | Replace `~/.config/thurbox/keybindings.json` (effective on next TUI start) |
| `reset_keybindings` | Delete the keybindings override file, restoring defaults |

**Role Management**: Roles are global presets.
`set_roles` performs an atomic replacement — all existing roles
are deleted and replaced in a single transaction. To add a role,
include all existing roles plus the new one.
See [`docs/MCP_ROLES.md`](docs/MCP_ROLES.md) for the complete
role configuration guide including permission modes, tool name
format, and example role patterns.

**Profiles**: A profile is a named bundle of role, MCP server,
and skill references applied together at session spawn. When
multiple roles are listed, their `RolePermissions` are merged:
union of `allowed_tools` and `disallowed_tools`, concatenated
`append_system_prompt`, env maps merged with later-wins
precedence, and the most-permissive `permission_mode` wins
(ranked `plan` < `default` < `acceptEdits` < `bypassPermissions`;
unknown modes rank lowest). Apply a profile via
`create_session {"profile": "<name>"}` or `thurbox-cli session
create --profile <name>`. Explicit `role`, `mcp_servers`, or
`skills` on the spawn call override the profile's contribution
for that field. One `orchestrator` profile is seeded by default
(roles=`[developer]`, skills=`[orchestrate]`).

### Admin Session (built-in MCP client)

The TUI includes a global "Admin" session that auto-configures
`thurbox-mcp` as an MCP server. On startup, Thurbox creates
`~/.local/share/thurbox/admin/.mcp.json` and spawns an admin
session there. Claude Code discovers the MCP config automatically,
enabling conversational role/session management inside
the TUI. See `docs/FEATURES.md` for details.

### Module Isolation

```text
mcp → storage, session, sync, paths
      (NEVER app, agent, ui, git)
```

## Architecture (TEA Pattern)

The app follows **The Elm Architecture**:
`Event → Message → update(model, msg) → view(model) → Frame`

### Module Dependency Rules (enforced by tests/architecture_rules.rs)

```text
session  ← pure data types, no local imports
agent    ← imports session only (NEVER ui or git)
ui       ← imports session only (NEVER agent or git)
mcp      ← imports storage, session, sync, paths only
app      ← coordinator, imports all modules
```

### Module Responsibilities

- **`app/`** — Model (`App` struct) + Update
  (`AppMessage` enum + `handle_key/resize`) + View.
  Owns all state, coordinates side effects.
- **`agent/`** — Side-effect layer. `AgentProvider` trait
  abstracts CLI command + arg construction (default:
  `ClaudeProvider`). `Session` wraps a `SessionBackend`
  trait. `BackendRegistry` holds the active backend
  (`LocalTmuxBackend` using `tmux -L thurbox`).
  Reads output into `Arc<Mutex<vt100::Parser>>`, writes input
  via mpsc channel. `input.rs` translates crossterm `KeyCode`
  → xterm ANSI bytes.
- **`session/`** — Plain data: `SessionId`, `SessionStatus`,
  `SessionInfo`, `SessionConfig` (with optional `cwd`).
  `default_developer_role()` provides the seeded developer role.
  No logic beyond Display/Default impls.
- **`ui/`** — Pure rendering functions. `layout.rs` computes
  panel areas (responsive: <80 = terminal only, >=80 = 2-panel,
  >=120 = optional 3-panel). Widgets: `project_list` (session
  list with repo/branch display), `terminal_view`, `info_panel`,
  `status_bar`, `repo_picker_modal` (repo selection with
  worktree toggle). `selection.rs` handles mouse-drag text
  selection, `links.rs` detects clickable URLs for Ctrl+Click.
- **`mcp/`** — MCP server (`thurbox-mcp` binary). Exposes
  role/session/skill/profile/MCP-server CRUD over stdio or
  Streamable HTTP JSON-RPC. Shares the same SQLite database
  as the TUI.

### Event Loop (main.rs)

```text
tokio::main → init local-tmux backend → open SQLite DB
→ init terminal → spawn/restore sessions → loop {
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
- All state (sessions, roles) in SQLite:
  `~/.local/share/thurbox/thurbox.db` (XDG_DATA_HOME respected)
- Requires tmux >= 3.2

## Keybindings (Vim-Inspired)

Global keys use `Ctrl` + semantic Vim conventions:

| Key | Action | Mnemonic |
|-----|--------|----------|
| `Ctrl+Q` | Quit (detach sessions) | **Q**uit |
| `Ctrl+N` | New session (opens repo picker) | **N**ew |
| `Ctrl+C` | Copy selection / SIGINT (terminal) | **C**opy |
| `Ctrl+V` | Paste from clipboard | Paste |
| `Ctrl+P` | Scheduled commands (list/cancel/new) | **P**rogram |
| `Ctrl+T` | Toggle shell pane | **T**erminal |
| `Ctrl+H` | Focus previous pane (cycle backward) | Vim: **h** = left |
| `Ctrl+J` | Select next session | Vim: **j** = down |
| `Ctrl+K` | Select previous session | Vim: **k** = up |
| `Ctrl+L` | Focus next pane (cycle forward) | Vim: **l** = right |
| `Ctrl+D` | Delete session | Vim: **d** = delete |
| `Ctrl+E` | Edit settings (roles, MCP servers, skills) | **E**dit |
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
`~/.config/thurbox/keybindings.json` (or via the MCP
`set_keybindings` tool). The file maps an `Action` name to one or
more chord strings, e.g. `{ "QuitApp": ["ctrl+x"] }`. Modal-internal
keys (j/k/Enter/Esc inside selectors) are not customizable.

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
- `docs/MCP_ROLES.md` — MCP role configuration guide (permissions, tool patterns, examples)

**Rule**: If a code change invalidates or extends a documented
decision, update the relevant doc in the same PR.
