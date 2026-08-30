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
just lint                            # fmt-check + clippy + deny + rumdl + shellcheck + the 3 Lua gates

cargo check --all                    # Type check (bare cargo still works)
cargo build --release                # Release build (LTO, stripped)
```

To **run thurbox in an isolated sandbox** use `scripts/dev/sandbox.sh` (a.k.a.
`just sandbox*`). By default it does **thurbox-only isolation**: redirects only
thurbox's config/data into the sandbox (via the `THURBOX_CONFIG_DIR`/
`THURBOX_DATA_DIR` overrides paths.rs honors) while keeping your real `HOME` —
so your authenticated agent CLIs (claude/codex/…) work — and puts dev
`target/debug` first on PATH so an agent hook's `thurbox-cli` hits the sandbox DB.
It also names the sandbox's tmux socket outright (`THURBOX_SOCKET`, `=
$TBX_DEV_SOCKET`): a relocated `THURBOX_DATA_DIR` otherwise derives one of its
own, and teardown kills the socket *by name*.

```bash
scripts/dev/sandbox.sh               # persistent "default" profile, launch the TUI
scripts/dev/sandbox.sh --fresh       # throwaway env, wiped on exit
scripts/dev/sandbox.sh --isolate-home    # full hermetic isolation (fresh HOME; agents have no creds)
scripts/dev/sandbox.sh --shell       # shell with the sandbox env (run thurbox-cli by hand)
scripts/dev/sandbox.sh -- session list   # run a thurbox-cli command in the sandbox
scripts/dev/sandbox.sh --clean       # wipe the persistent profile
```

The TUI is launched **from the sandbox root rather than the repo**:
the sandbox sets `THURBOX_CONFIG_DIR`, so the interface materialises at
`<sandbox>/thurbox-config/ui/` along with everything else and `--fresh` gives you a
clean one per run. (This used to matter more: `resolve_ui_dir` preferred a `./ui` in
the working directory, so a sandbox started from the repo isolated the database but
not the interface. That rule is gone, and the `cd` is now belt-and-braces.)

The isolation lives in one helper, `scripts/dev/lib/sandbox-env.sh`
(`tbx_sandbox_init` = thurbox-only, `tbx_sandbox_init_full` = full HOME/XDG),
sourced by the sandbox entrypoint plus `scripts/demo/record.sh` (which uses the
full flavor). Single source of truth for the `thurbox-dev` sandbox pattern;
`tests/tui_e2e.rs` isolates the same way in Rust.

## Working reference (skills)

The per-subsystem reference that used to live in this file is now **eleven
skills** under `.claude/skills/`, loaded on demand instead of on every turn.
Each carries its subject verbatim, so a section named elsewhere in the repo
("the *Agent Definitions* section of CLAUDE.md") is now the skill on this list
that names it. Read the one your change touches:

| Skill | Owns (the sections that moved) |
|---|---|
| `thurbox-testing` | Testing · Kernel and interface tests · Session-backend e2e harnesses |
| `thurbox-performance` | Performance (render loop) |
| `thurbox-release` | Release Process · Distribution Packages · Installation Script |
| `thurbox-agents` | Agent Definitions · Multi-repo sessions |
| `thurbox-remote-hosts` | Remote SSH & WSL Sessions |
| `thurbox-cli` | thurbox-cli · lifecycle hooks · parent sessions · ordering · messages · Tasks |
| `thurbox-extensions` | Extensions · Extension manifests + self-heal |
| `thurbox-session-status` | Session status (hooks-driven) · OS notifications |
| `thurbox-kernel` | Architecture (plugin kernel) · Writing an interface plugin |
| `thurbox-ui-surfaces` | Keybindings · Themes · Settings panel · Global search · Code review |
| `thurbox-demo-media` | Demo Video |

Two more are unrelated to this split and predate it: `ui-review` (screenshot
the TUI and critique it) and `thurbox-ui` (edit the *running* interface's Lua,
installed by the `ui-skill` extension into each coding CLI).

A skill is a working reference, not an owner: the docs under `docs/` still own
the rationale, and the **Rule** at the bottom of this file applies to a skill
too — a change that invalidates what one says updates it in the same PR.

## Linting & Formatting

```bash
cargo fmt --all                      # Format (rustfmt: 100 char max)
cargo clippy --all-targets --all-features -- -D warnings  # Lint
RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --all-features  # Docs
rumdl check .                        # Markdown lint (.rumdl.toml)
rumdl fmt .                          # Markdown auto-fix
selene ui                            # Lua lint (selene.toml + thurbox.yml)
stylua ui                            # Lua format (stylua.toml); --check in CI
# A RELATIVE --configpath resolves against the server's own install dir, is
# silently not found, and then reports every injected global as undefined (79
# phantom findings). `just lint` passes an absolute one:
lua-language-server --check ui --configpath "$PWD/.luarc.json" --checklevel=Warning
```

Three tools on `ui/`, chosen to match what the Lua ecosystem actually gates on —
**stylua** and **lua-language-server** are what neovim's own lint job runs. Each
covers a different half of the sandbox, and both halves matter:

| tool | catches | enforces absence of |
|---|---|---|
| `selene` | undefined variables, shadowing, `thurbox.*` typos | `print`, `dofile`, `load*` (base functions) |
| `lua-language-server` | type errors, undefined fields, unused locals | `os`, `io`, `debug`, `package` (libraries) |
| `stylua` | formatting | — |

The split is not redundancy: selene's `removed:` works on plain functions but not
on a table's fields, and luals' `runtime.builtin` disables whole libraries but
cannot drop a single base function. Verified by probing every withheld capability
against both.

**`thurbox.yml` is the plugin sandbox, checked statically.** It is selene's
standard library for `ui/`, and it deliberately declares **no `base:`** — it lists
only what `kernel::host::plugin_stdlib` grants (`string`, `table`, `math`,
`coroutine`, `utf8`) plus the six globals `install_api` injects. So `os`, `io`,
`debug`, `package`, `print` and the loaders are *absent* rather than marked
removed, which is the same shape the VM enforces and means a plugin
reaching for one fails lint instead of failing at runtime. Inheriting a base and
marking things `removed` does **not** work: selene applies that to plain functions
but not to a table's fields, so `os.time()` passed review while `dofile` was
caught.

It also declares the published shape of `thurbox`, so `thurbox.sesions` is a lint
error rather than a silently-nil pane. Keep it in step with `LuaHost::publish`;
a newly published field used by a plugin fails lint until it is added.

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

## Conventional Commits

All commits must follow
[Conventional Commits](https://www.conventionalcommits.org/).
Enforced by cocogitto via pre-commit hooks.

- **Types**: feat, fix, perf, refactor, docs, style, test,
  chore, ci, build, revert
- **Scopes**: api, cli, ui, git, core, docs, deps, config, mcp
- Use `cog commit feat "message"`
  or `cog commit fix "message" scope`

## Module Dependency Rules (enforced by tests/architecture_rules.rs)

```text
session  ← pure data types, no crate-internal references
agent    ← session (+ paths/shell utils; NEVER git)
kernel   ← session + storage + sync + paths + session_ops + git
           (+ agent/usage by fully-qualified path only)
