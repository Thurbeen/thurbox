# AGENTS.md

This file provides guidance to coding agents working in this repository.

## Project

Thurbox is a multi-session coding-agent TUI built with Rust. It runs
coding-agent CLIs in persistent multiplexer sessions and renders them as
terminal panels with ratatui + tui-term. Sessions survive crashes and restarts
because the multiplexer keeps the processes alive.

Each session is a Fleet Task (ADR-41), and the agent it runs is the Task's
harness: one of the harness profiles Fleet has, which launch, resume and watch
the agent's CLI. Thurbox is agent-neutral: it keeps no agent registry and
knows nothing about any agent's model, permissions, prompts or tools; it offers
the harnesses `fleet version --json` lists.

## Repository layout

The repository is one Cargo workspace (ADR-41). `crates/architecture.toml`
lists its crates and the dependencies each may have; `arch-rules` checks it.
`docs/README.md` describes the components and the contracts between them.
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
  `RUNTIME_MUX_BIN`), except under `CI`, where they fail; CI's Nextest job
  has both and the Fleet job excludes them. The `exec` kind starts the
  harness profile's argv (`fleet_api::harness`) and learns what only the
  agent knows from its `fleet signal` reports, never from the runtime.
- A test that runs `git` in a scratch directory removes git's location
  variables (`GIT_DIR`, `GIT_WORK_TREE`, … — `GIT_LOCATION` in
  `crates/fleet-cli/tests/e2e_runtime_exec.rs`) from the child: a commit hook
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
just lint                            # everything CI lints

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
clean one per run.

The isolation lives in one helper, `scripts/dev/lib/sandbox-env.sh`
(`tbx_sandbox_init` = thurbox-only, `tbx_sandbox_init_full` = full HOME/XDG),
sourced by the sandbox entrypoint and the demo recorders. Single source of truth
for the `thurbox-dev` sandbox pattern; `tests/tui_e2e.rs` isolates the same way in Rust.

## Working reference (skills)

The per-subsystem reference lives in the skills under `.agents/skills/`,
loaded on demand. Every skill's body lives there, agent-neutrally;
`.claude/skills/<name>` is a relative symlink into it, so Claude Code and
opencode read the one copy. Read the one your change touches:

| Skill | Covers |
|---|---|
| `thurbox-testing` | Testing · Kernel and interface tests · Session-backend e2e harnesses |
| `thurbox-performance` | Performance (render loop) |
| `thurbox-release` | Release Process · Distribution Packages · Installation Script |
| `thurbox-remote-hosts` | Remote SSH & WSL Sessions |
| `thurbox-cli` | thurbox-cli and its subcommands |
| `thurbox-session-status` | Session status (from Fleet) · OS notifications |
| `thurbox-kernel` | Architecture (plugin kernel) · Writing an interface plugin |
| `thurbox-ui-surfaces` | Keybindings · Themes · Settings panel · Global search · Code review |
| `thurbox-demo-media` | Demo Video |

`ui-review` screenshots the TUI and critiques it. The `fleet-*` skills belong
to Fleet.

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

thurbox's module graph is an allowlist over resolved edges in
`crates/thurbox/tests/architecture_rules.rs`: every module under `src/` has one
entry there, and that test is the only copy of the graph. `runtime-mux`'s
module rules are in `crates/architecture.toml`, which `arch-rules` checks. The module
responsibilities and the event loop are in the `thurbox-kernel` skill.

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

## Pre-commit Hooks

Hooks run automatically via `prek` (Rust-based pre-commit framework).
Install with `prek install`. Stages:

- **commit-msg**: conventional commit validation (`cog verify`)
- **pre-commit** (fast): fmt, clippy, large files, bats (the install script, the
  rumdl version check), shellcheck, rumdl (after checking the local
  version matches CI's pin), selene, stylua, prettier, htmlhint,
  stylelint, eslint
- **pre-push** (full build): nextest (which includes the architecture
  rules), rustdoc, cargo-deny, Windows clippy (`just clippy-windows`)

The suite runs at push rather than per commit, so committing stays
cheap.

**Delegated build agents** follow `.agents/agents/thurbox-builder.md`:
the repository facts, gate and house rules a brief would otherwise
repeat. Like the skills, the body is agent-neutral and
`.claude/agents/thurbox-builder.md` is a relative symlink to it, so
Claude Code offers it as the `thurbox-builder` agent type; brief any
other coding CLI to read the file first.

Shell scripts are linted with **shellcheck** (config in
`.shellcheckrc`); install it from your package manager (it is not a
cargo crate — `scripts/install-dev-tools.sh` prints a reminder).

## Key Technical Details

- MSRV: 1.95, Edition 2021 (`rust-version` in `crates/thurbox/Cargo.toml`)
- Async runtime: tokio (multi-threaded)
- Sessions are Fleet Tasks (ADR-41) on `runtime-mux`. thurbox keeps no
  database: its own state is plain files, listed in `docs/thurbox/config.md` →
  Files at a glance. The tmux socket an instance uses is in the same document
  → Relocating an instance.
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
