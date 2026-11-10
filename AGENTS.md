# AGENTS.md

This file provides guidance to coding agents working in this repository.

## Project

Thurbox is a multi-session coding-agent TUI orchestrator built
with Rust. It runs multiple coding-agent CLI instances (Claude
Code and Codex, through Fleet's harness profiles) inside
persistent tmux sessions, rendered as terminal
panels via ratatui + tui-term. Sessions survive crashes/restarts
because tmux keeps the processes alive.

Each session is a Fleet Task (ADR-41), and the agent it runs is the Task's
harness: one of the harness profiles Fleet has, which launch, resume and watch
the agent's CLI. Thurbox is agent-neutral: it keeps no agent registry and
knows nothing about any agent's model, permissions, prompts or tools; it offers
the harnesses `fleet version --json` lists.

## Repository layout

The repository is one Cargo workspace (ADR-41). Each component is a crate under
`crates/` — `crates/thurbox`, the interface, `crates/fleet-*`, the engine, and
`crates/runtime-mux`, the multiplexer runtime — and everything that is not a
crate stays at the root: `ui/`, `examples/`, `vendor/`, `docs/`, `scripts/`.
Fleet's own trees sit in a `fleet/` directory there (`examples/fleet/`,
`scripts/fleet/`, `docs/fleet/`).
`crates/architecture.toml` is the crate graph `fleet-arch` checks: a component
other than Fleet is `role = "component"`, and no component depends on another
(thurbox still links `runtime-mux` until Fleet drives the runtimes).
A bare `src/…` or `tests/…` path in this file, a skill or a document under
`docs/thurbox/` is the thurbox crate's (`crates/thurbox/src/…`);
`tests/doc_references.rs` resolves documents' `src/` paths there.

## Fleet

Fleet's crates keep Fleet's rules (the [Fleet docs](docs/fleet/README.md)
and the `fleet-*` skills own the detail):

- `fleet-api` and `fleet-controllers` are pure; adapters (the store, the
  runtime) perform I/O through ports; `fleet-exec` is the only process
  spawner; the manager writes under the lease; the CLI parses, wires and
  prints. `crates/architecture.toml` declares each crate's dependencies,
  status writers and effect boundaries: add a new crate there in the same
  change.
- TaskController owns Task status, except `status.result`, which the worker
  writes through `fleet result`. Unknown observations never mean absence.
  Persist an effect's intent before running it.
- Fleet's crates are edition 2024 and build on the pinned toolchain
  (`rust-toolchain.toml`); thurbox keeps edition 2021 and its own MSRV.
- `cargo xtask release` (`crates/fleet-dev`) is Fleet's release tooling; its
  workflows are a test fixture under `crates/fleet-dev/tests/fixtures/` until
  Fleet's release joins this repository's pipeline. The repository's gate
  (`.publish.yaml`, CI) covers Fleet's crates like every other.
- Fleet reaches sessions through the runtime protocol
  (`docs/runtime/README.md`): `[runtimes.<name>]` names a program,
  `[hosts.<name>]` the runtime serving a host, and `fleet-runtime`'s `exec`
  kind runs `<program> <verb>` through `fleet-exec`. Fleet and a runtime share
  no Rust type: `fleet runtime check -- <program>` (the conformance suite,
  `fleet_runtime::exec::check`) and `crates/fleet-cli/tests/e2e_runtime_exec.rs`,
  which drives the real `runtime-mux` binary on a private tmux socket
  directory, hold both sides to one protocol. Those tests skip without tmux or
  without a built `runtime-mux` (`cargo build -p runtime-mux`, or
  `RUNTIME_MUX_BIN`); CI's `runtime-protocol` job has both. The `exec` kind
  starts the harness profile's argv (`fleet_api::harness`) and learns what
  only the agent knows from its `fleet signal` reports, never from the
  runtime.
- A test that runs `git` in a scratch directory removes git's location
  variables (`GIT_DIR`, `GIT_WORK_TREE`, … — `GIT_LOCATION` in
  `crates/fleet-runtime/src/worktree.rs`) from the child: a commit hook
  exports them, and an unscrubbed `git config` rewrites this repository's.