main     ← the coordinator: the loop, the workers, the chrome
```

Enforcement is an **allowlist**: every module under `src/` needs a `ModuleRules`
entry naming what it may reference in *any* form, so a new module fails the test
until its place is declared. The full rule, the module responsibilities and the
event loop are in the `thurbox-kernel` skill.

## Pre-commit Hooks

20 hooks run automatically via `prek` (Rust-based pre-commit
framework). Install with `prek install`. Stages:

- **commit-msg**: conventional commit validation (`cog verify`)
- **pre-commit**: fmt, clippy, check, nextest, architecture,
  deny, doc, bats (install script + the extensions' shell scripts,
  one hook each), shellcheck, rumdl, selene, stylua, prettier,
  htmlhint, stylelint, eslint
- **pre-push**: commit history check (`cog check`)

Each bats hook has a CI twin (`install-script`,
`extension-script-tests`), so a suite that guards a script is
actually run rather than merely present.

Shell scripts are linted with **shellcheck** (config in
`.shellcheckrc`); install it from your package manager (it is not a
cargo crate — `scripts/install-dev-tools.sh` prints a reminder).

## Key Technical Details

- MSRV: 1.75, Edition 2021
- Async runtime: tokio (multi-threaded)
- Session backend: `TmuxBackend` over a `TmuxTransport`
  (local `tmux -L thurbox`, or `ssh <dest> tmux …` for
  `ssh:<host>` backends from `hosts.toml`). The local socket is
  `thurbox`/`thurbox-dev` only for an instance on the **default** data dir; one
  relocated by `THURBOX_DATA_DIR` derives its own (`thurbox-<digest>`) so it
  never creates windows on the operator's server, and `THURBOX_SOCKET`
  overrides both. `thurbox-cli version --json` reports the name in force —
  ADR-12, `docs/CONFIG.md` → Relocating an instance
- Output reader runs in `tokio::task::spawn_blocking`
  (blocking I/O), writer in `tokio::spawn` (async)
- Terminal state parsed by `vt100::Parser`,
  rendered by `tui_term::PseudoTerminal`
- Sessions persist across restarts (tmux keeps them alive)
- Session state in SQLite:
  `~/.local/share/thurbox/thurbox.db` (XDG_DATA_HOME respected);
  agent definitions in `~/.config/thurbox/agents.toml`;
  remote SSH hosts in `~/.config/thurbox/hosts.toml`;
  session lifecycle hooks in `~/.config/thurbox/hooks.toml`
- Requires tmux >= 3.2

## Design Documentation

For rationale behind decisions, see `docs/`:

- `docs/TUTORIAL.md` — The onboarding walkthrough (screenshots generated by
  `scripts/demo/record-tutorial.sh`; re-record when a step's screen changes)
- `docs/CONSTITUTION.md` — Core principles and non-negotiable rules
- `docs/ARCHITECTURE.md` — Architectural decisions with rationale
- `docs/FEATURES.md` — Feature-level design choices
- `docs/CONFIG.md` — Thurbox's own config files/env vars/DB settings in one place
- `docs/AGENTS.md` — Each built-in agent's exact config + behavior, and
  the checklist for adding a new built-in
- `docs/PERFORMANCE.md` — Render/tick performance: demand-driven redraw,
  perf counters, the session-order cache, and how to measure

**Rule**: If a code change invalidates or extends a documented
decision, update the relevant doc in the same PR.
