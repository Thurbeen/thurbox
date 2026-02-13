# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code)
when working with code in this repository.

## Project

Thurbox is a multi-session Claude Code TUI orchestrator built
with Rust. It runs multiple `claude` CLI instances inside PTYs,
rendered as terminal panels via ratatui + tui-term.

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
```

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
cargo modules structure --lib                             # Module viz
cargo +nightly-2026-01-22 pup check --pup-config pup.ron  # Arch rules
cargo deny check advisories                               # Advisories
cargo deny check bans licenses sources                    # Dep policy
cargo audit                                               # Audit
```

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

### Module Dependency Rules (enforced by pup.ron)

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
- **`claude/`** — Side-effect layer. `PtySession` spawns `claude`
  CLI in PTY via `portable-pty`, reads output into
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
tokio::main → load project config → init terminal
→ spawn initial session → loop {
    draw frame → poll crossterm events (10ms)
    → convert to AppMessage → app.update() → app.tick()
} → app.shutdown() → restore terminal
```

- Logging goes to `~/.local/share/thurbox/thurbox.log`
  (file-based, since stdout is owned by the TUI)
- Panic hook restores terminal before printing

## Pre-commit Hooks

12 hooks run automatically via `prek` (Rust-based pre-commit
framework). Install with `prek install`. Stages:

- **commit-msg**: conventional commit validation (`cog verify`)
- **pre-commit**: fmt, clippy, check, nextest, modules, pup,
  deny, audit, doc, rumdl
- **pre-push**: commit history check (`cog check`)

## Key Technical Details

- MSRV: 1.75, Edition 2021
- Async runtime: tokio (multi-threaded)
- PTY reader runs in `tokio::task::spawn_blocking` (blocking I/O),
  writer in `tokio::spawn` (async)
- Terminal state parsed by `vt100::Parser`,
  rendered by `tui_term::PseudoTerminal`
- `Ctrl+Q` is the quit key
  (only key not forwarded to PTY when terminal is focused)
- Config file: `~/.config/thurbox/config.toml`
  (XDG_CONFIG_HOME respected)
- Architecture tests in `tests/architecture_rules.rs` are
  `#[ignore]` due to upstream cargo-pup bug with workspace detection

## Design Documentation

For rationale behind decisions, see `docs/`:

- `docs/CONSTITUTION.md` — Core principles and non-negotiable rules
- `docs/ARCHITECTURE.md` — Architectural decisions with rationale
- `docs/FEATURES.md` — Feature-level design choices

**Rule**: If a code change invalidates or extends a documented
decision, update the relevant doc in the same PR.