- Design pages follow ASD-STE100 Simplified Technical English's writing rules:
  one fact per sentence, active voice, one term per concept, lists for steps.

## Build & Development Commands

The reproducible dev environment is a **Nix flake** (`flake.nix`, pins the Rust
toolchain + tmux/shellcheck/node/cargo-tools/just/demo stack) — enter it with
`nix develop` (or `direnv allow` once; see `.envrc`). Non-Nix fallback:
`scripts/install-dev-tools.sh`. Task entrypoint is **`just`** (`justfile`); full
guide in **`docs/thurbox/development.md`**.

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
`THURBOX_DATA_DIR` overrides `src/paths/` honors) while keeping your real `HOME` —
so your authenticated agent CLIs (claude/codex/…) work — and puts dev
`target/debug` first on PATH, so `fleet` and `runtime-mux` are this checkout's.
It gives the sandbox a Fleet of its own (`<sandbox>/fleet/config.toml`, exported as
`FLEET_CONFIG`), so its sessions are Tasks in the sandbox's store, never yours.
It also names the sandbox's tmux socket outright (`THURBOX_SOCKET`, `=
$TBX_DEV_SOCKET`): a relocated `THURBOX_DATA_DIR` otherwise derives one of its
own, and teardown kills the socket *by name*.

```bash
scripts/dev/sandbox.sh               # persistent "default" profile, launch the TUI
scripts/dev/sandbox.sh --fresh       # throwaway env, wiped on exit
scripts/dev/sandbox.sh --isolate-home    # full hermetic isolation (fresh HOME; agents have no creds)
scripts/dev/sandbox.sh --shell       # shell with the sandbox env (run thurbox-cli by hand)
scripts/dev/sandbox.sh -- config show   # run a thurbox-cli command in the sandbox
scripts/dev/sandbox.sh --clean       # wipe the persistent profile
```

The TUI is launched **from the sandbox root rather than the repo**:
the sandbox sets `THURBOX_CONFIG_DIR`, so the interface materialises at
`<sandbox>/thurbox-config/ui/` along with everything else and `--fresh` gives you a
clean one per run. (This used to matter more: `resolve_ui_dir` preferred a `./ui` in
the working directory, so a sandbox started from the repo isolated the data but
not the interface. That rule is gone, and the `cd` is now belt-and-braces.)

The isolation lives in one helper, `scripts/dev/lib/sandbox-env.sh`
(`tbx_sandbox_init` = thurbox-only, `tbx_sandbox_init_full` = full HOME/XDG),
sourced by the sandbox entrypoint plus `scripts/demo/record.sh` (which uses the
full flavor). Single source of truth for the `thurbox-dev` sandbox pattern;
`tests/tui_e2e.rs` isolates the same way in Rust.

## Working reference (skills)

The per-subsystem reference that used to live in this file is now **nine
skills** under `.agents/skills/`, loaded on demand instead of on every turn.
Every skill's body lives there, agent-neutrally; `.claude/skills/<name>` is a
relative symlink into it, so Claude Code and opencode read the one copy.
Each carries its subject verbatim, so a section named elsewhere in the repo
("the *Agent Definitions* section of AGENTS.md") is now the skill on this list
that names it. Read the one your change touches:

| Skill | Owns (the sections that moved) |
|---|---|
| `thurbox-testing` | Testing · Kernel and interface tests · Session-backend e2e harnesses |
| `thurbox-performance` | Performance (render loop) |
| `thurbox-release` | Release Process · Distribution Packages · Installation Script |
| `thurbox-remote-hosts` | Remote SSH & WSL Sessions |
| `thurbox-cli` | thurbox-cli · its subcommands · where the session verbs went |
| `thurbox-session-status` | Session status (from Fleet) · OS notifications |
| `thurbox-kernel` | Architecture (plugin kernel) · Writing an interface plugin |
| `thurbox-ui-surfaces` | Keybindings · Themes · Settings panel · Global search · Code review |
| `thurbox-demo-media` | Demo Video |

The others are unrelated to this split: `ui-review` (screenshot the TUI and
critique it), `thurbox-session` (inspect or drive one thurbox session) and the
`fleet-*` skills, which belong to Fleet.

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
selene ui examples                   # Lua lint (selene.toml + thurbox.yml)
stylua ui examples                   # Lua format (stylua.toml); --check in CI
# A RELATIVE --configpath resolves against the server's own install dir, is
# silently not found, and then reports every injected global as undefined (79
# phantom findings). `just lint` passes an absolute one:
lua-language-server --check ui --configpath "$PWD/.luarc.json" --checklevel=Warning
scripts/ci/check-lua-types.sh        # the definitions' own test (see below)
scripts/ci/check-lua-std.sh          # the standard library's own test (see below)
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

**`ui/lib/thurbox.d.lua` is the API as types**, and is what gives luals something
to check names against: node props and their allowed values, `ctx`, the
`hit`/`key`/`wheel` payloads, the declaration table, every published `thurbox.*`
row, every `command` verb's options, and the theme roles. Declarations only —
nothing loads it into the VM, which is why `selene.toml` excludes it (describing a
global means assigning one, the single thing the sandbox forbids a plugin).

`.luarc.json` names it in `workspace.library` for an editor opened at the
repository root. `--check ui` does not need that entry — luals loads a `---@meta`
file that is inside the workspace it is checking — which is also why the file
ships in `BUNDLED` and works in a user's own interface directory with no config
at all. The relative library path resolves against the workspace ROOT, not the
working directory, so it is spelled for the root the repository is opened at.

luals will **not** flag an extra key in a table constructor, so the file catches a
typo the other way round: each kind and each verb declares the field it cannot work
without, so a misspelt one reads as `missing-fields`, and each field drawn from a
fixed set is spelled as that set, so a misspelt value is `assign-type-mismatch`.
`scripts/ci/check-lua-types.sh` runs three panes from `tests/fixtures/lua_types/`
that each make one of those mistakes and fails if any stops being reported —
`--check ui` alone would also pass against a file describing nothing.

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
error rather than a silently-nil pane. That shape is **generated**: `src/kernel/lua_api.rs`
declares every published `thurbox.*` field once (both publish paths, `LuaHost::publish`
and `LuaHost::enter`, which sets `thurbox.runs` and `thurbox.granted` per plugin), and
`just lua-api` renders it into the generated regions of `thurbox.yml` and
`ui/lib/thurbox.d.lua` and into `tests/fixtures/lua_std/reads.lua`. `tests/lua_api.rs`
fails when a file drifts from it and when a real publish serves a key it does not
declare, so a new field is an edit to `lua_api.rs` plus `just lua-api`. Removing a field
or changing its meaning bumps `lua_api::API` (`thurbox.api`), which a plugin's
`requires_api` is checked against at load. selene
checks a **dotted** path one segment at a time and stops at the first `[…]`, so a
table read as `thurbox.platform.os` needs an entry per field while a list read as
`thurbox.sessions[i].name` stops at the list. `selene ui examples` cannot notice a
table left at the table — no bundled pane reads one by name — so
`scripts/ci/check-lua-std.sh` runs the panes in `tests/fixtures/lua_std/`:
`reads.lua` reads every declared path and must lint clean, and `typos/` holds one pane
per table misspelling one field, each of which must not. The assertion is selene's
`incorrect_standard_library_use` code out of its `Json2` output, and the table a
finding belongs to is the file it was found in — neither depends on the wording
of a message, which a selene release could change under us.

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
  `ADR-*` / `schema vNN` anchors (they point at `docs/thurbox/architecture.md` /
  `docs/thurbox/performance.md`). **Cut** restatements, obvious trailing labels (`// list`,
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

