# Thurbox

An agentic IDE and agent orchestrator for your terminal.
Run parallel Claude Code instances in persistent tmux panes —
and let one of them drive the others. Sessions, roles, worktrees,
skills, and MCP servers are first-class citizens.

[![CI](https://github.com/Thurbeen/thurbox/workflows/CI/badge.svg)](https://github.com/Thurbeen/thurbox/actions)
[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](https://opensource.org/licenses/MIT)
[![Website](https://img.shields.io/badge/Website-thurbox.thurbeen.eu-blue)](https://thurbox.thurbeen.eu/)
[![Quality Gate Status](https://sonarcloud.io/api/project_badges/measure?project=Thurbeen_thurbox&metric=alert_status)](https://sonarcloud.io/summary/new_code?id=Thurbeen_thurbox)

![Thurbox Demo](./docs/media/thurbox-demo.gif)

## Why Thurbox

Running `claude` in several terminals gets you far — until you want
to keep sessions alive across crashes, isolate them per-branch, or
have one Claude delegate work to others. Thurbox adds:

- **Persistence** — sessions live in tmux and survive Thurbox
  crashes, restarts, and reboots. Reattach from any terminal with
  `tmux -L thurbox attach`.
- **Parallelism** — many Claudes side-by-side, each with its own
  repo(s), branch, role, and skills.
- **Git worktree isolation** — each session can spawn on a fresh
  worktree; `Ctrl+S` syncs them with `origin/main` and asks Claude
  to resolve rebase conflicts automatically.
- **Orchestrator mode** — a built-in Admin session uses the
  `thurbox-mcp` server to `create_session`, `send_prompt`, and
  `capture_session_output` on other sessions. Say "spawn a reviewer
  on the api repo and audit auth.rs" and Thurbox coordinates it.

## Main Features

### Sessions

- Persistent tmux-backed panes, parallel Claudes, per-session
  working dirs (multi-repo via `--add-dir`).
- `Ctrl+R` restart with `--resume` preserves conversation; `Ctrl+F`
  forks a session; `Ctrl+T` toggles a shell pane.
- Fuzzy session search (`/`), clickable URLs, mouse selection +
  clipboard, scheduled commands (`Ctrl+P`), soft-delete with undo
  (`Ctrl+Z`) and restore (`Ctrl+U`).

### Git worktrees

- Pick "Worktree" in the new-session flow to branch off a base and
  launch Claude inside the worktree. Closing the session removes
  it. `Ctrl+S` syncs all worktree sessions with `origin/main`.

### Roles & skills

- Global role presets define permission mode and allowed/disallowed
  tools (e.g. `Bash(git:*)`). Skills are symlinked into the
  session's `.claude/skills/` and auto-discovered. Edit everything
  with `Ctrl+E`.

### Orchestrator mode

- The Admin session auto-configures `thurbox-mcp` and can spawn,
  prompt, and observe other sessions — a multi-agent loop driven
  conversationally. See
  [docs/FEATURES.md#orchestrator-mode](docs/FEATURES.md#orchestrator-mode).

### MCP server (`thurbox-mcp`)

- 24 tools over stdio or Streamable HTTP for roles, sessions,
  scheduled commands, editor config, and more. Shares the TUI's
  SQLite DB so changes appear live.

### Responsive UI

- `< 80` cols: terminal only · `>= 80`: sidebar + terminal ·
  `>= 120`: sidebar + terminal + info panel. Vim-inspired keys
  throughout.

## Prerequisites

- **tmux >= 3.2**
- **claude CLI** — [anthropics/claude-code](https://github.com/anthropics/claude-code)
- **git** (required for worktree features)
- **Rust 1.75+** (only to build from source)

## Installation

**One-liner (recommended):**

```bash
curl -fsSL https://raw.githubusercontent.com/Thurbeen/thurbox/main/scripts/install.sh | sh
```

Installs the latest release to `~/.local/bin` with checksum
verification and platform auto-detection.

**Options:**

```bash
# Custom directory
INSTALL_DIR=/usr/local/bin curl -fsSL https://raw.githubusercontent.com/Thurbeen/thurbox/main/scripts/install.sh | sh

# Pin a version
VERSION=v0.1.0 curl -fsSL https://raw.githubusercontent.com/Thurbeen/thurbox/main/scripts/install.sh | sh
```

**From source:**

```bash
git clone https://github.com/Thurbeen/thurbox.git
cd thurbox
cargo build --release
# binary at target/release/thurbox
```

## Getting Started

1. **Launch** — run `thurbox`. You'll see a sidebar on the left
   with a pinned **Admin** session at index 0 (that's your
   conversational control plane), and an empty terminal on the
   right.
2. **Create your first session** — press `Ctrl+N` to open the repo
   picker. Toggle repos with `Space`; press `w` on a repo to mark
   it as worktree mode (you'll be prompted for a base branch and
   new branch name). Confirm with `Enter`, then optionally pick a
   role, MCP servers, and skills.
3. **Work with Claude** — the right pane is a live Claude session.
   All keys are forwarded to the PTY; `Ctrl+C` copies if you have
   a selection, otherwise sends SIGINT.
4. **Navigate** — `Ctrl+J` / `Ctrl+K` move between sessions in the
   sidebar; `Ctrl+L` / `Ctrl+H` cycle focus between panes.
   `Ctrl+O` opens the session's worktree in your editor; `Ctrl+E`
   edits global settings.
5. **Quit without killing** — `Ctrl+Q` detaches all sessions.
   Tmux keeps them running; relaunch `thurbox` and they resume.

See the full [keybindings](#keybindings) below.

## Orchestrator Mode in 30 Seconds

The Admin session is already wired up to `thurbox-mcp`. Focus it
and ask:

> Spawn a worker session on the `api` repo using the `reviewer`
> role, tell it to audit `src/auth.rs` for missing permission
> checks, wait for it to finish, and summarize its findings.

Under the hood the Admin Claude calls `create_session` → polls
`get_session` until `Idle` → `send_prompt` → `capture_session_output`.
You'll see the new session appear in the sidebar; you can watch
it work or ignore it until Admin reports back. Full details:
[docs/FEATURES.md#orchestrator-mode](docs/FEATURES.md#orchestrator-mode).

## Common Workflows

- **Parallel branches** — `Ctrl+N`, pick a repo in Worktree mode,
  name a new branch. Repeat for a second branch. Two isolated
  Claudes now work in parallel with no git contention.
- **Conversational admin** — ask the Admin session "create a
  `reviewer` role with read-only Bash and no Edit/Write". It calls
  `set_roles` for you; the role appears in the `Ctrl+E` picker.
- **Recover a crash** — if Thurbox dies, relaunch it: sessions
  resume from tmux. Prefer raw tmux? `tmux -L thurbox attach`.

## Keybindings

### Global Keys

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
| `Ctrl+O` | Open active session's repos in editor | **O**pen |
| `Ctrl+R` | Restart active session | **R**estart |
| `Ctrl+F` | Fork active session | **F**ork |
| `Ctrl+S` | Sync worktrees with origin/main | **S**ync |
| `Ctrl+Z` | Undo session delete | **Z** = undo |
| `Ctrl+U` | Restore deleted sessions | **U**ndelete |
| `F1` | Help overlay | Universal |
| `F2` | Toggle info panel | Next to F1 |

### List Navigation

| Key | Action |
|-----|--------|
| `j` / `Down` | Next item |
| `k` / `Up` | Previous item |
| `/` | Fuzzy search (projects or sessions) |
| `Enter` | Select / focus |

Session search matches against name, role, and branch.

### Terminal Scrollback and Selection

| Key | Action |
|-----|--------|
| `Shift+Up` / `Shift+Down` | Scroll 1 line |
| `Shift+PageUp` / `Shift+PageDown` | Scroll half page |
| Mouse wheel | Scroll 3 lines |
| Mouse drag | Select text |
| Any other key | Snap to bottom + forward to PTY |

## MCP Server

The `thurbox-mcp` binary exposes Thurbox configuration over the
Model Context Protocol. It supports stdio (default) and
Streamable HTTP transports, and shares the same SQLite database
as the TUI — changes appear immediately.

```bash
cargo build --bin thurbox-mcp
thurbox-mcp                                    # stdio (default)
thurbox-mcp --transport streamable-http        # HTTP on 127.0.0.1:8080
thurbox-mcp --transport streamable-http --port 9090  # custom port
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
| `create_session` | Spawn a new local-tmux session (optionally on a fresh worktree) |
| `send_prompt` | Send text to a session's terminal immediately (orchestrator mode) |
| `capture_session_output` | Read rendered pane contents from a session |
| `delete_session` | Soft-delete a session (TUI cleans up tmux/worktree) |
| `restart_session` | Queue a session restart (TUI processes the command) |
| `restore_session` | Restore a soft-deleted session |
| `schedule_command` | Schedule text to be sent to a session at a future time |
| `list_scheduled_commands` | List pending scheduled commands, optionally by session |
| `get_scheduled_command` | Get a scheduled command by ID |
| `cancel_scheduled_command` | Cancel a pending scheduled command |
| `get_editor_command` | Get the editor command used by Ctrl+O |
| `set_editor_command` | Set the editor command used by Ctrl+O |

### Admin Session

The TUI includes a built-in Admin session that auto-configures
`thurbox-mcp` as an MCP server. Claude Code discovers the config
automatically, enabling conversational role/session management
inside the TUI. The Admin session is pinned at index 0 and
cannot be edited or deleted.

For the complete role configuration guide including permission
modes, tool name format, and example role patterns, see
[docs/MCP_ROLES.md](docs/MCP_ROLES.md).

## Architecture

Thurbox follows **The Elm Architecture** (TEA):
`Event → Message → update(model, msg) → view(model) → Frame`.
All state lives in a single `App` model. Sessions run via a
`SessionBackend` trait — the only backend is local tmux
(`tmux -L thurbox`). Terminal output is parsed by
`vt100::Parser` and rendered by `tui_term`. All persistent
state (sessions, roles, MCP servers, skills) is stored in SQLite.

### Module Dependency Rules

```text
session  ← pure data types, no local imports
agent    ← imports session only (NEVER ui or git)
ui       ← imports session only (NEVER agent or git)
mcp      ← imports storage, session, sync, paths only
app      ← coordinator, imports all modules
```

These rules are enforced by `tests/architecture_rules.rs`.
For the full set of architectural decisions with rationale,
see [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md).

## Documentation

- [docs/CONSTITUTION.md](docs/CONSTITUTION.md) — Core principles
  and non-negotiable rules
- [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) — Architectural
  decisions with rationale
- [docs/FEATURES.md](docs/FEATURES.md) — Feature-level design
  choices (including orchestrator mode)
- [docs/MCP_ROLES.md](docs/MCP_ROLES.md) — MCP role
  configuration guide

## Development

### Setup

```bash
git clone https://github.com/Thurbeen/thurbox.git
cd thurbox
prek install   # Install pre-commit hooks
```

All required dev tools are documented in `Cargo.toml` under
`[package.metadata.dev-tools]`. Run `./scripts/install-dev-tools.sh`
to install them, or install individually with `cargo install`.

### Build and Run

```bash
cargo build                          # Debug build
cargo build --release                # Release build (LTO, stripped)
cargo run                            # Run in dev mode
```

### Testing

```bash
cargo nextest run --all              # Run all tests (preferred)
cargo nextest run -E 'test(name)'    # Single test by name
cargo test --test architecture_rules # Architecture validation
bats scripts/install.bats            # Install script tests
```

### Code Quality

```bash
cargo fmt --all                      # Format (100 char max)
cargo clippy --all-targets --all-features -- -D warnings
RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --all-features
rumdl check .                        # Markdown lint
```

### Architecture Checks

```bash
cargo test --test architecture_rules # Module dependency rules
cargo deny check advisories          # Security advisories
cargo deny check bans licenses sources  # Dependency policy
```

## Committing Changes

This project uses
[Conventional Commits](https://www.conventionalcommits.org/).

```bash
cog commit feat "add worktree management"
cog commit fix "resolve memory leak" api
```

### Commit Types

- `feat`: New features (minor version bump)
- `fix`: Bug fixes (patch version bump)
- `docs`, `refactor`, `test`, `chore`, `perf`, `ci`, `style`,
  `build`, `revert`: No release

### Valid Scopes

`api`, `cli`, `ui`, `git`, `core`, `docs`, `deps`, `config`,
`mcp`

## Contributing

1. Fork the repository
2. Create a feature branch
3. Make your changes following our coding standards
4. Write tests for new functionality
5. Ensure all tests pass: `cargo nextest run --all`
6. Use conventional commits: `cog commit <type> "message"`
7. Submit a pull request

### Code Style

- Follow Rust naming conventions
- Maximum line width: 100 characters
- Use `rustfmt` for formatting
- Address all `clippy` warnings

## License

This project is licensed under the MIT License - see the
[LICENSE](LICENSE) file for details.

## Acknowledgments

- [Ratatui](https://github.com/ratatui-org/ratatui) — TUI
  framework
- [tui-term](https://github.com/a-kenji/tui-term) — terminal
  widget for ratatui
- [vt100](https://github.com/doy/vt100-rust) — terminal
  emulation
- [Claude Code CLI](https://github.com/anthropics/claude-code)
  — AI coding assistant
- [rmcp](https://github.com/anthropics/rmcp) — Rust MCP SDK
- [tmux](https://github.com/tmux/tmux) — terminal multiplexer

## Support

For issues, questions, or contributions, please visit our
[GitHub repository](https://github.com/Thurbeen/thurbox).
