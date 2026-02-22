# Thurbox

A multi-session Claude Code orchestrator for your terminal.
Run parallel Claude instances in persistent tmux panes —
sessions survive crashes and restarts.

[![CI](https://github.com/Thurbeen/thurbox/workflows/CI/badge.svg)](https://github.com/Thurbeen/thurbox/actions)
[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](https://opensource.org/licenses/MIT)

![Thurbox Demo](./docs/media/thurbox-demo.gif)

## Features

### Session Orchestration

Run multiple Claude Code instances side-by-side, each in its
own tmux pane. Sessions persist across crashes, restarts, and
even multiple concurrent Thurbox instances — tmux keeps them
alive in the background. Restart a session with `Ctrl+R` to
pick up new role permissions while preserving conversation
history via `--resume`. Each session displays elapsed time
("Waiting 45s", "Idle 2m") and highlights clickable URLs in
terminal output. Recover sessions externally at any time with
`tmux -L thurbox attach`.

### Project Management

Organize work into projects, each with its own sessions and
settings. The two-section left sidebar shows all projects on top
and the active project's sessions below. Projects support
multiple repositories — the first repo becomes the working
directory and the rest are passed via `--add-dir`. Edit
projects on the fly with `Ctrl+E` (name, repos, roles, MCP
servers) without losing running sessions. Soft-deleted
projects and sessions can be restored via the Admin session
or MCP API. A built-in Admin project (pinned at index 0)
provides conversational access to Thurbox management via MCP.

### Git Worktree Support

Optionally spawn sessions inside git worktrees for branch
isolation. When creating a session (`Ctrl+N`), choose "Worktree"
mode to select a base branch and name a new branch — Thurbox
creates the worktree and launches Claude inside it. Press
`Ctrl+S` to sync all worktree sessions with `origin/main` —
on rebase conflicts, Thurbox automatically sends a resolution
prompt to Claude. Closing the session automatically removes
the worktree. Worktree sessions show the branch name in the
terminal title and session list.

### Role System

Define per-project permission profiles that control Claude's
behavior. Each role specifies a permission mode (`default`,
`plan`, `acceptEdits`, `dontAsk`, `bypassPermissions`), lists
of allowed and disallowed tools with scope patterns like
`Bash(git:*)`, and optional system prompt text. Manage roles
from the TUI via `Ctrl+E` or programmatically through the MCP
server. When a project has roles, a role selector appears at
session creation.

### MCP Server

The `thurbox-mcp` binary exposes 13 tools over the Model Context
Protocol for managing projects, roles, sessions, and MCP server
configurations. The Admin session auto-configures `thurbox-mcp`,
so you can manage everything conversationally — "create a
reviewer role for my-app with read-only access." See
[MCP Server](#mcp-server-1) below for the full tool reference.

### Responsive UI

Three layout tiers adapt to your terminal width:

| Width | Layout |
|-------|--------|
| < 80 cols | Terminal only |
| >= 80 cols | Project/session sidebar + terminal |
| >= 120 cols | Sidebar + terminal + info panel |

Scrollback with `Shift+arrows` / `PageUp` / `PageDown` / mouse
wheel. Non-modal error messages in the status bar. Vim-inspired
keybindings throughout.

## Installation

### From Binary (Recommended)

```bash
curl -fsSL https://raw.githubusercontent.com/Thurbeen/thurbox/main/scripts/install.sh | sh
```

This installs the latest release to `~/.local/bin`. The script
automatically detects your platform, verifies checksums, and
handles API rate limits gracefully.

**Custom installation directory:**

```bash
INSTALL_DIR=/usr/local/bin curl -fsSL https://raw.githubusercontent.com/Thurbeen/thurbox/main/scripts/install.sh | sh
```

**Specific version:**

```bash
VERSION=v0.1.0 curl -fsSL https://raw.githubusercontent.com/Thurbeen/thurbox/main/scripts/install.sh | sh
```

### From Source

```bash
git clone https://github.com/Thurbeen/thurbox.git
cd thurbox
cargo build --release
```

The binary will be available at `target/release/thurbox`.

## Prerequisites

- **tmux >= 3.2** — session backend
- **claude CLI** — [github.com/anthropics/claude-code](https://github.com/anthropics/claude-code)
- **git** — required for worktree features
- **Rust 1.75+** — only needed for building from source

## Quick Start

1. **Launch Thurbox** — run `thurbox` in your terminal. The Admin
   session appears automatically in the sidebar.
2. **Create a project** — press `Ctrl+N` with the project list
   focused. Enter a name and one or more repository paths.
3. **Create a session** — select your project, then press
   `Ctrl+N` again. Choose a session mode (Normal or Worktree)
   and optionally select a role.
4. **Work with Claude** — the terminal panel shows the live
   Claude Code session. All keys are forwarded to the PTY.
5. **Navigate** — `Ctrl+L` cycles focus (project list → session
   list → terminal). `Ctrl+H` jumps to the project list.
   `Ctrl+J` / `Ctrl+K` switch projects or sessions.
6. **Manage projects** — `Ctrl+E` edits the active project
   (name, repos, roles, MCP servers). `Ctrl+D` deletes a
   session or project.
7. **Restart a session** — `Ctrl+R` restarts with `--resume` to
   preserve conversation history while picking up new
   role permissions.
8. **Quit** — `Ctrl+Q` detaches all sessions (tmux keeps them
   running). They resume automatically on next launch.

## Keybindings

### Global Keys

| Key | Action | Mnemonic |
|-----|--------|----------|
| `Ctrl+Q` | Quit (detach sessions) | **Q**uit |
| `Ctrl+N` | New project or session | **N**ew |
| `Ctrl+C` | Close active session | **C**lose |
| `Ctrl+H` | Focus project list | Vim: **h** = left |
| `Ctrl+J` | Next project (project list) / session | Vim: **j** = down |
| `Ctrl+K` | Previous project (project list) / session | Vim: **k** = up |
| `Ctrl+L` | Cycle focus | Vim: **l** = right |
| `Ctrl+D` | Delete session or project | Vim: **d** = delete |
| `Ctrl+E` | Edit active project | **E**dit |
| `Ctrl+R` | Restart active session | **R**estart |
| `Ctrl+S` | Sync worktrees with origin/main | **S**ync |
| `Ctrl+Z` | Undo session/project delete | **Z** = undo |
| `Ctrl+U` | Restore deleted sessions | **U**ndelete |
| `F1` | Help overlay | Universal |
| `F2` | Toggle info panel | Next to F1 |

### List Navigation

| Key | Action |
|-----|--------|
| `j` / `Down` | Next item |
| `k` / `Up` | Previous item |
| `Enter` | Select / focus |

### Terminal Scrollback

| Key | Action |
|-----|--------|
| `Shift+Up` / `Shift+Down` | Scroll 1 line |
| `Shift+PageUp` / `Shift+PageDown` | Scroll half page |
| Mouse wheel | Scroll 3 lines |
| Any other key | Snap to bottom + forward to PTY |

## MCP Server

The `thurbox-mcp` binary exposes Thurbox configuration over the
Model Context Protocol via stdio transport. It shares the same
SQLite database as the TUI — changes appear immediately.

```bash
cargo build --bin thurbox-mcp
```

### Available Tools

| Tool | Description |
|------|-------------|
| `list_projects` | List all active projects |
| `get_project` | Get a project by name or UUID |
| `create_project` | Create a new project with name and repo paths |
| `update_project` | Update project name and/or repos |
| `delete_project` | Soft-delete a project |
| `list_roles` | List all roles for a project |
| `set_roles` | Atomically replace all roles for a project |
| `list_mcp_servers` | List MCP servers for a project |
| `set_mcp_servers` | Set MCP servers for a project |
| `list_sessions` | List sessions, optionally filtered by project |
| `get_session` | Get a session by UUID |
| `delete_session` | Soft-delete a session |
| `restart_session` | Queue a session restart |

### Admin Session

The TUI includes a built-in Admin session that auto-configures
`thurbox-mcp` as an MCP server. Claude Code discovers the config
automatically, enabling conversational project/role/session
management inside the TUI. The Admin project is pinned at
index 0 and cannot be edited or deleted.

For the complete role configuration guide including permission
modes, tool name format, and example role patterns, see
[docs/MCP_ROLES.md](docs/MCP_ROLES.md).

## Architecture

Thurbox follows **The Elm Architecture** (TEA):
`Event → Message → update(model, msg) → view(model) → Frame`.
All state lives in a single `App` model. Sessions run inside a
dedicated tmux server (`tmux -L thurbox`), with terminal output
parsed by `vt100::Parser` and rendered by `tui_term`. All
persistent state (projects, sessions, roles) is stored in SQLite.

### Module Dependency Rules

```text
session  ← pure data types, no project-local imports
project  ← imports session only
claude   ← imports session only (NEVER ui, git, or project)
ui       ← imports session and project only (NEVER claude or git)
mcp      ← imports storage, session, project, sync, paths only
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
  choices
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