Pull requests land by **squash merge**, so the commit on `main` is
built from the pull request **title** plus GitHub's own `(#N)`
suffix — not from any commit on the branch. That title is what
`cog bump --auto` reads for the release decision and what the
changelog quotes, and `.github/workflows/pr-title.yml` (the required
`PR Title` check, via `scripts/ci/check-pr-title.sh`) is the only
thing that validates it. Title the pull request after its most
significant change.

Branch commits are held to the same convention locally by the
`commit-msg` hook, for a legible history; nothing in CI checks them,
because the squash throws them away.

- **Types**: feat, fix, perf, refactor, docs, style, test,
  chore, ci, build, revert
- **Scopes**: api, cli, ui, git, core, docs, deps, config, mcp, fleet, runtime
- Use `cog commit feat "message"`
  or `cog commit fix "message" scope`

## Module Dependency Rules (enforced by tests/architecture_rules.rs)

```text
node                 may reference                     [fully-qualified path only]
session              nothing — pure data, the dependency sink (no
                     process state, no env reads, no rendering crate;
                     re-exports runtime-mux's host model and SessionId)
agent                agent::* (declarations only)
agent::*             every file a node: session, paths, shell, config::file
                     each declares (NEVER git, NEVER backend)
config               config::* (declarations only)
config::*            every file a node: config::file (the one loader, over
                     paths) plus session (themes: theme instead)
update               update::* (declarations only)
update::*            paths, shell, agent::fetch, update::version_check
backend              session, paths, config::hosts     (the gateway to
                                                        runtime-mux, and what
                                                        thurbox injects)
git                  session, paths, shell
telemetry            session                           (ADR-38 diagnostic logs)
kernel               session, paths, git,              [agent|config|update::<file>,
                     notifications, theme               backend]
cli                  session, paths, notifications,    [agent|config|update::<file>,
                     ui_control, theme                  backend, kernel]
notifications        session, paths, shell
clipboard            session, paths
workspace            paths
paths                nothing — leaf utility
ui_control           paths
shell                nothing — runtime-mux's launchers and quoting, re-exported
theme                nothing — the presets, in ratatui colours
coordinator          agent::input, config::{hosts,     (main's body: the loop,
                     interface_state},                  the workers, the chrome)
                     backend, clipboard, kernel,
                     notifications, paths, session,
                     shell, telemetry, ui_control
```

The multiplexer runtime is its own crate, `crates/runtime-mux` (ADR-41), with
its own allowlist (`crates/runtime-mux/tests/architecture_rules.rs`,
`docs/constitution.md` R3): the adapters `tmux`, `psmux`, `rmux` are peers
over the shared `tmux_compat` server, only the factory `wiring` names one, and
it depends on no other crate of the repository — the settings and data
directory it needs are injected (`MuxSettings`, `HostsConfig`). Its
`runtime-mux` program (`src/main.rs`, the composition root, over the
`protocol` module) answers the runtime protocol on a tmux socket of its own,
`runtime-mux` (`RUNTIME_MUX_SOCKET` names another), never thurbox's.
`crates/architecture.toml` declares which crate may depend on which, and
Fleet's `fleet-arch` (its `architecture` test) holds the workspace to it.

Enforcement is an **allowlist** over **resolved** edges: every module under
`src/` needs a `ModuleRules` entry naming what it may reference in *any* form, so
a new module fails the test until its place is declared, and one loop asserts
every entry, so no rule can be declared and left unchecked. A node is a top-level
module or a governed submodule (every file module of `agent`, `config` and
`update` is one), and a
reference counts where it resolves — through `super::`, brace groups, re-exports
and `type` aliases. Both the actual and the declared graph must be acyclic, an
unused grant fails, and the crossings still scheduled for removal are listed,
item by item, in the test's `TRANSITIONAL` table, which must equal what the
source does — empty since status and the heartbeat went behind the contract
(ADR-32); `consumers_reach_no_concrete_backend` holds every module but
`coordinator` to reaching no adapter, protocol helper or factory through the
`backend` gateway, and `coordinator` to the factory alone;
`runtime_mux_is_named_only_by_its_gateways` keeps the gateway the only way in,
since the resolver follows no external crate: only `backend` and the modules
re-exporting a runtime type (`RUNTIME_GATEWAYS`) may name `runtime_mux`.
Inside `kernel`, `each_kernel_worker_lives_in_a_file_of_its_own` keeps each
worker of `kernel::snapshot` and `kernel::terminal` in a file of its own.
`docs/constitution.md` T1 lists the same graph. The full rule, the module
responsibilities and the event loop are in the `thurbox-kernel` skill.

## Pre-commit Hooks

18 hooks run automatically via `prek` (Rust-based pre-commit
framework). Install with `prek install`. Stages:

- **commit-msg**: conventional commit validation (`cog verify`)
- **pre-commit**: fmt, clippy, check, nextest, architecture,
  deny, doc, bats (the install script), shellcheck, rumdl, selene,
  stylua, prettier, htmlhint, stylelint, eslint

There is no **pre-push** stage: the hook that used it walked the
branch's commit messages, which squash merge discards.

The bats hook has a CI twin (`install-script`), so the suite that
guards that script is actually run rather than merely present.

Shell scripts are linted with **shellcheck** (config in
`.shellcheckrc`); install it from your package manager (it is not a
cargo crate — `scripts/install-dev-tools.sh` prints a reminder).

## Key Technical Details

- MSRV: 1.95, Edition 2021 (`rust-version` in `Cargo.toml`)
- Async runtime: tokio (multi-threaded)
- Session backend: Fleet runs every session on `runtime-mux`
  (`crates/runtime-mux`), a tmux-protocol server per adapter — `tmux`,
  `psmux` and opt-in `rmux`, peers over the shared `tmux_compat::server`
  (ADR-31). thurbox reaches its own panes (companion shells, stream panes)
  through the `backend` gateway to the same crate. Its local socket is
  `thurbox`/`thurbox-dev` only for an instance on the **default** data dir; one
  relocated by `THURBOX_DATA_DIR` derives its own (`thurbox-<digest>`) so it
  never creates windows on the operator's server, and `THURBOX_SOCKET`
  overrides both. `thurbox-cli version --json` reports the name in force —
  ADR-12, `docs/thurbox/config.md` → Relocating an instance
- Output reader runs in `tokio::task::spawn_blocking`
  (blocking I/O), writer in `tokio::spawn` (async)
- Terminal state parsed by `vt100::Parser`,
  rendered by `tui_term::PseudoTerminal`
- Sessions persist across restarts (tmux keeps them alive)
- Sessions are Fleet Tasks (ADR-41): thurbox keeps no database. Its own
  state is plain files: `interface.json` (theme, editor, the session
  order, each companion shell's pane) beside
  `settings.toml`, `bookmarks.json` and `perf.json` in the data directory;
  the agents are Fleet's harness profiles;
  remote SSH hosts in `~/.config/thurbox/hosts.toml`
- Requires tmux >= 3.2, psmux >= 3.3.8, or opt-in RMUX >= 0.10.0;
  version evidence and RMUX host limits are in `docs/thurbox/config.md`

## Design Documentation

For rationale behind decisions, see `docs/`:

- `docs/thurbox/tutorial.md` — The onboarding walkthrough (screenshots generated by
  `scripts/demo/record-tutorial.sh`; re-record when a step's screen changes)
- `docs/constitution.md` — The non-negotiable rules for every component, and
  each component's own
- `docs/thurbox/architecture.md` — Architectural decisions with rationale
- `docs/thurbox/features.md` — Feature-level design choices
- `docs/thurbox/config.md` — Thurbox's own config files and env vars in one place
- `docs/thurbox/agents.md` — Agents are Fleet's harnesses: where each is
  defined, and what thurbox no longer reads
- `docs/thurbox/performance.md` — Render/tick performance: demand-driven redraw,
  perf counters, the session-order cache, and how to measure
- `docs/thurbox/benchmark-multiplexers.md` — thurbox against raw tmux and Herdr as a
  host for agent sessions: the method, and `just bench-multiplexers`
- `docs/thurbox/review.md` — The per-path house rules a change is reviewed
  against, the trees excluded from review, and which document owns
  which class of fact. `.publish.yaml` names it in `review.rules`;
  read the blocks matching the paths you are touching
- `docs/README.md` — The whole system: thurbox, Fleet and the runtimes, the
  contracts between them (ADR-41)
- `docs/fleet/` — Fleet's documentation, indexed by `docs/fleet/README.md`
- `docs/runtime/` — the runtime protocol and the terminal stream

**Rule**: If a code change invalidates or extends a documented
decision, update the relevant doc in the same PR.
